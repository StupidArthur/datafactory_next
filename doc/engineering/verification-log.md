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

## 2026-11-02：R5 验证（OPC UA 库 PoC / Endpoint 与安全配置 / §9.1）

范围：`crates/df-runtime` 新增 `src/opcua/mod.rs`（`opcua` feature 门控）与 `tests/opcua_poc.rs`（required-features 门控）；`config.rs` 新增 §9.1 安全校验与 URI 约束（`validate_opcua_security`/`endpoint_host`/`is_loopback_host`）；`Cargo.toml` 新增 `opcua` feature（async-opcua 0.19 + tokio + tokio-util）并注册 `opcua_poc` 测试目标；`src/lib.rs` feature 接线。

### 1. 格式与测试

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0）

命令: cargo test -p df-runtime --features opcua
结果: 全部目标 0 failed

test result: ok. 60 passed; 0 failed  — df-runtime 单元测试（默认 57 + opcua 模块 3）
test result: ok. 2 passed; 0 failed   — df-runtime bin cli 参数解析测试
test result: ok. 2 passed; 0 failed   — df-runtime host_config 集成测试
test result: ok. 2 passed; 0 failed   — df-runtime opcua_poc 集成测试
小计: 66 passed / 0 failed

命令: cargo test --workspace（默认构建，无 opcua feature）
结果: 通过（退出码 0，无警告）

test result: ok. 19 passed; 0 failed  — df-core 单元测试
test result: ok. 3 passed; 0 failed   — df-core Rust SDK 集成测试
test result: ok. 57 passed; 0 failed  — df-runtime 单元测试（R4 后 51 + R5 新增 6 个 config 测试）
test result: ok. 2 passed; 0 failed   — df-runtime host_config 集成测试
test result: ok. 2 passed; 0 failed   — df-runtime bin cli 参数解析测试
test result: ok. 3 passed; 0 failed   — df-sdk 单元测试
合计: 86 passed / 0 failed
```

### 2. R5 PoC 覆盖点（tests/opcua_poc.rs，2 个集成测试）

- `anonymous_poc_connect_read_write_and_subscription`：匿名 + SecurityPolicy None 连接回环端点；读取 Counter 初始 0.0 与 Echo 动态读回调 42.0；主机线程 `set_value`（SimpleNodeManager + SubscriptionCache，即 R7 发布路径）后客户端读到 7.0；客户端订阅（100ms 发布间隔）收到 set_value 的 9.0 通知；客户端写 WritableValue 返回 Good、被写回调捕获并由读回调回读 3.5；`stop()` 幂等且停止后端口可重新绑定。
- `secure_channel_with_username_password_auth`：SignAndEncrypt + Basic256Sha256，证书/私钥路径缺省时由服务器在配置路径生成样例密钥对（测试断言文件生成）；`start_with_users` 注册 engineer 用户；正确口令会话激活并通过加密通道读到 Echo=42.0；错误口令会话在 3 秒超时内无法激活。

### 3. §9.1 配置策略与库行为约束（config.rs + opcua/mod.rs，新增 6 个 config 测试 + 3 个 opcua 模块测试）

- endpoint_url serde 缺省 `opc.tcp://127.0.0.1:4840`（`endpoint_url_defaults_to_loopback`）；`endpoint_url_validation_matrix`/`loopback_host_matrix` 覆盖 IPv6 字面量、路径、端口缺省等解析。
- 非回环端点（0.0.0.0 / 192.168.1.10 / [2001:db8::1]）+ mode/policy None → 拒绝（`non_loopback_unsecured_endpoint_is_rejected`）；+ allow_anonymous → 拒绝（`non_loopback_anonymous_endpoint_is_rejected`）；+ SignAndEncrypt/Basic256Sha256/非匿名 → 接受（`non_loopback_secure_non_anonymous_endpoint_is_accepted`）。`0.0.0.0` 刻意不视为回环。
- 库约束发现并固化为校验：async-opcua 0.19 内置诊断节点管理器在 build 时以 application_uri 注册命名空间并独占其 owns_node 路由，application_uri == namespace_uri 时自有节点读取返回 value=None → `validate_opcua_security` 强制二者不同（`opcua_uris_must_differ`，禁用段跳过）；设计文档 §4.1/§7.3、`examples/host/runtime.yaml` 与全部测试夹具同步拆分（namespace 用 `urn:datafactory:runtime:ns`）。
- 校验在配置加载与 `OpcUaServer::start` 两处共用：程序化构造且 enabled 的非回环配置在 start 处同样被拒（`start_rejects_disabled_and_insecure_config`）。

### 4. 服务器生命周期与依赖隔离

