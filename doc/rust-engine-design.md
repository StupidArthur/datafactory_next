# DataFactory Rust Engine 设计稿

状态：待审核  
版本：0.1  
范围：Studio调试执行与Runtime生产执行共享的Rust核心

## 1. 设计目标

Rust Engine是DataFactory唯一的执行内核，同时服务于：

- Studio中的单模板调试与数据生成。
- Runtime中的多模板实例长期实时运行。

核心目标：

1. Studio和Runtime使用完全相同的计算语义。
2. 高频周期执行不依赖Python解释器、字符串Tag查找或跨语言逐节点调用。
3. DSL在运行前编译成有类型的执行计划。
4. 内置算法和用户WASM算法在DSL中采用统一节点模型。
5. 支持确定性执行、周期边界原子写入、历史延迟和故障诊断。
6. 对外只提供粗粒度接口，避免通信成本侵入周期执行。

## 2. 非目标

首版Rust Engine不负责：

- HTTP、WebSocket页面协议。
- Studio或Monitor界面。
- 工作目录和文件对话框。
- Runtime Project编辑和发布流程。
- 用户、权限和鉴权。
- 历史文件查询界面。
- 多机分布式调度。
- 单个Runtime Project内的多时钟并行调度。
- 任意Python代码执行。
- 用户算法访问文件、网络、线程或操作系统API。

Engine可以提供归档所需的数据，但归档文件生命周期由Runtime Service管理。

## 3. 总体位置

```text
                 DataFactory Studio
                         │
                         │ Template Compile / Debug Job
                         ▼
┌──────────────────────────────────────────────────────┐
│                 DataFactory Rust Core                │
│                                                      │
│ DSL Compiler → Project Linker → Runtime Plan          │
│                          ↓                           │
│ Formula VM / Algorithm Registry / WASM Host           │
│                          ↓                           │
│ Typed Slots / Scheduler / Writes / Snapshots          │
└──────────────────────────────────────────────────────┘
                         ▲
                         │ Load Runtime Package / Run
                         │
                 DataFactory Runtime
```

Studio和Runtime可以有不同应用入口，但必须依赖同一版本的Rust Core。

## 4. Crate边界

建议拆分为少量稳定crate：

```text
df-types       公共数据类型、错误码和版本
df-dsl         DSL v2解析、校验和Formula编译
df-plan        Compiled Template与Runtime Plan格式
df-algorithms  内置算法与Descriptor Registry
df-wasm        用户算法包加载和WASM执行
df-engine      Slot、调度、写入、快照和诊断
```

Studio Service和Runtime Service作为应用层crate存在，不属于Engine Core。

不为每个算法创建独立crate；只有体量大、依赖特殊的算法族才单独拆分。

## 5. 编译模型

### 5.1 Template Compile

输入：单份DSL v2。

```text
YAML
→ 结构解析
→ 算法类型解析
→ 参数与端口校验
→ Formula AST和类型检查
→ 模板内部拓扑排序
→ 分配模板本地Slot
→ Compiled Template
```

Template Compile负责模板内部完整性，不处理生产UA端口、实例重名和最终可写策略。

### 5.2 Runtime Project Link

输入：Runtime Project和多个Compiled Template。

```text
Compiled Templates
→ 创建模板实例
→ 增加实例命名空间
→ 应用参数覆盖
→ 检查实例重名
→ 检查全局周期约束
→ 分配全局Slot
→ 生成可写/归档/报警Tag表
→ 生成UA映射
→ Compiled Runtime Plan
```

### 5.3 用户可见行为

- Template页面的“重新加载”“校验”和“运行”会自动执行Template Compile。
- Runtime Project页面的“项目校验”执行Template Compile和Project Link。
- “发布”生成Runtime Package。
- 用户不需要管理单独的中间编译文件。

### 5.4 编译缓存

缓存键至少包含：

```text
DSL内容Hash
Compiler版本
算法Descriptor版本集合
用户算法包Hash集合
```

缓存只用于提速。Studio每次运行仍先读取磁盘文件并重新计算Hash。

## 6. 编译产物

### 6.1 Compiled Template

包含：

