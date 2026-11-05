# TODO — DataFactory macOS 实机完整验收、留证及回传

> 状态：未完成，待真实 Mac 实测｜2026-11-05  
> 基线：StupidArthur/datafactory_next 的 main 分支。适用 Apple Silicon (arm64) 或 Intel (x86_64)。  
> 目标：验证 Rust Engine / Runtime、Go/Wails Studio、OPC UA 多实例闭环与 30 分钟稳定性，交付可复核的测试报告。  
> 辅助脚本：同目录的 [macos_soak.py](macos_soak.py)。仓库已有 [macOS 说明](../doc/engineering/macos-verification.md)。

## 0. 先看：强制验收/退出门禁

**本 TODO 不是测试完成证明。执行者不得在未完成必测项时标记 DONE。**

- [ ] G0：T00～T10 每个必测项目标记 PASS / FAIL / BLOCKED（不适用时写明依据），并保留证据。
- [ ] G1：双实例长稳**持续至少 1800 秒**。中断、崩溃、提前退出必须重跑完整时长。
- [ ] G2：至少一款**独立外部 OPC UA Client**完成真实 Browse/Read/Subscribe、加密 Writer 写入、PID 双水箱响应、Reader/只读拒绝与实例隔离。Rust 自带测试不能替代独立客户端实测。
- [ ] G3：Studio 打包后的 .app **从 Finder 启动**，不依赖 DF_ENGINE_PATH、不依赖 wails dev，仍能编译/仿真/导出。
- [ ] G4：REPORT.md、原始日志、截图、CSV 和测量表足以复现；附 commit SHA、版本、机型和已知问题。
- [ ] G5：若任何必测项失败或缺环境，**继续定位和复测，不得宣称通过**；无法排除的真实阻塞必须明示 FAIL/BLOCKED、原因和后续操作，不能造结果或无限空转。

**分支警告**：此文档以 main 为准。此前 PR #4 的 Fast/Realtime 专项等价和 soak 测试尚未合并时，不应假定这些测试存在。随文附的 30 分钟脚本专门支持 main 已有双实例 Host。之后若代码更新，须以新的 commit 完整复测受影响的项目，不能混用不同 commit 的结果。

**保密约束**：不要向 GitHub 上传 password、token、私钥、PKI、用户敏感信息。测试证书放仓库外的临时目录。原始敏感日志先脱敏再提交。

## T00 — Mac 环境与代码基线（必测）

在 Mac Terminal 打开仓库根目录，确认没有未保存的本地改动：

~~~bash
git switch main
git pull --ff-only
git status --short
export DF_TEST_SHA="$(git rev-parse HEAD)"
export DF_TEST_OUT="$PWD/work/reports/macos-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$DF_TEST_OUT/logs" "$DF_TEST_OUT/screenshots"
printf '%s\n' "$DF_TEST_SHA" > "$DF_TEST_OUT/commit.txt"
set -o pipefail
echo "$DF_TEST_OUT"
~~~

**多个 Terminal 共享同一个绝对路径 DF_TEST_OUT**；新终端请重新 export 成上面打印的路径，不能各自新建报告目录。

~~~bash
{
  date -u
  git branch --show-current
  git rev-parse HEAD
  git status --short
  uname -a
  uname -m
  sw_vers
  system_profiler SPHardwareDataType | head -25
  xcode-select -p
  rustc --version
  cargo --version
  go version
  node --version
  npm --version
  python3 --version
  wails version
} 2>&1 | tee "$DF_TEST_OUT/logs/T00-env.log"
~~~

- [ ] 记录 Mac 型号、芯片/架构、macOS 版本、是否 Rosetta。
- [ ] 有 Xcode Command Line Tools、Rust stable、满足 go.mod 要求的 Go、Node 22、Python >=3.10、Wails v2。未安装 Wails 可执行：
  
  ~~~bash
  go install github.com/wailsapp/wails/v2/cmd/wails@v2.12.0
  export PATH="$(go env GOPATH)/bin:$PATH"
  wails version
  ~~~

