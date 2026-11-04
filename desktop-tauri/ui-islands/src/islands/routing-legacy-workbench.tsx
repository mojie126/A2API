import * as React from 'react'
import { Badge, Button, Checkbox, Empty, Input, Spinner, cn, confirmDialog } from '@ui'
import { t } from '../i18n'
import { displayNameOf, providerOf } from './accounts-domain'
import { RoutingLivePanel } from './routing-live-panel'
import { AddRouteModelDialog, RoutingSettingsDialog } from './routing-settings'
import { IconButton, RouteIcon, RouteStateBadge, stickyLabel, strategyLabel } from './routing-ui'
import { draftDirty, type RoutingEditorState } from './routing-editor-state'
import { bridge, errorMessage, type RouteEntry, type RouteProfile, type RoutingWorkspace } from './routing-types'

type Props = {
  editor: RoutingEditorState
  now: number
  onRequestTrace(): void
  onAccount(id: string): void
  onSticky(): void
  onAddModel(): void
}

type Modal = { kind: 'settings'; workspace: RoutingWorkspace } | { kind: 'model' } | null

/**
 * beta 版路由画布的恢复实现
 *
 * 保留模型轨道、提供商与账号画布和此刻队列的视觉结构
 * 数据与保存统一走新版路由草稿，作用范围、策略、粘滞和版本校验不降级
 * 请求回放通过顶部入口进入，不在默认配置界面中替换画布
 */
