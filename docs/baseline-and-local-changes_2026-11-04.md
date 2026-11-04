# 官方基线与本地改动记录

本文记录个人 fork 的官方基线、本地分支规则、当前改动范围与后续同步方式，便于后续继续跟进官方更新和本地性能修复。

## 仓库与分支

- 个人 fork：`https://github.com/mojie126/A2API`
- 官方仓库：`https://github.com/aimod-cc/Agent2API`
- 官方基线分支：`main`
- 本地开发分支：`performance`

`main` 只用于同步官方更新，不直接承载本地功能改动。`performance` 以 `main` 为基线承载性能修复和构建流程调整。

## 官方基线

- 官方版本：`v3.4.1`
- 官方提交：`3ff4b82`
- 官方提交日期：`2026-11-04`
- 官方更新重点：工具服务页、AI 助手文本附件与账号导入、Tabbit、Kiro、AStudio、管理面板压缩、日志与请求统计批量维护、提供商适配修复

官方 `v3.4.1` 已经包含日志与请求统计的部分性能优化：日志每 64 条批量维护，请求统计每 128 次记账或每 60 秒维护，日志查询与统计缓存，请求列表三个计数合并为一次扫描。

## 本地性能改动

- 调试报文最多保留 100 条，请求日志上限低于 100 时跟随请求日志上限
- 调试报文正文合计最多 128 MiB，单条请求体或响应体最多 4 MiB
- 下游请求正文合计最多 64 MiB，超限时淘汰最旧记录
- 调试关闭时清理历史快照，不再写最近请求文件
- 大请求及时释放原始字节，避免请求解析与转发期间长期保留多份正文
- 图片逐张压缩，避免一次性复制全部图片地址
- 请求终态与限额事件在同一个事务内提交
- 在途请求状态最多每秒写入一次，减少 SQLite 写事务
- 日志与请求统计保留期和容量维护按时间或条数批量执行
- 请求总数使用内存缓存，无筛选时不再重复全表计数
- SQLite 检查点阈值提高到 10000 页，新库启用增量回收
- 调试关闭时不再输出详细日志
- 同类余额查询失败日志在 15 分钟内去重
- 更新检查复用成功结果，减少重复网络请求
- 路由页默认恢复 beta 版模型轨道与提供商画布，同时保留最新作用范围、策略、粘滞、草稿保存和请求回放能力
- 路由整组停用统一保留在提供商列表中并写入停用列表，兼容全部自动参与与仅勾选参与两种作用范围

主要改动位置：

- `desktop-tauri/src-tauri/server/src/server/core/debug_traffic.rs`
- `desktop-tauri/src-tauri/server/src/server/core/debug_traffic/sql.rs`
- `desktop-tauri/src-tauri/server/src/server/api/pipeline.rs`
- `desktop-tauri/src-tauri/server/src/server/api/pipeline/raw.rs`
- `desktop-tauri/src-tauri/server/src/server/api/chat.rs`
- `desktop-tauri/src-tauri/server/src/server/api/protocol.rs`
- `desktop-tauri/src-tauri/server/src/server/logs_store/mod.rs`
- `desktop-tauri/src-tauri/server/src/server/request_stats.rs`
- `desktop-tauri/src-tauri/server/src/server/request_stats/sql/raw.rs`
- `desktop-tauri/src-tauri/server/src/server/db/mod.rs`
- `desktop-tauri/src-tauri/server/src/server/core/upstream/image_compress.rs`
- `desktop-tauri/ui-islands/src/islands/routing-legacy-workbench.tsx`
- `desktop-tauri/ui-islands/src/islands/routing-page.tsx`
- `desktop-tauri/ui/css/page-routing-legacy.css`

## 工作流调整

- `build.yml` 只保留手动派发，不响应版本标签推送
- `frontend-artifacts.yml` 只保留手动校验，不响应分支推送和拉取请求
- `warm-cache.yml` 只保留手动预热，不响应主分支推送和定时任务
- `fork-build.yml` 只保留手动构建，生成 Windows 桌面端、Windows 服务端和 `linux/amd64` 镜像

## 构建流程

个人 fork 使用 `.github/workflows/fork-build.yml` 手动构建：

- Windows 桌面端安装包，产物名 `windows-nsis`
- Windows 服务端压缩包与校验文件，产物名 `windows-server-amd64`
- `ghcr.io/mojie126/a2api:latest`，仅支持 `linux/amd64`

仓库内所有 GitHub Actions 工作流只允许手动触发，不响应分支推送、拉取请求、标签推送或定时任务。

详细操作见 `docs/fork-build_2026-11-04.md`。

## 验证记录

- 服务端编译检查：通过
- 服务端测试：`283 passed; 0 failed; 1 filtered out`
- 前端岛类型检查：通过
- 前端主面板与登录页构建：通过
- GitHub Actions 工作流 YAML：解析通过
- 官方基线剩余警告：云同步原子操作弃用警告、登录任务收尾函数未使用警告、旧模型别名函数未使用警告

## 后续同步官方更新

1. 在 `main` 执行 `git fetch origin --prune` 并快进到官方最新版本
2. 切回 `performance`，将更新后的 `main` 合并到本地开发分支
3. 重点检查 `logs_store`、`request_stats`、`debug_traffic`、`db` 与 `.github/workflows`
4. 重新运行服务端编译和测试
5. 手动运行 `performance-build`，检查 Windows 产物与 `linux/amd64` 镜像
6. 更新本文的官方版本、提交和验证记录
