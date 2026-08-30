# DataFactory Go 与前端交互设计稿

状态：待审核  
版本：0.1  
范围：DataFactory Studio 与 Runtime Monitor 的桌面交互边界

## 1. 目标

减少 React、Go、Python 三层重复业务逻辑，同时保留 Wails 对桌面应用有价值的能力。

核心原则：

> Go负责桌面，Python负责业务，React负责界面状态和交互。

## 2. 产品界面边界

首版仍可打包为一个桌面 EXE，但界面明确分为两个工作区：

### Studio

- 选择工作目录并显示其中的DSL文件。
- 从列表中选择一个DSL文件。
- 显示DSL特征摘要，不显示或编辑DSL正文。
- 配置运行参数并启动调试运行。
- 显示校验结果、运行趋势并导出数据。

DSL由用户在外部IDE中编辑和保存，Studio不提供编辑器和保存能力。

### Runtime Monitor

- 选择并启动已验证组态。
- 查看 Runtime、OPC UA和归档状态。
- 查看实时值和趋势。
- 查询历史数据。
- 执行受控写入、强制和质量码操作。

首版不要求拆成两个 EXE。待功能和部署边界稳定后，再决定是否拆出独立 Runtime Monitor，避免现在提前增加构建和发布复杂度。

## 3. 总体调用关系

```text
React WebView
├── Wails Binding
│   ├── 选择文件/目录
│   ├── 保存路径
│   └── 获取 Python连接信息
│
├── HTTP → Python
│   ├── Studio校验与任务
│   ├── Runtime启停与状态
│   ├── 写入、报警与历史
│   └── 导出
│
└── WebSocket → Python
    └── Runtime实时快照

Go/Wails
├── 启动/关闭 DataFactoryService
├── 分配 loopback端口
├── 文件选择器
├── 应用目录与日志目录
└── 窗口生命周期
```

## 4. Go职责

### 4.1 必须保留

- `DataFactoryService.exe` 查找、启动、健康检查和关闭。
- 生成随机 loopback端口。
- 强制 Python只绑定 loopback。
- Wails窗口生命周期。
- 打开/保存文件与目录对话框。
- 返回应用数据目录、模板目录和日志目录。
- 在应用关闭前请求 Python优雅停止。

### 4.2 不再承担

- DSL解析和校验。
- 工程拓扑和副本展开。
- Runtime状态机。
- 报警规则业务校验。
- 强制值、质量码和参数写入业务。
- 批量任务状态。
- 历史数据解析。
- 为每个 Python HTTP接口编写一个 Go代理方法。

## 5. 最小 Wails Binding

建议只保留下列接口。

### 5.1 应用服务

```go
type ServiceConnection struct {
    Host  string `json:"host"`
    Port  int    `json:"port"`
}

GetServiceConnection() (ServiceConnection, error)
RestartService() (ServiceConnection, error)
GetServiceLogPath() (string, error)
```

服务不提供鉴权，连接信息只包含host和port。

### 5.2 文件能力

```go
ChooseWorkDir() (string, error)
ChooseExportFile(format string) (string, error)
ChooseProjectFile() (string, error)
```

Go只负责原生选择对话框。目录扫描、DSL读取、校验和特征提取由Python完成。

### 5.3 应用目录

```go
GetAppPaths() (AppPaths, error)
```

```json
{
  "templatesDir": "...",
  "projectsDir": "...",
  "logsDir": "...",
  "cacheDir": "..."
}
```

不为每个目录单独增加 Binding。

## 6. 服务启动流程

```text
Wails Startup
→ 创建加载页
→ Go选择空闲 loopback端口
→ Go启动 DataFactoryService
→ 轮询 /api/service/health
→ 就绪后发送 app:service-ready
→ React调用 GetServiceConnection
→ 创建唯一 Python API Client
→ 进入主界面
```

服务启动事件只保留：

```text
app:service-starting
app:service-ready
app:service-error
app:service-exited
```

事件只表达服务进程生命周期，不携带 Runtime业务状态。

## 7. 前端 HTTP Client

前端创建唯一客户端：

```ts
interface ServiceConnection {
  host: string
  port: number
}

interface ApiClient {
  service: ServiceApi
  studio: StudioApi
  runtime: RuntimeApi
}
```

