# mycode-core

共享词汇。agent、工具、提供商和桌面都引用这里的类型，所以一条消息从模型流到界面不必再翻译一遍。

这是叶子 crate：不依赖其它 mycode crate，也不知道磁盘布局、API 密钥或某个厂商的 JSON。

## 为什么单独成 crate

循环、协议适配和界面如果各自定义「一条助手消息」，字段一多就会漂。把对话数据放在最底层，上面的 crate 只能在这个形状上加行为，不能再发明一套。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `message` | `User` / `Assistant` / `ToolResult`，以及文本、思考、工具调用这些内容块。serde 用外部标签。`tool_label` / `tool_target` 给界面一行字：读了哪个文件、搜了什么、跑了哪条命令 |
| `events` | 循环向外广播的 `AgentEvent`：回合开始和结束、增量、工具结果、错误。必须 `Clone`，因为 agent 用 `tokio::broadcast` 分发 |
| `ids` | `CallId`，把一次工具调用和它的结果对上 |
| `tool` | `ToolSpec`：名字、说明、一份 JSON Schema。模型看见的规格和运行时校验用同一份 |
| `provider` | `Provider` trait、`Request`、`StreamEvent`、有界 `EventStream`、已脱敏的 `ProviderError` |
| `error` | 循环自己的 `MycodeError`，和提供商错误分开 |

## Provider 端口

`Provider::stream` 收一个已校验的 `Request` 和取消令牌，返回 `EventStream`。事件是文本增量、思考增量、工具调用增量、用量和停止原因。

请求编码上限是 8 MiB，在钩子改写之后、真正发出之前检查。流有固定容量；丢掉接收端会结束生产端，避免一个停住的界面把模型响应当内存攒住。

`ProviderError` 在构造时脱敏并截断。JSON 里的密钥字段、`Bearer`、`sk-` 一类前缀和常见赋值形式会被换成占位符。错误可以进日志和横幅，原始响应体不行。

`ReasoningLevel` 的拼写对齐 models.dev 的 effort，外加开关模型用的 `on` / `off`。core 只携带这个枚举，不决定某个厂商怎么写进请求体。

## 不放在这里的东西

- 把请求编成 Anthropic 或 OpenAI JSON（`mycode-providers`）
- 决定何时压缩历史（`mycode-app` 的钩子）
- 把事件画成气泡（`mycode-desktop`）
