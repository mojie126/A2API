/**
 * 「网络代理」页的 WARP 设备分页。
 *
 * 出口池由网关在后台自动注册设备并建立用户态隧道，每台设备对应一个本地
 * SOCKS5 端口；本分页只做清单展示与增删。设备不落盘，重启后按期望台数
 * 重新注册，所以这里看到的是本次运行期间的实际设备。
 */

import * as React from 'react'
import {
  Badge, Button, Card, CardHeader, StatusDot, Table, TableBody, TableCell, TableHead,
  TableHeader, TableRow, cn,
} from '@ui'
import { t } from '../i18n'
import { errorMessage, formatTime, toast, wb, type WarpExit, type WarpPayload } from './proxies-state'

/** 使用情况（在途连接、请求数）变化快，静默轮询的间隔 */
const REFRESH_MS = 3000

export function WarpPanel() {
  const [data, setData] = React.useState<WarpPayload | null>(null)
  const [error, setError] = React.useState('')
  const [refreshing, setRefreshing] = React.useState(false)
  /** 添加 / 删除在途：注册设备是串行 HTTP 调用，期间禁掉重复操作 */
  const [busy, setBusy] = React.useState(false)

  const load = React.useCallback(async (silent = false) => {
    const bridge = wb().workbuddyDesktop
    if (typeof bridge?.getWarpExits !== 'function') {
      setError(t('当前环境不支持 WARP 出口池（桥接方法缺失）'))
      return
    }
    if (!silent) setRefreshing(true)
    try {
      const payload = await bridge.getWarpExits()
      if (!payload || !Array.isArray(payload.exits)) throw new Error(t('WARP 设备清单响应异常'))
      setData(payload)
      setError('')
    } catch (reason) {
      // 静默轮询失败不覆盖已有清单与错误：池初始化期间偶发失败不该闪成空白
      if (!silent) setError(errorMessage(reason))
    } finally {
      if (!silent) setRefreshing(false)
    }
  }, [])

  React.useEffect(() => {
    void load()
    const timer = window.setInterval(() => void load(true), REFRESH_MS)
    return () => window.clearInterval(timer)
  }, [load])

  async function addExit(): Promise<void> {
    const bridge = wb().workbuddyDesktop
    if (typeof bridge?.addWarpExit !== 'function' || busy) return
    setBusy(true)
    // 注册、建隧道、探测出口 IP 串在一起，通常要十几秒
    toast(t('正在注册新的 WARP 设备，可能需要几十秒…'))
    try {
      const payload = await bridge.addWarpExit()
      if (!payload || !Array.isArray(payload.exits)) throw new Error(t('WARP 设备清单响应异常'))
      setData(payload)
      setError('')
      toast(t('✅ 已添加 WARP 设备'), 'ok')
    } catch (reason) {
      toast(t('添加 WARP 设备失败：{reason}', { reason: errorMessage(reason) }), 'err')
    } finally {
      setBusy(false)
    }
  }

  async function removeExit(exit: WarpExit): Promise<void> {
    const bridge = wb().workbuddyDesktop
    if (typeof bridge?.removeWarpExit !== 'function' || busy) return
    const ok = await wb().wbConfirm?.ask?.({
      title: t('删除 WARP 设备'),
      html: t('确定删除出口「{label}」？删除会注销 Cloudflare 上的这台设备，正在使用它的请求会中断。', {
        label: exit.label,
      }),
      okText: t('删除'),
      okClass: 'danger',
    })
    if (!ok) return
    setBusy(true)
    try {
      const payload = await bridge.removeWarpExit(exit.id)
      if (!payload || !Array.isArray(payload.exits)) throw new Error(t('WARP 设备清单响应异常'))
      setData(payload)
      setError('')
      toast(t('已删除 WARP 设备「{label}」', { label: exit.label }))
    } catch (reason) {
      toast(t('删除 WARP 设备失败：{reason}', { reason: errorMessage(reason) }), 'err')
    } finally {
      setBusy(false)
    }
  }

  const rows = data?.exits ?? []
  const ready = data?.ready !== false
  const target = data?.targetSize ?? 0
  /* 清单还没读到时也禁掉添加：此刻不知道池是否就绪，点下去只会换来一次 409 */
  const maxSize = data?.maxSize ?? 32
  const canAdd = data !== null && ready && rows.length < maxSize

  return <Card render={<section />} className='panel'>
    <CardHeader className='panel-head'>
      <h2>{t('WARP 设备')}</h2><Badge variant='secondary' shape='tag'>{rows.length}</Badge>
      <span className='panel-sub'>
        {!ready
          ? t('出口池准备中…')
          : target !== rows.length
            ? t('期望 {target} 台，当前 {n} 台', { target, n: rows.length })
            : t('出口池已就绪')}
      </span>
      <div className='head-actions'>
        <Button disabled={busy || !canAdd} onClick={() => void addExit()}>
          {busy ? t('处理中…') : t('＋ 添加设备')}
        </Button>
        <Button variant='ghost' disabled={refreshing || busy} onClick={() => void load()}>
          {refreshing ? t('刷新中…') : t('刷新')}
        </Button>
      </div>
    </CardHeader>
    {error ? <div className='text-[12px] text-destructive px-4 py-2'>{t('读取失败：{reason}', { reason: error })}</div> : null}
    <div className='models-table-wrap'>
      <Table container={false} className='models-table proxies-table'>
        <TableHeader><TableRow>
          <TableHead>{t('设备')}</TableHead>
          <TableHead>{t('出口')}</TableHead>
          <TableHead>{t('使用情况')}</TableHead>
          <TableHead>{t('流量')}</TableHead>
          <TableHead>{t('最近使用')}</TableHead>
          <TableHead className='ta-right'>{t('操作')}</TableHead>
        </TableRow></TableHeader>
        <TableBody>{rows.length ? rows.map(exit => <TableRow key={exit.id}>
          <TableCell>
            <div className='flex min-w-0 flex-col gap-0.5'>
              <span className='flex items-center gap-1.5 text-[12.5px] font-medium'>
                <StatusDot variant='success' />
                <span className='truncate'>{exit.label}</span>
              </span>
              <span className='flex min-w-0 items-center gap-1.5 text-[10.5px] text-muted-foreground'>
                <span className='shrink-0 font-mono' title={exit.id}>{shortId(exit.id)}</span>
                <span className='opacity-45' aria-hidden='true'>·</span>
                <span className='truncate'>{t('注册于 {time}', { time: formatTime(exit.createdAt) })}</span>
              </span>
            </div>
          </TableCell>
          <TableCell>
            <div className='flex min-w-0 flex-col gap-0.5'>
              <span className='truncate font-mono text-xs text-subtle' title={exit.egressIp || t('尚未探测到出口 IP')}>
                {exit.egressIp || t('尚未探测')}
              </span>
              <span className='truncate font-mono text-[10.5px] text-muted-foreground' title={exit.endpoint}>{exit.endpoint}</span>
            </div>
          </TableCell>
          <TableCell>
            <div className='flex min-w-0 flex-col gap-0.5 text-[11.5px]'>
              <span className='truncate'>{t('请求 {n} 次', { n: exit.assigned })}</span>
              <span className='flex items-center gap-1.5 text-[10.5px] text-muted-foreground'>
                <span>{t('在途 {n} 条', { n: exit.active })}</span>
                <span className='opacity-45' aria-hidden='true'>·</span>
                <span>{t('累计连接 {n} 条', { n: exit.connections })}</span>
              </span>
            </div>
          </TableCell>
          <TableCell>
            <div className='flex min-w-0 flex-col gap-0.5 font-mono text-[11px] text-muted-foreground'>
              <span className='truncate' title={t('客户端到上游')}>{t('↑ {value}', { value: bytesText(exit.uploadBytes) })}</span>
              <span className='truncate' title={t('上游到客户端')}>{t('↓ {value}', { value: bytesText(exit.downloadBytes) })}</span>
            </div>
          </TableCell>
          <TableCell>
            <span className='text-[11.5px] text-muted-foreground'>
              {exit.lastUsedAt ? formatTime(exit.lastUsedAt) : t('尚未使用')}
            </span>
          </TableCell>
          <TableCell className='ta-right'>
            <div className='row-actions'>
              <Button size='icon-xs' variant='ghost' disabled={busy} title={t('删除')} aria-label={t('删除')}
                className='text-muted-foreground hover:text-destructive'
                onClick={() => void removeExit(exit)}><TrashIcon /></Button>
            </div>
          </TableCell>
        </TableRow>) : <TableRow><TableCell colSpan={6} className={cn('empty', 'text-center py-8')}>
          {!data
            ? error || t('加载中…')
            : !ready
              ? <div className='flex flex-col items-center gap-2'>
                <b className='text-sm text-foreground'>{t('出口池正在准备')}</b>
                <p className='text-xs text-subtle'>{t('网关正在注册 WARP 设备并建立隧道，通常需要几十秒，完成后这里会列出设备。')}</p>
              </div>
              : <div className='flex flex-col items-center gap-2'>
                <b className='text-sm text-foreground'>{t('还没有 WARP 设备')}</b>
                <p className='text-xs text-subtle'>{t('添加设备后，使用 WARP 出口池的 ZCode 请求会从这些出口轮流发出。')}</p>
                <Button disabled={busy} onClick={() => void addExit()}>{t('添加第一台设备')}</Button>
              </div>}
        </TableCell></TableRow>}</TableBody>
      </Table>
    </div>
  </Card>
}

/** 设备标识只展示前 8 位，完整值放在 title 里 */
function shortId(id: string): string {
  return id.length > 8 ? `${id.slice(0, 8)}…` : id
}

/** 字节数按 1024 进制取一位小数（小于 10 时）；0 显示 0 B */
function bytesText(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let size = value
  let unit = 0
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024
    unit += 1
  }
  return `${unit > 0 && size < 10 ? size.toFixed(1) : Math.round(size)} ${units[unit]}`
}

function TrashIcon() {
  return <svg viewBox='0 0 24 24' className='size-3.5' fill='none' stroke='currentColor' strokeWidth='1.5' strokeLinecap='round' strokeLinejoin='round' aria-hidden='true'>
    <path d='M5 7h14' />
    <path d='M9 7V5.5A1.5 1.5 0 0 1 10.5 4h3A1.5 1.5 0 0 1 15 5.5V7' />
    <path d='M7 7l.8 11a1.5 1.5 0 0 0 1.5 1.4h5.4A1.5 1.5 0 0 0 16.2 18L17 7' />
  </svg>
}