统一处理：

- Base URL
- JSON序列化
- 超时
- AbortSignal
- 标准错误解析

禁止各组件直接拼接 URL 或自行调用 `fetch`。

Python OpenAPI生成请求和响应类型；可以手写很薄的 fetch实现，不要求引入重量级请求框架。

## 8. 前端状态设计

不追求把所有状态放进一个 Store。按实际职责保留四个 store。

### 8.1 AppStore

负责：

```ts
serviceState: 'starting' | 'ready' | 'error' | 'exited'
connection: ServiceConnection | null
serviceError: ApiError | null
```

不保存 Runtime状态。

### 8.2 StudioStore

负责：

```ts
workDir: string | null
dslFiles: DslFileEntry[]
selectedPath: string | null
features: DslFeatures | null
validation: ValidationResult | null
runOptions: StudioRunOptions
activeJob: StudioJob | null
preview: PreviewData | null
```

StudioStore不保存DSL正文、dirty或savedContent。磁盘文件是唯一真相源。

### 8.3 RuntimeStore

负责：

```ts
status: RuntimeStatus
tags: RuntimeTag[]
latestSnapshot: RuntimeSnapshot | null
connectionState: 'idle' | 'connecting' | 'connected' | 'disconnected'
selectedTags: string[]
lastError: ApiError | null
```

`status.runtimeState` 来自 Python；`connectionState` 只表示 WebSocket状态。

### 8.4 HistoryStore

负责：

```ts
sessions: HistorySession[]
query: HistoryQuery
values: HistoryValues | null
loading: boolean
error: ApiError | null
```

历史查询与实时趋势分开，避免实时断线影响已加载历史数据。

## 9. Studio交互流程

### 9.1 选择工作目录

```text
用户点击选择工作目录
→ Wails ChooseWorkDir
→ GET /api/studio/dsl-files?workDir=...
→ 显示目录第一层DSL文件列表
```

工作目录只保存在当前应用内存。是否跨启动记忆上次目录作为待审核项。

### 9.2 选择文件和特征页面

```text
用户在列表中选择一个文件
→ POST /api/studio/inspect(path)
→ Python从磁盘读取并校验
→ 显示DSL特征、拓扑摘要和问题列表
```

特征页不显示DSL正文。首版显示版本、名称、时钟、节点数、组件分类、tag数量、可写tag数量、是否存在控制环、执行顺序和校验问题。

特征页提供：

```text
刷新文件列表 → 重新扫描工作目录
重新加载     → 重新读取当前文件并刷新特征和校验
```

### 9.3 调试运行

```text
配置cycles/cycleTime/sampleInterval/startTime
→ 点击运行
→ POST /api/studio/jobs，传dslPath和运行参数
→ Python再次从磁盘读取文件
→ 完整校验通过后启动
→ 轮询 GET /jobs/{id}
→ 运行中按offset增量读取已生成数据
→ 持续刷新趋势图
→ 完成后保留完整任务结果供导出
```

IDE保存后的内容在下一次运行时立即生效，不依赖“重新加载”。重新加载只影响特征页。页面卸载不自动取消任务；用户显式点击取消才取消。

### 9.4 导出

```text
用户点击导出数据文件
→ 打开导出配置对话框
→ 配置文件类型、数据列、title行数和每行特征
→ 设置起始时间或结束时间
→ Wails ChooseExportFile
→ POST /api/studio/jobs/{id}/export
→ 显示成功路径或错误
```

对话框只提供与数据语义相关的设置：

- CSV或XLSX。
- 导出列。
- 0–5个title行。
- 每个title行选择tag、名称、说明、单位或数据类型。
- 起始时间或结束时间，二选一。
- CSV分隔符/编码，或XLSX sheet名称。

不提供字体、颜色、边框、合并单元格等排版功能。

## 10. Runtime交互流程

### 10.1 启动

```text
用户点击启动
→ 确认 Studio无运行任务
→ 读取已保存且验证通过的 DSL v2
→ POST /api/runtime/start
→ GET /api/runtime/status
→ GET /api/runtime/tags
→ GET /api/runtime/snapshot
→ 建立 WebSocket
```

Runtime启动不得直接使用未保存编辑内容。Studio调试数据生成可以使用未保存内容，两种语义必须区分。

