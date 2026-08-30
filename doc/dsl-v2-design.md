# DataFactory DSL v2 设计稿

状态：待审核  
版本：0.1  
适用范围：Studio校验、离线数据生成和Runtime运行

## 1. 设计目标

DSL v2用于描述确定性的工业过程仿真组态，满足：

- 人工阅读和编辑。
- Studio结构化编辑与静态校验。
- 自动计算执行顺序。
- 离线生成和实时运行使用同一语义。
- 多副本展开时安全重写引用。
- 为OPC UA、趋势和历史提供明确位号元数据。

DSL不做通用编程语言。复杂算法由Rust Engine组件实现，DSL只负责创建组件、设置参数和连接端口。

## 2. 版本与兼容策略

- 只支持`dsl_version: 2`。
- 不支持未声明版本的文件。
- 不支持`init_args`。
- 不支持手写`expression`。
- Runtime不自动转换旧格式。
- 旧文件如需保留，通过独立迁移工具转换，再由用户审核保存。

Studio、编译器和Runtime只维护一套语义。

## 3. 文件结构

```yaml
dsl_version: 2

metadata:
  name: 单阀门水箱
  description: 单回路液位控制案例

clock:
  mode: REALTIME
  cycle_time: 0.5
  sample_interval: 5.0
  start_time: 0

program: []

export:
  default_columns: []
```

| 顶层字段 | 必填 | 类型 | 说明 |
|---|---:|---|---|
| `dsl_version` | 是 | integer | 固定为2 |
| `metadata` | 否 | mapping | 说明信息，不参与运算 |
| `clock` | 是 | mapping | 执行时钟 |
| `program` | 是 | sequence | 节点列表 |
| `export` | 否 | mapping | 默认导出设置 |

禁止未定义的顶层字段。

## 4. 命名规则

节点名称必须匹配：

```text
^[A-Za-z_][A-Za-z0-9_]*$
```

规则：

- 节点名在文件内唯一且区分大小写。
- 禁止使用`cycle_count`、`sim_time`、`time_str`等保留名。
- 属性由组件元数据定义，DSL不能动态创建组件属性。
- 完整位号为`<node>.<attribute>`。
- 中文名称和说明放入metadata或组件描述字段。

## 5. Clock

```yaml
clock:
  mode: REALTIME
  cycle_time: 0.5
  sample_interval: 5.0
  start_time: 0
```

| 字段 | 必填 | 约束 |
|---|---:|---|
| `mode` | 否 | `REALTIME`或`GENERATOR`，默认`GENERATOR` |
| `cycle_time` | 是 | 有限正数，单位秒 |
| `sample_interval` | 否 | 有限正数且不小于cycle_time |
| `start_time` | 否 | Unix秒、ISO时间字符串或0 |

Runtime启动时强制REALTIME；Studio离线生成时强制GENERATOR。文件中的mode仅作为直接运行时的默认值。

## 6. Program公共字段

组件节点通用形态：

```yaml
- name: tank_1
  type: CYLINDRICAL_TANK
  params: {}
  inputs: {}
  display_args: []
  execute_first: false
```

| 字段 | 必填 | 说明 |
|---|---:|---|
| `name` | 是 | 节点唯一名称 |
| `type` | 是 | 节点类型 |
| `params` | 组件节点必填 | 初始化参数 |
| `inputs` | 视组件而定 | 输入端口与来源引用 |
| `display_args` | 否 | 默认展示和导出属性 |
| `execute_first` | 否 | 控制环断环节点，默认false |

不同类型允许的字段不同。不适用字段应报错，不能静默忽略。

## 7. 节点类型

### 7.1 Variable

表示基础变量：

```yaml
- name: source_flow
  type: Variable
  value: 0.0012
  writable: false
  display_args: []
```

`value`必填，只允许number或boolean；`writable`默认false。Variable不允许params、inputs和execute_first。

### 7.2 Expression

表示纯计算变量：

```yaml
- name: level_percent
  type: Expression
  formula: tank_1.level / 1.2 * 100
  display_args:
    - "level_percent[100]"
```

formula只允许本文定义的安全表达式子集。

### 7.3 Lag

表示固定周期延迟：

```yaml
- name: delayed_level
  type: Lag
  source: tank_1.level
  delay: 10
```

delay是正整数，单位为Engine周期。用户不直接书写`[-N]`。

### 7.4 组件节点

```yaml
- name: tank_1
  type: CYLINDRICAL_TANK
  params:
    height: 1.2
    radius: 0.15
    outlet_area: 0.00025
    initial_level: 0.15
  inputs:
    inlet_flow: valve_1.outlet_flow
  display_args:
    - "level[1.2]"
```

组件类型、参数、输入端口和输出属性必须来自组件元数据。

## 8. Params规则

- 参数名必须存在于组件元数据。
- 必填参数不得缺失。
- 参数类型必须匹配。
- 数值必须有限，不允许NaN和Infinity。
- 有上下限的参数必须在范围内。
- 未知参数直接报错。
- 默认值只由组件元数据定义，解析器不重复维护。

Studio可以展示默认值；保存时建议显式写入实际值，使运行组态可读、可追溯。

## 9. Inputs与引用

