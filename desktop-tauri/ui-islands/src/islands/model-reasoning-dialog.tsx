/**
 * Agent2API · 「思考档位」弹窗：手动维护一条模型认识的档位表。
 *
 * ── 解决什么 ────────────────────────────────────────────────
 * 「这个模型认识哪几档」此前只有两个来源：上游目录与 models.dev。两者都可能错
 * （远程目录撒谎、静态表跟不上上游调整），自定义家更是整个键都没有；用户此前
 * 能改的只有「某条映射用哪一档」，改不了「这家到底认哪几档」。这个弹窗就是那层
 * 纠正的入口：手动写下的表**优先于上游声明与 models.dev**，同时决定
 * `/v1/models` 对下游声明的档位、映射弹窗里可选的等级与管理页的档位展示
 * （应用点见后端 `core::model_meta::fill` 的 `apply_user_reasoning_levels`）。
 *
 * ── 与「模型能力」弹窗的关系 ────────────────────────────────
 * 覆盖存在**同一张覆盖表**里（内置家 `modelRules.capabilities`、自定义家
 * `models[].capabilities`），但这一项换的是整份声明、值又是数组，所以单独一个
 * 弹窗、单独一次写入：「支持思考」那行三态只回答「下游当它支持思考吗」，
 * 这里回答「支持哪几档」，两者互不推导。
 *
 * ── 存的是覆盖还是继承 ──────────────────────────────────────
 * 删光所有行再保存 = 清除这条覆盖（回到上游与 models.dev 的结论），与后端
 * 「空表 = 没有覆盖」同一口径；「恢复继承」按钮是同一件事的显式入口。
 *
 * ── 只改出口声明，不改注入能力 ──────────────────────────────
 * 「这条通道收不收思考档位字段」由 `reasoningBindingSupported` 回答，属于上游
 * 事实，不跟着这张表走：手动填了档位而对下游声明之外仍注入不了档位，是一种如实
 * 存在的组合（文案里说清楚，免得用户以为改了没生效）。
 */

import * as React from 'react'
import {
  Button, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader, DialogTitle, Input, Label,
} from '@ui'

import {
  MAX_REASONING_LEVELS, MAX_REASONING_LEVEL_CHARS, REASONING_LEVELS_KEY, normalizeReasoningLevels,
} from './model-capability'
import { levels as candidateLevels, modelReasoning } from './models-reasoning'
import {
  accept, errorMessage, getSnapshot, modelRowOf, shared, toast, writeCapabilities,
} from './models-panel-state'
import { t } from '../i18n'

/**
 * 思考档位弹窗。上下文只有 `(provider, id)`，行数据每次渲染从快照现读 —— 与
 * 能力弹窗同一口径（目录刷新 / 别的写操作改了数据，这里的当前值跟着走）。
 */
