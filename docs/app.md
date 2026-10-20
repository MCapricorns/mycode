# mycode-app

没有界面依赖的应用核。前端启动 `CoreBridge`，发送 `BridgeCommand`，等待 `BridgeReply`，渲染 `BridgeEvent`。会话、回合、工具宿主、MCP、网页、OAuth、设置、导出和自更新都在这里。

## 桥与线程

- `CoreBridge` 独占名为 `mycode-core` 的线程，里面是 current-thread tokio 运行时和 `SessionService`。前端不碰 tokio。
- 命令通道无界，`send` 同步。工作循环 `await` 命令，不能堵住运行时线程。
- 回复走 oneshot，接收端在哪个执行器上 poll 都行。丢掉回复 future 只是不再等，持久效果已经发生。
- 事件通道是 tokio 无界 mpsc，`send` 同步返回并唤醒接收端，慢界面不会卡住核心。前端每帧只取一小段增量（80 字、16 个增量），结束事件留到下一帧。
- `ChatTurn`、`CompactSession`、标题、OAuth、目录刷新、更新、探测各自 spawn。`CancelChat`、`Steer`、`CancelSubagent`、`UiReady` 在命令循环上同步处理。其余存储/配置命令进 `JoinSet`，关闭时先排空，再关 `SessionService`。
- 文件锁、原子写、目录遍历都在 `spawn_blocking` 里，阻塞池上限 32。

启动顺序：回收上次崩溃留下的子代理 worktree（`agent-worktrees/`，以及旧版 `task-worktrees/`）；在阻塞池读取并校验目录缓存信封；建 `CoreState`（同时从 `ui.json` 恢复会话到文件夹的绑定）。目录刷新和更新检查等首帧 `UiReady` 后才开始；`ui.json` 的 `auto_update` 关闭时不做更新检查。

## 模块

| 模块 | 干什么 |
| --- | --- |
| `protocol` | 纯数据协议：会话行、转录条目、活动对话、旧页、流式回复、`ask_user` 问题。身份类型从 agent 再导出 |
| `dispatch` | 命令循环、并发分发、会话删除的取消顺序 |
| `state` | 家目录、`SessionService`、内存目录、会话到文件夹映射、Copilot bearer 缓存、回合/子代理取消令牌、steer 收件箱、MCP 连接池 |
| `ledger` | 读历史、CAS 按头追加、打开尾部 40 条、每次再翻 40 条、召回、删除 |
| `turn` | 一次回合和 `/compact`：凭据、认证、历史、工具、提示词、压缩钩子、`Agent`、事件泵 |
| `projection` | 账本事件 → 转录行；同时恢复最近一次请求的上下文大小和缓存读 |
| `compaction` | 自动压缩、检查点读写、摘要请求与重试 |
| `tool_hosts` | `ask_user` 路由；网页后端密钥先看环境变量，再看密钥库 |
| `subagent` | 角色目录、工具白名单、每回合 8 个并发名额、worktree、按角色路由 |
| `mcp_client` / `mcp_tools` | stdio / Streamable HTTP，协议版本 `2025-06-18` |
| `web_client` | Querit、AnySearch、Exa；搜索最多 32 条、一次抓 8 个 URL、响应 8 MiB、单页 1M 字符、30 秒超时。Bearer 由传输层加 |
| `oauth` | Copilot、Codex 设备码登录，以及每次请求前的令牌处理 |
| `user_agent` | 固定 UA：`MYCode-Agent/<版本> (<平台> <系统版本>; <架构>; <shell>)`，不是设置项 |
| `settings_io` | 设置 CAS、单把密钥写入、代理设置生效 |
| `search` | 作曲栏 `@` 文件搜索：不区分大小写子串，跳过 `.git`、`node_modules`、`target`、`dist`、`build` 等目录，最多访问 8192 个目录项，返回最短的 8 条 |
| `export` | 导出/导入设置、界面状态和明文 API 密钥，不带会话；包版本 2，上限 48 MiB，可导入版本 1/2 |
| `updates` | 检查、下载、SHA-256 校验、暂存和重启安装；资产上限 512 MiB，待安装包最多尝试 3 次启动 |
| `install` | 自安装：终端版复制进 `<home>/bin` 并写 PATH；桌面版仅 Windows 自安装 |
| `title` | 侧栏标题：一次无工具、无 system prompt 的后台请求，60 秒超时，标题最多 72 字符 |

