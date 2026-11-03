# FM350 后端架构迁移设计（参照 luci-app-mt5700）

> 状态：**阶段 1–3 已实施完成（2026-11-04），阶段 4 验证数据见文末**。
> 实施记录：RPC newline-JSON 迁移、AT 优先级调度器（`at/queue.rs`，全
> 调用点迁移）、TaskManager（`async_runtime.rs`，含硬上限拒绝）、
> 优雅关闭、ucode 熔断/预检/rid 白名单均已上线，259 项单测全绿。
> 参照物：`luci-app-mt5700` 的 `at_queue.rs`（PriLock/SmsMode/SmsTransaction）、
> `rpcserver.rs`（本地 TCP newline-JSON）、`async_runtime.rs`（TaskManager）、
> `mt5700.uc`（rpcd 侧保护）。
> 原则：**借鉴机制，不照搬实现；不破坏 FM350 已实机验证的串口独占与业务语义。**

## 1. 两侧架构差异审查

| 维度 | MT5700 | FM350 现状 | 结论 |
|---|---|---|---|
| RPC 传输 | 本地 TCP newline-JSON（busybox nc + timeout 代理） | 自实现最小 HTTP/1.1（`rt/server.rs`）+ ucode 每次 popen 一个 `fm350d` CLI 进程再连 HTTP | **迁移**：换 newline-JSON，删 HTTP；ucode 直连省掉 CLI fork |
| AT 执行 | `AsyncFd` 异步 termios 串口，全异步 | 专用 AT worker **线程** + 同步 mailbox + 内核阻塞看门狗 | **保留线程模型**（见 §2.1） |
| AT 排队 | PriLock 优先级门（5 级 + SMS 租约暂停门 + 事务独占） | FIFO mailbox（同优先级 FIFO） | **迁移调度器**，去掉 SmsMode（§2.2） |
| 长任务 | TaskManager（扫频）：6 态 + 取消令牌 + cap 淘汰 | 无（拨号等同步跑满 90s 预算） | **迁移** + 硬上限拒绝（MT5700 只有软淘汰） |
| Runtime | `#[tokio::main]` 多线程 | current_thread + blocking pool | **保留 current_thread** |
| ucode 保护 | timeout 硬超时 + /proc/net/tcp 预检 + 熔断 flag + safeRid | timeout 硬超时 + 门控 + 文件缓存 | **补齐预检/熔断** |
| 优雅关闭 | watch ctx 全链路 | 无（procd SIGTERM 直接杀） | **补齐** |

## 2. 不迁移清单（及理由）

1. **AsyncFd 异步串口** → 保留专用 worker 线程。FM350 实机踩过内核级 tty 阻塞
   （`tty_wait_until_sent` 无界、wedged USB endpoint），同步线程 + 看门狗是已验证的
   防线；换异步串口等于重赌一遍故障链。
2. **SmsMode 租约暂停门** → 用事务独占替代。MT5700 需要它是因为多 WebSocket 会话
   并发；FM350 是单用户 LuCI，事务独占已覆盖「短信操作不被插入」。
3. **EventBus/WebSocket 推送** → FM350 前端走 ubus 轮询，不引入新通道。
4. **多线程 Runtime** → current_thread + blocking pool 已满足，串口阻塞不出现在
   事件循环。
5. **auth_key 机制** → 仅监听 127.0.0.1，调用方是 root rpcd；回环 + rpcd ACL 已控，
   不增加配置面。

## 3. AT 优先级分类（P0–P4，逐业务指定）

| 级别 | 业务（FM350 实际调用点） |
|---|---|
| **P0 Critical** | 端口重选探测（`identify_at_port`）、看门狗复位前的探活、净看护强制 hangup |
| **P1 High** | 用户主动：`dial`/`hangup`/`set apn`/`lock`(GTACT 事务)/短信发送/删除、IMEI 写、USB 模式切换、AT 透传（`/api/at`、CLI `at`） |
| **P2 Normal** | `status`/`info`/`sms list`/`net`/`config`/`ports` 等用户页面拉取的查询 |
| **P3 Low** | daemon 巡检 tick（pdp 轮询、net_guard 探测、v6 维护、信号采样） |
| **P4 Background** | 批量/维护类（当前无常态业务，预留给扫频类扩展） |

