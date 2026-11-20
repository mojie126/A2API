//! 同一次转发的凭证准备与插件调用共用选路结果，不重新挑选账号出口。
//! 请求scope只弱持原ReplayGuard；管理scope不能签发对话来源，JSON只是兼容投影。

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::sync::{Arc, Weak};

use serde_json::Value;

use crate::server::core::{
    proxies::{chain::{self, SelectedPath}, ResolvedProxy},
    upstream::{replay_guard::ReplayGuard, usage::RequestTelemetry},
};
use crate::server::errors::GatewayError;

struct AccountEgress {
    provider: String,
    account: String,
    choice: Choice,
    direct: bool,
    user_agent: Option<String>,
}

enum Choice {
    Path(Result<SelectedPath, String>),
    // None + direct=false只维持普通调用原语义，不生成SelectedPath::Direct。
    Unspecified,
    // 无真实请求的管理scope保留旧解析时点，不能取得EgressSelection。
    Legacy(Option<ResolvedProxy>),
}

struct Scope {
    request: Option<Weak<ReplayGuard>>,
    telemetry: Option<Weak<RequestTelemetry>>,
    current: RefCell<Option<Arc<AccountEgress>>>,
    failed: Cell<bool>,
}

tokio::task_local! {
    static CURRENT: Scope;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::server::core::plugin_host) enum EgressError {
    MissingScope,
    NoRequest,
    NotSelected,
    DifferentSource,
    Ambiguous,
    Unavailable,
}

/// 唯一构造入口是capture_current；同一次select的Arc身份与完整路径一起保留。
/// 没有Clone/serde/裸代理构造器；不是冻结客户端、批准或调用租约。
pub(in crate::server::core::plugin_host) struct EgressSelection {
    request: Weak<ReplayGuard>,
    selected: Arc<AccountEgress>,
}

impl AccountEgress {
    fn trusted_path(&self) -> Result<&SelectedPath, EgressError> {
        match &self.choice {
            Choice::Path(Ok(path)) if path.is_direct() == self.direct => Ok(path),
            Choice::Path(Ok(_)) | Choice::Unspecified => Err(EgressError::Ambiguous),
            Choice::Path(Err(_)) => Err(EgressError::Unavailable),
            Choice::Legacy(_) => Err(EgressError::NoRequest),
        }
    }

    fn wire_proxy(&self) -> Result<Value, GatewayError> {
        let projected = match &self.choice {
            Choice::Path(Ok(path)) => path.wire_json(),
            Choice::Path(Err(message)) => Err(message.clone()),
            Choice::Unspecified => Ok(Value::Null),
            Choice::Legacy(proxy) => proxy.as_ref().map(chain::wire_json)
                .transpose().map(|proxy| proxy.unwrap_or(Value::Null)),
        };
        projected.map_err(|message| GatewayError::with_status(502, message))
    }
}

pub(crate) async fn scope<F: Future>(telemetry: &Arc<RequestTelemetry>, work: F) -> F::Output {
    CURRENT.scope(Scope {
        request: Some(Arc::downgrade(telemetry.replay_guard())),
        telemetry: Some(Arc::downgrade(telemetry)),
        current: RefCell::new(None),
        failed: Cell::new(false),
    }, work).await
}

pub(crate) async fn scope_selection<F: Future>(provider: &str, account: &str,
    proxy: Option<ResolvedProxy>, direct: bool, work: F) -> F::Output {
    CURRENT.scope(Scope {
        request: None,
        telemetry: None,
        current: RefCell::new(Some(Arc::new(AccountEgress {
            provider: provider.to_string(), account: account.to_string(), choice: Choice::Legacy(proxy), direct,
            user_agent: None,
        }))),
        failed: Cell::new(false),
    }, work).await
}