## 回合装配

`chat_turn` 的顺序：

1. 工作目录用会话绑定的文件夹，不存在就建；没有绑定用 `<home>/scratch/`。
2. 读设置和密钥。提供商必须启用且有密钥；和保存命令并发时最多读 3 次，间隔 150ms。
3. `resolve_request_auth` 取认证头和额外头，再 `ResolvedProvider::resolve`。
4. 重新读该分支的持久头，从账本回放整段分支。界面专用的 `COMPACTION SUMMARY` 行跳过；并行工具结果按调用顺序重排。
5. 取最后一条有文字的用户消息作为本次 prompt；它后面的助手消息或工具结果只在本次请求里省略，账本不动。
6. 连接 MCP，发现角色与工作区额外文件夹。
7. 建工具表：内置工具、`ask_user`；网页后端可用（`web_backend_ready`：有可用后端，需要 key 的已有 key）时才加 `web_search`、`fetch_content`；至少一个角色启用时加 `agent`；连上的 MCP 按直连/延迟规则注册。
8. 拼 system prompt：固定身份和 `<working_style>` + `build_system_prompt` 的工具说明 + 子代理委派表；然后是 `<environment>` / `<shell>`、资源文件（`AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md`）、skill 目录、MCP 说明、其它工作区文件夹和临时目录。skill 和文件夹排序，保证同样的文件得到相同字节。
9. 跑一次回合前压缩；写出检查点就立刻写摘要卡片并发 `SummaryShown`。
10. `AgentConfig` 带 system prompt、`prompt_cache_key` = 会话 id、输出上限（设置 `maxOutput` → 目录 `limit.output` → 32000）、会话自己的思考档位（`ChatTurn.reasoning`）。装压缩钩子，`Agent::prompt`。不保存文件快照。

## 事件泵与取消

事件泵拥有分支头：

- `ToolStarted` 先提交 `ToolCall` 事件，`ToolCompleted` 提交结果。
- 模型还在流式写 `agent` 调用时（`ToolCallStarted` 之后的 `ToolCallDelta`），泵从不完整的 JSON 里读出 `agent` 和 `description`，发 `ToolPreparing`；同一个服务商调用 id 的 `ToolStarted` 到达时替换它。前端把占位卡片放在 `preparing_calls`（不进转录条目，行 id 以 `prep-` 开头），画在流式回复下面；调用开始后，正式行接在已提交的助手步骤之后，与重新打开时的账本顺序一致；回合结束时清掉没启动的占位。
- 带工具调用的中间助手消息到达即提交并发 `AssistantStep`；纯文本回复先攒着。
- 每条带 usage 的助手消息累加并立即发一次 `UsageSnapshot`。用量不写账本。
- 回合中的 steer 先提交攒着的助手消息，再写用户消息并发 `SteerCommitted`，随后发 `PromptCacheWatch(Steer)`。
- `TurnEnded` 时提交最后一条助手消息（没有就用最后一个步骤），再发布钩子里攒下的摘要卡片，最后发 `ChatDone`。
- 取消时先提交已收到的半截助手消息，发布摘要卡片，再发 `ChatFailed`，消息是 `CHAT_CANCELLED`，不发 `ChatDone`。下一条请求回放这段历史，prompt cache key 仍是会话 id。

每个会话同时只有一个回合：新 `ChatTurn` 先取消旧回合。子代理令牌是回合令牌的子令牌，会跟着停；但子代理登记表不清，避免误停用户正在 steer 的子代理。删除会话才按 `session_id:call_id` 前缀清掉该会话的全部子代理。

## 用量与缓存

`UsageSnapshot` 字段：

