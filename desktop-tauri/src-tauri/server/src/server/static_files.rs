//! headless 形态的管理界面托管：把 `ui/` 静态目录随网关一并出出去。
//!
//! ── 为什么不用 tower-http 的 ServeDir ───────────────────────
//! 界面是一组固定扩展名的扁平文件，需要的只是「读文件 + content-type +
//! 路径安全」三件事，百来行自持实现足够；引 tower-http 只为这一件事，
//! 还得对齐它的 404 / 过滤器行为 —— 不划算（CORS 不用它也是同一理由）。
//!
//! ── 安全边界 ────────────────────────────────────────────────
//!   · 只服务 `ui/` 目录内的文件：路径逐段归一化，`..` 与绝不允许逃出根；
//!   · `/api`、`/v1`、`/auth`、`/health` 前缀不进静态服务 —— 静态 fallback
//!     只接「未注册路由」，但显式挡一道，避免将来某条管理路由改名后
//!     意外落进静态分支；
//!   · 注入的桥接脚本只在 index.html 上发生，其它原样透出。
//!
//! ── 缓存 ────────────────────────────────────────────────────
//! 分两档处理，既保证网页端升级后立即加载新版前端，也兼顾局域网直连的 304 速度：
//!   · 入口页（`.html`）与 Service Worker（`sw.js`）：响应 `Cache-Control:
//!     no-store, no-cache, must-revalidate` 且**不回 304**，保证浏览器、CDN 与
//!     反向代理每次都拿到最新的页面清单与资源版本；
//!   · 其余子资源（`.js` / `.css` / 图标 / 清单等）：带 ETag 与 `Cache-Control:
//!     no-cache, must-revalidate`，支持 If-None-Match 304 秒开；同时在服务 `.html`
//!     时为页面引用的本地静态资源自动追加 `?v=<版本+文件指纹>` 查询串 —— 即使前方
//!     CDN 给 `.js` / `.css` 覆盖了长 `max-age`，新版入口页给出的 `?v=` 也会命中
//!     全新缓存键，杜绝「后端已升级、前端仍读旧版」。
//! 压缩由 `compression` 中间件统一处理，这里只声明 `Vary: Accept-Encoding`。

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use axum::response::Response as AxumResponse;

use crate::server::errors;

/// 入口页与 Service Worker：禁存旧副本并跳过 304，确保更新后立刻切到新版
const CACHE_CONTROL_ENTRYPOINT: &str = "no-store, no-cache, must-revalidate";
/// 其余静态资源：每次校验，允许同版本 ETag 304 复用（跨版本由 HTML 注入 `?v=` 破除 CDN 缓存）
const CACHE_CONTROL_ASSET: &str = "no-cache, must-revalidate";
/// 同一路径的正文随客户端是否接受 gzip 而不同，缓存必须按这个头分桶。
const VARY: &str = "Accept-Encoding";

/// 判定路径是否属于「接口保留前缀」：静态服务绝不碰它们，
/// 未注册的接口路径继续走原来的 404 文案（Node 版行为）。
fn is_api_path(path: &str) -> bool {
    path == "/health"
        || path.starts_with("/api/")
        || path.starts_with("/v1/")
        || path.starts_with("/auth/")
        || path == "/api"
        || path == "/v1"
        || path == "/auth"
}

/// 静态服务入口：`ui_dir` 已就绪时尝试出文件，失败回到 404 兜底。
pub async fn serve(request: Request<Body>, ui_dir: PathBuf) -> AxumResponse {
    let path = request.uri().path().to_string();
    if is_api_path(&path) {
        return not_found(request).await;
    }
    let if_none_match = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let Some(file_path) = resolve_file(&ui_dir, &path) else {
        return not_found(request).await;
    };
    read_response(&ui_dir, &file_path, if_none_match.as_deref()).await
}