pub(crate) fn select(provider: &str, account: &str, proxy: Option<&ResolvedProxy>, direct: bool) {
    let _ = CURRENT.try_with(|scope| {
        if scope.failed.get() { return; }
        // 在真正选号处选定完整链上游；不持RefCell借用读系统设置，不初始化本机桥。
        let choice = if scope.request.is_none() {
            Choice::Legacy(proxy.cloned())
        } else if proxy.is_none() && !direct {
            Choice::Unspecified
        } else {
            Choice::Path(chain::capture_selected(proxy.cloned()))
        };
        let Ok(mut current) = scope.current.try_borrow_mut() else {
            scope.failed.set(true);
            return;
        };
        // 每次select都是新的对象身份，即使包/账号/代理内容完全相同也不复用。
        *current = Some(Arc::new(AccountEgress {
            provider: provider.to_string(), account: account.to_string(), choice, direct,
            user_agent: None,
        }));
    });
}

/// 在当前转发作用域内设置选中账号的最终出站身份，不改写持久化账号记录
pub(crate) fn set_user_agent(provider: &str, account: &str, user_agent: Option<&str>) {
    let value = user_agent.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    let _ = CURRENT.try_with(|scope| {
        let Ok(mut current) = scope.current.try_borrow_mut() else { return };
        let Some(selected) = current.as_mut() else { return };
        if selected.provider != provider || selected.account != account { return }
        // AccountEgress 刻意不实现 Clone（选路身份不可复制），因此只能原地写：
        // 引用计数仍为 1 时成功，已被 EgressSelection 取走时静默跳过。
        // 调用顺序保证 set_user_agent 在 capture_current 之前执行。
        if let Some(current) = Arc::get_mut(selected) { current.user_agent = value; }
    });
}

pub(super) fn apply(params: &mut Value) -> Result<(), GatewayError> {
    apply_current(params)?;
    Ok(())
}

pub(super) fn apply_conversation(params: &mut Value, fallback: Option<ResolvedProxy>) -> Result<(), GatewayError> {
    if apply_current(params)? { return Ok(()); }
    let proxy = fallback.as_ref().map(chain::wire_json).transpose()
        .map_err(|message| GatewayError::with_status(502, message))?.unwrap_or(Value::Null);
    if let Some(fields) = params.as_object_mut() { fields.insert("proxy".to_string(), proxy); }
    Ok(())
}