- `input`、`output`、`cache` 是整回合累加，计费看这三个。`input` 只累加新鲜输入，即 `prompt_tokens` 减去缓存读：OpenAI 系 `input_tokens` 已含缓存读，直接累加会把重发的上下文重复计成输入。
- `context` 是最近一次请求的提示大小：`prompt_tokens`，缺失时用 `input_tokens`。
- `context_cache` 是这次请求的缓存读。上下文条和输入框的环用 `context`，不和本轮命中率混用。

自动压缩真正发出摘要请求时，核心先发 `ContextCompacting { active: true }`，摘要结束（成功、失败或取消）后发 `active: false`。回合前压缩直接发；回合中的经 agent 总线由事件泵转发，保证落在上一次回复之后、下一次请求的增量之前。手动 `/compact` 不发它。

回合前或回合中改写了前缀（压缩），以及本回合注入 steer 时，核心在下一次请求前发 `PromptCacheWatch`。前端拿当时的命中率做基线：基线至少 50%、下一次跌到一半以下且至少少 30 个点，留下警告；回到 50% 以上或换会话，警告消失。

## 子代理

角色来自内置、`<home>/agents/` 和项目 `.mycode/agents/`，同名时项目覆盖用户覆盖内置。父提示只在角色启用时出现，而且只介绍启用的角色：关掉 scout 或 artisan 后，`<dispatch>` 不再提它；`agent` 工具的描述本身不写角色名，只指向 `<dispatch>`。没有可独立、边界清楚且值得委派的工作就不要派。

- 每个父回合最多 8 个子代理，多出的排队；artisan 排队时发一条 `capped` 进度，scout 只排队。
- 没有墙钟超时。父回合取消、`CancelSubagent`、删除会话都会结束它，排队中的也一样。
- 工具取角色白名单和父级工具的交集。角色没写时继承 `read`、`write`、`edit`、`shell`、`grep`、`find`、`web_search`、`fetch_content`。`shell`/`bash`/`powershell`/`cmd` 都映射到当前进程实际的那个解释器。
- scout 强制只读：不注册写工具和 shell，MCP 只拿 `readOnlyHint` 为 true 的工具。
- 子代理的 system prompt 带上会话工作区的 `AGENTS.md` / `MYCODE.md` 资源（按会话 cwd 发现，worktree 里看不到未提交的改动），并把它们和匹配的 `SKILL.md` 当作用户的常驻指令；其它文件、工具输出和网页仍是数据。
- 连上的 MCP 会注册，`<mcp>` 说明与父回合共用同一份目录文本，只读角色另注明只拿 `readOnlyHint` 工具；子代理不能 `ask_user`，不能再调 `agent`；父回合没注册网页工具时子代理也拿不到。子代理有自己的压缩钩子（见「历史压缩」）。
- provider/model 只有设置里两项都填了才覆盖，否则用父回合的 provider；思考档位取设置覆盖或角色定义，默认交给提供商，不继承父回合的档位。换提供商走 `resolve_request_auth`，共用 Copilot bearer 缓存。
- 只有能写的角色能用 worktree：在 `<home>/agent-worktrees/` 建分离 checkout，结束时把 diff（最多 256 KiB）附在答案或错误后面，再删掉 worktree。

## MCP

每个回合连接设置里启用的服务器，再合并项目 `.mycode/mcp.json`。所有握手并发，单台 20 秒；JSON-RPC 每跳 60 秒，单条消息 1 MiB，每台最多 128 个工具。连不上就跳过，不让回合失败。

- 项目服务器按追加处理：id 和用户配置重复时跳过并给出警告，且永远拿不到用户密钥库里的 key。它不会覆盖全局同 id 服务器。
- 工具不超过 4 个、每个 schema 不超过 2 KiB 的服务器，把工具直接注册成普通工具（与已注册工具重名的改走 `search_tool`）；其余经 `search_tool` 按精确名字、关键词或 `"list"` 查找并取 schema，再用 `use_tool` 调用。跨服务器的同名工具，后出现的叫 `server__tool`。参数按服务器 `inputSchema` 校验；schema 编译不了时放行，由服务器自己拒绝。
- 结果超过 20 KiB 时只保留首尾，全文写入 `<home>/mcp-results/`；存新结果时顺带清掉 7 天前的文件。
- 设置和密钥的指纹没变、连接没坏时，下回合复用池里的服务器。连接中途断开或指纹变化，整池重连；某台启动失败的服务器等 120 秒后再单独重试，冷却期内只用已连上的。工具顺序按设置里的服务器顺序，保证相同输入的列表字节一致。

