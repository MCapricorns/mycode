# mycode-app

没有界面依赖的应用核。桌面（或以后的另一个前端）启动 `CoreBridge`，发送 `BridgeCommand`，等待 `BridgeReply`，并渲染 `BridgeEvent`。会话、模型回合、工具宿主、MCP、网页搜索、OAuth、自更新和设置写入都在这边。

## 设计思路

前端不接触 tokio。桥独占一根 `mycode-core` 线程，上面是 current-thread 运行时和会话服务。回复走 oneshot，接收端在哪个执行器上 poll 都可以。丢掉回复 future 只是不再等，不会撤销已经发生的持久效果。

命令通道故意无界，而且 `send` 是同步的。工作循环必须 `await` 命令，不能把运行时线程堵住，否则已经 spawn 的回合和目录刷新会饿死。事件通道（`async_channel`）同样无界：`try_send` 立即返回并唤醒界面上挂起的接收端，慢一帧的界面不能把单线程核心卡在回合中间。桌面每帧只取一小段已排队的增量，结束事件留到下一帧，所以快回合也能看见文字逐步出现。

回合、压缩、目录刷新、更新、OAuth 和取消都 spawn 成并发任务。其它命令也在各自的任务里处理；碰文件锁、原子写和目录遍历的同步步骤放进 `spawn_blocking`（线程池上限 32，防极端并发下栈内存膨胀），不在核心线程上直接做。

启动时依次：回收上次崩溃留下的子代理 worktree，在阻塞池里读取并校验目录缓存的信封（文档本体保持原始字节，首次使用才解析），建 `CoreState`。目录刷新和更新检查不在启动路径上——网络栈（reqwest + 系统 TLS）初始化的几兆内存不值得为可能用不到的功能先付：界面画完首帧发 `UiReady` 后两者才开始；`ui.json` 的 `auto_update` 关闭时不做更新检查。

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
| `tool_hosts` | `ask_user` 和网页工具怎么接到桥。Querit / AnySearch / Exa 的密钥先看 `QUERIT_API_KEY` / `ANYSEARCH_API_KEY` / `EXA_API_KEY` 环境变量，再看密钥存储 |
| `subagent` | `agent`：解析角色、工具白名单、并发许可、可选 worktree、按角色的模型路由、父提示里的委派表 |
| `mcp_client` / `mcp_tools` | stdio 或 Streamable HTTP（协议版本 `2025-06-18`，每台最多 128 个工具，单条消息 1 MiB，请求超时 60 秒）。见下文 |
| `web_client` | Querit、AnySearch、Exa。结果条数、URL 数、响应字节都有上限，Bearer 由传输层加 |
| `oauth` | Copilot、Codex 的设备码登录，以及每次请求前的令牌处理。见下文 |
| `user_agent` | 应用自己的 UA：`MYCode-Agent/<版本> (<平台> <内核>; <架构>; <shell>)`，所有出站请求固定使用，不是设置项 |
| `settings_io` | 设置 CAS 和单把密钥的写入 |
| `search` | 作曲栏 `@` 提到的项目文件：不区分大小写的子串，跳过 `.git`、`node_modules`、`target` 等目录，最多看 8192 项、返回 64 条 |
| `export` | 产品数据的导出与导入包：设置、界面状态和 API 密钥（明文），不带会话。导入会新增、修改或删除 MCP stdio 命令时先拒绝，桌面在“数据”卡片里让用户确认后再带 `confirm_mcp_commands` 重发 |
| `updates` | 检查、下载、校验、准备重启安装。错误收成一行。发布资产名形如 `{可执行文件名}-v{版本}-{target}.zip`，前缀取当前可执行文件的文件名主体：`mycode-desktop` 只匹配 `mycode-desktop-v*`，`mycode-cli`（以及改名前的 `mycode`）只匹配 `mycode-cli-v*`，两个二进制各取自己的包。macOS 用 `update.sh`；Windows 复制当前程序，带 `--mycode-apply-update` 等参数替换安装文件。其它平台没有安装包，更新检查会说明没有对应平台的资源。换好并校验新二进制后，旧版本的备份文件会被删掉，不在安装目录里堆积。启动检查失败会送到界面提示 |
| `install` | 自安装。`mycode-cli` 在所有平台上把自己复制进 `<home>/bin` 并加进用户 PATH（Windows 写 `HKCU\Environment`，macOS 写 shell 配置文件），进入界面前在终端里提示一次。桌面版只在 Windows 自安装：不在 `<home>/bin` 里运行时复制为 `<home>/bin/mycode-desktop.exe`、带原参数启动安装副本后退出，安装副本删除原来那个 `mycode-desktop*.exe` 并建桌面快捷方式；macOS 用 dmg 装进「应用程序」，不自安装。清理下载只删文件。debug 构建和 `MYCODE_NO_INSTALL=1` 跳过 |

