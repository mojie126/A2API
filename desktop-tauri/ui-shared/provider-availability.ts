// 稳定 ID 仍用于存量账号；Rust 执行体恢复前不开放添加和调用。
// Kiro 已由「提供商增强」插件实现（2026-11），与后端
// `core/providers/availability.rs` 的 SUSPENDED_IDS 同步移出。
//
// 2026-11 又移出八家（cursor / devin / factory / zed / grok / kimi-work /
// opendesign / nous-portal）：它们由 Magpie 社区插件接手（见
// `script::catalog::recommendations`），后端同一份名单已放开 —— 两边必须一致，
// 只放开后端会让可用账号在添加页消失。Claude、ChatGPT、Codex 已由本体执行；
// Copilot 和 Gemini 的订阅身份仍停用，不与同品牌插件身份混用。
export const SUSPENDED_PROVIDER_IDS: ReadonlySet<string> = new Set([
  // 千问办公国际版已由「提供商增强」插件实现（设备授权型登录，不依赖本机
  // 回调），与后端 `core/providers/availability.rs` 的 SUSPENDED_IDS 一致地
  // 不在名单里；企业版 (`qwenwork-enterprise`) 仍待独立接入。
  'qwenwork-enterprise',
  'copilot-subscription',
  'gemini-subscription',
  // AutoClaw 国际版：上游已停止签发国际版授权，换码恒定返回 631001，
  // 与后端同一份停用名单一致地摘出添加入口（存量账号保留）。
  'autoclaw-intl',
])

export function isSuspendedProvider(id: string): boolean {
  return SUSPENDED_PROVIDER_IDS.has(id)
}

export function isAddAccountProviderHidden(id: string): boolean {
  return isSuspendedProvider(id)
}
