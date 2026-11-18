/**
 * 账号页代理列的 WARP 设备清单。
 *
 * 与「网络代理」页的 WARP 分页读同一个网关接口（`GET /api/proxies/warp`），
 * 但这里只保留代理下拉需要的摘要：设备 id、名称与出口地址。设备由出口池在
 * 运行期注册，重启后重建、端口也会变，所以缓存只做展示，解析与端口的最终
 * 事实始终在网关侧（账号代理只存设备 id）。
 *
 * 失败自愈与代理池缓存同一口径：没有缓存时按 30 秒节流补拉，失败把原因
 * 通知出去，代理列显示「读取失败（重试中）」。
 */

import { t } from '../i18n'
import { shared } from './accounts-shared'

/** 下拉可展示的一份设备摘要 */
export type WarpExitItem = {
  id: string
  label: string
  egressIp: string
  port: number
  endpoint: string
}

/** 已缓存的设备清单（`null` = 还没读过） */
let cache: WarpExitItem[] | null = null
/** 在途读取（并发调用合并成一次） */
let inflight: Promise<{ items: WarpExitItem[]; ready: boolean }> | null = null
/** 出口池是否已就绪（`false` = 正在注册设备与建隧道，清单为空不代表「没有设备」） */
let ready = true
/** 读取失败的原因（成功后清空） */
let lastError: string | null = null
/** 自愈节流：没有它，「失败 → 重画 → 又拉」会变成死循环 */
let retryAt = 0
/** 出口池准备期的重试定时器（只按 30 秒补拉，不打扰代理下拉） */
let retryTimer: ReturnType<typeof setTimeout> | null = null
let bridgeWarned = false

let lastSignature: string | null = null
/** 内容版本号：每次内容变化 +1，代理单元格据此判断是否重绘 */
let version = 0

const listeners = new Set<() => void>()

function signatureOf(items: WarpExitItem[], ready: boolean): string {
  return (ready ? '1' : '0') + '#' + items
    .map(item => [item.id, item.label, item.egressIp, item.port, item.endpoint].join(':'))
    .join('|')
}

function unchanged(items: WarpExitItem[], nextReady: boolean): boolean {
  return lastSignature !== null && signatureOf(items, nextReady) === lastSignature
}

function remember(items: WarpExitItem[], nextReady: boolean): void {
  lastSignature = signatureOf(items, nextReady)
}

export function subscribeWarpExits(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

function emit(): void {
  version += 1
  listeners.forEach(listener => listener())
}

/** 已缓存的设备清单（`null` = 还没读过） */
export function warpExitsSnapshot(): WarpExitItem[] | null {
  return cache
}

/** 设备清单读取失败的原因（`null` = 没失败） */
export function warpExitsError(): string | null {
  return lastError
}

/** 出口池是否已就绪（`false` = 正在注册设备，清单暂为空） */
export function warpExitsReady(): boolean {
  return ready
}

/** 内容版本号（每次变化 +1）：代理单元格的 memo 比较读它 */
export function warpExitsVersion(): number {
  return version
}

/**
 * 设备的展示文案：出口 IP 是同一台设备最有辨识度的读数（设备名只有序号）。
 * 还没探测到出口 IP 时只写名字。
 */
export function warpExitLabel(item: WarpExitItem): string {
  return item.egressIp ? `WARP · ${item.label}（${item.egressIp}）` : `WARP · ${item.label}`
}

function normalize(data: unknown): { items: WarpExitItem[]; ready: boolean } {
  const payload = data && typeof data === 'object' ? data as { exits?: unknown; ready?: unknown } : null
  const rows = Array.isArray(payload?.exits) ? payload.exits : []
  const items: WarpExitItem[] = []
  for (const row of rows) {
    const item = row && typeof row === 'object' ? row as Record<string, unknown> : {}
    const id = String(item.id || '').trim()
    if (!id) continue
    items.push({
      id,
      label: String(item.label || '').trim() || id,
      egressIp: String(item.egressIp || '').trim(),
      port: Number(item.port) || 0,
      endpoint: String(item.endpoint || '').trim(),
    })
  }
  return { items, ready: payload?.ready !== false }
}

/**
 * 读一次设备清单（共享缓存；并发调用合并成一次）。
 *
 * 桥异常时可能 resolve 出 `undefined`（而不是 reject），那种情况按失败处理。
 */
export async function warpExits(options: { force?: boolean } = {}): Promise<WarpExitItem[]> {
  // 未就绪时缓存只是「准备中」的空读数，不能当最终结果早退 —— 否则准备期
  // 的补拉永远只拿到那份空缓存
  if (!options.force && cache && ready) return cache
  if (!inflight) {
    inflight = (async (): Promise<{ items: WarpExitItem[]; ready: boolean }> => {
      try {
        const data = await shared().workbuddyDesktop?.getWarpExits?.()
        if (!data || typeof data !== 'object') {
          throw new Error(t('WARP 设备清单响应异常（{type}）', { type: typeof data }))
        }
        return normalize(data)
      } finally {
        inflight = null
      }
    })()
  }
  const next = await inflight
  lastError = null
  cache = next.items
  ready = next.ready
  if (!unchanged(next.items, next.ready)) {
    remember(next.items, next.ready)
    emit()
  }
  return next.items
}

/**
 * 出口池准备期的补拉：注册设备与建隧道要几十秒，期间清单为空。
 * 只排一个 30 秒后的重试，就绪后自然停止。
 */
function scheduleReadyRetry(): void {
  if (retryTimer) return
  retryTimer = setTimeout(() => {
    retryTimer = null
    ensureWarpExits()
  }, 30_000)
}

/** 代理列的补拉（没读到就补，节流 30 秒） */
export function ensureWarpExits(): void {
  // 已就绪且读过：不再重拉（切页 / 准备期结束由 refreshWarpExits 或定时器负责）
  if (cache && ready) return
  if (Date.now() < retryAt) return
  if (typeof shared().workbuddyDesktop?.getWarpExits !== 'function') {
    // 桥没挂上（加载失败等）：本页面会话内不再尝试，并告警一次
    if (!bridgeWarned) {
      bridgeWarned = true
      console.warn('[accounts] 桥未提供 getWarpExits：代理列的「WARP 设备」选项不可用')
    }
    retryAt = Infinity
    return
  }
  retryAt = Date.now() + 30_000
  void warpExits()
    .then(() => {
      if (ready) { retryAt = 0; return }
      // 池还没就绪：保持节流，到点再补一次
      scheduleReadyRetry()
    })
    .catch(error => {
      lastError = error instanceof Error ? error.message : String(error)
      emit()
    })
}

/**
 * 强制重读一次（切回账号页时调）。
 *
 * 与 `ensureWarpExits` 的区别：这个一定发请求；不清缓存，失败时旧列表仍可用。
 */
export async function refreshWarpExits(): Promise<void> {
  retryAt = 0
  try {
    await warpExits({ force: true })
    if (!ready) {
      retryAt = Date.now() + 30_000
      scheduleReadyRetry()
    }
  } catch (error) {
    lastError = error instanceof Error ? error.message : String(error)
    emit()
  }
}
