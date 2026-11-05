# DataFactory Next

当前目录包含 Rust Engine、Project 编译器、Rust/Python SDK，以及独立的 DataFactory Studio 桌面应用。

Rust Compiler 只接受正式 `dsl_version: 2`。当前 Engine 已支持经典模板使用的 `Variable`、`Expression`、`Lag`、`SINE_WAVE`、`LIST_WAVE`、`RANDOM`、`VALVE`、`CYLINDRICAL_TANK` 和 `PID`。

## Studio 能力

- 选择工作目录并列出 DSL v2 YAML 文件；
- 从磁盘重新读取 DSL，实时校验并展示 Project/Tag 特征；
- 编译 DSL 并启动独立 Rust Engine 调试进程；
- 配置执行周期和首周期写值；
- 查看调试趋势并导出 CSV；
- 当前 Studio 不提供 DSL 编辑器。独立 Rust Runtime、OPC UA Server、双实例与双水箱 PID 闭环已有代码及专项测试；完整安全、性能、长稳与生产历史库仍未全部验收。

## 整体规划与模块分工

DataFactory Next 的整体规划包含经典组态仿真、离线数据生成，以及持续实时运行并通过 OPC UA 向外部系统提供模拟数据的在线 mock 能力。Engine、Studio 与 Runtime/OPC UA 主链路已有实现。现阶段重点是完善一致性、安全、客户端与长稳验证；规划能力与验证通过的能力必须分别表述。

| 模块 | 职责 | 当前状态 |
| --- | --- | --- |
| Engine / Compiler / SDK | 统一 DSL v2 编译、模型计算、周期推进、写值与快照，为 Studio 和 Runtime 提供共同执行核心 | 已有实现，支持当前经典模板；后续按 Runtime 需求扩展 |
| Studio | 组态校验、调试、离线仿真、趋势查看与数据导出 | 已有桌面应用，是当前开发重点 |
| Runtime | 加载组态、管理多实例、按真实时间持续调度 Engine，并管理生命周期 | 已实现；性能及长稳验收未完成 |
| OPC UA 接入 | 将 Tag 映射为 OPC UA 节点，提供真实客户端读写及闭环 | 已实现并有专项验证；全部客户端及安全验收进行中 |
| Runtime Monitor 与运行管理 | 查看实时状态和趋势，执行受控写入、强制和质量码操作，并提供报警、历史归档与查询 | 后续实现 |

这些能力可以分模块、分阶段交付；Studio 与 Runtime 可以采用不同应用入口或部署方式，共用同一套 DSL 和 Engine 计算语义。模块拆分不要求所有能力都放进 Studio EXE，最终打包方式随实现和部署需求确定。

后续工作优先围绕 Runtime/OPC UA 的数值一致性、安全、客户端与长稳证据收口；Monitor、报警和历史管理保留为后续规划。上表是整体功能规划，具体实施顺序与验收标准在对应模块开发时细化，暂未承诺交付日期。

操作与验收说明见 [Runtime / OPC UA 使用与验收说明](doc/product/runtime-opcua-guide.md)。相关设计背景见 [Rust Engine 设计稿](doc/rust-engine-design.md)、[Go 与前端交互设计稿](doc/go-frontend-interaction-design.md) 和 [Studio 第二阶段说明](doc/studio-backend-phase2.md)。设计稿保留了不同阶段的架构设想；其中的 Python 服务、HTTP 等技术方案需要结合当前 Rust Engine 与 Go Studio 实现继续收敛，不代表已落地能力。

## 构建 Studio

```powershell
cargo build --release -p df-engine
wails build -clean
Copy-Item target/release/df-engine.exe build/bin/df-engine.exe
```

最终运行目录需要同时包含：

```text
build/bin/datafactory-studio.exe
build/bin/df-engine.exe
```

## 开发运行

```powershell
$env:DF_ENGINE_PATH = "$PWD/target/debug/df-engine.exe"
wails dev
```

## 验证

```powershell
cargo test --workspace
go test ./...
npm run build --prefix frontend
```

真实 Studio/Engine 闭环测试：

```powershell
$env:DF_ENGINE_PATH = "$PWD/target/release/df-engine.exe"
$env:DF_STUDIO_WORKSPACE = "$PWD/examples/two-tank"
go test ./studio/application -run TestStudioRealEngineWorkflow -v
```

Engine、Rust SDK 和 Python SDK 的 100% 生产代码物理可执行行覆盖率门禁仍由以下脚本执行：

```powershell
.\scripts\coverage-rust.cmd
.\scripts\coverage-python.cmd
```

Engine 不读取墙上时间。每次 `execute` 只按照 Project 中的 `cycle_time` 推进模型物理时间。
