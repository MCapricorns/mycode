# 架构

MYCode 是本地编码代理。对话、工具调用和会话数据都在本机；模型请求发往设置里的 API。发布目标只有 Windows x64、Windows ARM64 和 macOS Apple Silicon，没有 Linux 包。前端不直接读密钥；设置和会话通过 `CoreBridge`，桌面启动时直接读 `ui.json` 恢复窗口大小。会话、模型、工具和网络请求都在核心 crate 里。

## Crate 依赖

依赖只向下。`mycode-core` 和 `mycode-config` 是叶子，互不依赖。

```text
mycode-desktop   → frontend, app, config, providers, tools
mycode-cli       → frontend, app, config
mycode-frontend  → app, config, providers, tools
mycode-app       → agent, providers, tools, config, core
mycode-agent     → tools, config, core
mycode-providers → config, core
mycode-tools     → core
mycode-config    （叶子）
mycode-core      （叶子）
```

| Crate | 职责 | 当前边界 |
| --- | --- | --- |
| `mycode-core` | 消息、内容块、事件、工具规格、Provider 端口 | 叶子 crate。还声明 workspace 唯一的全局分配器（mimalloc）。没有协议、磁盘或策略 |
| `mycode-config` | 家目录、严格 JSON 文档、settings/secrets/ui、压缩检查点、角色与资源发现 | 叶子 crate。路径纯词法解析，不跟随符号链接 |
| `mycode-providers` | `anthropic-messages`、`openai-completions`、`openai-responses` 三种线协议，模型目录，Copilot/Codex 的 OAuth 设备流与刷新，代理测试 | 密钥由调用方传入；目录刷新也在这里，配置 crate 保持无网络依赖 |
| `mycode-tools` | `Tool` trait、`ToolRegistry`、内置文件/搜索/进程/web/ask/agent 工具 | 每个工具用 schemars 生成一份 JSON Schema，模型规格和运行时校验共用；同名注册后者覆盖前者。搜索和读文件在进程内完成，不启动外部 `rg`/`fd` |
| `mycode-agent` | `Agent` 回合循环、`TurnEnv`、`HookRunner`、system prompt、`SessionService` 会话账本 | 循环不依赖磁盘；账本是独立一层 |
| `mycode-app` | `CoreBridge`、命令分发、回合装配、MCP、网页、子代理、OAuth、设置读写、导入导出、自更新 | `CoreBridge` 用独立线程上的 current-thread tokio；命令和事件都是无界通道 |
| `mycode-frontend` | `FrontendDriver`、`WorkspaceState`、纯 reducer、`Effect`、i18n、git 状态、CLI 参数 | 产品行为集中在这里；reducer 是纯函数，返回的 `Effect` 由 GPUI 或 TUI 自己执行。前端只通过 `CoreBridge` 驱动后端 |
| `mycode-desktop` | GPUI 渲染、输入、窗口 chrome、目录选择、git 文件监听 | 不跑模型回合，也不执行模型工具 |
| `mycode-cli` | ratatui/crossterm 终端，共用同一个 `FrontendDriver` | 可执行文件名是 `mycode-cli` |

工作区成员就是这九个。网页和 MCP 客户端是 `mycode-app` 的模块，没有独立 crate。

`CoreBridge` 是前后端唯一接口。前端发 `BridgeCommand`，每个命令带一个 oneshot 回复；核心把会阻塞的存储和配置 I/O 放到 `spawn_blocking`，模型回合并发成任务。`BridgeEvent` 走单独的事件流，负责流式文本、思考、工具进度、用量和终止事件；`ChatDone` 和 `ChatFailed` 是每个回合唯一的终止信号。

`mycode-app` 的模块按职责分开：`dispatch` 跑命令循环，`turn` 装配回合，`ledger` 读写账本，`tool_hosts` 提供 ask/web/agent 宿主，`mcp_client` 和 `mcp_tools` 处理 MCP，`web_client` 走网页后端，`oauth` 管设备流，`settings_io` 管设置和密钥，`projection` 生成显示条目，`updates` 和 `install` 管自更新，`export` 管导入导出。

`mycode-agent` 的模块：`agent` 是循环，`turn` 管单个响应周期和工具派发，`env` 注入 provider/工具/取消/steer，`hooks` 是请求前钩子，`prompt` 拼默认 system prompt，`session` 包含 actor、store、runtime 和账本。

