# DataFactory macOS 实机验收报告（复制后填写）

> 来源：[macOS 实机验收 TODO](TODO-macos-acceptance.md) ｜状态：**待填写，不能视为验收通过**  
> 建议保存路径：work/reports/macos-YYYYMMDD-HHMMSS/REPORT.md  
> 所有测量结果必须来自本次实测，未执行填 NOT_RUN，失败填 FAIL，缺环境填 BLOCKED；不得删除失败行。

## 1. 测试信息

| 字段 | 实测填写 |
|---|---|
| 执行人 / 日期 / 时区 | |
| Mac 机型 | |
| CPU 架构（uname -m） | |
| macOS 版本 | |
| Rosetta / 原生 | |
| Rust / Cargo | |
| Go | |
| Node / npm | |
| Python | |
| Wails | |
| 代码仓库及分支 | main |
| Git 完整 SHA | |
| DF_TEST_OUT 证据目录 | |
| 独立 OPC UA Client（名称/版本） | |
| 本机是否接电 / 是否关闭自动睡眠 | |
| GitHub Actions 链接（仅作参考） | |

## 2. 验收状态汇总

PASS=完整通过，FAIL=真实执行失败，BLOCKED=外部条件导致无法完成，NOT_RUN=尚未执行。最后三种状态都不等于通过。

| 编号 | 测试项目 | 状态 | 关键测量 / 结果 | 证据文件相对路径 | 问题编号 |
|---|---|---|---|---|---|
| T00 | 系统环境与代码基线 | NOT_RUN | | | |
| T01 | Rust/React/Go 构建 | NOT_RUN | | | |
| T02 | Rust workspace + Python SDK | NOT_RUN | | | |
| T03 | Go 与真实 Engine 集成 | NOT_RUN | | | |
| T04 | Studio 开发模式 GUI | NOT_RUN | | | |
| T05 | 独立 .app / Finder 启动 | NOT_RUN | | | |
| T06 | Runtime 双实例 Host | NOT_RUN | | | |
| T07 | 完整 30 分钟双实例长稳 | NOT_RUN | | | |
| T08 | OPC UA Rust 全功能集成 | NOT_RUN | | | |
| T09 | 外部客户端 Browse/Read/Subscribe | NOT_RUN | | | |
| T10 | 加密 Writer/Reader 与 PID 闭环 | NOT_RUN | | | |

## 3. 关键命令及测试计数

| 命令 / 用例 | 返回码 | 运行数/通过/失败 | 实测耗时 | 原始日志 |
|---|---:|---|---|---|
| cargo build -p df-engine | | | | |
| cargo build -p df-runtime --features opcua | | | | |
| cargo test --workspace --locked | | | | |
| Python unittest discover | | | | |
| go test ./... | | | | |
| TestStudioRealEngineWorkflow（不可 SKIP） | | | | |
| cargo test --features opcua --tests | | | | |
| OPC UA PID 定向测试 | | | | |
| OPC UA 跨实例否决定向测试 | | | | |
| build-studio-macos.sh / codesign | | | | |

## 4. Studio GUI 验收记录

| 场景 | 使用的项目/参数 | 实际表现 | 截图/CSV | 判定 |
|---|---|---|---|---|
| Wails dev 启动 | | | | |
| 工作区与 Tag 检查 | examples/two-tank/project.yaml | | | |
| 离线 1000 周期 | 0.5s/周期，期望 500s | | | |
| 趋势数据 | tank_1.level、tank_2.level | | | |
| CSV 导出 | 列名/数据行数/样例 | | | |
| 打包 .app 启动 | unset DF_ENGINE_PATH | | | |
| Finder 重启、退出、子进程清理 | | | | |

## 5. 双实例 30 分钟长稳