- [ ] 记录完整 commit SHA，测试期间不要切换分支或自动拉取新提交。

**PASS**：命令实际可运行且环境记录完整。任何关键工具缺失标记 BLOCKED，安装后复测；不要修改项目锁文件绕过安装问题。

## T01 — Rust、React 与 Go 构建（必测）

~~~bash
cargo fmt --all -- --check 2>&1 | tee "$DF_TEST_OUT/logs/T01-fmt.log"
cargo build --locked -p df-engine --bin df-engine 2>&1 | tee "$DF_TEST_OUT/logs/T01-engine.log"
cargo build --locked -p df-runtime --features opcua 2>&1 | tee "$DF_TEST_OUT/logs/T01-runtime.log"
npm ci --prefix frontend 2>&1 | tee "$DF_TEST_OUT/logs/T01-npm-ci.log"
npm run build --prefix frontend 2>&1 | tee "$DF_TEST_OUT/logs/T01-frontend.log"
gofmt -l main.go engine_path.go engine_path_test.go \
  | tee "$DF_TEST_OUT/logs/T01-gofmt.log"
test ! -s "$DF_TEST_OUT/logs/T01-gofmt.log"
~~~

- [ ] Rust Engine、OPC UA Runtime、React 前端均成功构建。
- [ ] Go 格式输出为空；不存在新 warning/error 造成的编译失败。

**PASS**：上述所有命令退出码均为 0，存在 target/debug/df-engine、target/debug/df-runtime、frontend/dist。必须启用 shell 的 pipefail，避免 tee 掩盖命令失败。

## T02 — Rust workspace、Python SDK 回归（必测）

~~~bash
export PATH="$PWD/target/debug:$PATH"
cargo test --workspace --locked 2>&1 | tee "$DF_TEST_OUT/logs/T02-rust.log"
python3 -m unittest discover -s python/tests -v \
  2>&1 | tee "$DF_TEST_OUT/logs/T02-python.log"
~~~

- [ ] Rust workspace 实际执行并通过全部非忽略测试；没有 panic、崩溃或失败。
- [ ] Python SDK 测试实际执行，不是 0 tests；进程读写、非法输入、周期快照与停止行为符合测试断言。
- [ ] 记录测试总数、失败数和耗时。

**PASS**：两条命令退出码均为 0。Fast/Realtime 专项的同输入逐周期数值等价测试（PR #4）如不在 main，只能标注“尚未包含”，不能宣称已完成该专项。

## T03 — Go Studio 与真实 Engine 集成（必测）

~~~bash
go test ./... 2>&1 | tee "$DF_TEST_OUT/logs/T03-go.log"
DF_ENGINE_PATH="$PWD/target/debug/df-engine" \
DF_STUDIO_WORKSPACE="$PWD/examples/two-tank" \
go test ./studio/application -run '^TestStudioRealEngineWorkflow$' \
  -count=1 -v 2>&1 | tee "$DF_TEST_OUT/logs/T03-real-engine.log"
~~~

- [ ] 所有 Go 测试通过。
- [ ] TestStudioRealEngineWorkflow **确实运行而非 SKIP**；按现有断言完成 2,000,000 周期的离线模拟（1200 快照）、调试三周期、趋势数据读取。
- [ ] 保留测试名称、运行耗时、退出码及失败日志。

**PASS**：两个命令返回 0，真实 Engine 集成用例明确显示 PASS。

## T04 — Studio 开发模式 GUI（必测）

~~~bash
export DF_ENGINE_PATH="$PWD/target/debug/df-engine"
wails dev 2>&1 | tee "$DF_TEST_OUT/logs/T04-dev.log"
~~~

逐项操作并保存截图：

