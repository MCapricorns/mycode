# mycode-agent

`Agent` 是 UI 无关、会话无关的循环；`SessionService` 是把对话写成可恢复日志的账本。两层互不依赖。

## 循环

```text
prompt(user_message)
  用户消息进入历史（先不广播）
  loop
    回合已开始时，把 steer 合并成新的用户消息
    用 AgentConfig 和当前历史拼 Request
    before_request 钩子改写 Request；改写后的 messages 写回历史
    在请求副本上运行 pair_tool_results
    Request::validate；第一次通过后发 TurnStarted，再发用户消息
    provider 流式输出，增量作为 AgentEvent 广播
    Done：完整助手消息进入历史
    分发工具调用，结果写回历史
    没有工具调用且没有待处理 steer 时结束
  TurnEnded(Completed | Aborted)
```

`AgentConfig` 是静态配置：`system_prompt` 片段、`reasoning`、`max_output_tokens`、`reasoning_token`、`prompt_cache_key`，原样带进每次 `Request`。`max_output_tokens` 为 `None` 或 0 时不填；空白 `prompt_cache_key` 也不填。

`TurnEnv` 提供这一回合的环境：provider、工具注册表、钩子、取消令牌、事件总线、`cwd`、额外工作区根 `extra_roots`、只读根 `read_roots`、steer 收件箱。`TurnEnv::new` 的默认值是新取消令牌、容量 256 的 broadcast 和进程当前目录。`Agent::seed_history` 可以在第一次 `prompt` 前灌入回放消息；循环本身不打开 `secrets.json`，也不创建会话目录。

### 回合开始与失败

`Request::validate` 失败时先发 `AgentEvent::Error`。如果本回合还没发过 `TurnStarted`，`prompt` 返回错误、把历史恢复成调用前的样子，不补 `TurnEnded`。如果回合已经开始，则发 `TurnEnded(Aborted)` 并返回错误。

`TurnStarted` 每个回合只发一次；用户消息在它之后才广播 `MessageAdded`。取消发生在 `TurnStarted` 之前时，历史同样回滚，且不会出现 `TurnEnded`。

### 流式

循环本身不重试。瞬时失败的重试只在 `mycode-providers` 的 driver 里做，而且只在还没有发出任何事件时重试（见 [providers.md](providers.md)）；一旦有输出，再重放请求会重复已经流出的内容。

- `Done` 到达后，完整助手消息进入历史。如果 provider 的最终消息漏了已经流出的思考或正文，用草稿补上。
- 用户取消：流以 `Cancelled` 结束时，已收到的思考和正文连同 `interrupted by user` 一行留在历史里；两者都为空时不产生消息。这条流没有产出 `Done`，所以本次响应没有成形工具调用。`prompt` 返回 `Aborted`。
- 提供方或传输失败：保留同样的半截消息，中断说明用提供方的原因，`stop_reason` 是 `Error`，回合以 `Completed` 结束。
- `stop_reason` 是 `Error` 时，消息上带 id 和 name 的工具调用不执行，逐个补 `is_error` 结果。
- `stop_reason` 是 `Length` 时，所有工具调用都不执行（参数可能被截断），逐个补错误结果让模型重发。
- id 或 name 为空的工具调用无法配对，直接从消息里丢掉。

### 取消

`prompt` 使用调用方令牌的子令牌。触发后，进行中的流以取消结束；工具派发阶段尚未启动或尚未派发的调用补「未执行」错误结果，正在运行的 `agent` 子调用不再等待，结果丢弃并写回「已中止」错误结果。每个 tool call id 都会有一个结果。`TurnEnded` 是 `Aborted`。

### 工具分发

