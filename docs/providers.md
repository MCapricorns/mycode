# mycode-providers

把一条已配置的端点变成 `mycode-core` 的 `Provider`。厂商差异是数据：`kind` 只有三种，新厂商加一行设置即可，不必再写一个 crate。

密钥不进这个 crate 的配置。`ResolvedProvider::resolve` 由调用方传入密钥和 User-Agent。设置里没有密钥字段。

## 设计思路

三种协议各有一个「组请求体」和一个「帧归约器」，共用同一个 SSE 驱动。驱动只负责把传输层的字节流变成 `StreamEvent`，取消令牌一响就停。传输是 `SseTransport` trait，生产用 `reqwest`，测试或替换实现可以注入别的传输。

所有请求都带配置的 User-Agent（未设置时用 pi agent 的默认值）。HTTP 客户端走操作系统 TLS 和系统代理，不启用会再编一套加密库的 rustls 特性。

出站请求先解析域名，再只连接检查过的地址。`PublicHttps`（网页检索、更新下载）要求 https、443 端口，并且解析结果全部是公网地址。GitHub 发布用的 `api.github.com`、`github.com`、`www.github.com`、`codeload.github.com`、`uploads.github.com` 和 `*.githubusercontent.com` 是例外：DNS 里夹了私网、ULA 或链路本地地址时，丢掉这些地址，只连接剩下的公网地址。整段都是 `198.18.0.0/15`（本机 fake-ip）时可以连接这些地址。和 RFC1918、回环或链路本地混在一起则拒绝。其他主机的混合解析、字面私网地址、回环和链路本地仍然拒绝。`CheckRedirect` 的第一跳可以整段是公网或整段是私网（本机模型），之后的跳转必须全是公网。

端点拼接避免把路径写两遍：base 已经以 `/v1/messages` 或 `/chat/completions` 结尾时原样使用。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `anthropic_messages` | Anthropic Messages：`x-api-key`、`anthropic-version`、消息体与事件归约 |
| `openai_completions` | Chat Completions：`Authorization: Bearer`，`/chat/completions` |
| `openai_responses` | Responses API：同样的 Bearer，路径 `/responses` |
| `wire_common` | 三种协议共用的消息和工具字段整理 |
| `xml_tool_calls` | 从文本里收回模型用 XML 写出的工具调用，补那些不走原生 tool 字段的端点 |
| `sse` | SSE 帧切分 |
| `driver` | 用归约器把传输流驱动成 `EventStream`。`stream()` 在运行时里 `spawn` 这段驱动 |
| `transport` | `SseTransport` 与 reqwest 实现 |
| `oauth` | Copilot、Codex、xAI 的设备码登录、轮询、刷新。令牌解析后仍交给调用方存进 `secrets.json` |
| `catalog` | 编进二进制的 models.dev 快照、家目录缓存、条件刷新，以及设置页用的预设 |

## 目录

`catalog` 放在这里是因为刷新是一次 HTTP。`mycode-config` 保持无网络。

离线基线是生成好的 `snapshot.json`。家目录的 `catalog-cache.json` 盖在上面。后台刷新用条件请求；失败时继续用已有缓存或内置快照。缓存文件本身解析失败时，原字节备份为 `catalog-cache.json.broken-*`，再写入内置快照，下次启动不会反复提示。读不出来则不动原文件，内存里用内置快照。设置页的预设、上下文窗口、思考档位、工具调用和价格都从这份目录来。界面因此能在不发版的情况下看到新模型。

单份目录最多 1024 个提供商、每个提供商 512 个模型，字符串字段有字节上限。

## 一次解析

```text
ProviderSettings + model id + api key + user agent
        │  kind 必须是三种之一，model 必须在该提供商的列表里
        ▼
ResolvedProvider { endpoint, headers, model }
        │  绑上 Arc<dyn SseTransport>
        ▼
WireProvider::stream → EventStream
```

未知 `kind` 或未提供的模型返回 `Rejected`，不发出请求。请求体序列化失败时用空体（调用前 `Request::validate` 已经挡住明显非法的请求）。

## 不放在这里的东西

选哪个提供商、密钥从哪读、压缩、把流画出来，都属于 `mycode-app` 和桌面。本 crate 收到的是已经决定好的端点和密钥。
