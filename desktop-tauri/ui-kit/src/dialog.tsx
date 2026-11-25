import * as React from 'react'
import { Dialog as DialogPrimitive } from '@base-ui/react/dialog'
import { cn } from './lib/cn'
import { Button } from './button'
import { t } from './i18n'
import { holdModalActive } from './lib/modal-activity'

/**
 * 弹窗（标准形态，与 shadcn/ui 的 Dialog 一致）。
 *
 * 部件划分照标准：Dialog / DialogTrigger / DialogPortal / DialogClose /
 * DialogOverlay / DialogContent / DialogHeader / DialogTitle / DialogDescription /
 * DialogFooter。DialogContent 默认自带遮罩与右上角关闭按钮（showCloseButton），
 * 不需要时传 false。
 *
 * 另外两个是本项目的扩展件，对应 ui/css/components.css 里的弹窗结构，
 * shadcn 标准里没有：
 *   · DialogBody    ← `.modal-body`（纵向排布、超高滚动的正文区）
 *   · DialogSection ← `.modal-section`（正文里的分区块，内嵌凹槽底 + 描边）
 *
 * 视觉对齐既有的 `.modal-mask` / `.modal` 一族：遮罩 46% 黑、弹窗宽
 * min(620px, 100%)、--r-lg 圆角、--shadow-3 投影；入场是「淡入 + 上移 10px
 * 收 1.5%」，出场反向。z-index 取 30（同 .modal-mask 档，轻提示在 40 之上）。
 *
 * ── 遮罩不再做背景模糊 ────────────────────────────────────────
 * 原先遮罩挂 `backdrop-filter: blur(3px)`。该属性按定义要对元素背后的像素
 * 重新采样，下层任何一帧变化都要把整个视口重算一次；而面板里的定时轮询
 * （全局 20 秒、在途请求 5 秒、账号页 2 秒）在弹窗打开期间照旧跑，等于把
 * 「每秒重算全屏模糊」挂在弹窗活着的整段时间上。模糊已去掉，改用纯色遮罩；
 * 需要压暗靠 46% 黑本身就够。若将来要恢复，先读 lib/modal-activity.ts 的说明。
 *
 * ── 出场不用关键帧 ──────────────────────────────────────────
 * tw-animate 的 `enter` / `exit` 关键帧里带 `filter: blur(...)` 声明，即使值
 * 为 0 也会让元素单独建层并逐帧计算滤镜。这里改用 transition + 状态数据属性，
 * 出场只是 opacity 与 transform 的插值，没有滤镜参与。
 *
 * 面板记录（`lib/modal-activity.ts`）在 DialogContent 挂载期间累加，弹窗打开
 * 时给 `<html>` 打标记，各定时轮询据此暂停。
 *
 * 交互交给 Base UI 的 Dialog：焦点陷阱、Esc 关闭、滚动锁定、aria-modal 与
 * 关闭后焦点归位全部内建。
 */

function Dialog(props: DialogPrimitive.Root.Props) {
  return <DialogPrimitive.Root data-slot='dialog' {...props} />
}

function DialogTrigger(props: DialogPrimitive.Trigger.Props) {
  return <DialogPrimitive.Trigger data-slot='dialog-trigger' {...props} />
}

function DialogPortal(props: DialogPrimitive.Portal.Props) {
  return <DialogPrimitive.Portal data-slot='dialog-portal' {...props} />
}

function DialogClose(props: DialogPrimitive.Close.Props) {
  return <DialogPrimitive.Close data-slot='dialog-close' {...props} />
}

const DialogTitle = (props: DialogPrimitive.Title.Props) => (
  <DialogPrimitive.Title data-slot='dialog-title' {...props} />
)

const DialogDescription = (props: DialogPrimitive.Description.Props) => (
  <DialogPrimitive.Description data-slot='dialog-description' {...props} />
)

function DialogOverlay({ className, ...props }: DialogPrimitive.Backdrop.Props) {
  // 纯色遮罩：不再做 backdrop-filter。原因见模块头「遮罩不再做背景模糊」。
  // 出场只淡出 opacity。Base UI 的出场状态属性是 `data-ending-style`
  // （`data-closed` 只在真正关闭后出现，那时元素已被卸载/隐藏）。
  return (
    <DialogPrimitive.Backdrop
      data-slot='dialog-overlay'
      className={cn(
        'fixed inset-0 isolate z-30 bg-mask',
        'transition-opacity duration-150',
        'starting:opacity-0 data-ending-style:opacity-0',
        className
      )}
      {...props}
    />
  )
}

