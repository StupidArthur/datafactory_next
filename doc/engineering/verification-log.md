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