- `OpcUaServer::start`：专用 OS 线程 + current_thread tokio runtime（ServerBuilder/服务器 future 非 Send）；服务器 build 后先绑定 tokio TcpListener 再发送 ready——端口占用（专项测试 `start_fails_cleanly_when_the_port_is_taken`）在启动阶段同步报 "failed to start"，而非探测超时；ready 后宿主线程 TCP 探测确认端点可接受连接；`run_with` 内部监听取消 token，`stop()` 取消 + join 线程，Drop 兜底调用。
- 实现约束记录：VariableBuilder 必须显式 `.data_type(DataTypeId::Double)`（0.19 不从 builder value 推断，缺失时 insert 即 panic "The node is not valid"）；0.19 写回调拦截后不落盘，须配对读回调回写状态；`wait_for_connection` 在激活失败时挂起，负向认证测试必须用 timeout 包裹；默认 user_access_level 仅 CURRENT_READ，可写节点须显式 `CURRENT_READ | CURRENT_WRITE`。
- 依赖隔离（§9.2）：`cargo tree -p df-runtime`（默认）无 async-opcua/tokio/tokio-util；`--features opcua` 时三者全部出现——默认构建零 OPC UA 依赖成立。

### 5. 边界说明（未验证项）

- PoC 地址空间（DataFactory/PoC/{Counter,Echo,WritableValue}）为临时验证节点；Tag Catalog → NodeId 映射（R6）、快照周期发布（R7）、UA 写处理器 → `RuntimeHandle::submit_writes` 与 allow_anonymous 认证器及用户配置（R8）随后接入。
- 退出码 4（OPC UA 启动失败）的 CLI 接线在 R8；绑定失败检测路径已就绪（ready 阶段同步报错）。
- `default_pki_dir`（临时目录 + pid + 序号）仅为无证书配置的样例密钥对落盘目录；正式部署应显式配置证书路径与 trust_list，生产信任链验证归 R8 部署配置。
- 环境说明：G: 盘构建空间不足（曾到 0 空闲），`cargo clean` 回收 4.7 GB 后全部 cargo 命令以 `CARGO_TARGET_DIR=D:\cargo_target_df` 在 D: 盘构建（会话环境变量，未改仓库配置）。

## 2026-11-02：R6 验证（Tag Catalog → NodeId/Variant/AccessLevel 映射）

范围：`crates/df-runtime` 新增 `src/opcua/value.rs`（§7.3/§7.4 类型与 NodeId 映射）与 `src/opcua/address_space.rs`（§7.2 地址空间构建与索引）；`src/opcua/mod.rs` 重构（`start_with_catalogs` 接入 catalog、移除 PoC 节点、新增 `address_space()` 访问器）；`tests/opcua_poc.rs` 重写为 4 个 catalog 驱动集成测试；`src/lib.rs` 增加 `#![allow(unexpected_cfgs)]`。

### 1. 格式与测试

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0）

命令: cargo test -p df-runtime --features opcua
结果: 全部目标 0 failed

test result: ok. 67 passed; 0 failed  — df-runtime 单元测试（默认 57 + R5 opcua 模块 3 + R6 新增 7：value.rs 5 + address_space.rs 2）
test result: ok. 2 passed; 0 failed   — df-runtime bin cli 参数解析测试
test result: ok. 2 passed; 0 failed   — df-runtime host_config 集成测试
test result: ok. 4 passed; 0 failed   — df-runtime opcua_poc 集成测试（R5 的 2 个重写为 catalog 驱动 4 个）
小计: 75 passed / 0 failed

命令: cargo test --workspace（默认构建，无 opcua feature）
结果: 通过（退出码 0，无警告）
合计: 86 passed / 0 failed（df-core 19 + rust_sdk 3 + df-runtime 57 + host_config 2 + cli 2 + df-sdk 3）

