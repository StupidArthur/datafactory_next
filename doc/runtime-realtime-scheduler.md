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
- Engine 返回的最新 Snapshot 通过只读快照接口发布。
- 统计最近/最大周期执行耗时、超期次数、错过的墙上时钟截止点和拒绝的写入批次。
- Delay 超期策略将下一周期安排在当前周期完成后一个完整周期；CatchUp 保留原定节拍并连续执行逾期周期。两种策略都不跳过模型周期。
- 停止时唤醒调度线程、拒绝尚未处理的写入并释放 Engine。

## 写入语义

submit_writes 只表示写入批次成功进入队列。调用方通过返回的 WriteTicket 等待 Engine 确认。

- 成功：返回实际生效的 cycle_index 和 simulation_time。
- 非法批次：Engine 原子拒绝整批写入，Runtime 记录错误，并在该墙上时钟节拍继续执行不带写入的模型周期。
- 队列已满：立即返回 QueueFull，不隐式丢弃已经接收的批次。
- Runtime 已停止：返回 WorkerStopped。

OPC UA/HTTP 适配层必须调用同一个 submit_writes，不能直接修改 Engine 或 OPC UA 节点缓存。节点发布应读取 Runtime::snapshot()，并在模型周期完成后更新。

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

目前仓库内的单元测试使用 Fake Engine 验证调度器、写入确认、原子拒绝、队列容量、停止唤醒和故障状态。尚未在本轮工具环境实际运行 cargo test，也尚未完成 Windows 真实 Engine 端到端验证；因此不能将实时 Runtime 标记为已验收。
