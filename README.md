# DataFactory Next

当前目录包含 Rust Engine、Project 编译器、Rust/Python SDK，以及独立的 DataFactory Studio 桌面应用。

Rust Compiler 只接受正式 `dsl_version: 2`。当前 Engine 已支持经典模板使用的 `Variable`、`Expression`、`Lag`、`SINE_WAVE`、`LIST_WAVE`、`RANDOM`、`VALVE`、`CYLINDRICAL_TANK` 和 `PID`。

## Studio 能力

- 选择工作目录并列出 DSL v2 YAML 文件；
- 从磁盘重新读取 DSL，实时校验并展示 Project/Tag 特征；
- 编译 DSL 并启动独立 Rust Engine 调试进程；
- 配置执行周期和首周期写值；
- 查看调试趋势并导出 CSV；
- 当前 Studio 不提供 DSL 编辑器；Runtime、OPC UA 和生产历史库尚未实现，属于下述整体规划。

## 整体规划与模块分工

DataFactory Next 的整体规划包含经典组态仿真、离线数据生成，以及持续实时运行并通过 OPC UA 向外部系统提供模拟数据的在线 mock 能力。当前优先交付 Engine 与 Studio；在线 mock、Runtime 和相关运行管理能力均在整体设计范围内，后续按模块实现。

| 模块 | 职责 | 当前状态 |
| --- | --- | --- |
| Engine / Compiler / SDK | 统一 DSL v2 编译、模型计算、周期推进、写值与快照，为 Studio 和 Runtime 提供共同执行核心 | 已有实现，支持当前经典模板；后续按 Runtime 需求扩展 |
| Studio | 组态校验、调试、离线仿真、趋势查看与数据导出 | 已有桌面应用，是当前开发重点 |
| Runtime | 加载组态、管理多模板实例、按真实时间持续调度 Engine，并管理运行生命周期 | 后续实现 |
| OPC UA 接入 | 将运行位号映射为 OPC UA 节点，向外部系统提供模拟数据读写，实现在线 mock | 后续实现 |
| Runtime Monitor 与运行管理 | 查看实时状态和趋势，执行受控写入、强制和质量码操作，并提供报警、历史归档与查询 | 后续实现 |

这些能力可以分模块、分阶段交付；Studio 与 Runtime 可以采用不同应用入口或部署方式，共用同一套 DSL 和 Engine 计算语义。模块拆分不要求所有能力都放进 Studio EXE，最终打包方式随实现和部署需求确定。

后续工作围绕 Runtime 实时运行、OPC UA 在线 mock，以及 Monitor、报警和历史管理逐步展开。上表是整体功能规划，具体实施顺序与验收标准在对应模块开发时细化，暂未承诺交付日期。

相关设计背景见 [Rust Engine 设计稿](doc/rust-engine-design.md)、[Go 与前端交互设计稿](doc/go-frontend-interaction-design.md) 和 [Studio 第二阶段说明](doc/studio-backend-phase2.md)。设计稿保留了不同阶段的架构设想；其中的 Python 服务、HTTP 等技术方案需要结合当前 Rust Engine 与 Go Studio 实现继续收敛，不代表已落地能力。

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
