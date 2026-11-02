# Runtime 实时调度层

## 目标

df-runtime 在不修改模型计算语义的前提下，为现有 df-core::Engine 增加真实时间调度。Studio 继续直接使用 Engine 的快速执行能力；Runtime 按 Project 的 cycle_time 周期推进同一个 Engine。

## 当前实现

- 单个 Runtime 实例持有一个 Engine。
- 启动时读取 Engine 的 cycle_time，要求其为有限正数。
- 使用独立调度线程按墙上时钟节拍执行，每个节拍最多推进一个模型周期。
- 模型时间仍由 Engine 的 cycle_index * cycle_time 计算，不使用墙上时钟替代仿真时间。
- 外部写入进入有界 FIFO 队列，在周期边界交给 Engine 原子校验和应用。
- 一个周期只处理一个写入批次；同周期的写入不会在组件计算中途改变模型变量。
- Engine 返回的最新 Snapshot 通过只读快照接口发布；每次发布递增 snapshot revision（`u64` 回绕安全）并通过 `Condvar` 唤醒所有订阅等待者。订阅者醒来后自行重读快照，慢消费者只会跳过中间 revision，不会阻塞模型。
- `Runtime::handle()` 返回可克隆、只读的 `RuntimeHandle`：可查询状态与快照、等待新 revision、提交写入批次、查询/取消写入。Handle 不能停止 Runtime，也拿不到 Engine 引用；生命周期所有权始终在 `Runtime`（Owner）侧。
- 每个写入批次分配单调递增 request_id，在写入登记表中记录生命周期：`Queued → Applying → Applied / Rejected / Cancelled`（终态不可覆盖）。登记表容量 1024，超限时按最老的终态记录修剪，排队中批次不会被驱逐。
- 状态中新增 `snapshot_revision`、`queue_depth`、`last_cycle_started_at`（最近周期开始墙钟时间）与 `last_cycle_lag`（相对本周期墙上截止点的落后量）。
- 统计最近/最大周期执行耗时、超期次数、错过的墙上时钟截止点和拒绝的写入批次。
- Delay 超期策略将下一周期安排在当前周期完成后一个完整周期；CatchUp 保留原定节拍并连续执行逾期周期。两种策略都不跳过模型周期。
- 停止时唤醒调度线程、拒绝尚未处理的写入（登记表置 `Rejected`、ticket 回执 `WorkerStopped`）、停止 revision 通知并释放 Engine。

## 写入语义

submit_writes 只表示写入批次成功进入队列。调用方通过返回的 WriteTicket 等待 Engine 确认；ticket 携带 request_id，可随时通过 `RuntimeHandle::write_state` 查询生命周期状态。

- 成功：返回实际生效的 cycle_index 和 simulation_time，登记表状态置 `Applied`。
- 取消：排队中的批次可 `cancel_write`；调度线程在周期边界发现已取消后跳过该批次并回执 `WriteCancelled`，模型周期照常执行。已进入 Applying 或终态的批次不可取消。
- 非法批次：Engine 原子拒绝整批写入，登记表状态置 `Rejected` 并记录错误，Runtime 统计拒绝数，并在该墙上时钟节拍继续执行不带写入的模型周期。
- 队列已满：立即返回 QueueFull，不隐式丢弃已经接收的批次；失败批次同样登记为 `Rejected`。
- Runtime 已停止：返回 WorkerStopped；停止时仍排队的批次一律置 `Rejected` 并回执 WorkerStopped。

超时不是取消：`WriteTicket::wait` 超时后批次仍在队列中，可继续等待或查询状态。

OPC UA/HTTP 适配层必须通过 `RuntimeHandle` 调用同一个 submit_writes，不能直接修改 Engine 或 OPC UA 节点缓存。节点发布应读取 handle 的 `snapshot()`，并可用 `wait_for_snapshot(after_revision, timeout)` 在新周期发布后被唤醒；`Timeout` 可安全重试，`Stopped` 表示实例已结束。

## 使用方式

先用现有编译器将 DSL v2 YAML 编译为 .dfproject：

```powershell
df-engine compile .\project.yaml .\project.dfproject
```

再启动 Runtime：

```powershell
df-runtime run .\project.dfproject
```

标准输入命令：

| 命令 | 作用 |
|---|---|
| status | 查看运行状态、周期耗时和超期统计 |
| snapshot | 查看最新模型快照 |
| stop | 优雅停止 Runtime |

输入流关闭时也会执行停止清理。

## 当前不包含

- OPC UA Server、HTTP API 和认证。
- 多 YAML/多实例管理。
- Runtime Monitor GUI、报警和历史归档。
- 服务安装、Windows Service 和系统信号处理。
- 自动将 YAML 编译后再启动的 CLI 工作流。

## 验收边界

仓库内的单元测试（Fake Engine）验证调度器、RuntimeHandle 观测与 revision 等待、写入五状态生命周期、原子拒绝、队列容量、停止唤醒和故障状态；最新验证记录见 `doc/engineering/verification-log.md`（2026-11-02：42 个工作区测试通过，Windows 真实引擎端到端通过）。OPC UA、多实例与真实外部写入闭环仍未实现，对应 R2–R10。