- 模板元数据。
- 所需算法ID和版本。
- 已校验参数。
- 本地Slot描述。
- 节点执行顺序。
- Formula字节码。
- Tag元数据。
- 推荐周期和周期限制。
- 调试默认展示列。

### 6.2 Compiled Runtime Plan

包含：

- Plan格式版本。
- Engine ABI版本。
- Runtime Project名称和版本。
- 全局cycle_time。
- 实例清单与模板Hash。
- 全局Typed Slot布局。
- 节点执行计划。
- 算法参数和初始状态。
- Formula字节码。
- 用户算法包ID、版本和Hash。
- 最终可写Tag表。
- 归档Tag表。
- 报警规则。
- OPC UA Node映射。

### 6.3 序列化

`runtime-plan.bin`使用带显式版本号的稳定序列化格式，不直接dump Rust内存。

首版可使用固定schema的二进制序列化库，但必须满足：

- 字段顺序不依赖HashMap迭代顺序。
- 所有长度有上限。
- 读取时完整校验。
- 版本不兼容时拒绝加载。
- 不能反序列化任意Rust类型。

## 7. 数据类型

首版核心类型：

```text
BOOL
INT64
FLOAT64
STRING
```

### 7.1 类型转换

- BOOL只能连接BOOL。
- STRING只能连接STRING。
- INT64可以隐式提升为FLOAT64。
- FLOAT64不能隐式转INT64。
- 数值不能隐式转BOOL。
- 其他转换必须使用显式转换算法。

### 7.2 类型与属性分离

每个公开Tag还包含：

```text
data_type
unit
description
readable
writable_capability
min/max
quality
timestamp
```

可写能力不是数据类型。最终可写必须同时满足：

```text
算法Descriptor允许写
AND
Runtime Project将完整Tag加入allowlist
```

Quality和Timestamp作为Tag状态存在，不混入Value类型。

## 8. Typed Slot模型

编译阶段把Tag映射为整数Slot：

```text
tank_line_1.pid1.PV → FLOAT64 slot 127
tank_line_1.pid1.MODE → INT64 slot 8
```

运行时按类型分区：

```text
bool_slots[]
int_slots[]
float_slots[]
string_slots[]
```

引用使用：

```rust
struct SlotRef {
    data_type: DataType,
    index: u32,
}
```

高频执行不使用Tag字符串和通用JSON Value。

### 8.1 Slot元数据

Tag名称、单位、描述和UA NodeId保存在只读Catalog中，运行期数值数组只保存值、质量和时间戳。

### 8.2 Snapshot双缓冲

Engine维护：

```text
working buffer
published snapshot buffer
```

周期结束后原子发布新Snapshot。外部读取稳定Snapshot，不访问正在计算的工作缓冲。

## 9. 算法节点模型

### 9.1 Algorithm Descriptor

每个算法声明：

- 唯一type ID。
- 算法版本。
- 参数schema。
- 输入端口schema。
- 输出端口schema。
- 公开状态schema。
- 周期约束。
- 可写能力。

Descriptor是DSL校验、Studio特征、UA映射和Runtime写入校验的唯一元数据来源。

### 9.2 内置算法接口

概念接口：

```rust
trait Algorithm {
    fn reset(&mut self, context: &ResetContext) -> Result<(), EngineError>;

    fn step(
        &mut self,
        inputs: &InputView,
        outputs: &mut OutputView,
        context: &StepContext,
    ) -> Result<(), EngineError>;
}
```

`StepContext`只提供确定性信息：

- cycle_time
- cycle_count
- sim_time

算法不能自行读取任意Tag、修改其他算法、创建线程或改变执行顺序。

### 9.3 算法状态

每个节点实例拥有独立状态。多个节点可以共享算法代码，但不能共享可变状态。

## 10. Formula能力

Formula是一种内置算法节点，用于保留DSL四则运算易用性。

```yaml
- name: level_percent
  type: Formula
  inputs:
    level: tank_1.level
    scale: level_scale
  formula: level / scale * 100
  output:
    type: FLOAT64
```

### 10.1 允许语法

- 常量和括号。
- `+ - * / % **`。
- `< <= > >= == !=`。
- `and or not`。
- 显式inputs中的变量。
- 白名单纯函数。

### 10.2 禁止语法