- 工具名 `ask` 映射到 `ask_user`。未知工具返回 `is_error` 结果，并列出最多 40 个可用工具名，超出部分给数量。
- `prepare_tool_ctx` 为声明了文件或搜索能力的工具绑定路径；失败变成该调用的错误结果。
- 工具通过 `ToolStream` 发进度，派发时转成 `ToolProgress`。每次调用都会发 `ToolStarted` 和 `ToolCompleted`。
- 工具的 poll 和析构都在 `catch_unwind` 里；panic 变成该调用的错误结果，不会冲垮回合任务。未知 panic 载荷只留下通用文本。
- 同一回复里所有 `agent` 调用一起启动，并与其余调用并行。其余调用中，相邻的 `parallel_safe()` 工具合成一批并发执行；不可并行的工具等前一批结束，再按模型顺序单独执行。`Tool` 的默认 `parallel_safe()` 是 `reads_only()` 或声明了 `search_access()`：内置 `read`、`grep`、`find`，网页工具，`search_tool`，以及标了 `readOnlyHint` 的 MCP 工具可并行；`write`、`edit`、shell、`ask_user` 不并行。
- 写回历史的结果按模型给出的调用顺序排列；`ToolCompleted` 事件则在每次调用实际完成时立即到达，所以并行批次的事件顺序可能和调用顺序不同。
- 工具失败、未知工具、参数错误都只是 `is_error` 结果，不会结束回合。回合也不会在工具前给文件做快照。

### 提示词

`build_system_prompt` 由当前注册表生成：

- 固定开头，加 `Available tools:`，每个工具一行名字和可选的 prompt snippet。
- 注册了 `web_search` 或 `fetch_content` 时追加 `<web>`。
- 注册了进程工具时追加 `<tool_calling>`。进程工具只有一个，名字是 `powershell`、`bash`、`zsh`、`sh` 或 `cmd`；PowerShell 5.1 和 7 都用 `powershell`。规则是：`find` 列目录和找文件，`grep` 搜内容，`read` 读文件，`write` / `edit` 改文件；shell 只做构建、测试、git、包管理、运行程序；命令会等到退出，不要起 dev server；批量或生成式改写用 `mode` `script`；`mode` `program` 直接传 argv，不做 shell 解析。
- 注册表不变时，生成的字节不变。发给模型的工具 spec 是注册表里的全部工具，按名字排序。

`AgentConfig.system_prompt` 非空时完全替换默认提示。`mycode-app` 把默认提示拼在自己的身份、环境、skills 和 MCP 片段里，并另加 `<environment>` 和 `<shell>`。工具的具体规则还来自各工具的 prompt snippet，例如 `agent` 工具的 scout 说明。

`HookRunner` 只有一个请求前钩子 `with_before_request`，应用层用它做自动压缩。没有工具前观察者。

## 会话账本

### 布局

- 索引：`<home>/sessions.db`，SQLite，WAL，`synchronous=NORMAL`。它是提交权威：分支头、事件条数、已提交字节数、标题、当前分支。
- 事件：`sessions/<ses1-id>/<br1-id>.jsonl`，一行一个 JSON 对象。行里有事件 id、kind、call id、时间戳、digest、字节数，以及载荷：可读文本、base64，或外部文件名。
- 载荷达到 64 KiB 时写到 `payloads/<evt1-id>.bin`，JSONL 行只留 digest 和文件名；载荷不进 SQLite BLOB。
- `compaction.json` 由 `mycode-config` 管理，不属于账本。

### 事件与上限

事件种类是 `Message`（0）、`ToolCall`（1）、`ToolResult`（2）、`Usage`（3）、`Task`（4）。`Usage` 和 `Task` 是旧账本遗留：仍可打开和读取，本版本不再写入；新的 `Task` 预留会被拒绝。`ToolCall` 和 `ToolResult` 必须带 call id；结果必须对上一个尚未闭合的调用；同一分支上 call id 不能重复。

固定上限：

- 单条载荷 8 MiB，`Usage` 64 KiB。
- 单会话已提交字节合计 512 MiB（行字节加外部载荷）。
- 每会话 64 个分支。
- 每次读取 1 到 256 条。
- 常驻账本 8 个；超出时淘汰最久未加载且没有未完成预留的会话。
- 每会话最多 64 个事件预留和 64 个分支预留；超出时丢最旧的。

