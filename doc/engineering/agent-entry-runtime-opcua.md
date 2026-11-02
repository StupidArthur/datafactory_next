# Runtime / OPC UA 已实现基线与后续工作入口

> 本文用于下一轮 Agent 接续工作。Runtime 多实例、OPC UA 写入和 R9 双实例闭环与 PID 设定值闭环补充实施例已有可复现工程证据；后续重点转为双时间对照、客户端验收补齐和产品/专利材料整理，不默认继续扩展 Runtime 功能。  
> Runtime / OPC UA 代码实现基线 Commit：`456723421b58c927bda421d40f36210eaf4c72a2`（PR #3 已合并，包含 PID 闭环补充实施例）；代码/测试源提交：`ddec2a2ece80c89ec7ec5130d9761107a9a20aa6`。  
> 第一版技术证据索引：`doc/engineering/runtime-opcua-evidence-map.md`。

## 1. 第一轮完成状态

- R8-A：Windows 路径规范化已修复。相对路径仍以 Host 配置文件目录为基准，去除拼接结果中的 `.` 路径组件；短名 / 长名 Windows 临时目录均完成本地复验。
- R8-B：OPC UA Write Handler 已按实例分组。同一实例内任一已知 Tag 写入非法，该实例合法项整批不提交；其他实例的合法批次独立提交。跨实例事务原子性不作承诺。
- R9：完成双水箱双实例最小闭环。两个独立 Engine / Runtime、一个 SignAndEncrypt Server、两个独立 Client Session；A 的 plant_inlet_flow 写入经 Engine 确认后驱动 tank_1.level 变化；B 的输入和 Engine 快照保持独立。
- PID 闭环补充实施例：新增外部 `level_setpoint` → PID → 阀门 → 双水箱 → PID PV 工程和真实 OPC UA Client 测试；上调/下调设定值时，PID 输出、阀门开度与出口流量按预期响应，两个水箱液位变化。
- 代码、测试和实际验证记录以 `doc/engineering/verification-log.md` 的 R8–R9 与 PID 闭环补充实施例条目为准；专利技术特征—代码—测试—实施例映射见 `doc/engineering/runtime-opcua-evidence-map.md`。

## 2. CI 与本地验证边界

默认工作区 CI：

- [R8-A / R8-B 默认 CI #41417657194](https://github.com/StupidArthur/datafactory_next/actions/runs/41417657194)
- [R8 修正记录 CI #41418883892](https://github.com/StupidArthur/datafactory_next/actions/runs/41418883892)
- [R9 记录 Commit CI #41419403497](https://github.com/StupidArthur/datafactory_next/actions/runs/41419403497)
- [PID 闭环补充实施例默认工作区 CI #41437587420](https://github.com/StupidArthur/datafactory_next/actions/runs/41437587420)
- [PID 闭环补充实施例 Windows OPC UA 专项 CI #41437587512](https://github.com/StupidArthur/datafactory_next/actions/runs/41437587512)

默认 `.github/workflows/rust-ci.yml` 在 Ubuntu / Windows 执行格式检查、Engine 构建和 `cargo test --workspace --locked`，不启用 `opcua` feature。新增的 `.github/workflows/runtime-opcua-pid-e2e.yml` 只在 Windows 上执行一个指定的 OPC UA PID 集成测试。

必须区分：

- R8-A Windows 路径修复：有双平台默认 CI 证据。
- R8-B OPC UA 跨实例批次测试：Windows 本地 feature 测试证据，不是 CI 覆盖。
- R9 双实例闭环：Windows 本地 feature 测试证据，不是 CI 覆盖。
- PID 设定值闭环补充实施例：Windows 单项 feature CI 已通过；不代表完整 OPC UA feature 测试电池或 Ubuntu feature 覆盖。

R9 的聚焦命令：

```bash
cargo fmt --all -- --check
cargo test -p df-runtime --features opcua --test opcua_write
```

结果：6 passed / 0 failed。

PID 闭环补充实施例的聚焦命令：

```bash
cargo test --locked -p df-runtime --features opcua --test opcua_write opcua_pid_setpoint_controls_valve_and_two_tank_levels -- --exact --nocapture
```

结果：1 passed / 0 failed（24.86 s，Windows）。专项 CI 先检查目标测试存在，再执行该测试。R8-B 和 R9 的本地 feature 结果不得写成 CI 覆盖；PID 补充实施例的单项 Windows CI 也不得写成完整双平台 OPC UA CI。

## 3. 后续工作目标

优先整理证据和产品说明，不为“看起来完整”而扩展 Runtime：

1. **Fast / Realtime 对照。** 对同一 YAML、初始条件、输入序列和 cycle_index 比较 Fast/Offline 与 Realtime 的模型值，明确数值容差；没有结果前不能宣称两种模式已完成一致性验证。
2. **OPC UA 客户端验收补齐。** 独立客户端 Browse（UA-02）、BOOL / INT64 / FLOAT64 / STRING 完整类型矩阵（UA-05）、未受信客户端证书拒绝（SEC-03）。
3. **CL-03 需求决策。** Runtime 内部已有 WriteReceipt / effective_cycle_index；客户端查询接口尚不存在。只有明确用户场景和安全边界后再设计 OPC UA 诊断查询，不要直接把内部状态暴露出去。
4. **性能与长稳证据。** 先确定目标机器、实例数、Tag / Node 数和周期，再采集 CPU、内存、周期耗时、overrun、P95 / P99；没有实测数据前不声明容量上限。
5. **产品文档。** 以已实现代码和证据为准编写 Runtime / OPC UA 产品说明、双水箱案例说明；把已实现、部分验证、规划能力分开描述。

## 4. 当前不能宣称完成的事项

- Fast / Offline 与 Realtime 同输入序列的数值一致性专门对照。
- PID 阶跃后的液位收敛时间、稳态误差、长期稳定性和实物整定结果。
- OPC UA 客户端查询写入最终状态 / effective_cycle_index（CL-03）。
- 完整 Browse 层级（UA-02）和完整客户端类型矩阵（UA-05）。
- OPC UA 标准 Write 请求的取消入口；当前取消能力仅在 Runtime 内部 Queued 阶段。
- 完整 PKI 未受信证书拒绝测试（SEC-03）。
- 30 分钟长稳、目标规模性能与受支持实例容量声明。
- 完整 OPC UA feature 测试电池的 Ubuntu / Windows CI 覆盖（目前只有 Windows 单项 PID 测试）。

## 5. 必读文件

- `doc/engineering/runtime-opcua-evidence-map.md`：第一版技术特征、代码、测试、实施例和证据边界映射。
- `doc/runtime-system-design.md`：设计基线和验收编号；尤其 §6–§9、§14、§17。
- `doc/engineering/verification-log.md`：R1–R9 与 PID 闭环补充实施例的实际验证命令、结果和未覆盖项。
- `doc/TODO.md`：专利证据与产品化任务状态。
- `crates/df-runtime/src/` 与 `crates/df-runtime/tests/`：以当前基线代码和测试为准。

**执行约束：** 不把设计目标写成已实现事实；区分 R8-B/R9 本地 feature 证据与 PID 补充实施例 Windows 单项 feature CI；不把 PID 阶跃响应扩大描述为液位已收敛或实物整定完成；不先做 Monitor、报警、历史归档等非核心扩展。