规则：禁止一律默认最高级；daemon 自身轮询一律 P3；RPC 读 P2、写 P1；
P0 仅恢复路径。每级队列容量上限 + 单请求排队超时 + **Priority Aging**
（等待超过 5 s 升一级，直到 P0，杜绝 P3/P4 饿死）。

## 4. AT 事务边界（按 FM350-GL AT 手册 V2.10）

事务 = 拿到通道后连续下发多条命令、中间不放行的执行单元。已有业务全部收编：

| 事务 | 命令序列 | 优先级 | 备注 |
|---|---|---|---|
| dial | CPIN?/CFUN?/CEREG? → [CFUN=1+注册等待] → CGATT? → [CGATT=1] → CGDCONT → CGAUTH → CGACT=1 → CGPADDR/CGCONTRDP/GTDNS 轮询 | P1 | 已实现为函数体，整体一个事务 |
| hangup | CGACT=0 | P1 | |
| lock 频段 | CFUN=0 → GTACT=… → CFUN=1 → CEREG 等待 | P1 | 手册：需先下电 |
| USB 模式 | GTUSBMODE=… → 等重枚举 → 重绑定 | P1 | AT 口会短暂消失 |
| sms send | CPMS → CMGF=0 → CSCS → CSCA? → CMGS×N 片 | P1 | 多片必须连续 |
| sms list/del | CMGL / CMGD×N | P2/P1 | 删除多条是事务 |
| imei 写 | EGMREXT 读校验 → 写 | P1 | 高危，独占 |

已开始的**命令**不可粗暴取消（写入串口就要读完响应）；已开始的**事务**完成或
安全超时才交还通道。迟到响应防护由 worker 现有「一问一答同步读」保证。

## 5. RPC 协议（替代自实现 HTTP/1.1）

- 传输：`127.0.0.1:8766`，一行请求一行响应，UTF-8 JSON。
- 请求：`{"id":<原样回显>,"method":"status","params":{...}}\n`
- 响应：`{"id":…,"ok":true,"result":…}\n` 或
  `{"id":…,"ok":false,"code":<int>,"error":"…"}\n`
- method 与现 `/api/*` 路径一一对应（status/signal/pdp/dial/…），返回结构不变
  → LuCI 前端零改动。
- 约束：MAX_LINE 1 MiB；并发连接 ≤ 8（超限立即回 `code=-32004` 后关闭）；
  读空闲 30 s 断开；每请求总期限沿用 `budget::for_request`；
  非法 JSON 回 `-32700` 不断连，超长断连。
- CLI `fm350d` 保留：`run_cli` 改走同一 newline-JSON 协议（SSH 手工调试用）。

## 6. 异步任务管理器（TaskManager）

参考 MT5700 六态 + 取消令牌，**两处增强**：
1. **硬上限**：in-flight（pending+running）≥ cap（默认 8）时 `spawn` 返回
   `Err("任务槽已满")`，而非排队或只淘汰历史。
2. **状态覆盖保护**沿用（仅 Running/Pending 可被完成回调改写）；
   超时置 Timeout + 发取消信号，不 drop 任务体（收尾自己跑完）。

接入：`dial`/`hangup`/`redial` 类写操作提交为任务，RPC 立即回
`{"task":"dial-1"}`；新增 method `task_status`。**fm350.uc 的 dial 调用在
ucode 侧轮询任务直到完成（沿用 TMO_WRITE 预算），对前端保持原返回结构**
——短查询仍同步，前端零改动。

## 7. Runtime 与背压

- current_thread 不变；blocking pool 全局 `Semaphore(4)` 封顶。
- RPC accept/逐连接为 async 任务（有界连接数）；路由 dispatch 仍在
  blocking_scope。
- SIGTERM/SIGINT → watch 关闭信号 → 停 accept、等待在途请求/任务（上限 5 s）
  → 退出；AT worker 线程随进程退出（串口由内核回收）。
- 新代码禁止锁跨 await；调度器等待全部在锁外 select。

