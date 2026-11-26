/**
 * 弹窗活跃标记与设备性能档位。
 *
 * ── 为什么需要这一层 ─────────────────────────────────────────
 * 遮罩用的是 `backdrop-filter`：它对元素**背后**的像素做滤镜，所以只要下层
 * 有任何一帧变化，浏览器就要把整个视口重新采样并模糊一次。而面板里有一串
 * 周期性重绘（20 秒全局 `refresh()`、5 秒在途请求角标、账号页 2 秒连接数、
 * 日志页自己的轮询），它们在弹窗打开期间照旧跑，于是「打开弹窗」等于把
 * 「每秒都在重算全屏模糊」这个代价挂上。
 *
 * 这里维护一个全屏弹窗的**存活计数**，由弹窗组件在挂载/卸载时增减：
 *   · 计数器 > 0 时给 `<html>` 打 `data-modal-active`，各定时轮询据此跳过；
 *   · 计数归零时移除标记，轮询自然恢复。
 * 用计数而不是布尔，是因为弹窗可以嵌套（设置弹窗里再开确认框）。
 *
 * ── 低配降级 ────────────────────────────────────────────────
 * 遮罩模糊与入场动画在低配机器上是纯成本。首帧用
 * `hardwareConcurrency` 与 `deviceMemory` 粗判一次，判定为低配就挂
 * `data-lite`，样式层据此关掉模糊与动画（见 ui-kit/styles/primitives.css）。
 * 这两个信号都拿不到时按「正常」处理 —— 宁可多算一点，也不要把正常机器
 * 误降级成没有过渡效果。
 */
export const MODAL_ACTIVE_ATTRIBUTE = 'data-modal-active'
export const LITE_ATTRIBUTE = 'data-lite'

const MODAL_EVENT = 'wb:modal-active'

let modalCount = 0

/**
 * 真实打开中的全屏弹窗：Base UI 的 Popup 在打开时挂 `data-open`，
 * 关闭后变成 `data-closed`（keepMounted 时内容仍在 DOM 里）。
 */
const OPEN_MODAL_SELECTOR = [
  '[data-slot="dialog-content"][data-open]',
  '[data-slot="sheet-content"][data-open]',
  '[data-slot="alert-dialog-content"][data-open]',
].join(',')

/**
 * 当前是否有全屏弹窗打开（供非 React 侧的经典脚本查询）。
 *
 * 以**弹窗自身的 `data-open` 属性**为唯一事实来源：组件是否挂载、计数是否
 * 归零都与「用户眼前是否真有弹窗」无关（`keepMounted` 的弹窗关闭后组件仍在，
 * 历史上有过一次 sync 失败也会让计数停在正数）。只看计数会把各面板的实时
 * 刷新与轮询永久按住 —— 账号页连接数、路由在途、请求日志都不再更新。
 * 没有打开中的弹窗时顺带把计数归零，让后续弹窗的开关重新对齐。
 */
export function isModalActive(): boolean {
  if (typeof document === 'undefined') return modalCount > 0
  if (document.querySelector(OPEN_MODAL_SELECTOR)) return true
  modalCount = 0
  return false
}

/**
 * 增减弹窗计数并把结果同步到 `<html>`。
 *
 * 返回移除函数，调用方在 effect 清理里调它。重复调用移除函数是安全的
 * （弹窗被连续关两次不会把计数减成负数）。
 */
export function holdModalActive(): () => void {
  let released = false
  modalCount += 1
  sync()
  return () => {
    if (released) return
    released = true
    modalCount = Math.max(0, modalCount - 1)
    sync()
  }
}

/** 订阅弹窗开关事件：`detail.active` 是当前是否仍有弹窗打开 */
export function onModalActiveChange(listener: (active: boolean) => void): () => void {
  const handler = (event: Event) => listener((event as CustomEvent<{ active: boolean }>).detail.active)
  window.addEventListener(MODAL_EVENT, handler)
  return () => window.removeEventListener(MODAL_EVENT, handler)
}

function sync(): void {
  if (typeof document === 'undefined' || !document.documentElement) return
  const root = document.documentElement
  const active = modalCount > 0
  if (active) root.setAttribute(MODAL_ACTIVE_ATTRIBUTE, '')
  else root.removeAttribute(MODAL_ACTIVE_ATTRIBUTE)
  window.dispatchEvent(new CustomEvent(MODAL_EVENT, { detail: { active } }))
}

/**
 * 判定是否低配设备并挂 `data-lite`。
 *
 * 阈值取「能拿到的信号里偏保守的一半」：4 线程、4 GB 内存。这两个属性在
 * 部分浏览器缺失（Safari 没有 `deviceMemory`），缺失就只按拿到的那个判，
 * 两个都缺失则判定为正常设备。
 */
export function applyDeviceTier(): void {
  if (typeof document === 'undefined') return
  const cores = Number(navigator.hardwareConcurrency) || 0
  const memory = Number((navigator as Navigator & { deviceMemory?: number }).deviceMemory) || 0
  const lowCores = cores > 0 && cores <= 4
  const lowMemory = memory > 0 && memory <= 4
  if (lowCores || lowMemory) document.documentElement.setAttribute(LITE_ATTRIBUTE, '')
  else document.documentElement.removeAttribute(LITE_ATTRIBUTE)
}