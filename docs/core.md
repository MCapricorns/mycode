# mycode-core

共享词汇。agent、工具、提供商和应用核都引用这里的类型，所以一条消息从模型流到界面不必再翻译一遍。

这是叶子 crate：不依赖其它 mycode crate（也不依赖 `mycode-config`），不知道磁盘布局、API 密钥或某个厂商的 JSON。

## 为什么单独成 crate

循环、协议适配和界面如果各自定义「一条助手消息」，字段一多就会漂。把对话数据放在最底层，上面的 crate 只能在这个形状上加行为，不能再发明一套。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `message` | `Message`：`User` / `Assistant` / `ToolResult` / `Custom`；内容块 `Text`、`Thinking`（带可选 `signature` 和 `replay`）、`ToolCall`、`Image`。serde 用外部标签。`SharedMessage = Arc<Message>`，历史和请求共享同一份消息。`Usage` 带输入、输出、缓存读写和 `prompt_tokens`（给上下文计量用）。`tool_target` / `tool_label` 给界面一行字：读了哪个文件、搜了什么、跑了哪条命令。`interrupted_response_text` 是流中断时那一行 `[error] the response was interrupted: …` |
| `events` | 循环向外广播的 `AgentEvent`：`TurnStarted`、`MessageDelta`、`MessageAdded`、`ToolStarted` / `ToolProgress` / `ToolCompleted`、`TurnEnded(Completed \| Aborted)`、`Error`。必须 `Clone`，因为 agent 用 `tokio::broadcast` 分发 |
| `ids` | `CallId`，把一次工具调用和它的结果对上 |
| `tool` | `ToolSpec`：名字、说明、一份 JSON Schema。模型看见的规格和运行时校验用同一份。`inline_schema_refs` 把本地 `$ref` 展开、去掉 `$defs`，循环引用或超过 32 层收成 `{type: object}` |
| `provider` | `Provider` trait、`Request`、`ReasoningLevel`、`StreamEvent`、有界 `EventStream`、已脱敏的 `ProviderError` |
| `error` | 循环自己的 `MycodeError`，和提供商错误分开 |

## Provider 端口

`Provider::stream` 收一个 `Request` 和取消令牌，返回 `EventStream`。流里是 `TextDelta`、`ThinkingDelta`、`ToolCallDelta`，最后恰好一个终止事件：`Done { message }`（完整的助手消息，用量和停止原因都在里面）或 `Error`。

`Request` 除了 system prompt、共享的历史和工具规格，还带 `reasoning`、`max_output_tokens`（应用层按设置 `maxOutput` → 目录 `limit.output` → 32000 填入）、`reasoning_token`（目录里有、内置档位里没有的 effort 拼写）和 `prompt_cache_key`（应用层填会话 id）。

请求编码上限是 8 MiB（`MAX_REQUEST_ENCODED_BYTES`），由 `Request::validate` 在钩子改写之后、真正发出之前检查。流的容量是 16 个事件，单个事件编码后最多 1 MiB，超限换成一个协议错误终止。丢掉接收端会取消请求并关闭通道，阻塞的生产端随之醒来，避免一个停住的界面把模型响应当内存攒住。生产端没发终止就消失，会合成一个协议错误，而不是静默结束。

`ProviderError` 有五类：`Cancelled`、`Unavailable`、`Timeout`、`Rejected`、`Protocol`。消息在构造和反序列化时脱敏并截断（最多扫描 64 KiB，保留 512 字符）。JSON 里的密钥字段、`Bearer`、`sk-` / `sk_` 前缀和常见赋值形式会被换成 `[REDACTED]`。错误可以进日志和横幅，原始响应体不行。

`ReasoningLevel` 是 `off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`、`on`，拼写对齐 models.dev 的 effort（`none` 也解析为 `off`），`on` / `off` 给只有开关的模型。core 只携带这个枚举，不决定某个厂商怎么写进请求体。

## 不放在这里的东西

- 把请求编成 Anthropic 或 OpenAI JSON（`mycode-providers`）
- 决定何时压缩历史（`mycode-app` 的钩子）
- 把事件画成气泡（`mycode-desktop`）
