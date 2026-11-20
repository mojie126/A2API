use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use super::{cancellation, connections::ConnectionGuard, rotate, route_context::RequestRouting,
    usage::RequestTelemetry, RouteTarget, UpstreamService};
use crate::server::errors::GatewayError;

pub(super) async fn select(service: &UpstreamService, providers: &[&str], keys: &rotate::CooldownKeys<'_>,
    tried: &[String], pinned: Option<&str>, conversation: &str, routing: &RequestRouting,
    connection: &mut ConnectionGuard, telemetry: &Arc<RequestTelemetry>,
) -> Result<RouteTarget, GatewayError> {
    let deadline = Instant::now() + Duration::from_secs(crate::server::core::runtime_capacity::limits().queue_wait_seconds);
    let mut excluded = tried.to_vec();
    let notify = service.connections().changed_notify();
    loop {
        let wake = notify.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        if telemetry.is_cancelled() { return Err(cancellation::cancelled_error()); }
        let selected = rotate::select_target_account(service, providers, keys, &excluded, pinned, conversation, routing).await;
        match selected {
            Ok(target) if target.account_id.is_none() && excluded.len() > tried.len() => {},
            Ok(target) => {
                // 上限取组内并发分配结果（`concurrency_limit_of`），而非仅账号自身的
                // `maxConcurrent`：路由组的「组内并发设置」会写入 routingConcurrencyCap，
                // 只用 max_concurrent_of 会绕开组内限制（本地增强，见 core/routing_concurrency）
                // 语义对齐 try_rebind：0 = 不限，>0 = 已达该数即拒绝；Some(0) 是「本轮分不到槽位」，
                // 按已达上限处理（用 1 表示一个都不允许更直观，这里直接用 saturating 处理）。
                let limit = match target.account.as_ref().and_then(crate::server::core::routing::concurrency_limit_of) {
                    None => 0,
                    Some(0) => 1, // 本轮零槽位：已有 0 个在途也必须阻断
                    Some(cap) => cap,
                };
                if connection.try_rebind(target.account_id.clone(), &target.provider, limit) { return Ok(target); }
                if pinned.is_none() {
                    if let Some(key) = target.attempt_key() { excluded.push(key); }
                    if excluded.len().saturating_sub(tried.len()) < 32 { continue; }
                }
            }
            Err(error) if error.code.as_deref() == Some("account_busy") || excluded.len() > tried.len() => {},
            Err(error) => return Err(error),
        }
        connection.rebind(None, "");
        if Instant::now() >= deadline { return Err(busy()); }
        let token = telemetry.cancel_token();
        tokio::select! {
            biased;
            _ = async { match token { Some(token) => token.cancelled().await, None => std::future::pending().await } } => {
                return Err(cancellation::cancelled_error());
            }
            _ = tokio::time::sleep_until(deadline) => return Err(busy()),
            _ = &mut wake => {},
            _ = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
        excluded.clear();
        excluded.extend_from_slice(tried);
    }
}

fn busy() -> GatewayError {
    GatewayError::with_status(503, "当前路由内的账号均忙，排队等待超时，请稍后重试")
        .with_code("account_busy").with_retry_after_seconds(1)
}

pub(super) fn all_busy(accounts: &[Value], counts: &std::collections::HashMap<String, usize>, tried: &[String]) -> bool {
    let mut found = false;
    for account in accounts {
        let Some(id) = crate::server::core::routing::account_id(account) else { continue; };
        if account["enabled"] == false || account["hasCredentials"] == false
            || tried.contains(&crate::server::core::routing_groups::attempt_key(account)) { continue; }
        // 同上：上限含路由组的组内并发分配（`concurrency_limit_of`），
        // None = 不限即视为不忙；Some(0) 表示本轮零槽位即视为忙
        match crate::server::core::routing::concurrency_limit_of(account) {
            None => return false,
            Some(cap) if (counts.get(id).copied().unwrap_or(0) as u64) < cap => return false,
            Some(_) => {}
        }
        found = true;
    }
    found
}
