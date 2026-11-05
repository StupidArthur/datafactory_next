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


## T09 — 独立 OPC UA 客户端：Browse、Read、Subscribe（必测）

**必须使用仓库外的 OPC UA Client**，例如在当前 macOS 上可运行的 UaExpert 版本或其他等效客户端。工具须支持浏览节点、读取、订阅、读写和安全身份设置。记录名称、版本和安装来源；缺少此类工具记 BLOCKED，不能用 T08 的 Rust 测试代替。

这一阶段只使用本机回环地址 127.0.0.1，允许 None/Anonymous **仅用于本机只读连通性验证**；不要开放到局域网，也不能用此配置代替 T10 的加密写入验证。

在一个 Terminal 中执行（沿用 T00 中 DF_TEST_OUT 的绝对路径）：

~~~bash
export DF_MAC_PKI="$(mktemp -d /tmp/datafactory-mac-pki.XXXXXX)"
echo "Temporary test PKI: $DF_MAC_PKI"
cat > "$DF_TEST_OUT/T09-anonymous.yaml" <<EOF
runtime_version: 1
opcua:
  enabled: true
  endpoint_url: "opc.tcp://127.0.0.1:4840"
  application_uri: "urn:datafactory:runtime"
  namespace_uri: "urn:datafactory:runtime:ns"
  security:
    mode: None
    policy: None
    allow_anonymous: true
    trust_list: "$DF_MAC_PKI"
instances:
  - id: tank-a
    project: "$PWD/examples/two-tank/pid-closed-loop.yaml"
    enabled: true
    scheduler: {overrun_policy: delay, write_queue_capacity: 256}
  - id: tank-b
    project: "$PWD/examples/two-tank/pid-closed-loop.yaml"
    enabled: true
    scheduler: {overrun_policy: catch_up, write_queue_capacity: 256}
EOF

./target/debug/df-runtime check --config "$DF_TEST_OUT/T09-anonymous.yaml" \
  2>&1 | tee "$DF_TEST_OUT/logs/T09-check.log"
./target/debug/df-runtime run --config "$DF_TEST_OUT/T09-anonymous.yaml" \
  2>&1 | tee "$DF_TEST_OUT/logs/T09-runtime.log"
~~~

让 Runtime 终端保持运行。在独立客户端连接 opc.tcp://127.0.0.1:4840，Policy=None、Anonymous，然后逐项操作：

- [ ] 客户端建立 Session，Runtime 日志含 opcua serving at；如果有“built without the opcua feature”，返回 T01 重新构建正确二进制。
- [ ] 浏览 Objects → DataFactory → Instances，存在 tank-a、tank-b；两实例下都有 Tags、Status。
- [ ] 读取 tank-a / tank-b 的 CycleTime=0.5、CycleIndex、SimulationTime 和 State。间隔 5 秒再读取，两个实例的 CycleIndex 均增加。
- [ ] 两实例都能读取 level_setpoint、pid2.SV、pid2.PV、pid2.MV、valve_1.current_opening、tank_1.level、tank_2.level；数值非空、有限，质量码 Good。
- [ ] 记录实际命名空间索引（不要假定固定为 2）；例如 Tag 的字符串节点标识符应为 df/instances/tank-a/tags/level_setpoint，两实例同名节点的 NodeId 不相同。
- [ ] 订阅一个实时变化的节点（推荐 CycleIndex），持续至少 30 秒，收到多个新值并记录时间戳。
- [ ] 以 Anonymous 尝试写 FLOAT64 0.40 到 tank-a/level_setpoint，应返回 BadUserAccessDenied 或其他拒绝码（匿名仅有 Reader 权限）；读取值仍为原值。
- [ ] 保存浏览树、两实例状态、订阅曲线/表格、写入拒绝码的截图和客户端软件版本。

之后在 Runtime Terminal 输入 stop 并记录正常退出。**PASS**：以上都满足，端点正常释放、证据完整。本节不是加密/授权写入验收。


## T10 — 独立 OPC UA 客户端：加密、权限、外部 PID 双水箱闭环（必测）

此阶段必须与 T09 分开启动；先 stop T09 的 Runtime，确保 4840 端口空闲。**必须 SignAndEncrypt + Basic256Sha256 + Username/Password，不得降级为 None/Anonymous。** 其中 engineer 为 Writer，viewer 为 Reader。

沿用 T09 的 DF_MAC_PKI 临时目录；如果新开 Terminal，需把 DF_MAC_PKI 设置为 T09 时输出的同一路径。服务器样例证书/私钥必须存在；如缺失，标 BLOCKED 并附 T09 服务端日志，禁止把真实私钥复制进 Git 仓库。

