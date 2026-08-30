# DataFactory Python API 设计稿

状态：待审核\
版本：0.1\
范围：新目录下后续实现，不约束现有 `datacenter/engine_api.py` 的兼容行为

## 1. 目标

新版本只保留一个 Python 后端服务，通过两组接口支持两个使用场景：

- Studio：频繁编辑、快速校验、离线生成数据。
- Runtime：稳定实时运行、实时/历史数据查看、OPC UA 读写。

本设计优先保证功能完整和边界清晰，不引入微服务、消息队列、外部数据库或通用插件框架。

## 2. 已确定原则

1. 业务后端统一使用 Python。
2. Studio 和 Runtime 共用 DSL Parser、Validator、Engine 和组件库。
3. 只接受 `dsl_version: 2` 的结构化 DSL。
4. Python Runtime 状态是运行状态的唯一真相源。
5. 一个服务进程只允许一个实时 Runtime；离线任务与实时 Runtime 互斥。
6. Go 只负责桌面能力、服务生命周期和连接信息传递。
7. 服务默认只监听 `127.0.0.1`。

## 3. 不做的事情

- 不拆分多个 Python 服务。
- 不引入数据库保存工程；工程仍使用普通文件和目录。
- 不实现分布式运行、集群调度或远程多租户。
- 不实现在线修改 DSL 和热重载生产 Runtime。
- 不为每个成功响应增加多层 `data/result/meta` 包装。
- 不保留 v1 `init_args + expression` DSL 兼容逻辑。

## 4. 服务边界

```text
DataFactoryService
├── /api/service       服务健康与关闭
├── /api/studio        校验、离线任务和导出
├── /api/runtime       实时运行、写入、报警和历史
└── /api/runtime/stream
```

首版仍可由一个 FastAPI `app` 提供，但代码应按 Router 分文件，避免继续扩充单个 `engine_api.py`：

```text
datafactory_api/
├── app.py
├── errors.py
├── auth.py
├── service_routes.py
├── studio_routes.py
├── runtime_routes.py
├── runtime_stream.py
└── models/
```

这是代码整理，不是新增部署单元。

## 5. 通用约定

### 5.1 基础地址

```text
http://127.0.0.1:<random-port>
```

端口由 Go 启动器选择。Python 服务不得自行暴露到局域网地址。

### 5.2 访问范围

- 不提供任何鉴权功能。
- REST 和 WebSocket 均不要求 Token、Cookie 或登录状态。
- Python 服务必须只监听 `127.0.0.1`，不得配置成局域网或公网地址。
- API 的安全边界是本机进程边界和 loopback 监听范围。

### 5.3 JSON 命名

HTTP JSON 统一使用 `camelCase`，例如：

```json
{
  "runtimeState": "running",
  "cycleCount": 120,
  "cycleTime": 0.5
}
```

Python 内部可继续使用 snake\_case，通过 Pydantic alias 转换。

### 5.4 错误格式与错误码

所有非 2xx 响应使用：

```json
{
  "code": 3001,
  "message": "已有实时运行在进行中",
  "details": {}
}
```

状态码约定：

| 状态码 | 用途                      |
| --- | ----------------------- |
| 400 | 请求字段或参数非法               |
| 404 | 文件、任务、运行记录不存在           |
| 409 | 当前状态不允许该操作              |
| 422 | DSL 内容合法但校验不通过          |
| 500 | 未预期的服务内部错误              |
| 503 | Runtime、OPC UA 或依赖组件不可用 |

不把 Python traceback 返回前端；完整异常写服务日志，响应携带稳定整数错误码和可读信息。

错误码分段：

| 范围 | 分类 |
| --- | --- |
| 1000–1999 | 通用与服务错误 |
| 2000–2999 | DSL与Studio错误 |
| 3000–3999 | Runtime生命周期错误 |
| 4000–4999 | 写入、强制与质量码错误 |
| 5000–5999 | OPC UA错误 |
| 6000–6999 | 报警与历史错误 |

首版错误码表：