## 一条消息的生命周期

1. 输入提交经过 frontend 的 `submit` 和 `FrontendAction`/`reduce`，先改界面状态；driver 发 `BridgeCommand::SendMessage`。
2. `SendMessage` 在核心写账本：`HeadWriter` 预留事件，在期望分支头上 CAS；冲突时按新头重试。成功后回 `Sent { head, entry }`。
3. 前端收到 `Sent` 才发 `ChatTurn`。`dispatch` 先取消这个会话正在跑的回合 token，再启动 `chat_turn`。
4. `turn_credentials` 读 settings 和 secrets，短暂重试以避开并发保存；`resolve_request_auth` 处理 Copilot bearer 缓存和 Codex refresh。
5. `ResolvedProvider` 解析 endpoint、协议和 headers；`ReqwestTransport` 建立 SSE 传输。
6. 重新读分支头，按账本回放整条分支为 `Message` 列表。最后一条非空 user message 作为本回合 prompt；它后面的 assistant/tool 结果留在账本里，但这次请求不带。
7. 连接 MCP（复用池，只重连失败或新加的 server），组装注册表：`read`、`write`、`edit`、`grep`、`find`、当前平台的 shell、`ask_user`、网页后端可用时的 `web_search`/`fetch_content`、按需注册的 `agent`，以及 MCP 工具。
8. 拼 system prompt：固定身份、工具说明、`AGENTS.md` 资源、skills 目录、MCP 提示、额外工作区目录、环境块。
9. 回合前检查压缩：可用窗口是模型窗口的 95%，到其中 90% 触发；尾部随窗口变，最多约 20k token；到阈值时写 `compaction.json` 和一张摘要卡。
10. `AgentConfig` 带 system prompt、会话 id 作 `prompt_cache_key`、输出上限和推理档位；启动 `LedgerPump`，`seed_history` 后调用 `Agent::prompt`。
11. 每个响应周期先过 before-request 钩子（再次压缩；失败则用原历史），再发流式请求；provider 以超出上下文拒绝且还没有输出时，压缩后重发一次。有工具调用就派发：`agent` 调用立即并行，连续的只读/查询工具成批，其余工具按顺序单独跑；结果按模型给出的 call 顺序回填。工具失败变成 `is_error` 结果，回合继续。
12. Pump 在 `ToolStarted` 写 `ToolCall`，在 `ToolCompleted` 写 `ToolResult`；带工具调用的 assistant step 在 `MessageAdded` 时写。纯文本回复先压住，回合结束才写。用量只发 `UsageSnapshot` 实时事件，不写账本；旧账本里的 `EventKind::Usage` 仍可读。
13. 回合结束：最后一条 assistant 消息（或最后一个工具 step）提交，延迟的摘要卡发布，然后 `ChatDone { head, entry }`。
14. 取消用本回合的子 token；子代理 token 是它的子节点，一起停。已完成的工具调用/结果和部分 thinking/text 留在账本，部分回复标 `interrupted by user`；未完成的调用不执行。Pump 发 `ChatFailed`，不发 `ChatDone`。下一条请求继续用同一会话 id 和这段历史。
15. 回合进行中有三种发法，桌面和 CLI 都走 `FrontendDriver`：Enter 进入最多 8 条的队列，等当前回合自然结束；Ctrl+Enter（打断发送）先取消，再立即发；Ctrl+G（引导）发 `BridgeCommand::Steer`，回合不停，核心在下一个循环边界把文本作为用户消息追加进历史，账本写入后界面上的「引导中」消失。回合在读到它之前就结束（`Steered(Err)` 或 `SteersReturned`）时，文本退回该会话的队列；那个会话不在屏幕上时进入它的暂存队列。

一次 `Agent::prompt` 是一个 turn，内部可以有多个响应周期。`TurnStarted` 只在请求通过校验后发。provider 失败保留同样的部分消息并正常结束 turn；用户取消才是 `TurnOutcome::Aborted`。`StopReason::Length` 截断的工具调用不执行，模型需要重发。

## 家目录

`MYCODE_HOME` 非空时就是根；否则用 `~/.mycode`。Windows 上没有 `HOME` 时用 `USERPROFILE`。解析纯词法，不访问磁盘，也不按当前工作目录拼接；非法的高优先级值直接失败，不回退。根必须是绝对、规范化路径，不允许 `..`、文件系统根或不安全的盘符/UNC 形式。

