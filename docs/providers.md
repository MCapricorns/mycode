# mycode-providers

把一条已配置的端点变成 `mycode-core::Provider`。协议只有三种：`anthropic-messages`、`openai-completions`、`openai-responses`。厂商差异放在 `family.rs` 的数据表里，新厂商通常只改设置。

密钥不在 `settings.json`。`ResolvedProvider::resolve` 由调用方传入 key 和 User-Agent。

## 模块

| 模块 | 职责 |
| --- | --- |
| `anthropic_messages` | Messages 请求体、SSE 归约。签名思考块和 `redacted_thinking` 原样回放；GLM 也接受无签名思考文字 |
| `openai_completions` | Chat Completions 请求体、SSE 归约、`<think>` 拆分、XML 工具调用回收 |
| `openai_responses` | Responses 请求体、SSE 归约。固定 `store: false`，不向上游回放思考或图片 |
| `family` | 思考字段、推理回放、显式缓存、`prompt_cache_key` 排除名单、用量字段名。每张表首个命中行生效，末行是通用兜底 |
| `cache` | `cache_control` 断点、`prompt_cache_key` 探测与进程内拒绝记录 |
| `wire_common` | 三种协议共用的工具 schema、用量整理、停止原因映射、消息拼装 |
| `xml_tool_calls` | 从文本里回收 `<tool_call>` / `<minimax:tool_call>` 内的工具调用，合成 id |
| `sse` | `data:` 行切分；单行上限 1 MiB；不拼接多行 data 事件 |
| `driver` | 把传输流驱动成 `EventStream`，负责重试、取消和终态 |
| `transport` | `SseTransport` trait 与 reqwest 实现 |
| `http_pin` | DNS 解析、地址检查、钉住连接、手动重定向 |
| `proxy` | 系统代理或手动代理路由；本机地址直连 |
| `oauth` | Copilot / Codex 设备码登录、轮询、令牌刷新 |
| `catalog` | models.dev 下载与缓存，以及编进二进制的 Codex / OpenDesign 行 |

各协议的请求体要点：Anthropic 的 `system` 是 text block 数组，`tool_result` 会合并进同一条 user 消息；Chat Completions 的 system 用 `\n\n` 拼接，固定发 `stream_options.include_usage`，图片转 `image_url` 的 data URL；Responses 的 system 放在 `instructions`，工具是扁平结构。

## 解析与端点

`ResolvedProvider::resolve` 先去掉 `base_url` 末尾的 `/`，再按 `kind` 拼端点：

- Anthropic：base 以 `/v1/messages` 结尾原样使用；以 `/v1` 结尾补 `/messages`；否则补 `/v1/messages`。
- Chat Completions：以 `/chat/completions` 结尾原样使用，否则补上。
- Responses：以 `/responses` 结尾原样使用，否则补上。

请求头先放调用方的 `user-agent`，再放 `x-api-key` + `anthropic-version: 2023-06-01`，或 `authorization: Bearer <key>`。`kind` 未知或模型不在 `settings.models` 里时返回 `Rejected`，不发请求。调用前先跑 `Request::validate`；请求体序列化失败时用空 body。

## 传输、取消、重试

`ReqwestTransport` 每个进程只探测一次 TLS。连接超时 10s；读超时是 chunk 之间的空闲上限 180s，有字节就重置，不是整次请求的截止时间。错误响应体最多读 64 KiB。HTTP 408、429、5xx 归为 `Unavailable`，其它 4xx 归为 `Rejected`，其余为 `Protocol`。

流式请求先解析并检查地址，再连接；取消令牌触发时，请求、等待和重试退避都会立即结束。单帧上限 1 MiB，整个流累计上限 8 MiB，内容块或工具调用索引上限 64。超过累计上限会以 `Protocol` 错误终止，不会派发半个工具调用。

`driver` 对同一 body 最多请求 4 次（含第一次）。可重试的失败：

- 429：等待 1s / 4s / 10s。
- 408、500–503、507、529，或没有状态码的 `Unavailable` / `Timeout`：等待 400ms / 1.2s / 3.5s。
- 404、520–526、其它 4xx、`Rejected`、`Protocol`：不重试。

流已经开始后只在还没有发出任何事件时重试；一旦有 text、thinking 或 tool delta 出去，后续失败直接终止并保留已归约内容（`StopReason::Error`）。Anthropic 在输出前的 `overloaded_error` / `rate_limit_error` / `api_error` 分别按 529 / 429 / 500 处理，因此也走同一套重试。

另有两类“改 body 再试一次”的规则。它们在同一 body 的重试次数用完之后才触发，所以一次请求最多发出 5 次：