~~~bash
test -s "$DF_MAC_PKI/own/cert.der"
test -s "$DF_MAC_PKI/private/private.pem"

# 交互输入，不会将明文口令写入 Shell 历史或 YAML
export DF_MAC_WRITER_PW="$(python3 -c 'import getpass; print(getpass.getpass("Writer password: "))')"
export DF_MAC_READER_PW="$(python3 -c 'import getpass; print(getpass.getpass("Reader password: "))')"
test -n "$DF_MAC_WRITER_PW" && test -n "$DF_MAC_READER_PW"

cat > "$DF_TEST_OUT/T10-secure.yaml" <<EOF
runtime_version: 1
opcua:
  enabled: true
  endpoint_url: "opc.tcp://127.0.0.1:4840"
  application_uri: "urn:datafactory:runtime"
  namespace_uri: "urn:datafactory:runtime:ns"
  security:
    mode: SignAndEncrypt
    policy: Basic256Sha256
    allow_anonymous: false
    server_certificate: "$DF_MAC_PKI/own/cert.der"
    server_private_key: "$DF_MAC_PKI/private/private.pem"
    trust_list: "$DF_MAC_PKI"
    users:
      - username: engineer
        password_env: DF_MAC_WRITER_PW
        role: writer
      - username: viewer
        password_env: DF_MAC_READER_PW
        role: reader
instances:
  - id: tank-a
    project: "$PWD/examples/two-tank/pid-closed-loop.yaml"
    enabled: true
    scheduler: {overrun_policy: delay, write_queue_capacity: 256}
  - id: tank-b
    project: "$PWD/examples/two-tank/pid-closed-loop.yaml"
    enabled: true
    scheduler: {overrun_policy: catch_up, write_queue_capacity: 256}
EOF

./target/debug/df-runtime check --config "$DF_TEST_OUT/T10-secure.yaml" \
  2>&1 | tee "$DF_TEST_OUT/logs/T10-check.log"
./target/debug/df-runtime run --config "$DF_TEST_OUT/T10-secure.yaml" \
  2>&1 | tee "$DF_TEST_OUT/logs/T10-runtime.log"
~~~

说明：当前 main 的服务器已实现 OPC UA 模式与 Writer/Reader 身份控制，但尚未合入 PR #4 的客户端证书强制信任改动。本节证明配置的加密通道及业务写入，不等于全面安全渗透测试。信任客户端证书的负向验收留待修订后专项进行。

在独立客户端中，信任刚生成的**本机测试服务端证书**，选择 SignAndEncrypt / Basic256Sha256、用户名 engineer；配置时不要开启全局“信任任何服务器”用于非本机环境。逐项执行，每步记录 **墙钟时间 + 完整节点路径 + 读数 + 返回 StatusCode**：

1. [ ] **加密连接**：以 engineer 成功建立安全 Session，核对 SecurityPolicy 与 MessageSecurityMode 没有被降级，记录不含密码的连接设置截图。
2. [ ] **基线**：读取 tank-a 和 tank-b 的 level_setpoint，约为 0.09706422；读取 tank-a 的 pid2.SV、pid2.PV、pid2.MV（初值约 23）、valve_1.target_opening、valve_1.current_opening、valve_1.outlet_flow、tank_1.level（初值约 0.0621211）、tank_2.level（初值约 0.09706422）。启动后允许少量连续模型变化，但不能为 NaN/Infinity。
3. [ ] **上调阶跃**：向 tank-a 的 level_setpoint **以 FLOAT64 类型**写入 0.40，客户端写响应为 Good；随后 pid2.SV≈0.40。继续订阅/记录至少 **60 秒**，PID MV、阀门目标及实际开度相对基线增加，tank_1.level / tank_2.level 出现与改变流量一致的动态响应。至少记录写前、写后中间点、60 秒末三组数值及写入时间。
4. [ ] **实例隔离**：上一步全过程 tank-b/level_setpoint 仍保持写前值，tank-b 的 CycleIndex 持续增加；tank-a 写入不得修改 tank-b 的变量或导致其停止。
5. [ ] **下调阶跃**：以 FLOAT64 将 tank-a/level_setpoint 写成 0.20，响应 Good；观察至少 **60 秒**。随后 PID MV、阀门目标/实际开度以及阀门流量相对上调阶段下降。阀门全行程约 12s，水箱存在过程滞后，因此不要求同一个周期水箱液位立刻反向。
6. [ ] **只读拒绝**：以 engineer 向 tank-a 的 tank_2.level 或 pid2.MV 写值；应返回 BadNotWritable（若客户端显示其他 Bad*，须记录并解释）；被拒绝的值不能直接覆写模型快照，模型继续按周期运行。
7. [ ] **Viewer 拒绝**：以 viewer 建立加密 Session，Reader 可正常 Read/Subscribe；尝试写 tank-a 的 level_setpoint 应返回 BadUserAccessDenied，并且原值不变。
8. [ ] **错误节点/错误类型**：以 engineer 向不存在的 NodeId 写入，应返回 BadNodeIdUnknown；向 FLOAT64 Tag 发送 STRING，应返回 BadTypeMismatch。若客户端不允许发出错误类型请求，改用支持原始 WriteRequest 的另一客户端；当前项记 BLOCKED，不能误写 PASS。
9. [ ] **停止与重启**：Runtime Terminal 输入 stop，服务器释放端口、客户端断开；以相同配置重启，原有 NodeId 字符串标识符稳定，两个实例恢复 Running 且可读取，然后再次 stop。
10. [ ] **证据**：提供客户端连接设置、浏览树、身份与拒绝码、时间序列曲线、上下阶跃表，及服务端日志。至少记录 SV、PID MV、阀门实际开度、两水箱液位、tank-b SV、两实例 CycleIndex。

