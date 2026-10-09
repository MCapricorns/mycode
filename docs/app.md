# mycode-app

没有界面依赖的应用核。桌面（或以后的另一个前端）启动 `CoreBridge`，发送 `BridgeCommand`，等待 `BridgeReply`，并渲染 `BridgeEvent`。会话、模型回合、工具宿主、MCP、网页搜索、OAuth、自更新和设置写入都在这边。

## 设计思路

前端不接触 tokio。桥独占一根 `mycode-core` 线程，上面是 current-thread 运行时和会话服务。回复走 oneshot，接收端在哪个执行器上 poll 都可以。丢掉回复 future 只是不再等，不会撤销已经发生的持久效果。

命令通道故意无界，而且 `send` 是同步的。工作循环必须 `await` 命令，不能把运行时线程堵住，否则已经 spawn 的回合和目录刷新会饿死。事件通道（`async_channel`）同样无界：`try_send` 立即返回并唤醒界面上挂起的接收端，慢一帧的界面不能把单线程核心卡在回合中间。桌面每帧只取一小段已排队的增量，结束事件留到下一帧，所以快回合也能看见文字逐步出现。

回合、压缩、目录刷新、更新、OAuth 和取消都 spawn 成并发任务。其它命令也在各自的任务里处理；碰文件锁、原子写和目录遍历的同步步骤放进 `spawn_blocking`，不在核心线程上直接做。