/// 把 URL 路径解析到 `ui/` 内的具体文件（`None` = 不存在 / 非法）。
///
/// 逐段归一化而不是字符串拼接：`..`、内嵌点段、重复斜杠都被消化掉，
/// 归一化结果仍带 `..` 前缀的（试图逃出根）一律拒绝。
fn resolve_file(ui_dir: &PathBuf, url_path: &str) -> Option<PathBuf> {
    // 只取路径段，忽略查询串（axum 的 uri().path() 已不含 query，双保险）
    let url_path = url_path.split('?').next().unwrap_or("");
    let mut segments: Vec<&str> = Vec::new();
    for segment in url_path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    let mut file_path = ui_dir.clone();
    if segments.is_empty() {
        file_path.push("index.html");
        return file_path.is_file().then_some(file_path);
    }
    for segment in &segments {
        // 任何一段都不该再带路径分隔符（Windows 反斜杠也算）
        if segment.contains('/') || segment.contains('\\') {
            return None;
        }
        file_path.push(segment);
    }
    if file_path.is_file() {
        return Some(file_path);
    }
    // 无扩展名的路径按页面补 .html（/login → login.html）：
    // 面板的独立登录页就是这种形态；带扩展名的请求不补（/app.js 照字面找）
    let no_extension = segments
        .last()
        .is_some_and(|segment| !segment.contains('.'));
    if no_extension {
        file_path.set_extension("html");
        return file_path.is_file().then_some(file_path);
    }
    None
}

/// 入口页与 Service Worker 不回 304、禁存旧副本；其余资源按 ETag 协商
fn is_entrypoint_file(file_path: &Path) -> bool {
    file_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("index.html")
                || name.to_ascii_lowercase().ends_with(".html")
                || name.eq_ignore_ascii_case("sw.js")
        })
}

fn cache_control_for(entrypoint: bool) -> &'static str {
    if entrypoint {
        CACHE_CONTROL_ENTRYPOINT
    } else {
        CACHE_CONTROL_ASSET
    }
}

/// 读文件并组装响应；读失败（权限等）按 404 处理（不区分 404/403，
/// 面板资源缺失的唯一正确动作就是「当它不存在」）。
///
/// 子资源 ETag 命中时直接回 304，不读文件正文 —— 这是「第二次打开面板不再重下
/// 几 MB 资源」的关键路径；入口页与 `sw.js` 永不 304（见模块头）。
async fn read_response(ui_dir: &Path, file_path: &PathBuf, if_none_match: Option<&str>) -> AxumResponse {
    let metadata = match tokio::fs::metadata(file_path).await {
        Ok(metadata) => metadata,
        Err(_) => return errors::not_found_response("GET", "static"),
    };
    let entrypoint = is_entrypoint_file(file_path);
    let etag = etag_of(&metadata);
    if !entrypoint && if_none_match.is_some_and(|value| etag_matches(value, &etag)) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .header(header::CACHE_CONTROL, cache_control_for(entrypoint))
            .header(header::VARY, VARY)
            .body(Body::empty())
            .unwrap_or_else(|_| errors::not_found_response("GET", "static"));
    }
    let bytes = match tokio::fs::read(file_path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return errors::not_found_response("GET", "static");
        }
    };
    let is_html = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html"));
    let is_index = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "index.html");
    let body = if is_index {
        stamp_local_assets(&inject_web_shim(&bytes), ui_dir)
    } else if is_html {
        stamp_local_assets(&bytes, ui_dir)
    } else {
        bytes
    };
    let mime = mime_of(file_path);
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, cache_control_for(entrypoint))
        .header(header::ETAG, &etag)
        .header(header::VARY, VARY)
        .header(header::CONTENT_LENGTH, body.len())
        .body(Body::from(body));
    response.unwrap_or_else(|_| errors::not_found_response("GET", "static"))
}

