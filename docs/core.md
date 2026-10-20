# mycode-core

`mycode-agent`、`mycode-tools`、`mycode-providers` 和 `mycode-app` 直接依赖这里的类型；`mycode-frontend`、`mycode-desktop` 和 `mycode-cli` 经由它们使用。它不依赖任何其它 mycode crate，不知道磁盘布局、API 密钥或厂商 JSON。

## 模块

| 模块 | 内容 |
| --- | --- |
| `message` | `Message`、内容块、`Usage`、`StopReason`、工具结果配对 |
| `events` | `AgentEvent`、`MessageDelta`、`TurnOutcome` |
| `ids` | `CallId` |
| `tool` | `ToolSpec`、`inline_schema_refs` |
| `provider` | provider 端口：`Request`、`Provider`、`StreamEvent`、`EventStream`、`ProviderError` |
| `error` | `MycodeError` |

## 对话数据

`Message` 只有 `User`、`Assistant`、`ToolResult` 三个变体；serde 使用外部标签并拒绝未知字段。内容块是 `Text`、`Thinking`、`ToolCall`、`Image`（`BinaryData`：base64 字节和 MIME 类型）。

- `SharedMessage = Arc<Message>`。历史、provider 请求和事件广播共享同一份消息，克隆只增加引用计数。
- `AssistantMessage` 带 `usage` 和 `stop_reason`。`stop_reason` 是 `Stop`、`ToolUse`、`Length`、`Error`。
- `ThinkingBlock` 有 `text`、可选 `signature`、可选 `replay`。`signature` 是厂商的推理完整性字段（例如 Anthropic thinking signature），适配器必须原样往返，包括 `text` 为空的 signature-only 块。`replay` 是同一条线路上下一次请求要带回去的厂商原始数据（MiniMax reasoning details、Anthropic redacted thinking）。
- `ToolCall { id, name, arguments }`。`id` 是厂商给出的不透明字符串，适配器不能解析它来恢复其它标识。
- `ToolResultMessage { tool_call_id, content, is_error, details }`。`details` 只给界面（结构化 diff、cwd 等），不进入模型上下文。
- `Usage`：`input_tokens`、`output_tokens`、`cache_read_tokens`、`cache_write_tokens`、`prompt_tokens`。`prompt_tokens` 是上下文计量用的提示大小。OpenAI 的 `prompt_tokens` 已包含缓存读取，Anthropic 的 `input_tokens` 不包含，所以适配器要填两者之和。

`tool_target(name, arguments)` 生成一行目标（路径、查询、命令），`tool_label` 在前面加上工具名。目标会折叠空白并截到 160 字符。`interrupted_response_text(detail)` 生成 `[error] the response was interrupted: …`，控制字符折叠成空格，detail 最多 240 字符。

`pair_tool_results(messages)` 修复崩溃、窗口关闭或回合中断后留下的不完整工具配对。助手消息后面的一组结果按它们在账本里的出现顺序保留，每个 call id 恰好一个；缺失的调用按 call 顺序补 `is_error` 结果，多余和重复的结果丢掉。修复是确定性的，所以重复请求的字节不变；返回是否有改动。会话账本保留原始形状，`mycode-agent` 只在请求副本上调用它。

## 事件

`AgentEvent` 是循环向外广播的协议，必须 `Clone`，因为 `mycode-agent` 用 `tokio::broadcast` 分发：

- `TurnStarted`、`TurnEnded(Completed | Aborted)`
- `MessageDelta`、`MessageAdded(SharedMessage)`
- `ToolStarted { call_id, name, target }`、`ToolProgress { call_id, message }`、`ToolCompleted { call_id, result }`
- `CacheDisrupted`、`Compacting(bool)`、`Error(MycodeError)`

`CacheDisrupted` 表示下一次请求的前缀已被压缩改写。`Compacting(true/false)` 是请求钩子里的摘要请求开始和结束。两者都由应用层的请求钩子发出，必须和普通事件走同一条总线：走旁路时，慢消费者会把它们配到上一条提示。`MessageDelta` 是 `TextDelta`、`ThinkingDelta`、`ToolCallStarted { id, name }`（一个工具调用开始流式输出时至多发一次；线路允许时在它的第一个参数增量之前）、`ToolCallDelta { id, partial_json }`；`partial_json` 本身不保证是合法 JSON。