命令: cargo tree -p df-runtime（默认）
结果: 无 async-opcua/tokio/tokio-util；--features opcua 时三者全部出现（§9.2 隔离复验）
```

### 2. R6 映射覆盖点（value.rs，5 个单元测试）

- `data_type_matrix_matches_the_design_table`：BOOL→Boolean、INT64→Int64、FLOAT64→Double、STRING→String（§7.4）；未知（FLOAT32）、小写、空串类型名一律拒绝，不猜测。
- `value_variant_round_trip_preserves_the_type`：四种类型 value→variant→value 严格往返。
- `variant_to_value_rejects_type_mismatches_and_empty`：数值近似类型（Double→INT64、Int64→FLOAT64）、跨类（Boolean/UAString）、`Variant::Empty`、未知 tag 类型全部返回 None，不静默转换（WR-03 的映射层基础）。
- `percent_encoding_keeps_unreserved_and_escapes_the_rest`：RFC 3986 unreserved（字母/数字/-/`.`/`_`/`~`）直通；`/`→%2F（杜绝路径歧义）、空格→%20、中文按 UTF-8 字节大写 %XX；编码可逆。
- `node_identifiers_follow_the_design_paths`：Tag 节点 `df/instances/{instance}/tags/{encoded_tag}`、Status 节点 `df/instances/{instance}/status/{name}`（§7.3）。
- STRING 映射实现说明：0.19 `UAString::as_ref()` 直接返回 `&str`（非 Option），null 串归一为 ""——映射据此返回 `Value::String("")` 而非拒绝。

### 3. 地址空间覆盖点（address_space.rs + 集成测试）

- `catalogs_reject_duplicates_and_unknown_types`：空实例 ID/空 Tag 名/重复实例 ID/重复 Tag 名/未知类型在插入任何节点前拒绝（§4.2 严格性，不留半建地址空间）。
- `status_initial_values_follow_the_node_types`：ServerStatus（State="Starting"、InstanceCount=实例数、RunningInstanceCount=0）与实例 Status 8 项（CycleTime/SimulationTime/LastCycleDurationMs 为 Double 归零，CycleIndex/OverrunCount/MissedDeadlines/RejectedWriteBatches 为 UInt64 归零）初值与类型一致。
- 集成 `catalog_address_space_reads_writes_and_subscriptions`（两水箱 catalog 双实例）：§7.2 骨架直读（ServerStatus "Starting"/2/0；实例 Status State/CycleTime/CycleIndex/RejectedWriteBatches 归零）；tank_1.level 初始 0.15（FLOAT64）；EngineeringUnit 属性读到 "m"（编译器 tank level 单位，UA-06 单位路径）；实例隔离 UA-04（tank-a/tank-b 同名 Tag NodeId 不同，set_value 后 tank-a=7.0 而 tank-b 保持 0.15）；主机线程 `set_value` 跨线程可读且客户端订阅（100ms）收到 9.0 更新（R7 发布路径预验证）；客户端写可写 Tag plant_inlet_flow=0.002 落盘回读（R6 为 SimpleNodeManager 默认存储）；客户端写只读 Tag 返回 BadUserAccessDenied（库按 AccessLevel 位拒绝；WR-02 设计值 BadNotWritable 的最终语义在 R8 写处理器接入时决断）；stop 幂等且端口释放。
- 集成 `node_ids_are_stable_across_restarts`：同一实例重启后全部 Tag 节点与 Status 节点 NodeId 逐一相同（UA-03）。
- 集成 `tag_metadata_description_and_unit_property`：合成 catalog（unit="m"、description="water level"、initial=1.0）——初值、Description 属性（OPC UA Description 读取返回 LocalizedText 而非 String）、EngineeringUnit 属性均正确（UA-06）。
- 集成 `secure_channel_with_username_password_auth`：catalog 地址空间下 SignAndEncrypt + Basic256Sha256 + 用户名口令复验（正确口令经加密通道读 tank_1.level=0.15；错误口令 3 秒内无法激活）。

### 4. 实现与构建说明

- `build_address_space` 在服务器线程内、`run_with` 之前执行：add_folder/VariableBuilder 均走 `&mut AddressSpace` 写锁；任一节点插入失败即整体启动失败（Err 上抛，不留残缺空间）。`AddressSpaceIndex`（instance/tag → NodeId/类型/可写性索引）经 `OpcUaServer::address_space()` 暴露，供 R7 发布器与 R8 写处理器使用。
- Tag/Status 节点 NodeId 为字符串标识 `ns={namespace_uri 索引}; s=df/...`，不含动态命名空间号；实例 ID 受 Host 配置校验约束（字母/数字/下划线/连字符），仅 Tag 名需 percent-encode。
- df-runtime/src/lib.rs 增加 `#![allow(unexpected_cfgs)]`（与 df-core/lib.rs 既有做法一致）：`#[cfg_attr(coverage, coverage(off))]` 的 `coverage` cfg 仅由 nightly 覆盖率工具定义，stable 下 inert 但会触发 lint。

### 5. 边界说明（未验证项）

- UA-02（独立客户端 Browse DataFactory/Instances 层级）与客户端侧完整 UA-05 类型矩阵（BOOL/INT64 直读）归 R9 闭环自动化测试；R6 已直读覆盖 FLOAT64/String/UInt32/UInt64，BOOL/INT64 变体映射由 value.rs 单元测试覆盖。
- Status 节点在 R6 保持初值（"Starting"/0）：实时值写入归 R7 Snapshot 发布器；写路径当前为地址空间默认存储，与 Engine 的闭环（`RuntimeHandle::submit_writes`）归 R8。
- percent-encode 仅由单元测试覆盖：编译器 Tag 名限制为 `[A-Za-z0-9_.]`，实际项目不含需转义字符（设计如此，编码路径为未来宽松命名预留）。

