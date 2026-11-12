/**
 * 商汤日日新手机号验证码登录（余额查询凭证）。
 *
 * 上游控制台已关闭账号密码登录，发码与校验由网关的 `/api/sensenova/sms/*`
 * 两段接口完成。组件只处理手机号、验证码与一次性 ticket，验证码不落草稿，
 * refresh token 由后端加密保存。
 *
 * ── 布局口径 ──────────────────────────────────────────────────
 * 两种宿主：
 *   · 账号设置弹窗（`variant='field'`）—— 用 `acct-dlg-field` 那套字段行，
 *     与同一张卡里的「自动查询余额」「查询间隔」等行对齐；
 *   · 添加提供商弹窗（`variant='add'`）—— 用 `.add-field` 的标签网格，
 *     与上面「名称」「协议」「Base URL」等行对齐。
 * 两种宿主都用同一份字段结构，只是外层类名不同 —— 字段各写一遍必然漂。
 */

import * as React from 'react'
import { Button, Input, Label } from '@ui'

import { describeError, shared, toast } from './add-account-bridge'
import { SensenovaCaptchaDialog } from './sensenova-captcha-dialog'
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
  variant = 'field',
}: {
  context: SensenovaSmsContext
  disabled?: boolean
  /** 宿主形态：账号设置弹窗（字段行） / 添加提供商弹窗（标签网格） */
  variant?: 'field' | 'add'
}): React.ReactElement {
  const [phone, setPhone] = React.useState('')
  const [code, setCode] = React.useState('')
  const [ticket, setTicket] = React.useState('')
  const [masked, setMasked] = React.useState(context.phoneMasked || '')
  const [tenants, setTenants] = React.useState<Tenant[]>([])
  const [tenantIndex, setTenantIndex] = React.useState(0)
  const [captcha, setCaptcha] = React.useState<{ codeKey: string; image: string; block: string } | null>(null)
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

  async function verifyCaptcha(offset: number): Promise<void> {
    if (busy || !captcha || !ticket) return
    setBusy('send')
    try {
      const data = (await request('/api/sensenova/sms/captcha', {
        ticket,
        codeKey: captcha.codeKey,
        codeValue: offset,
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
    <div className={variant === 'add' ? 'sensenova-sms sensenova-sms-add' : 'sensenova-sms sensenova-sms-field'}>
      {variant === 'add' ? (
        <div className='add-field sensenova-sms-section'>
          <Label>{t('余额查询')}</Label>
          <span className='hint sensenova-sms-section-note'>
            {t('余额查询走手机号验证码登录；短信由商汤官方发送，验证码不落草稿。')}
          </span>
        </div>
      ) : null}
      <div className='add-field sensenova-sms-field'>
        <Label htmlFor='sensenova-sms-phone' className={variant === 'add' ? '' : 'acct-dlg-label'}>
          {t('手机号')}
        </Label>
        <div className='sensenova-sms-control'>
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
            className='sensenova-sms-send'
            disabled={disabled || busy !== null}
            onClick={() => { void send() }}
          >
            {busy === 'send' ? t('发送中…') : t('获取验证码')}
          </Button>
        </div>
        <span className='hint'>{t('手机号与短信验证码由商汤官方发送；每次发码的有效期约 5 分钟。')}</span>
      </div>
      {captcha ? (
        <p className='sensenova-sms-need'>
          {t('商汤要求先完成滑块验证才能发送短信验证码，点上面的「获取验证码」会弹出验证窗口。')}
        </p>
      ) : null}
      <div className='add-field sensenova-sms-field'>
        <Label htmlFor='sensenova-sms-code' className={variant === 'add' ? '' : 'acct-dlg-label'}>
          {t('验证码')}
        </Label>
        <Input
          id='sensenova-sms-code'
          className='sensenova-sms-code'
          inputMode='numeric'
          autoComplete='one-time-code'
          maxLength={6}
          value={code}
          disabled={disabled || busy !== null}
          placeholder={t('6 位数字')}
          onChange={event => setCode(event.currentTarget.value.replace(/\D/g, ''))}
        />
        <span className='hint'>{t('填入刚收到的 6 位数字验证码。')}</span>
      </div>
      {tenants.length > 1 ? (
        <div className='add-field sensenova-sms-field'>
          <Label htmlFor='sensenova-sms-tenant' className={variant === 'add' ? '' : 'acct-dlg-label'}>
            {t('租户')}
          </Label>
          <select
            id='sensenova-sms-tenant'
            className='sensenova-sms-tenant'
            value={String(tenantIndex)}
            onChange={event => setTenantIndex(Number(event.currentTarget.value) || 0)}
          >
            {tenants.map((tenant, index) => (
              <option key={`${tenant.user_id || index}-${tenant.tenant_code || ''}`} value={String(index)}>
                {tenant.tenant_name || tenant.username || tenant.tenant_code || t('租户 {index}', { index: index + 1 })}
              </option>
            ))}
          </select>
          <span className='hint'>{t('该手机号绑定了多个租户，请选择账号所属的那个。')}</span>
        </div>
      ) : null}
      <div className='sensenova-sms-foot'>
        {hint ? <span className='sensenova-sms-hint'>{hint}</span> : null}
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
      </div>
      <SensenovaCaptchaDialog
        open={Boolean(captcha)}
        payload={captcha ? { image: captcha.image, block: captcha.block } : null}
        busy={busy !== null}
        onSubmit={offset => { void verifyCaptcha(offset) }}
        onClose={() => {
          setCaptcha(null)
          setHint(t('滑块验证已取消，点「获取验证码」可重新开始'))
        }}
      />
    </div>
  )
}
