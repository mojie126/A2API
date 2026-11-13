//! 同一次转发的凭证准备与插件调用共用选路结果，不重新挑选账号出口。

use std::cell::RefCell;
use std::future::Future;

use serde_json::Value;
use crate::server::errors::GatewayError;

use crate::server::core::proxies::ResolvedProxy;

struct AccountEgress {
    provider: String,
    account: String,
    proxy: Option<ResolvedProxy>,
    direct: bool,
    user_agent: Option<String>,
}

tokio::task_local! {
    static CURRENT: RefCell<Option<AccountEgress>>;
}

pub(crate) async fn scope<F: Future>(work: F) -> F::Output {
    CURRENT.scope(RefCell::new(None), work).await
}

pub(crate) fn select(provider: &str, account: &str, proxy: Option<&ResolvedProxy>, direct: bool) {
    let proxy = proxy.cloned();
    let _ = CURRENT.try_with(|slot| {
        if let Some(current) = slot.borrow_mut().as_mut() {
            if current.provider == provider && current.account == account {
                current.proxy = proxy;
                current.direct = direct;
                return;
            }
        }
        *slot.borrow_mut() = Some(AccountEgress {
            provider: provider.to_string(), account: account.to_string(), proxy, direct,
            user_agent: None,
        });
    });
}

/// 在当前转发作用域内设置选中账号的最终出站身份，不改写持久化账号记录
pub(crate) fn set_user_agent(provider: &str, account: &str, user_agent: Option<&str>) {
    let value = user_agent.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    let _ = CURRENT.try_with(|slot| {
        let mut current = slot.borrow_mut();
        let Some(current) = current.as_mut() else { return };
        if current.provider == provider && current.account == account {
            current.user_agent = value;
        }
    });
}

pub(super) fn apply(params: &mut Value) -> Result<(), GatewayError> {
    CURRENT.try_with(|slot| {
        let current = slot.borrow();
        let Some(current) = current.as_ref() else { return Ok(()); };
        if params.get("provider").and_then(Value::as_str) != Some(current.provider.as_str())
            || params.get("account").and_then(Value::as_str) != Some(current.account.as_str())
        {
            return Ok(());
        }
        let proxy = current.proxy.as_ref().map(crate::server::core::proxies::chain::wire_json)
            .transpose().map_err(|message| GatewayError::with_status(502, message))?
            .unwrap_or(Value::Null);
        if let Some(fields) = params.as_object_mut() {
            fields.insert("proxy".to_string(), proxy);
            fields.insert("proxy_override".to_string(), Value::Bool(current.direct));
            if let Some(user_agent) = current.user_agent.as_ref() {
                fields.insert("user_agent".to_string(), Value::String(user_agent.clone()));
            }
        }
        Ok(())
    }).unwrap_or(Ok(()))
}

/// 读取账号在当前请求上的出站身份覆盖值
pub(crate) fn user_agent_for(provider: &str, account: &str) -> Option<String> {
    CURRENT.try_with(|slot| {
        let current = slot.borrow();
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
        let outside = scope(async {
            select("codex", "acc", None, true);
            set_user_agent("codex", "acc", Some("codex_cli_rs/1.0"));
            current_user_agent().await
        }).await;
        let other = scope(async {
            select("codex", "other", None, true);
            set_user_agent("codex", "other", Some("claude-cli/1.0"));
            current_user_agent().await
        }).await;
        assert_eq!(outside.as_deref(), Some("codex_cli_rs/1.0"));
        assert_eq!(other, None);
    }
}
