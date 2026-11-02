# DataFactory 验证台账

> 本文件按时间顺序记录对已提交代码的实际验证轮次：命令、环境与真实输出摘要。
> 约定：只有在本台账留有记录的验证才能作为"已验证"状态依据；未记录的验证不得在 TODO 或设计文档中声称完成。
> 环境：Windows（win32）、rustc/cargo 1.99.1（stable-x86_64-pc-windows-msvc）、Go 1.26.4、Node 24.12.0、Python 3.11.9。

## 2026-11-02：Runtime 首轮验证

范围：`crates/df-runtime`（含既有 df-core/df-sdk 回归）。

### 1. 格式检查

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0，无格式差异）
```

### 2. 工作区全量测试

```text
命令: cargo test --workspace
结果: 通过（退出码 0）

test result: ok. 19 passed; 0 failed  — df-core 单元测试 (src\lib.rs)
test result: ok. 3 passed; 0 failed   — df-core Rust SDK 集成测试 (tests\rust_sdk.rs)
test result: ok. 8 passed; 0 failed   — df-runtime 调度层单元测试 (src\lib.rs)
test result: ok. 3 passed; 0 failed   — df-sdk 单元测试 (src\lib.rs)
合计: 33 passed / 0 failed
```

### 3. Windows 真实引擎端到端（two-tank 实时运行）

```text
编译: target\debug\df-engine.exe compile examples\two-tank\project.yaml %TEMP%\df_two_tank_verify.dfproject
运行: target\debug\df-runtime.exe run <dfproject>，stdin 依次发送 status / (等待约2s) / status / snapshot / stop
结果: 通过（退出码 0）

runtime started: state=Starting, cycle_time=0.5s, cycle_index=0
status#1 (≈0.8s): state=Running cycle_index=1 simulation_time=0.5s last_cycle=95µs overruns=0 missed_deadlines=0
status#2 (≈2.8s): state=Running cycle_index=5 simulation_time=2.5s last_cycle=46.6µs max_cycle=95µs overruns=0 missed_deadlines=0
snapshot: cycle_index=5 simulation_time=2.5s
  values={"plant_inlet_flow": 0.0012, "tank_1.inlet_flow": 0.0012,
          "tank_1.level": 0.1767405686760015, "tank_1.outlet_flow": 0.00045858161816505306,
          "tank_2.inlet_flow": 0.00045858161816505306, "tank_2.level": 0.10568507436488177,
          "tank_2.outlet_flow": 0.000286330974852232}
stop → "runtime stopped"，退出码 0
```

判定：

- 实时节拍符合设计：5 个周期 × cycle_time 0.5s = simulation_time 2.5s，无跳周期（CLK-01 语义通过；CLK-02 长时门限不在本轮范围）。
- 模型输出正确：液位自初值上升，流量沿 plant_inlet_flow→tank_1→tank_2 逐级传递。
- 优雅停止：stop 后工作线程回收、进程退出码 0（CLI-03 的 stdin stop 路径）。

### 4. 边界说明（未验证项）

- 真实引擎上的外部写入端到端：当前 `df-runtime` CLI 无写入命令；`submit_writes` 的确认/原子拒绝/队列容量/停止拒绝语义由 df-runtime 单元测试（FakeEngine）覆盖。真实写入闭环验收在 R8/R9 执行。
- 多实例、OPC UA、Host 配置：尚未实现（对应 R2–R10）。

### 5. 环境修复记录

- 现象：`cargo test` 全量链接失败，`x86_64-w64-mingw32-gcc ... cannot find -lgcc_eh`。
- 根因：`G:\github\supcon_tools\review3\datafactory-next` 目录存在 rustup 目录覆盖（stable-x86_64-pc-windows-gnu），该工具链绑定 TDM-GCC 10.3 链接器，缺少 libgcc_eh。
- 处理：`rustup override unset`（在该目录执行），恢复默认 stable-x86_64-pc-windows-msvc（VS2022 BuildTools 已安装）。

## 2026-11-02：R1 验证（RuntimeHandle / Snapshot revision / 写入请求状态）

范围：`crates/df-runtime` 新增 `src/runtime_handle.rs`，修改 `src/lib.rs`（R1 接线）。

### 1. 格式与工作区测试

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0）

命令: cargo test --workspace
结果: 通过（退出码 0，无警告）

test result: ok. 19 passed; 0 failed  — df-core 单元测试
test result: ok. 3 passed; 0 failed   — df-core Rust SDK 集成测试
test result: ok. 17 passed; 0 failed  — df-runtime 单元测试（原 8 + R1 新增 9）
test result: ok. 3 passed; 0 failed   — df-sdk 单元测试
合计: 42 passed / 0 failed
```