### 打开与校验

打开只读索引，不解析 JSONL 内容。加载时校验：同一分支内事件 id 不重复、digest 拼写合法、偏移连续、行长度累计等于 `committed_bytes`、事件条数等于索引、分支头等于最后一个事件、call/result 顺序、分支数 1 到 64、总字节不超过上限。JSONL 比已提交长度长时截掉尾巴；更短则报 `Corrupt`。任何校验失败都 fail closed，不静默修复。

读取载荷时还会核对行里的 id、kind、call id、digest、长度和索引，外部载荷再核对文件长度和 digest。`open` 返回按分支 id 排序的 `heads`，以及 `current`：最后发生持久写入的分支；指针失效时退到最小的分支 id。

### 写入

1. `reserve_event` 校验 kind、call id、ordering 和大小上限，把载荷留在 actor 内存里，签发一次性预留。预留绑定期望头和摘要，不跨进程重启。
2. `append` 消费预留。头已被推进时返回 `Conflict`，并带上实际的头；预留作废。
3. 追加时先截掉超过已提交长度的尾巴，再写外部载荷（排他创建）和一行 JSONL，最后在同一个 SQLite 事务里推进分支头、条数、字节数、事件行、当前分支指针，并在标题为空时写入标题。提交失败会删掉刚写的外部载荷；已经落到 JSONL 的多余行留给下一次打开或追加截掉。

账本只有一个写者，由会话 actor 串行化。应用层的普通发送遇到 `Conflict` 会在实际的头上重新预留并提交；撤回和分叉直接以「会话已经向前走了」失败，不在别的快照上截断。

标题取第一条非助手、非压缩摘要的 `Message` 载荷，折叠空白后取前 60 字符；同时写一个供人看的 `title.txt`，重命名会更新它。

### 分支与读取

`reserve_branch` 检查目标事件和分支数上限，签发新分支 id，并计算覆盖源分支、预留 id、类型、源分支头、目标事件和新分支 id 的 mutation digest。`fork` / `rewind` 在源分支头上做比较交换，复制到目标事件为止的 JSONL 前缀，新分支头就是目标事件，复制出的字节计入会话上限。

读取接口都按快照头固定边界：`read` 返回元数据页，`read_payloads` 在一次 actor 往返里返回元数据加已验证载荷，`read_payload_window` 从快照头向前取最新 N 条并给出 `older` 游标，`load_event` 读单个事件。未知游标返回 `NotFound`，越过快照边界返回 `InvalidArgument`。应用首屏用 `read_payload_window` 取最新 40 条。

### 列表与删除

`inspect_sessions` 只查索引，按更新时间倒序；数据库不存在时返回空列表；没有索引的目录不会出现；标成损坏或身份校验失败的行进 `corrupt` 列表，调用方可以删除。

删除时先 `forget`（`Evict`）清掉内存账本和预留，让在途写入不能再把目录造出来；`Evict` 幂等且不碰磁盘。`delete_session_index` 只删索引行，会话目录和旧检查点目录由 `mycode-app` 删除。

### 运行时

`SessionService` 持有一个串行 actor。每个请求经过准入：最多 1024 个未完成操作和 4096 个活动资源；操作通过容量 1025 的命令通道串行执行。磁盘工作走 `spawn_blocking`，调用方的异步运行时不会被占住。每次 facade 调用有 30 秒的 invoke 和 pull 截止时间；取消或超时只终止这次操作。

存储错误只让当前调用失败，actor 不退出，后续调用可以重试。generation fence 让关闭与在途提交线性化：`shutdown` 先退休发布、关闭活动操作，等所有活动排空后再关闭。单个存储错误不会把服务永久标成不可用。

## 不在这里

选模型、拼 MCP、压缩用的摘要请求、把账本投影成界面，都在 `mycode-app`（见 [app.md](app.md)）。厂商 JSON 在 `mycode-providers`。工具细节在 [tools.md](tools.md)。