- [ ] GUI 打开，无白屏、闪退、持续报错。
- [ ] 选择 examples/two-tank 工作区，能够列出 project.yaml 与 pid-closed-loop.yaml。
- [ ] 对 project.yaml 执行校验/编译，显示项目元数据、Tag，路径解析无问题。
- [ ] 离线模拟 1000 周期：最终周期=1000，仿真时间=500s（项目周期 0.5s）。
- [ ] 可观察 tank_1.level、tank_2.level 趋势，数据不是空、NaN/Infinity。
- [ ] 调试执行至少 3 周期，尝试将合法可写变量 plant_inlet_flow 改为 0.0015，记录响应。
- [ ] 导出 CSV，用 Finder 找到文件，实际打开并检查表头、数据行数、数值列。
- [ ] 正常退出，确认无残留 Engine 子进程。

**PASS**：每条操作成功并保存关键界面、趋势与 CSV 截图；如果 UI 的按钮名称不同，报告记录实际路径和操作顺序。

## T05 — 独立 macOS .app / Finder 启动（必测）

退出 wails dev，**先 unset 开发变量**：

~~~bash
unset DF_ENGINE_PATH
bash scripts/build-studio-macos.sh 2>&1 | tee "$DF_TEST_OUT/logs/T05-build.log"
APP="$PWD/build/bin/datafactory-studio.app"
test -x "$APP/Contents/MacOS/df-engine"
file "$APP/Contents/MacOS/df-engine" | tee "$DF_TEST_OUT/logs/T05-arch.log"
codesign --verify --deep --strict "$APP"
open "$APP"
~~~

- [ ] 打包和本地 ad-hoc 签名校验通过；内嵌 df-engine 架构匹配 uname -m。
- [ ] 从 Finder **手工双击 .app** 可启动，无 DF_ENGINE_PATH 和 wails dev 依赖。
- [ ] 在独立 .app 重复选择项目、编译、执行至少 10 周期、趋势查看、CSV 导出。
- [ ] 关闭/重启两次，程序不崩溃；退出后没有孤儿 df-engine。
- [ ] 保存桌面截图、导出文件、构建日志、Codesign 结果。

**PASS**：完整独立应用工作流可用。ad-hoc 签名不等同于 Apple Developer ID 签名/公证，不以这项测试声称可对外发行。


## T06 — Runtime CLI：配置校验、双实例状态与快照（必测）

先使用 T01 编译生成的 Runtime（已启用 OPC UA feature；本项使用的示例配置关闭了 OPC UA）。

~~~bash
./target/debug/df-runtime version 2>&1 | tee "$DF_TEST_OUT/logs/T06-version.log"
./target/debug/df-runtime check --config examples/host/runtime.yaml \
  2>&1 | tee "$DF_TEST_OUT/logs/T06-check.log"
./target/debug/df-runtime run --config examples/host/runtime.yaml \
  2>&1 | tee "$DF_TEST_OUT/logs/T06-host.log"
~~~

第三条命令进入交互模式，不要立即关闭。依次输入以下命令，查询两次状态之间实际等待 **5 秒**：

~~~text
status tank-a
status tank-b
snapshot tank-a
snapshot tank-b
[等待 5 秒；不要输入本行]
status tank-a
status tank-b
stop
~~~

- [ ] Host 配置检查显示 tank-a 与 tank-b 两个项目均为 [ok]，没有文件路径或编译错误。
- [ ] 两实例均报告 Running；周期索引第二次查询大于第一次，仿真时间上升。
- [ ] 运行项目的 cycle_time 为 0.5 秒；两实例各自拥有状态、队列与周期计数；不要求两个独立周期计数始终完全相同。
- [ ] 快照包含 tank_1.level / tank_2.level 等真实变量；状态及数值可解析。
- [ ] 输入 stop 后正常退出，显示 runtime host stopped，退出状态 0。
- [ ] 在报告记录每个实例的前后两次 cycle_index、simulation_time 和两项液位。

**PASS**：两实例独立正常推进、可读取快照、停止成功，保存交互日志。注意 examples/host/runtime.yaml 的 OPC UA disabled；T06 不代表通信验收。

## T07 — **双实例完整 30 分钟长稳 + CPU/RSS 留证**（必测）

