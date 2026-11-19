import * as React from 'react'
import { Dialog as DialogPrimitive } from '@base-ui/react/dialog'
import { cn } from './lib/cn'
import { Button } from './button'
import { t } from './i18n'
import { holdModalActive } from './lib/modal-activity'

/**
 * 底部抽屉（Sheet）—— 与 shadcn/ui 的 Sheet 同形，但**贴底展开**而不是侧滑。
 *
 * 部件划分照 shadcn 标准：Sheet / SheetTrigger / SheetClose / SheetPortal /
 * SheetOverlay / SheetContent / SheetHeader / SheetTitle / SheetDescription /
 * SheetBody / SheetFooter。SheetContent 默认自带遮罩与右上角关闭按钮。
 *
 * ── 为什么要它 ────────────────────────────────────────────────
 * 桌面端的 Dialog 是居中弹窗，在手机上够不到上半屏、还会把上下文整个遮掉。
 * 移动端二级动作（账号详情、请求详情、模型设置）一律走这条从底部升起的抽屉：
 * 拇指可及、上下文保留、拖拽手柄明确表达「可以关掉」。
 *
 * ── 与 Dialog 的三处差异 ──────────────────────────────────────
 *   · 位置：`inset-x-0 bottom-0`，圆角只留上两角（`rounded-t-xl`）；
 *   · 高度：`max-h-[85svh]`，正文区自己滚（SheetBody），头尾常驻；
 *   · 手柄：顶部一条 38×4 的把手，是「抽屉」而非「弹窗」的唯一视觉线索。
 *     `showHandle={false}` 可关掉（嵌套在另一抽屉里时用）。
 *
 * 交互仍交给 Base UI 的 Dialog：焦点陷阱、Esc、滚动锁定、关闭后焦点归位全部内建。
 * 入场/出场用 transition + `starting` / `data-ending-style`，不用 tw-animate 的
 * 关键帧（关键帧带 `filter: blur()` 声明，会逐帧计算滤镜，理由见 dialog.tsx）。
 *
 * `side` 只在桌面端宽屏有意义：`side='center'` 时按普通弹窗居中显示，
 * 用于同一份代码在桌面浏览器里打开时的观感（移动端不传即可）。
 */

function Sheet(props: DialogPrimitive.Root.Props) {
  return <DialogPrimitive.Root data-slot='sheet' {...props} />
}

function SheetTrigger(props: DialogPrimitive.Trigger.Props) {
  return <DialogPrimitive.Trigger data-slot='sheet-trigger' {...props} />
}

function SheetClose(props: DialogPrimitive.Close.Props) {
  return <DialogPrimitive.Close data-slot='sheet-close' {...props} />
}

function SheetPortal(props: DialogPrimitive.Portal.Props) {
  return <DialogPrimitive.Portal data-slot='sheet-portal' {...props} />
}

const SheetTitle = (props: DialogPrimitive.Title.Props) => (
  <DialogPrimitive.Title data-slot='sheet-title' {...props} />
)

const SheetDescription = (props: DialogPrimitive.Description.Props) => (
  <DialogPrimitive.Description data-slot='sheet-description' {...props} />
)

function SheetOverlay({ className, ...props }: DialogPrimitive.Backdrop.Props) {
  // 与 dialog 的遮罩同一口径：纯色不模糊（原因见 dialog.tsx 模块头），
  // 出场只淡出 opacity。
  return (
    <DialogPrimitive.Backdrop
      data-slot='sheet-overlay'
      className={cn(
        'fixed inset-0 isolate z-30 bg-mask',
        'transition-opacity duration-200',
        'starting:opacity-0 data-ending-style:opacity-0',
        className
      )}
      {...props}
    />
  )
}

type SheetContentProps = DialogPrimitive.Popup.Props & {
  /** 是否自带右上角关闭按钮，默认 true */
  showCloseButton?: boolean
  /** 顶部拖拽手柄，默认 true。嵌套抽屉里关掉，避免两层手柄叠在一起 */
  showHandle?: boolean
  /**
   * 形态：`bottom` 贴底（手机默认）；`center` 居中（桌面浏览器打开时用）；
   * `right` 贴右侧（筛选面板这类「列一组条件」的抽屉）。
   *
   * 默认 `bottom` —— 这份组件的存在理由就是移动端。
   */
  side?: 'bottom' | 'center' | 'right' | 'left'
  /**
   * 关闭后是否把 Portal 留在 DOM 里（内容保留、只是隐藏），默认 false。
   * 语义与 DialogContent 的 keepMounted 相同：内容里挂着有状态的第三方 DOM
   * （登录引擎的容器、发码冷却）时需要它。
   */
  keepMounted?: boolean
}

