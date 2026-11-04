import * as React from 'react'
import {
  Badge, Button, Checkbox, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle,
  Empty, Input, Spinner, cn, confirmDialog,
} from '@ui'
import { t } from '../i18n'
import { activeGroupSortRules, groupSortSummary, manualOrderBlockedMessage } from './accounts-group-settings'
import { displayNameOf, providerOf } from './accounts-domain'
import { PROVIDER_ICONS } from './add-provider-pick'
import { AddRouteModelDialog, RoutingSettingsDialog } from './routing-settings'
import { draftDirty, type RoutingEditorState } from './routing-editor-state'
import { host, notify, type RouteEntry, type RouteProfile, type RoutingWorkspace } from './routing-types'
import {
  formatDuration, IconButton, ProviderRouteBadge, RouteIcon, RouteStateBadge, stickyLabel, strategyLabel,
} from './routing-ui'

type Props = {
  editor: RoutingEditorState
  onRequestTrace(): void
  onAccount(id: string): void
  onSticky(): void
  onAddModel(): void
}

type Modal =
  | { kind: 'settings'; workspace: RoutingWorkspace }
  | { kind: 'model' }
  | { kind: 'addProvider' }
  | { kind: 'provider'; id: string }
  | null

type DragState = {
  kind: 'account' | 'provider'
  id: string
  provider: string
  pointerId: number
  startX: number
  startY: number
  active: boolean
  target?: string
  after?: boolean
}

function ProviderMark({ id, label }: { id: string; label: string }) {
  return PROVIDER_ICONS[id]
    ? <img className='routing-provider-mark' src={PROVIDER_ICONS[id]} alt='' loading='lazy' />
    : <span className='routing-provider-mark routing-letter'>{label.slice(0, 1).toUpperCase()}</span>
}

/**
 * beta 版画布的连线层
 *
 * 同时绘制模型轨道到网关、网关到提供商卡片的两段贝塞尔曲线
 * 滚动和尺寸变化通过观察器重算，拖动重排后由提供商列表的节点变化触发重绘
 */
function CanvasConnections({ root, gateway, runningProviders, revision }: {
  root: React.RefObject<HTMLDivElement | null>
  gateway: React.RefObject<HTMLDivElement | null>
  runningProviders: ReadonlySet<string>
  revision: string
}) {
  const [paths, setPaths] = React.useState<Array<{ id: string; d: string; off: boolean; model?: boolean }>>([])
  const [size, setSize] = React.useState({ width: 0, height: 0 })

  React.useLayoutEffect(() => {
    const container = root.current
    const node = gateway.current
    if (!container || !node) return
    const list = container.querySelector<HTMLElement>('.routing-legacy-provider-list')
    const modelList = container.querySelector<HTMLElement>('.routing-legacy-model-list')
    let frame = 0
    const draw = () => {
      const bounds = container.getBoundingClientRect()
      const gatewayRect = node.getBoundingClientRect()
      if (gatewayRect.width <= 0 || gatewayRect.height <= 0) {
        setPaths([])
        setSize({ width: container.clientWidth, height: container.clientHeight })
        return
      }
      const next: Array<{ id: string; d: string; off: boolean; model?: boolean }> = []
      const model = container.querySelector<HTMLElement>('.routing-legacy-model-item.is-selected')
      const rail = container.querySelector<HTMLElement>('.routing-legacy-model-rail')?.getBoundingClientRect()
      if (model && rail) {
        const item = model.getBoundingClientRect()
        if (item.bottom > rail.top && item.top < rail.bottom) {
          const x1 = item.right - bounds.left
          const y1 = item.top + item.height / 2 - bounds.top
          const x2 = gatewayRect.left - bounds.left
          const y2 = gatewayRect.top + gatewayRect.height / 2 - bounds.top
          const mid = x1 + (x2 - x1) * 0.55
          next.push({ id: '__model__', d: `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`, off: false, model: true })
        }
      }
      if (list) {
        const listBounds = list.getBoundingClientRect()
        const x1 = gatewayRect.right - bounds.left
        const y1 = gatewayRect.top + gatewayRect.height / 2 - bounds.top
        for (const card of list.querySelectorAll<HTMLElement>('[data-routing-provider][data-routing-connect="true"]')) {
          const header = card.querySelector<HTMLElement>('header')?.getBoundingClientRect() || card.getBoundingClientRect()
          const center = header.top + header.height / 2
          if (center < listBounds.top || center > listBounds.bottom) continue
          const x2 = header.left - bounds.left
          const y2 = center - bounds.top
          const mid = x1 + (x2 - x1) * 0.52
          next.push({
            id: card.dataset.routingProvider || '',
            d: `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`,
            off: card.dataset.routingInactive === 'true',
          })
        }
      }
      setPaths(next)
      setSize({ width: container.clientWidth, height: container.clientHeight })
    }
    const schedule = () => {
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(draw)
    }
    const observer = new ResizeObserver(schedule)
    observer.observe(container)
    observer.observe(node)
    if (list) {
      observer.observe(list)
      list.querySelectorAll('[data-routing-provider]').forEach(card => observer.observe(card))
      list.addEventListener('scroll', schedule, { passive: true })
    }
    if (modelList) {
      observer.observe(modelList)
      modelList.addEventListener('scroll', schedule, { passive: true })
    }
    const mutation = new MutationObserver(schedule)
    if (list) mutation.observe(list, { childList: true, subtree: true })
    window.addEventListener('resize', schedule)
    schedule()
    return () => {
      observer.disconnect()
      mutation.disconnect()
      cancelAnimationFrame(frame)
      list?.removeEventListener('scroll', schedule)
      modelList?.removeEventListener('scroll', schedule)
      window.removeEventListener('resize', schedule)
    }
  }, [root, gateway, revision])

  return <svg className='routing-legacy-edges' width={size.width} height={size.height} aria-hidden='true'>
    {paths.map(path => <path key={path.id} d={path.d}
      className={cn('routing-legacy-edge', path.model && 'is-model', path.off && 'is-off', !path.off && runningProviders.has(path.id) && 'is-running')} />)}
  </svg>
}

