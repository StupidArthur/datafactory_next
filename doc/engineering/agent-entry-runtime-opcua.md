# Runtime / OPC UA 下一轮 Agent 入口

> 本文是下一轮 Agent 的执行入口。目标是完成第一轮可运行、可复现的专利实施验证，不是把 Runtime 做成完整工业产品。  
> 基线 Commit：`2dcba94687f53f0bf0d7e84e4aed5fdba071593a`（`main`，2026-11-02）

## 1. 本轮目标与边界

先完成 R8 的必要修正和验收，再做最小 R9 闭环验证。只实现证明核心技术路线所需的功能：

- Runtime 管理多个独立实例。
- OPC UA 客户端读取实例 Tag。
- OPC UA 写入经过 Runtime 写入队列，在 Engine 周期边界生效。
- 能用自动化测试证明写入、模型响应和实例隔离。

**不做产品化扩展**：本轮不实现 Monitor、报警、历史归档、Windows Service、通用诊断平台、额外协议、性能压测或复杂运维能力；不重构现有架构，不增加非必要依赖。

## 2. 当前代码与验收状态

- R1–R8 代码已在上述 Commit 中。R8 尚未正式验收，不要把文档中的本地测试记录等同于 CI 通过。
- 当前 GitHub Actions：  
  [Rust CI #41399825148](https://github.com/StupidArthur/datafactory_next/actions/runs/41399825148)
  - Ubuntu：通过。
  - Windows：失败，失败发生在 `df-runtime` 的 3 个配置路径测试。
- Windows 失败的测试：
  - `config::tests::relative_paths_resolve_against_config_dir_not_cwd`
  - `config::tests::opcua_cert_paths_resolve_against_config_dir`
  - `config::tests::valid_config_uses_declared_and_default_values`
- 已知断言差异：实际路径包含 `./`，期望路径经过 Windows 短路径形式（例如 `RUNNER~1`）；失败表现为路径字符串表示不同。修复时要保证“相对路径始终以配置文件目录为基准”，不能简单删除测试、忽略断言或改变路径基准。
- 当前 CI 工作流执行默认 `cargo test --workspace --locked`，没有启用 `opcua` feature。R8 的 OPC UA feature 本地验证记录在 `doc/engineering/verification-log.md`，但不要把它说成已由当前 CI 覆盖。

## 3. 立即处理：R8 最小修正

### R8-A：修复 Windows 路径测试

1. 检查 `crates/df-runtime/src/config.rs` 的路径解析和三个失败测试。
2. 修复路径规范化或测试比较方式，使其正确处理 Windows 的 `.` 路径段、大小写和短/长路径表示。
3. 保留相对路径以配置文件目录为基准的行为。
4. 不允许通过跳过测试、平台屏蔽或放宽到“不检查路径”的方式消除失败。

### R8-B：修正多实例请求的批次边界

设计依据：`doc/runtime-system-design.md` §8.3。**原子性以实例为边界，不以整个 OPC UA Write 请求为边界。**

期望行为：

- 同一实例的一组 Tag 写入合并为一个批次；该实例内任一 Tag 无效，则该实例的合法写入也不得部分生效。
- 一个 OPC UA 请求包含多个实例时，各实例独立处理；实例 A 的无效写入不能阻止实例 B 的合法批次提交。
- 不承诺跨实例事务原子性。
- 响应状态应能区分具体无效项、同实例批次中未提交的合法项，以及其他实例独立批次的结果。

只调整必要的分组、校验和提交逻辑；不要引入通用事务框架。

至少增加一个有代表性的 OPC UA 集成测试：同一个请求覆盖两个实例，实例 A 内包含一项非法写入，实例 B 包含合法写入；验证 A 不发生部分应用、B 仍能在 Engine 周期边界生效。测试可复用现有 `opcua_write.rs` fixture 和客户端辅助函数。

### R8-C：写入状态查询的本轮边界

Runtime 内部已有 `WriteTicket::request_id()`、`RuntimeHandle::write_state(request_id)`。第一轮不新增 OPC UA 诊断节点、查询服务或独立诊断 API，也不为此扩展配置模型。

- 保留现有 Runtime 内部请求状态能力。
- OPC UA 正常写入仍以 Engine 确认后返回 Good 为准。
- `BadTimeout` 仍表示结果不确定，不能解释为写入必然失败；本轮测试应使用合理周期和等待时间验证正常闭环。
- 将“客户端通过 OPC UA 查询超时写入最终状态”明确记录为后续增强，不作为本轮 R8/R9 的阻塞项。

## 4. R8 通过后：最小 R9 闭环

只做一个可复现的双水箱代表性场景，不扩展成通用测试平台：

1. 启动两个独立 Runtime 实例，加载双水箱项目。
2. 使用独立 OPC UA Client Session 连接服务端并读取两实例的 Tag。
3. 向实例 A 的一个可写输入写入合法值。
4. 验证写服务在 Engine 确认生效后返回 Good；随后读取到该输入值，并观察到双水箱模型的相关输出发生预期变化。
5. 验证实例 B 的对应状态不受实例 A 写入影响。
6. 停止服务和实例，确认测试资源被正常释放。

优先复用现有测试 fixture、客户端函数和双水箱项目。若现有 R8 集成测试已经覆盖某一项，只补缺失证据，不重复造测试框架。无需额外启动 Studio，也无需构建完整外部进程编排系统。

## 5. 测试策略：短、小、针对性

研发效率优先。先运行单个失败测试，再运行相关小范围测试；不要反复运行全量测试。

建议命令：

```bash
cargo fmt --all -- --check

# 在 Windows 上逐个复验当前失败项
cargo test -p df-runtime --lib config::tests::relative_paths_resolve_against_config_dir_not_cwd
cargo test -p df-runtime --lib config::tests::opcua_cert_paths_resolve_against_config_dir
cargo test -p df-runtime --lib config::tests::valid_config_uses_declared_and_default_values

# 只运行新增/修改的 OPC UA 集成测试；将 <test_name> 替换为实际测试名
cargo test -p df-runtime --features opcua --test opcua_write <test_name>

# 代码完成后，全工作区默认测试只运行一次
cargo test --workspace --locked
```

约束：

- 不运行约 62 秒的完整 OPC UA feature 测试电池，除非用户明确要求。
- 不增加 Ubuntu/Windows 双平台全量 OPC UA feature 测试矩阵。
- 不做长时间压力测试、性能基准或重复全量回归。
- 最终至少确认当前 Windows 默认 CI 失败项已修复，并用一个聚焦 OPC UA 集成测试验证多实例批次边界和闭环关键路径。

## 6. 完成标准与交付

满足以下条件才报告 R8/R9 本轮完成：

- 三个 Windows 路径测试通过，且相对路径基准语义未改变。
- 多实例写入按实例独立处理；同实例批次仍保持原子性。
- 聚焦的 OPC UA 双实例测试通过，能证明实例 A 写入、模型响应和实例 B 隔离。
- 当前默认工作区 CI 在 Ubuntu、Windows 均通过。注意：当前 CI 仅执行默认 feature 的 `cargo test --workspace --locked`，没有启用 `opcua` feature；因此它只能证明默认构建/测试通过，不能证明 R8-B 的 OPC UA 集成测试已被 CI 覆盖。R8-B 的聚焦 OPC UA 测试结果必须单独标注为本地验证证据。
- `TODO.md` 和 `verification-log.md` 只记录实际执行并通过的项目；未验证项继续标为未完成或明确标注边界。
- 提交范围只包含必要代码、测试和验证记录；不顺手整理无关模块。

完成后向用户汇报：改动文件、关键行为、实际执行的短测试命令及结果、Commit SHA、尚未实现的边界。**不要仅凭本地口头总结宣布验收通过，也不要在 R8 未通过前开始 R9。**

## 7. 必读文件（只读相关章节）

- `doc/runtime-system-design.md`：§8.3–8.5、§9.1–9.3、§12.2、§14.5。
- `doc/TODO.md`：R8/R9 进度及实施记录。
- `doc/engineering/verification-log.md`：R8 验证记录。
- `crates/df-runtime/src/config.rs`：路径解析与配置测试。
- `crates/df-runtime/src/opcua/write.rs`：OPC UA 写入校验、分组和提交。
- `crates/df-runtime/tests/opcua_write.rs`：现有真实客户端集成测试。

本入口文档优先约束本轮工作范围：以最小实现形成可复现的专利实施证据，不把后续产品化能力提前塞进本轮。
