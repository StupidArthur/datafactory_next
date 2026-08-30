# 第一阶段Engine实现契约

状态：已实现并通过测试  
版本：0.1.0

## 1. 交付范围

第一阶段只包含：

- Rust Engine及Project编译命令。
- Rust SDK。
- Python SDK。
- 二阶水箱源Project和编译后的Project。

不包含UA、HTTP、前端、真实时间调度、历史库和`interval_time`。

## 2. 时间语义

`cycle_time`保存在编译后的Project中，表示Engine执行一个周期所推进的模型物理时间。算法不得使用两次外部调用之间的墙上时间。

Engine不提供加速倍数。外部连续调用`execute`就是加速运行；按真实时间间隔调用就是实时运行。`interval_time`属于上游采样或导出模块，不进入Engine。

## 3. SDK接口

概念接口：

```text
engine = start(project_path)
engine.get(name) -> content
engine.execute(cycles, writes) -> snapshot
engine.stop()
```

SDK管理一个常驻Engine子进程，并隐藏标准输入/输出协议。Python的`engine_path`参数和Rust的`start_with_engine`仅用于开发、测试或非标准安装位置；正常安装可通过`DF_ENGINE_PATH`指定Engine。

### 3.1 execute

- `cycles = 0`：`writes`必须为空，不推进周期，只返回当前Snapshot。
- `cycles > 0`：首先原子校验并应用`writes`，随后连续执行指定周期数，只返回最终Snapshot。
- 任一写值无效时，整批拒绝，不写入且不推进周期。
- Project状态保存在对应Engine进程内，直到`stop`。

Snapshot包含：

```text
cycle_index
simulation_time = cycle_index * cycle_time
values
```

### 3.2 get

第一阶段资源名固定为：

| 名称 | 内容 |
|---|---|
| `project_metadata` | Project名称、cycle_time、格式和ABI版本、Tag与节点数量 |
| `tag_schema` | Tag名称、类型、单位、说明、初值和可写属性 |
| `runtime_status` | 当前cycle_index和simulation_time |

动态值统一通过`execute(0, {})`读取，不提供第二套Snapshot接口。

## 4. 进程协议

Engine以`df-engine run <project.dfproject>`启动。标准输入和标准输出采用一行一条JSON消息的严格串行请求/响应协议；标准错误仅用于日志。SDK使用递增请求ID检查响应匹配关系。

协议操作只有：

- `execute`
- `get`
- `stop`

第一阶段不支持并发请求、事件推送和自动重启。

## 5. 整数错误码

| CODE | 含义 |
|---:|---|
| 0 | 成功 |
| 1001 | 文件读写失败 |
| 1101 | 源Project无效 |
| 1102 | 编译后Project无效或版本不兼容 |
| 1201 | 写入Tag不存在 |
| 1202 | Tag不可写 |
| 1203 | 写入值类型错误 |
| 1204 | 请求参数或资源名无效 |
| 1301 | Engine进程错误 |
| 1302 | 进程通信协议错误 |

## 6. Project文件

源Project使用DSL v2 YAML。编译命令：

```text
df-engine compile <source.yaml> <output.dfproject>
```

编译产物包含固定文件头、载荷长度、显式Project格式版本和Engine ABI版本。Engine只加载编译产物，不在运行周期内解析YAML。

当前二阶水箱案例包含一个可写入口流量和两个按顺序连接的圆柱水箱，模型计算与旧Python圆柱水箱一致，使用托里拆利定律和Project中的`cycle_time`。

## 7. 测试覆盖率门槛

第一阶段Rust Engine、Rust SDK和Python SDK的生产代码采用100%行覆盖率要求。覆盖率脚本以“未覆盖生产代码物理可执行行数为0”为硬门槛，任何新增但没有测试执行到的生产代码都会导致检查失败。测试代码自身不计入覆盖率分母；分支覆盖率和Region覆盖率另行统计，不冒充行覆盖率。
