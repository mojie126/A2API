//! 插件目录、发行版与下载统一读取本 fork 的插件索引仓库，包保持可信签名校验。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) use super::packages::MAX_PACK_BYTES;

use crate::server::core::update::{compare_versions, is_prerelease_version, UpdateError};

/// 本 fork 的插件索引仓库，index.json、插件清单与图标托管在 GitHub
pub const INDEX_REPO: &str = "Agent2plugin/Agent2plugin";
/// 本 fork 的插件构建仓库，索引不可达时的兜底
pub const FALLBACK_REPO: &str = "mojie126/A2P";
const QUERY_TIMEOUT_MS: u64 = 30_000;

/// 索引仓库内相对路径对应的 raw 地址
///
/// @param path 索引仓库内的相对路径，禁止绝对路径、反斜杠与 .. 上跳
fn index_raw(path: &str) -> Option<String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.contains(['?', '#'])
        || path.split('/').any(|part| part == "..")
    {
        return None;
    }
    Some(format!("https://raw.githubusercontent.com/{INDEX_REPO}/main/{path}"))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PackAsset {
    pub version: String,
    pub name: String,
    pub url: String,
    pub size: u64,
    pub page: String,
    pub require_signed: bool,
    #[serde(default)]
    pub sha256: Option<String>,
}

pub fn asset_name(pack_id: &str, version: &str, target: &str) -> String {
    format!("{pack_id}-{version}-{target}.zip")
}

#[derive(Clone, Debug)]
pub struct PluginDetail {
    pub protocol: u32,
    pub repo: String,
    pub tag_template: String,
    pub asset_template: String,
    pub homepage: String,
    pub require_signed: bool,
}

#[derive(Clone, Debug)]
pub struct ReleaseAssets {
    pub version: String,
    pub page: String,
    pub notes: String,
    pub published_at: String,
    pub prerelease: bool,
    pub platforms: Vec<(String, String, u64)>,
    pub digests: std::collections::HashMap<String, String>,
    pub require_signed: bool,
}

