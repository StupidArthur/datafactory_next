# Studio 后端模块（第二阶段）

本阶段只实现 Studio 的后端，不包含编辑器、Runtime、OPC UA 或生产历史库。

## 模块边界

### Workspace / DSL 文件

- `Open(root)` 选择工作目录；
- `ListDSL()` 返回目录下的 YAML DSL 文件；
- `Read(path)` 只允许读取工作目录内的文件；
- 文件内容以磁盘为唯一真相源。

### DSL 校验与编译

后端调用 Rust `df-engine compile <source> <artifact>`。每次创建调试任务前重新读取并编译磁盘文件，不依赖 Studio 页面缓存。编译错误原样保留命令输出，并由上层映射为统一 API CODE。

### Debug Job（调试会话 + 趋势）

调试会话和趋势缓存属于同一生命周期模块：

- 一个 Job 对应一个 Engine 进程；
- `Execute(cycles, writes)` 原子调用 Engine；
- 每次成功执行将最终 Snapshot 追加到短期趋势缓存；
- `Trend(offset)` 用偏移量读取当前任务数据；
- `Stop()` 同时结束 Engine 进程和 Job；
- 趋势缓存不承担 Runtime 历史归档。

## 明确不做

- 不在 Studio 保存 DSL 编辑内容；
- 不让 Studio 连接 OPC UA；
- 不复用 Runtime 状态或配置；
- 不在 Studio 内实现第二套模型执行逻辑；
- 不引入 WebSocket，首版由 HTTP 轮询趋势数据。

当前 Go 包位于 `studio/files`、`studio/compiler` 和 `studio/debug`，后续 HTTP Service 只负责把这些模块组合成 API。
