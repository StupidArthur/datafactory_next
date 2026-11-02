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
