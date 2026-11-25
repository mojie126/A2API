import type { PanelChanges } from '../../../ui-shared/panel-events'
import { PANEL_EVENTS_PATH, subscribePanelEvents } from '../../../ui-shared/panel-events'
import { requestDirect } from './assistant-api'
import { bump, getStore } from './accounts-store'
import { refreshAccountIndex } from './accounts-index-refresh'
import { setAccountEventsConnected } from './accounts-live-state'
import { invalidateAccountDetails } from './accounts-list-api'
import { syncBalancesSnapshot } from './accounts-usage'
import { syncConnections } from './accounts-data'
import { refreshProxyPool } from './proxy-pool-cache'
import { shared, type AccountRecord } from './accounts-shared'
import { isModalActive, onModalActiveChange } from '@ui/lib/modal-activity'

let started = false
let stop: (() => void) | undefined
let syncing = false
let generation = 0
let balanceRevision = 0
let pending: PanelChanges = { accounts: false, balances: false, usage: false, connections: false, proxies: false }

export const accountBalanceEventsRevision = (): number => balanceRevision

function visible(): boolean {
  // 弹窗打开时不刷新：下方重绘会让遮罩重新合成，高负载下就是弹窗卡顿的来源
  return !document.hidden && !isModalActive() && shared().wbApp?.currentPage === 'accounts'
}

async function sync(changes: PanelChanges): Promise<void> {
  const tasks: Array<Promise<unknown>> = []
  if (changes.accounts || changes.proxies) tasks.push(refreshAccountIndex())
  if (changes.accounts) tasks.push(shared().wbProviders?.refreshCustom?.() || Promise.resolve())
  if (changes.balances || changes.usage) tasks.push(syncBalancesSnapshot())
  if (changes.connections) tasks.push(syncConnections())
  if (changes.proxies) {
    tasks.push(refreshProxyPool())
    invalidateAccountDetails(getStore().visibleUsageIds)
    bump()
  }
  await Promise.allSettled(tasks)
  if (changes.balances) { balanceRevision++; bump() }
}

function enqueue(changes: PanelChanges): void {
  for (const key of Object.keys(pending) as Array<keyof PanelChanges>) pending[key] ||= changes[key]
  if (syncing) return
  const run = generation
  syncing = true
  void (async () => {
    while (visible() && run === generation && Object.values(pending).some(Boolean)) {
      const changes = pending
      pending = { accounts: false, balances: false, usage: false, connections: false, proxies: false }
      await sync(changes)
    }
  })().finally(() => {
    syncing = false
    if (visible() && Object.values(pending).some(Boolean)) enqueue(pending)
  })
}

export function startAccountEvents(): void {
  if (started) return
  started = true
  const activate = () => {
    if (!visible()) {
      generation++
      stop?.()
      stop = undefined
      setAccountEventsConnected(false)
      return
    }
    if (stop) return
    // 进页先取一次连接数：不依赖 SSE 首帧与 fallback 的 60 秒窗口，
    // 否则首屏在建立订阅前始终是空读数
    void syncConnections()
    stop = subscribePanelEvents({
      open: signal => requestDirect(PANEL_EVENTS_PATH, { method: 'GET', signal }),
      changed: enqueue,
      fallback: () => sync({ accounts: true, balances: true, usage: true, connections: true, proxies: true }),
      connection: status => { setAccountEventsConnected(status === 'connected') },
    })
  }
  // 弹窗关闭后立即恢复：门控期间 SSE 仍在线但 enqueue 不消费，关闭那一刻
  // 没有新事件，不主动补一次就会一直停在旧读数上
  const resume = (active: boolean) => {
    if (active || !visible()) return
    activate()
    enqueue({ accounts: false, balances: false, usage: false, connections: false, proxies: false })
  }
  window.addEventListener('wb:page', activate)
  document.addEventListener('visibilitychange', activate)
  onModalActiveChange(resume)
  activate()
}

export function accountListSignature(accounts: AccountRecord[]): string {
  return JSON.stringify(accounts.map(account => {
    const { detailsLoaded: _loaded, ...fields } = account
    return fields
  }))
}
