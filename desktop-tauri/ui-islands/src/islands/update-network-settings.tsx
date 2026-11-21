import * as React from 'react'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue, cn } from '@ui'
import {
  buildProxyPick, EMPTY_PROXY_SELECTION, fetchProxySelection, saveProxy, type ProxySelection,
} from './update-shared'
import { t } from '../i18n'
import { WARP_VALUE_PREFIX } from './accounts-shared'
import {
  ensureWarpExits, refreshWarpExits, subscribeWarpExits, warpExitsError, warpExitsReady, warpExitsSnapshot,
} from './accounts-warp-cache'

/**
 * 「更新设置 → 出网代理」分页：指定软件更新与下载安装走哪条线路
 *
 * 选项由 `buildProxyPick` 拼装，来源三份：当前线路、代理池、WARP 设备
 * WARP 设备清单与账号页共用 `accounts-warp-cache` 的缓存（同一网关接口、
 * 同一节流补拉口径），这里的订阅只负责在清单变化时重绘下拉
 */
export function UpdateNetworkSettings({ plugin = false }: { plugin?: boolean }) {
  const [selection, setSelection] = React.useState<ProxySelection>(EMPTY_PROXY_SELECTION)
  const [busy, setBusy] = React.useState(false)
  const [warpTick, setWarpTick] = React.useState(0)

  React.useEffect(() => {
    let alive = true
    void fetchProxySelection().then(value => { if (alive) setSelection(value) })
    return () => { alive = false }
  }, [])

  React.useEffect(() => subscribeWarpExits(() => setWarpTick(tick => tick + 1)), [])
  React.useEffect(() => { ensureWarpExits() }, [])

  const pick = React.useMemo(() => buildProxyPick(selection, {
    items: warpExitsSnapshot(),
    ready: warpExitsReady(),
    error: warpExitsError() || '',
  }), [selection, warpTick])

  const choose = React.useCallback(async (value: string) => {
    if (busy || !pick.items.some(item => item.value === value && !item.disabled)) return
    setBusy(true)
    try {
      const result = await saveProxy(value)
      if (result) setSelection(previous => ({ ...previous, proxyChoice: result.choice }))
    } finally {
      // 线路切换可能让 WARP 清单的引用状态变化，成功切换后强制补读一次
      if (value.startsWith(WARP_VALUE_PREFIX)) void refreshWarpExits()
      setBusy(false)
    }
  }, [busy, pick.items])

  return <div>
    <div className='settings-dlg-pane-head'>
      <h3>{t('出网代理')}</h3>
      {plugin ? <p>{t('与软件更新共用出网代理，修改任意一处都会同步生效。')}</p> : null}
    </div>
    <div className='settings-dlg-fields'>
      <Select value={pick.current} disabled={busy} onValueChange={value => void choose(String(value))}>
        <SelectTrigger className={cn('w-full', pick.broken && 'border-destructive-bd')}
          title={pick.title} aria-label={t('更新出网代理')}>
          <SelectValue className='min-w-0 truncate'>{pick.selected?.label || t('直连')}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          {pick.items.map(item => <SelectItem key={item.value} value={item.value} disabled={item.disabled}>
            {item.label}
          </SelectItem>)}
        </SelectContent>
      </Select>
      <p className='mt-1.5 text-[12px] leading-[1.7] text-subtle'>
        {t('可选直连、代理池条目与 WARP 设备出口；WARP 设备随服务重启重建，重启后按同一出口自动接回')}
      </p>
    </div>
  </div>
}