## 2026-11-02：R7 验证（Snapshot 周期发布与订阅通知；含 R1 停机协议顺序修复）

范围：`crates/df-runtime` 新增 `src/opcua/publisher.rs`（§7.5 发布器）与 `tests/opcua_publisher.rs`（2 个集成测试）；`Cargo.toml` 注册 `opcua_publisher` 测试目标；`src/opcua/mod.rs` 接线并更新模块文档；`src/lib.rs` 修复 R1 停机协议顺序并新增回归测试。

### 1. 格式与测试

```text
命令: cargo fmt --all -- --check
结果: 通过（退出码 0）

命令: cargo test -p df-runtime --features opcua
结果: 全部目标 0 failed

test result: ok. 72 passed; 0 failed  — df-runtime 单元测试（R6 的 67 + publisher 4 + R1 停机回归 1）
test result: ok. 2 passed; 0 failed   — df-runtime bin cli 参数解析测试
test result: ok. 2 passed; 0 failed   — df-runtime host_config 集成测试
test result: ok. 4 passed; 0 failed   — df-runtime opcua_poc 集成测试
test result: ok. 2 passed; 0 failed   — df-runtime opcua_publisher 集成测试（新增）
小计: 82 passed / 0 failed

命令: cargo test --workspace（默认构建，无 opcua feature）
结果: 通过（退出码 0，无警告）
合计: 87 passed / 0 failed（df-core 19 + rust_sdk 3 + df-runtime 58 + host_config 2 + cli 2 + df-sdk 3）

命令: cargo tree -p df-runtime（默认）
结果: 无 async-opcua/tokio/tokio-util；--features opcua 时三者全部出现（§9.2 隔离复验）
```

### 2. 发布器实现要点（publisher.rs，§7.5）

- 每实例一个 OS 线程阻塞在 `RuntimeHandle::wait_for_snapshot(last_revision, refresh)`：attach 后立即发布当前快照；`Changed` 分支随 cycle 发布；`Timeout` 分支仅在 RuntimeStatus 变化时重发布（默认刷新间隔 `DEFAULT_STATUS_REFRESH_INTERVAL = 200ms`）；`Stopped` 分支做最终发布后退出。
- 批量一致性（UA-08）：标签值 + 8 项实例 Status 节点合并为一次 `SimpleNodeManager::set_values`（整个批次持一把地址空间写锁），客户端不会读到两个 cycle 的混合值。0.19 `set_values` 无 index_range 时经 `set_data_value` 原样落盘（status/source_timestamp 保留）——这是 BadWaitingForInitialData/BadOutOfService 与 UA-09 源时间戳能到达客户端的关键。
- 共享服务器状态线程：200ms 轮询聚合 `ServerStatus/{State,InstanceCount,RunningInstanceCount}`，仅在样本变化时写入。
- §7.5 状态码映射（每实例统一，无逐 tag 质量层）：Starting→BadWaitingForInitialData、Running/Stopping→Good、Stopped/Failed→BadOutOfService；State 字符串用生命周期名；host_state 聚合优先级：任一 Failed→"Failed"、全部终态→"Stopped"、任一 Running→"Running"、任一 Stopping→"Stopping"、空集→"Stopped"、否则→"Starting"。
- `SnapshotPublisher::start` 校验：未知实例（地址空间未含其 catalog）与重复实例一律拒绝（Config 错误，不留半启动线程）；空实例集允许（宿主可无启用实例）；`stop()` 幂等且 join 全部线程，Drop 兜底。
- 慢消费者不反压引擎：发布线程只读已发布快照，通知排队/过滤归服务器订阅机制。

### 3. 停机协议顺序修复（R1 缺陷，根因 + 回归）

- 现象：`publisher_streams_cycles_status_and_marks_out_of_service_on_stop` 单独运行稳定、并行复跑复现失败（stop 后等不到 BadOutOfService）。
- 根因（临时探针定位：宿主侧地址空间直读 + 客户端读 + 发布线程埋点；探针已删除）：`Runtime::stop()` 在 `request_stop` 后**立即** `feed.mark_stopped()`，而 worker 线程要到 join 完成才把状态置为 Stopped——发布线程被 `WaitOutcome::Stopped` 唤醒时 `status()` 仍是 Running，最终发布以 Good 落盘（探针输出 `STOPPED final publish (state=Running)`），BadOutOfService 永远不会出现。
- 修复（协议：`WaitOutcome::Stopped` 必须保证后续观察到终态）：
  - `Runtime::stop()` 先 `worker.join()` 再 `feed.mark_stopped()`；join 失败（worker panic）路径先把状态置 Failed 再 mark。
  - worker 致命引擎错误路径：先 `update_status(Failed)` 再 `feed.mark_stopped()`。
  - 发布器 `Stopped` 分支增加有界防御性重同步 `wait_for_terminal_status`（终态或 5s 超时；正常路径 join 后立即命中，0ms）。
