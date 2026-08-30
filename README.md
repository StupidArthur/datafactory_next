# DataFactory Next

当前目录包含 Rust Engine、Project 编译器、Rust/Python SDK，以及独立的 DataFactory Studio 桌面应用。

Rust Compiler 只接受正式 `dsl_version: 2`。当前 Engine 已支持经典模板使用的 `Variable`、`Expression`、`Lag`、`SINE_WAVE`、`LIST_WAVE`、`RANDOM`、`VALVE`、`CYLINDRICAL_TANK` 和 `PID`。

## Studio 能力

- 选择工作目录并列出 DSL v2 YAML 文件；
- 从磁盘重新读取 DSL，实时校验并展示 Project/Tag 特征；
- 编译 DSL 并启动独立 Rust Engine 调试进程；
- 配置执行周期和首周期写值；
- 查看调试趋势并导出 CSV；
- Studio 不编辑 DSL，不包含 Runtime、OPC UA 或生产历史库。

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