### 9.1 引用格式

```text
node
node.attribute
```

组件输入通常引用`node.attribute`；引用Variable、Expression或Lag时允许直接引用`node`。

inputs禁止函数调用、算术表达式、文件路径和运行元数据引用。需要计算时，应增加Expression节点。

### 9.2 校验规则

- input端口必须存在。
- 必填input必须连接。
- 引用节点必须存在。
- 引用属性必须是公开输出。
- 来源与目标数据类型必须兼容。
- 同一input只能有一个来源。

## 10. Formula安全子集

允许：

- 数值、布尔常量和括号。
- `+ - * / ** %`。
- `< <= > >= == !=`。
- `and or not`。
- 节点和公开属性引用。
- 白名单函数：`abs`、`min`、`max`、`sqrt`、`sin`、`cos`。

禁止：

- 赋值。
- 超过`node.attribute`的属性链。
- 下标访问。
- lambda、推导式和生成器。
- import、文件、网络和系统调用。
- 调用组件execute。
- 以下划线开头的名称。

formula必须先通过AST白名单校验，再执行。

## 11. 执行顺序与控制环

依赖来源：

- 组件节点读取inputs。
- Expression读取formula引用。
- Lag读取source。

解析器执行稳定拓扑排序；无依赖冲突时保持YAML原始顺序。

典型闭环为：

```text
PID → Valve → Tank → PID
```

环内必须明确一个节点：

```yaml
execute_first: true
```

其语义是该节点本周期使用输入的上一周期值先执行，从而断开本周期代数环。

规则：

- 无环节点不得设置execute_first。
- 每个简单环只能有一个execute_first。
- 无标记或无法断开的环报错。
- 校验结果返回最终执行顺序供Studio展示。

## 12. Display Args

```yaml
display_args:
  - "PV[1.2]"
  - "SV[1.2]"
  - "MV[100]"
```

格式是`attribute`或`attribute[plot_scale_ref]`。

- 属性必须公开存在。
- plot_scale_ref必须是有限正数。
- 缺省比例为100。
- 缺少display_args表示不默认展示。
- 空列表明确表示不展示。
- display_args不参与Engine计算。

## 13. Export

```yaml
export:
  default_columns:
    - tank_1.level
    - pid1.SV
  sample_interval: 5.0
```

API请求中的设置优先。首版不在DSL定义Excel样式、多Sheet布局等展示细节。

## 14. 完整示例

```yaml
dsl_version: 2

metadata:
  name: 单阀门单水箱PID
  description: PID控制水箱液位

clock:
  mode: REALTIME
  cycle_time: 0.5
  sample_interval: 5.0
  start_time: 0

program:
  - name: source_flow
    type: Variable
    value: 0.001
    writable: false
    display_args: []

  - name: pid1
    type: PID
    execute_first: true
    params:
      PB: 50.0
      TI: 90.0
      TD: 0.0
      KD: 10.0
      MODE: 5
      SV: 0.8
      MV: 0.0
    inputs:
      PV: tank_1.level
    display_args:
      - "PV[1.2]"
      - "SV[1.2]"
      - "MV[100]"

  - name: valve_1
    type: VALVE
    params:
      full_travel_time: 12.0
      initial_opening: 0.0
    inputs:
      target_opening: pid1.MV
      inlet_flow: source_flow
    display_args: []

  - name: tank_1
    type: CYLINDRICAL_TANK
    params:
      height: 1.2
      radius: 0.2
      outlet_area: 0.0001
      initial_level: 0.15
    inputs:
      inlet_flow: valve_1.outlet_flow
    display_args:
      - "level[1.2]"

export:
  default_columns:
    - tank_1.level
    - pid1.PV
    - pid1.SV
    - pid1.MV
```

## 15. 校验结果

```json
{
  "severity": "error",
  "code": 2005,
  "message": "节点 valve_1 的输入 inlet_flow 引用了不存在的节点 source_flow",
  "path": "program[2].inputs.inlet_flow",
  "line": 37,
  "column": 19
}
```

code使用Python API文档中的整数错误码。DSL主要使用2001–2099。

校验顺序：

1. YAML语法。
2. 版本和顶层结构。
3. 节点字段与类型。
4. 组件参数和端口。
5. 引用存在性和类型。
6. 拓扑与控制环。
7. display和export。

一次校验尽量返回全部独立问题；无法解析YAML或program结构时除外。

## 16. 多副本展开

多副本属于工程编译能力，不写入单个DSL文件。展开时：

- replica 0保留原名。
- replica N使用`<name>_<N>`。
- inputs、formula和source引用同步重写。
- 不需要AST重写execute表达式，因为v2不存在手写expression。
- 展开后重新进行全局唯一性和拓扑校验。

## 17. 待审核项

1. Variable是否需要writable字段，还是全部Variable默认可写。
2. Studio选择文件后，特征页是否需要展示完整拓扑图，还是只展示执行顺序和统计信息。
3. Formula首版需要开放哪些数学函数。
4. execute_first是否采用“每个环只能一个”的严格限制。
5. export是否保留在DSL中，还是完全由Studio任务参数管理。
6. metadata是否增加作者、创建时间和修改时间。