/**
 * beta 版路由画布
 *
 * 保留模型轨道、网关节点、提供商与账号画布、已停用提供商分组和拖动排序
 * 数据统一来自新版路由草稿，保存、撤销、版本校验与多作用范围缓存不降级
 */
export function RoutingLegacyWorkbench({ editor, onRequestTrace, onAccount, onSticky, onAddModel }: Props) {
  const [modal, setModal] = React.useState<Modal>(null)
  const [search, setSearch] = React.useState('')
  const [opened, setOpened] = React.useState<ReadonlySet<string>>(new Set())
  const [inactiveExpanded, setInactiveExpanded] = React.useState(false)
  const [dragging, setDragging] = React.useState('')
  const [drop, setDrop] = React.useState<{ id: string; after: boolean } | null>(null)
  const root = React.useRef<HTMLDivElement>(null)
  const gateway = React.useRef<HTMLDivElement>(null)
  const drag = React.useRef<DragState | null>(null)
  const draft = editor.draft
  const disabled = editor.busy || editor.loading

  if (!draft) {
    return <div className='routing-legacy-loading'><Empty>{editor.loading ? <Spinner /> : editor.error || t('选择作用范围后配置路由')}</Empty></div>
  }

  const workspace = draft.workspace
  const profile = draft.profile
  const locked = disabled
  const dirty = draftDirty(draft)
  const scopeLabel = editor.scope || t('全局配置')
  const configured = new Set(workspace.configuredModels.map(model => model.toLowerCase()))
  const models = [...workspace.models, ...(editor.scope && !workspace.models.some(model => model.toLowerCase() === editor.scope.toLowerCase()) ? [editor.scope] : [])]
  const query = search.trim().toLowerCase()
  const accounts = new Map(workspace.accounts.map(account => [account.id, account]))
  const queue = new Map<string, RouteEntry>([...workspace.queue.ordered, ...workspace.queue.blocked].map(entry => [entry.id, entry]))
  const providerLabel = (id: string) => workspace.providers.find(provider => provider.id === id)?.label || id
  const providerAvailable = (provider: string) => workspace.providers.find(item => item.id === provider)?.available !== false
  const accountIds = (provider: string) => workspace.accounts.filter(account => providerOf(account) === provider).map(account => account.id)
  const accountOrder = (provider: string) => {
    const known = new Set(accountIds(provider))
    return [...(profile.accountOrder[provider] || []), ...workspace.accounts.filter(account => providerOf(account) === provider)
      .sort((left, right) => Number(left.priority) - Number(right.priority)).map(account => account.id)]
      .filter((id, index, list) => known.has(id) && list.indexOf(id) === index)
  }
  const ensure = (next: RouteProfile, provider: string) => {
    if (!next.providers.includes(provider)) next.providers.push(provider)
    const ids = accountIds(provider)
    const current = next.accountOrder[provider] || []
    next.accountOrder[provider] = [...current.filter(id => ids.includes(id)), ...ids.filter(id => !current.includes(id))]
  }
  const providerEnabled = (provider: string) => providerAvailable(provider)
    && (profile.providerScope === 'all' || profile.providers.includes(provider))
    && !profile.disabledProviders.includes(provider)
  const orderRules = (provider: string) => editor.scope && workspace.customized
    ? []
    : activeGroupSortRules(provider, workspace.sorting.rules[provider] || [])
  const orderLocked = (provider: string) => orderRules(provider).length > 0
  const orderLockMessage = (provider: string) => manualOrderBlockedMessage(orderRules(provider))
  const baseProviders = [...new Set([...profile.providers, ...workspace.providers.map(provider => provider.id)])]
  const activeProviders = baseProviders.filter(providerEnabled)
  const inactiveProviders = baseProviders.filter(provider => !providerEnabled(provider))
  const providerRanks = new Map<string, number>()
  for (const entry of workspace.queue.ordered) {
    if (!providerEnabled(entry.provider)) continue
    if (!providerRanks.has(entry.provider)) providerRanks.set(entry.provider, providerRanks.size + 1)
  }
  const activeRanks = new Map(activeProviders.map((provider, index) => [provider, index + 1]))
  const providerMatches = (provider: string) => !query
    || providerLabel(provider).toLowerCase().includes(query)
    || accountOrder(provider).some(id => displayNameOf(accounts.get(id)).toLowerCase().includes(query))
  const visibleActive = activeProviders.filter(providerMatches)
  const visibleInactive = inactiveProviders.filter(providerMatches)
  const renderedInactive = inactiveExpanded || Boolean(query)
  const allCollapsed = visibleActive.every(provider => !opened.has(provider))
  const gatewayBase = host().wbPortPanel?.gatewayBase() || `http://127.0.0.1:${workspace.gateway.port}`
  const runningProviders = new Set(workspace.accounts.filter(account => (workspace.metrics.accountRunning?.[account.id] || 0) > 0).map(providerOf))
  const revision = `${workspace.updatedAt}:${profile.providers.join(',')}:${[...opened].join(',')}:${query}:${inactiveExpanded}`

  const updateDraft = (fn: (profile: RouteProfile, workspace: RoutingWorkspace) => void) => {
    editor.update(current => {
      const next = current.mode === 'inherit'
        ? { ...current, mode: 'custom' as const, baseline: null, profile: structuredClone(current.profile) }
        : current
      const profile = structuredClone(next.profile)
      fn(profile, next.workspace)
      return { ...next, profile }
    })
  }

  const latest = React.useRef({ locked, disabled, query, activeProviders, inactiveProviders, orderLocked, orderLockMessage, updateDraft })
  latest.current = { locked, disabled, query, activeProviders, inactiveProviders, orderLocked, orderLockMessage, updateDraft }

  React.useEffect(() => {
    const move = (event: PointerEvent) => {
      const current = drag.current
      if (!current || current.pointerId !== event.pointerId) return
      if (!current.active && Math.hypot(event.clientX - current.startX, event.clientY - current.startY) < 5) return
      current.active = true
      setDragging(current.id)
      event.preventDefault()
      const hit = document.elementFromPoint(event.clientX, event.clientY)
      const found = hit?.closest<HTMLElement>(current.kind === 'account' ? '[data-routing-account]' : '[data-routing-provider]')
      const target = found && root.current?.contains(found) ? found : null
      const id = current.kind === 'account' ? target?.dataset.routingAccount : target?.dataset.routingProvider
      const sameGroup = current.kind === 'account'
        ? target?.dataset.provider === current.provider
        : target?.dataset.routingInactive === String(latest.current.inactiveProviders.includes(current.id))
      if (!target || !id || id === current.id || !sameGroup) {
        current.target = undefined
        current.after = undefined
        setDrop(null)
        return
      }
      const bounds = target.getBoundingClientRect()
      current.target = id
      current.after = event.clientY > bounds.top + bounds.height / 2
      setDrop({ id, after: current.after })
    }
    const end = (event: PointerEvent) => {
      const current = drag.current
      if (!current || event.pointerId !== current.pointerId) return
      drag.current = null
      setDragging('')
      setDrop(null)
      if (event.type === 'pointercancel' || !current.active || !current.target) return
      const state = latest.current
      if (state.locked || state.disabled) return
      if (current.kind === 'account' && state.orderLocked(current.provider)) {
        notify(state.orderLockMessage(current.provider), 'err')
        return
      }
      state.updateDraft((next, snapshot) => {
        if (current.kind === 'provider') {
          ensure(next, current.id)
          ensure(next, current.target!)
          const from = next.providers.indexOf(current.id)
          const target = next.providers.indexOf(current.target!)
          if (from < 0 || target < 0) return
          const [provider] = next.providers.splice(from, 1)
          const insert = target + (current.after ? 1 : 0) - (from < target ? 1 : 0)
          next.providers.splice(insert, 0, provider)
          return
        }
        ensure(next, current.provider)
        const ids = next.accountOrder[current.provider]
        const from = ids.indexOf(current.id)
        const target = ids.indexOf(current.target!)
        if (from < 0 || target < 0) return
        const [account] = ids.splice(from, 1)
        const insert = target + (current.after ? 1 : 0) - (from < target ? 1 : 0)
        ids.splice(insert, 0, account)
      })
    }
    window.addEventListener('pointermove', move, { passive: false })
    window.addEventListener('pointerup', end)
    window.addEventListener('pointercancel', end)
    return () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', end)
      window.removeEventListener('pointercancel', end)
    }
  }, [])

  const startDrag = (event: React.PointerEvent<HTMLButtonElement>, kind: DragState['kind'], id: string, provider: string) => {
    if (event.button !== 0 || locked || disabled || query) return
    if (kind === 'account' && orderLocked(provider)) {
      event.preventDefault()
      notify(orderLockMessage(provider), 'err')
      return
    }
    event.preventDefault()
    event.currentTarget.setPointerCapture?.(event.pointerId)
    drag.current = {
      kind, id, provider, pointerId: event.pointerId,
      startX: event.clientX, startY: event.clientY, active: false,
    }
  }

  const toggleOpen = (provider: string) => setOpened(current => {
    const next = new Set(current)
    if (next.has(provider)) next.delete(provider)
    else next.add(provider)
    return next
  })
  const toggleProvider = (provider: string, on: boolean) => updateDraft(next => {
    ensure(next, provider)
    next.disabledProviders = on
      ? next.disabledProviders.filter(id => id !== provider)
      : [...new Set([...next.disabledProviders, provider])]
    if (on && next.providerScope !== 'all' && !next.providers.includes(provider)) next.providers.push(provider)
  })
  const toggleAccount = (id: string, on: boolean) => updateDraft(next => {
    next.excludedAccounts = on
      ? next.excludedAccounts.filter(value => value !== id)
      : [...new Set([...next.excludedAccounts, id])]
  })
  const moveProvider = (provider: string, direction: -1 | 1) => updateDraft(next => {
    ensure(next, provider)
    const inactive = !providerEnabled(provider)
    const peers = inactive ? inactiveProviders : activeProviders
    const index = peers.indexOf(provider)
    const target = peers[index + direction]
    if (!target) return
    ensure(next, target)
    const from = next.providers.indexOf(provider)
    const to = next.providers.indexOf(target)
    if (from < 0 || to < 0) return
    const [moved] = next.providers.splice(from, 1)
    next.providers.splice(to, 0, moved)
  })
  const moveAccount = (provider: string, id: string, direction: -1 | 1) => updateDraft(next => {
    ensure(next, provider)
    const ids = next.accountOrder[provider]
    const index = ids.indexOf(id)
    const target = ids[index + direction]
    if (index < 0 || !target) return
    const [moved] = ids.splice(index, 1)
    ids.splice(index + direction, 0, moved)
  })
  const removeProvider = (provider: string) => {
    if (locked) return
    updateDraft(next => {
      if (next.providerScope === 'all') {
        next.disabledProviders = [...new Set([...next.disabledProviders, provider])]
        return
      }
      next.providers = next.providers.filter(id => id !== provider)
      next.disabledProviders = next.disabledProviders.filter(id => id !== provider)
      delete next.accountOrder[provider]
    })
    setModal(null)
  }
  const removeIndependent = async () => {
    if (!editor.scope || disabled) return
    if (!await confirmDialog(t('删除后这个模型会重新跟随全局配置，已保存的独立设置和当前未保存修改都会被移除。'), {
      title: t('删除独立配置'), confirmText: t('删除'), destructive: true,
    })) return
    await editor.remove()
  }
  const createIndependent = async () => {
    if (!editor.scope || disabled) return
    await editor.create(editor.scope)
  }

  const renderProvider = (provider: string, inactive: boolean, group: string[], providerIndex: number) => {
    const label = providerLabel(provider)
    const ids = accountOrder(provider)
    const filteredIds = ids.filter(id => !query || label.toLowerCase().includes(query)
      || displayNameOf(accounts.get(id)).toLowerCase().includes(query))
    const on = providerEnabled(provider)
    const open = opened.has(provider)
    const running = ids.reduce((sum, id) => sum + (workspace.metrics.accountRunning?.[id] || 0), 0)
    const rank = inactive ? undefined : activeRanks.get(provider)
    const effectiveRank = providerRanks.get(provider)
    const unavailable = !providerAvailable(provider)
    const lockedOrder = orderLocked(provider)
    const states = ids.map(id => {
      const account = accounts.get(id)!
      return profile.excludedAccounts.includes(id) ? 'excluded'
        : queue.get(id)?.state || (account.enabled === false ? 'disabled' : account.hasCredentials === false ? 'no-credentials' : 'unready')
    })
    return <section key={provider}
      className={cn('routing-legacy-provider-card', inactive && 'is-inactive',
        dragging === provider && 'is-dragging', drop?.id === provider && (drop.after ? 'drop-after' : 'drop-before'))}
      data-routing-provider={provider} data-routing-connect={!inactive}
      data-routing-inactive={inactive}>
      <header>
        <Checkbox checked={on} disabled={locked} aria-label={label}
          onCheckedChange={(value: boolean | 'indeterminate') => toggleProvider(provider, value === true)} />
        <Button variant='ghost' size='icon' className='routing-grip'
          disabled={disabled || Boolean(query)} aria-label={t('拖动调整提供商顺序')}
          title={t('拖动调整提供商顺序')} onPointerDown={event => startDrag(event, 'provider', provider, provider)}>
          <RouteIcon name='gripVertical' size={14} />
        </Button>
        <button type='button' className='routing-legacy-provider-toggle' onClick={() => toggleOpen(provider)}
          title={open ? t('折叠这一组') : t('展开这一组')}>
          <ProviderMark id={provider} label={label} />
          <span className='routing-provider-name'><b>{label}</b><span>{t('{n} 个账号 · {count} 个请求', { n: ids.length, count: running })}</span></span>
          {!inactive && !lockedOrder ? <ProviderRouteBadge rank={rank} effectiveRank={effectiveRank}
            unavailable={unavailable} disabled={!on} states={states} /> : null}
          {lockedOrder ? <Badge shape='tag' variant='secondary' title={orderLockMessage(provider)}>{groupSortSummary(orderRules(provider))}</Badge> : null}
          {inactive ? <RouteStateBadge state='excluded' /> : null}
          <RouteIcon name={open ? 'chevronDown' : 'caretRight'} size={14} />
        </button>
        <IconButton name='arrowUp' title={t('提高优先级')} disabled={disabled || Boolean(query) || providerIndex <= 0}
          onClick={() => moveProvider(provider, -1)} />
        <IconButton name='arrowDown' title={t('降低优先级')} disabled={disabled || Boolean(query) || providerIndex >= group.length - 1}
          onClick={() => moveProvider(provider, 1)} />
        <IconButton name='settings' title={t('提供商路由设置')} onClick={() => setModal({ kind: 'provider', id: provider })} />
      </header>
      {open ? <div className='routing-legacy-account-list'>
        {lockedOrder ? <div className='routing-legacy-order-lock'>
          <RouteIcon name='lock' size={13} /><span>{orderLockMessage(provider)}</span>
          <Button variant='ghost' size='sm' onClick={() => setModal({ kind: 'provider', id: provider })}>{t('消耗顺序')}</Button>
        </div> : null}
        {filteredIds.length ? filteredIds.map(id => {
          const account = accounts.get(id)!
          const included = !profile.excludedAccounts.includes(id)
          const state = included ? queue.get(id)?.state || (account.enabled === false ? 'disabled' : account.hasCredentials === false ? 'no-credentials' : 'unready') : 'excluded'
          const description = !included ? t('当前视图未参与')
            : account.enabled === false ? t('账号已停用')
              : queue.get(id)?.reason || t('按当前顺序参与选路')
          return <div key={id} className={cn('routing-legacy-account-row', !included && 'is-excluded',
            dragging === id && 'is-dragging', drop?.id === id && (drop.after ? 'drop-after' : 'drop-before'))}
          data-routing-account={id} data-provider={provider} title={queue.get(id)?.reason}>
            <Button variant='ghost' size='icon' className={cn('routing-grip', lockedOrder && 'is-locked')}
              disabled={disabled || Boolean(query)} aria-disabled={lockedOrder || undefined}
              aria-label={t('拖动调整账号顺序')} title={lockedOrder ? orderLockMessage(provider) : t('拖动调整账号顺序')}
              onClick={event => { if (lockedOrder && event.detail === 0) notify(orderLockMessage(provider), 'err') }}
              onPointerDown={event => startDrag(event, 'account', id, provider)}><RouteIcon name='gripVertical' size={14} /></Button>
            <Checkbox checked={included} disabled={locked || !on} aria-label={displayNameOf(account)}
              onCheckedChange={(value: boolean | 'indeterminate') => toggleAccount(id, value === true)} />
            <span className='routing-account-rank'>{ids.indexOf(id) + 1}</span>
            <span className='routing-account-avatar'>{displayNameOf(account).slice(0, 1).toUpperCase()}</span>
            <span className='routing-account-name'><b>{displayNameOf(account)}</b><span>{description}</span></span>
            {inactive ? <RouteStateBadge state='excluded' /> : <RouteStateBadge state={state} />}
            <span className='routing-account-concurrency' title={t('在途请求 / 并发上限')}>{workspace.metrics.accountConnections?.[id] || 0}<em>/ {Number(account.maxConcurrent) || '∞'}</em></span>
            <IconButton name='arrowUp' title={t('提高优先级')} disabled={disabled || Boolean(query) || lockedOrder || ids.indexOf(id) <= 0}
              onClick={() => moveAccount(provider, id, -1)} />
            <IconButton name='arrowDown' title={t('降低优先级')} disabled={disabled || Boolean(query) || lockedOrder || ids.indexOf(id) >= ids.length - 1}
              onClick={() => moveAccount(provider, id, 1)} />
            <IconButton name='settings' title={t('账号设置')} onClick={() => onAccount(id)} />
          </div>
        }) : <p className='routing-legacy-empty-account'>{t('没有参与路由的账号')}</p>}
      </div> : <p className='routing-legacy-collapsed'>{t('{n} 个账号', { n: ids.length })}{lockedOrder ? ` · ${groupSortSummary(orderRules(provider))}` : ''}</p>}
    </section>
  }

  return <div className='routing-legacy-shell'>
    <div className='routing-legacy-workspace' ref={root}>
      <CanvasConnections root={root} gateway={gateway} runningProviders={runningProviders} revision={revision} />
      <aside className='routing-legacy-model-rail'>
        <div className='routing-legacy-rail-head'>
          <span>{t('模型路由')}</span>
          <IconButton name='plus' title={t('新增模型独立路由')} disabled={disabled} onClick={onAddModel} />
        </div>
        <Button variant='ghost' className={cn('routing-legacy-model-item', !editor.scope && 'is-selected')}
          disabled={disabled} onClick={() => void editor.load('')}>
          <span className='routing-model-mark'><RouteIcon name='route' /></span>
          <span><b>{t('全局配置')}</b><em>{t('未配置模型的默认路由')}</em></span>
          {!editor.scope ? <span className='routing-model-dot' /> : null}
        </Button>
        <div className='routing-legacy-rail-divider'>
          {t('模型独立配置')}<Badge shape='tag' variant='secondary'>{workspace.configuredModels.length}</Badge>
        </div>
        <div className='routing-legacy-model-list'>{models.map(model => <Button key={model} variant='ghost'
          className={cn('routing-legacy-model-item', editor.scope.toLowerCase() === model.toLowerCase() && 'is-selected')}
          disabled={disabled} onClick={() => void editor.load(model)}>
          <span className='routing-model-mark'><RouteIcon name='gateway' /></span>
          <span><b title={model}>{model}</b><em>{configured.has(model.toLowerCase()) ? t('独立配置') : t('继承全局')}</em></span>
          {editor.scope.toLowerCase() === model.toLowerCase() ? <span className='routing-model-dot' /> : null}
        </Button>)}</div>
        <Button variant='ghost' className='routing-legacy-add-model' disabled={disabled} onClick={onAddModel}>
          <span>＋</span>{t('新增模型')}
        </Button>
      </aside>

      <div className='routing-legacy-map'>
        <div className='routing-legacy-map-head'>
          <div className='routing-legacy-map-title'>
            <b>{scopeLabel}</b>
            {editor.scope ? workspace.customized
              ? <Badge shape='tag' variant='brand'>{t('独立配置')}</Badge>
              : <Badge shape='tag' variant='secondary'>{t('继承全局')}</Badge>
              : <Badge shape='tag' variant={workspace.globalCustomized ? 'brand' : 'secondary'}>{workspace.globalCustomized ? t('全局配置') : t('默认路由')}</Badge>}
            {dirty ? <span className='routing-legacy-dirty'>{t('有未保存修改')}</span> : null}
          </div>
          <div className='routing-legacy-map-actions'>
            {editor.scope && !workspace.customized ? <Button variant='outline' size='sm' disabled={disabled} onClick={() => void createIndependent()}>{t('创建独立配置')}</Button> : null}
            <Button variant='ghost' size='sm' onClick={onRequestTrace}><RouteIcon name='requests' />{t('请求跟踪')}</Button>
            <Button variant='ghost' size='sm' onClick={onSticky}><RouteIcon name='lock' />{t('会话粘滞')} · {workspace.stickyCount}</Button>
            <Button variant='ghost' size='sm' onClick={() => setModal({ kind: 'settings', workspace })}><RouteIcon name='settings' />{t('设置')}</Button>
            {editor.scope && workspace.customized ? <Button variant='ghost' size='sm' className='is-danger' disabled={disabled} onClick={() => void removeIndependent()}><RouteIcon name='trash' />{t('删除')}</Button> : null}
            <Button variant='ghost' size='sm' disabled={disabled || !dirty} onClick={editor.discard}>{t('撤销')}</Button>
            <Button size='sm' disabled={disabled || !dirty} onClick={() => void editor.save()}>{editor.busy ? t('保存中…') : t('保存')}</Button>
          </div>
        </div>

        <div className='routing-legacy-canvas-viewport'>
          <div className='routing-legacy-canvas'>
            <div className='routing-legacy-gateway-column'>
              <div className='routing-legacy-column-label'><RouteIcon name='traffic' />{t('本地网关')}</div>
              <div className='routing-legacy-gateway-node' ref={gateway}>
                <div className='routing-legacy-gateway-title'>
                  <span className='routing-legacy-gateway-icon'><RouteIcon name='route' size={24} /></span>
                  <div><h3>Agent2API</h3><span>{t('当前视图的请求入口')}</span></div>
                </div>
                <div className='routing-legacy-running'>
                  <strong>{workspace.metrics.running ?? '—'}</strong><span>{t('当前请求')}</span>
                  <Badge shape='tag' variant='success'>{t('运行中')}</Badge>
                </div>
                <div className='routing-legacy-gateway-url'>
                  <code title={gatewayBase}>{gatewayBase}/v1</code>
                  <IconButton name='copy' title={t('复制网关地址')} data-copy={`${gatewayBase}/v1`} />
                </div>
                <div className='routing-legacy-gateway-stats'>
                  <div><span>{t('今日请求')}</span><b>{workspace.metrics.todayRequests?.toLocaleString() ?? '—'}</b></div>
                  <div><span>{t('成功率')}</span><b>{workspace.metrics.todaySuccessRate == null ? '—' : `${workspace.metrics.todaySuccessRate.toFixed(1)}%`}</b></div>
                  <div><span>{t('平均耗时')}</span><b>{formatDuration(workspace.metrics.averageDurationMs)}</b></div>
                </div>
                <div className='routing-legacy-gateway-policy'>
                  <Badge shape='tag' variant='brand'>{strategyLabel(profile.strategy || workspace.settings.routingStrategy)}</Badge>
                  <Badge shape='tag' variant='outline'>{t('粘性：{mode}', { mode: stickyLabel(profile.sticky || workspace.settings.routingSticky) })}</Badge>
                </div>
                <Button variant='outline' className='w-full' onClick={() => setModal({ kind: 'settings', workspace })}>
                  <RouteIcon name='settings' />{t('统一路由设置')}
                </Button>
                <div className='routing-legacy-gateway-shortcuts'>
                  <Button variant='ghost' size='sm' onClick={onRequestTrace}>{t('请求跟踪')}</Button>
                  <Button variant='ghost' size='sm' onClick={onSticky}>{t('会话粘滞')} · {workspace.stickyCount}</Button>
                </div>
              </div>
              <div className='routing-legacy-scope-note'>
                <RouteIcon name={workspace.customized ? 'lock' : 'proxies'} />
                <p>{workspace.customized ? t('当前模型配置独立保存，不影响全局和其他模型。') : t('没有独立配置的模型沿用全局路由。')}</p>
              </div>
            </div>

            <div className='routing-legacy-provider-column'>
              <div className='routing-legacy-provider-head'>
                <span><RouteIcon name='plugins' />{t('提供商与账号')}<Badge shape='tag' variant='secondary'>{activeProviders.length}</Badge></span>
                <div>
                  <Input className='routing-legacy-search' placeholder={t('搜索提供商或账号')} value={search}
                    onChange={event => setSearch(event.currentTarget.value)} />
                  <Button variant='ghost' size='sm' disabled={!visibleActive.length}
                    title={allCollapsed ? t('展开全部分组') : t('折叠全部分组')}
                    onClick={() => setOpened(allCollapsed ? new Set(visibleActive) : new Set())}>{allCollapsed ? t('展开全部') : t('折叠全部')}</Button>
                </div>
              </div>
              <div className='routing-legacy-provider-list'>
                {!visibleActive.length && !visibleInactive.length ? <Empty>{query ? t('没有匹配的提供商或账号') : t('还没有参与路由的提供商')}</Empty> : null}
                {visibleActive.map((provider, index) => renderProvider(provider, false, visibleActive, index))}
                {visibleInactive.length ? <Button variant='ghost' className='routing-legacy-inactive-toggle'
                  aria-expanded={renderedInactive} disabled={Boolean(query)}
                  onClick={() => setInactiveExpanded(value => !value)}>
                  <RouteIcon name='chevronDown' />{t('已停用提供商 · {n}', { n: visibleInactive.length })}
                  <span>{renderedInactive ? t('收起') : t('展开')}</span>
                </Button> : null}
                {renderedInactive ? visibleInactive.map((provider, index) => renderProvider(provider, true, visibleInactive, index)) : null}
                <Button variant='ghost' className='routing-legacy-add-provider' onClick={() => setModal({ kind: 'addProvider' })}>
                  <RouteIcon name='plus' />{t('添加提供商')}
                </Button>
              </div>
            </div>
          </div>
        </div>
        <div className='routing-legacy-foot'>
          <span>{t('拖动提供商与账号调整顺序')}</span>
          <span>{strategyLabel(profile.strategy || workspace.settings.routingStrategy)} · {t('粘滞 {mode}', { mode: stickyLabel(profile.sticky || workspace.settings.routingSticky) })}</span>
          <span>{dirty ? t('有未保存修改，保存后只影响后续请求') : t('保存后只影响后续请求')}</span>
        </div>
      </div>
    </div>

    {modal?.kind === 'settings' ? <RoutingSettingsDialog workspace={modal.workspace} onClose={() => setModal(null)} /> : null}
    {modal?.kind === 'model' && draft ? <AddRouteModelDialog workspace={draft.workspace} onClose={() => setModal(null)} onAdd={editor.create} /> : null}
    {modal?.kind === 'addProvider' ? <AddProviderDraftDialog workspace={workspace} active={activeProviders} locked={locked}
      onAdd={ids => updateDraft(next => {
        for (const provider of ids) {
          ensure(next, provider)
          next.disabledProviders = next.disabledProviders.filter(id => id !== provider)
        }
      })} onClose={() => setModal(null)} /> : null}
    {modal?.kind === 'provider' ? <ProviderDraftDialog provider={modal.id} workspace={workspace} profile={profile}
      locked={locked} orderRules={orderRules(modal.id)} onToggle={on => toggleProvider(modal.id, on)}
      onToggleAccount={(id, on) => toggleAccount(id, on)} onRemove={() => removeProvider(modal.id)}
      onClose={() => setModal(null)} /> : null}
  </div>
}