- 400 的错误文本包含 `prompt_cache_key`：删掉该字段重发，并把这个主机记入进程内拒绝集合。
- 400 的错误文本同时包含 `signature` 和 `thinking`：删掉所有 `thinking` / `redacted_thinking` 块后重发，保留 text 和工具调用；只剩思考的 assistant 消息整条移除。

`StopReason` 映射：`length` / `max_tokens` / `model_context_window_exceeded` 是 `Length`；`tool_calls` / `function_call` / `tool_use` 是 `ToolUse`；`error` / `network_error` / `content_filter` / `sensitive` / `refusal` 是 `Error`；其余是 `Stop`。有工具调用时最终记为 `ToolUse`，但被输出上限截断时仍是 `Length`。

## 钉住与代理

出站请求先解析域名，再只连接检查过的地址；重定向手动跟随，最多 5 跳，每跳重新检查，跨 origin 只保留 `accept`、`accept-language`、`content-type`。所有跳转都必须是 https，不能降级到 http。

- `PinMode::PublicHttps`：用于网页检索和更新下载。要求 https、443 端口，DNS 结果必须全是公网地址。`github.com`、`www.github.com`、`api.github.com`、`codeload.github.com`、`uploads.github.com`、`*.githubusercontent.com` 允许混入非公网地址，但只连接其中的公网地址。任何主机的解析结果如果整段都是 `198.18.0.0/15`（本机 fake-ip，如 Clash TUN），可以连接这些地址；只要混入回环、RFC1918、链路本地或 ULA 就拒绝。
- `PinMode::CheckRedirect`：用于模型流和目录刷新。第一跳可以全公网或全私网（本机模型），之后的跳转必须全公网。

代理由 `proxy` 设置决定：`System` 跟随系统代理，`Url` 让所有公网主机走指定代理。回环、`.local` 和字面私网地址永远直连。走代理的一跳由代理解析域名，不做地址钉住；仍会做 URL 级检查，并在本地 DNS 返回全私网地址时改为直连。OAuth 使用 `CheckRedirect` 且 `follow_redirects: false`，代理同样生效。

## 思考强度

Chat 表先看主机行（OpenRouter、DashScope、OpenDesign），再看模型行。下表按首个命中行排列。

| 族 | Chat Completions | Responses | Anthropic Messages |
| --- | --- | --- | --- |
| OpenRouter（`openrouter.ai`） | `reasoning.effort`；Off=`none`，On=`high` | 同左 | — |
| DashScope（`dashscope` / `aliyuncs`） | `enable_thinking` + `reasoning_effort` | 同左 | — |
| OpenDesign（`open-design.ai`） | 只发 `reasoning_effort`；Off=`none` | 只发 `reasoning.effort`；Off=`none` | — |
| GLM（模型含 `glm`，或 z.ai / bigmodel / zhipu） | `thinking {type: enabled, clear_thinking: false}` + `reasoning_effort` | 同左 | `thinking {type: enabled, clear_thinking: false}` + `output_config.effort`，只取 `low` / `high` / `max`，不发 `budget_tokens` |
| MiniMax | `thinking {type: adaptive}` + `reasoning_split: true`，没有档位 | `thinking {type: adaptive}`，不发 `reasoning_split` | `thinking {type: adaptive}`，不发 `reasoning_split` |
| Kimi / K3 | K3 不发 `thinking`，只发 `reasoning_effort`；其它 Kimi 发 `thinking` 开关，目录公布对应档位时再带 effort | 同左 | `thinking {type: adaptive, display: summarized}` + `output_config.effort` |
| Qwen | `enable_thinking` + `reasoning_effort` | 同左 | 无专用行，走 Claude budget |
| DeepSeek | `deepseek-v4*` / `deepseek-flash*`：`thinking` + `reasoning_effort`；旧 id 只有开关 | 同左 | 无专用行，走 Claude budget |
| Claude | 无专用行，走主机/通用行 | 无专用行 | 4.6 Opus/Sonnet 和 4.7+ 用 `adaptive` + `output_config.effort`（4.7+ 带 `display: summarized`）；更早用 `budget_tokens` |
| 其它 | `reasoning_effort` | `reasoning.effort` | `budget_tokens` |

档位映射：

