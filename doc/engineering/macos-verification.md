# macOS 适配与实机验收记录（2026-11-05）

## 兼容性边界

本改动只涉及 Studio 对 Rust Engine 可执行文件的定位、macOS 原生 .app 的 Engine 打包和 Apple Silicon / Intel CI。模型计算与 Runtime/OPC UA 的语义不变。

- engine_path.go：优先使用 DF_ENGINE_PATH；否则在 Studio 执行文件同级查找 df-engine（Windows 是 df-engine.exe）。
- scripts/build-studio-macos.sh：编译本机 Engine → Wails v2 build → 安装 sidecar Engine 到 Contents/MacOS → 本地 ad-hoc 签名与校验。
- .github/workflows/macos-ci.yml：macos-15 (ARM64) / macos-15-intel (x86_64) 验证 Rust、Go、前端、Wails 打包和 OPC UA 集成。

ad-hoc 签名不等于 Developer ID 签名或苹果公证；不是已签名可公开分发的产品。每台 Mac 本机生成对应架构应用，并不是 Universal App。

## 环境检查

安装 Xcode Command Line Tools、Rust stable、Go（依 go.mod）、Node 22、Wails v2 CLI。

    xcode-select -p
    rustc --version
    cargo --version
    go version
    node --version
    go install github.com/wailsapp/wails/v2/cmd/wails@v2.12.0
    export PATH="$(go env GOPATH)/bin:$PATH"
    wails version

## 本地验证

### Rust Engine / Runtime

    cargo build --locked -p df-engine -p df-runtime
    cargo test --workspace --locked
    cargo run --locked -p df-runtime -- run examples/two-tank/project.yaml

检查周期序号、仿真时间与正常终止。

### OPC UA

    cargo test --locked -p df-runtime --features opcua --tests

此处验证集成测试使用的真实 server/client；仍需在目标 Mac 上使用独立外部客户端检查加密身份、订阅、写入、PID 双水箱闭环。

### Studio Dev

    cargo build --locked -p df-engine
    export DF_ENGINE_PATH="$PWD/target/debug/df-engine"
    wails dev

在 Studio 中选择 examples/two-tank，完成组态校验、仿真与 CSV 导出。

### Studio 原生应用包

    bash scripts/build-studio-macos.sh
    test -x build/bin/datafactory-studio.app/Contents/MacOS/df-engine
    codesign --verify --deep --strict build/bin/datafactory-studio.app
    open build/bin/datafactory-studio.app

注意：从 Finder 启动时不要依赖开发模式的 DF_ENGINE_PATH；必须验证 .app 内同级 df-engine 能自动被找到。

## 实测记录模板

| 项目 | 结果 |
|---|---|
| 机器型号、架构与 macOS | 待填写 |
| Rust / Go / Node / Wails 版本 | 待填写 |
| 验证 Commit | 待填写 |
| Rust workspace tests | 待填写 |
| OPC UA feature tests | 待填写 |
| Studio dev 与 CSV 导出 | 待填写 |
| .app 打包 / 签名 / Finder 启动 | 待填写 |
| 外部 OPC UA Client 闭环 | 待填写 |
| 失败日志和修正记录 | 待填写 |

只有完成目标 Mac 的实际设备测试后，才能标注 macOS 实机验收完成。
