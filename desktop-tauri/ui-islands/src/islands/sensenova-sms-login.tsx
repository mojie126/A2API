/**
 * 商汤日日新手机号验证码登录（余额查询凭证）。
 *
 * 上游控制台已关闭账号密码登录，发码与校验由网关的 `/api/sensenova/sms/*`
 * 两段接口完成。组件只处理手机号、验证码与一次性 ticket，验证码不落草稿，
 * refresh token 由后端加密保存。
 */

import * as React from 'react'
import { Button, Field, FieldLabel, Input } from '@ui'

import { describeError, shared, toast } from './add-account-bridge'
import { t } from '../i18n'

export type SensenovaSmsContext = {
  /** 更新已有账号的查询凭证 */
  accountId?: string
  /** 往已有自定义提供商添加账号 */
  providerId?: string
  /** 新建自定义提供商 + 首个账号；返回 createProvider 请求体 */
  createProvider?: () => Record<string, unknown> | null
  /** 转发用 API Key（添加账号时提交） */
  apiKey?: () => string
  /** 备注名 */
  name?: () => string
  /** 账号级 User-Agent（添加账号时提交） */
  userAgent?: () => string
  /** 已绑定手机号的脱敏值（仅展示） */
  phoneMasked?: string
  /** 账号级代理 */
  proxy?: () => unknown
  onSuccess?: (data: unknown) => void | Promise<void>
}

type Tenant = {
  username?: string
  user_id?: string
  tenant_code?: string
  tenant_name?: string
  sign?: string
}

const PHONE_RE = /^1\d{10}$/
const CODE_RE = /^\d{6}$/

function request(path: string, body: Record<string, unknown>): Promise<unknown> {
  const client = shared().wbProviders
  if (typeof client?.customRequest !== 'function') {
    return Promise.reject(new Error(t('网关请求接口不可用，请更新客户端')))
  }
  return client.customRequest('POST', path, body)
}