function AddProviderDraftDialog({ workspace, active, locked, onAdd, onClose }: {
  workspace: RoutingWorkspace
  active: string[]
  locked: boolean
  onAdd(ids: string[]): void
  onClose(): void
}) {
  const [picked, setPicked] = React.useState<string[]>([])
  const options = workspace.providers.filter(provider => !active.includes(provider.id))
  return <Dialog open onOpenChange={open => { if (!open) onClose() }}>
    <DialogContent>
      <DialogHeader><DialogTitle>{t('添加路由提供商')}</DialogTitle></DialogHeader>
      <DialogBody>
        <p className='text-xs text-muted-foreground'>{t('添加到当前路由草稿，完成后再统一保存。')}</p>
        {options.length ? options.map(provider => <label key={provider.id}
          className='flex items-center gap-3 rounded-md border border-border p-3'>
          <Checkbox checked={picked.includes(provider.id)} disabled={locked}
            onCheckedChange={(value: boolean | 'indeterminate') => setPicked(current => value === true
              ? [...current, provider.id] : current.filter(id => id !== provider.id))} />
          <span className='flex-1'>{provider.label}</span>
          <Badge shape='tag' variant='secondary'>{t('{n} 个账号', { n: provider.accountCount })}</Badge>
        </label>) : <Empty>{t('所有可用提供商都已在当前路由中')}</Empty>}
      </DialogBody>
      <DialogFooter>
        <Button variant='outline' onClick={onClose}>{t('取消')}</Button>
        <Button disabled={locked || !picked.length} onClick={() => { onAdd(picked); onClose() }}>{t('添加')}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
}

