import * as React from 'react'
import { Input, Label, Switch } from '@ui'
import { t } from '../i18n'
import type { AccountRecord } from './accounts-shared'
import { RoutingSelect } from './routing-select'

/** 组内并发上限，与后端 `MAX_ROUTING_CONCURRENCY` 一致 */
export const ROUTING_CONCURRENCY_MAX = 100000

export type ConcurrencySummary = {
  /** 账号全部设置了并发上限，总数锁定为上限之和 */
  locked: boolean
  /** 默认或锁定的组内并发总数 */
  total: number
}

/** 按候选账号推导组内并发的默认总数（与后端 `routing_concurrency` 同一口径） */
export function concurrencySummary(accounts: AccountRecord[]): ConcurrencySummary {
  const caps = accounts.map(account => Number(account.maxConcurrent) || 0)
  const locked = caps.length > 0 && caps.every(cap => cap > 0)
  return { locked, total: locked ? caps.reduce((sum, cap) => sum + cap, 0) : accounts.length }
}

/**
 * 组内并发设置：启用开关、总数输入框与并发策略下拉
 *
 * 默认关闭，关闭时不限制组内总并发
 * 启用且账号全部有并发上限时总数固定为上限之和，不可修改，其余情况默认为账号数
 */
export function RoutingConcurrencyFields({ accounts, enabled, total, strategy, disabled, onEnabled, onTotal, onStrategy }: {
  accounts: AccountRecord[]; enabled?: boolean; total?: number; strategy?: string; disabled?: boolean
  onEnabled(value: boolean): void; onTotal(value?: number): void; onStrategy(value?: string): void
}) {
  const summary = concurrencySummary(accounts)
  const active = Boolean(enabled)
  const value = summary.locked ? summary.total : total ?? summary.total
  const update = (text: string) => {
    if (!text) { onTotal(undefined); return }
    // 只接受纯数字，小数与负号直接忽略
    if (!/^\d+$/.test(text)) return
    const parsed = Math.min(Number(text), ROUTING_CONCURRENCY_MAX)
    onTotal(parsed > 0 ? parsed : undefined)
  }
  return <>
    <div className='routing-rule-heading'><Label>{t('组内并发设置')}</Label>
      <label className='routing-rule-switch'>
        <span>{t('启用组内并发')}</span>
        <Switch checked={active} disabled={disabled} onCheckedChange={onEnabled} />
      </label>
    </div>
    <div className='routing-rule-field'>
      <Label htmlFor='routing-concurrency'>{t('组内并发')}</Label>
      <Input id='routing-concurrency' type='text' inputMode='numeric' pattern='[0-9]*'
        maxLength={String(ROUTING_CONCURRENCY_MAX).length} disabled={disabled || !active || summary.locked}
        value={String(value)} onChange={event => update(event.currentTarget.value)} />
      <small className='routing-rule-hint'>{!active
        ? t('未启用时不限制组内总并发')
        : summary.locked
          ? t('账号均设置并发上限，总数固定为 {n}', { n: summary.total })
          : t('默认为组内 {n} 个账号各分配 1 个并发', { n: accounts.length })}</small>
    </div>
    <div className='routing-rule-field'>
      <Label>{t('并发策略')}</Label>
      <RoutingSelect label={t('并发策略')} value={strategy || 'average'} disabled={disabled || !active || summary.locked}
        options={[['average', t('平均分配')]]}
        onChange={next => onStrategy(next === 'average' ? undefined : next)} />
      <small className='routing-rule-hint'>{t('按账号平均分配并发，且不超过账号自身的并发上限')}</small>
    </div>
  </>
}