### 10.2 实时连接

```text
连接 WebSocket
→ 发送当前可见组件的 tag订阅并集
→ 接收 subscribed确认
→ snapshot进入趋势缓冲
→ heartbeat更新连接健康
```

断线策略：

- 保留最后值并标记 stale。
- 1、2、4、8、16、30秒退避重连。
- 重连前先请求 REST snapshot。
- 页面卸载关闭 socket和定时器。

保留当前实现中已经验证过的这些规则，不重新设计协议。

### 10.3 停止

```text
用户点击停止
→ POST /api/runtime/stop
→ 成功后关闭 WebSocket
→ 再次 GET /api/runtime/status
→ 清理实时缓存
```

停止失败时：

- 不清空 Runtime页面。
- 保留最后数据。
- 显示 Python返回的状态和错误。
- 允许用户重试停止。

### 10.4 Runtime异常

收到 `runtime-error` 或状态轮询发现 `failed`：

- 立即标记 Runtime失败。
- 冻结最后值并标记 stale。
- 不自动重启。
- 提供“停止清理”和“重新启动”操作。

## 11. 写入交互

### 11.1 普通写入

```text
用户输入新值
→ 前端做基础格式检查
→ POST /api/runtime/writes
→ 轮询 writeId或等待 snapshot确认
→ 显示 applied/failed
```

真正的可写性、范围和 PID模式校验只在 Python实现。前端校验只是改善体验，不能作为安全边界。

### 11.2 强制值

强制操作必须与普通写入在 UI上明确区分：

- 显示强制标记。
- 显示剩余时间。
- 提供单点解除。
- 停止 Runtime后自动清空。

### 11.3 OPC UA回写

OPC UA写入不经过 React或 Go，直接进入 Python统一写入服务：

```text
UA Client
→ Python OPC UA Server
→ Runtime Write Service
→ Engine周期边界应用
→ Snapshot/WebSocket反映结果
```

HTTP和UA必须共用同一套权限和范围规则。

## 12. 历史查看交互

```text
进入历史页
→ GET /api/runtime/history
→ 用户选择一次运行
→ 选择 tags和时间范围
→ GET /history/{id}/values?maxPoints=2000
→ 绘图
```

首版不做无限滚动、后台缓存、复杂多分辨率切片。单次最多返回适合绘图的点数即可。

## 13. 类型与契约

类型来源：

```text
Python Pydantic models
→ FastAPI OpenAPI
→ 生成 TypeScript types/client
```

Wails生成类型只描述：

- ServiceConnection
- AppPaths
- 文件选择结果

目标是删除前端业务接口中的 `any/as any`，但不要求一次消除所有 UI内部 `any`。

## 14. 错误展示

前端按错误作用域展示：

| 错误 | 展示位置 |
|---|---|
| 服务启动失败 | 全屏启动页 |
| DSL校验问题 | Studio特征页问题面板 |
| 离线任务失败 | Studio任务区域 |
| Runtime启动/停止失败 | Runtime状态栏 |
| 单次写入失败 | 对应写入控件 |
| WebSocket断线 | 实时数据 stale提示 |
| 历史查询失败 | 历史页面 |

不要把所有错误汇总到一个全局字符串，也不要同时弹 Toast和页面错误造成重复提示。

## 15. 从现有实现迁移的最小顺序

1. 建立新的 Python API模型和统一前端 ApiClient。
2. 修通前端 REST/WS统一连接配置。
3. 将前端 Runtime状态改为读取 Python `/runtime/status`。
4. Studio按文件路径启动任务，Python每次从磁盘重新读取DSL。
5. 将 Force/Quality/Write从 Go代理迁移到前端 ApiClient。
6. 将工程校验和编译迁入 Python。
7. 删除未使用的旧 Go Binding和重复 store字段。

每一步都必须保持可运行，不安排一次性大爆炸式重写。

## 16. 待审核项

请重点确认：

1. 首版继续使用一个 EXE、两个工作区，还是立即拆成两个 EXE。
2. Studio是否跨应用启动记忆上次工作目录。
3. Runtime是否只允许启动已保存文件。
4. Go是否只保留本文列出的最小 Binding。
5. 历史查看首版是否只做 tag、时间范围和最大点数三项查询。