**PASS**：十项全部按独立 OPC UA Client 操作取得真实证据、写入成功/拒绝与预期一致，观察到模型闭环响应，并正常关闭服务。连接或权限受阻时保留失败状态码和日志、确认身份和客户端版本，不能为了“通过”改成明文或取消角色校验。

结束后执行：unset DF_MAC_WRITER_PW DF_MAC_READER_PW。不要提交 DF_MAC_PKI 临时目录、私钥、凭据或完整客户端密钥文件。


## 11. 最终报告、回传和验收结束条件

请复制 [REPORT-macos-template.md](REPORT-macos-template.md) 为本次报告：

~~~bash
cp work/REPORT-macos-template.md "$DF_TEST_OUT/REPORT.md"
# 打开 REPORT.md 逐项填写，写明未完成的地方和所有失败日志
git status --short
~~~

- [ ] 每个 T00～T10 的状态均已填写：PASS / FAIL / BLOCKED / NOT_RUN；禁止空白状态或自行删除失败项。
- [ ] 对每个 PASS 给出**实测值 + 命令或 GUI 操作 + 证据路径**；不能仅写“OK”。
- [ ] 如有 FAIL / BLOCKED：写出现象、触发条件、完整报错、最小复现步骤、是否需要修复、复测结果，并把整体结论标注 **INCOMPLETE**。
- [ ] T07 真实连续运行 >=1800 秒，所需 JSON/CSV/日志齐全，未达到标准不得勾完成。
- [ ] T09/T10 外部独立客户端的浏览、订阅、加密真实读写、PID 响应、实例隔离和权限拒绝全部有证据。
- [ ] 核对 .app 从 Finder 启动的测试独立于 wails dev、且不依赖 DF_ENGINE_PATH。
- [ ] 每次修改源码后记录新 commit 并重测影响范围；报告不得把旧 commit 结果和新 commit 结果合并成一次“全 PASS”。
- [ ] 提交报告前**逐个审阅文件**，确认不含私钥、明文密码、证书数据库、token、用户隐私；不能直接无检查地 git add -A。
- [ ] 用一个报告专用分支（建议 test/macos-physical-acceptance-YYYYMMDD）提交非敏感证据、REPORT.md，并发 PR；如不使用分支，请确保没有覆盖或丢失原有代码更改。
- [ ] 把 PR URL、完整 SHA、测试架构和最终 PASS / INCOMPLETE 状态回传给维护者；保留原始未脱敏证据在本机安全位置，需要时另行安全交换。

**关闭条件（强制）**：仅当 T00～T10 每项必测 PASS、连续 30 分钟长稳 PASS、独立 OPC UA 外部闭环 PASS、Finder .app PASS、证据和 REPORT.md 可复核时，才可以把本 TODO 标记为“验收完成”。

如果遇到真实硬件/第三方客户端/系统权限阻塞，可以结束**当前执行会话**并交付一份状态为 INCOMPLETE 的报告求助；这不等于验收完成。下一轮针对阻塞项修复和重测，直至全 PASS 或维护者正式接受风险并书面记录例外。

---

**执行人提示**：实际测试可能跨多次终端会话；断点记录在 REPORT.md 中，下一次从未通过项继续。不要编造结果，也不要反复运行已知必然失败的步骤来冒充进度。