| CODE | HTTP | 含义 |
| ---: | ---: | --- |
| 1000 | 500 | 未分类内部错误 |
| 1001 | 400 | 请求参数非法 |
| 1002 | 404 | 请求资源不存在 |
| 1003 | 409 | 当前状态不允许操作 |
| 1004 | 503 | 服务尚未就绪 |
| 2001 | 400 | YAML无法解析 |
| 2002 | 422 | DSL版本不受支持 |
| 2003 | 422 | DSL字段不合法 |
| 2004 | 422 | DSL拓扑存在无法断开的环 |
| 2005 | 422 | DSL引用不存在 |
| 2101 | 409 | 离线任务与Runtime冲突 |
| 2102 | 404 | 离线任务不存在 |
| 2103 | 409 | 离线任务状态不允许操作 |
| 2104 | 500 | 离线任务执行失败 |
| 2201 | 400 | 导出参数非法 |
| 2202 | 500 | 导出失败 |
| 3001 | 409 | Runtime已经运行或正在启动 |
| 3002 | 409 | Runtime尚未运行 |
| 3003 | 500 | Runtime启动失败 |
| 3004 | 500 | Runtime停止失败 |
| 3005 | 500 | Engine线程异常退出 |
| 3006 | 503 | Runtime不可用 |
| 4001 | 400 | 位号不可写 |
| 4002 | 400 | 写入值类型或范围非法 |
| 4003 | 409 | 当前控制模式不允许写入 |
| 4004 | 404 | 写入批次不存在 |
| 4005 | 500 | 写入确认超时 |
| 4101 | 400 | 位号不可强制 |
| 4102 | 404 | 强制记录不存在 |
| 4201 | 400 | 质量码非法 |
| 5001 | 503 | OPC UA启动失败 |
| 5002 | 500 | OPC UA停止失败 |
| 5003 | 400 | OPC UA回写被拒绝 |
| 6001 | 404 | 报警不存在 |
| 6002 | 409 | 报警不可确认 |
| 6101 | 404 | 历史运行不存在 |
| 6102 | 500 | 历史查询失败 |
| 6103 | 500 | 历史归档失败 |

同一个 CODE 的语义不得随接口变化；新增错误码只能追加到对应分段。

## 6. DSL v2 契约摘要

完整定义见 [DSL v2设计稿](dsl-v2-design.md)。本节只保留 API边界需要执行的规则。

所有 Studio 校验、批量生成和 Runtime 启动入口都必须验证：

```yaml
dsl_version: 2
```

v2 允许：

- `params`
- `inputs`
- `Variable.value`
- `Expression.formula`
- `Lag.source + Lag.delay`
- `execute_first`
- `display_args`

v2 禁止：

- `init_args`
- 手写 `expression`
- 未声明 `dsl_version`
- 未识别的顶层字段和 program 字段

拒绝旧文件时返回：

```json
{
  "code": 2002,
  "message": "仅支持 dsl_version: 2",
  "details": {"actualVersion": null}
}
```

## 7. Service API

### 7.1 健康检查

```http
GET /api/service/health
```

响应：

```json
{
  "serviceState": "ready",
  "runtimeState": "stopped",
  "vrsion": "1.0.0"
}
```

### 7.2 关闭服务

```http
POST /api/service/shutdown
```

语义：先停止 Runtime、刷新归档、停止 OPC UA，再结束服务。重复调用应幂等。

## 8. Studio API

Studio不编辑DSL。用户在外部IDE中编辑和保存，Studio始终读取磁盘上的最新文件。

### 8.1 扫描工作目录

```http
GET /api/studio/dsl-files?workDir=G%3A%2Fprojects%2Ftank
```

响应包含工作目录和第一层`.yaml/.yml`文件的名称、绝对路径、修改时间与大小。首版不递归扫描。工作目录由Go目录选择器获得。

### 8.2 读取DSL特征并校验

```http
POST /api/studio/inspect
Content-Type: application/json
```

```json
{
  "path": "G:/projects/tank/tank.yaml"
}
```

Python每次调用都重新从磁盘读取文件。响应：

```json
{
  "valid": true,
  "issues": [],
  "features": {
    "dslVersion": 2,
    "name": "单阀门单水箱PID",
    "cycleTime": 0.5,
    "nodeCount": 4,
    "componentCounts": {"PID": 1, "VALVE": 1, "CYLINDRICAL_TANK": 1},
    "tagCount": 18,
    "writableTagCount": 6,
    "hasControlLoop": true
  },
  "topology": {
    "executionOrder": ["pid1", "valve1", "tank1"],
    "nodes": [],
    "edges": []
  },
  "tags": []
}
```

选择文件和“重新加载”都调用该接口。校验不通过仍返回200和`valid=false`；路径或文件错误返回400/404。

### 8.3 创建调试运行任务

```http
POST /api/studio/jobs
```

请求：

```json
{
  "dslPath": "G:/projects/tank/tank.yaml",
  "cycles": 6000,
  "cycleTime": 0.5,
  "sampleInterval": 5.0,
  "startTime": 0
}
```