`CallId` 是透明的字符串 newtype，用在 `ToolStarted`、`ToolProgress`、`ToolCompleted` 事件上。消息里的 `ToolCall.id` 和 `ToolResultMessage.tool_call_id` 是普通 `String`，因为它们是厂商给的不透明值。

## Provider 端口

`Provider::stream(&Request, CancellationToken)` 返回 `Result<EventStream, ProviderError>`：建流前的失败（认证、请求编码等）是 `Err`，之后的失败是流里的终止事件。流里是 `TextDelta`、`ThinkingDelta`、`ToolCallStarted`、`ToolCallDelta`，最后恰好一个终止事件：`Done { message }`（完整助手消息，用量和停止原因都在里面）或 `Error(ProviderError)`。生产端必须响应取消，并在 `send` 返回 `false` 后停止。

`Request` 字段：

- `system_prompt: Arc<[String]>`：有序片段。
- `messages: Vec<SharedMessage>`。
- `tools: Arc<[ToolSpec]>`。
- `reasoning`、`max_output_tokens`、`reasoning_token`、`prompt_cache_key`。

应用层填 `max_output_tokens` 的顺序是设置里的 `maxOutput`、目录的 `limit.output`、最后的 32000。`prompt_cache_key` 通常是会话 id。`reasoning_token` 用于目录里存在、但内置档位没有的 effort 拼写，原样发送。

`Request::validate` 在 `before_request` 钩子改写之后、发给 provider 之前检查 JSON 编码大小，上限 8 MiB（`MAX_REQUEST_ENCODED_BYTES`）。编码失败是 `Protocol`，超限是 `Rejected`。

`EventStream` 的通道容量是 16（`EVENT_STREAM_CAPACITY`）。单个事件编码后最多 1 MiB（`MAX_EVENT_ENCODED_BYTES`）；超限或无法编码的事件会被替换成一个协议错误终止。终止只能被认领一次，终止之后不能再发增量。接收端先排空已排队的事件，再观察取消。生产者全部消失但没有发出终止事件时，合成一个 `Protocol` 错误，不会静默 EOF。丢弃接收端会取消请求并关闭通道，阻塞的生产者随之醒来。

`ProviderError` 只有五类：`Cancelled`、`Unavailable`、`Timeout`、`Rejected`、`Protocol`，可带 HTTP `status`。消息在构造和反序列化时脱敏并截断：最多扫描 64 KiB，保留 512 字符，超出后追加 `… [truncated]`。JSON 里的敏感键（authorization、api key、token、secret、password、cookie 等）、`Bearer`、`sk-` / `sk_` 前缀和常见赋值形式会被换成 `[REDACTED]`。原始响应体从不保留。

`ReasoningLevel` 是 `off`、`minimal`、`low`（默认）、`medium`、`high`、`xhigh`、`max`、`on`，拼写对齐 models.dev；`none` 也解析为 `off`。`effort_token()` 返回线路上的 effort：`Off` 是 `none`，`On` 没有 effort 字段。core 只携带枚举，不决定厂商请求体。

## 工具规格

`ToolSpec { name, description, params_schema }` 是注册表生成、provider 使用的工具描述。运行时校验和 provider spec 都来自同一个 `params_schema`。

`inline_schema_refs` 把本地 `#/$defs/...` 和 `#/definitions/...` 引用展开，并删掉 `$defs` / `definitions`。已经在展开栈里的引用，或深度达到 32 层的引用（`SCHEMA_REF_DEPTH`），收成 `{type: object}`，保留原节点的 `description`。无法解析的 `$ref` 会被删掉，兄弟字段继续展开。

## 其它

`MycodeError` 是循环自己的错误：`Io`、`Serde`、`Provider`、`Tool`。负载都是字符串，保证 `Clone` 和序列化，可以随事件广播。

本 crate 声明了全局分配器 `mimalloc`。这是全工作区唯一的 `#[global_allocator]`，桌面二进制和所有测试目标共用它；再加一个会编译失败。

`unix_now()` 返回自 Unix 纪元起的时长；系统时钟早于纪元时返回 0。

## 不在这里

- 把 `Request` 编成厂商 JSON、传输和鉴权：`mycode-providers`（见 [providers.md](providers.md)）
- 选择模型、压缩历史、投影会话：`mycode-app`（见 [app.md](app.md)）
- 渲染界面：`mycode-desktop`（见 [desktop.md](desktop.md)）