function SheetContent({
  className,
  children,
  showCloseButton = true,
  showHandle = true,
  side = 'bottom',
  keepMounted = false,
  ...props
}: SheetContentProps) {
  const centered = side === 'center'
  const right = side === 'right'
  const left = side === 'left'
  // 弹窗打开期间标记 `<html data-modal-active>`，各定时轮询据此暂停。
  React.useEffect(() => holdModalActive(), [])
  return (
    <SheetPortal keepMounted={keepMounted}>
      <SheetOverlay />
      <DialogPrimitive.Popup
        data-slot='sheet-content'
        className={cn(
          'fixed z-30 flex flex-col overflow-hidden border border-border bg-surface shadow-3 outline-none',
          // transition 替代 tw-animate 关键帧：关键帧里的 `filter: blur()` 会逐帧
          // 计算滤镜，纯位移与淡入用 transition 即可，代价低得多。
          // `translate` 是独立属性（不与 `-translate-x-1/2` 的定位冲突）。
          'transition-[opacity,translate,scale] duration-200 [transition-timing-function:cubic-bezier(.32,.9,.3,1)]',
          centered
            ? [
                'top-1/2 left-1/2 max-h-[calc(100vh-48px)] w-[min(560px,calc(100vw-48px))]',
                '-translate-x-1/2 -translate-y-1/2 rounded-lg',
                'starting:opacity-0 starting:scale-[.985]',
                'data-ending-style:opacity-0 data-ending-style:scale-[.985]',
              ]
            : right
              ? [
                  // 侧边抽屉最多占 86vw，保留关闭与上下文区域。
                  'top-0 right-0 bottom-0 h-full w-[min(340px,86vw)] rounded-l-xl',
                  'starting:translate-x-full starting:opacity-0',
                  'data-ending-style:translate-x-full data-ending-style:opacity-0',
                ]
              : left
                ? [
                    'top-0 left-0 bottom-0 h-full w-[min(340px,86vw)] rounded-r-xl',
                    'starting:-translate-x-full starting:opacity-0',
                    'data-ending-style:-translate-x-full data-ending-style:opacity-0',
                  ]
                : [
                  'inset-x-0 bottom-0 max-h-[85svh] rounded-t-xl',
                  'starting:translate-y-full starting:opacity-0',
                  'data-ending-style:translate-y-full data-ending-style:opacity-0',
                ],
          className
        )}
        {...props}
      >
        {showHandle && !centered && !right && !left && (
          <div
            aria-hidden='true'
            className='mx-auto mt-2 mb-1 h-1 w-9.5 shrink-0 rounded-pill bg-border-strong'
          />
        )}
        {children}
        {showCloseButton && (
          <DialogPrimitive.Close
            data-slot='sheet-close'
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
    </SheetPortal>
  )
}

/** 抽屉标题栏：标题左、操作右，底部一条 hairline。右侧留 40px 给自带关闭按钮 */
function SheetHeader({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot='sheet-header'
      className={cn(
        'flex shrink-0 items-center gap-2.5 border-b border-hairline px-5 py-3 pr-10',
        '[&_h2]:flex-1 [&_h2]:text-[15px] [&_h2]:font-semibold',
        className
      )}
      {...props}
    />
  )
}

/** 抽屉正文：唯一可滚动的区域，超高时自己滚、头尾常驻 */
function SheetBody({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot='sheet-body'
      className={cn('flex min-h-0 flex-1 flex-col gap-3.5 overflow-y-auto px-5 py-4', className)}
      {...props}
    />
  )
}

/** 抽屉底栏：按钮靠右；手机端常用「整行大按钮」，调用方传 flex-col 覆盖 */
function SheetFooter({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot='sheet-footer'
      className={cn(
        'flex shrink-0 items-center gap-2 border-t border-hairline bg-surface-2 px-5 py-3 pb-[max(12px,env(safe-area-inset-bottom))]',
        className
      )}
      {...props}
    />
  )
}

export {
  Sheet,
  SheetTrigger,
  SheetClose,
  SheetPortal,
  SheetOverlay,
  SheetContent,
  SheetTitle,
  SheetDescription,
  SheetHeader,
  SheetBody,
  SheetFooter,
}