```text
<home>/
├─ settings.json           提供商、MCP、网页、外观、代理、推理默认值、子代理、技能开关。不含密钥
├─ secrets.json            API 密钥、OAuth 令牌、MCP/网页凭证（`mcp-<id>`、`web-<id>`）
├─ ui.json                 工作区、最近项目、会话归属、每个会话的模型/思考档位、窗口大小。可丢弃
├─ catalog-cache.json      models.dev 目录缓存（最大 32 MiB；启动只校验信封，首次使用时才解析）
├─ AGENTS.md               可选的全局指令
├─ sessions.db             SQLite WAL 索引：标题、分支头、事件数、JSONL 偏移
├─ sessions/<项目键>/sess-YYYYMMDD-HHMMSS-<8 hex>/
│  ├─ <br1-…>.jsonl        分支事件；一行一个 JSON
│  ├─ payloads/<evt1-…>.bin  ≥64 KiB 的 payload
│  └─ compaction.json      每会话一份压缩检查点
├─ agents/<name>.md        用户自定义子代理角色
├─ agent-worktrees/        子代理 worktree 租约，启动时回收（旧版 `task-worktrees/` 也回收）
├─ mcp-results/            截断的 MCP 结果全文，7 天后清理
├─ scratch/                没有绑定文件夹时的会话 cwd
├─ bin/                    CLI 安装副本；该目录会补进 PATH
├─ update/                 自更新暂存的 pending 二进制、sha256 和版本
└─ checkpoints/<session>/  旧版文件快照；不再写，删除会话时清掉
```

`settings.json`、`secrets.json`、`ui.json` 是严格 JSON，`deny_unknown_fields`。解析或校验失败（包括尾逗号）会把原文件复制成 `*.broken-<nanos>`，保留还能解码的字段并发布默认值；没有“只修尾逗号”的就地改写。settings 和 secrets 带 revision；完整替换走 CAS。`ui.json` 是 advisory，最后写入者赢。会话账本不走这套修复。

`settings.json`、`secrets.json`、`ui.json` 的上限分别是 256 KiB、64 KiB、256 KiB。密钥在 `secrets.json`，不在设置里。

提示词资源按固定位置发现：工作区的 `AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md`，MYCODE_HOME 的 `AGENTS.md`，以及 OS 用户主目录的 `.agents/AGENTS.md`。skills 从工作区 `.agents`（含 `skills/`）和 OS 用户主目录的 `.agents` 发现；每个提示词最多列 32 个。提示词只放 skill 的路径和描述，正文由模型用 `read` 加载。

首条用户消息写入时，索引标题取折叠空白后的前 60 个字符。第一次发送还会异步发 `SummarizeTitle`；成功时把索引标题换成模型生成的短标题（最多 72 字符），失败则保留前 60 个字符。侧栏没有标题时显示项目目录名，再没有就显示 New task。发送队列只在内存里，不写 `ui.json`。会话 ID 形如 `sess-20261019-153012-0123abcd`，是本地创建时间加 8 位随机十六进制，也是会话目录名。目录放在创建时所在项目的文件夹下：项目路径里字母、数字、`-`、`_` 以外的字符都换成 `-`（`D:\work\app` → `D--work-app`），没有项目的会话放在 `no-project/`。之后切换工作目录不会移动会话目录，按 ID 定位时扫描 `sessions/*/`。`br1-`、`evt1-`、`call1-` 是内部 ID。

## 会话账本

一个 `SessionService` 里只有一个 session actor 持有 SQLite 写连接。写入先 `reserve_event` 预留单次事件，再在期望头上 `append`；generation fence 把提交窗口和 publication retirement 排开。冲突不是错误：`HeadWriter` 按实际头重新预留并再提交一次。

新写入的事件种类只有 `Message`、`ToolCall`、`ToolResult`；`Usage` 和 `Task` 只用于打开旧账本。压缩摘要在账本里是一条 display-only 文本，回放时跳过，避免 `covered_messages` 偏移。同时驻留的账本上限是 8 个；有未消费预留的会话不会被淘汰，数量可短暂超出。

侧栏列表只读索引，不扫日志。打开会话只读索引里的偏移行；payload 按行延迟读取。JSONL 尾部超过已提交长度时截掉；日志比已提交长度短、偏移不连续、tool call/result 配对错、digest 或结构不符，都报损坏，不猜。