创建任务时Python必须再次从磁盘读取`dslPath`。IDE刚保存的内容无需在Studio中重新加载即可用于本次运行；“重新加载”只刷新特征页面。

响应：

```json
{
  "jobId": "uuid",
  "state": "queued"
}
```

约束：

- `cycles > 0`
- 同一时刻最多一个离线任务
- Runtime 正在运行时返回 409
- 任务在 Python 后台线程执行，不新建 Python 子进程
- 每次启动前重新解析并完整校验磁盘文件
- 运行参数覆盖clock默认值，但不回写DSL

### 8.4 查询任务

```http
GET /api/studio/jobs/{jobId}
```

```json
{
  "jobId": "uuid",
  "state": "running",
  "completedCycles": 2000,
  "totalCycles": 6000,
  "columns": ["simTime", "pid1.PV"],
  "error": null
}
```

状态只保留：`queued | running | completed | failed | cancelled`。

### 8.5 读取趋势数据

```http
GET /api/studio/jobs/{jobId}/rows?offset=0&limit=1000
```

响应：

```json
{
  "offset": 0,
  "limit": 1000,
  "total": 1200,
  "columns": ["simTime", "pid1.PV"],
  "rows": []
}
```

`limit` 首版最大 5000，防止一次把完整大结果传给 WebView。

任务处于`running`时即可读取已经生成的行。前端使用上次返回的`offset + rows.length`继续拉取，实现运行过程中持续刷新趋势；首版不为Studio任务额外增加WebSocket。

### 8.6 取消和删除任务

```http
POST   /api/studio/jobs/{jobId}/cancel
DELETE /api/studio/jobs/{jobId}
```

取消应幂等。运行中的任务必须先取消成功才能删除。

### 8.7 导出任务结果

```http
POST /api/studio/jobs/{jobId}/export
```

```json
{
  "path": "D:/export/result.csv",
  "fileType": "csv",
  "columns": ["simTime", "pid1.PV"],
  "header": {
    "rowCount": 3,
    "features": ["tag", "description", "unit"]
  },
  "timeAnchor": {
    "type": "start",
    "value": "2026-08-29T08:00:00+08:00"
  },
  "csv": {"delimiter": ",", "encoding": "utf-8-sig"},
  "xlsx": {"sheetName": "数据"}
}
```

导出配置：

- `fileType`支持`csv | xlsx`。
- `header.rowCount`为0–5。
- `header.features`数量必须等于rowCount，顺序即title行顺序。
- title特征只支持`tag | name | description | unit | dataType`。
- `timeAnchor.type`为`start | end`，二选一；value是第一条或最后一条数据的绝对时间。
- CSV只配置分隔符和编码；XLSX只配置sheet名称。

首版不支持字体、颜色、合并单元格和多个sheet。路径由Go保存对话框选择。

## 9. Runtime API

### 9.1 Runtime 状态机

```text
stopped → starting → running → stopping → stopped
               ↘ failed ←──────────────┘
```

规则：

- `start` 只允许从 `stopped` 或已清理完成的 `failed` 进入。
- `stop` 幂等。
- Engine线程异常必须进入 `failed`。
- `failed` 必须包含 `lastError`。
- Runtime状态由 Python维护，Go和前端不得独立推算。

### 9.2 启动 Runtime

```http
POST /api/runtime/start
```

请求：

```json
{
  "content": "dsl_version: 2\n...",
  "runtimeName": "tank-project",
  "cycleTime": 0.5,
  "opcUaHost": "0.0.0.0",
  "opcUaPort": 18951,
  "archiveEnabled": true,
  "alarmRules": []
}
```

首版直接传已发布或已编译的 DSL 内容，避免 Python依赖调用方本地绝对路径。启动成功必须表示：

- DSL通过校验
- Engine线程已启动
- OPC UA已监听
- 归档已就绪（启用时）

任何一步失败都回滚本次已创建资源。

### 9.3 停止 Runtime

```http
POST /api/runtime/stop
```

停止顺序：

```text
停止接收新写入
→ 停止 OPC UA
→ 通知 Engine线程退出
→ 刷新并关闭归档
→ 清理运行期资源
→ runtimeState=stopped
```

### 9.4 查询状态

```http
GET /api/runtime/status
```

