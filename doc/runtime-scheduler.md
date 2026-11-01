# Runtime 实时调度层

状态：代码已提交；本地编译与测试验收待执行

## 1. 模块职责

新增 df-runtime crate，位于现有 Rust Engine 之上。它不实现第二套模型计算逻辑，而是通过 df-sdk::EngineClient 驱动已有 df-engine 进程。

```text
Runtime API / 后续 OPC UA Adapter
              │
       bounded write queue
              │
       df-runtime scheduler
              │ execute(1, writes)
              ▼
          df-sdk
              │
          df-engine
              │
           df-core
```

- df-core 继续只处理仿真周期和模型状态。
- df-engine 继续提供单进程请求/响应执行接口。
- df-runtime 负责真实时间调度、外部写入排队、运行状态和最新 Snapshot。
- OPC UA 尚未集成；后续 OPC UA Adapter 应调用同一个 Runtime::submit_writes，读取 Runtime::snapshot，不得直接修改 Engine 状态。

## 2. 时间语义

1. Runtime 从编译后 Project 的 project_metadata.cycle_time 读取周期。
2. 启动时先读取一次初始 Snapshot。
3. 每到一个墙上时钟周期，Runtime 调用一次 EngineClient::execute(1, writes)。
4. Engine 仍只按 cycle_time 推进仿真时间，不读取系统时钟。
5. 正常情况下，模型周期与真实时间节拍一致；发生超时时，按照 OverrunPolicy 处理。
6. Runtime 不跳过模型周期。cycle_index 只由 Engine 成功执行周期后推进。

### 2.1 超时策略

- Delay（默认）：某周期执行超时后，从该周期结束时重新计算下一周期截止时间。不会跳过模型周期，但仿真时间可能落后真实时间。
- CatchUp：保持原始周期时间表，落后的周期立即执行，逐步追赶真实时间。不会通过一次调用跳过多个模型周期。

状态中记录 last_execution_duration、max_execution_duration、overrun_count 和 missed_deadlines。

## 3. 写入语义

- 写入通过有界 FIFO 队列进入 Runtime，默认容量为 256 个批次。
- 每个模型周期最多应用一个写入批次；多个批次按入队顺序分布到后续周期。
- 同一批次由 Engine 原子校验和应用；任一 Tag 不存在、不可写或类型不匹配时，整批拒绝。
- 写入成功时，WriteReceipt 返回实际生效的 cycle_index 和 simulation_time。
- 写入被拒绝时，Engine 不推进该次调用的周期；Runtime 随后以空写入执行该墙上时钟周期，避免非法写入导致实时节拍少推进一个周期。调用方收到原始拒绝错误。
- 队列满时立即返回 QueueFull，不静默丢弃或覆盖已有写入。
- 等待写入确认超时不代表取消写入；同一个 WriteTicket 可以再次等待。

## 4. 快照与状态

- Runtime 持有最近一次成功周期的 Snapshot 副本。
- Runtime::snapshot() 只读取已发布快照，不会访问正在执行中的 Engine 内部状态。
- Runtime::status() 提供运行状态、周期、仿真时间、执行耗时、超时统计、写入拒绝计数和最后错误。
- Engine 致命错误使 Runtime 进入 Failed，拒绝队列中的待处理写入并尝试关闭 Engine。
- stop() 唤醒调度线程，拒绝尚未生效的排队写入，再关闭 Engine；重复调用安全。

## 5. 当前公开接口

```rust
let engine = EngineClient::start(project_path)?;
let mut runtime = Runtime::start(engine)?;

let ticket = runtime.submit_writes(BTreeMap::from([
    ("pid1.SV".to_string(), Value::Float(0.8)),
]))?;

let receipt = ticket.wait(Duration::from_secs(1))?;
let status = runtime.status();
let snapshot = runtime.snapshot();

runtime.stop()?;
```

Runtime::start_with_config 可配置超时策略和写入队列容量。Runtime 当前一次管理一个 Engine 实例；多 YAML 实例管理属于后续 Runtime Host/CLI 阶段。

## 6. 当前实现边界

本阶段已经新增调度 crate、EngineExecutor 适配层、双时间调度策略、周期边界写入队列、写入确认、Snapshot 发布、状态统计和单元测试。

尚未完成或验收的内容：

- cargo fmt --check、cargo check --workspace 和 cargo test --workspace 尚未在目标环境运行。
- 尚未实现独立 Runtime CLI 和多 YAML 实例管理。
- 尚未实现 OPC UA Server、节点映射、客户端订阅和闭环写入。
- 尚未实现 Monitor、报警和历史归档。
- 尚未完成 Windows 目标平台的启动、停止和长时间运行验证。

因此，不能将本阶段标记为“已验收”，也不能在专利材料中将 OPC UA 闭环等未实现能力描述为现有功能。