## OAuth

`StartOAuthSignIn` 只支持 Copilot 和 Codex。Windows 和 macOS 打开浏览器，其它平台只把验证链接给面板。Codex 设备码流程 15 分钟，连续 3 次轮询失败才放弃；Copilot 收到 `slow_down` 时轮询间隔加 5 秒。

成功后令牌写进密钥库，并把该提供商写入设置（启用、补上或更新模型）。调用方没给模型时，取目录里前 6 个支持工具调用的模型。

每次请求前 `resolve_request_auth`：

- Copilot 用存下的 GitHub 令牌换短期 bearer，缓存到过期前 60 秒。
- Codex 访问令牌过期时用 refresh token 换新并写回；全进程串行刷新，等锁的回合直接采用别人刚写回的新令牌。请求另带 `originator`、`openai-beta`、`chatgpt-account-id` 等头。
- 其它提供商的 OAuth blob 过期就直接报错，提示改贴 API key。

## 目录

启动只校验缓存信封：`formatVersion`、`kind`、etag 语法和整体 JSON 结构。首次 `document()` 才完整解析、归一化并注入内置的 Codex / OpenDesign 行；结果由 `OnceLock` 缓存，解析失败回退到内置保底行。

`GetCatalog` 在阻塞池里完成首次物化。首帧后的后台刷新：缓存不到 24 小时直接 `Fresh`，不联网；否则发条件请求。`RefreshCatalog` 和更新包下载成功后的预刷新强制跳过时间窗。`Fresh`、`NotModified`、`Updated` 都替换内存目录并发 `CatalogUpdated`；失败保留旧目录。

## 历史压缩

可用窗口是模型上下文的 95%，触发线是可用窗口的 90%。窗口取设置 `contextLimit`，再取目录；都没有时触发线是 48000 token。

请求大小按 provider 的计数算：本回合上一次请求的回复带回的 prompt token 数（含缓存读），加上之后新增消息（回复本身、工具结果、steer）的估算。回合第一次请求还没有回复，整段估算。估算是 ASCII 每 4 字符 1 token、其他字符（中日韩等）每字 1 token、每张图 1600，再乘上一次实测的比例（provider 计数 ÷ 估算，限在 0.75–3 倍，按会话记住）。provider 报的数远低于估算时，按比例下限算。大小包含 system prompt 和工具 schema。

尾部和摘要长度随窗口变：触发线减去 system prompt 和工具后的空间，先给摘要（空间的 1/6 个词，400–2500 词），剩下的一半留给尾部（2000–20000 token），另一半留给对话继续增长。尾部不拆开 tool call 和 tool result。自动压缩换下的头部不超过“摘要 + 触发线的 10%（至少 2000）”时不压：小窗口里那样压完，下一次请求又会过线。每次请求的输出上限压到窗口剩下的空间（留窗口 2%、至少 1000 的余量，最低 4096），免得输入加输出上限超过窗口被拒。

摘要请求：

