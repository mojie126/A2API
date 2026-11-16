# ZCode 免费套餐并发限制与 WARP 出口池方案（2026-11-17）

## 一、结论

ZCode 免费套餐（套餐池通道）的并发 429 不是单账号额度耗尽，而是上游按用户与出口维度判定的并发限制。实测结果表明：

- 同一出口 IP 下，一个请求在途时，另一个请求即使换用其他 zcode 账号仍返回 429
- 换到不同出口 IP 后，两个请求可以同时成功

因此可以参考 opencode2api 的出口池做法：为 ZCode 请求准备多个出口，429 时切换出口重发，对客户端保持无感。

## 二、实测记录（2026-11-17）

### 2.1 上游原始响应

```text
POST https://zcode.z.ai/api/v1/zcode-plan/anthropic/v1/messages
HTTP 429

{"code":3008,"msg":"user concurrency limit exceeded","logid":"..."}
```

响应头没有 `Retry-After`，也没有 `x-ratelimit-*` 字段。

### 2.2 对照实验

| 场景 | 结果 |
| --- | --- |
| A（家宽出口）在途，B 使用同一出口的另一个 zcode 账号 | B 返回 429，`upstream_code=3008` |
| A（家宽出口）在途，B 使用新加坡出口 | B 返回 200，1.35 秒完成 |
| 两者都使用家宽出口，临时 Key 仅允许 zcode / zcode-intl | 依次尝试多个账号全部 429，最终返回网关合成的 429 |

### 2.3 网关当前行为

- `Agent2Providers` 的 `zcode/errors.rs` 把 429 与业务码 1005 归类为 `QuotaLimited`
- 网关收到 `QuotaLimited` 后标记该账号对该模型冷却，再顺延下一个账号
- 换账号不能绕开该限制，结果是所有候选账号被依次标记冷却，最终整条请求链返回 429

## 三、opencode2api 的做法

opencode2api 是 OpenCode Zen / Zen Go 的网关，核心机制：

- 支持手动 HTTP / HTTPS / SOCKS5 代理池与用户态 Cloudflare WARP 出口池
- 免费模型请求按在途请求分配出口，出口第一次收到 429 或 403 即淘汰并换下一个
- IPv6 出口按 `/64` 去重并冷却 30 分钟，IPv4 出口按 `/32`
- 代码注释明确说明 OpenCode Zen 的免费层按 IP 限流，换代理就是故障转移手段
- README 说明上游免费额度按出口网段计算，只有换到不同网段才有意义

其可借鉴的部分是“出口池 + 按连接或请求分配出口 + 429 淘汰出口”，不要求 ZCode 使用同一套 WARP 实现。

## 四、目标

1. 客户端无感：调用方仍然只配置一个模型名与一个 Key，不感知出口切换
2. ZCode 账号配置一次轮询档后，网关自动在多个出口之间分配请求
3. 遇到 `3008` 时先原地换出口重发，不立即把账号标记为额度冷却
4. 所有出口都 429 时保留现有兜底：按账号限额冷却并顺延下一个账号

## 五、总体架构

```text
客户端
  └─ A2API 网关
       ├─ 账号轮询档（source=rotate）
       │    └─ 代理池：多个 WARP SOCKS5 出口
       ├─ 429 换出口重试（同一账号，换下一个出口）
       └─ WARP 出口池 sidecar（用户态，多出口）
            └─ 每个出口一个 SOCKS5 端口，或单端口按连接轮换
```

职责划分：

- WARP sidecar 只负责提供多个可用出口，不参与账号与模型路由
- A2API 负责出口池选择、429 重试、请求日志与冷却兜底
- 账号配置只声明“使用轮询档”，不感知具体出口数量

## 六、实施步骤

### 6.1 网关侧：内置提供商支持 429 换出口

现状：`attempt_custom` 已实现轮询档 429 换出口，但 ZCode 是内置插件，走 `attempt_queue` 的无状态路径，没有这段补救。

落点：

- 文件：`desktop-tauri/src-tauri/server/src/server/core/upstream/provider_loop.rs`
- 函数：`attempt_queue`
- 复用：
  - `core::proxy_rotate::next` 或 `AccountStore::next_rotate_proxy`
  - `provider_loop::MAX_ROTATE_EXIT_RETRIES`
  - `telemetry.note_attempt_retry`

逻辑：

1. 发送前判断当前账号是否为轮询档（代理配置 `source=rotate`）
2. `send_with_retry` 返回 `QuotaLimited { status: 429 }` 时：
   - 若为轮询档且剩余换出口次数大于 0，取下一个出口
   - 更新本轮 `proxy`，重建 `TransportRequest`
   - 在同一账号上原地重发，不标记账号冷却，不消耗换号预算
   - 记录一次内部重试，请求日志显示“轮询换出口”