impl ReleaseAssets {
    pub fn asset(&self, id: &str, target: &str) -> Option<PackAsset> {
        let (platform, url, size) = self
            .platforms
            .iter()
            .find(|(platform, _, _)| platform == target || platform == super::wasm::manifest::TARGET)?;
        Some(PackAsset {
            version: self.version.clone(),
            name: asset_name(id, &self.version, platform),
            url: url.clone(),
            size: *size,
            page: self.page.clone(),
            require_signed: self.require_signed,
            sha256: self.digests.get(url).cloned(),
        })
    }
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn waiting_message(state: &crate::server::core::task_state::TaskState) -> String {
    state.waiting_message()
}

pub fn clear_cooldown() {
    super::catalog::clear_cooldown();
}

/// 读取本 fork 索引仓库的 index.json
///
/// @return 索引文档，schema 须为 1 且 plugins 数组不超过 200 项
pub(super) async fn fetch_index_document() -> Result<Value, UpdateError> {
    let url = format!("https://raw.githubusercontent.com/{INDEX_REPO}/main/index.json");
    let headers = vec![("User-Agent".to_string(), "agent2api-plugin".to_string())];
    let response = crate::server::core::update::client::fetch_public(&url, &headers, Some(QUERY_TIMEOUT_MS))
        .await.map_err(UpdateError::new)?;
    if !response.status().is_success() {
        return Err(UpdateError::new(format!(
            "读取插件索引失败：GitHub 返回 HTTP {}",
            response.status().as_u16()
        )));
    }
    let bytes = response.bytes().await.map_err(|error| UpdateError::new(error.to_string()))?;
    let document: Value = serde_json::from_slice(&bytes).map_err(|_| UpdateError::new("插件索引不是合法 JSON"))?;
    if document["schema"] != 1 { return Err(UpdateError::new("插件索引版本不受支持")); }
    if document.get("plugins").and_then(Value::as_array).is_none() {
        return Err(UpdateError::new("插件索引缺少 plugins 数组"));
    }
    if document["plugins"].as_array().is_some_and(|items| items.len() > 200) {
        return Err(UpdateError::new("插件目录过大"));
    }
    Ok(document)
}

/// 读取索引仓库内的插件清单
pub(super) async fn fetch_manifest(path: &str) -> Option<Value> {
    let url = index_raw(path)?;
    let headers = vec![("User-Agent".to_string(), "agent2api-plugin".to_string())];
    let response = crate::server::core::update::client::fetch_public(&url, &headers, Some(QUERY_TIMEOUT_MS))
        .await.ok()?;
    if !response.status().is_success() { return None; }
    let bytes = response.bytes().await.ok()?;
    serde_json::from_slice(&bytes).ok()?
}

/// 读取索引仓库内的插件图标并编码为 data URL
pub(super) async fn read_store_icon(path: &str) -> Option<String> {
    let extension = path.rsplit('.').next()?.to_ascii_lowercase();
    let mime = match extension.as_str() {
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        _ => return None,
    };
    let url = index_raw(path)?;
    let headers = vec![("User-Agent".to_string(), "agent2api-plugin".to_string())];
    let response = crate::server::core::update::client::fetch_public(&url, &headers, Some(2 * QUERY_TIMEOUT_MS))
        .await.ok()?;
    if !response.status().is_success() { return None; }
    let bytes = response.bytes().await.ok()?;
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    use base64::Engine;
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn valid_repo(repo: &str) -> bool {
    let parts: Vec<_> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn release_version(release: &Value, template: &str) -> Option<String> {
    let tag = text(release, "tag_name");
    let (prefix, suffix) = template.split_once("{version}")?;
    let version = tag.strip_prefix(prefix)?.strip_suffix(suffix)?;
    compare_versions(version, version)?;
    Some(version.to_string())
}

pub(super) fn pick_release_assets(
    release: &Value,
    id: &str,
    detail: Option<&PluginDetail>,
) -> Option<ReleaseAssets> {
    let tag = detail
        .map(|detail| detail.tag_template.as_str())
        .unwrap_or("v{version}");
    let version = release_version(release, tag)?;
    let template = detail
        .map(|detail| detail.asset_template.as_str())
        .unwrap_or("{id}-{version}-{target}.zip")
        .replace("{id}", id)
        .replace("{version}", &version);
    let (prefix, suffix) = template.split_once("{target}")?;
    let mut platforms: Vec<_> = release
        .get("assets")?
        .as_array()?
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            let target = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
            // 本 fork 同时发布原生（按本机三元组）与 WASM（固定 wasm 目标）资产，
            // 因此这里接受任意平台目标，不再只认 wasm 那一个（见 support.rs 模块头）
            let url = text(item, "browser_download_url");
            if url.is_empty() {
                return None;
            }
            Some((
                target.to_string(),
                url,
                item.get("size").and_then(Value::as_u64).unwrap_or(0),
            ))
        })
        .collect();
    platforms.sort_by(|left, right| left.0.cmp(&right.0));
    if platforms.is_empty() {
        return None;
    }
    let digests = release.get("assets").and_then(Value::as_array).into_iter().flatten().filter_map(|asset| {
        let value = asset.get("digest")?.as_str()?.strip_prefix("sha256:")?;
        (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| (text(asset, "browser_download_url"), value.to_ascii_lowercase()))
    }).collect();
    Some(ReleaseAssets {
        prerelease: release.get("prerelease").and_then(Value::as_bool) == Some(true)
            || is_prerelease_version(&version),
        version,
        // GitHub Releases 页原链接（别把人引去官网）
        page: {
            let html = text(release, "html_url");
            if url::Url::parse(&html).is_ok_and(|parsed| parsed.host_str() == Some("github.com")) {
                html
            } else {
                String::new()
            }
        },
        notes: text(release, "body").chars().take(32_000).collect(),
        published_at: text(release, "published_at"),
        platforms,
        digests,
        require_signed: false,
    })
}

pub(super) fn validate_release_urls(_release: &Value, assets: &ReleaseAssets, id: &str) -> Result<(), UpdateError> {
    for (platform, url, _) in &assets.platforms {
        let name = asset_name(id, &assets.version, platform);
        // GitHub Releases：资产名须一致、走 releases/download
        let github = url::Url::parse(url).is_ok_and(|parsed| {
            parsed.host_str() == Some("github.com")
                && parsed.path().contains("/releases/download/")
                && parsed.path().ends_with(&format!("/{name}"))
        });
        if !github {
            return Err(UpdateError::new("插件下载地址与包或版本不一致"));
        }
    }
    Ok(())
}

pub async fn release_for(
    repo: &str,
    id: &str,
    detail: Option<&PluginDetail>,
    current: Option<&str>,
) -> Result<Option<ReleaseAssets>, UpdateError> {
    release_for_channels(repo, id, detail, current, super::settings::current().channels()).await
}

pub(super) async fn release_for_channels(
    repo: &str,
    id: &str,
    detail: Option<&PluginDetail>,
    current: Option<&str>,
    channels: crate::server::config::UpdateChannels,
) -> Result<Option<ReleaseAssets>, UpdateError> {
    if !valid_repo(repo) {
        return Err(UpdateError::new("插件发布仓库必须是 owner/repo"));
    }
    let custom_tag = detail
        .map(|detail| detail.tag_template != "v{version}")
        .unwrap_or(false);
    let listed = channels.beta || custom_tag;
    let response = match query_releases(repo, id, listed).await? {
        Some(response) => response,
        None => return Ok(None),
    };
    let document = response.value;
    let list: Vec<&Value> = if listed {
        document
            .as_array()
            .ok_or_else(|| UpdateError::new("插件 Release 列表格式错误"))?
            .iter()
            .collect()
    } else {
        vec![&document]
    };
    let mut best: Option<ReleaseAssets> = None;
    for release in list {
        if release.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(mut assets) = pick_release_assets(release, id, detail) else {
            continue;
        };
        validate_release_urls(release, &assets, id)?;
        assets.require_signed = true;
        if !channels.accepts(assets.prerelease) {
            continue;
        }
        if current.is_some()
            && !assets
                .platforms
                .iter()
                .any(|(target, _, _)| *target == super::host_target() || target == super::wasm::manifest::TARGET)
        {
            continue;
        }
        if best
            .as_ref()
            .map(|item| compare_versions(&assets.version, &item.version) == Some(1))
            .unwrap_or(true)
        {
            best = Some(assets);
        }
    }
    Ok(best)
}

async fn query_releases(
    repo: &str,
    id: &str,
    listed: bool,
) -> Result<Option<crate::server::core::update::website::JsonResponse>, UpdateError> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') {
        return Err(UpdateError::new("插件标识无效"));
    }
    // 直接查 GitHub Releases（按 repo，本 fork 的插件仓库发布在这里，见模块头）
    if !valid_repo(repo) { return Ok(None); }
    let api = format!("https://api.github.com/repos/{repo}/releases{}", if listed { "?per_page=100" } else { "/latest" });
    let headers = vec![
        ("User-Agent".to_string(), "agent2api-plugin".to_string()),
        ("Accept".to_string(), "application/vnd.github+json".to_string()),
    ];
    let response = crate::server::core::update::client::fetch_public(&api, &headers, Some(QUERY_TIMEOUT_MS))
        .await.map_err(UpdateError::new)?;
    if response.status().as_u16() == 404 { return Ok(None); }
    if !response.status().is_success() {
        return Err(UpdateError::new(format!(
            "查询插件更新失败：GitHub 返回 HTTP {}",
            response.status().as_u16()
        )));
    }
    let bytes = response.bytes().await.map_err(|error| UpdateError::new(error.to_string()))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| UpdateError::new("GitHub 插件版本信息不是合法 JSON"))?;
    Ok(Some(crate::server::core::update::website::JsonResponse { value, status: reqwest::StatusCode::OK }))
}

