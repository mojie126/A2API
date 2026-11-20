//! ZCode 客户端版本号探测：签到触发时从官网更新日志取最新版本并写回设置。
//!
//! ── 为什么在网关而不是插件里写 ──────────────────────────────
//! 「客户端版本号」是插件声明给用户设置的字段（`appVersion`），网关按
//! 「用户设置只能由管理界面修改」的规则拒绝插件自行覆盖；探测与写回因此
//! 放在网关这一侧，插件只负责读取生效值。
//!
//! ── 版本号从哪来 ────────────────────────────────────────────
//! 上游没有公开的 JSON 版本接口（`/api/v1/client/version` 等均 404），
//! 最新版本只出现在服务端渲染的更新日志页首条发布标签里；同一串也会出现在
//! 首页下载地址 `cdn-zcode.z.ai/zcode/electron/releases/<版本>/…` 中。
//!
//! 探测失败不影响签到：页面改版、网络不通都直接返回，保留用户已填的值。

use serde_json::json;

use crate::server::core::plugin_host::provider_settings;
use crate::server::core::update::client;
use crate::server::logging;

/// 两家共用同一份设置（见插件侧 `APP_VERSION_SETTING_OWNER`）。
const PROVIDERS: &[&str] = &["zcode", "zcode-intl"];
const FIELD: &str = "appVersion";
const CHANGELOG_URL: &str = "https://zcode.z.ai/cn/changelog";
/// 版本标签在页面里的固定前缀（首条即最新版本）。
const VERSION_MARKER: &str = "font-mono text-sm\">";
const TIMEOUT_MS: u64 = 12_000;

/// 从更新日志页面解析首条版本号。
fn parse_latest_version(html: &str) -> Option<String> {
    let start = html.find(VERSION_MARKER)? + VERSION_MARKER.len();
    let rest = &html[start..];
    let version = rest[..rest.find('<')?].trim();
    let parts: Vec<&str> = version.split('.').collect();
    let valid = parts.len() == 3
        && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    valid.then(|| version.to_string())
}

/// 数字段版本比较（只比较按点分段的十进制数字）。
fn version_key(value: &str) -> Option<Vec<u64>> {
    let parts: Vec<u64> = value
        .trim()
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (!parts.is_empty()).then_some(parts)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_key(candidate), version_key(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

async fn latest_version() -> Option<String> {
    let response = client::fetch_public(
        CHANGELOG_URL,
        &[("Accept".to_string(), "text/html".to_string())],
        Some(TIMEOUT_MS),
    )
    .await
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let html = response.text().await.ok()?;
    parse_latest_version(&html)
}

/// 探测并写回最新客户端版本号；不需要更新或失败时静默返回。
pub(crate) async fn refresh() {
    let Ok(current) = provider_settings::legacy_provider(PROVIDERS, FIELD).await else {
        return;
    };
    let saved = current["values"][FIELD].as_str().map(str::trim).unwrap_or("");
    let provider = current["provider"].as_str().unwrap_or("");
    let revision = current["revision"].as_str().unwrap_or("");
    if provider.is_empty() || revision.is_empty() {
        return;
    }
    let Some(latest) = latest_version().await else { return; };
    // 设置为空表示使用插件默认值：此时直接用官网最新版本登记，界面也能看到。
    if !saved.is_empty() && !is_newer(&latest, saved) {
        return;
    }
    match provider_settings::configure(provider, revision, &json!({ FIELD: latest })).await {
        Ok(_) => logging::log(
            "[ZCode]",
            &format!(
                "客户端版本号已更新为 {latest}（原值：{}）",
                if saved.is_empty() { "插件默认" } else { saved }
            ),
        ),
        Err(error) => logging::verbose(
            "[ZCode]",
            &format!("客户端版本号写回失败：{}", error.message),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, parse_latest_version};

    #[test]
    fn the_changelog_version_label_parses_as_latest() {
        let html = r#"<span class="w-fit rounded-full border border-white px-2.5 py-0.5 font-mono text-sm">3.18.2</span><span>发布于 2026年11月13日</span><span class="font-mono text-sm">3.17.2</span>"#;
        assert_eq!(parse_latest_version(html).as_deref(), Some("3.18.2"));
    }

    #[test]
    fn a_page_without_the_version_label_yields_none() {
        assert!(parse_latest_version("<html><body>no version</body></html>").is_none());
        assert!(parse_latest_version(r#"<span class="font-mono text-sm">latest</span>"#).is_none());
    }

    #[test]
    fn version_comparison_is_numeric_per_segment() {
        assert!(is_newer("3.18.2", "3.17.2"));
        assert!(is_newer("3.9.10", "3.9.9"));
        assert!(!is_newer("3.17.2", "3.17.2"));
        assert!(!is_newer("3.17.2", "3.18.2"));
        assert!(!is_newer("not-a-version", "3.17.2"));
    }
}