3. 换满 `MAX_ROTATE_EXIT_RETRIES` 仍为 429 时，交回原有逻辑：标记账号冷却并顺延
4. 非轮询档账号与非 429 错误保持现状，避免影响其他提供商

验收：

- 轮询档 + 429：请求日志出现“轮询换出口”明细，最终成功或按原逻辑失败
- 静态代理、直连、非 zcode 提供商：行为与改动前一致

### 6.2 WARP 出口池 sidecar

推荐形态：独立进程或独立容器，用户态 WARP，不依赖 TUN、root 或宿主机路由表。

实现选择：

- 复用 opencode2api 的 `internal/warp` 池实现，构建一个只提供代理入口的 sidecar
- 或使用现成的用户态 WARP 代理实现，启动多个实例，每个实例一个出口

出口暴露方式（二选一）：

1. 多端口：每个出口一个本地 SOCKS5 端口，例如 `17890`、`17891`、`17892`
2. 单端口轮换：一个 SOCKS5 端口，按新连接轮换出口，网关每次 429 重试都会建立新连接

配置要点：

- `warp.pool_size` 建议 2 到 4，按并发与注册限流调整
- 优先使用 IPv6 出口（按 `/64` 去重）；IPv4 出口可能因为共享 NAT 仍被上游拒绝
- 宿主机或容器必须能向 Cloudflare 端点发送 UDP（端口 2408）
- 出口首次返回 429 或 403 即淘汰并补注册新出口

### 6.3 代理池与账号配置

1. 在「网络代理」页新增 WARP 出口条目：
   - 多端口形态：每个端口一条，协议 SOCKS5
   - 单端口轮换形态：只新增一条
2. 在 ZCode 账号的代理设置中选择“轮询档”：
   - 全部出口：`{"source":"rotate","all":true}`
   - 指定出口：`{"source":"rotate","all":false,"proxyIds":["..."]}`
3. 账号的其他配置（模型、套餐通道、验证码令牌）不变

### 6.4 可观测性

- 请求日志：本轮尝试明细里追加“轮询换出口（下一个出口）”，不新增 attempt
- 运行日志：只记录出口池状态变化，不按请求刷屏
- 代理池页：显示每个 WARP 出口的可用状态与最近失败原因
- sidecar：暴露健康检查，返回出口数量、活跃/备用/冷却状态

## 七、部署步骤（NAS Docker 示例）

以下命令为示例，实际路径与端口按部署环境调整。

```bash
# 1. 启动 WARP 出口池 sidecar
docker run -d --name zcode-warp-pool \
  -p 17890:17890 -p 17891:17891 \
  -e WARP_POOL_SIZE=2 \
  <warp-sidecar-image>

# 2. 检查出口状态
curl -s http://127.0.0.1:17890/health
```

在 A2API 代理池添加：

```text
socks5://192.168.1.11:17890
socks5://192.168.1.11:17891
```

将 ZCode 账号代理切换为轮询档后，发一次真实请求确认请求日志出现出口信息。

## 八、验证步骤

1. 基线复现：同一出口下，长请求在途时发短请求，确认返回 429 与 `upstream_code=3008`
2. 换出口验证：长请求走出口 A，短请求走出口 B，确认两个请求都成功
3. 轮询档验证：
   - 账号配置多个出口
   - 长请求在途时发短请求
   - 确认短请求先 429、随后在同一账号换出口重发并成功
   - 确认账号没有被标记为额度冷却
4. 兜底验证：关闭或占满所有出口，确认最终返回 429，且冷却逻辑与改动前一致
5. 回归验证：非轮询档账号、非 zcode 提供商的转发行为与改动前一致

## 九、风险与回滚

风险：

- WARP 出口可能被上游风控或共享出口网段影响，仍出现 429
- WARP 设备注册存在限流，池子不能无限扩大
- IPv4 出口通常是共享 NAT，优先使用 IPv6 出口
- 上游可能调整并发判定策略，方案需要随上游行为复核

回滚：

- 账号代理改回静态代理或直连，立即停止使用出口池
- 停止 sidecar，不影响网关其他功能
- 网关侧换出口逻辑只对轮询档生效，关闭轮询档即恢复原行为

## 十、验收标准

- 同一出口并发时返回 429 的现象可稳定复现
- 不同出口并发时两个请求均成功
- 轮询档下 429 自动换出口，客户端只看到一次成功响应
- 请求日志能看出“换出口”这一内部动作
- 非轮询档路径无行为变化，现有测试全部通过

## 十一、参考

- opencode2api：https://github.com/jasonxu114514/opencode2api
- ZCode 套餐池通道实现：`Agent2Providers/crates/providers/src/providers/zcode/plan.rs`
- 网关轮询档实现：`A2API/desktop-tauri/src-tauri/server/src/server/core/proxy_rotate.rs`
