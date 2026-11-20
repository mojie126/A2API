//! 插件支持状态判定。
//!
//! ── 本地 fork 与上游的分歧（2026-11-20 合并 beta.2 时确定）─────────
//! 上游 beta.2 起把提供商插件全面转成独立 WASM 包，网关层加了 `require_wasm`
//! 与 `LegacyUnsupported` 两道闸：非 WASM 的包被拒装、已装的原生包被判为
//! 「旧原生插件已停止支持」。本 fork 的插件仓库（Agent2Providers）仍以原生
//! 插件为主流程（`crates/providers` 而非 `wasm/`），因此这里保留原生插件的
//! 安装与运行能力：
//!   · `require_wasm` 只做协议版本之外的校验放行，原生包照常安装；
//!   · `installed_state` 一律返回 Supported，原生包不被标记为弃用；
//!   · `legacy_reason` 恒为 None，界面不出现「已停止支持」的提示。
//! 上游那套 WASM 专属的批量安装与回滚能力仍在 `plugin_host/packages` 里可用，
//! 只是不以「非 WASM 即弃用」为前提。

use agent2api_provider_protocol as proto;
use serde::Serialize;

use super::install::InstalledPack;

/// 早期给「非 WASM 包」用的弃用文案。本 fork 保留原生插件，不再使用，
/// 但常量留着以维持上游代码里对它的引用不报未使用。
pub(super) const LEGACY_REASON: &str = "旧原生提供商插件已停止支持，请安装对应的 WASM 插件；已有账号和配置保留。";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackSupportState {
    Supported,
    LegacyUnsupported,
}

/// 本 fork 接受原生与 WASM 两种插件形态，不做 WASM-only 限制。
pub(super) fn require_wasm(_manifest: &proto::PackManifest) -> Result<(), String> {
    Ok(())
}

pub(crate) fn installed_state(_pack: &InstalledPack) -> PackSupportState {
    // 原生包与 WASM 包同等支持
    PackSupportState::Supported
}

pub(super) fn legacy_reason(_pack: &InstalledPack) -> Option<String> {
    None
}