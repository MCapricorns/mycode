# mycode-providers

把一条已配置的端点变成 `mycode-core` 的 `Provider`。厂商差异是数据：`kind` 只有三种，新厂商加一行设置即可，不必再写一个 crate。

密钥不进这个 crate 的配置。`ResolvedProvider::resolve` 由调用方传入密钥和 User-Agent。设置里没有密钥字段。

## 设计思路

三种协议各有一个「组请求体」和一个「帧归约器」，共用同一个 SSE 驱动。驱动只负责把传输层的字节流变成 `StreamEvent`，取消令牌一响就停。传输是 `SseTransport` trait，生产用 `ReqwestTransport`，测试或替换实现可以注入别的传输。连接超时 10s。模型流的读超时是块与块之间的空闲上限 180s（有字节就重置），不是整次请求的硬截止。

所有请求都带调用方传入的 User-Agent，生产环境固定是 `mycode-app` 的 `default_user_agent()`（`MYCode-Agent/<版本> (<平台> <内核>; <架构>; <shell>)`），不是设置项。reqwest 只开 `native-tls`（操作系统 TLS）和 `system-proxy`，不启用会再编一套加密库的 rustls 特性。

出站请求先解析域名，再只连接检查过的地址（`http_pin`），重定向手动跟随、最多 5 跳、每跳重新检查。`PublicHttps`（网页检索、更新下载）要求 https、443 端口，并且解析结果全部是公网地址。GitHub 发布用的 `api.github.com`、`github.com`、`www.github.com`、`codeload.github.com`、`uploads.github.com` 和 `*.githubusercontent.com` 是例外：DNS 里夹了私网、ULA 或链路本地地址时，丢掉这些地址，只连接剩下的公网地址。任何主机的解析结果整段都是 `198.18.0.0/15`（本机 fake-ip，如 Clash TUN）时可以连接这些地址；和回环、RFC1918、链路本地或 ULA 混在一起则拒绝。其他主机的混合解析、字面私网地址、回环和链路本地仍然拒绝。`CheckRedirect`（模型流、目录刷新）的第一跳可以整段是公网或整段是私网（本机模型），之后的跳转必须全是公网；OAuth 用同一钉住模式，但不跟随重定向。

端点拼接避免把路径写两遍：Anthropic 的 base 以 `/v1/messages` 结尾时原样使用，以 `/v1` 结尾时只补 `/messages`，否则补 `/v1/messages`；Chat Completions 补 `/chat/completions`，Responses 补 `/responses`，已经带上时不重复。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `anthropic_messages` | Anthropic Messages：`x-api-key`、`anthropic-version: 2023-06-01`、消息体与事件归约。思考签名原样回放 |
| `openai_completions` | Chat Completions：`Authorization: Bearer`，`/chat/completions` |
| `openai_responses` | Responses API：同样的 Bearer，路径 `/responses` |
| `family` | 按服务商 / 模型族的策略表：思考字段、推理回放、显式缓存、`prompt_cache_key` 排除名单、用量字段名。每张表首个命中的行生效，末行是通用兜底 |
| `cache` | 提示缓存断点和 `prompt_cache_key` 探测 |
| `wire_common` | 三种协议共用的消息、工具字段和用量整理 |
| `xml_tool_calls` | 从文本里收回模型用 XML 写出的工具调用，补那些不走原生 tool 字段的端点 |
| `sse` | SSE 帧切分，单帧最多 1 MiB |
| `driver` | 用归约器把传输流驱动成 `EventStream`。`stream()` 在运行时里 `spawn` 这段驱动 |
| `transport` | `SseTransport` 与 reqwest 实现 |
| `http_pin` | 解析、检查并钉住地址的 HTTP 请求 |
| `oauth` | Copilot、Codex 的设备码登录、轮询、刷新。令牌解析后仍交给调用方存进 `secrets.json` |
| `catalog` | models.dev 云端目录的下载、家目录缓存与条件刷新，以及编进二进制的 Codex / OpenDesign 硬编码保底行。`catalog/opendesign` 是 OpenDesign 网关的实测模型表 |

## 思考强度

请求带 `ReasoningLevel` 时按 `family.rs` 的表写字段，对齐 opencode `transform.ts`。Chat Completions 先看主机（OpenRouter、DashScope、OpenDesign），再看模型族：