type DialogContentProps = DialogPrimitive.Popup.Props & {
  /** 是否自带右上角关闭按钮，默认 true */
  showCloseButton?: boolean
  /**
   * 关闭后是否把 Portal 留在 DOM 里（内容保留、只是隐藏），默认 false（关闭即卸载）。
   *
   * 需要它的场景：弹窗里挂着一份**有状态的第三方 DOM**（比如登录引擎建的容器，
   * 里面有它自己持有的 deviceId、发码冷却）。关闭即卸载会把这些一起清掉，
   * 用户「发码 → 关掉 → 重新打开 → 填码」就会拿到一个已经作废的 deviceId。
   * 打开这一档，内容留着、状态就还在（Base UI 会把隐藏时的内容标记为 inert，
   * 键盘与读屏不会跑进去）。
   */
  keepMounted?: boolean
  /**
   * 打开期间是否暂停后台轮询（`data-modal-active`），默认 true。
   *
   * 与 `keepMounted` 配合时必须传实际的打开状态：Portal 保持挂载时组件不会
   * 卸载，按挂载计数的弹窗标记会一直留在 `<html>` 上，把各面板的定时刷新与
   * 实时事件全部按住（连接数不更新、请求日志停在加载文案）。
   */
  pauseBackground?: boolean
  /**
   * 嵌在另一层弹窗里时也画自己那层遮罩，默认 false。
   *
   * Base UI 的嵌套约定是「子层不画遮罩」，好让父层干净地留在后面（见 DialogBackdrop
   * 的 `enabled: forceRender || !nested`）。但两件事要落在遮罩上：
   *   · 压暗整个下层（含父弹窗）—— 没有遮罩就只有换了个框，层叠感靠不住；
   *   · 点空白处关闭这一层（useDialogRoot 的 outsidePress 认的就是遮罩本身：
   *     `backdrop === target`），没有遮罩时点空白处什么都不会发生。
   * 子层的遮罩排在父层 Popup 之后，颜色与模糊与普通弹窗同一套，因此父层是被压暗
   * 的一层、子层浮在上面。
   */
  overlayForceRender?: boolean
}

function DialogContent({
  className,
  children,
  showCloseButton = true,
  keepMounted = false,
  pauseBackground = true,
  overlayForceRender = false,
  ...props
}: DialogContentProps) {
  // 弹窗打开期间标记 `<html data-modal-active>`，各定时轮询据此暂停：
  // 轮询重绘下层会让遮罩重新合成，高负载下这就是弹窗卡顿的主要来源。
  // `keepMounted` 时 Portal 关闭后仍在 DOM 里，所以标记必须跟随 `pauseBackground`
  // （实际打开状态），不能只按挂载累加。
  React.useEffect(() => {
    if (!pauseBackground) return
    return holdModalActive()
  }, [pauseBackground])
  return (
    <DialogPortal keepMounted={keepMounted}>
      <DialogOverlay forceRender={overlayForceRender} />
      <DialogPrimitive.Popup
        data-slot='dialog-content'
        className={cn(
          'fixed top-1/2 left-1/2 z-30 flex max-h-[calc(100vh-48px)] w-[min(620px,calc(100vw-48px))] -translate-x-1/2 -translate-y-1/2 flex-col',
          'overflow-hidden rounded-lg border border-border bg-surface shadow-3 outline-none',
          // 只用 transition，不用 tw-animate 的关键帧：enter/exit 关键帧带
          // `filter: blur()` 声明，即使为 0 也会逐帧计算滤镜。
          'transition-[opacity,scale] duration-200 [transition-timing-function:cubic-bezier(.2,.9,.3,1)]',
          'starting:opacity-0 starting:scale-[.985]',
          'data-ending-style:opacity-0 data-ending-style:scale-[.985]',
          className
        )}
        {...props}
      >
        {children}
        {showCloseButton && (
          <DialogPrimitive.Close
            data-slot='dialog-close'
            render={<Button variant='ghost' size='icon-sm' className='absolute top-3 right-3' />}
          >
            <svg viewBox='0 0 12 12' className='size-3' aria-hidden='true'>
              <path
                d='M2.5 2.5l7 7M9.5 2.5l-7 7'
                stroke='currentColor'
                strokeWidth='1.5'
                strokeLinecap='round'
                fill='none'
              />
            </svg>
            <span className='sr-only'>{t('关闭')}</span>
          </DialogPrimitive.Close>
        )}
      </DialogPrimitive.Popup>
    </DialogPortal>
  )
}

/** 弹窗标题栏：标题左、操作右，底部一条 hairline。
    右侧留 40px 给 DialogContent 自带的关闭按钮（26px 按钮 + 12px 外边距 + 2px 间隙），
    标题再长也不会压到它下面。 */
function DialogHeader({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot='dialog-header'
      className={cn(
        'flex items-center gap-2.5 border-b border-hairline px-5 py-[15px] pr-10',
        '[&_h2]:flex-1 [&_h2]:text-[14.5px] [&_h2]:font-semibold',
        className
      )}
      {...props}
    />
  )
}

/** 弹窗正文（项目扩展，对应 `.modal-body`）：纵向排布、超高滚动 */
function DialogBody({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot='dialog-body'
      className={cn('flex flex-col gap-4 overflow-y-auto px-5 py-[18px]', className)}
      {...props}
    />
  )
}

/** 弹窗底栏：按钮靠右，左侧留白用 mr-auto 顶开 */
function DialogFooter({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot='dialog-footer'
      className={cn(
        'flex items-center justify-end gap-2 border-t border-hairline bg-surface-2 px-5 py-[13px]',
        className
      )}
      {...props}
    />
  )
}

/** 正文里的分区块（项目扩展，对应 `.modal-section`）：内嵌凹槽底 + 描边 */
function DialogSection({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot='dialog-section'
      className={cn(
        'flex flex-col gap-2.5 rounded-md border border-border bg-surface-inset p-3.5',
        '[&_h3]:text-[12.5px] [&_h3]:font-semibold',
        '[&_p]:text-xs [&_p]:leading-[1.65] [&_p]:text-subtle',
        className
      )}
      {...props}
    />
  )
}

export {
  Dialog,
  DialogTrigger,
  DialogPortal,
  DialogClose,
  DialogOverlay,
  DialogContent,
  DialogTitle,
  DialogDescription,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSection,
}