- 头部转录放在 `<conversation>` 里，后面再提醒只输出摘要；思考设为关闭。
- 摘要要求覆盖用户目标与中途的纠正、决定、文件、命令及哪些检查通过/失败/未跑、子代理结论、未完成的工作和下一步；用用户的语言写，代码、路径、命令和报错原样保留，不抄入密钥。
- 每条消息截到 4000 字符；整段不超过可用窗口减去输出上限和提示，最多 80000 token，超出时先丢最旧的消息；思考块和开头的 `<think>` 不进转录。
- 输出上限 8192；`keeps_thinking_when_off` 为真的模型用 32768；都不超过可用窗口的 1/4（保留思考的模型 1/2），最低 2048。截断时用 65536 重试一次，不超过窗口剩下的空间；摘要短于 200 字符时重试一次；再失败就放弃。整次请求 180 秒超时。提示按上面算出的词数要求摘要长度。
- 模型的摘要最多 24000 字符，超出时去掉中间、保留开头和结尾（下一步通常写在最后）。
- 被摘要的头部里用户自己发的消息原样接在摘要后面（`<user_messages>` 块）：从新到旧取，最多尾部预算的 1/4、22000 字符，最新一条放不下时保留首尾，最后按从旧到新排列。上一次检查点里保留的用户消息也参与挑选。长工具回合里尾部全是工具输出，只靠摘要会丢掉用户的原话。界面卡片只显示模型的摘要。
- 摘要加用户消息写入该会话的 `compaction.json`。发给模型的消息和界面卡片都以 `COMPACTION SUMMARY` 开头。
- 同一分支的旧检查点作为下一次摘要的前文，已覆盖的消息不再重发。

行为：

- 摘要失败、太短或写检查点失败时，这次请求仍用完整历史，回合继续。账本不改写。
- provider 以超出上下文拒绝请求、且还没有任何输出时（400/413/422 或流里的第一条错误，消息含 `context_length_exceeded`、`maximum context length`、`prompt is too long`、`上下文长度` 等），不看阈值和收益立即压缩：估算比例至少调到让被拒请求等于可用窗口，尾部减半，然后重发一次；仍被拒就照常报错。主回合和子代理都这样。
- 检查点的 `coveredMessages` 落不到这段账本上（为 0、超过消息数、或切在工具结果上）时整份忽略：既不用它的摘要，也不用它的下标。
- 检查点仍匹配且“摘要 + 未覆盖尾部”没超阈值时，直接用拼接结果，不再请求模型。
- 拼接结果超阈值但原始账本还能切分时，重新摘要并写新检查点；只有拼接结果超阈值时，在内存里再压一次，不把更短数组的下标写回检查点。
- 历史已经以摘要开头、工具输出又把窗口撑过阈值时，先在内存里再压一次（新摘要把旧摘要当前文）。回合结束、事件泵排空后，若内存历史仍以摘要开头，就核对它的未压缩尾部与账本末尾逐条一致（角色和文本），且切点不落在工具结果上，再按账本下标写成检查点；与旧检查点相同就不写。所以一个回合里压缩多次，下一回合也直接从最新摘要继续。
- 子代理用同一个 `compact_history`，只是不带检查点槽（`checkpoint: None`）：只在内存里压缩，不读写父会话的检查点。窗口按子代理实际的模型计算：设置给角色指定了已启用的服务商和模型就用它，否则用父回合的模型。压缩后发一条 `step` 进度 `compacted`，卡片显示「已压缩上下文」。
- 回合前压缩写出卡片时立刻发布；before-request 钩子里产生的卡片先攒着，等最后一条消息提交后再写，避免插在工具调用和结果中间。
- 手动 `/compact` 忽略阈值，在当前头上强制摘要。`compacted`、`covered`、`empty` 算成功；会话打不开、提供商失败、摘要卡片没写进去都是 `ok: false`。`CompactFinished` 带回新的头。

## 删除会话

顺序固定：

1. 取消该会话的回合和全部子代理。
2. 让 `SessionService` 忘掉内存账本。
3. 删索引行、会话目录、旧版本留下的 `checkpoints/<id>/`。
4. 清 `ui.json` 里该会话的文件夹、工作区和模型记录。
5. 去掉内存里的文件夹绑定（只在持久删除成功后）。

持久删除失败时不留下半个目录，也不会提前丢掉工具用的 cwd 绑定。

## 不放在这里的东西

像素、主题、窗口尺寸在 `mycode-desktop`。SSE 怎么变成 `StreamEvent` 在 `mycode-providers`。工具怎么读写文件在 `mycode-tools`。见 [tools.md](tools.md)。
