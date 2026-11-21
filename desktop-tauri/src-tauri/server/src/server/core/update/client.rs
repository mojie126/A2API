//! 公网请求共用更新出网代理；指定代理失效时不回退直连。

use std::time::Duration;

use serde_json::json;
use serde_json::Value;

use crate::server::config;
use crate::server::core::egress;
use crate::server::core::proxies::{
    resolve_account_proxy, ProxyResolution, ResolvedProxy, CLASH_MIXED_UID,
};
use crate::server::logging;

/// 引用 WARP 设备时等待出口池重建的最长时间
///
/// 设备注册与隧道建立通常十几秒；重启后的第一次检查更新正好落在这个窗口里，
/// 不能立刻判成「出口不可用」
const WARP_READY_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) async fn resolve_egress_candidates() -> Result<Vec<(String, Option<ResolvedProxy>)>, String> {
    if let Some(configured) = config::update_proxy() {
        // 保存的是设备 id，重启后设备会重建、id 会变，解析要等池就绪后按槽位接回
        if configured.get("source").and_then(Value::as_str) == Some("warp") {
            let device = configured.get("deviceId").and_then(Value::as_str).unwrap_or("");
            let resolved = crate::server::core::warp::pool::global()
                .and_then(|pool| pool.find(device))
                .is_some();
            // 对照表里有这台设备才值得等：手改成未知 id 时等多久都接不回来
            let known = crate::server::core::warp::settings::device_slot(device).is_some();
            if !resolved && known {
                let ready = crate::server::core::warp::pool::wait_device(device, WARP_READY_TIMEOUT).await;
                logging::log(
                    "[Update]",
                    if ready {
                        "WARP 设备已就绪，重新应用更新出网线路"
                    } else {
                        "等待 WARP 设备超时，更新出网线路暂不可用"
                    },
                );
                if !ready {
                    return Err("指定的更新出口不可用（WARP 设备尚未就绪）".to_string());
                }
            }
        }
        match resolve_account_proxy(Some(&configured)) {
            Some(ProxyResolution::Resolved(proxy)) if proxy.port.is_some() => {
                let label = if proxy.label.is_empty() {
                    "指定的更新出口".to_string()
                } else {
                    proxy.label.clone()
                };
                return Ok(vec![(label, Some(proxy))]);
            }
            Some(ProxyResolution::Failed(reason)) => {
                return Err(format!("指定的更新出口不可用（{reason}）"));
            }
            _ => return Err("指定的更新出口没有可用端口".to_string()),
        }
    }
    let mut candidates = vec![("直连".to_string(), None)];
    if let Some(resolution) = resolve_account_proxy(Some(&json!({
        "source": "clash",
        "listenerUid": CLASH_MIXED_UID,
    }))) {
        if let Some(proxy) = resolution.resolved() {
            if proxy.port.is_some() {
                let label = if proxy.label.is_empty() {
                    "Clash 混合端口".to_string()
                } else {
                    proxy.label.clone()
                };
                candidates.push((label, Some(proxy.clone())));
            }
        } else if let Some(error) = resolution.error() {
            logging::verbose("[Update]", &format!("Clash 出口不可用（{error}），仅用直连"));
        }
    }
    Ok(candidates)
}

/// 普通公网资源只复用出口，不使用更新地址白名单或发送凭证。
pub async fn fetch_public(
    url: &str,
    headers: &[(String, String)],
    timeout_ms: Option<u64>,
) -> Result<reqwest::Response, String> {
    let host = url::Url::parse(url).ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string());
    let candidates = resolve_egress_candidates().await?;
    let mut last_error = "未知错误".to_string();
    for (label, proxy) in candidates {
        let client = egress::client_for(proxy.as_ref());
        let mut builder = client.get(url);
        if let Some(timeout_ms) = timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        for (name, value) in headers {
            if !name.eq_ignore_ascii_case("authorization") {
                builder = builder.header(name.as_str(), value.as_str());
            }
        }
        match builder.send().await {
            Ok(response) => return Ok(response),
            Err(error) => {
                last_error = egress::describe_error_detail(&error);
                logging::verbose("[Network]", &format!("经 {label} 访问 {host} 失败：{last_error}"));
            }
        }
    }
    Err(format!("无法连接 {host}：{last_error}"))
}