/// 文件版本标识：程序版本 + 修改时间 + 长度。
///
/// 不含文件内容哈希是刻意的：静态资源最大 2 MB 级，每次请求都算一遍哈希
/// 反而比直接读文件便宜不了多少；而 `ui/` 是随版本整体替换的，mtime 与长度
/// 足以区分新旧。加上程序版本是为了覆盖 `index.html` 的桥接注入脚本 ——
/// 那个脚本编在二进制里，升级后即使 html 文件没动，正文也可能变。
fn etag_of(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "W/\"{}-{:x}-{:x}\"",
        env!("CARGO_PKG_VERSION"),
        metadata.len(),
        modified
    )
}

/// `If-None-Match` 的匹配：支持 `*`、列表与弱校验前缀（`W/`）。
///
/// 两边都按弱比较（去掉 `W/`）处理：同一份文件的 gzip 与未压缩正文共用
/// 这个 ETag，弱校验正是「语义相同、字节可能不同」的表达。
fn etag_matches(header: &str, etag: &str) -> bool {
    let target = etag.strip_prefix("W/").unwrap_or(etag);
    header.split(',').any(|item| {
        let item = item.trim();
        item == "*" || item.strip_prefix("W/").unwrap_or(item).trim() == target
    })
}

/// 给 HTML 里引用的**本地**静态资源追加 `?v=<版本+指纹>` 查询串。
///
/// 只处理 `href` / `src` 指向的同源相对路径（`/` 开头或同级相对路径），
/// 跳过外链、`data:`、已带查询串与锚点的写法；找不到对应文件时保持原样。
fn stamp_local_assets(html: &[u8], ui_dir: &Path) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(html) else {
        return html.to_vec();
    };
    let mut output = String::with_capacity(text.len() + 512);
    let mut rest = text;
    while let Some(index) = find_attr(rest) {
        let (before, tail) = rest.split_at(index);
        output.push_str(before);
        let (url_start, url_end) = match attribute_value(tail) {
            Some(bounds) => bounds,
            None => {
                output.push_str(tail);
                rest = "";
                break;
            }
        };
        let url = &tail[url_start..url_end];
        match stamped_url(ui_dir, url) {
            Some(stamped) => {
                output.push_str(&tail[..url_start]);
                output.push_str(&stamped);
                output.push_str(&tail[url_end..]);
            }
            None => output.push_str(&tail[..url_end]),
        }
        rest = &tail[url_end..];
    }
    output.push_str(rest);
    output.into_bytes()
}

/// 找下一个 `href="` / `src="`（单双引号都认），返回属性名起始下标。
///
/// 属性名前必须是分隔符（空白 / `<`），避免把 `data-href`、`x-src` 一类
/// 自定义属性当成资源引用。
fn find_attr(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let boundary = index == 0 || matches!(bytes[index - 1], b' ' | b'\t' | b'\n' | b'\r' | b'<');
        if boundary {
            for name in [b"href".as_slice(), b"src".as_slice()] {
                if bytes[index..].starts_with(name) {
                    let after = index + name.len();
                    if matches!(bytes.get(after), Some(b'=')) {
                        return Some(index);
                    }
                }
            }
        }
        index += 1;
    }
    None
}

/// 取属性值在 `tail`（从属性名开始）里的下标区间
fn attribute_value(tail: &str) -> Option<(usize, usize)> {
    let bytes = tail.as_bytes();
    let eq = bytes.iter().position(|byte| *byte == b'=')?;
    let mut start = eq + 1;
    while matches!(bytes.get(start), Some(b' ' | b'\t')) {
        start += 1;
    }
    let quote = *bytes.get(start)?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let value_start = start + 1;
    let value_end = value_start + bytes[value_start..].iter().position(|byte| *byte == quote)?;
    Some((value_start, value_end))
}