- 赋值、循环、lambda和定义。
- 文件、网络和系统调用。
- 动态属性和任意对象方法。
- 直接调用其他算法。
- 访问未声明Tag。
- 修改外部状态。

### 10.3 执行方式

Formula在Template Compile阶段转换成有类型字节码：

```text
LOAD_INPUT 0
LOAD_INPUT 1
DIV_FLOAT64
PUSH_FLOAT64 100
MUL_FLOAT64
STORE_OUTPUT 0
```

Runtime不保存或解释AST文本，只执行预编译字节码。首版不引入JIT。

### 10.4 异常语义

- 除零、NaN、Infinity和函数域错误视为节点计算失败。
- 失败周期不发布非法输出。
- 节点输出保持上一次有效值并将Quality标记为Bad CalculationError。
- 连续节点失败参与Engine Safe State判断。

## 11. Delay能力

Delay作为普通内置算法，不作为DSL特殊语法：

```yaml
- name: delayed_level
  type: DELAY
  params:
    delay_steps: 10
    initial_value: 0.0
  inputs:
    input: tank_1.level
```

RingBuffer由DELAY节点实例内部持有。DSL不提供`[-N]`和全局Lag推断。

## 12. 用户自定义算法

### 12.1 使用方式

用户算法以WASM算法包交付，并在DSL中作为一等算法节点使用：

```yaml
algorithm_packages:
  - file: algorithms/my-controller.dfalgo

program:
  - name: controller_1
    type: com.example.my_controller
    params:
      gain: 2.0
    inputs:
      pv: tank_1.level
      sv: setpoint
```

其输出、公开状态、趋势、归档和UA行为与内置算法一致。

### 12.2 算法包

```text
my-controller.dfalgo
├── manifest.yaml
├── algorithm.wasm
├── source.sha256
└── build.json
```

Manifest声明：

- Algorithm ID和版本。
- ABI版本。
- 参数、输入、输出和公开状态。
- 类型、单位、范围和可写能力。
- 周期限制和资源上限。

### 12.3 WASM能力边界

用户算法只能：

- 读取声明的参数和输入。
- 读写自身内部状态。
- 写入声明的输出。
- 读取Engine提供的周期信息。

用户算法不能：

- 按字符串读取任意Tag。
- 修改其他节点状态。
- 访问文件、网络和环境变量。
- 创建线程或子进程。
- 动态创建Tag。
- 调用Runtime HTTP或UA。
- 绕过写入权限。

### 12.4 WASM交互

Engine一次传入完整输入缓冲、参数和上下文，一次取回完整输出缓冲；禁止逐Tag跨边界调用。

WASM模块启动时加载并缓存。同一算法的多个节点共享代码模块，但持有独立实例状态。

### 12.5 资源限制

Runtime加载用户算法时配置：

- 最大线性内存。
- 每周期最大Fuel或执行预算。
- 禁止未声明import。
- 最大状态区大小。
- 最大输入输出数量。

超限视为节点执行失败，不允许阻塞整个Engine线程。

## 13. 执行调度

### 13.1 单周期顺序

```text
接收周期边界写入
→ 更新允许写入的Slot
→ 按Compiled Runtime Plan顺序执行节点
→ 计算Quality和诊断
→ 发布Snapshot
→ 推进cycle_count和sim_time
```

### 13.2 确定性

- 相同Plan、初始值和写入序列应产生相同输出。
- 节点顺序由编译器固定。
- Runtime不根据HashMap顺序调度。
- 首版一个Runtime Project只使用一个Engine执行线程。
- 外部请求不能在周期中间修改Slot。

### 13.3 控制环

控制环在编译期检查。`execute_first`节点本周期使用指定输入的上周期值断开代数环。

无法明确断开的环拒绝编译。

## 14. Engine模式

### 14.1 Debug/Generator

用于Studio：

- 指定周期数。
- 可不等待真实时间。
- 禁用生产UA、报警和归档。
- 支持增量读取生成行。
- 使用与Runtime相同的算法和周期语义。

### 14.2 Realtime

用于Runtime：

- 按cycle_time调度。
- 记录每周期执行耗时。
- 接收HTTP和UA写入队列。
- 发布实时Snapshot。
- 向归档、报警和UA提供稳定数据。