### 2. R1 新增测试清单（df-runtime 内）

- `handle_keeps_observing_after_owner_is_moved_away`：Owner（`Runtime`）被移动并释放后，克隆的 `RuntimeHandle` 仍可读取快照推进；revision 等待返回 `Stopped`；`submit_writes` 返回 `WorkerStopped`。
- `snapshot_revision_wait_reports_changed_and_timeout`：新周期发布后 `wait_for_snapshot` 返回 `Changed{revision}`；慢引擎（2.0s 周期）在 150ms 窗口内返回 `Timeout`。
- `write_lifecycle_cancelled_while_queued`：排队批次可 `cancel_write` → `Cancelled`；重复取消返回 `false`；调度线程在周期边界跳过该批次并回执 `WriteCancelled`，模型周期照常执行；未知 id 查询为 `None`、取消为 `false`。
- `write_lifecycle_applied_with_receipt`：批次 `Queued→Applied`，`write_state` 记录 tag_count 与 `WriteReceipt`（生效周期与仿真时间），且与 ticket 等待结果一致。
- `write_lifecycle_rejected_records_the_error`：非法批次（未知 tag）整批原子拒绝 → `Rejected`，错误文本含引擎码 1201，无回执。
- `queue_depth_tracks_pending_batches_and_stop_rejects_them`：两批排队时 `queue_depth()==2`（含 `status().queue_depth`）；`stop()` 后归零，排队批次状态置 `Rejected`，ticket 等待返回 `WorkerStopped`。
- `queue_full_rejection_is_queryable`：容量 1 的队列，第二个批次即时 `QueueFull` 失败且在登记表中记录为 `Rejected`。
- `status_exposes_revision_start_time_and_lag`：`snapshot_revision`、`last_cycle_started_at`、`last_cycle_lag`、`queue_depth` 出现在状态中。
- `registry_prunes_oldest_terminal_records`：登记表超过容量（1024）后按最老终态记录修剪；未终态记录不会被驱逐（由实现保证，容量修剪仅选取终态）。

### 3. Windows 真实引擎端到端复验（two-tank）

```text
运行: target\debug\df-runtime.exe run <two-tank.dfproject>（R1 改动后重新构建）
status#1 (≈0.8s): cycle_index=1 simulation_time=0.5s last_cycle=96.2µs overruns=0
status#2 (≈2.8s): cycle_index=5 simulation_time=2.5s last_cycle=60.8µs max_cycle=96.2µs overruns=0 missed_deadlines=0
snapshot: tank_1.level=0.1767405686760015 tank_2.level=0.10568507436488177（与首轮基线逐位一致）
stop → "runtime stopped"，退出码 0
```

### 4. 边界说明（未验证项）

- `RuntimeHandle` 目前没有真实外部消费者；OPC UA/HTTP 适配层（R5–R8）接入后，通过真实 OPC UA 客户端验证快照订阅与写入提交路径。
- `WriteRegistry` 容量修剪测试仅覆盖终态记录修剪路径；「排队中批次不被驱逐」由实现结构保证（修剪仅从终态记录中选取），未单独构造并发场景测试。

## 2026-11-02：R2 验证（Runtime Host 配置 / 严格解析 / 路径解析 / 项目加载）

范围：`crates/df-runtime` 新增 `src/config.rs`、`src/loader.rs`、`tests/host_config.rs`、`examples/host/runtime.yaml`；`Runtime::load` 改为经由统一加载器。

### 1. 格式与工作区测试

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0）

命令: cargo test --workspace
结果: 通过（退出码 0，无警告）