- 回归测试 `waiter_woken_by_stop_observes_a_terminal_status`（lib.rs）：并发 waiter 阻塞在 feed 上，owner stop 后被 Stopped 唤醒时必须读到 Stopped/Failed。
- 修复后验证：探针 5/5（最终发布 state=Stopped，客户端 ~1ms 内读到 BadOutOfService）；opcua_publisher 5/5 稳定。

### 4. 覆盖点（真实 engine 0.5s 周期 + 独立客户端）

- 集成 `publisher_streams_cycles_status_and_marks_out_of_service_on_stop`：State→"Running" 与 ServerStatus 聚合（Running/1/1）；UA-07 订阅 tank_1.level 收到 ≥3 个不同值（引擎每 cycle 水位上升）；实例 Status 节点 CycleTime==0.5、CycleIndex≥3、SimulationTime≥1.5、LastCycleDurationMs≥0；UA-09 标签读带近期 UTC source_timestamp；`runtime.stop()` 后标签 BadOutOfService 且最后一个快照值保留、实例 State→"Stopped"、ServerStatus State→"Stopped"、RunningInstanceCount→0；publisher.stop() 幂等；端口释放。
- 集成 `publisher_rejects_unknown_and_duplicate_instances`：未知实例报 "unknown opcua publisher instance"；重复 tank-a 报 "duplicate"；空实例集允许启动后干净停止。
- publisher 单元测试（4）：State 字符串跟随生命周期名、§7.5 状态码映射、host_state 聚合矩阵（空集/全 Starting/Running 混合/Stopping 混合/Stopped/Failed 组合）、status_values 的 §7.2 名称与类型（CycleTime 0.5→Double、CycleIndex 7→UInt64、LastCycleDurationMs 250µs→0.25 等）。

### 5. 边界说明（未验证项）

- UA-02（独立客户端 Browse DataFactory/Instances 层级）与完整 UA-05 客户端类型矩阵归 R9 闭环测试。
- Starting 窗口的 BadWaitingForInitialData 未做客户端验证：映射由单元测试覆盖，集成测试在实例 Running 后接入（两水箱启动不足一个 cycle，客户端可观测窗口极窄）。
- ServerStatus State 聚合语义较简单（按优先级聚合，无部分降级/未知态概念），与 §7.2 设计一致，但客户端侧只断言了 Running/Stopped 两个聚合值。
- 停机瞬间订阅是否推送最终 BadOutOfService 通知未显式断言（验证的是读路径；通知路径由服务器订阅机制保证）。
- LastCycleDurationMs 仅断言 ≥0，未校验数值合理性。
- 环境说明：全部 cargo 命令以 `CARGO_TARGET_DIR=D:\cargo_target_df` 在 D: 盘构建（会话环境变量，未改仓库配置）。

## 2026-11-02：R8 验证（OPC UA Write Handler / 批次聚合 / 确认与取消）

### 1. 形式验证（Windows / rustc 1.99.1 msvc）

```
命令: cargo fmt --all -- --check
结果: 通过（无格式差异）

命令: cargo test -p df-runtime --features opcua
结果: 通过（退出码 0，全套约 62 秒）

test result: ok. 84 passed — df-runtime lib（默认 62 = R7 的 58 + config R8 新增 4；opcua 模块门控 22 = R7 的 14 + write.rs R8 新增 8）
test result: ok. 2 passed  — df-runtime bin cli 参数解析测试
test result: ok. 2 passed  — df-runtime host_config 集成测试
test result: ok. 4 passed  — df-runtime opcua_poc 集成测试
test result: ok. 2 passed  — df-runtime opcua_publisher 集成测试
test result: ok. 4 passed  — df-runtime opcua_write 集成测试（R8 新增）
小计: 98 passed / 0 failed

命令: cargo test --workspace（默认构建，无 opcua feature）
结果: 通过（退出码 0，无警告）
19（df-core）+ 3（rust_sdk）+ 62（df-runtime lib）+ 2（host_config）+ 2（cli）+ 3（df-sdk）= 91 passed / 0 failed

命令: cargo tree -p df-runtime；cargo tree -p df-runtime --features opcua
结果: 默认树无 async-opcua/tokio（零 OPC UA 依赖）；feature 树 async-opcua/tokio/async-trait 全部出现（§9.2 复验通过）
```

### 2. 写处理器实现要点（opcua/write.rs，8 个单元测试）