| 族 | Chat Completions / Responses | Anthropic Messages |
| --- | --- | --- |
| GLM（模型含 `glm`，或 z.ai / bigmodel / zhipu） | `thinking {type: enabled, clear_thinking: false}` + `reasoning_effort` | `thinking {type: enabled, clear_thinking: false}` + `output_config.effort` 为 `low` / `high` / `max`，不发 `budget_tokens` |
| MiniMax | `thinking {type: adaptive}` + `reasoning_split: true`，只有开关 | `thinking {type: adaptive}`，不发 `reasoning_split` |
| Kimi / K3 | K3 不发 `thinking`，只发 `reasoning_effort` `low` / `high` / `max`（先看运行时 / 缓存目录，再退回内置保底行；目录里没有该模型 id 时仍按 id 映射发送）；其它 Kimi 发 `thinking` 开关，运行时目录公布了对应档位时再带 `reasoning_effort` | `thinking {type: adaptive, display: summarized}` + `output_config.effort` |
| DeepSeek | `deepseek-v4` / `deepseek-flash`：`thinking` + `reasoning_effort` `low` / `high` / `max`；旧 id 只有开关 | 走 Claude 规则 |
| Qwen | `enable_thinking` + `reasoning_effort` | 走 Claude 规则 |
| OpenRouter | `reasoning.effort` | — |
| OpenDesign（`open-design.ai`） | 只发 `reasoning_effort`，关闭为 `none` | — |
| DashScope | `enable_thinking` + `reasoning_effort` | — |
| Claude | — | Opus / Sonnet 4.6 和 4.7 起的版本用 `adaptive` + `output_config.effort`（4.7 起 `display: summarized`）；其它用 `budget_tokens`，必要时抬高 `max_tokens` |
| 其它 | `reasoning_effort`（Responses 为 `reasoning.effort`） | 同上 |

K3 识别：路径段为 `k3`、`k3-*`、`kimi-k3`、`kimi-k3-*`；`k30`、`mk3` 之类不算。关闭思考时各族发 `{type: disabled}` 或 `enable_thinking: false`；K3 和 `kimi-k2.7-code` 没有关闭开关，字段直接省略。

推理回放：GLM、DeepSeek、Kimi 在下一次 Chat Completions 请求里带 `reasoning_content`，MiniMax 带 `reasoning_details`，其它省略。Messages 回放有签名的思考块，GLM 也接受无签名的上一轮思考文字。

## 输出上限

输出上限顺序：设置里的 `maxOutput` → 目录 `limit.output` → 32000。Anthropic Messages 必须带 `max_tokens`（请求未填时也用 32000）；公布了的 `limit.output` 原样发送，不再取最小值。应用层回合总会填上这个上限，因此 Chat Completions / Responses 也会带上对应字段。压缩摘要请求有自己的上限。

## 提示缓存

- **Anthropic Messages**：每个请求最多 4 个 `cache_control: {type: ephemeral}` 断点：最后一个工具、最后一段 system，再加最后两个消息块。MiniMax 改为钉住开头和结尾，不写进带思考的消息
- **Chat Completions**：OpenRouter 上的 Anthropic / Gemini、DashScope Qwen、Z.AI / 智谱也写同样的断点（最后一个工具、第一条 system，再加之后的首尾消息），不改写带回放推理的消息，保证前缀字节不变
- **`prompt_cache_key`**：Chat Completions 和 Responses 发会话 id（截到 64 个字符）。已知会拒绝或自有缓存机制的主机（OpenRouter、Z.AI、DeepSeek、MiniMax、DashScope、Moonshot / Kimi、Groq、Together 等）从不发；其它主机先发一次，400 且错误里提到这个字段时去掉字段重试一次，并在本进程内不再给该主机发。这些主机靠自动前缀缓存。OpenRouter 另带 `x-session-id` 粘性路由头
- **用量**：一套解析器接受各家字段名。缓存读取认 `cache_read_input_tokens`、`cached_tokens`（含 `prompt_tokens_details` / `input_tokens_details` 里的嵌套字段）、`prompt_cache_hit_tokens` 等；缓存写入认 `cache_creation_input_tokens` 等

## 工具 schema

发出前对每个工具的参数 schema 调用 `mycode_core::inline_schema_refs`（`wire_common::tool_parameters`）：本地 `$ref` 展开，丢掉 `$defs` / `definitions`。环引用或深于 32 层时换成 `{type: object}`。Moonshot 的校验器遇到 schemars 生成的 `$ref` + `$defs` 会报无限递归，所以所有服务商都收展开后的 schema。

## 目录

`catalog` 放在这里是因为刷新是一次 HTTP。`mycode-config` 保持无网络。

离线基线是编进二进制的两行硬编码：OpenAI Codex 预设（`catalog.rs` 的 `openai_codex_preset`）和 OpenDesign 实测表（`catalog/opendesign` 的 `CURATED`）——models.dev 都不含它们。没有「内置 models.dev 快照」：云端目录只从网络来，维护流程见 `docs/model-catalog-updates.md`。

