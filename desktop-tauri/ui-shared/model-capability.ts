export const CAPABILITY_KEYS = [
  'maxInputTokens',
  'maxOutputTokens',
  'supportsToolCall',
  'supportsImages',
  'supportsVideo',
  'supportsReasoning',
] as const

export type CapabilityKey = (typeof CAPABILITY_KEYS)[number]
export type Capabilities = Partial<Record<CapabilityKey, number | boolean | null>>
export type CapabilitySource = 'upstream' | 'modelsdev' | 'user'

/**
 * 思考档位表在覆盖层里用的键名（与后端 `core::capability::REASONING_LEVELS_KEY`
 * 逐字一致）。
 *
 * 它不是「能力位」：值是**字符串数组**（这个模型认识哪几档），出口键名也另定
 * （`/v1/models` 的 `reasoning_efforts`）。混进 `CAPABILITY_KEYS` 会让六项展示、
 * 三态判定与来源标记全部多出一堆数组特例，所以它单列在下面那组常量里。
 */
export const REASONING_LEVELS_KEY = 'reasoningLevels'

/** 档位表的条数与单条字符上限（与后端同一口径）。 */
export const MAX_REASONING_LEVELS = 32
export const MAX_REASONING_LEVEL_CHARS = 32

/**
 * 归一思考档位表：逐项去空白、丢弃空项、忽略大小写去重（保留首次出现的写法），
 * 原样保留用户写的拼写与顺序。非数组一律给空表。
 *
 * 与后端 `capability::normalize_levels` 同口径；这里更宽松的地方只有一条：
 * 空项与重复项直接跳过（后端会拒收），因为界面上删到一半的行、连点两次
 * 「新增」都可能临时产生它们，没必要让用户先自己清干净。
 */
export function normalizeReasoningLevels(raw: unknown): string[] {
  if (!Array.isArray(raw)) return []
  const levels: string[] = []
  for (const item of raw) {
    const level = typeof item === 'string' ? item.trim() : ''
    if (!level || level.length > MAX_REASONING_LEVEL_CHARS) continue
    if (levels.some(existing => existing.toLowerCase() === level.toLowerCase())) continue
    levels.push(level)
    if (levels.length >= MAX_REASONING_LEVELS) break
  }
  return levels
}

export const TOKEN_KEYS: readonly CapabilityKey[] = ['maxInputTokens', 'maxOutputTokens']
export const BOOLEAN_KEYS: readonly CapabilityKey[] = [
  'supportsToolCall', 'supportsImages', 'supportsVideo', 'supportsReasoning',
]

// 与后端 core::capability::MAX_TOKEN_VALUE 保持一致。
export const MAX_TOKEN_VALUE = 100_000_000

export function isTokenKey(key: CapabilityKey): boolean {
  return TOKEN_KEYS.includes(key)
}

export function normalizeCapability(key: CapabilityKey, value: unknown): number | boolean | null {
  if (isTokenKey(key)) {
    const number = typeof value === 'number' ? value : Number(value)
    if (!Number.isFinite(number) || number < 1 || number > MAX_TOKEN_VALUE) return null
    return Math.trunc(number)
  }
  return typeof value === 'boolean' ? value : null
}

export function normalizeCapabilities(raw: unknown): Capabilities {
  const result: Capabilities = {}
  if (!raw || typeof raw !== 'object') return result
  const source = raw as Record<string, unknown>
  for (const key of CAPABILITY_KEYS) {
    const value = normalizeCapability(key, source[key])
    if (value !== null) result[key] = value
  }
  return result
}

export function normalizeOverrides(raw: unknown): CapabilityKey[] {
  if (!Array.isArray(raw)) return []
  return CAPABILITY_KEYS.filter(key => raw.some(item => item === key))
}

export function capSourceOf(
  model: { capSources?: unknown } | null | undefined,
  key: CapabilityKey,
): CapabilitySource | '' {
  const raw = (model?.capSources as Record<string, unknown> | undefined)?.[key]
  return raw === 'upstream' || raw === 'modelsdev' || raw === 'user' ? raw : ''
}
