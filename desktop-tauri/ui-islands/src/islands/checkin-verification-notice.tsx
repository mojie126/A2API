import * as React from 'react'
import { Button, Collapsible, CollapsibleContent, CollapsibleTrigger, Spinner } from '@ui'
import { t } from '../i18n'

type Verification = {
  requestId: string
  accountId: string
  accountName: string
  planName: string
  phase: string
}

type VerificationWindow = {
  wbZcodeClaimVerification?: { cancel(): void; status(): Verification | null }
  wbZcodeClaimVerificationHeartbeat?: {
    state(): string
    lastBeatAt(): number
    delay(): number
  }
}

type Presence = {
  state: string
  clientId?: string
  at?: number
  hint?: string
}

/**
 * ZCode 自动领取验证的状态提示。
 *
 * ── 为什么待在侧边栏底部而不是浮在主窗口上 ────────────────
 * 早先它是一张 `fixed` 卡片，钉在右下角常驻。正文区本来就窄，一张 380px 的
 * 浮层压在上面会盖住表格右半边，而它多数时候只需要一句「还在跑」——不值得
 * 占主窗口的位置。侧边栏底部的位置是「不挡事的地方」：网关状态与主题切换
 * 也在那儿，都是常驻的次要信息。
 *
 * ── 三种状态 ──────────────────────────────────────────────────
 *   · 正在验证：展开，显示账号与套餐名，带「取消验证」；
 *   · 待验证（空闲）：折叠成一行，只写标题与页面状态；
 *   · 隐藏：用户点「✕」后彻底不占位，直到下一次真的来了验证任务。
 */
export function CheckinVerificationNotice() {
  const api = () => (window as unknown as VerificationWindow).wbZcodeClaimVerification
  const heartbeat = () => (window as unknown as VerificationWindow).wbZcodeClaimVerificationHeartbeat
  const [state, setState] = React.useState<Verification | null>(() => api()?.status() || null)
  const [presence, setPresence] = React.useState<Presence | null>(null)
  const [expanded, setExpanded] = React.useState(false)
  /** 用户点「✕」后不再占位，直到下一次真的来了验证任务 */
  const [dismissed, setDismissed] = React.useState(false)
  React.useEffect(() => {
    const update = () => {
      const next = api()?.status() || null
      setState(next)
      // 有任务了就重新露出来：上一次收起的是「空闲提示」，不是这次任务
      if (next) setDismissed(false)
    }
    const receivePresence = (event: Event) =>
      setPresence((event as CustomEvent<Presence>).detail || null)
    window.addEventListener('zcode-claim-verification', update)
    window.addEventListener('zcode-claim-presence', receivePresence)
    const ticker = window.setInterval(() => {
      const current = heartbeat()
      if (!current) return
      const last = current.lastBeatAt()
      setPresence(previous => ({
        state: current.state(),
        clientId: previous?.clientId,
        at: last,
        hint: previous?.hint,
      }))
    }, 5000)
    update()
    return () => {
      window.removeEventListener('zcode-claim-verification', update)
      window.removeEventListener('zcode-claim-presence', receivePresence)
      window.clearInterval(ticker)
    }
  }, [])
  if (!state && !presence) return null
  if (dismissed) return null

  const submitting = state?.phase === 'submitting'
  const stateText = presence?.state === 'stopped'
    ? t('已关闭')
    : presence?.state === 'background' ? t('后台运行') : t('运行中')
  const hint = presence?.hint || t('保持此页面打开；可切到后台，但浏览器不能完全退出')
  const body = state ? (
    <>
      <p className='break-words text-xs text-muted-foreground'>{state.accountName} · {state.planName}</p>
      <p className='text-xs text-muted-foreground'>{submitting
        ? t('验证凭据正在提交，领取结果请查看签到记录')
        : t('正在为本次领取验证；若出现滑块，请手动完成')}</p>
      <Button variant='outline' size='xs' disabled={submitting} onClick={() => api()?.cancel()}>{t('取消验证')}</Button>
    </>
  ) : (
    <>
      <p className='m-0 break-words text-xs text-muted-foreground'>{hint}</p>
      <p className='m-0 text-[11px] text-muted-foreground'>{t('页面状态：{state}', { state: stateText })}</p>
    </>
  )
  return (
    <Collapsible open={expanded} onOpenChange={setExpanded} className='zcode-verify'>
      <div className='zcode-verify-head'>
        <CollapsibleTrigger render={<Button variant='ghost' size='default' />}
          className='zcode-verify-title'
          aria-label={expanded ? t('收起 ZCode 自动领取验证') : t('展开 ZCode 自动领取验证')}>
          {state ? <Spinner className='size-3.5 shrink-0' /> : null}
          <span className='min-w-0 truncate'>{t('ZCode 自动领取验证')}</span>
        </CollapsibleTrigger>
        <Button variant='ghost' size='icon-xs' className='zcode-verify-close'
          aria-label={t('隐藏 ZCode 自动领取验证提示')}
          title={t('隐藏')}
          onClick={() => setDismissed(true)}>✕</Button>
      </div>
      <CollapsibleContent className='zcode-verify-body'>{body}</CollapsibleContent>
    </Collapsible>
  )
}