- 自定义 NodeManager：`pub type RuntimeNodeManager = InMemoryNodeManager<RuntimeNodeManagerImpl>`——只覆盖 `write`，init/browse/读路径继承 async-opcua 内置 InMemoryNodeManager；`RuntimeNodeManagerImpl` 与 `runtime_node_manager()` 公开供宿主复用。发布器与写处理器共用同一 `AddressSpaceIndex`，NodeId ↔ Tag 目录一一对应。
- `WriteHandlerState` 持有每实例 Tag 条目（字符串标识 → writable/DataType + RuntimeHandle）并实现 §8.4 状态映射（所有拒绝不入队）：
  - 未知节点 / 非字符串 NodeId 标识 → `BadNodeIdUnknown`（WR-04）
  - host-owned：实例与服务器 Status 节点、非 Value 属性 → `BadNotWritable`
  - index_range ≠ None → `BadIndexRangeInvalid`（Tag 模型无数组语义）
  - DataValue 无 value → `BadNothingToDo`
  - writable=false → `BadNotWritable`（WR-02，先于角色判定——只读语义与权限语义分离）
  - 身份角色 ≠ Writer（匿名视为 Reader）→ `BadUserAccessDenied`（SEC-04）
  - Variant 与目录 DataType 严格匹配（FLOAT64↔Double / INT64↔Int64 / BOOL↔Boolean / STRING↔String，不转换）→ 否则 `BadTypeMismatch`（WR-03）
  - 校验全部通过 → 整批 `submit_writes` 入队，按周期边界回执回答（WR-01）；批次内任一节点被拒 → 整批不入队，有效节点答 `BadNothingToDo`（WR-05 原子性）
  - 实例无挂接 handle → `BadOutOfService`；队列满 → `BadServerTooBusy` 不入队（WR-06）；等待回执超时 → `BadTimeout`（§8.5.4 结果不定，批次不撤销）
- 等待时长：`derive_write_wait` = 2×cycle_time + 2 s 余量，夹取 [2 s, 30 s]；`opcua.write_wait_timeout_ms` 配置覆盖优先（`write_wait_derives_from_cycle_time_and_clamps` / `wait_override_takes_precedence` 两个专项测试）。

### 3. 集成测试覆盖点（tests/opcua_write.rs，4 个合并场景）

环境：真实 two-tank engine + SignAndEncrypt/Basic256Sha256 服务端（配置用户 engineer=Writer、viewer=Reader，密码经环境变量注入，配置文件不含明文）+ 每场景独立证书目录（服务端样例密钥对 + 客户端样例密钥对 + `trust_server_certs(true)`）+ 用户名口令激活会话。

- `rejection_matrix_enforces_catalog_identity_and_atomicity`：WR-02 只读 tank_1.level → BadNotWritable；WR-03 String/Int64 写 FLOAT64 → BadTypeMismatch；index-range → BadIndexRangeInvalid；缺 value → BadNothingToDo；WR-04 未知节点 → BadNodeIdUnknown；WR-05 有效+无效混合批次 → 有效节点 BadNothingToDo、无效节点 BadNotWritable，且后续落点精确证明有效值未部分应用；SEC-04 viewer 写可写 Tag → BadUserAccessDenied；CycleIndex 状态节点写 → BadNotWritable。
- `writer_writes_apply_in_order_at_cycle_boundaries`：WR-01 写入 Good 且在引擎应用后经发布链回读 0.002（WR-07：引擎应用 → 快照 → 地址空间 → 客户端读）；模型闭环响应——tank_1.level 自初始 0.15 上升；WR-08 第二批 0.003 在第一批之后生效。
- `queue_full_rejects_and_timed_out_writes_still_apply`：capacity=1 + 500 ms 覆盖等待 + 2 s 周期（`wait_for_cycle_start` 同步到周期起点）：首批入队 → BadTimeout 但批次保留；第二批 → BadServerTooBusy 即时拒绝；queue_depth 保持 1；最终经发布链回读 0.002——§8.5.4 "超时 ≠ 未生效" 实证。
- `stopped_instances_reject_writes_and_pending_batches_are_discarded`：10 s 周期 + 300 ms 覆盖：提交 → BadTimeout；`runtime.stop()` 后快照仍为初值 0.0012（WR-09 待处理批次丢弃不应用）；后续写入 → BadOutOfService。

### 4. CLI 接线与 e2e（Windows 真实二进制 df-runtime.exe，4 条路径）