运行时家目录的 `catalog-cache.json` 是唯一的云端副本。缓存 24 小时内不发请求（`DEFAULT_MAX_AGE_SECS`），之后用 `If-None-Match` 条件请求 `https://models.dev/api.json`；失败时继续用已有缓存，缓存也没有就用保底的两行。下载和缓存物化时都会把 Codex / OpenDesign 行注入文档，所以它们不受云端数据影响。云端文档最大 32 MiB。缓存是旧格式版本时直接换成保底；解析失败时，原字节备份为 `catalog-cache.json.broken-*`，再写入保底，下次启动不会反复提示。读不出来则不动原文件，内存里用保底。设置页的预设、上下文窗口、输出上限、思考档位、工具调用和价格都从这份目录来，models.dev 数据更新（含 Copilot 模型）不需要发版。

解析时跳过带 `status` 的模型、Bedrock / Vertex / Azure / Google SDK 的服务商；每个提供商最多 512 个模型，字符串字段最多 8 KiB。思考档位只取 models.dev 公布的 `reasoning_options`（开关、effort 列表），effort 为空时取 `variants` 的键，不补猜测的档位；effort 最多 16 个，未知标识符保留，丢掉 `default` 和非标识符。设备登录服务商（`github-copilot` / `openai-codex` 的 device-code）用固定端点，不用云端文档里的控制台 URL。自定义端点按服务商 id、再按 base URL、最后按模型 id 在全目录里找行。

## OpenDesign

`catalog/opendesign` 接 OpenDesign 网关（`https://amr-link.open-design.ai/v1`，Chat Completions），数据做法参照 `@lokeraar/pi-opendesign-bridge`。网关的 `/models` 只给 id、输入模态和 `metadata.context_limit` / `context_budget`，没有思考档位，也没有真实的输出上限，所以客户端不实时拉取，只用 `CURATED` 实测表：针对该网关测过的 id。网关拒绝的档位不列（GLM-5.3 只有 low / high / max，GPT-6 没有 minimal，K2.7 Code 没有 max）；`reasoning_effort: "none"` 既被接受、又真的关掉思考的模型（DeepSeek、GPT-6 Luna、MiMo）才有「关」；输出上限取网关 budget 和厂商上限里较小的那个。

任何上限都给提示留 2048 token：上限等于窗口时网关会拒绝。bridge 对新 id 逐档探测（每个约 15 次付费请求）这里不做。

OpenDesign 是目录里的普通 provider 行（id `opendesign`），出现在设置页模型库列表里，粘贴 API key 即添加；再次添加替换它的模型列表（端点和覆盖不变），不会多出 `opendesign-2`。上下文、输出和思考档位走和其它服务商一样的查找。Chat Completions 的思考表里它是一个主机行，排在 GLM / Kimi / DeepSeek 这些模型行之前：该网关对所有模型都只发 `reasoning_effort`（关闭发 `none`），不发厂商的 `thinking` 对象。

## 登录

`github-copilot`、`openai-codex` 是设备码登录，其它服务商（包括 `xai`）贴 API key。旧版本缓存里 `xai` 的 `oauth` 标记按普通 API key 服务商处理。每份目录文档都注入内置的 `openai-codex` 预设（Responses，`https://chatgpt.com/backend-api/codex`）：models.dev 不含 Codex 行，所以它和 OpenDesign 一样完全手工维护，模型 id 和思考档位都在 `openai_codex_preset` 里，没有实时拉取。预设只保留 `gpt-5.6`、`gpt-6`、`gpt-6.1` 三个系列的模型 id，其它族（`o3`、5.3/5.4/5.5 等）不进目录。Copilot 的模型行则直接来自 models.dev 云端目录，不硬编码。OAuth 请求仍走钉住地址的客户端（`CheckRedirect`），调用方通过 `PinnedCall` 显式传入 User-Agent 和总超时（默认 30s），不跟随重定向；调用方的代理不生效。错误信息只带状态码和字段名，不带令牌。

## 一次解析

```text
ProviderSettings + model id + api key + user agent
        │  kind 必须是三种之一，model 必须在该提供商的列表里
        ▼
ResolvedProvider { id, kind, endpoint, headers, model }
        │  绑上 Arc<dyn SseTransport>
        ▼
WireProvider::stream → EventStream
```

未知 `kind` 或未提供的模型返回 `Rejected`，不发出请求。请求体序列化失败时用空体（调用前 `Request::validate` 已经挡住明显非法的请求）。HTTP 408、429 和 5xx 是 `Unavailable`，其它 4xx 是 `Rejected`。

## 不放在这里的东西

选哪个提供商、密钥从哪读、压缩、把流画出来，都属于 `mycode-app` 和桌面。本 crate 收到的是已经决定好的端点和密钥。
