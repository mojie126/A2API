/**
 * 级联模型多选：第一级选提供商、第二级选该提供商的模型，两级都可搜索
 *
 * ── 为什么不用现成的 MultiSelect ─────────────────────────────
 * 平铺清单在模型一多的时候就只剩「搜」这一条路：用户记得模型名搜得动，
 * 只记得是哪家的时候只能滚。识别页的「哪些模型用这种方式」正是这种场景 ——
 * 一屏几十只模型按提供商分组后才是可读的。第二级再按「能不能识图」分两组，
 * 让用户一眼看出自己选的是「本来就看不了图的」还是「本来能识图的」
 *
 * ── 两级各自独立的搜索框 ─────────────────────────────────────
 * 搜索不跨级联：在提供商框里打字只过滤左边那列，在模型框里打字只过滤右边
 * 那列。共用一个搜索框时，「GLM」既可能是家名也可能是模型名，过滤结果会
 * 把两列同时打散，反而更难对上「这家有哪些模型」
 */

import * as React from 'react'
import { Button, Popover, PopoverContent, PopoverTrigger, cn } from '@ui'
import { t } from '../ui-kit/src/i18n'

export type CascadeModelOption = {
  /** 唯一键（调用方负责，与提交给后端的模型标识一致） */
  key: string
  provider: string
  providerLabel: string
  model: string
  /** 展示名；与模型标识相同时不必重复显示 */
  name: string
  /** 经过能力补充后的生效值：true = 支持识图，false / null = 不支持（含未声明） */
  supportsImages: boolean | null
}

type Props = {
  value: string[]
  options: CascadeModelOption[]
  disabled?: boolean
  /** 支持识图的模型是否可选（强制辅助识图才允许把本来能识图的模型拉进来） */
  allowVisionCapable: boolean
  onChange(next: string[]): void
  className?: string
  'aria-label'?: string
}

function matches(query: string, ...parts: string[]): boolean {
  const word = query.trim().toLowerCase()
  if (!word) return true
  return parts.some(part => part.toLowerCase().includes(word))
}

/** 已选的键里只要有一个在该提供商下，就把这一家标成有选中项 */
function countSelected(entry: { models: CascadeModelOption[] }, selected: Set<string>): number {
  return entry.models.filter(option => selected.has(option.key)).length
}

