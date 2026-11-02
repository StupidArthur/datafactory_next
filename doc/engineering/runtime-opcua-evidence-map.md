# Runtime / OPC UA 第一轮实现：技术特征与实施证据映射

> 文档性质：工程事实索引 / 专利实施证据整理底稿，不构成专利权利要求或可专利性法律意见。  
> 代码基线：main，commit `456723421b58c927bda421d40f36210eaf4c72a2`（R9 + PID 闭环补充实施例）。  
> 验证日期：2026-11-02。  
> 设计依据：`doc/runtime-system-design.md`，重点为 §3–§9、§14、§17。  
> 验证记录：`doc/engineering/verification-log.md` 中 R1–R9 条目及 PID 闭环补充实施例。

## 1. 使用规则与证据等级

本表只描述基线 Commit 中实际存在的代码和已记录的测试。设计文档中的目标、TODO 中的计划、未执行的测试，不自动构成实现证据。

| 标记 | 含义 |
|---|---|
| **CI-default** | GitHub Actions 在 Ubuntu / Windows 上执行默认 feature 的格式检查、Engine 构建和工作区测试 |
| **Local-feature** | 本地启用 opcua feature 的 Rust 单元/集成测试通过 |
| **Local-e2e** | 本地真实 Engine + OPC UA Server + 独立 OPC UA Client 的端到端测试通过 |
| **Unit** | 单元测试证明局部规则；不等同于外部客户端或生产环境验证 |
| **Partial** | 已有部分实现或证据，但尚未覆盖设计文档中的完整验收条件 |
| **Not tested** | 当前没有足够测试或运行记录，不得作为已验证效果陈述 |

代码链接均固定到上述基线 Commit，避免后续行号变化导致证据漂移。

## 2. 系统实现链路

当前实现形成以下运行链路：

1. Host 配置加载 YAML / dfproject 路径和实例调度参数。
2. RuntimeManager 为每个实例分别加载 Engine，并创建独立 Runtime。
3. 每个 Runtime 的调度 worker 按 cycle_time 等待墙上时钟周期，最多取一个写入批次，在 Engine 周期执行入口应用。
4. RuntimeHandle 向适配层提供只读快照、快照 revision、状态和写入提交/查询能力，不向适配层暴露 Engine 所有权或停止权。
5. OPC UA Server 根据每个 Engine 的 Tag Catalog 构建实例地址空间；SnapshotPublisher 发布每个实例的周期快照和状态。
6. OPC UA Write Handler 校验 NodeId、属性、范围、可写性、身份角色和 Variant 类型；合法写入按实例聚合后提交到对应 RuntimeHandle。
7. Runtime 在 Engine 确认周期执行后生成 WriteReceipt；OPC UA 写响应据此返回 Good。客户端可从发布链读取更新后的输入和模型输出。

**状态边界：** Engine 是模型状态的唯一权威来源；OPC UA 地址空间是读写适配层，不是第二份模型状态。跨实例事务原子性不作承诺；原子性边界是单个实例的一批写入。

## 3. 技术特征—代码—测试—实施例映射

### 3.1 共享 Engine 与实时周期执行