test result: ok. 19 passed; 0 failed  — df-core 单元测试
test result: ok. 3 passed; 0 failed   — df-core Rust SDK 集成测试
test result: ok. 41 passed; 0 failed  — df-runtime 单元测试（R1 后 17 + R2 新增 24）
test result: ok. 2 passed; 0 failed   — df-runtime host_config 集成测试
test result: ok. 3 passed; 0 failed   — df-sdk 单元测试
合计: 68 passed / 0 failed
```

### 2. R2 新增测试清单（df-runtime）

config.rs（17 个单元测试）：

- 有效配置解析：默认值（enabled=true、overrun_policy=delay、write_queue_capacity=256）与显式值共存；项目路径解析为配置目录下的绝对路径。
- 严格性：未知字段、重复实例 ID、非法 ID（空/含空格/超 64 字符）、runtime_version=2、write_queue_capacity=0、instances 为空均被拒绝并给出明确错误。
- 路径解析（CFG-03）：相对路径以配置文件目录为基准（测试工作目录与配置目录不同），绝对路径保留；缺失项目文件、不支持的扩展名（.txt）被拒绝。
- OPC UA：enabled=true 时 endpoint_url 必须 opc.tcp（http 被拒）；SignAndEncrypt+Basic256Sha256+证书/私钥/trust_list 相对路径解析且校验存在；Sign 模式缺证书被拒；mode 非 None 缺 policy 被拒；enabled=false 保留结构不做端点校验。
- 端点矩阵：`opc.tcp://127.0.0.1:4840`、`opc.tcp://localhost`、`opc.tcp://[::1]:4840` 合法；`opc.tcp://0.0.0.0:0`、`http://...`、`opc.tcp://`、`opc.tcp://host:notaport` 非法。
- 缺失配置文件报 `cannot read host config`。

loader.rs（7 个单元测试）：YAML 编译加载并经实时调度推进 2 个周期；.dfproject 加载；.txt 与无扩展名拒绝；损坏 YAML 报编译错误；损坏 dfproject 报引擎错误；缺失文件报读取错误。

tests/host_config.rs（2 个集成测试）：`examples/host/runtime.yaml` 可加载（CFG-01 语义），双实例 tank-a/tank-b 路径绝对且存在，策略 delay/catch_up 正确；两实例项目均可编译为 Engine（cycle_time=0.5s）。

### 3. Windows 真实引擎端到端（YAML 直接启动）

```text
运行: target\debug\df-runtime.exe run examples\two-tank\project.yaml（不经 df-engine 预编译）
status (≈2s): cycle_index=4 simulation_time=2s last_cycle=76.4µs max_cycle=96.1µs overruns=0 missed_deadlines=0
stop → "runtime stopped"，退出码 0
```

`.dfproject` 旧命令（CLI-02）路径不变：`Runtime::load` 经统一加载器后仍走 `Engine::load`，由 loader 单元测试与 R1 e2e 覆盖。

### 4. 边界说明（未验证项）

- `df-runtime check --config` / `run --config` CLI 与多实例启动在 R3/R4 接线后做端到端验收；本阶段配置校验仅以库 API + 单元/集成测试验证。
- OPC UA 安全配置仅做结构与文件存在性校验；证书生成、信任链与「非回环网卡不得使用不安全配置」在 R5 落地。

## 2026-11-02：R3 验证（RuntimeManager 多实例生命周期与故障隔离）

范围：`crates/df-runtime` 新增 `src/instance.rs`、`src/manager.rs`。

### 1. 格式与工作区测试

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0）

命令: cargo test --workspace
结果: 通过（退出码 0，无警告）