- check 预检（CFG-01 扩展）：`df-runtime check --config` 现在解析 users 的密码环境变量——未设置时退出 2 并指名变量名（实测 `DF_E2E_PW`）；设置后退出 0。
- 退出 4：run --config 下 OPC UA 端口被占 → 绑定失败在启动阶段同步报告（"opcua server failed to start: cannot bind ..."）→ shutdown 已启动实例 → 退出 4（R5 遗留未验证项闭环）。
- 退出 0：完整 host 路径——"opcua serving at" + 实例 Running + `status`/`stop` 控制台正常，优雅停止退出 0；二进制未启用 opcua feature 而 opcua.enabled=true 时打印提示不启动。
- 退出 5（首次真实触发）：致命工程为 DSL `type: Expression, formula: sqrt(base)`、`base: -1.0` → 首周期 sqrt(-1)=NaN → `eval_expression` 的 is_finite 检查拒绝 → 实例 Failed。`Runtime::start` 不同步跑首周期，故 start_all 返回 Running、崩溃发生在控制台循环内 → 检测到原 Running 实例 Failed → shutdown → 退出 5。致命工程（可复现）：

```yaml
dsl_version: 2
metadata: {name: fatal_expr, description: e2e fatal}
clock: {mode: GENERATOR, cycle_time: 0.2}
program:
  - name: base
    type: Variable
    value: -1.0
    writable: false
    display_args: []
  - name: boom
    type: Expression
    formula: sqrt(base)
    display_args: []
export: {default_columns: [boom]}
```

- e2e 脚本（PowerShell，延迟写 stdin 以赢得首周期竞态）为会话临时工件不入库；上述命令与结果均已内联记录，可按本节复现。

### 5. 测试规模控制记录（响应"测试过慢"反馈）

- 背景：初版 11 个单场景集成测试出现 ~15 分钟超时与挂起。定位出两个根因：① 每个测试独立生成 RSA 密钥对 + 全套启动，开销叠加；② 夹具 scratch 目录同名互删（`two_tank_with_cycle_time` 与 `start_fixture` 同名调用时 `remove_dir_all` 删除了工程文件，3 个测试连锁失败）。
- 处置：11 → 4 个合并场景；工程 YAML 写入独立 `*_project` 目录；`wait_for_cycle_start` 以 cycle_index 同步消除写提交跨越周期边界的竞态；`wait_for_connection` 以 15 s 超时包裹（客户端必须 `trust_server_certs(true)`，否则激活静默挂起——poc 已验证结论的再次确认）。曾尝试进程级共享密钥对路径，出现会话激活挂起，按"不引入未验证捷径"原则回退到 poc 验证过的每夹具独立证书模式。
- 结果：opcua_write 4/4 约 22–37 s，全 opcua 测试面 ~62 s，退出码 0。

### 6. 边界说明（未验证项）

- `BadOutOfService` 独立判定仅 Writer over SignAndEncrypt 可达（匿名/Reader 在 SEC-04 先被拒），由 write.rs 单元测试覆盖，未做独立客户端场景。
- 取消路径（§8.5.3 仅 Queued 可取消）：OPC UA 标准 Write 服务无取消语义，写处理器未暴露取消入口；`WriteTicket::cancel` 的 Queued-only 取消语义由 R1 单元测试覆盖。
- UA-02（独立客户端 Browse Instances/Tags 层级）与客户端侧完整 UA-05 类型矩阵验证归 R9 闭环测试。
- Starting 窗口 BadWaitingForInitialData、ServerStatus State 聚合等遗留边界沿用 R7 记录。
- 环境说明：全部 cargo 命令以 `CARGO_TARGET_DIR=D:\cargo_target_df` 在 D: 盘构建（会话环境变量，未改仓库配置）。

## 2026-11-02：R8 修正与验收（Windows 路径表示 / 实例级批次边界）

本轮修正 R8 的两个阻塞项：CI 上 Windows 的 3 个配置路径测试失败，以及多实例写入按整个请求而非按实例判原子性。R9 未在本轮开始。

### 1. 复现（Windows）

CI 原始断言（run 41399825148，windows-latest）：

```
left : C:\Users\runneradmin\AppData\Local\Temp\df_runtime_config_7656_opcua_certs_15\./cert/server.der
right: C:\Users\RUNNER~1\AppData\Local\Temp\df_runtime_config_7656_opcua_certs_15\cert/server.der
```

两类差异同时存在：`HostConfig::load` 会 canonicalize 配置文件路径，Windows 上把 8.3 短名（`RUNNER~1`）展开为长名（`runneradmin`），而测试期望来自 `std::env::temp_dir()` 的短名形式；另外拼接后残留 `./` 组件。

本地复现方法（本机 `Administrator` 目录没有 8.3 短名，故人为构造混合短名根）：

1. 新建目录 `...\Temp\opencode\df_repro_long_component_name`，用 `GetShortPathNameW` 取到其短名 `...\Temp\opencode\DF_REP~1`。
2. 设 `TMP`/`TEMP` 为该短名路径后运行 `cargo test -p df-runtime --lib config::tests`。

复现结果：与 CI 完全一致的 3 个测试失败、同样的两类断言差异（`df_repro_long_component_name` vs `DF_REP~1`、`\./`）。