- 请求时长：1800 秒；**实际连续运行时长**：
- Host 进程退出码：
- 是否中途睡眠/人工中断：
- tank-a 最初/最后 cycle_index：
- tank-b 最初/最后 cycle_index：
- tank-a 最后 simulation_time：
- tank-b 最后 simulation_time：
- 两实例周期是否全部严格递增：
- CPU 峰值（Host）：
- RSS 峰值（Host，MiB）：
- 热身后 RSS 首尾均值与趋势：
- 采样条数：
- soak-summary.json 中 gates：
- soak-summary.json 中 pass：
- 是否存在 panic / Failed / unexpected exit：
- 日志与 CSV 位置：

> 注意：脚本资源采样不是逐实例性能统计，也不是 P95/P99 时延测量。

## 6. 独立 OPC UA 客户端实测

- 客户端名称/版本：
- 端点：opc.tcp://127.0.0.1:4840
- T09：None / Anonymous 测试的浏览树、订阅、拒绝结果：
- T10：SignAndEncrypt / Basic256Sha256 / engineer 的会话结果：
- T10：viewer 能读不能写，具体 StatusCode：
- T10：只读 Tag 写入拒绝、未知 Tag、类型不匹配各返回码：
- 两实例 NodeId/读值隔离：
- 关闭/重启后地址空间标识符是否保持一致：
- 证据路径：

### PID 闭环数值表（填真实墙钟时间与测量值）

| 时刻 / 事件 | tank-a level_setpoint | pid2.MV | 阀门目标开度 | 阀门实际开度 | 出口流量 | tank_1.level | tank_2.level | tank-b level_setpoint | 返回码 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| 基线 | | | | | | | | | |
| 上调 0.40 后中间点 | | | | | | | | | |
| 上调观察 60 秒后 | | | | | | | | | |
| 下调 0.20 后中间点 | | | | | | | | | |
| 下调观察 60 秒后 | | | | | | | | | |
| 只读写入拒绝后 | | | | | | | | | |
| Reader 写入拒绝后 | | | | | | | | | |

### 重要事件与日志索引

| UTC/本地时刻 | 操作 | Instance/Tag | 请求值和类型 | 响应/StatusCode | 客户端截图、日志 |
|---|---|---|---|---|---|
| | | | | | |

## 7. 问题、原因、修复与复测

| 编号 | 失败现象与严重性 | 复现步骤/命令 | 日志/截图 | 归因 | 修复 commit 或建议 | 复测结果 |
|---|---|---|---|---|---|---|
| ISSUE-001 | | | | | | |

**任何 FAIL/BLOCKED 必须保留**，不能为了显示“全部通过”而删除该行。修复后应重跑受影响步骤；若 T07 被中断或脚本通过门槛失败，必须重新完整运行 1800 秒。

## 8. 结束核对与签字

- [ ] 全部 T00～T10 必测项目 PASS；
- [ ] 长稳 >=1800 秒且 soak-summary.json 的 pass=true；
- [ ] 外部独立 OPC UA Client 实际完成 T09/T10；
- [ ] Finder 启动的独立 .app 通过；无 DF_ENGINE_PATH 外部依赖；
- [ ] 所有命令、测试数量、证据链接均可复核；
- [ ] 已核查没有提交密码、token、证书私钥、账号隐私信息；
- [ ] 失败项处理完并复测成功，或明确标明 INCOMPLETE 不宣称验收完成。

**本次最终结论（选择其一）**：

- [ ] **PASS（全部必测通过）**
- [ ] **INCOMPLETE（仍有 FAIL/BLOCKED/NOT_RUN）**

执行人确认及时间：  
复核人确认及时间（可由维护者填写）：  
待解决问题与需要维护者处理的事项：

## 9. 回传要求

请将填写的 REPORT.md 和非敏感日志/CSV/截图，提交到仓库的 work/reports/对应日期目录（可以通过独立分支/PR 提交），并把完整 commit SHA/PR 链接发给维护者。

提交前逐一检查文件内容。不要使用未经审查的 git add -A 将仓库外证书私钥、终端历史、密码、token 或个人信息传上公开仓库。无法公开的敏感测试证据请单独通过安全渠道发送，报告只引用经过脱敏的版本。