两种模式不能改变算法计算公式、节点顺序和类型规则。

## 15. 写入模型

### 15.1 写入命令

```text
WriteBatch
├── write_id
├── expected_cycle（可选）
└── items[]
    ├── slot_id
    ├── data_type
    └── value
```

### 15.2 写入规则

- HTTP和OPC UA使用同一个Engine写入入口。
- 外部Tag名称在Service边界转换成Slot ID。
- 一批写入在同一周期边界原子应用。
- 应用前检查类型、范围、算法能力和Runtime allowlist。
- 写入不能直接获得可变Slot引用。
- Engine返回accepted/applied/rejected状态和对应周期。

### 15.3 Force和Quality

Force和Quality作为独立覆盖层存在，不改变基础Slot类型和算法内部状态。

最终Snapshot按顺序组合：

```text
算法值
→ Force覆盖
→ Quality覆盖
→ 发布值
```

具体Force业务生命周期由Runtime Service管理，Engine只执行已验证覆盖命令。

## 16. Snapshot与数据读取

### 16.1 Snapshot内容

```text
snapshot_id
cycle_count
sim_time
execution_time
safe_state
values[]
qualities[]
timestamps[]
```

内部Snapshot按Slot组织，不使用Tag字符串字典。

### 16.2 订阅读取

Service在连接建立时把Tag名称解析成Slot列表：

```text
["pid1.PV", "pid1.SV"] → [127, 128]
```

之后按Slot批量读取。Engine不生成JSON；JSON或WebSocket编码由Service完成。

### 16.3 Studio生成结果

Studio使用粗粒度接口：

```text
run_cycles(n)
read_rows(offset, limit, selected_slots)
```

不得从外部循环调用`step()`生成大量数据。

## 17. Safe State与故障

Engine维护：

- runtime_state
- safe_state
- consecutive_failures
- last_error
- per-node diagnostics

建议规则：

- 单节点失败：保留该节点上次有效输出，标记Bad。
- 连续失败达到阈值：进入Safe State。
- Safe State停止算法推进，但继续响应状态和停止命令。
- Engine线程panic必须在最外层捕获并转换为failed状态。
- WASM trap不得导致Engine进程直接退出。

Safe State恢复策略首版只支持停止后重新启动，不做自动恢复。

## 18. 对外交互原则

### 18.1 禁止细粒度接口

不提供：

```text
get_tag(name)
set_tag(name, value)
execute_node(name)
get_component_attribute(name)
```

这些接口会引入字符串查找、锁竞争和高频跨模块调用。

### 18.2 提供粗粒度接口

Engine Core对Host提供：

```text
compile_template(request) -> compiled_template + diagnostics
link_project(request) -> runtime_plan + diagnostics
create_engine(runtime_plan, mode) -> engine_handle
start(engine_handle)
stop(engine_handle)
run_cycles(engine_handle, cycles)
submit_write_batch(engine_handle, writes)
get_status(engine_handle)
get_catalog(engine_handle)
read_snapshot(engine_handle, slot_subscription)
read_rows(engine_handle, offset, limit, slots)
destroy_engine(engine_handle)
```

具体Rust API可以更类型化，但对跨进程或FFI暴露时保持上述粒度。

## 19. Studio交互边界

Studio调用Rust Core完成：

- DSL Template Compile。
- 特征和诊断提取。
- Debug Engine创建。
- 指定周期运行。
- 增量趋势数据读取。
- Runtime Project Link。
- Runtime Package生成。
- 用户算法编译产物校验和WASM调试。

Studio不直接访问Engine内部节点和Slot数组。

## 20. Runtime Service交互边界

Runtime Service负责：

- 加载Runtime Package。
- 调用Rust Core创建和启停Engine。
- HTTP/WebSocket协议。
- OPC UA Server适配。
- 历史归档文件。
- 报警事件持久化。
- Runtime Package选择和运行记录。

Engine负责：

- 执行计划和算法状态。
- 周期调度。
- 写入应用。
- Snapshot发布。
- 节点级Quality和诊断。

Service与Engine之间使用批量Slot数据，不传递逐Tag JSON。