一个会话最多 64 个分支。回放从分支头开始，一页最多读 256 条事件；tool call/result 用 `call1-` 身份配对，结果按模型给出的 call 顺序重排。fork 和 rewind 把源分支的 JSONL 前缀复制到新分支。

删除顺序是：先取消该会话的回合和子代理，`service.forget` 赶出内存账本，再删索引行、会话目录和旧的 `checkpoints/<id>/`，项目文件夹空了也一并删掉，最后清 `ui.json` 里的绑定。索引里校验失败的会话作为 `corrupt` 行返回，侧栏仍能显示并删除，不会让整个列表失败。

压缩检查点只影响发往模型的历史，不重写账本已有事件。摘要请求超时、被截断、太短或检查点写失败时，本次请求用原历史。摘要被截断或太短时最多重试一次。手动 `/compact` 走 `CompactSession`，强制压缩并先取消正在跑的回合。

编辑或撤回会从目标事件处新建分支；旧事件仍留在磁盘上。工作区文件不恢复，也不删除。

## 运行时边界

**权限。** 工具以当前用户权限运行，没有每次调用前的许可弹窗。schema 校验通过的调用直接执行。环境变量白名单不是沙箱。生产回合只有压缩用的请求钩子（发送前和超长重发），没有 before-tool 观察者，也不为改文件装快照。`write`、`edit` 和 shell 改过的文件没有撤销；编辑或撤回对话也不恢复工作区文件。`ask_user` 通过事件把问题送到前端，答案走 `AskAnswer`；dismiss 让工具拿到可捕获的错误，不取消回合。

**进程工具。**

- 注册名在进程启动时固定，整个进程不变：两种 PowerShell 都叫 `powershell`，bash/zsh/sh 各用自己的名字，Windows 上两种 PowerShell 都没有时才是 `cmd`。模型直接调用这个名字，不是调用 `tools.shell`。
- `script` 模式走解释器；`program` 模式直接启动 PE/ELF/Mach-O，或按直接 shebang 改执行它的解释器；批处理和带参数的 shebang（如 `-c`）被拒。
- 启动会钉住程序映像；取消或超时回收整棵进程树（macOS 进程组加 `setsid` 子孙，Windows Job Object）。主进程退出后最多再等 50 ms 读管道，不等后台子进程关闭继承的写端。
- `script` 模式也能改文件：bash/zsh/sh 用 Python 的 quoted heredoc 或短脚本，PowerShell 用 here-string 管道给 `python`；`program` 模式不经过 shell。
- PowerShell 用 UTF-16LE Base64 的 `-EncodedCommand`，命令不在 shell 之间翻译；捕获输出会清 CLIXML 和 ANSI 控制序列。
- Windows 选择顺序是 `pwsh`、Windows PowerShell 5.1、`cmd.exe`，不选 Git Bash/WSL；macOS 用 `$SHELL` 里的 bash/zsh/sh，否则用 zsh。
- `MYCODE_SHELL` 只提示一次并忽略；`tools.shell` 和 `tools.osSandbox` 已从设置移除，旧文档按未知字段重置。

**MCP。** 支持 stdio 和 Streamable-HTTP。配置来自 `settings.json` 的 `mcpServers` 和工作区的 `.mycode/mcp.json`。单条消息最大 1 MiB，每个 server 最多 128 个工具。工具数不超过 4 且每个 schema 不超过 2048 字节的 server 直接按原始工具名注册，但不会覆盖已有的同名工具；其余工具不直接注册，模型用 `search_tool` 查 schema、用 `use_tool` 调用。客户端按 server 复用，settings 指纹变化时重建；单个握手最多等 20 秒，连接失败的 server 跳过，不会让回合失败。

**子代理。** 角色来自内置的 `scout`、`artisan`，`<home>/agents/*.md` 和工作区 `.mycode/agents/*.md`；后一层覆盖前一层。`scout` 是硬只读，只能拿 `read`/`grep`/`find`/`web_search`/`fetch_content`。`artisan` 默认 `worktree` 隔离：在一个 detached git worktree 里工作，结束时把 diff（最多 256 KiB）附在结果后面并删除 worktree，不会合回主工作区，也不提交。只有可写角色能拿 worktree。单回合最多 8 个并发子代理。子代理不能再调用 `agent` 或 `ask_user`；它和主会话走同一套压缩，只在内存里进行，窗口按子代理自己的模型计算；只读角色的 MCP 工具按 `readOnlyHint` 过滤。子代理的提示同样带上会话工作区的 `AGENTS.md` 资源。