export function RoutingLegacyWorkbench({ editor, now, onRequestTrace, onAccount, onSticky, onAddModel }: Props) {
  const [modal, setModal] = React.useState<Modal>(null)
  const [search, setSearch] = React.useState('')
  const [opened, setOpened] = React.useState<ReadonlySet<string>>(new Set())
  const [showEmpty, setShowEmpty] = React.useState(false)
  const [preview, setPreview] = React.useState<RoutingWorkspace | null>(null)
  const [previewLoading, setPreviewLoading] = React.useState(false)
  const [previewError, setPreviewError] = React.useState('')
  const [previewModel, setPreviewModel] = React.useState('')
  const [previewToken, setPreviewToken] = React.useState(0)
  const draft = editor.draft
  const disabled = editor.busy || editor.loading

  const mutate = React.useCallback((fn: (profile: RouteProfile, workspace: RoutingWorkspace) => void) => {
    editor.update(current => {
      const profile = structuredClone(current.profile)
      fn(profile, current.workspace)
      return { ...current, profile }
    })
  }, [editor])

  React.useEffect(() => {
    if (!draft) {
      setPreview(null)
      setPreviewError('')
      setPreviewLoading(false)
      return
    }
    let target = editor.scope
    if (!target) {
      const models = draft.workspace.models
      const last = draft.workspace.queue.lastRequestModel || ''
      const known = models.find(model => model.toLowerCase() === previewModel.toLowerCase())
      target = known || models.find(model => model.toLowerCase() === last.toLowerCase()) || models[0] || ''
      if (target !== previewModel) {
        setPreviewModel(target)
        return
      }
    }
    if (!target) {
      setPreview(null)
      setPreviewLoading(false)
      return
    }
    let alive = true
    setPreviewLoading(true)
    setPreviewError('')
    bridge().getRoutingWorkspace(target).then(next => {
      if (alive) {
        setPreview(next)
        setPreviewLoading(false)
      }
    }).catch(failure => {
      if (alive) {
        setPreviewError(errorMessage(failure))
        setPreviewLoading(false)
      }
    })
    return () => { alive = false }
  }, [draft, editor.scope, previewModel, previewToken])

  if (!draft) {
    return <div className='routing-legacy-loading'><Empty>{editor.loading ? <Spinner /> : editor.error || t('选择作用范围后配置路由')}</Empty></div>
  }

  const workspace = draft.workspace
  const inherited = draft.mode === 'inherit'
  const profile = inherited ? workspace.inheritedProfile || draft.profile : draft.profile
  const locked = disabled || inherited
  const dirty = draftDirty(draft)
  const scopeLabel = editor.scope || t('全局配置')
  const configured = new Set(workspace.configuredModels.map(model => model.toLowerCase()))
  const models = [...workspace.models, ...(editor.scope && !workspace.models.some(model => model.toLowerCase() === editor.scope.toLowerCase()) ? [editor.scope] : [])]
  const query = search.trim().toLowerCase()
  const accounts = new Map(workspace.accounts.map(account => [account.id, account]))
  const queue = new Map<string, RouteEntry>([...workspace.queue.ordered, ...workspace.queue.blocked].map(entry => [entry.id, entry]))
  const providerLabel = (id: string) => workspace.providers.find(provider => provider.id === id)?.label || id
  const providerLive = (provider: string) => {
    const on = (profile.providerScope === 'all' || profile.providers.includes(provider)) && !profile.disabledProviders.includes(provider)
    return on && workspace.accounts.some(account => providerOf(account) === provider && account.enabled !== false)
  }
  const allProviders = [...new Set([...profile.providers, ...workspace.providers.map(provider => provider.id)])]
  const visibleProviders = allProviders.filter(provider => {
    if (!query) return showEmpty || workspace.accounts.some(account => providerOf(account) === provider)
    return providerLabel(provider).toLowerCase().includes(query)
      || workspace.accounts.some(account => providerOf(account) === provider && displayNameOf(account).toLowerCase().includes(query))
  }).sort((left, right) => Number(providerLive(right)) - Number(providerLive(left)))
  const emptyCount = allProviders.filter(provider => !workspace.accounts.some(account => providerOf(account) === provider)).length

  const toggleOpen = (provider: string) => setOpened(current => {
    const next = new Set(current)
    if (next.has(provider)) next.delete(provider)
    else next.add(provider)
    return next
  })
  const accountOrder = (provider: string) => {
    const ids = new Set(workspace.accounts.filter(account => providerOf(account) === provider).map(account => account.id))
    return [...(profile.accountOrder[provider] || []), ...ids].filter((id, index, list) => ids.has(id) && list.indexOf(id) === index)
  }
  const ensure = (next: RouteProfile, provider: string) => {
    if (!next.providers.includes(provider)) next.providers.push(provider)
    if (!next.accountOrder[provider]) next.accountOrder[provider] = accountOrder(provider)
  }
  const toggleProvider = (provider: string, on: boolean) => mutate(next => {
    ensure(next, provider)
    next.disabledProviders = on
      ? next.disabledProviders.filter(id => id !== provider)
      : [...new Set([...next.disabledProviders, provider])]
    if (next.providerScope !== 'all') next.providers = [...new Set([...next.providers, provider])]
  })
  const toggleAccount = (id: string, on: boolean) => mutate(next => {
    next.excludedAccounts = on
      ? next.excludedAccounts.filter(value => value !== id)
      : [...new Set([...next.excludedAccounts, id])]
  })
  const moveAccount = (provider: string, id: string, direction: number) => mutate(next => {
    ensure(next, provider)
    const values = next.accountOrder[provider]
    const index = values.indexOf(id)
    const to = index + direction
    if (index >= 0 && to >= 0 && to < values.length) [values[index], values[to]] = [values[to], values[index]]
  })
  const moveProvider = (id: string, direction: number) => mutate(next => {
    ensure(next, id)
    const peers = next.providers.filter(provider => providerLive(provider))
    const index = peers.indexOf(id)
    const target = peers[index + direction]
    if (!target) return
    const from = next.providers.indexOf(id)
    const to = next.providers.indexOf(target)
    if (from < 0 || to < 0) return
    next.providers.splice(from, 1)
    next.providers.splice(to, 0, id)
  })
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

  return <div className='routing-legacy-shell'>
    <div className='routing-legacy-toolbar'>
      <div className='routing-legacy-scope'>
        <RouteIcon name='route' />
        <b>{scopeLabel}</b>
        {editor.scope ? workspace.customized
          ? <Badge shape='tag' variant='brand'>{t('独立配置')}</Badge>
          : <Badge shape='tag' variant='secondary'>{t('继承全局')}</Badge>
          : <Badge shape='tag' variant={workspace.globalCustomized ? 'brand' : 'secondary'}>{workspace.globalCustomized ? t('全局配置') : t('默认路由')}</Badge>}
        {dirty ? <Badge shape='tag' variant='warning'>{t('未保存')}</Badge> : null}
      </div>
      <div className='routing-legacy-actions'>
        {editor.scope && !workspace.customized ? <Button variant='outline' size='sm' disabled={disabled} onClick={() => void createIndependent()}>{t('创建独立配置')}</Button> : null}
        {editor.scope && workspace.customized ? <Button variant='outline' size='sm' disabled={disabled} onClick={() => void removeIndependent()}>{t('删除独立配置')}</Button> : null}
        <Button variant='ghost' size='sm' onClick={onRequestTrace}><RouteIcon name='requests' />{t('请求回放')}</Button>
        <Button variant='ghost' size='sm' onClick={onSticky}><RouteIcon name='lock' />{t('会话粘滞')} · {workspace.stickyCount}</Button>
        <Button variant='ghost' size='sm' onClick={() => setModal({ kind: 'settings', workspace })}><RouteIcon name='settings' />{t('设置')}</Button>
        <Button variant='ghost' size='sm' disabled={disabled} onClick={() => void editor.load(editor.scope, true)}><RouteIcon name='refresh' />{t('重新读取')}</Button>
      </div>
    </div>

    <div className='routing-legacy-workspace'>
      <aside className='routing-legacy-model-rail'>
        <div className='routing-legacy-rail-head'><span>{t('模型路由')}</span><IconButton name='plus' title={t('新增模型独立路由')} disabled={disabled} onClick={onAddModel} /></div>
        <Button variant='ghost' className={cn('routing-legacy-model-item', !editor.scope && 'is-selected')} disabled={disabled} onClick={() => void editor.load('')}>
          <span className='routing-model-mark'><RouteIcon name='route' /></span>
          <span><b>{t('全局配置')}</b><em>{t('未配置模型的默认路由')}</em></span>
        </Button>
        <div className='routing-legacy-rail-divider'>{t('模型独立配置')}<Badge shape='tag' variant='secondary'>{workspace.configuredModels.length}</Badge></div>
        <div className='routing-legacy-model-list'>{models.map(model => <Button key={model} variant='ghost'
          className={cn('routing-legacy-model-item', editor.scope.toLowerCase() === model.toLowerCase() && 'is-selected')} disabled={disabled}
          onClick={() => void editor.load(model)}>
          <span className='routing-model-mark'><RouteIcon name='gateway' /></span>
          <span><b title={model}>{model}</b><em>{configured.has(model.toLowerCase()) ? t('独立配置') : t('继承全局')}</em></span>
        </Button>)}</div>
        <Button variant='ghost' className='routing-legacy-add-model' disabled={disabled} onClick={onAddModel}><span>＋</span>{t('新增模型')}</Button>
      </aside>

      <div className='routing-legacy-map'>
        <div className='routing-legacy-map-head'>
          <div className='routing-legacy-map-title'><b>{scopeLabel}</b>
            {dirty ? <span className='routing-legacy-dirty'>{t('{n} 处未保存', { n: draftDirty(draft) ? 1 : 0 })}</span> : null}
          </div>
          <div className='routing-legacy-map-actions'>
            {editor.scope ? <span>{t('预览 {model}', { model: editor.scope })}</span> : null}
            <Button variant='ghost' size='sm' disabled={disabled || !dirty} onClick={editor.discard}>{t('撤销')}</Button>
            <Button size='sm' disabled={disabled || !dirty} onClick={() => void editor.save()}>{editor.busy ? t('保存中…') : t('保存')}</Button>
          </div>
        </div>
        <div className='routing-legacy-columns'>
          <section className='routing-legacy-provider-column'>
            <div className='routing-legacy-provider-head'>
              <span><RouteIcon name='plugins' />{t('提供商与账号')}<Badge shape='tag' variant='secondary'>{visibleProviders.length}</Badge></span>
              <div><Input className='routing-legacy-search' placeholder={t('搜索提供商或账号')} value={search} onChange={event => setSearch(event.currentTarget.value)} />
                <Button variant='ghost' size='sm' onClick={() => setOpened(opened.size ? new Set() : new Set(visibleProviders))}>{opened.size ? t('折叠全部') : t('展开全部')}</Button></div>
            </div>
            <div className='routing-legacy-provider-list'>
              {!visibleProviders.length ? <Empty>{query ? t('没有匹配的提供商或账号') : t('还没有参与路由的提供商')}</Empty> : null}
              {visibleProviders.map(provider => {
                const accountsForProvider = accountOrder(provider)
                const filtered = accountsForProvider.filter(id => !query || providerLabel(provider).toLowerCase().includes(query)
                  || displayNameOf(accounts.get(id)).toLowerCase().includes(query))
                const on = (profile.providerScope === 'all' || profile.providers.includes(provider)) && !profile.disabledProviders.includes(provider)
                const open = opened.has(provider)
                const live = providerLive(provider)
                return <section key={provider} className={cn('routing-legacy-provider-card', !on && 'is-off')}>
                  <header>
                    <Checkbox checked={on} disabled={locked} aria-label={providerLabel(provider)} onCheckedChange={(value: boolean | 'indeterminate') => toggleProvider(provider, value === true)} />
                    <span className='routing-provider-mark routing-letter'>{providerLabel(provider).slice(0, 1).toUpperCase()}</span>
                    <button className='routing-legacy-provider-toggle' type='button' onClick={() => toggleOpen(provider)}>
                      <span className='routing-provider-name'><b>{providerLabel(provider)}</b><span>{t('{n} 个账号', { n: accountsForProvider.length })}</span></span>
                      <RouteStateBadge state={live ? 'usable' : 'excluded'}>{live ? t('参与') : t('未参与')}</RouteStateBadge>
                      <RouteIcon name={open ? 'chevronDown' : 'caretRight'} />
                    </button>
                    <IconButton name='arrowUp' title={t('提高优先级')} disabled={locked || !live || visibleProviders.indexOf(provider) <= 0} onClick={() => moveProvider(provider, -1)} />
                    <IconButton name='arrowDown' title={t('降低优先级')} disabled={locked || !live || visibleProviders.indexOf(provider) >= visibleProviders.length - 1} onClick={() => moveProvider(provider, 1)} />
                    <IconButton name='settings' title={t('提供商路由设置')} onClick={() => toggleOpen(provider)} />
                  </header>
                  {open ? <div className='routing-legacy-account-list'>
                    {filtered.length ? filtered.map(id => {
                      const account = accounts.get(id)!
                      const included = !profile.excludedAccounts.includes(id)
                      const state = queue.get(id)?.state || (account.enabled === false ? 'disabled' : 'unready')
                      return <div key={id} className={cn('routing-legacy-account-row', !included && 'is-excluded')}>
                        <span className='routing-account-rank'>{accountsForProvider.indexOf(id) + 1}</span>
                        <Checkbox checked={included} disabled={locked || !on} aria-label={displayNameOf(account)} onCheckedChange={(value: boolean | 'indeterminate') => toggleAccount(id, value === true)} />
                        <span className='routing-account-avatar'>{displayNameOf(account).slice(0, 1).toUpperCase()}</span>
                        <span className='routing-account-name'><b>{displayNameOf(account)}</b><span>{queue.get(id)?.reason || t('按当前顺序参与选路')}</span></span>
                        <RouteStateBadge state={included ? state : 'excluded'} />
                        <IconButton name='arrowUp' title={t('提高优先级')} disabled={locked || accountsForProvider.indexOf(id) <= 0} onClick={() => moveAccount(provider, id, -1)} />
                        <IconButton name='arrowDown' title={t('降低优先级')} disabled={locked || accountsForProvider.indexOf(id) >= accountsForProvider.length - 1} onClick={() => moveAccount(provider, id, 1)} />
                        <IconButton name='settings' title={t('账号设置')} onClick={() => onAccount(id)} />
                      </div>
                    }) : <p className='routing-legacy-empty-account'>{t('没有参与路由的账号')}</p>}
                  </div> : <p className='routing-legacy-collapsed'>{t('{n} 个账号', { n: accountsForProvider.length })}</p>}
                </section>
              })}
              {emptyCount && !query ? <Button variant='ghost' className='routing-legacy-show-empty' onClick={() => setShowEmpty(value => !value)}>{showEmpty ? t('隐藏无账号 ({n})', { n: emptyCount }) : t('显示无账号 ({n})', { n: emptyCount })}</Button> : null}
            </div>
          </section>
          <RoutingLivePanel workspace={preview} model={editor.scope || previewModel || workspace.models[0] || ''} loading={previewLoading}
            error={previewError} dirty={dirty} globalScope={!editor.scope} modelsAvailable={workspace.models.length > 0}
            onOpenScope={model => void editor.load(model)} onRefresh={() => setPreviewToken(value => value + 1)} />
        </div>
        <div className='routing-legacy-foot'><span>{t('从上到下就是实际选路顺序')}</span><span>{strategyLabel(profile.strategy || workspace.settings.routingStrategy)} · {t('粘滞 {mode}', { mode: stickyLabel(profile.sticky || workspace.settings.routingSticky) })}</span>
          <span>{dirty ? t('有未保存修改，预演仍按已保存配置') : t('保存后只影响后续请求')}</span></div>
      </div>
    </div>

    {modal?.kind === 'settings' ? <RoutingSettingsDialog workspace={modal.workspace} onClose={() => setModal(null)} /> : null}
    {modal?.kind === 'model' && draft ? <AddRouteModelDialog workspace={draft.workspace} onClose={() => setModal(null)} onAdd={editor.create} /> : null}
  </div>
}