- K3 只取 `low` / `high` / `max`：minimal/low→low，medium/high→high，xhigh/max→max。目录里该模型公布了档位列表时只发列表内的 token；目录完全不含该 id 时仍按 id 映射发送。
- Claude adaptive：minimal/low→low，medium→medium，On/High→high，xhigh→xhigh，max→max。Claude budget：minimal/low 1024，On/medium 4096，high 16384，xhigh/max 32768；如果 `max_tokens` 不大于 budget，会改成 budget + 4096。
- DeepSeek effort：minimal/low→low，max→max，其余（含 On/medium/high/xhigh）→high。GLM 的 Messages 路由：minimal/low→low，medium/high→high，On/xhigh/max→max。

K3 识别只看路径段：`k3`、`k3-*`、`kimi-k3`、`kimi-k3-*`。`k30`、`k3x`、`mk3`、`task-k3` 不算。

`Off` 的行为：

- OpenRouter 发 `reasoning.effort: none`；OpenDesign 和通用行发 `reasoning_effort: none`。
- Z.AI、DeepSeek 发 `thinking {type: disabled}`；其它 Kimi（除 K3 和 `kimi-k2.7-code`）也发 `disabled`；DashScope/Qwen 发 `enable_thinking: false`；MiniMax 仍发 `adaptive`；K3 和 `kimi-k2.7-code` 直接省略字段。
- Claude 5、`fable`、`mythos` 省略 `thinking`，只给 `output_config.effort: low`；更早的 Claude 发 `thinking {type: disabled}`。

`keeps_thinking_when_off` 对 Claude 5+/`fable`/`mythos`、MiniMax、K3、`kimi-k2.7-code` 返回 true，用于给输出上限留思考空间。

推理回放：MiniMax 回放 `reasoning_details`（原本是 `reasoning_content` 时回放该字段）；GLM、DeepSeek、Kimi 回放 `reasoning_content`；其它省略。Messages 回放带签名的思考块和 `redacted_thinking`。

## 输出上限

应用层顺序：设置 `maxOutput`（>0）→ 目录 `limit.output`（>0）→ 32000。Anthropic Messages 必须带 `max_tokens`，没有传入时也用 32000；已公布的 `limit.output` 原样发送，不取厂商常量最小值。Chat Completions 用 `max_tokens`，Responses 用 `max_output_tokens`，只在有值时发送。

压缩摘要用自己的上限：默认 8192；模型 `keeps_thinking_when_off` 时为 32768；遇到 `Length` 重试一次，上限 65536；摘要请求超时 180s。

## 提示缓存

- Anthropic Messages：每个请求最多 4 个 `cache_control: {type: ephemeral}` 断点，依次是最后一个工具、最后一段 system text、最后最多两条消息。MiniMax 改为钉住首尾消息，并跳过带思考的消息。
- Chat Completions：只有 OpenRouter 的 Anthropic/Gemini 模型、DashScope 的 Qwen/QwQ、`api.z.ai` / `bigmodel.cn` / `zhipu` 写断点。位置是最后一个工具、第一条 system/developer，再之后的首尾消息；带 `reasoning_content` / `reasoning_details` / thinking 块的消息不改写。
- `prompt_cache_key`：Chat Completions 和 Responses 发会话 id，截到 64 个 Unicode 字符。排除名单命中的主机不发，包括 `openrouter.ai`、`api.z.ai`、`bigmodel.cn`、`zhipu`、`deepseek.com`、`minimax`、`dashscope`、`aliyuncs.com`、`moonshot.ai`、`moonshot.cn`、`kimi.com`、`kimi.ai`、`api.groq.com`、`api.together.ai`、`api.together.xyz`、`volces.com`、`volcengine.com`、`qianfan`、`baidubce.com`、`generativelanguage.googleapis.com`、`api.anthropic.com`、`freemodel.dev`、`subconscious.dev`、`thinkingmachines.dev`。
- 其它主机先带一次；400 且错误文本提到 `prompt_cache_key` 时删字段重试，并把主机记入进程内拒绝集合。OpenRouter 另发 `x-session-id` 粘性路由头，值不截断。

## 用量

一套解析器接受多家字段名。输入认 `input_tokens`、`prompt_tokens`、`input`、`promptTokenCount`、`prompt_token_count`；输出认 `output_tokens`、`completion_tokens`、`output`、`candidatesTokenCount`、`candidates_token_count`。缓存读取认 `cache_read_tokens`、`cache_read_input_tokens`、`cached_tokens`、`prompt_cache_hit_tokens`、`cachedContentTokenCount`、`cached_content_token_count`，以及 `prompt_tokens_details` / `input_tokens_details` 里的嵌套字段；缓存写入认 `cache_creation_input_tokens`、`cache_write_input_tokens`、`cache_write_tokens`，以及 `cache_creation` 下的 `ephemeral_5m_input_tokens` / `ephemeral_1h_input_tokens`。

