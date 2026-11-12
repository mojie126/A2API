/**
 * 商汤日日新的人机验证（滑块）弹窗。
 *
 * ── 为什么单独弹窗 ────────────────────────────────────────────
 * 滑块要人工拖动，而触发它的场景有两个：账号设置弹窗与添加提供商弹窗 ——
 * 那两个弹窗本身还有各自的表单（手机号、验证码、Base URL…）。把滑块塞进
 * 它们的正文里，弹窗会变得又长又挤，用户还得在两层里找那个滑块。单独一层
 * 弹窗把注意力收拢到一件事上，拖完关掉，原来的表单还在原地。
 *
 * ── 滑块条为什么自己画 ────────────────────────────────────────
 * 官网的题面是「一张背景图 + 一块拼图切片」，拼图在图上移动的那一条轨道与
 * 图片里的滑块手柄是同一条。用原生 `<input type="range">` 会得到一个系统
 * 风格的滑杆：轨道粗细、拇指形状都跟图里的对不上，看着不像同一个东西。
 * 这里改成自己画：一条底槽 + 一个圆形手柄，拖动手柄时**拼图切片跟着手柄
 * 一起动**，手柄停在哪个位置，切片就贴在哪里 —— 与官网的操作一致。
 */

import * as React from 'react'
import { Button, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle } from '@ui'
import { t } from '../i18n'
import '../styles/sensenova-captcha.css'

export type SensenovaCaptchaPayload = {
  /** 背景图（base64） */
  image: string
  /** 拼图切片（base64） */
  block: string
}

/** 背景图与切片在官网那边的实际尺寸（实测 316×190 与 47×190） */
const IMAGE_WIDTH = 316
const IMAGE_HEIGHT = 190
/** 拼图切片宽度（像素） */
const BLOCK_WIDTH = 47

/**
 * 滑块弹窗。
 *
 * `onSubmit(offset)` 提交用户拖到的像素偏移；`onClose` 是用户放弃时的回调。
 * 拖动范围是 `0 .. IMAGE_WIDTH - BLOCK_WIDTH`，与官网一致。
 */
export function SensenovaCaptchaDialog({
  open,
  payload,
  busy,
  onSubmit,
  onClose,
}: {
  open: boolean
  payload: SensenovaCaptchaPayload | null
  busy?: boolean
  onSubmit: (offset: number) => void
  onClose: () => void
}): React.ReactElement | null {
  const [offset, setOffset] = React.useState(0)
  const [dragging, setDragging] = React.useState(false)
  const trackRef = React.useRef<HTMLDivElement | null>(null)
  const maxOffset = IMAGE_WIDTH - BLOCK_WIDTH

  React.useEffect(() => {
    if (open) { setOffset(0); setDragging(false) }
  }, [open, payload?.image])

  /** 指针位置 → 偏移（夹在合法范围内） */
  const offsetFromEvent = React.useCallback((clientX: number): number => {
    const track = trackRef.current
    if (!track) return 0
    const rect = track.getBoundingClientRect()
    const ratio = (clientX - rect.left) / Math.max(1, rect.width)
    return Math.round(Math.min(1, Math.max(0, ratio)) * maxOffset)
  }, [maxOffset])

  const onPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (busy) return
    event.preventDefault()
    event.currentTarget.setPointerCapture(event.pointerId)
    setDragging(true)
    setOffset(offsetFromEvent(event.clientX))
  }
  const onPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging || busy) return
    setOffset(offsetFromEvent(event.clientX))
  }
  const onPointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging) return
    event.currentTarget.releasePointerCapture?.(event.pointerId)
    setDragging(false)
  }
  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (busy) return
    if (event.key === 'ArrowLeft') { event.preventDefault(); setOffset(value => Math.max(0, value - 1)) }
    if (event.key === 'ArrowRight') { event.preventDefault(); setOffset(value => Math.min(maxOffset, value + 1)) }
    if (event.key === 'Home') { event.preventDefault(); setOffset(0) }
    if (event.key === 'End') { event.preventDefault(); setOffset(maxOffset) }
  }

  if (!payload) return null
  /**
   * 拼图切片在背景图上的左边缘（占背景图宽度的百分比）。
   *
   * 滑块条与背景图**同宽**（都是 316），因此这里用同一个百分比，
   * 手柄（与切片同宽 47）在轨道上的位置也就与切片在图上的位置严格对齐 ——
   * 拖到手柄的哪儿，切片就贴到图上的哪儿。
   */
  const blockLeftPercent = (offset / IMAGE_WIDTH) * 100
  /** 轨道填充：填到手柄的中点（手柄与切片同宽，居中在左边缘上） */
  const fillPercent = ((offset + BLOCK_WIDTH / 2) / IMAGE_WIDTH) * 100
  return (
    <Dialog open={open} onOpenChange={next => { if (!next && !busy) onClose() }}>
      <DialogContent className='w-[min(420px,calc(100vw-48px))]' overlayForceRender>
        <DialogHeader><DialogTitle>{t('安全验证')}</DialogTitle></DialogHeader>
        <DialogBody className='gap-3'>
          <p className='m-0 text-xs leading-relaxed text-muted-foreground'>
            {t('商汤要求先完成滑块验证才能发送短信验证码。拖动下方滑块，让拼图与背景图的缺口对齐。')}
          </p>
          <div className='sensenova-captcha'>
            <div className='sensenova-captcha-stage'
              style={{ width: IMAGE_WIDTH, height: IMAGE_HEIGHT, maxWidth: '100%' }}>
              <img src={`data:image/png;base64,${payload.image}`} alt='' className='sensenova-captcha-bg'
                draggable={false} />
              <img
                src={`data:image/png;base64,${payload.block}`}
                alt=''
                draggable={false}
                className='sensenova-captcha-block'
                style={{
                  width: BLOCK_WIDTH,
                  height: IMAGE_HEIGHT,
                  left: `${blockLeftPercent}%`,
                }}
              />
            </div>
            <div
              ref={trackRef}
              className='sensenova-captcha-track'
              data-moved={offset > 0 ? '' : undefined}
              role='slider'
              tabIndex={0}
              aria-label={t('拖动滑块完成人机验证')}
              aria-valuemin={0}
              aria-valuemax={maxOffset}
              aria-valuenow={offset}
              aria-disabled={busy || undefined}
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerCancel={onPointerUp}
              onKeyDown={onKeyDown}
            >
              <div className='sensenova-captcha-fill' style={{ width: `${fillPercent}%` }} />
              <div className='sensenova-captcha-handle'
                style={{ left: `${blockLeftPercent}%` }}>
                <span className='sensenova-captcha-handle-grip' aria-hidden='true' />
              </div>
              <span className='sensenova-captcha-hint'>{t('按住滑块，拖到缺口处')}</span>
            </div>
          </div>
        </DialogBody>
        <DialogFooter>
          <Button variant='outline' disabled={busy} onClick={onClose}>{t('取消')}</Button>
          <Button disabled={busy} onClick={() => onSubmit(offset)}>
            {busy ? t('验证中…') : t('完成验证')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

