import * as React from 'react'
import { Button, Card, Spinner } from '@ui'
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

/** 常驻提示不抢焦点，阿里云弹出的滑块继续由用户本人操作。 */
export function CheckinVerificationNotice() {
  const api = () => (window as unknown as VerificationWindow).wbZcodeClaimVerification
  const heartbeat = () => (window as unknown as VerificationWindow).wbZcodeClaimVerificationHeartbeat
  const [state, setState] = React.useState<Verification | null>(() => api()?.status() || null)
  const [presence, setPresence] = React.useState<Presence | null>(null)
  React.useEffect(() => {
    const update = () => setState(api()?.status() || null)
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
  if (!state) {
    const hint = presence?.hint || t('保持此页面打开；可切到后台，但浏览器不能完全退出')
    return <Card className='fixed bottom-5 right-5 z-[2147482900] w-[min(380px,calc(100vw-40px))] space-y-1 p-4 shadow-lg' role='status' aria-live='polite'>
      <div className='text-sm font-semibold'>{t('ZCode 自动领取验证')}</div>
      <p className='m-0 break-words text-xs text-muted-foreground'>{hint}</p>
      <p className='m-0 text-[11px] text-muted-foreground'>{t('页面状态：{state}', {
        state: presence?.state === 'stopped' ? t('已关闭') : presence?.state === 'background' ? t('后台运行') : t('运行中'),
      })}</p>
    </Card>
  }
  const submitting = state.phase === 'submitting'
  return <Card className='fixed bottom-5 right-5 z-[2147482900] w-[min(380px,calc(100vw-40px))] space-y-2 p-4 shadow-lg' role='status' aria-live='polite'>
    <div className='flex items-center gap-2 text-sm font-semibold'>
      <Spinner className='size-4 shrink-0' />{t('ZCode 自动领取验证')}
    </div>
    <p className='break-words text-xs text-muted-foreground'>{state.accountName} · {state.planName}</p>
    <p className='text-xs text-muted-foreground'>{submitting
      ? t('验证凭据正在提交，领取结果请查看签到记录')
      : t('正在为本次领取验证；若出现滑块，请手动完成')}</p>
    <Button variant='outline' size='xs' disabled={submitting} onClick={() => api()?.cancel()}>{t('取消验证')}</Button>
  </Card>
}