export function SensenovaSmsLogin({
  context,
  disabled,
}: {
  context: SensenovaSmsContext
  disabled?: boolean
}): React.ReactElement {
  const [phone, setPhone] = React.useState('')
  const [code, setCode] = React.useState('')
  const [ticket, setTicket] = React.useState('')
  const [masked, setMasked] = React.useState(context.phoneMasked || '')
  const [tenants, setTenants] = React.useState<Tenant[]>([])
  const [tenantIndex, setTenantIndex] = React.useState(0)
  const [captcha, setCaptcha] = React.useState<{ codeKey: string; image: string; block: string } | null>(null)
  const [captchaOffset, setCaptchaOffset] = React.useState(0)
  const [busy, setBusy] = React.useState<'send' | 'verify' | null>(null)
  const [hint, setHint] = React.useState('')

  const updating = Boolean(context.accountId)
  const phoneRequired = !updating

  async function send(): Promise<void> {
    if (busy) return
    const value = phone.trim()
    if (phoneRequired && !PHONE_RE.test(value)) {
      toast(t('请填写 11 位大陆手机号'), 'err')
      return
    }
    setBusy('send')
    setHint('')
    try {
      const proxy = context.proxy?.()
      const data = (await request('/api/sensenova/sms/send', {
        phone: value || undefined,
        accountId: context.accountId || undefined,
        proxy,
      })) as {
        status?: string
        ticket?: string
        phoneMasked?: string
        captcha?: { codeKey?: string; image?: string; block?: string }
      } | null
      const nextTicket = String(data?.ticket || '')
      if (!nextTicket) throw new Error(t('网关没有返回短信登录会话'))
      setTicket(nextTicket)
      setMasked(String(data?.phoneMasked || value))
      if (data?.status === 'captcha') {
        setCaptcha({
          codeKey: String(data.captcha?.codeKey || ''),
          image: String(data.captcha?.image || ''),
          block: String(data.captcha?.block || ''),
        })
        setCaptchaOffset(0)
        setHint(t('请完成滑块验证后继续发送验证码'))
      } else {
        setCaptcha(null)
        setHint(t('验证码已发送，请查看短信'))
      }
    } catch (error) {
      setHint(t('发送失败：{reason}', { reason: describeError(error) }))
      toast(t('发送失败：{reason}', { reason: describeError(error) }), 'err')
    } finally {
      setBusy(null)
    }
  }

  async function verifyCaptcha(): Promise<void> {
    if (busy || !captcha || !ticket) return
    setBusy('send')
    try {
      const data = (await request('/api/sensenova/sms/captcha', {
        ticket,
        codeKey: captcha.codeKey,
        codeValue: captchaOffset,
        accountId: context.accountId || undefined,
        proxy: context.proxy?.(),
      })) as {
        status?: string
        ticket?: string
        phoneMasked?: string
        captcha?: { codeKey?: string; image?: string; block?: string }
      } | null
      if (data?.status === 'captcha') {
        setCaptcha({
          codeKey: String(data.captcha?.codeKey || ''),
          image: String(data.captcha?.image || ''),
          block: String(data.captcha?.block || ''),
        })
        setCaptchaOffset(0)
        setHint(t('滑块验证又刷新了一次，请重新拖动'))
        return
      }
      setCaptcha(null)
      setTicket(String(data?.ticket || ticket))
      setMasked(String(data?.phoneMasked || masked))
      setHint(t('验证码已发送，请查看短信'))
      toast(t('验证码已发送，请查看短信'))
    } catch (error) {
      setHint(t('人机验证失败：{reason}', { reason: describeError(error) }))
      toast(t('人机验证失败：{reason}', { reason: describeError(error) }), 'err')
    } finally {
      setBusy(null)
    }
  }

  async function verify(): Promise<void> {
    if (busy) return
    if (!ticket) {
      toast(t('请先获取短信验证码'), 'err')
      return
    }
    const value = phone.trim()
    if (phoneRequired && !PHONE_RE.test(value)) {
      toast(t('请填写 11 位大陆手机号'), 'err')
      return
    }
    // 多租户第一阶段已消费短信验证码，第二阶段只提交租户选择
    if (!tenants.length && !CODE_RE.test(code.trim())) {
      toast(t('请填写 6 位数字验证码'), 'err')
      return
    }
    setBusy('verify')
    setHint('')
    try {
      const createProvider = context.createProvider?.() || undefined
      const selected = tenants.length > 1 ? tenants[tenantIndex] : undefined
      const data = (await request('/api/sensenova/sms/verify', {
        ticket,
        code: tenants.length ? undefined : code.trim(),
        phone: value || undefined,
        accountId: context.accountId || undefined,
        providerId: context.providerId || undefined,
        createProvider,
        apiKey: context.apiKey?.() || undefined,
        name: context.name?.() || undefined,
        userAgent: context.userAgent?.() || undefined,
        proxy: context.proxy?.(),
        tenant: selected,
      })) as { status?: string; tenants?: Tenant[] } | null
      if (data?.status === 'tenant_required') {
        const list = Array.isArray(data.tenants) ? data.tenants : []
        setTenants(list)
        setTenantIndex(0)
        setHint(t('该手机号绑定了多个租户，请选择后再次确认'))
        return
      }
      setCode('')
      setTicket('')
      setTenants([])
      setHint(t('登录成功，余额查询凭证已更新'))
      toast(t('商汤日日新登录成功'))
      await context.onSuccess?.(data)
    } catch (error) {
      setHint(t('登录失败：{reason}', { reason: describeError(error) }))
      toast(t('登录失败：{reason}', { reason: describeError(error) }), 'err')
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className='flex flex-col gap-3'>
      <Field orientation='horizontal' className='field-row mt-2.5'>
        <FieldLabel htmlFor='sensenova-sms-phone'>{t('手机号')}</FieldLabel>
        <div className='flex min-w-[260px] flex-1 items-center gap-2'>
          <Input
            id='sensenova-sms-phone'
            type='tel'
            maxLength={11}
            autoComplete='tel'
            value={phone}
            disabled={disabled || busy !== null}
            placeholder={updating && masked ? t('已绑定 {phone}，留空沿用', { phone: masked }) : t('11 位大陆手机号')}
            onChange={event => setPhone(event.currentTarget.value.replace(/\D/g, ''))}
          />
          <Button
            type='button'
            variant='outline'
            disabled={disabled || busy !== null}
            onClick={() => { void send() }}
          >
            {busy === 'send' ? t('发送中…') : t('获取验证码')}
          </Button>
        </div>
      </Field>
      {captcha ? (
        <div className='flex flex-col gap-2 rounded-md border border-border bg-surface-inset p-3'>
          <p className='text-xs text-subtle'>{t('拖动下方滑块，让拼图与缺口对齐，然后点「完成验证」。')}</p>
          <div className='relative h-[190px] w-[316px] max-w-full overflow-hidden rounded-md bg-slate-100'>
            <img src={`data:image/png;base64,${captcha.image}`} alt='' className='h-full w-full select-none' draggable={false} />
            <img
              src={`data:image/png;base64,${captcha.block}`}
              alt=''
              draggable={false}
              className='pointer-events-none absolute inset-y-0 h-full select-none'
              style={{ left: `${(captchaOffset / 316) * 100}%`, width: '14.873417721518987%' }}
            />
          </div>
          <input
            type='range'
            min={0}
            max={256}
            step={1}
            value={captchaOffset}
            disabled={disabled || busy !== null}
            onChange={event => setCaptchaOffset(Number(event.currentTarget.value) || 0)}
            aria-label={t('拖动滑块完成人机验证')}
          />
          <div className='flex items-center gap-2'>
            <Button type='button' size='sm' disabled={disabled || busy !== null} onClick={() => { void verifyCaptcha() }}>
              {busy === 'send' ? t('验证中…') : t('完成验证')}
            </Button>
            <span className='text-xs text-subtle'>{t('滑块未对齐时不会通过，请重新拖动。')}</span>
          </div>
        </div>
      ) : null}
      <Field orientation='horizontal' className='field-row'>
        <FieldLabel htmlFor='sensenova-sms-code'>{t('验证码')}</FieldLabel>
        <Input
          id='sensenova-sms-code'
          className='min-w-[260px]'
          inputMode='numeric'
          autoComplete='one-time-code'
          maxLength={6}
          value={code}
          disabled={disabled || busy !== null}
          placeholder={t('6 位数字')}
          onChange={event => setCode(event.currentTarget.value.replace(/\D/g, ''))}
        />
      </Field>
      {tenants.length > 1 ? (
        <Field orientation='horizontal' className='field-row'>
          <FieldLabel htmlFor='sensenova-sms-tenant'>{t('租户')}</FieldLabel>
          <select
            id='sensenova-sms-tenant'
            className='min-w-[260px] rounded-md border border-border bg-surface px-2 py-1.5 text-sm'
            value={String(tenantIndex)}
            onChange={event => setTenantIndex(Number(event.currentTarget.value) || 0)}
          >
            {tenants.map((tenant, index) => (
              <option key={`${tenant.user_id || index}-${tenant.tenant_code || ''}`} value={String(index)}>
                {tenant.tenant_name || tenant.username || tenant.tenant_code || t('租户 {index}', { index: index + 1 })}
              </option>
            ))}
          </select>
        </Field>
      ) : null}
      <div className='flex items-center gap-3'>
        <Button
          type='button'
          disabled={disabled || busy !== null}
          onClick={() => { void verify() }}
        >
          {busy === 'verify'
            ? t('登录中…')
            : updating
              ? t('登录并更新查询凭证')
              : t('登录并添加账号')}
        </Button>
        {hint ? <span className='text-xs text-subtle'>{hint}</span> : null}
      </div>
    </div>
  )
}
