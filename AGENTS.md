# AGENTS.md

> **涉及分支、发版、版本号的操作，一律按 Skill `release-flow` 执行：**
> **`.agents/skills/release-flow/SKILL.md`** —— 动手前先读它，细则一律以该文件为准，本文件只作入口指路。
>
> 覆盖范围：切分支 / 给 PR 选 base / 合并 PR / 回复 Issue、PR / 改版本号 / 写更新日志 /
> 打 tag / 挂 GitHub Release / 发 beta 预发布版 / 发版验收 / 排查发版失败。
>
> **向上游整理 PR 的操作，一律按 Skill `upstream-pr` 执行：**
> **`.agents/skills/upstream-pr/SKILL.md`** —— PR 分支命名、base 选择、fork 专属剥离、候选核对等细则见该 Skill。

## 两条红线（先问人，不得自行推进）

1. **发版**：改版本号、推 `main` / `beta`、打 tag、挂 Release 之前，先把版本号与「这次要发什么」讲清楚，等用户确认；**未经确认不得推任何分支或 tag** —— tag 一旦推出去就会触发构建、产出公开的安装包与镜像，收不回来。
2. **更新日志**：写好的日志（= 发布提交信息 = Release 正文）必须先给用户逐条过目；按反馈改完**再确认一次**才能提交。

其余细则（分支落脚点、署名与 AI 落款、六处版本号、正式版 / beta 完整流程、验收清单、已知坑与排查）见上述 Skill。

## 仓库与分支模型（fork 三分支）

本仓库是 `aimod-cc/Agent2API` 的 fork，采用三分支模型；官方 Skill `release-flow` 第 1 节描述的「beta 集成、main 发版」是上游仓库口径，本 fork 以本节为准，发版红线与流程仍按该 Skill 执行。

- `main`：官方正式版镜像。只做同步与发版基线，不承载本地功能改动
- `beta`：官方 beta 集成镜像。只做同步，不承载本地功能改动
- `develop`：当前工作分支。承载全部本地改动（客户端归属规则、models.dev、图片兼容、Bun 运行时清理等）与权威文档；本地改进一律落在这里，再按「可 PR 上游清单」向上游整理
- 插件仓库 `Agent2Providers`（origin `https://github.com/mojie126/A2P`）同口径维护 `main` / `beta` / `develop` 三分支，其权威文档见该仓库 `docs/插件仓库基线与本地改动记录_2026-11-10.md`
- 同步分支重置步骤：`git reset --hard upstream/<分支>` → 全部工作流修剪为仅 `workflow_dispatch`（移除推送、拉取请求、tag、定时等一切自动触发）→ 单独提交 → 强推 origin；两个同步分支的重置互不影响
- 任何 GitHub Actions 工作流都不得有自动触发（推送、拉取请求、tag、定时、workflow_run 均不允许）；新建或合并工作流时同样遵守

## 权威文档维护

- `docs/baseline-and-local-changes_2026-11-10.md`：官方基线、本地改动全史、部署运行基线与「可 PR 上游清单」的唯一权威清单。每次同步官方、跟进修复、发版验证后滚动更新；历史分日期文档已并入本文并删除，不再新增分日期记录
- 「可 PR 上游清单」是整理上游 PR 的唯一依据：先核对条目未被官方覆盖、未被其它 PR 承接，再动手整理
- 插件仓库对应文档：`docs/插件仓库基线与本地改动记录_2026-11-10.md`（在 Agent2Providers 仓库），维护口径相同

## 提交与本地测试注意事项

- 本地测试网关改动：先跑 `scripts/build-desktop.ps1` 构建 Windows 桌面端（串联 i18n 词典、页面岛、手机端 bundle、Rust 四步，按产物时间增量跳过），产物为 `target\release\agent2api.exe`（桌面壳）与 `target\release\agent2api-server.exe`（headless 网关）
- 插件仓库（Agent2Providers）有更新：先构建 Windows 插件包——`cargo build --release -p agent2api-providers` 后 `node scripts/pack.mjs --target x86_64-pc-windows-msvc --skip-build` 产出 `dist/*.zip`，再更新到本地网关的插件目录 `%USERPROFILE%\.agent2api\plugins\agent2api-providers`（保持 `pack.json`、`bin/`、`ui/`、`licenses/` 目录结构，`plugins.json` 为登记表），或经网关面板「提供商插件 → 安装插件包」安装 zip
- 推送 develop 触发 `performance-build` 构建时，注意发布步骤的校验包含「协议库 tag 与网关版本一致」——升级网关版本号时必须同步升级协议库 tag（官方已发布 tag 见 Agent2Providers 的 tags 页）

## 草稿发布维护

- `performance-build`（A2API develop）与 `dev-build`（插件仓库 develop）产出的草稿 Release 是 fork 自有的测试资产，与官方发版流程无关，资产草稿可随时删除重建，不影响 develop 源码、构建缓存与运行中的部署
- 草稿描述必须维护：按「新增：」「修复：」「优化：」分类列出本轮构建包含的改动，条目与提交信息一一对应，不写与产物无关的内容
- 同一个大版本的多次构建：描述要整合或追加——同 tag 覆盖式重建时把历次条目合并为一份完整清单，不丢失先前条目；已发布同 tag 加 `-r<序号>` 后缀的重复构建，在描述里注明与上一版的差异
- GHCR 镜像推送偶发 `permission_denied: write_package` 拒绝：重跑失败步骤即可恢复（runner 侧瞬时网络或令牌问题，2026-11-10 复现一次、重跑恢复），不是额度或包授权问题
- 增量构建口径：两个构建工作流已配置 rust-cache（Cargo 注册表与 target 目录）、npm 缓存（`desktop-tauri/package-lock.json`）与 Docker 的 gha 层缓存，依赖与产物按缓存键增量复用；`Cargo.lock`、锁文件或 Dockerfile 变更引发相应重拉重编属预期行为，不改缓存配置

## 反代提供商问题排查

收到反代提供商相关的问题反馈时，**优先使用网络搜索查找已有解决方案**，重点参考同类项目的 Issue、PR、修复记录和适配实现，了解其他项目是否遇到过相同问题、如何解决。**不要默认从零自行排查**：先核对已有方案与本项目的适用性，优先借鉴可靠的解决思路；未找到可靠方案、现有方案不适用或仍有未解决的问题时，再开展针对性的本地排查。

## 文档落点

安装 / 部署 / 升级 / 排错说明只在 `docs/install.md`（另有 5 份译文 `docs/install.{en,zh-Hant,ja,ko,pt-BR}.md`，六份一一对应）里维护；各语言 README 只留「快速开始」速查表 + 一个指向安装文档的链接，**不要再把安装细节写回 README**。产物名、端口、环境变量、安装脚本参数有改动时，六份一起改。

**README 语言落点：根 `README.md` 是英文**（GitHub 默认展示），简体中文在 `docs/README.zh-CN.md`，其余四份为 `docs/README.{zh-Hant,ja,ko,pt-BR}.md`。中文仍是文案源语言，改支持矩阵等内容时六份同步，别把中文写回根目录。