function ProviderDraftDialog({ provider, workspace, profile, locked, orderRules, onToggle, onToggleAccount, onRemove, onClose }: {
  provider: string
  workspace: RoutingWorkspace
  profile: RouteProfile
  locked: boolean
  orderRules: ReturnType<typeof activeGroupSortRules>
  onToggle(on: boolean): void
  onToggleAccount(id: string, on: boolean): void
  onRemove(): void
  onClose(): void
}) {
  const label = workspace.providers.find(item => item.id === provider)?.label || provider
  const accounts = workspace.accounts.filter(account => providerOf(account) === provider)
  const enabled = (profile.providerScope === 'all' || profile.providers.includes(provider))
    && !profile.disabledProviders.includes(provider)
  return <Dialog open onOpenChange={open => { if (!open) onClose() }}>
    <DialogContent className='w-[min(640px,calc(100vw-48px))]'>
      <DialogHeader>
        <ProviderMark id={provider} label={label} />
        <DialogTitle>{t('{name} · 提供商路由设置', { name: label })}</DialogTitle>
      </DialogHeader>
      <DialogBody>
        <Badge variant='brand' shape='tag' className='self-start'>{workspace.model || t('全局配置')}</Badge>
        <section className='flex flex-col gap-2 border-b border-hairline pb-4'>
          <div className='flex items-center justify-between gap-3'>
            <h3 className='text-sm font-semibold'>{t('消耗顺序')}</h3>
            <Button variant='outline' size='sm' onClick={() => host().wbApp?.showPage('accounts')}>
              {t('调整全局消耗规则')}
            </Button>
          </div>
          <p className='text-xs text-muted-foreground'>{workspace.model && workspace.customized
            ? t('当前模型使用独立顺序，不受全局自动消耗排序影响。')
            : t('与账号页共用：{rules}。积分到期等规则直接影响网关实际选路。', {
              rules: groupSortSummary(orderRules) || t('仅按手动顺序'),
            })}</p>
        </section>
        <label className='flex items-center gap-3 rounded-md border border-border p-3'>
          <Checkbox checked={enabled} disabled={locked} aria-label={t('参与当前视图转发')}
            onCheckedChange={(value: boolean | 'indeterminate') => onToggle(value === true)} />
          <span className='flex min-w-0 flex-1 flex-col gap-px'>
            <b className='text-xs font-semibold'>{t('参与当前视图转发')}</b>
            <span className='text-[10.5px] text-subtle'>{t('停用后保留卡片与账号，修改会随当前草稿统一保存。')}</span>
          </span>
        </label>
        <section className='flex flex-col gap-2 border-t border-hairline pt-4'>
          <h3 className='text-sm font-semibold'>{t('参与路由的账号')}</h3>
          {accounts.length ? accounts.map(account => <label key={account.id}
            className='flex items-center gap-3 border-b border-hairline py-2 last:border-0'>
            <Checkbox checked={!profile.excludedAccounts.includes(account.id)} disabled={locked || !enabled}
              aria-label={displayNameOf(account)}
              onCheckedChange={(value: boolean | 'indeterminate') => onToggleAccount(account.id, value === true)} />
            <span className='min-w-0 flex-1 text-xs'>{displayNameOf(account)}</span>
            <RouteStateBadge state={account.enabled === false ? 'disabled' : 'usable'} />
          </label>) : <Empty>{t('该提供商没有账号，请先到账号页添加。')}</Empty>}
          <p className='text-xs text-muted-foreground'>{t('这里的参与范围只影响当前视图，账号启停、并发和限制器属于账号本身。')}</p>
        </section>
      </DialogBody>
      <DialogFooter>
        <Button variant='ghost' className='mr-auto text-destructive' disabled={locked}
          onClick={onRemove}>{profile.providerScope === 'all' ? t('停用此提供商') : t('从当前视图移除')}</Button>
        <Button variant='outline' onClick={onClose}>{t('取消')}</Button>
        <Button onClick={onClose}>{t('完成')}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
}