**模型与缓存。** provider 的地址、协议和模型来自 settings。endpoint 按协议补 `/v1/messages`、`/chat/completions` 或 `/responses`，base 已带路径时不重复；未知协议和 provider 未列出的 model 直接拒绝。请求体差异（thinking 开关、`cache_control`、`prompt_cache_key`）在 `family.rs` 的表里按主机/模型匹配，最后一行是通用兜底。会话 id 作为 `prompt_cache_key`（截到 64 个标量）；明确拒绝该字段的主机会在进程内被记住并跳过。OAuth refresh 用全局锁串行，避免 refresh token 被并发消耗。system prompt 在文件不变时保持字节稳定，所以同一会话的请求能命中前缀缓存。用量按 response cycle 累加，上下文表只取最近一次 prompt。

**模型目录。** 完整目录从 models.dev 下载到 `catalog-cache.json`；24 小时内不重复请求，过期后用 ETag 条件 GET。失败保留旧缓存，再失败用编进二进制的 Codex/OpenDesign 保底行。CI 不维护打包的完整目录。

**上限。** 单条事件 8 MiB（usage 64 KiB），单条请求 8 MiB；shell 输出保留 50 KiB；MCP 内联结果 20 KiB；压缩摘要最多 24,000 字符；单个会话最多 512 MiB；`ui.json` 最多 128 个会话模型固定项；输入框最多 64K 字符，发送队列最多 8 条；每个回合最多 8 个子代理并发。

**性能与构建。** `grep`/`find` 在进程内跑；HTTP 用操作系统 TLS（Windows schannel、macOS Security.framework）。改动面板由文件系统事件触发 `git status`，以 `--no-optional-locks` 只读执行，不按固定间隔轮询。界面被事件唤醒，核心线程和界面互不阻塞。release 用 fat LTO 和单个 codegen unit；dev/test 只保留 line tables。

**更新与安装。** 更新器按可执行文件名找 release 资产，下载 zip 和 `.sha256`，校验后把 `{stem}.pending`、`.sha256`、`.version` 放进 `update/`。Windows 由等待进程退出的 helper 完成替换；macOS 由 `update-*.sh` 立即替换。下次启动会重试未完成的 payload。debug 构建和 `MYCODE_NO_INSTALL` 会跳过自安装和自动暂存，手动点安装仍会替换当前二进制。`mycode-cli` 首次运行会把自身复制到 `bin/` 并把该目录补进 PATH。同一个家目录只允许一个桌面进程（`<home>/mycode-desktop.instance` 上的独占锁，进程退出即释放）：会话账本每个进程只有一个写入者，两个桌面打开同一会话会互相把对方已提交的 JSONL 当残尾截掉。终端版只写自己新建的会话，可以和桌面版、其他终端同时运行。

**CI。**

- PR 只跑 `check`：`windows-latest` 上 `cargo test --workspace --locked` 和 `cargo clippy --workspace --all-targets --locked -- -D warnings`。
- 推 main 时同一份 `check` 再跑，并在 windows-latest、windows-11-arm、macos-latest 上构建 `mycode-desktop` 和 `mycode-cli`；发布构建用 `RUSTFLAGS=-Dwarnings`，不跑测试或 clippy。
- `check` 和三个构建都成功，且发布计划要求发布时，才打标签。计划优先用 `v<workspace version>`；该标签已存在时，在临时 `ci/release-*` ref 上把 patch 版本加一，再构建并发布。
- 发布说明是该版本 `CHANGELOG.md` 加上自上一个标签以来的 PR 和关闭的 issue。
- 每个 tag 产出 desktop 和 CLI 的 zip、macOS desktop 的 dmg 及各自的 sha256；资产名按可执行文件名区分，自更新按同一规则找包。
- 没有 fmt 任务，也没有 Linux 任务；产品不支持 Linux。

各 crate 的模块细节见 [core.md](core.md)、[config.md](config.md)、[providers.md](providers.md)、[agent.md](agent.md)、[app.md](app.md)、[tools.md](tools.md) 和 [desktop.md](desktop.md)。