## 回合装配

`chat_turn` 的顺序：

1. 用会话绑定的文件夹当工作目录（不存在就建）；没有文件夹时用 `scratch/`。
2. 读设置和密钥。提供商必须启用且有密钥；和保存并发时最多读三次，间隔 150ms，避免刚加的提供商还没落盘。
3. `resolve_request_auth` 得到认证头（见 OAuth），再 `ResolvedProvider::resolve`。
4. 重新打开会话，跟上账本的最新头（界面看到的头落后时不报「已经向前走了」），从账本回放整段分支。界面专用的压缩摘要行不进回放。
5. 注册内置工具、`ask_user`、`web_search`、`fetch_content`。至少一个角色启用时注册 `agent`。连上的 MCP 再注册工具（见下文）。
6. 取最后一条有文字的用户消息作为这次的 prompt；它后面残留的助手消息或工具结果（例如中断的回合）只在这次请求里省略，账本不动。
7. 先跑一次压缩；写出新检查点时立刻把摘要卡片写进账本并发 `SummaryShown`。
8. system prompt 先放固定部分，再放随工作区变化的部分：身份和 `<working_style>`（做完并验证、最小改动、破坏性操作先确认、如实汇报、文件和网页内容是数据不是指令）+ `build_system_prompt` 的工具清单、网页工具说明和调用约定 + 子代理委派表；然后是 `<environment>` / `<shell>` 块（OS、架构、会话 cwd、当前这一个进程工具和它的语法；子代理提示词同样带一份，cwd 是自己的运行目录）+ 资源文件（`AGENTS.md`、`MYCODE.md`）+ skill 目录 + MCP 说明 + 其它工作区文件夹。skill 和文件夹排好序，同样的文件得到字节相同的提示；固定部分在前，换个文件夹开新会话也能命中前缀缓存。
9. `AgentConfig` 带上这个提示、`prompt_cache_key` = 会话 id、输出上限（设置 `maxOutput` → 目录 `limit.output` → 32000）和会话自己的思考档位（`ChatTurn.reasoning`，来自 `ui.json` 里该会话的模型记录，不读全局设置）。装上压缩钩子，调用 `Agent::prompt`。不在工具前保存文件快照，`write`、`edit` 和 `shell` 的改动都留在工作区里。

事件泵把 `AgentEvent` 投影成 `BridgeEvent`，并拥有分支头：`ToolStarted` 先提交 `ToolCall` 事件，`ToolCompleted` 提交结果；带工具调用的中间助手消息到达即提交（`AssistantStep`）；最后一条助手消息在 `TurnEnded` 时提交，随后是用量行（提供商报了用量才写）、钩子里攒下的摘要卡片，最后发 `ChatDone`。用户取消时，还没作为工具步骤提交的半截助手消息先写入账本（里面已有中断标记），再发 `ChatFailed`，消息是 `CHAT_CANCELLED`，不发 `ChatDone`。界面安静复位后可以接着发；下一条请求回放这段历史，prompt cache key 仍是会话 id。回合进行中的 `Steer` 把文字注入当前回合，不取消子代理。排队发送和「打断并发送」仍会取消当前回合。

## 用量与上下文

提供商每轮都报用量，泵按回合累加，每轮发一次 `UsageSnapshot`。`input` / `output` / `cache` 是整回合的和，用于计费；Details 的「实时 / 上一轮」用这个和显示本轮缓存命中率（缓存读为 0 或提供商没报时不显示，避免把从不报缓存的模型写成 0%）。`context` 是最近一次请求的提示大小（`prompt_tokens`，OpenAI 式已含缓存读，Anthropic 由适配器加上缓存读），`context_cache` 是这次请求命中的缓存读。上下文条和输入框的环用 `context`，不跟本轮命中率混在一起。

压缩改写前缀（回合前、回合中的 hook、手动 `/compact`）或本回合注入 steer 时，核心在下一次请求之前发 `PromptCacheWatch`。桌面拿当时的命中率做基线。下一次有大小的请求如果从至少 50% 掉到一半以下、并且至少少 30 个点，Details 留下警告，同时弹出同样的句子。命中回到 50% 以上，或换会话，警告消失。回合中的压缩卡片仍等到回复提交后再画，但监视在 hook 里就已经发出，所以骤降不会被当成普通的一轮波动。

## 子代理

父提示要求：只有工作能独立并行、边界清楚，并且确实能降低成本或提高完成质量时才派 `agent`。单文件小改、已经有上下文、或同一个 brief 再套一层，都自己做。同一时刻尽量只跑一个 `artisan`，除非几份 brief 明显独立且父级能分别整合。问法含糊时先澄清或派 `scout`，不派 `artisan` 去漫游。