启动时依次：回收上次崩溃留下的子代理 worktree，在阻塞池里解析目录缓存，建 `CoreState`，然后后台刷新一次目录，并在 `ui.json` 的 `auto_update` 打开时检查一次更新。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `protocol` | 前端和核心约定的纯数据：会话行、转录条目、打开的对话、较早的一页、正在流的回复、`ask_user` 的问题。身份类型从 agent 再导出，前端不必依赖账本实现 |
| `dispatch` | 命令循环。每个 `BridgeCommand` 变成一个 spawn 出去的任务。`ChatTurn` 先取消同一会话正在跑的回合和它的子代理：每个会话同时只有一个回合 |
| `state` | 共享核心状态：家目录、会话服务、内存目录、会话到文件夹的映射、Copilot bearer 缓存、回合和子代理的取消令牌、MCP 连接池 |
| `ledger` | 读历史、按头追加、把存储错误收成界面能显示的短句。打开对话只取尾部 40 条，向上翻页每次再取 40 条；模型回放仍走整段分支 |
| `turn` | 一次 `ChatTurn` 和 `/compact`：读设置和密钥、解析 provider、注册工具、跑 `Agent`、把消息、工具结果和用量提交回账本 |
| `projection` | 账本事件 → 转录行。用量行拼成 `provider/model: in / out · ctx · hit · cache · tok/s`，桌面重放这些行恢复每个模型的累计 |
| `compaction` | 历史压缩。见下文 |
| `tool_hosts` | `ask_user` 和网页工具怎么接到桥。Querit / AnySearch 的密钥先看 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY` 环境变量，再看密钥存储 |
| `subagent` | `agent`：解析角色、工具白名单、并发许可、可选 worktree、按角色的模型路由、父提示里的委派表 |
| `mcp_client` / `mcp_tools` | stdio 或 Streamable HTTP（协议版本 `2025-06-18`，每台最多 128 个工具，单条消息 1 MiB，请求超时 60 秒）。见下文 |
| `web_client` | Querit、AnySearch、自定义后端。结果条数、URL 数、响应字节都有上限，Bearer 由传输层加 |
| `oauth` | Copilot、xAI、Codex 的设备码登录，以及每次请求前的令牌处理。见下文 |
| `settings_io` | 设置 CAS 和单把密钥的写入。读设置时补上探测到的 shell 和默认 User-Agent 并写回 |
| `search` | 作曲栏 `@` 提到的项目文件：不区分大小写的子串，跳过 `.git`、`node_modules`、`target` 等目录，最多看 8192 项、返回 64 条 |
| `export` | 产品数据的导出与导入包：设置、界面状态和会话日志，不带密钥。导入后为 JSONL 补建索引 |
| `updates` | 检查、下载、校验、准备重启安装。错误收成一行。macOS / Linux 用 `update.sh`；Windows 复制当前程序，带 `--mycode-apply-update` 等参数替换安装文件。Linux x86_64 安装包是 `mycode-desktop-*-x86_64-unknown-linux-gnu.zip`。已发布平台缺这个包时报错，不当成已是最新。启动检查失败会送到界面提示 |

## 回合装配

`chat_turn` 的顺序：

1. 用会话绑定的文件夹当工作目录（不存在就建）；没有文件夹时用 `scratch/`。
2. 读设置和密钥。提供商必须启用且有密钥；和保存并发时最多读三次，间隔 150ms，避免刚加的提供商还没落盘。
3. `resolve_request_auth` 得到认证头（见 OAuth），再 `ResolvedProvider::resolve`。
4. 重新打开会话，跟上账本的最新头（界面看到的头落后时不报「已经向前走了」），从账本回放整段分支。界面专用的压缩摘要行不进回放。
5. 注册内置工具、`ask_user`、`web_search`、`fetch_content`。至少一个角色启用时注册 `agent`。连上的 MCP 再注册工具（见下文）。
6. 取最后一条有文字的用户消息作为这次的 prompt；它后面残留的助手消息或工具结果（例如中断的回合）只在这次请求里省略，账本不动。
7. 先跑一次压缩；写出新检查点时立刻把摘要卡片写进账本并发 `SummaryShown`。
8. system prompt = 身份 + 资源文件（`AGENTS.md`、`MYCODE.md`）+ skill 目录 + MCP 说明 + 网页工具说明 + 其它工作区文件夹 + `build_system_prompt` 的工具清单和约定 + 子代理委派表。skill 和文件夹排好序，同样的文件得到字节相同的提示，方便提供商命中前缀缓存。
9. `AgentConfig` 带上这个提示、`prompt_cache_key` = 会话 id、输出上限（设置 `maxOutput` → 目录 `limit.output` → 32000）和会话自己的思考档位（`ChatTurn.reasoning`，来自 `ui.json` 里该会话的模型记录，不读全局设置）。装上压缩钩子，调用 `Agent::prompt`。不在工具前保存文件快照，`write`、`edit` 和 `shell` 的改动都留在工作区里。

事件泵把 `AgentEvent` 投影成 `BridgeEvent`，并拥有分支头：`ToolStarted` 先提交 `ToolCall` 事件，`ToolCompleted` 提交结果；带工具调用的中间助手消息到达即提交（`AssistantStep`）；最后一条助手消息在 `TurnEnded` 时提交，随后是用量行（`usage.enabled` 默认开，且提供商报了用量）、钩子里攒下的摘要卡片，最后发 `ChatDone`。取消时发 `ChatFailed`，消息是 `CHAT_CANCELLED`，界面安静复位。

## 用量与上下文

提供商每轮都报用量，泵按回合累加，每轮发一次 `UsageSnapshot`，结束时写入 `Usage` 事件并发 `UsageRecorded`。`input` / `output` / `cache` 是整回合的和，用于计费；`context` 是最近一次请求的提示大小（`prompt_tokens`，OpenAI 式已含缓存读，Anthropic 由适配器加上缓存读），`context_cache` 是这次请求命中的缓存读。桌面的上下文计量用 `context`，不是多轮工具的总和。

## 子代理

父提示要求：只有工作能独立并行、边界清楚，并且确实能降低成本或提高完成质量时才派 `agent`。单文件小改、已经有上下文、或同一个 brief 再套一层，都自己做。同一时刻尽量只跑一个 `artisan`，除非几份 brief 明显独立且父级能分别整合。问法含糊时先澄清或派 `scout`，不派 `artisan` 去漫游。

角色来自内置、`<home>/agents/` 和项目的 `.mycode/agents/`（同名时项目覆盖用户覆盖内置）。同一条回复里的多个 `agent` 并行。并发上限默认是 4，每个回合一个信号量；设置里的 `subagents.maxConcurrent` 为 `0` 时使用这个默认值，不是零个子代理。子代理没有墙钟超时；取消父回合或单独 `CancelSubagent` 会停掉它。子代理用角色声明的工具白名单；角色没写时继承父级的 `read`、`write`、`edit`、`shell`、`grep`、`find`、`web_search`、`fetch_content`。连上的 MCP 也会注册给它。子代理不能 `ask_user`，也不能再调 `agent`，没有压缩钩子。角色可以在设置里指定自己的 provider / model 和思考档位，否则沿用父回合。路由到其它提供商时走与父回合相同的 `resolve_request_auth`（OAuth 刷新、Copilot bearer 交换，共用 bearer 缓存），不会把密钥库里的整段 blob 当 bearer 发出。`worktree` 隔离只给能写的角色：在 `<home>/agent-worktrees/` 下建分离的 git worktree，结束时把 diff（最多 256 KiB）附在答案后面再删掉。旧会话里的 `task` 调用不会按新名字重放。

## MCP

每个回合开始时连接设置里启用的服务器。项目文件夹在 `ui.json` 的 `trusted_projects` 里（在侧栏的项目菜单里信任或撤销；与侧栏、信任列表共用同一套路径比较：去掉末尾斜杠，Windows 上再折叠斜杠方向与 ASCII 大小写）时，它的 `.mycode/mcp.json` 也会加入，同 id 覆盖全局；打开文件夹本身不会信任它。

工具不超过 4 个、每个 schema 不超过 2 KiB 的服务器直接注册成普通工具（不与内置工具重名时）；其余只能经 `search_tool` 查 schema、再用 `use_tool` 调用。参数先按服务器的 `inputSchema` 校验。超过 20 KiB 的结果只保留首尾，全文存到 `<home>/mcp-results/`。某台服务器连不上就跳过，不让这一回合失败。设置和密钥没变、每台启用的服务器都连着时，下一回合复用这些连接；有服务器没连上，或调用中途断开，下一回合整池重连。

## OAuth

`StartOAuthSignIn` 支持 Copilot、xAI、Codex 三种设备码登录；Windows 和 macOS 会打开浏览器，其它平台只在面板上给出链接。成功后令牌写进密钥存储，并在设置里补上或更新这个提供商（没选模型时取目录里前 6 个支持工具调用的模型）。

每次请求前 `resolve_request_auth`：Copilot 用存下的 GitHub 令牌换一个短期 bearer，缓存到过期前 60 秒；xAI 和 Codex 的访问令牌过期时用 refresh token 换新并写回；其它提供商存的 OAuth 令牌过期时报错，要求重新登录。Codex 请求另外带 `chatgpt-account-id` 等头。子代理按角色换提供商时也走这条路径。

## 目录

`GetCatalog` 返回内存里的目录。启动时的后台刷新在缓存不到 6 小时时不联网（`Fresh`），否则发条件请求；`RefreshCatalog` 跳过这个时间窗。结果是 `Fresh`、`NotModified` 或 `Updated` 时都用磁盘缓存替换内存目录并发 `CatalogUpdated`，所以上下文窗口和输出上限立刻用上新数据；失败时保留原来的目录。

## 历史压缩

可用窗口是模型上下文的 95%，触发线是可用窗口的 90%。窗口取设置里的 `context_limit`，再取目录；都没有时触发线回退到 4.8 万 token。尾部保留大约 2 万 token，并且不把一对 tool call / tool result 拆开。

头部交给同一个 provider 做交接摘要：转录包在 `<conversation>` 里，后面再提醒一遍「只输出摘要，不要继续任务」；思考关闭；每条消息截到 4000 字符，整段最多 30 万字符，思考块和开头的 `<think>` 不进转录。输出上限 8192 token（名字含 `kimi-k2.7-code`、不接受关思考的模型用 32768），被截断时用 65536 重试一次；摘要短于 200 字符时同样重试一次；再失败就放弃。整次请求 180 秒超时。成功的摘要（最多 2.4 万字符）写入 `compaction.json`；发给模型的那条消息和界面卡片都以 `COMPACTION SUMMARY` 开头。同一分支上已有的检查点作为下一次摘要的前文，已覆盖的消息不再重发。

摘要失败、太短或写检查点失败时，这次请求仍用完整历史，回合继续。账本里已有的事件不改写；成功时追加一条界面可见的 SUMMARY 卡片（`SummaryShown`），模型回放跳过这一行。请求前钩子里产生的卡片先攒着，等回合的最后一条消息和用量提交后再写，免得插在工具调用和结果中间。

`covered_messages` 是账本回放里的下标，回放本身没有摘要前缀。当前头已经有检查点、而这次历史还是完整回放时，直接拼回摘要和尾部，不再打一轮模型。历史已经以摘要开头、工具输出又把窗口撑过阈值时，只在内存里再压一次，不把这个更短数组的下标写回检查点。

手动 `/compact`（`CompactSession`）不管阈值，在当前头上强制摘要，结束发 `CompactFinished`：`compacted`、`covered`（这个头已经压过）、`empty`（没东西可压）算成功；会话打不开、提供商失败、摘要卡片没写进去都是 `ok: false` 并带原因。

## 删除会话

顺序是固定的：取消该会话的回合和子代理，让会话服务忘掉内存账本，删索引行、会话目录和旧版本留下的 `checkpoints/<id>/`，再清 `ui.json` 里该会话的文件夹、工作区和模型记录，最后去掉内存里的文件夹绑定。这样不会留下半个目录让列表永远报校验失败，也不会让 actor 停掉。

## 不放在这里的东西

像素、主题、窗口尺寸在 `mycode-desktop`。帧怎么从 SSE 变成 `StreamEvent` 在 `mycode-providers`。工具怎么读文件在 `mycode-tools`。
