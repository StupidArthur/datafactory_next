# 双水箱 PID 外部设定值闭环实施例

## 目的

该工程用于验证外部 OPC UA Client 写入液位设定值后，真实 Runtime Engine 是否沿以下链路产生响应：

`level_setpoint → pid2.SV → pid2.MV → valve_1.target_opening → valve_1.current_opening → valve_1.outlet_flow → tank_1 → tank_2.level / pid2.PV`

它不是离线数据回放。PID、阀门和两个水箱均由 DSL v2 组件在 Engine 周期内执行。

## 初始工作点

- 周期：0.5 s。
- 入口最大流量：0.0012 m³/s。
- 阀门初始开度与 PID 输出偏置：23%。
- 水箱 1 初始液位：约 0.0621211 m。
- 水箱 2 初始液位 / 初始设定值：约 0.09706422 m。

初始液位由稳态流量关系估算，使 23% 阀门开度对应的入口流量与两级水箱出口流量近似平衡。这样测试中的液位变化主要由外部设定值阶跃引起，而不是启动初期的任意液位漂移。

## OPC UA Tag

- 可写输入：`level_setpoint`（FLOAT64）。
- PID 只读状态：`pid2.SV`、`pid2.PV`、`pid2.MV`。
- 阀门只读状态：`valve_1.target_opening`、`valve_1.current_opening`、`valve_1.outlet_flow`。
- 过程变量：`tank_1.level`、`tank_2.level` 及对应出口流量。

除 `level_setpoint` 外，本案例的 PID 状态和过程输出均不可由 OPC UA Client 直接写入。

## 自动化验证

在 Windows 上运行：

```powershell
cargo test --locked -p df-runtime --features opcua --test opcua_write opcua_pid_setpoint_controls_valve_and_two_tank_levels -- --nocapture
```

测试启动真实双水箱 Engine、SignAndEncrypt OPC UA Server 和独立 Writer Client：

1. 读取初始设定值、PID 状态、阀门开度和两个水箱液位。
2. 将 `level_setpoint` 从约 0.0971 m 提高到 0.40 m。
3. 验证写入返回 Good，且经周期确认后 `pid2.SV` 更新。
4. 验证 PID 输出和阀门目标/实际开度增加，两个水箱液位上升。
5. 将设定值降低到 0.20 m，验证 PID 输出和阀门目标/实际开度随之降低。

阀门有 12 s 全行程时间，两个水箱也存在过程滞后；设定值下降后，液位不会瞬时反向。测试据此检查控制输出先行下降，而不错误要求过程变量在同一周期内反向。

## 复现与解释边界

- 参数是为了形成稳定、可观察的演示工作点而选择，不是经过真实设备辨识的工业 PID 整定值。
- 本实施例证明外部设定值、PID、阀门和过程变量的实时闭环链路；不代表已完成真实设备标定、抗积分饱和、故障安全或 SIL 验证。
- 测试证据应与项目 YAML、测试源代码、Git Commit、Windows Actions 日志一起保留。