```
test config::tests::relative_paths_resolve_against_config_dir_not_cwd ... FAILED
test config::tests::opcua_cert_paths_resolve_against_config_dir ... FAILED
test config::tests::valid_config_uses_declared_and_default_values ... FAILED
test result: FAILED. 25 passed; 3 failed
```

### 2. R8-A：路径解析修正

- `resolve_relative` 拼接后去掉 `.` 组件（`drop_cur_dir`），`project: ./x.yaml` 解析为 `<配置目录>\x.yaml`；`..` 保持原样——按字面消解 `..` 会在跨符号链接时改变语义。相对路径基准仍是配置文件所在目录，未改为进程 cwd。
- 测试期望改为从 canonicalize 后的临时目录构造，与加载器使用同一路径表示；并在 `relative_paths_resolve_against_config_dir_not_cwd` 新增“解析结果不含 `.` 组件”断言（属于加强而非放宽断言）。
- 验证（Windows 本地）：

```
cargo fmt --all -- --check                      → 通过
cargo test -p df-runtime --lib config::tests    → 28 passed / 0 failed
（短名 TMP=...\DF_REP~1 环境下复验）              → 28 passed / 0 failed
cargo test -p df-runtime                        → 62 + 2 + 2 passed / 0 failed
```

### 3. R8-B：实例级批次边界

- `WriteRejection` 新增 `instance_id`：目标为已知 Tag 时携带其实例 id；未知节点、host-owned 节点（Status/非 Value 属性）不携带，因此不否决任何批次。
- `handle_write` 改为：先逐项校验；任一实例的 Tag 被拒时，仅该实例的合法项置 `BadNothingToDo` 且整批不入队；其他实例各自提交并在自己的周期边界等待确认。跨实例事务原子性仍不承诺。
- 同实例原子性未变：`rejection_matrix_enforces_catalog_identity_and_atomicity` 中 A 内“合法+非法”混合批次仍整批拒绝。
- 新增集成测试 `multi_instance_request_vetoes_one_batch_without_blocking_the_other`（tests/opcua_write.rs）：一个真实 Write 请求覆盖 `tank-a` 合法写 + `tank-a` 只读 Tag + `tank-b` 合法写；断言 A 合法项 `BadNothingToDo`、A 非法项 `BadNotWritable`、B `Good`；随后经发布链读到 B=0.003，A 再跑 2 个周期后引擎快照仍为初值 0.0012，且 B 的值不泄漏到 A。
- 回退复验（证明测试确实覆盖缺陷）：`git stash push -- crates/df-runtime/src/opcua/write.rs` 后运行该测试，旧代码失败：

```
thread 'multi_instance_request_vetoes_one_batch_without_blocking_the_other' panicked at crates\df-runtime\tests\opcua_write.rs:785:5:
instance B's valid write must succeed independently, got BadNothingToDo
test result: FAILED. 0 passed; 1 failed
```

- 验证（Windows 本地）：

```
cargo test -p df-runtime --features opcua --lib opcua::write::tests  → 9 passed / 0 failed
cargo test -p df-runtime --features opcua --test opcua_write         → 5 passed / 0 failed（约 15 s）
cargo test --workspace --locked                                      → 91 passed / 0 failed
  （19 df-core + 3 rust_sdk + 62 df-runtime lib + 2 host_config + 2 cli + 3 df-sdk）
```

### 4. CI（默认 feature）

GitHub Actions [Rust CI #41417657194](https://github.com/StupidArthur/datafactory_next/actions/runs/41417657194)，commit `c545651`：

- ubuntu-latest：通过（42 s）。
- windows-latest：通过（1 m 34 s）；日志中 `config::tests::relative_paths_resolve_against_config_dir_not_cwd`、`config::tests::opcua_cert_paths_resolve_against_config_dir`、`config::tests::valid_config_uses_declared_and_default_values` 均为 `ok`。

### 5. 边界说明（本轮未覆盖）

- **CI 不覆盖 OPC UA feature**：工作流只执行默认 feature 的 `cargo test --workspace --locked`，未启用 `opcua`。因此上面 R8-B 的双实例集成测试属于**本地验证证据**，不能表述为“CI 已覆盖 R8-B”。Windows CI 只证明默认构建/测试（含 3 个路径测试）通过。
- 客户端通过 OPC UA 查询超时写入最终状态仍为后续增强；本轮未新增诊断节点/查询服务（R8-C 边界不变）。`BadTimeout` 依旧表示结果不确定。
- 本轮未运行约 62 秒的完整 `--features opcua` 测试电池，只运行了 `opcua::write::tests` 与 `tests/opcua_write.rs` 两个聚焦目标。
- 取消路径、UA-02 Browse、UA-05 客户端矩阵等边界沿用上文 R8 记录，归 R9 或后续轮次。