角色来自内置、`<home>/agents/` 和项目的 `.mycode/agents/`（同名时项目覆盖用户覆盖内置）。同一条回复里的多个 `agent` 并行。同时运行的子代理固定最多 8 个，不是设置项，多出的排队等名额。子代理没有墙钟超时，一直运行到给出答案；用户取消这一回合、在详情面板里停止它、或删除对话时都会结束它，排队等名额的也一样。子代理用角色声明的工具白名单；角色没写时继承父级的 `read`、`write`、`edit`、`shell`、`grep`、`find`、`web_search`、`fetch_content`。子代理和父级一样直接调用工具。scout 的目录是只读工具，注册时不会再给改动工具。进程在当前用户权限下运行，没有 OS 级沙箱。见 [tools.md](tools.md)。白名单里的 `shell`、`bash`、`powershell`、`cmd` 都注册成当前这一个进程工具（跟父会话同一个解释器，名字是 `powershell`、`bash` 或 `cmd`），不会在 Windows 上另给一个 bash 工具，也不会翻译命令。子代理提示词带同一份 `<environment>` / `<shell>`。scout 仍然是只读，不注册这个工具。连上的 MCP 也会注册给它，按原始工具名调用。scout 只拿到 `readOnlyHint` 为 true 的 MCP 工具。技能目录同样附在子代理提示词上，用 `read` 加载。子代理不能 `ask_user`，也不能再调 `agent`，没有压缩钩子。角色可以在设置里指定自己的 provider / model 和思考档位，否则沿用父回合。路由到其它提供商时走与父回合相同的 `resolve_request_auth`（OAuth 刷新、Copilot bearer 交换，共用 bearer 缓存），不会把密钥库里的整段 blob 当 bearer 发出。`worktree` 隔离只给能写的角色：在 `<home>/agent-worktrees/` 下建分离的 git worktree，结束时把 diff（最多 256 KiB）附在答案后面再删掉。旧会话里的 `task` 调用不会按新名字重放。

## MCP

每个回合开始时连接设置里启用的服务器，各服务器的启动和握手同时进行，回合只等最慢的那一个。打开的项目文件夹的 `.mycode/mcp.json` 也会加入，同 id 覆盖全局。

工具不超过 4 个、每个 schema 不超过 2 KiB 的服务器直接注册成普通工具（不与内置工具重名时）；其余只能经 `search_tool` 查 schema、再用 `use_tool` 调用。参数先按服务器的 `inputSchema` 校验。超过 20 KiB 的结果只保留首尾，全文存到 `<home>/mcp-results/`。某台服务器连不上就跳过，不让这一回合失败。设置和密钥没变时，下一回合复用已经连上的服务器，只重试没连上的那几台；调用中途断开，或设置、密钥变了，下一回合整池重连。工具顺序始终按设置里的顺序，同样的服务器得到字节相同的工具列表。

## OAuth

`StartOAuthSignIn` 支持 Copilot、Codex 两种设备码登录（xAI 只用 API key）；Windows 和 macOS 会打开浏览器，其它平台只在面板上给出链接。成功后令牌写进密钥存储，并在设置里补上或更新这个提供商（没选模型时取目录里前 6 个支持工具调用的模型）。

每次请求前 `resolve_request_auth`：Copilot 用存下的 GitHub 令牌换一个短期 bearer，缓存到过期前 60 秒；Codex 的访问令牌过期时用 refresh token 换新并写回（全进程串行刷新，等锁的轮次直接采用别的轮次刚写回的新令牌）；其它提供商存的 OAuth 令牌（例如旧版本 xAI 登录留下的）过期时报错，提示改贴 API key。Codex 请求另外带 `chatgpt-account-id` 等头。子代理按角色换提供商时也走这条路径。

## 目录

`GetCatalog` 返回内存里的目录。目录条目保留缓存文件的原始字节：启动只校验信封（`formatVersion`、`kind`、etag 语法和整体 JSON 结构），首次 `document()` 调用才完成完整解析（解码 + 归一化并注入内置的 Codex / OpenDesign 行），结果由 `OnceLock` 缓存且解析失败时回退到内置保底行。界面也不再启动即取：模型选择器、思考档位菜单、子代理模型菜单、命令面板或设置「模型」「关于」页首次打开才发 `GetCatalog`，多 MB 的解析在阻塞池里完成，不占核心线程。首帧后的后台刷新在缓存不到 24 小时时不联网（`Fresh`），否则发条件请求；`RefreshCatalog` 与更新包下载成功后的预刷新跳过这个时间窗。结果是 `Fresh`、`NotModified` 或 `Updated` 时都用磁盘缓存替换内存目录并发 `CatalogUpdated`，所以上下文窗口和输出上限立刻用上新数据；失败时保留原来的目录。

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