/// 把一个本地静态资源 URL 换成带 `?v=` 的形态；不该处理时返回 `None`
fn stamped_url(ui_dir: &Path, url: &str) -> Option<String> {
    if url.is_empty()
        || url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("//")
        || url.starts_with("data:")
        || url.starts_with('#')
        || url.contains('?')
        || url.contains('#')
    {
        return None;
    }
    let trimmed = url.trim_start_matches("./").trim_start_matches('/');
    if trimmed.is_empty() || !trimmed.contains('.') {
        return None;
    }
    let candidate = ui_dir.join(trimmed);
    let metadata = std::fs::metadata(&candidate).ok()?;
    let stamp = etag_of(&metadata);
    // ETag 形如 `W/"版本-长度-时间"`，去掉包围的 `W/"` 与 `"` 作为查询串
    let value = stamp.trim_start_matches("W/\"").trim_end_matches('"');
    Some(format!("{url}?v={value}"))
}

/// 把网页端桥接注入 index.html：`<head>` 开标签后插一段内联脚本，
/// 使 `window.workbuddyDesktop` 在所有界面脚本之前就绪
/// （与桌面壳 bridge.rs 的「页面脚本执行前注入」同一时序保证）。
fn inject_web_shim(html: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(html) else {
        return html.to_vec();
    };
    let shim = format!("<script>{}</script>", crate::web_shim::shim_js());
    match text.replacen("<head>", &format!("<head>{shim}"), 1) {
        replaced if replaced != text => replaced.into_bytes(),
        // 没有 <head>（结构被改坏）也别让面板整页失效：追加到末尾兜底
        _ => format!("{text}<script>{}</script>", crate::web_shim::shim_js()).into_bytes(),
    }
}

/// 扩展名 → content-type（界面用到的固定集合，其余按二进制流处理）
fn mime_of(file_path: &PathBuf) -> String {
    let extension = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        // PWA 清单（`ui/manifest.webmanifest`）。规范类型是 `application/manifest+json`，
        // 浏览器对 `application/json` 也宽松接受；但**落到下面 `_` 那支的
        // `application/octet-stream` 不是 JSON 类型** —— 清单被判解析失败，
        // 「添加到主屏幕」驱动的显示名 / 主题色 / 图标清单整套读不出来
        // （iOS 那条路走 `apple-touch-icon` + `apple-mobile-web-app-*`，不受影响）。
        "webmanifest" => "application/manifest+json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
    .to_string()
}

async fn not_found(request: Request<Body>) -> AxumResponse {
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    errors::not_found_response(&method, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例独立目录：测试并行执行，共用一个目录会被另一用例的清理删掉
    fn fixture(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("a2api-static-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("css")).expect("fixture dir");
        std::fs::write(dir.join("css/app.css"), b"body{}").expect("fixture css");
        dir
    }

    #[test]
    fn stamps_local_assets_and_leaves_external_untouched() {
        let dir = fixture("stamp");
        let html = br#"<link href="css/app.css"><link href="https://cdn.example/x.css"><script src="missing.js"></script>"#;
        let out = String::from_utf8(stamp_local_assets(html, &dir)).expect("utf8");
        assert!(out.contains("css/app.css?v="), "{out}");
        assert!(out.contains("https://cdn.example/x.css\""), "{out}");
        assert!(out.contains("src=\"missing.js\""), "不存在的文件保持原样: {out}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn does_not_touch_custom_attributes_or_existing_queries() {
        let dir = fixture("skip");
        let html = br#"<div data-href="css/app.css"></div><link href="css/app.css?v=1">"#;
        let out = String::from_utf8(stamp_local_assets(html, &dir)).expect("utf8");
        assert_eq!(out, String::from_utf8_lossy(html), "自定义属性与已带查询串不应改写");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn entrypoint_files_skip_validation() {
        assert!(is_entrypoint_file(Path::new("index.html")));
        assert!(is_entrypoint_file(Path::new("login.html")));
        assert!(is_entrypoint_file(Path::new("sw.js")));
        assert!(!is_entrypoint_file(Path::new("islands/ui.js")));
        assert_eq!(cache_control_for(true), CACHE_CONTROL_ENTRYPOINT);
        assert_eq!(cache_control_for(false), CACHE_CONTROL_ASSET);
    }
}
