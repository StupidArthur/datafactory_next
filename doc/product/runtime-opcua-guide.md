# DataFactory Runtime / OPC UA 操作与验收说明

> 2026-11-05；基于现有源码及本轮收口 PR。新增测试只有 CI 通过后才能标为“已验证”。不构成生产现场安全认证。

## 1. 系统边界与运行语义

| 模式 | 宿主 | 时间语义 | 用途 |
|---|---|---|---|
| Fast / Offline | Studio / df-engine | 每次 Engine 调用推进 cycle_time，不等待实际时间 | 数据集、趋势、组态校验 |
| Realtime | 独立 df-runtime | 根据墙上时间调度同一 df-core Engine | 多实例实时运行、OPC UA 供数与闭环 |

两种模式共享 DSL v2、Compiler、Engine 和组件计算实现。Realtime 不通过回放离线 CSV 模拟控制。外部写入按实例进入有界 FIFO 队列，在周期边界生效，回执含 effective_cycle_index。

## 2. 构建、检查与运行

在仓库根目录执行（PowerShell）：

    cargo build --locked --release -p df-engine -p df-runtime --features df-runtime/opcua
    cargo run --locked -p df-runtime -- check --config examples/host/runtime.yaml
    cargo run --locked -p df-runtime -- run --config examples/host/runtime.yaml

Host 标准输入支持 status [id]、snapshot <id>、stop [id] 等命令。示例 Host 配置默认 opcua.enabled=false；启用前需自行配置网络访问、身份与证书。相对项目路径以 Host YAML 所在目录为基准。

## 3. OPC UA 安全和地址空间

推荐加密连接 SignAndEncrypt + Basic256Sha256，并使用受控用户身份、PKI 信任目录；密码在环境变量中提供。配置片段（需准备对应目录和证书）：

    opcua:
      enabled: true
      endpoint_url: "opc.tcp://127.0.0.1:4840"
      application_uri: "urn:datafactory:runtime"
      namespace_uri: "urn:datafactory:runtime:ns"
      security:
        mode: SignAndEncrypt
        policy: Basic256Sha256
        allow_anonymous: false
        auto_trust_client_certs: false
        server_certificate: "./cert/server.der"
        server_private_key: "./cert/server-key.pem"
        trust_list: "./pki"
        users:
          - username: engineer
            password_env: DF_RUNTIME_ENGINEER_PW
            role: writer

客户端必须信任服务端证书；服务端必须信任客户端应用证书。默认拒绝未受信客户端。auto_trust_client_certs=true 仅允许 loopback 自动化测试，严禁将其用于非 loopback endpoint。

地址空间结构：Objects / DataFactory / ServerStatus；Objects / DataFactory / Instances / 实例ID / Status、Tags。每个 Tag 有独立的 NodeId、数据类型、读写标记，可配置工程单位。

## 4. 写入语义与限制

- 仅 writable Tag + Writer 身份可写。
- 单实例同批次整体校验与提交；跨实例各批次独立提交，不承诺跨实例事务原子性。
- 每周期最多处理一个待写批次；客户端超时并不等于未生效。
- Runtime 内部存在 WriteStatusInfo / WriteReceipt，但客户端写入结果查询接口尚未交付。
- Monitor UI、报警、生产历史归档、工业现场 PID 整定和 SIL 类认证不在当前已实现范围。

## 5. 可执行验收

| 方向 | 命令 | 证据边界 |
|---|---|---|
| Rust + Engine | cargo test --locked --workspace | 默认工作区测试 |
| Fast/Realtime 对照 | cargo test --locked -p df-runtime --test fast_realtime_equivalence | 同 YAML、同写入序列、按生效周期对照 Tag |
| OPC UA 全套 | cargo test --locked -p df-runtime --features opcua --tests | Browse、四种标量、订阅、身份、未受信客户端、闭环 |
| Go Studio | go test ./... | 先 npm ci --prefix frontend && npm run build --prefix frontend，以生成 embedded dist |
| 前端构建 | npm run build --prefix frontend | TypeScript 与 Vite 构建 |
| 多实例长稳 | cargo test --locked -p df-runtime --test runtime_soak -- --ignored --nocapture | 手动运行，不能由 CI 代替 |

30 分钟长稳：设置环境变量 DF_SOAK_DURATION_SECS=1800、DF_SOAK_INSTANCES=2、DF_SOAK_OUTPUT=<CSV 路径>，然后执行上表的 ignored 测试。

CSV 只覆盖周期、耗时、overrun、missed_deadlines、队列深度等 Runtime 指标；**不能**据此宣称 CPU/RSS、P95/P99、OPC UA 客户端延迟或容量上限。完整容量报告必须记录硬件、OS、编译配置、实例数、Tag/Node 数、周期、客户端数，并同步采集系统和客户端侧指标。

## 6. 证据来源

- 设计：doc/runtime-system-design.md
- 代码与证据映射：doc/engineering/runtime-opcua-evidence-map.md
- 原始验证台账：doc/engineering/verification-log.md
- 双水箱 PID 案例：examples/two-tank/README.md

不要把新加但尚未 CI 通过的测试描述成已验收结果。