## 8. ucode 保护（fm350.uc 增量）

- 调用方式：`printf '%s' <payload> | timeout N nc 127.0.0.1 8766`（替代
  `timeout N fm350d <cmd>`，省一次 fork + CLI 启动）。
- `/proc/net/tcp` 监听预检（0A 行 + 端口十六进制匹配）→ 未监听写
  `/tmp/fm350-backend-down` 熔断标记（TTL 15 s），窗口内全部毫秒级快速失败。
- `_rid` 白名单校验（`[A-Za-z0-9_-]`，≤32），临时文件名含 rid 的路径防注入。
- 按方法分预算沿用 TMO_READ=12 / TMO_WRITE=90；任务轮询新档。
- `daemon_alive()` 门控与 `/tmp/fm350.cache.*` 缓存保留。

## 9. 实施顺序

1. **阶段 1 — RPC 迁移**：`rpc/`（server + codec）替换 `rt/server.rs` HTTP；
   `fm350.uc`、`cli.rs` 改 newline-JSON；删除 HTTP 代码。回归：前端全部页面 + CLI。
2. **阶段 2 — AT 调度器**：`at/queue.rs`（PriLock 变体 + aging + 有界队列 +
   排队超时/取消）替换 mailbox 入口；全部调用点按 §3 标级；事务化（§4）。
   测试：优先级序 / 同级 FIFO / aging / 队满 / 排队超时 / 取消 / 事务独占。
3. **阶段 3 — TaskManager + ucode 加固**：任务管理器、dial 异步化（ucode 轮询
   封装）、/proc/net/tcp 预检 + 熔断 + safeRid、优雅关闭。
4. **阶段 4 — 验证**：cargo test；实机：并发 6 路 p50/p95、RPC 延迟对比、
   RSS/线程数对比、后端停启时 LuCI 可用性、长跑内存。

## 10. 阶段 4 验证数据（实机：ImmortalWRT / MT7981）

| 指标 | 结果 |
|---|---|
| RPC 串行 ping ×40（含每次 fork busybox nc） | < 1 s（≤25 ms/次） |
| 6 路并发 ping ×40 / ports×40 | 全部成功，零失败 |
| 常驻 RSS | ≈ 1.5 MB |
| 线程数 | 9（事件循环 + AT worker + 看门狗 + 阻塞池） |
| 后端停止时 LuCI 可用性 | ucode 预检 + 15 s 熔断 → 亚毫秒降级提示 |
| 优雅关闭 | SIGTERM → 「巡检循环退出，fm350d 已优雅关闭」日志确认 |
| 基准脚本 | `scripts/bench_rpc.sh`（随仓库发布） |

调度器单测覆盖：优先级序 / 同级 FIFO / Priority Aging 升级 / 队满快速
失败 / 取消跳过 / worker 串行与高优先级插队（`at/queue.rs` + `at/worker.rs`）。
任务管理器单测覆盖：完成/失败回报、槽位占满明确拒绝、Pending 取消留痕
（`async_runtime.rs`）。

## 11. 验收对照（用户规格 → 落点）

| 要求 | 落点 |
|---|---|
| 不自实现 HTTP / 不引 Web 框架 | §5（删除 rt/server.rs 的 HTTP） |
| request_id / 半包粘包 / 长度限制 | §5（行分隔天然定界 + MAX_LINE） |
| 串口唯一执行者 / 同级 FIFO / 不中断在执行命令 | §2.1 保留 worker + §4 |
| Priority Aging / 配额 | §3（aging 升级即隐式配额） |
| 队列容量上限 / 排队超时 / 取消等待 | §3 |
| 事务独占（短信/拨号/锁频/扫频） | §4 |
| 请求合并（只读查询） | RPC 层 single-flight + TTL 缓存（P2 读类） |
| 任务硬上限拒绝 / 状态不被覆盖 / 区分超时 | §6 |
| fs.popen 硬超时 / 熔断 / 预检 / rid 校验 | §8 |
| 优雅关闭 / 事件循环无阻塞 / 有界 | §7 |
| 保留 ACL / 回环 / 参数校验 / 无注入 / 无敏感日志 | §5、§8 |