```json
{
  "runtimeState": "running",
  "runtimeName": "tank-project",
  "cycleCount": 120,
  "simTime": 60.0,
  "cycleTime": 0.5,
  "safeState": false,
  "opcUaState": "running",
  "archiveState": "running",
  "startedAt": "2026-08-28T10:00:00+08:00",
  "lastError": null
}
```

### 9.5 Tag catalog与快照

```http
GET /api/runtime/tags
GET /api/runtime/snapshot
```

Tag：

```json
{
  "tags": [
    {
      "name": "pid1.SV",
      "dataType": "number",
      "description": "设定值",
      "writable": true,
      "forceable": true,
      "display": true,
      "plotScaleRef": 1.2
    }
  ]
}
```

Snapshot：

```json
{
  "cycleCount": 120,
  "simTime": 60.0,
  "values": {
    "pid1.PV": 0.72,
    "pid1.SV": 0.8
  }
}
```

### 9.6 受控写入

```http
POST /api/runtime/writes
```

```json
{
  "writes": [
    {"tag": "pid1.SV", "value": 0.9}
  ]
}
```

响应：

```json
{
  "writeId": "uuid",
  "state": "accepted",
  "acceptedCycleCount": 120
}
```

```http
GET /api/runtime/writes/{writeId}
```

写入由 Engine在周期边界原子应用。HTTP写入和 OPC UA写入必须调用同一个 Python写入服务，使用同一套可写性、范围和 PID模式校验。

### 9.7 强制与质量码

```http
GET    /api/runtime/forces
PUT    /api/runtime/forces/{tag}
DELETE /api/runtime/forces/{tag}

GET    /api/runtime/qualities
PUT    /api/runtime/qualities/{tag}
DELETE /api/runtime/qualities/{tag}
```

强制请求：

```json
{
  "value": 50.0,
  "durationSeconds": 60
}
```

质量码请求：

```json
{
  "quality": "Bad"
}
```

不再保留独立 `/params` 和 `/override` 正式接口。

### 9.8 报警

```http
GET  /api/runtime/alarms
GET  /api/runtime/alarm-events?limit=100
POST /api/runtime/alarms/{alarmId}/ack
POST /api/runtime/alarms/ack-all
```

报警规则在 Runtime启动请求中一次性传入。运行中首版不支持修改规则，避免配置态与运行态不一致。

### 9.9 历史数据

```http
GET    /api/runtime/history
GET    /api/runtime/history/{sessionId}/values
POST   /api/runtime/history/{sessionId}/export
DELETE /api/runtime/history/{sessionId}
```

查询参数：

```text
tags=pid1.PV,pid1.SV
from=<ISO time or sim time>
to=<ISO time or sim time>
maxPoints=2000
```

首版继续使用现有文件归档。`maxPoints` 只要求做简单等距或极值保留下采样，不引入时序数据库。

## 10. WebSocket协议

### 10.1 建立连接

```text
ws://127.0.0.1:<port>/api/runtime/stream
```

连接后客户端发送：

```json
{
  "type": "subscribe",
  "tags": ["pid1.PV", "pid1.SV"]
}
```

服务确认：

```json
{
  "type": "subscribed",
  "tags": ["pid1.PV", "pid1.SV"]
}
```

实时帧：

```json
{
  "type": "snapshot",
  "cycleCount": 120,
  "simTime": 60.0,
  "values": {
    "pid1.PV": 0.72,
    "pid1.SV": 0.8
  }
}
```

心跳：

```json
{
  "type": "heartbeat",
  "timestamp": 1787882400000,
  "runtimeState": "running"
}
```

运行失败事件：

```json
{
  "type": "runtime-error",
  "code": 3005,
  "message": "运行线程异常退出"
}
```

每个客户端只保留最新一帧，慢客户端不得阻塞 Engine。订阅上限保留 5000。

## 11. 并发与生命周期

- Runtime Start/Stop使用同一把生命周期锁。
- 离线任务管理器和 Runtime共享互斥检查。
- Engine只允许自身线程推进周期。
- HTTP/UA写入只入队，不直接跨线程修改 Engine。
- Snapshot先写入最新值，再广播，再归档和评估报警。
- 报警每周期只评估一次。
- 归档异常不应静默；状态接口必须显示 `archiveState=failed`。

## 12. 待审核项

请重点确认：

1. Runtime启动是否统一传 DSL内容，不再传绝对文件路径。
2. 是否删除 `/params` 和 `/override`，统一为受控 writes。
3. 报警规则是否只允许随 Runtime启动加载。
4. 历史首版是否接受继续使用文件归档。
5. Studio和Runtime是否继续共用一个 Python服务进程。