/// 把选中账号的出站信息写进参数；返回是否真的命中当前作用域内的账号。
fn apply_current(params: &mut Value) -> Result<bool, GatewayError> {
    let provider = params.get("provider").and_then(Value::as_str).unwrap_or("");
    let account = params.get("account").and_then(Value::as_str).unwrap_or("");
    let selected = match current(provider, account) {
        Ok((_, selected)) => selected,
        Err(EgressError::Unavailable) => return Err(GatewayError::with_status(503,
            "本次账号出口选择不可用，未重新选择代理")),
        // 普通调用没有scope/匹配账号时保留原兼容行为；这不是可信选路成功。
        Err(_) => return Ok(false),
    };
    let proxy = selected.wire_proxy()?;
    let user_agent = selected.user_agent.clone();
    if let Some(fields) = params.as_object_mut() {
        fields.insert("proxy".to_string(), proxy);
        fields.insert("proxy_override".to_string(), Value::Bool(selected.direct));
        if let Some(user_agent) = user_agent {
            fields.insert("user_agent".to_string(), Value::String(user_agent));
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

fn current(provider: &str, account: &str) -> Result<(Option<Weak<ReplayGuard>>, Arc<AccountEgress>), EgressError> {
    CURRENT.try_with(|scope| {
        if scope.failed.get() { return Err(EgressError::Unavailable); }
        let saved = scope.current.try_borrow().map_err(|_| {
            scope.failed.set(true);
            EgressError::Unavailable
        })?;
        let selected = saved.as_ref().ok_or(EgressError::NotSelected)?;
        if selected.provider != provider || selected.account != account {
            return Err(EgressError::DifferentSource);
        }
        Ok((scope.request.clone(), selected.clone()))
    }).map_err(|_| EgressError::MissingScope)?
}

pub(in crate::server::core::plugin_host) fn capture_request(
    provider: &str,
    account: &str,
) -> Result<(Arc<RequestTelemetry>, EgressSelection), EgressError> {
    let telemetry = CURRENT.try_with(|scope| {
        if scope.failed.get() { return Err(EgressError::Unavailable); }
        scope.telemetry.as_ref().and_then(Weak::upgrade).ok_or(EgressError::NoRequest)
    }).map_err(|_| EgressError::MissingScope)??;
    let selected = capture_current(provider, account, &telemetry)?;
    if !selected.still_selected(provider, account, &telemetry) {
        return Err(EgressError::DifferentSource);
    }
    Ok((telemetry, selected))
}

/// 只能在原请求scope中捕获；读锁异常、缺选择、管理来源或不明确直连都不产生资格。
pub(in crate::server::core::plugin_host) fn capture_current(
    provider: &str,
    account: &str,
    telemetry: &RequestTelemetry,
) -> Result<EgressSelection, EgressError> {
    let (request, selected) = current(provider, account)?;
    let request = request.ok_or(EgressError::NoRequest)?;
    if !Weak::ptr_eq(&request, &Arc::downgrade(telemetry.replay_guard()))
        || provider.is_empty() || account.is_empty()
    { return Err(EgressError::DifferentSource); }
    // blocked也涵盖ReplayGuard锁中毒；这里只拒绝，不冻结、不解除请求安全状态。
    if telemetry.replay_guard().blocked() { return Err(EgressError::Unavailable); }
    selected.trusted_path()?;
    Ok(EgressSelection { request, selected })
}

impl EgressSelection {
    pub(in crate::server::core::plugin_host) fn selected_path(&self) -> Result<SelectedPath, EgressError> {
        self.selected.trusted_path().cloned()
    }
    /// 仅open_call成功后、尚未离开同一scope时核对；旧select的来源不能贴给后一次选号。
    pub(in crate::server::core::plugin_host) fn still_selected(
        &self, provider: &str, account: &str, telemetry: &RequestTelemetry,
    ) -> bool {
        if telemetry.replay_guard().blocked() { return false; }
        let Ok((Some(request), current)) = current(provider, account) else { return false; };
        Weak::ptr_eq(&self.request, &request)
            && Weak::ptr_eq(&request, &Arc::downgrade(telemetry.replay_guard()))
            && Arc::ptr_eq(&self.selected, &current)
            && self.matches_account(provider, account)
    }

    /// 绑定后只做现成身份/路径资格比较，不再查询task_local或现时配置。
    pub(in crate::server::core::plugin_host) fn matches_account(&self, provider: &str, account: &str) -> bool {
        self.selected.provider == provider && self.selected.account == account
            && self.selected.trusted_path().is_ok()
    }

    /// 消费已绑定的选择，不重新读取CURRENT或系统代理；普通请求若仍使用原快照则只复制路径。
    /// 一次性机会由ConversationEgress的不可回补槽控制，本对象不会被轻量终态保留。
    pub(in crate::server::core::plugin_host) fn into_path(self) -> Result<SelectedPath, EgressError> {
        self.selected.trusted_path().cloned()
    }
}

/// 读取账号在当前请求上的出站身份覆盖值
pub(crate) fn user_agent_for(provider: &str, account: &str) -> Option<String> {
    CURRENT.try_with(|slot| {
        // current 是 RefCell：借用失败（正在写入）时按「无覆盖值」处理
        let current = slot.current.try_borrow().ok()?;
        current.as_ref()
            .filter(|current| current.provider == provider && current.account == account)
            .and_then(|current| current.user_agent.clone())
    }).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn current_user_agent() -> Option<String> {
        user_agent_for("codex", "acc")
    }

    #[tokio::test]
    async fn request_user_agent_is_scoped_and_not_shared_across_accounts() {
        let outside = scope_selection("codex", "acc", None, true, async {
            set_user_agent("codex", "acc", Some("codex_cli_rs/1.0"));
            current_user_agent().await
        }).await;
        let other = scope_selection("codex", "other", None, true, async {
            set_user_agent("codex", "other", Some("claude-cli/1.0"));
            current_user_agent().await
        }).await;
        assert_eq!(outside.as_deref(), Some("codex_cli_rs/1.0"));
        assert_eq!(other, None);
    }
}