test result: ok. 19 passed; 0 failed  — df-core 单元测试
test result: ok. 3 passed; 0 failed   — df-core Rust SDK 集成测试
test result: ok. 50 passed; 0 failed  — df-runtime 单元测试（R2 后 41 + R3 新增 9）
test result: ok. 2 passed; 0 failed   — df-runtime host_config 集成测试
test result: ok. 3 passed; 0 failed   — df-sdk 单元测试
合计: 77 passed / 0 failed
```

### 2. R3 新增测试清单（manager.rs，真实 two-tank 工程文件夹具）

- `start_all_isolates_failures_and_skips_disabled`：四实例配置（tank-a、tank-b、broken=损坏 YAML、off=禁用）；start_all 仅 broken 失败（错误含 "cannot compile"），tank-a/tank-b Running，broken Failed 带错误记录，off 保持 Configured。
- `instances_keep_independent_engines_and_schedulers`：两实例同项目独立推进；停止 tank-a 后 tank-b 周期继续；tank-a handle 提交写入返回 WorkerStopped，tank-b 写入 Applied——引擎/队列/调度互不共享。
- `restart_resets_the_engine_to_initial_values`：重启前液位推进至 >0.155，重启后 1 个周期内回到 0.15–0.157 初始区间，cycle_index 归零——新 Engine、初始值、不续旧状态。
- `double_start_is_rejected_and_double_stop_is_idempotent`：重复启动报 "already running"；重复停止幂等；Stopped 后可 restart 回 Running。
- `unknown_instance_ids_are_reported`：start/stop/get 未知 ID 均报 "unknown instance id"。
- `shutdown_stops_everything_and_handles_report_stopped`：shutdown 后全部 Stopped（broken 保持 Failed、off 保持 Configured）；旧 handle `wait_for_snapshot` 返回 Stopped、提交写入 WorkerStopped；shutdown 可重复。
- `summaries_expose_live_metrics`：InstanceSummary 的 cycle_index/simulation_time/snapshot_revision/queue_depth/error/project 字段与实际运行一致。
- `manager_loads_the_shipped_example_host_config`：从 `examples/host/runtime.yaml` 加载双实例并完整走 start→shutdown。
- `manager_config_load_errors_are_reported`：配置文件缺失报 "cannot read host config"。

### 3. 边界说明（未验证项）

- 多实例 CLI 入口（`run --config`、stdin 多实例控制台、退出码 2/3）在 R4 接线后做端到端验收。

## 2026-11-02：R4 验证（CLI check / run --config / version / 退出码）

范围：`crates/df-runtime` 新增 `src/cli.rs`（bin 模块），重写 `src/main.rs`；`df-core` 导出 `PROJECT_FORMAT_VERSION`/`ENGINE_ABI_VERSION`。

### 1. 格式与工作区测试

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0）

命令: cargo test --workspace
结果: 通过（退出码 0，无警告）

test result: ok. 19 passed; 0 failed  — df-core 单元测试
test result: ok. 3 passed; 0 failed   — df-core Rust SDK 集成测试
test result: ok. 51 passed; 0 failed  — df-runtime 单元测试（R3 后 50 + R4 新增 1 个 config 顺序测试）
test result: ok. 2 passed; 0 failed   — df-runtime host_config 集成测试
test result: ok. 2 passed; 0 failed   — df-runtime bin cli 参数解析测试
test result: ok. 3 passed; 0 failed   — df-sdk 单元测试
合计: 80 passed / 0 failed
```

### 2. R4 新增测试

- config.rs `structural_errors_are_reported_before_path_errors`：两个实例重复 ID 且项目文件均缺失时，错误先报 `duplicate instance id`（CFG-02 不被路径错误掩盖）。
- cli.rs `parses_all_command_forms` / `rejects_incomplete_arguments_with_usage`：run/check --config/run --config/version 全部形式与 9 组非法参数（含 `run --config` 缺值不误判为单项目名）。

### 3. Windows 端到端（真实 CLI 进程）

```text
version:        df-runtime 0.1.0 / project format version 3 / engine ABI version 1（exit 0）
check (CFG-01): examples\host\runtime.yaml → 列出 tank-a/tank-b 绝对项目路径 [ok]（exit 0）
check (CFG-03): 分别在仓库根与 examples 目录用相对配置路径执行，均按配置目录解析（exit 0）
check (CFG-02): 重复 ID（即使项目文件缺失）→ "duplicate instance id \"same\""（exit 2）
check (exit 3): 存在但损坏的 YAML → 实例标 [FAILED: cannot compile project source ...]（exit 3）
bad args:       usage 输出（exit 2）

run --config（双实例会话）:
  tank-a/tank-b 同时 Running，2 秒各推进 4 周期，revision=4，overruns=0
  status tank-a / snapshot tank-b 正常输出
  stop tank-a → tank-a Stopped；tank-b 继续推进至 cycle_index=5（故障隔离）
  未知命令 → stderr 警告，会话继续
  stop → "runtime host stopped"（exit 0）

run <project.dfproject>（CLI-02 兼容）: 4 周期 / 2s，status 含 queue_depth/revision，stop 退出 0
run --config（全部启用实例启动失败）: "all enabled instances failed to start"（exit 3）
```

### 4. 边界说明（未验证项）

- 退出码 4（OPC UA 启动失败）在 R5 接入 UA Server 后验证；退出码 5（运行中不可恢复错误）的触发路径已实现（stop 时检查 Failed 状态），但未构造真实引擎致命错误场景做端到端触发。
- Ctrl+C 信号路径依赖控制台环境，当前以 stdin 关闭与 stop 命令覆盖优雅停止语义（CLI-03 的两条可达路径）。

- 注册表锁边界（§5.2「模型执行不持有注册表锁」）由实现结构保证（锁只在查找/簿记时持有，加载与执行在锁外），未构造并发压力场景专项测试。



