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





