# 经典 DSL v2 模板

本目录保存从 `classical_config/` 迁移的正式 DSL v2 模板。迁移规则：

- `init_args` 转为 `params`；
- `execute(...)` 参数转为 `inputs`；
- 赋值表达式转为 `Variable`、`Expression` 或 `Lag`；
- `[-N]` 历史访问转为显式 `Lag` 节点；
- 控制环使用 `execute_first` 明确断环；
- 所有组件默认参数均显式展开。

模板清单：

- `expression_demo.yaml`：原 `test_bug.yaml`；
- `lag_demo.yaml`：原 `滞后案例.yaml`；
- `tank_level_control.yaml`：原 `典型水箱液位控制.yaml`；
- `two_tank_pid.yaml`：原 `单阀门二阶水箱.yaml`；
- `two_tank_open_loop.yaml`：原 `单阀门二阶水箱开环辨识.yaml`；
- `two_tank_plant.yaml`：一期二阶水箱案例的正式 DSL v2 表达。

`examples/two-tank/project.yaml` 也已切换为同一套正式 DSL v2，不再保留一期临时输入格式。