## 21. OPC UA边界

OPC UA适配层启动时读取Tag Catalog并建立：

```text
UA NodeId ↔ SlotRef
```

读取流程：

```text
Engine Snapshot
→ 批量Slot读取
→ UA Variant转换
→ UA节点刷新
```

写入流程：

```text
UA Write
→ NodeId解析为Slot
→ 类型与allowlist校验
→ submit_write_batch
→ 周期边界应用
```

UA层不能直接修改Slot。

首版OPC UA可以与Runtime Service位于同一Rust进程，避免高频跨进程同步。

## 22. 性能原则

- 周期路径不解析YAML、AST或Tag字符串。
- 周期路径不生成JSON。
- 周期路径避免堆分配。
- 节点输入输出使用预分配Slot引用。
- 外部写入使用有界队列。
- Snapshot使用双缓冲或有界RingBuffer。
- Studio批量运行一次提交多个周期。
- WASM算法一次传递完整输入输出缓冲。
- 归档和WebSocket慢消费者不能阻塞Engine。

## 23. 线程模型

首版一个Engine实例包含：

```text
Engine线程       周期执行和周期边界写入
Host/API线程     提交命令、读取Snapshot
归档线程         消费已发布Snapshot
WebSocket线程    消费已发布Snapshot
```

共享原则：

- Engine线程独占工作Slot和算法状态。
- 外部线程只读Published Snapshot。
- 写命令通过有界队列进入Engine。
- 不在单Project内部并行执行节点。

## 24. 版本兼容

至少维护：

```text
DSL_VERSION
PLAN_FORMAT_VERSION
ENGINE_ABI_VERSION
ALGORITHM_ABI_VERSION
WASM_HOST_ABI_VERSION
```

Runtime Package声明所有要求。Runtime启动时逐项检查，不支持时返回明确错误，不做隐式降级。

Studio发布包时写入自身Compiler和Core版本。

## 25. 测试与验收

### 25.1 语义一致性

- Studio Generator与Runtime Realtime在相同输入序列下结果一致。
- Rust Engine与现有Python黄金数据进行迁移期对比。
- PID、Delay、Formula和水箱建立固定数值基线。

### 25.2 编译器

- DSL字段、类型、引用和拓扑错误。
- Formula合法和非法语法。
- 模板实例命名空间。
- Project参数覆盖和可写Tag校验。

### 25.3 用户算法

- ABI不匹配。
- 缺失或错误Hash。
- WASM trap。
- 内存和Fuel超限。
- 多实例状态隔离。

### 25.4 并发

- Start/Stop竞争。
- 写入与Snapshot一致性。
- 慢消费者不阻塞Engine。
- Engine失败状态可观察和可停止。

### 25.5 性能

每个生产基准记录：

- P50/P95/P99周期执行时间。
- 最大周期时间。
- Tag和节点数量。
- Snapshot发布耗时。
- WASM边界耗时。
- CPU和内存占用。

## 26. 推荐实现顺序

1. `df-types`和算法Descriptor。
2. DSL v2结构解析与类型检查。
3. Typed Slot和Compiled Template。
4. 内置Constant、Formula、Delay及基础Math算法。
5. Engine单周期和Generator模式。
6. PID、水箱、阀门等生产标准算法。
7. Runtime Project Link和命名空间。
8. 写入、Snapshot、Safe State和诊断。
9. WASM Host与算法包。
10. Realtime时钟和Runtime Service集成。
11. OPC UA、归档和WebSocket集成。
12. Python黄金数据对比和性能验收。

## 27. 待审核项

1. 首版是否只支持BOOL、INT64、FLOAT64和STRING。
2. Formula输出是否限制为单输出。
3. Formula白名单函数的最终集合。
4. 用户算法是否确定采用WASM，而不支持原生DLL。
5. 用户算法首版是否要求用户安装Rust工具链，还是Studio内置固定工具链。
6. WASM执行预算采用Fuel还是墙钟超时，或两者结合。
7. Runtime Service和OPC UA是否都直接使用Rust实现。
8. Runtime Plan具体序列化格式。
9. Safe State连续失败阈值是否继续采用5个周期。
10. 一个Runtime Service是否只运行一个Runtime Project。