必须连续执行至少 **1800 秒**。不要使用缩短版替代正式测试；Mac 应接电源，并避免睡眠（可以在另一终端运行 caffeinate -dimsu，在测试结束后终止 caffeinate）。

当前 main 的长稳测试脚本为 work/macos_soak.py，自动启动已有双实例 Host；每 30 秒查询各实例状态并记录整体进程的 CPU 和内存 RSS，最后发 stop。

~~~bash
python3 work/macos_soak.py \
  --duration-secs 1800 \
  --sample-secs 30 \
  --out "$DF_TEST_OUT/T07-soak" \
  2>&1 | tee "$DF_TEST_OUT/logs/T07-console.log"
~~~

脚本输出文件：

- T07-soak/runtime.log：Runtime 输出、两实例状态、最终快照和停止日志；
- T07-soak/resources.csv：elapsed_s、rss_kib、cpu_percent、process_alive；
- T07-soak/soak-summary.json：总时长、结束码、各实例首末状态、资源峰值、各自动验收门槛及 pass。

**必过指标：**

- [ ] 实测运行时间 >= 1800 秒，Runtime 正常停止，进程退出码 0；没有进程崩溃或 Failed 状态。
- [ ] tank-a、tank-b 均有连续记录，cycle_index 严格单调增加，simulation_time 与 cycle_index×0.5s 误差 <0.001s。
- [ ] 两实例最终 cycle_index 均 >=3420，即目标周期 3600 的 95%；这是本次测量的临时最低门槛，不是产品对外实时性能承诺。
- [ ] 采样大约 60 条，记录 RSS 峰值、CPU 峰值，观察 5 分钟热身后 RSS 首尾均值；脚本提示异常增长时须调查。
- [ ] soak-summary.json 中所有 gates=true，pass=true；任何 gate 为 false 则记 FAIL 并分析根因。
- [ ] 记录完成后的两个实例状态及快照；检查是否发生异常重启。
- [ ] 保存原始 stdout 日志、CSV、JSON。期间测试被人为停止、设备进入休眠或失去电源的，必须重新完整运行。

**PASS**：持续跑满 1800 秒，脚本退出 0、JSON pass=true、证据齐全。CPU 和 RSS 为整个 Host 进程的采样，不代表单实例独立资源。当前采样没有逐周期延迟数据，不能宣称 P95/P99 的真实数值。

## T08 — OPC UA Rust 集成测试全套 + 两项关键回归（必测）

这些测试使用 Rust 测试代码启动真实 OPC UA Server/Client，不要求用户另行安装 UaExpert；但不替代 T09/T10 的独立客户端验证。

~~~bash
cargo test --locked -p df-runtime --features opcua --tests \
  -- --nocapture 2>&1 | tee "$DF_TEST_OUT/logs/T08-opcua-all.log"

cargo test --locked -p df-runtime --features opcua \
  --test opcua_write opcua_pid_setpoint_controls_valve_and_two_tank_levels \
  -- --exact --nocapture 2>&1 | tee "$DF_TEST_OUT/logs/T08-pid.log"

cargo test --locked -p df-runtime --features opcua \
  --test opcua_write multi_instance_request_vetoes_one_batch_without_blocking_the_other \
  -- --exact --nocapture 2>&1 | tee "$DF_TEST_OUT/logs/T08-instance-isolation.log"
~~~

- [ ] 第一条实际运行含 OPC UA 连接、浏览、订阅、权限、写入、队列、生命周期测试，而非 0 tests。
- [ ] 第二条实际执行并 PASS：外部 OPC UA PID 设定值驱动阀门与双水箱动态模型；设定值上下阶跃。
- [ ] 第三条实际执行并 PASS：同一请求中 A 实例批次被否决，不阻止 B 实例有效写入；状态不串扰。
- [ ] 记录所有测试数、通过数、失败数、实际耗时；保留完整日志与失败时端口信息。

**PASS**：三个命令均返回 0，关键测试名称明确显示 passed。若失败不删除测试、不改测试数据掩盖问题；先留证并排查端口、证书、系统权限、时间调度等。