pub async fn latest(pack_id: &str, target: &str) -> Result<Option<PackAsset>, String> {
    latest_in(FALLBACK_REPO, pack_id, target).await
}

pub async fn latest_in(
    repo: &str,
    pack_id: &str,
    target: &str,
) -> Result<Option<PackAsset>, String> {
    Ok(release_for(repo, pack_id, None, None)
        .await.map_err(|error| error.message)?
        .and_then(|release| release.asset(pack_id, target)))
}

pub async fn release_assets_in(repo: &str, id: &str) -> Result<Option<ReleaseAssets>, UpdateError> {
    release_for(repo, id, None, None).await
}

/// progress 的 total 是已知包大小（未知时为 0），不是硬上限；
/// 元数据、响应声明与逐块累计均受 MAX_PACK_BYTES 限制。
pub async fn download(
    asset: &PackAsset,
    mut progress: impl FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    // 来源：本 fork 插件仓库的 GitHub Releases（github.com/<repo>/releases/download/…）
    let github = url::Url::parse(&asset.url).is_ok_and(|url| {
        url.host_str() == Some("github.com")
            && url.path().starts_with('/')
            && url.path().contains("/releases/download/")
    });
    if !github {
        return Err("插件包只能从 GitHub Releases 下载，请重新刷新插件列表".to_string());
    }
    if asset.size > MAX_PACK_BYTES as u64 {
        return Err(format!("插件包超过 {} MiB 上限", MAX_PACK_BYTES / (1024 * 1024)));
    }
    let headers = vec![
        ("User-Agent".to_string(), "agent2api-plugin".to_string()),
        ("Accept".to_string(), "application/octet-stream".to_string()),
    ];
    let version_marker = format!("-{}-", asset.version);
    let pack_id = asset.name.strip_suffix(".zip").and_then(|name| {
        name.split_once(version_marker.as_str())
            .map(|(id, _)| id.to_string())
    });
    let response = crate::server::core::update::client::fetch_public(&asset.url, &headers, None).await
        .map_err(|error| format!("下载插件包失败：{error}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!("下载插件包失败：上游返回 HTTP {}", response.status().as_u16()));
    }
    let buffer = read_download(response, asset, &mut progress).await?;
    if let Some(expected) = asset.sha256.as_ref() {
        if super::install::sha256_hex(&buffer) != *expected {
            return Err("下载的插件包 SHA-256 不一致，已拒绝使用".to_string());
        }
    }
    let verify = buffer.clone();
    let version = asset.version.clone();
    tokio::task::spawn_blocking(move || {
        super::packages::prepare(&verify, pack_id.as_deref(), Some(&version), true)
    }).await.map_err(|error| error.to_string())?
        .map_err(|error| format!("下载的插件未通过可信签名校验：{error}"))?;
    Ok(buffer)
}

async fn read_download(
    response: reqwest::Response,
    asset: &PackAsset,
    progress: &mut impl FnMut(u64, u64),
) -> Result<Vec<u8>, String> {
    let declared = response.content_length().unwrap_or(0);
    let total = if asset.size > 0 { asset.size } else { declared };
    if total > MAX_PACK_BYTES as u64 || declared > MAX_PACK_BYTES as u64 {
        return Err(format!("插件包超过 {} MiB 上限", MAX_PACK_BYTES / (1024 * 1024)));
    }
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    progress(0, total);
    loop {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(30), stream.next())
            .await
            .map_err(|_| "插件下载长时间没有数据".to_string())?;
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|error| format!("下载中断：{error}"))?;
        if buffer.len().saturating_add(chunk.len()) > MAX_PACK_BYTES {
            return Err(format!("插件包超过 {} MiB 上限", MAX_PACK_BYTES / (1024 * 1024)));
        }
        buffer.extend_from_slice(&chunk);
        progress(buffer.len() as u64, total);
    }
    let received = buffer.len() as u64;
    if received == 0
        || (declared > 0 && received != declared)
        || (asset.size > 0 && received != asset.size)
    {
        return Err("下载到的插件包不完整，已拒绝使用".to_string());
    }
    Ok(buffer)
}