| 项目 | 证据 |
|---|---|
| 技术事实 | Runtime 通过 EngineExecutor 抽象调用现有 Engine 的单周期执行接口；Runtime worker 使用 cycle_time 调度，不复制或重写模型组件计算逻辑 |
| 主要代码 | [EngineExecutor / Runtime 启动与执行](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/lib.rs#L142-L180)；[Runtime::start_with_config](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/lib.rs#L205-L250)；[run_worker](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/lib.rs#L442-L620) |
| 测试 | publishes_cycles_and_stops_cleanly；acknowledges_a_write_at_the_cycle_where_it_takes_effect；Windows two-tank 真实引擎运行记录 |
| 证据等级 | Unit + Local-e2e；Runtime 默认测试包含于 CI-default |
| 边界 | **Partial：** 已证明 Runtime 使用共享 Engine 执行核心并实时推进；尚未完成同一 YAML、同一初始条件下 Fast/Offline 与 Realtime 的定量结果对照，不能据此宣称两种模式的数值一致性已完成专门验收 |

### 3.2 周期边界写入、回执与写入生命周期

| 项目 | 证据 |
|---|---|
| 技术事实 | 写入先进入有界 FIFO 队列；worker 每个模型周期最多取一个批次；Engine 在周期执行入口接收批次；成功后生成含 effective_cycle_index / simulation_time 的 WriteReceipt |
| 主要代码 | [Runtime::submit_writes / WriteTicket](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/lib.rs#L368-L440)；[周期 worker](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/lib.rs#L442-L620)；[RuntimeHandle / WriteState / WriteStatusInfo](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/runtime_handle.rs#L25-L80) |
| 测试 | acknowledges_a_write_at_the_cycle_where_it_takes_effect；rejected_batch_is_atomic_and_does_not_consume_the_tick；bounded_queue_rejects_excess_batches_and_stop_wakes_the_worker；write_lifecycle_cancelled_while_queued；write_lifecycle_applied_with_receipt；write_lifecycle_rejected_records_the_error；queue_depth_tracks_pending_batches_and_stop_rejects_them |
| 证据等级 | Unit；OPC UA 外部写入确认另见 §3.8 |
| 边界 | **Partial：** Runtime 内部可查询 request_id 和 effective_cycle_index；当前没有通过 OPC UA 向客户端公开 CL-03 诊断查询。BadTimeout 表示等待超时、结果不确定，不表示写入已取消或必然失败 |

### 3.3 Host 配置、实例生命周期与故障隔离

| 项目 | 证据 |
|---|---|
| 技术事实 | Host 配置严格解析；相对路径以配置文件目录为基准；每个实例单独加载 Engine、创建 Runtime、管理状态；单实例停止/重启不要求停止其他实例 |
| 主要代码 | [HostConfig / 配置校验](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/config.rs#L22-L45)；[HostConfig::load / parse](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/config.rs#L266-L363)；[路径解析](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/config.rs#L616-L675)；[RuntimeManager](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/manager.rs#L21-L218) |
| 测试 | config 单元测试；example_host_config_lists_instances_with_resolved_projects；start_all_isolates_failures_and_skips_disabled；instances_keep_independent_engines_and_schedulers；restart_resets_the_engine_to_initial_values；shutdown_stops_everything_and_handles_report_stopped |
| 证据等级 | Unit + CI-default |
| 边界 | **Partial：** 已验证两实例独立推进、A 停止后 B 继续运行、实例加载失败隔离；设计验收 INS-03（不同 cycle_time 的独立推进）和 INS-07（实例数量上限）尚无专门验收证据 |

### 3.4 Runtime CLI 与独立启动

| 项目 | 证据 |
|---|---|
| 技术事实 | 支持单 dfproject 启动兼容入口、Host 配置 check/run、version，以及 stdin status/snapshot/stop 控制 |
| 主要代码 | [CLI 参数解析与入口](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/cli.rs#L62-L112)；[check_config / run_host](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/cli.rs#L181-L356)；[Host 命令处理](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/cli.rs#L414-L479) |
| 测试 / 实施例 | CLI 参数解析测试；verification-log 的 R4、R8 Windows 真实二进制运行记录，包含 Host 启动、状态查询、stop、退出码 4 / 5 的实际触发 |
| 证据等级 | Unit + Windows Local-e2e；默认 CI 运行 CLI 参数测试 |
| 边界 | Windows Service / 系统信号优雅停止仍未实现；不应描述为 Windows Service 级守护进程 |

### 3.5 OPC UA 安全配置与身份角色

| 项目 | 证据 |
|---|---|
| 技术事实 | 支持 None / Sign / SignAndEncrypt 与策略配置；非回环监听受安全配置约束；用户名密码由环境变量解析；Reader / Writer 分权，匿名身份按 Reader 处理 |
| 主要代码 | [安全配置与用户角色](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/config.rs#L58-L145)；[安全配置校验](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/config.rs#L446-L525)；[凭据解析](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/config.rs#L134-L160) |
| 测试 / 实施例 | config 的 endpoint、loopback、users、password_env 校验测试；secure_channel_with_username_password_auth；rejection_matrix_enforces_catalog_identity_and_atomicity |
| 证据等级 | Unit + Local-feature + Local-e2e |
| 边界 | **Partial：** 已验证 SignAndEncrypt / Basic256Sha256 用户名密码会话和 Reader 写拒绝；未把未受信客户端证书拒绝（SEC-03）作为独立客户端负向测试完成验收。不要把样例客户端 trust_server_certs(true) 描述成完整 PKI 信任链验收 |

### 3.6 Tag Catalog 到 OPC UA 地址空间映射

| 项目 | 证据 |
|---|---|
| 技术事实 | 地址空间从 Engine Tag Catalog 构建；每实例有独立 Tags / Status 节点；NodeId 使用确定性路径；Tag DataType 严格映射；单位与说明作为元数据暴露 |
| 主要代码 | [Catalog / AddressSpaceIndex](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/address_space.rs#L19-L82)；[目录校验与地址空间构建](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/address_space.rs#L104-L260)；[类型与 NodeId 编码](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/value.rs#L9-L80) |
| 测试 / 实施例 | catalogs_reject_duplicates_and_unknown_types；catalog_address_space_reads_writes_and_subscriptions；node_ids_are_stable_across_restarts；tag_metadata_description_and_unit_property |
| 证据等级 | Unit + Local-feature + Local-e2e |
| 边界 | **Partial：** NodeId 稳定、实例同名 Tag 不冲突、Description/Unit 元数据有客户端读取证据；UA-02 的独立客户端完整 Browse 层级验收尚未完成，UA-05 完整客户端 BOOL/INT64/FLOAT64/STRING 矩阵也未全部完成（部分映射目前只有单元测试） |

### 3.7 OPC UA 周期快照发布、订阅与状态

| 项目 | 证据 |
|---|---|
| 技术事实 | 每个实例独立等待 Snapshot revision；周期快照和实例 Status 批量写入地址空间；另有线程聚合 ServerStatus；Tag DataValue 附带 SourceTimestamp 和运行状态码 |
| 主要代码 | [SnapshotPublisher 与周期发布](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/publisher.rs#L97-L214)；[ServerStatus 聚合](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/publisher.rs#L215-L264)；[Publisher 生命周期](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/publisher.rs#L265-L380) |
| 测试 / 实施例 | publisher_streams_cycles_status_and_marks_out_of_service_on_stop：独立客户端订阅多个变化值、读取 CycleIndex / SimulationTime / CycleTime、检查 SourceTimestamp、停止后检查 BadOutOfService；Publisher 单元测试覆盖状态码和 Host 聚合规则 |
| 证据等级 | Unit + Local-feature + Local-e2e |
| 边界 | **Partial：** 有客户端订阅变化和时间戳读取证据；Starting 窗口状态码、停止瞬间最终订阅通知，以及“任意多 Tag 同一快照”的完整客户端一致性矩阵尚未全部独立验收 |

### 3.8 OPC UA 写入校验、批次提交与实例级原子性

| 项目 | 证据 |
|---|---|
| 技术事实 | Write Handler 对已知 Tag 校验 Value 属性、IndexRange、值存在性、writable、Writer 角色和严格 Variant 类型；已知 Tag 的拒绝携带 instance_id；合法项按实例分组，各实例独立提交；同实例任一 Tag 非法则该实例整批不入队 |
| 主要代码 | [WriteHandlerState / validate_write](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/write.rs#L83-L271)；[handle_write 两阶段校验、分组和提交](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/src/opcua/write.rs#L273-L399) |
| 测试 / 实施例 | write.rs 单元测试；rejection_matrix_enforces_catalog_identity_and_atomicity；writer_writes_apply_in_order_at_cycle_boundaries；queue_full_rejects_and_timed_out_writes_still_apply；stopped_instances_reject_writes_and_pending_batches_are_discarded；multi_instance_request_vetoes_one_batch_without_blocking_the_other |
| 证据等级 | Unit + Local-feature + Local-e2e |
| 关键回归证据 | R8-B 新增跨实例集成测试；回退 write.rs 后测试失败，实例 B 被旧实现错误连坐为 BadNothingToDo；恢复修复后测试通过 |
| 边界 | 不承诺跨实例事务原子性；OPC UA 标准 Write 服务没有本项目自定义取消入口；BadTimeout 不等于撤销；客户端查询超时请求最终状态仍未接入 OPC UA |

### 3.9 R9 双水箱、双 Runtime、双 Session 最小闭环

| 项目 | 证据 |
|---|---|
| 技术事实 | 两个独立 Engine 分别加载同一双水箱项目，创建两个 Runtime 并挂到同一个 SignAndEncrypt Server；两个独立 Client Session 读取各自实例；A 的输入写入由 Engine 确认，模型输出随后变化；B 的输入保持初值且自身模型继续推进 |
| 主要代码 | [双实例测试夹具](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/tests/opcua_write.rs#L216-L312)；[R9 测试](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/crates/df-runtime/tests/opcua_write.rs#L741-L877) |
| 实施操作 | A 的 plant_inlet_flow 写入 0.004，回读 0.004，tank_1.level 高于初值；再次写 0.006 并回读；B 的客户端输入与 Engine 快照保持 0.0012；B 液位仍由自己的模型推进；停止 A 后 A 写入 BadOutOfService，B 仍可读；停止 Server 后端口停止接受连接 |
| 证据等级 | **Local-e2e（Windows，本地启用 opcua feature）** |
| 重要限定 | 这是 R9 最小闭环实施例。它写的是 plant_inlet_flow 并观察 tank_1.level，不是设计 §14.5 中更完整的 level_setpoint → PID MV → 阀门开度 → 液位链。因此可证明“外部输入进入实时 Engine 并引起当前模型响应”，但不能宣称完整 PID 设定值控制场景已经验收 |
| 其他限定 | B 的隔离断言聚焦在输入值（客户端读值 + Engine 快照）和 B 自身模型继续运行；没有逐一断言 B 的所有 Tag / 内部组件状态都不变 |

### 3.10 外部 PID 设定值双水箱闭环补充实施例

| 项目 | 证据 |
|---|---|
| 技术事实 | 外部 OPC UA Writer 只能写入 level_setpoint；PID 读取 tank_2.level 作为 PV、读取 level_setpoint 作为 SV，输出 pid2.MV 驱动阀门目标开度；阀门实际开度改变出口流量，两个水箱液位随后变化，tank_2.level 再反馈给 PID PV |
| 主要工程 | [PID 闭环 DSL v2 工程](https://github.com/StupidArthur/datafactory_next/blob/456723421b58c927bda421d40f36210eaf4c72a2/examples/two-tank/pid-closed-loop.yaml)；[实施例说明](https://github.com/StupidArthur/datafactory_next/blob/456723421b58c927bda421d40f36210eaf4c72a2/examples/two-tank/README.md) |
| 主要代码 / 测试 | [真实 OPC UA Client PID 阶跃测试](https://github.com/StupidArthur/datafactory_next/blob/456723421b58c927bda421d40f36210eaf4c72a2/crates/df-runtime/tests/opcua_write.rs#L999-L1248) |
| 实施操作 | SignAndEncrypt Writer 将 level_setpoint 从约 0.09706422 m 上调至 0.40 m，再下调至 0.20 m；读取 PID SV/PV/MV、阀门 target/current opening/outlet_flow 和两级水箱液位；真实写入测试断言 PID 状态、阀门状态及过程输出均返回 BadNotWritable，只有 level_setpoint 可写 |
| 证据等级 | **Windows 专项 CI（单项 OPC UA feature 真实客户端 E2E）** |
| 实测样本 | 初始 MV/阀门开度 23.000、出口流量 0.000276 m³/s；上调后 PID MV / 阀门目标后续达到 53.515，阀门实际开度 53.515、出口流量 0.000642 m³/s，tank_1.level = 0.083614 m、tank_2.level / PID PV = 0.107077 m；下调后 PID MV / 阀门目标降至 40.243，阀门实际开度降至 45.182、出口流量降至 0.000542 m³/s |
| 边界 | 证明真实 Engine 的闭环响应链，不证明液位已收敛至设定值、实物 PID 整定、抗积分饱和、故障安全或 SIL；该 CI 只跑这一项 Windows feature 测试，不是完整 OPC UA feature 测试矩阵 |

## 4. R8 / R9 与 PID 闭环补充实施例修正、验收记录

| Commit | 内容 | CI / 本地证据 |
|---|---|---|
| df02c92 | R8-A Windows 路径规范化 | 最终修复进入后续提交；不要单独把该 SHA 当作双平台 CI 通过证据 |
| c545651 | R8-A / R8-B 修正与跨实例回归测试 | [Actions #41417657194](https://github.com/StupidArthur/datafactory_next/actions/runs/41417657194)：Ubuntu、Windows 默认工作区均通过；OPC UA 跨实例测试是 Windows 本地 feature 验证 |
| 2605dca | R8 修正与验收记录 | [Actions #41418883892](https://github.com/StupidArthur/datafactory_next/actions/runs/41418883892)：Ubuntu、Windows 默认工作区均通过 |
| 1313a19 | R9 双水箱双实例闭环测试与记录 | [Actions #41419403497](https://github.com/StupidArthur/datafactory_next/actions/runs/41419403497)：Ubuntu、Windows 默认工作区均通过；R9 OPC UA 测试是 Windows 本地 feature 验证 |
| 4567234（PR #3 squash merge；源提交 ddec2a2） | PID 设定值闭环补充实施例（含真实只读写入拒绝与出口流量断言） | [Actions #41437587420](https://github.com/StupidArthur/datafactory_next/actions/runs/41437587420)：Ubuntu、Windows 默认工作区均通过；[Actions #41437587512](https://github.com/StupidArthur/datafactory_next/actions/runs/41437587512)：Windows 专项 OPC UA PID 测试通过（1 passed / 0 failed，24.86 s） |

### CI 的准确含义

默认工作流 [rust-ci.yml](https://github.com/StupidArthur/datafactory_next/blob/1313a1958694124c76470fb17ecf3ead58988603/.github/workflows/rust-ci.yml) 在 Ubuntu / Windows 执行：

- cargo fmt --all -- --check
- cargo build -p df-engine --bin df-engine
- cargo test --workspace --locked

该默认工作流没有启用 opcua feature；另新增 [runtime-opcua-pid-e2e.yml](https://github.com/StupidArthur/datafactory_next/blob/456723421b58c927bda421d40f36210eaf4c72a2/.github/workflows/runtime-opcua-pid-e2e.yml)，只在 Windows 上运行一个指定的 PID 闭环集成测试。因此：

- R8-A 路径修复由默认工作区 CI 的双平台结果覆盖。
- R8-B 的 OPC UA 跨实例回归测试、R9 的 OPC UA 双实例闭环测试，仍是本地 feature 测试证据，不属于 CI 覆盖。
- PID 闭环补充实施例有 Windows 单项 feature CI 证据；没有 Ubuntu feature 测试，也没有完整 OPC UA feature 测试电池的 CI 覆盖。
- 不应将“Ubuntu / Windows 默认 CI 通过”简写成“完整 OPC UA feature 在 Ubuntu / Windows CI 通过”。

## 5. 当前可陈述与不可陈述的技术事实

### 5.1 当前已有代码与证据支持的陈述

1. Runtime 使用现有 Engine 执行模型周期，并按配置周期进行实时调度。
2. Host 可管理多个独立 Runtime 实例；实例拥有独立 Engine、快照、调度和写入队列。
3. OPC UA 地址空间由 Tag Catalog 构建，节点标识包含实例路径，支持实例内 Tag 读写权限和类型映射。
4. SnapshotPublisher 将 Runtime 快照和状态发布到 OPC UA 地址空间，并有独立客户端读取、订阅和时间戳验证记录。
5. OPC UA 写入经过权限、可写性、属性、范围和类型校验，按实例形成批次，并在对应 Runtime 的 Engine 周期执行边界应用。
6. OPC UA Write 响应以 Engine 周期确认结果为依据；超时语义保持不确定，不把超时当作取消。
7. R9 最小实施例证明：向实例 A 写入过程输入后，A 的模型输出变化；实例 B 的对应输入没有被 A 的写入覆盖，且 B 继续独立运算。
8. PID 闭环补充实施例证明：外部设定值经 PID 和阀门作用于双水箱，出口流量和液位变化反馈至 PID PV；设定值上调和下调时控制输出按预期变化。

### 5.2 当前不能作为已完成效果陈述的事项

- Fast/Offline 与 Realtime 对同一配置、同一输入序列的数值一致性对照。
- PID 设定值变化后的液位收敛时间、稳态误差和长期稳定性；当前只验证阶跃后的方向性响应。
- OPC UA 客户端查询 WriteReceipt / effective_cycle_index（CL-03）。
- UA-02 完整 Browse 层级的独立客户端验收。
- UA-05 BOOL / INT64 / FLOAT64 / STRING 完整客户端矩阵验收。
- OPC UA 请求超时后的客户端最终状态查询，以及通过 OPC UA 取消已排队写入。
- 不受信任客户端证书拒绝的完整负向验收。
- 30 分钟长稳、目标规模性能、CPU / 内存、P95 / P99 周期预算和可支持实例上限。
- 完整 OPC UA feature 测试电池的 Ubuntu / Windows CI 覆盖（目前只有 Windows 单项 PID 测试）。

## 6. 下一阶段证据补齐顺序

按证明价值和复用程度排序，不建议先扩展 Monitor、报警或历史归档：

1. **Fast / Realtime 对照：** 同一 YAML、初始值、写入序列和周期数，比较相同 cycle_index 的 Tag 值；明确浮点容差和墙上时间差异。
2. **OPC UA 客户端验收补齐：** Browse 层级、四类 Tag 类型客户端读取、未受信证书拒绝；保留客户端脚本、输出和服务端日志。
3. **可追溯性决策：** 如确需 CL-03，再单独设计安全、稳定的 OPC UA 写入状态查询接口；当前 Runtime 内部 WriteReceipt 不等于客户端可查询能力。
4. **性能与长稳：** 先确定目标硬件和代表性 Tag / Node 数，再执行定时采样；在有实测数据前不声明容量或性能指标。

## 7. 复现入口

- 设计与验收编号：`doc/runtime-system-design.md` §14。
- Runtime / OPC UA 验证台账：`doc/engineering/verification-log.md`，R1–R9 及 PID 闭环补充实施例。
- 单实例 / Runtime 写入测试：`crates/df-runtime/src/lib.rs`、`crates/df-runtime/src/runtime_handle.rs`。
- 多实例配置和生命周期：`crates/df-runtime/src/config.rs`、`crates/df-runtime/src/manager.rs`、`crates/df-runtime/tests/host_config.rs`。
- OPC UA 地址空间、发布、写入：`crates/df-runtime/src/opcua/`。
- 独立客户端集成测试：`crates/df-runtime/tests/opcua_poc.rs`、`opcua_publisher.rs`、`opcua_write.rs`。
- R9 双水箱过程输入工程：`examples/two-tank/project.yaml` 与 `examples/two-tank/two-tank.dfproject`。
- PID 设定值闭环补充实施例工程：`examples/two-tank/pid-closed-loop.yaml`；测试：`opcua_pid_setpoint_controls_valve_and_two_tank_levels`。

**证据保全要求：** 每个新增实施例应保存配置文件、实际执行命令、测试源文件、关键日志/读数、运行环境和 Commit SHA。只保留测试名称或设计说明，不足以替代可复现实施记录。