出现 Anthropic 缓存字段时，`prompt_tokens = input_tokens + cache_read + cache_write`；OpenAI 的 `prompt_tokens` 和 Responses 的 `input_tokens` 已包含缓存，不重复相加。多次 partial usage 合并时保留非零值。DeepSeek 的 `prompt_cache_miss_tokens` 故意不读，因为 `prompt_tokens` 已含命中与未命中。

## 工具 schema

发出前对每个工具参数调用 `mycode_core::inline_schema_refs`：展开本地 `$ref`，丢掉 `$defs` / `definitions`。引用已在当前栈上或深度达到 32 层时换成 `{type: object}`，并保留 `description`；无法解析的 `$ref` 被移除。三家协议都用展开后的 schema，因为 Moonshot 的 coding-plan 校验器会对 schemars 的 `$ref` + `$defs` 报无限递归。

## 目录

models.dev 的 `https://models.dev/api.json` 由 `catalog` 下载并写入家目录的 `catalog-cache.json`。刷新要发 HTTP，所以这部分在 providers，`mycode-config` 不依赖网络。

离线基线只有两行硬编码：`openai_codex_preset()` 和 `catalog/opendesign.rs` 的 `CURATED`。models.dev 不含这两个服务商。每次加载云端文档、物化缓存和生成基线时都会调用 `codex::attach` + `opendesign::attach`，所以它们不受云端数据影响。

缓存 24 小时内不发请求（`DEFAULT_MAX_AGE_SECS`）。之后带 `If-None-Match` 条件请求；304 只更新 `fetchedAt` 和 ETag。下载失败保留旧缓存；没有缓存就用基线。云端文档和缓存上限都是 32 MiB，缓存格式版本 3。启动时只校验缓存信封，模型列表在第一次 `document()` 时才解析。

旧的非零格式版本迁移为基线；信封损坏时把原字节备份为 `catalog-cache.json.broken-<nanos>`，再写入基线；I/O 读失败不动原文件，内存里用基线。信封合法但文档不可用时，第一次解析静默回退到基线。

解析规则：带任意 `status` 的模型跳过；`npm` 含 `bedrock` / `vertex` / `azure` / `google` 的服务商跳过；每个服务商最多 512 个模型；字符串字段最多 8 KiB；思考档位只取 `reasoning_options` 的 toggle 和 effort，effort 为空时取 `variants` 的键，最多 16 个，未知但合法的标识符保留，`default` 和非标识符丢弃。设备码服务商始终使用固定端点，不使用云端文档里的控制台 URL。

`model_for_endpoint` 的查找顺序：先按 provider id，再按精确 base URL；两者都没有命中任何服务商时，才按模型 id 搜整个目录（优先带思考档位的行，其次有输出上限的行）。命中了服务商但该服务商没有这个模型时返回 `None`，不会借用别家的行。

## 登录

`github-copilot` 和 `openai-codex` 用设备码登录；其它服务商（包括 `xai`）贴 API key。旧缓存里 `xai` 的 `oauth` 标记按普通 API key 处理。

- Copilot：GitHub 设备码，scope 是 `read:user`；轮询成功后保存 GitHub token，每次请求前用它换短期 bearer（缓存到过期前 60s），并附加 `COPILOT_CHAT_HEADERS`。
- Codex：OpenAI 设备码，轮询拿到 authorization code + PKCE verifier 后换 token。`OAuthSecret` 存 `access`、`refresh`、`expires`、`account_id`；过期时间取 `now + expires_in - 300`，距离过期 60s 内即刷新。请求额外带 `originator: mycode`、`openai-beta: responses=experimental`，以及从 JWT 的 `https://api.openai.com/auth/chatgpt_account_id` 取出的 `chatgpt-account-id`。

OAuth 调用走钉住客户端（`CheckRedirect`），不跟随重定向，调用方通过 `PinnedCall` 传入 User-Agent 和总超时（应用默认 30s）。错误信息只带状态码和字段名，不带 token。

## 常量

| 项 | 值 |
| --- | --- |
| 连接超时 / 流空闲超时 | 10s / 180s |
| SSE 单行 / 单流累计 | 1 MiB / 8 MiB |
| 内容块或工具调用索引 | 64 |
| 重试 | 同一 body 最多 4 次；改 body 重发最多再加 1 次 |
| 重定向跳数 | 5 |
| 目录文档 / 缓存 / 有效期 | 32 MiB / 32 MiB / 24h |
| 每个服务商模型数 / 每字符串字段 | 512 / 8 KiB |
| OpenDesign 提示预留 | 2048 token |