pub fn check_result(
    id: &str,
    current: &str,
    release: Option<&ReleaseAssets>,
    error: Option<&str>,
) -> Value {
    let asset = release.and_then(|release| release.asset(id, &super::host_target()));
    let available = asset
        .as_ref()
        .is_some_and(|asset| compare_versions(&asset.version, current) == Some(1));
    json!({
        "id": id, "available": available, "checked": true, "current": current,
        "latest": release.map(|release| &release.version),
        "notes": release.map(|release| &release.notes),
        "publishedAt": release.map(|release| &release.published_at),
        "prerelease": release.map(|release| release.prerelease).unwrap_or(false),
        "page": release.map(|release| &release.page),
        "asset": asset.map(|asset| json!({ "name": asset.name, "url": asset.url, "size": asset.size, "page": asset.page })),
        "error": error.is_some(),
        "reason": error.map(str::to_string).or_else(|| {
            if available { None } else if release.is_some() { Some("已经是最新版本".to_string()) }
            else { Some("当前通道暂无可安装的版本".to_string()) }
        }),
    })
}

#[cfg(test)]
fn pick_asset(release: &Value, id: &str, target: &str) -> Option<PackAsset> {
    pick_release_assets(release, id, None)?.asset(id, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 资产名必须与打包脚本的产出名逐字一致（两处任一处改了命名，
    /// 更新就会变成「找不到对应平台的包」）。
    #[test]
    fn asset_name_matches_pack_script() {
        assert_eq!(
            asset_name("agent2api-providers", "0.1.0", "x86_64-pc-windows-msvc"),
            "agent2api-providers-0.1.0-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn picks_asset_by_exact_name() {
        let release = json!({
            "tag_name": "v0.2.0",
            "html_url": "https://github.com/x/y/releases/tag/v0.2.0",
            "assets": [
                { "name": "agent2api-providers-0.2.0-x86_64-pc-windows-msvc.zip",
                  "browser_download_url": "https://example.com/a.zip", "size": 100 },
                { "name": "agent2api-providers-0.2.0-aarch64-apple-darwin.zip",
                  "browser_download_url": "https://example.com/b.zip", "size": 200 }
            ]
        });
        let asset = pick_asset(&release, "agent2api-providers", "x86_64-pc-windows-msvc")
            .expect("应当挑到 Windows 包");
        assert_eq!(asset.version, "0.2.0");
        assert_eq!(asset.size, 100);
        assert_eq!(asset.url, "https://example.com/a.zip");
    }

    /// 找不到当前平台时返回 None（而不是错挑一个别的平台）。
    #[test]
    fn skips_other_platforms() {
        let release = json!({
            "tag_name": "v0.2.0",
            "assets": [
                { "name": "agent2api-providers-0.2.0-aarch64-apple-darwin.zip",
                  "browser_download_url": "https://example.com/b.zip" }
            ]
        });
        assert!(pick_asset(&release, "agent2api-providers", "x86_64-pc-windows-msvc").is_none());
    }

    /// tag 带 `v` 前缀也要认。
    #[test]
    fn strips_v_prefix_from_tag() {
        let release = json!({
            "tag_name": "v1.2.3",
            "assets": [
                { "name": "agent2api-providers-1.2.3-x86_64-unknown-linux-gnu.zip",
                  "browser_download_url": "https://example.com/a.zip" }
            ]
        });
        let asset = pick_asset(&release, "agent2api-providers", "x86_64-unknown-linux-gnu")
            .expect("应当挑到 Linux 包");
        assert_eq!(asset.version, "1.2.3");
    }
}