export function CascadeModelSelect({
  value, options, disabled, allowVisionCapable, onChange, className, ...props
}: Props) {
  const [open, setOpen] = React.useState(false)
  const [providerQuery, setProviderQuery] = React.useState('')
  const [modelQuery, setModelQuery] = React.useState('')
  const selected = React.useMemo(() => new Set(value), [value])

  /** 按提供商聚合；同一家只出现一次，标签取第一只模型给的 providerLabel */
  const providers = React.useMemo(() => {
    const map = new Map<string, { provider: string; label: string; models: CascadeModelOption[] }>()
    for (const option of options) {
      let entry = map.get(option.provider)
      if (!entry) {
        entry = { provider: option.provider, label: option.providerLabel || option.provider, models: [] }
        map.set(option.provider, entry)
      }
      entry.models.push(option)
    }
    return [...map.values()]
  }, [options])

  const [activeProvider, setActiveProvider] = React.useState<string | null>(null)
  /** 第二级标题展示的提供商：用户点过哪家就是哪家，否则回落第一家有选中项的那家 */
  const active = providers.find(entry => entry.provider === activeProvider)
    || providers.find(entry => countSelected(entry, selected) > 0)
    || providers[0]

  // 浮层每次打开都回到干净状态：上一次的搜索词留着会让人以为清单缺项
  React.useEffect(() => {
    if (!open) return
    setProviderQuery('')
    setModelQuery('')
  }, [open])

  const visibleProviders = providers.filter(entry =>
    matches(providerQuery, entry.label, entry.provider))
  const activeModels = (active?.models || []).filter(option =>
    matches(modelQuery, option.model, option.name, active?.label || ''))
  const unsupported = activeModels.filter(option => option.supportsImages !== true)
  const visionCapable = activeModels.filter(option => option.supportsImages === true)

  const toggle = (option: CascadeModelOption) => {
    if (disabled) return
    const checked = selected.has(option.key)
    // 支持识图的模型在「辅助识图 / 整体接管」下只能取消，不能再勾回来
    if (!checked && option.supportsImages === true && !allowVisionCapable) return
    const next = new Set(selected)
    if (checked) next.delete(option.key)
    else next.add(option.key)
    onChange([...next])
  }

  const labels = options.filter(option => selected.has(option.key))
    .map(option => `${option.providerLabel} · ${option.model}`)
  const summary = labels.length
    ? labels.length > 2 ? t('已选 {n} 个模型', { n: labels.length }) : labels.join('、')
    : t('选择一个或多个模型')

  const groupRow = (group: CascadeModelOption[]) => group.map(option => {
    const checked = selected.has(option.key)
    // 置灰只挡「新勾选」：已选中的项必须还能取消，否则换个处理方式之后就删不掉了
    const blocked = option.supportsImages === true && !allowVisionCapable && !checked
    const label = option.name && option.name !== option.model ? `${option.model}（${option.name}）` : option.model
    return <button key={option.key} type='button' disabled={disabled || blocked}
      aria-pressed={checked}
      onClick={() => toggle(option)}
      title={blocked ? t('只有「强制辅助识图」才能把支持识图的模型拉进来') : label}
      className={cn(
        'flex w-full min-w-0 items-center gap-2 rounded-md border-0 px-[10px] py-[7px] text-left',
        'text-[12.5px] leading-[1.4] text-foreground select-none outline-none transition-colors duration-[120ms]',
        'hover:bg-nav-hover focus-visible:bg-nav-hover',
        checked ? 'bg-primary-soft font-semibold text-primary-fg' : 'bg-transparent',
        (disabled || blocked) && 'cursor-not-allowed text-muted-foreground hover:bg-transparent',
      )}>
      <span aria-hidden='true' className={cn(
        'flex size-3 flex-none items-center justify-center',
        checked ? 'text-primary' : 'text-transparent')}>
        <svg viewBox='0 0 12 12' className='size-3'><path d='M2.5 6.2 4.8 8.5 9.5 3.8'
          stroke='currentColor' strokeWidth='1.8' fill='none' strokeLinecap='round' strokeLinejoin='round' /></svg>
      </span>
      <span className='min-w-0 flex-1 truncate'>{label}</span>
    </button>
  })

  return <Popover open={open} onOpenChange={setOpen}>
    <PopoverTrigger render={<Button type='button' variant='outline' disabled={disabled}
      aria-label={props['aria-label']} className={cn('h-[30px] w-full min-w-0 justify-between font-normal', className)} />}>
      <span className='min-w-0 flex-1 truncate text-left'>{summary}</span>
      <svg viewBox='0 0 10 6' className='size-2.5 flex-none text-muted-foreground' aria-hidden='true'>
        <path d='M1 1l4 4 4-4' stroke='currentColor' strokeWidth='1.4' fill='none'
          strokeLinecap='round' strokeLinejoin='round' />
      </svg>
    </PopoverTrigger>
    <PopoverContent align='start' sideOffset={4}
      className='w-[min(560px,var(--available-width))]'>
      <div className='flex h-[360px] min-w-0'>
        <div className='flex w-[38%] min-w-[150px] flex-col border-r border-hairline'>
          <div className='flex-none border-b border-hairline p-2'>
            <input value={providerQuery} onChange={event => setProviderQuery(event.currentTarget.value)}
              placeholder={t('搜索提供商')} aria-label={t('搜索提供商')} autoComplete='off'
              className='h-[26px] w-full rounded-md border border-control-border bg-control px-2 text-[12px] outline-none placeholder:text-muted-foreground focus:border-primary' />
          </div>
          <div className='flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto overscroll-contain p-1.5'>
            {visibleProviders.length ? visibleProviders.map(entry => {
              const count = countSelected(entry, selected)
              return <button key={entry.provider} type='button'
                onClick={() => setActiveProvider(entry.provider)}
                aria-current={entry.provider === active?.provider ? 'true' : undefined}
                title={entry.label}
                className={cn(
                  'flex min-w-0 items-center gap-2 rounded-md border-0 px-[10px] py-[7px] text-left text-[12.5px]',
                  'leading-[1.4] transition-colors duration-[120ms] hover:bg-nav-hover',
                  entry.provider === active?.provider
                    ? 'bg-primary-soft font-semibold text-primary-fg' : 'bg-transparent text-foreground')}>
                <span className='min-w-0 flex-1 truncate'>{entry.label}</span>
                {count ? <span className='flex-none text-[10.5px] text-muted-foreground'>{count}</span> : null}
                <span aria-hidden='true' className='flex-none text-muted-foreground'>›</span>
              </button>
            }) : <p className='px-2 py-3 text-[11.5px] text-muted-foreground'>{t('没有匹配的提供商')}</p>}
          </div>
        </div>
        <div className='flex min-w-0 flex-1 flex-col'>
          <div className='flex-none border-b border-hairline p-2'>
            <input value={modelQuery} onChange={event => setModelQuery(event.currentTarget.value)}
              placeholder={t('搜索该提供商的模型')} aria-label={t('搜索该提供商的模型')} autoComplete='off'
              className='h-[26px] w-full rounded-md border border-control-border bg-control px-2 text-[12px] outline-none placeholder:text-muted-foreground focus:border-primary' />
          </div>
          <div className='flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto overscroll-contain p-1.5'>
            {!active ? <p className='px-2 py-3 text-[11.5px] text-muted-foreground'>{t('没有可选的模型')}</p> : <>
              {unsupported.length ? <>
                <p className='px-2 pb-0.5 pt-1 text-[10.5px] font-medium text-muted-foreground'>
                  {t('不支持识图（能力补充后仍看不了图）')}</p>
                {groupRow(unsupported)}
              </> : null}
              {visionCapable.length ? <>
                <p className='px-2 pb-0.5 pt-3 text-[10.5px] font-medium text-muted-foreground'>
                  {allowVisionCapable ? t('支持识图') : t('支持识图（仅「强制辅助识图」可选）')}</p>
                {groupRow(visionCapable)}
              </> : null}
              {!unsupported.length && !visionCapable.length
                ? <p className='px-2 py-3 text-[11.5px] text-muted-foreground'>{t('没有匹配的模型')}</p> : null}
            </>}
          </div>
        </div>
      </div>
      <div className='flex flex-none items-center gap-2 border-t border-hairline px-3 py-2'>
        <span className='mr-auto min-w-0 truncate text-[11.5px] text-muted-foreground'>
          {labels.length ? t('已选 {n} 个模型', { n: labels.length }) : t('未选择模型')}
        </span>
        <Button type='button' size='xs' variant='ghost' disabled={disabled || !labels.length}
          onClick={() => onChange([])}>{t('清空')}</Button>
        <Button type='button' size='xs' variant='outline' onClick={() => setOpen(false)}>{t('完成')}</Button>
      </div>
    </PopoverContent>
  </Popover>
}