export function ModelReasoningDialog({ provider, id, onClose }: {
  provider: string
  id: string
  onClose: () => void
}) {
  const row = modelRowOf(provider, id)
  const info = modelReasoning(row)
  const [levels, setLevels] = React.useState<string[]>(() => normalizeReasoningLevels(row?.reasoningLevels))
  const [status, setStatus] = React.useState('')
  const [saving, setSaving] = React.useState(false)
  const providerLabel = row?.providerLabel || shared().wbProviders?.labelOf?.(provider) || provider
  /** 当前这份表是用户手写的（后端记 `reasoningSource = "user"`，见 `model_meta`） */
  const overridden = info.source === 'user'
  /**
   * 常用档位：通用候选表（后端下发，缺了走本地兜底），已在本表里的不再列 ——
   * 点一下就是「加这一档」，省去手打。
   *
   * `off` / `none`（关闭思考）**不列**：本项目对这两档一律不注入（会让部分上游
   * 直接拒单或行为异常，见后端 `model_rules::reasoning` 的模块头），把它们写进
   * 「这个模型认识哪几档」的声明里只会让下游点一个注定落空的档位。
   */
  const candidates = candidateLevels(getSnapshot().data)
    .filter(level => !['off', 'none'].includes(level.toLowerCase()))
    .filter(level => !levels.some(existing => existing.toLowerCase() === level.toLowerCase()))
  const inherited = info.fixedLevel ? [info.fixedLevel] : info.levels
  const inheritedLabel = info.fixedLevel
    ? t('上游固定档位')
    : info.source === 'modelsdev'
      ? t('models.dev 参考档位')
      : info.declared
        ? t('上游声明档位')
        : t('没有上游声明')

  function setLevel(index: number, value: string): void {
    setLevels(prev => prev.map((item, position) => (position === index ? value : item)))
  }

  function removeLevel(index: number): void {
    setLevels(prev => prev.filter((_item, position) => position !== index))
  }

  function addLevel(level = ''): void {
    setLevels(prev => (prev.length >= MAX_REASONING_LEVELS ? prev : [...prev, level]))
  }

  /**
   * 收集要落库的档位表：空行跳过（删到一半的行不该拦住保存），重复与超长拦下。
   * 归一与后端同一口径（`capability::normalize_levels` 收非空字符串、忽略大小写
   * 去重），写进去的是**用户写的拼写**。
   */
  function collect(): { levels: string[]; error: string } {
    const result: string[] = []
    for (const item of levels) {
      const level = item.trim()
      if (!level) continue
      if (level.length > MAX_REASONING_LEVEL_CHARS) {
        return {
          levels: [],
          error: t('单条档位最多 {n} 个字符：{level}', { n: MAX_REASONING_LEVEL_CHARS, level }),
        }
      }
      if (result.some(existing => existing.toLowerCase() === level.toLowerCase())) {
        return { levels: [], error: t('档位不能重复：{level}', { level }) }
      }
      result.push(level)
    }
    if (result.length > MAX_REASONING_LEVELS) {
      return { levels: [], error: t('最多 {n} 个档位', { n: MAX_REASONING_LEVELS }) }
    }
    return { levels: result, error: '' }
  }

  /** `clear` = 恢复继承：与「删光所有行再保存」是同一件事，走同一个空表 */
  async function save(clear = false): Promise<void> {
    if (saving) return
    const collected = collect()
    if (!clear && collected.error) {
      setStatus(collected.error)
      return
    }
    const values = clear ? [] : collected.levels
    setSaving(true)
    setStatus(t('保存中…'))
    try {
      // 空表给 null（= 清除覆盖）：后端把空数组也当清除，但显式给 null 更直白
      accept(await writeCapabilities(provider, id, {
        [REASONING_LEVELS_KEY]: values.length ? values : null,
      }))
      setSaving(false)
      onClose()
      toast(values.length
        ? t('✅ 已保存 {id} 的思考档位：{levels}', { id, levels: values.join(' / ') })
        : t('✅ 已恢复 {id} 的思考档位为上游声明', { id }))
    } catch (error) {
      setStatus(t('保存失败：{message}', { message: errorMessage(error) }))
      setSaving(false)
    }
  }

  return (
    <Dialog open onOpenChange={(next, eventDetails) => {
      if (next) return
      // 保存中不许关：关掉会让「到底存没存进去」变成未知状态
      if (saving) { eventDetails.cancel(); return }
      onClose()
    }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('思考档位')}</DialogTitle>
        </DialogHeader>
        <DialogBody>
          {/* 预览行：改的是哪一条模型（与能力弹窗、映射弹窗的预览区同一用意） */}
          <div className='rounded-md border border-border bg-surface-inset px-3 py-2.5 font-mono text-[12px] text-subtle'>
            <b className='text-primary-fg'>{id}</b>{t('（{provider}）', { provider: providerLabel })}
          </div>

          <div className='flex items-center justify-between gap-3'>
            <Label>{t('这个模型认识的档位')}</Label>
            <span className='text-xs text-subtle'>
              {t('{n} / {max} 档', { n: levels.length, max: MAX_REASONING_LEVELS })}
            </span>
          </div>

          <div className='flex flex-col gap-2'>
            {levels.length ? levels.map((level, index) => (
              <div key={index} className='flex items-center gap-2'>
                <Input value={level} maxLength={MAX_REASONING_LEVEL_CHARS} disabled={saving}
                  autoComplete='off' spellCheck={false}
                  placeholder={t('档位名，例如 low / high / max')}
                  onChange={event => setLevel(index, event.currentTarget.value)}
                  onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); void save() } }} />
                <Button variant='ghost' size='sm' disabled={saving}
                  title={t('删除这一档')} onClick={() => removeLevel(index)}>{t('删除')}</Button>
              </div>
            )) : (
              <p className='text-xs leading-[1.65] text-subtle'>
                {t('这一项还没有手动设置。加一档或多档，保存后即以本表为准。')}
              </p>
            )}
          </div>

          <div className='flex flex-wrap items-center gap-2'>
            <Button variant='outline' size='sm' disabled={saving || levels.length >= MAX_REASONING_LEVELS}
              onClick={() => addLevel()}>{t('＋ 新增档位')}</Button>
            {candidates.map(level => (
              <Button key={level} variant='secondary' size='2xs' disabled={saving}
                title={t('加一档：{level}', { level })} onClick={() => addLevel(level)}>{level}</Button>
            ))}
          </div>

          <div className='flex flex-col gap-1 rounded-md border border-border bg-surface-inset px-3 py-2.5 text-xs leading-[1.7] text-subtle'>
            <div>
              {t('当前生效：')}
              <span className='text-primary-fg'>{inherited.length ? inherited.join(' / ') : t('未声明')}</span>
              {t('（{source}）', { source: inheritedLabel })}
            </div>
            <div>
              {overridden
                ? t('当前已由手动设置覆盖上游与 models.dev；删光全部档位再保存即恢复继承。')
                : t('手动设置的档位优先于上游声明与 models.dev，也作为映射弹窗里可选的等级。')}
            </div>
            {info.bindingSupported === false ? (
              <div>{t('这条通道声明不接收思考档位字段，映射等级不会注入；本表只影响对下游的声明。')}</div>
            ) : null}
          </div>

          {/* 状态行：高度固定，出现错误时弹窗不跳高 */}
          <div className='min-h-[18px] text-xs text-subtle' role='status'>{status}</div>
        </DialogBody>
        <DialogFooter>
          <div className='mr-auto'>
            {overridden ? (
              <Button variant='ghost' size='sm' disabled={saving}
                onClick={() => void save(true)}>{t('恢复继承')}</Button>
            ) : null}
          </div>
          <Button variant='outline' disabled={saving} onClick={onClose}>{t('取消')}</Button>
          <Button variant='default' disabled={saving} onClick={() => void save()}>{t('保存')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
