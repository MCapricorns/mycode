# mycode-agent

没有界面的代理运行时，里面是两层，互不依赖。

1. **循环**。`Agent::prompt` 对着内存里的历史跑，可以完全没有磁盘。
2. **会话账本**。宿主把已经发生的事件写成可恢复的日志。宿主自己的任务运行时驱动它。

它们放在一个 crate 里，是因为一次对话只对应一份持久历史。工作区里没有「只要循环不要账本」或反过来的第二个调用方。

## 循环

```text
prompt(用户消息)
  消息进入内存历史
  loop
    拼 Request；钩子可以改写它（生产环境用它压缩历史），改写后的消息写回历史
    Request::validate；第一次通过后才发出 TurnStarted 和这条用户消息
    provider 流式输出，增量作为 AgentEvent 广播
    只有完整的助手消息进入历史
    分发工具调用，结果按模型给出的顺序写回历史
    直到模型不再调用工具
  TurnEnded(Completed 或 Aborted)
```

`AgentConfig` 是静态配置：system prompt 片段、`reasoning`、`max_output_tokens`、`reasoning_token`、`prompt_cache_key`，原样带进每次 `Request`。`TurnEnv` 注入这一回合的一切：provider、注册表、钩子、取消令牌、事件总线、工作目录和额外工作区根（`extra_roots`）。循环不打开 `secrets.json`，也不创建会话目录。

请求校验（例如超过 8 MiB）在回合开始之前失败时，`prompt` 返回错误并把历史恢复原样，不发 `TurnStarted`，也不补一个 `TurnEnded(Aborted)`。回合开始之后的错误先发 `Error`，再发 `TurnEnded(Aborted)`。

一条回复里的工具调用：所有 `agent` 调用立即并发启动，其余调用按模型顺序依次执行，两组同时进行。取消时不再等已经启动的 `agent` 调用，它们的结果被丢弃，换成「已中止」的错误结果；还没派发的调用补「未执行」的错误结果，保证每个 tool call id 都有回应。停止原因是 `Length` 时，这条消息上的工具调用一律不执行（参数可能被截断），让模型重发。

取消是调用方令牌的子令牌。触发后，进行中的流以取消结束。已经到达的思考、正文，以及这条助手消息上已经成形的工具调用留在历史里，并附带一行 `interrupted by user`；没成形的工具调用不执行。`prompt` 返回 `Aborted`。工具里的 panic 被接住，变成该次调用的错误结果，不把回合任务冲垮。提供方或传输失败留下同样的半截消息，中断说明用提供方的原因，但回合以完成结束，而不是把气泡清掉。本机没有 `git` 只会影响需要工作树的子代理，失败变成该次工具的错误结果，不会回滚已经出现的助手消息。

`HookRunner` 只有一个请求前钩子 `with_before_request`：改写 `Request`，应用层在这里做自动压缩。没有工具前观察者，回合也不会在改文件之前做快照。

`build_system_prompt` 输出固定的工具契约、注册表里每个工具的一句说明（`Available tools:`），再加 `<tool_calling>` 约定。注册表里没有的名字不会出现在提示里。进程工具只有当前解释器那一个（`powershell`、`bash` 或 `cmd`），约定里的文件编辑走它的 `mode` `script`：bash 用 Python 的引号 heredoc 或短脚本，PowerShell 用 here-string 管道给 `python`。`mode` `program` 不经过 shell，只启动一个可加载映像。应用层另加 `<environment>` 和 `<shell>`，写明 OS、cwd 和这个解释器的语法（PowerShell 5.1 不用 `&&`）。输出里的 CLIXML 和 ANSI 颜色会先收成可读文本，细节在 [tools.md](tools.md)。编辑或撤回一条对话不会恢复或删除工作区文件。`AgentConfig` 没给 system prompt 时才用它；应用层把它拼在自己的提示末尾。

## 会话账本

索引在 `<home>/sessions.db`（SQLite，WAL）。事件在 `sessions/<ses1-id>/<br1-id>.jsonl`，一行一个 JSON 对象。达到 64 KiB 的载荷写到同目录的 `payloads/<evt1-id>.bin`，JSONL 行里只留 digest，不放进 SQLite 的 BLOB。`compaction.json` 仍由 `mycode-config` 放在会话目录里。

事件种类是 `Message`、`ToolCall`、`ToolResult`、`Usage`，外加旧的 `Task`（标签 4）：旧账本还能打开，回放跳过，新预留拒绝。工具结果必须对上一个已经提交的同一身份的 `ToolCall`。

`sessions.db` 是提交权威：分支头、事件条数、已提交字节数、标题（第一条用户消息，折叠空白后取前 60 字符）。崩溃留下的 JSONL 尾巴（超过已提交长度）在打开或下一次追加时截掉。日志比已提交长度短，或读载荷时行的 digest、结构和索引对不上，则报损坏（fail closed）。打开不扫描 JSONL，只读索引。上限：单会话日志合计 512 MiB，单条载荷 8 MiB（用量 64 KiB），每会话 64 个分支，每页最多读 256 条。

写路径是：在期望头上预留（载荷随预留留在 actor 内存里）→ 追加一行 JSONL → 在同一事务里推进索引。预留是一次性的；头已被别人推进时，这次追加返回 `Conflict`（带实际的头），预留作废，多出来的行尾留到下一次打开或追加时截掉。应用层的发送和回合写入遇到 `Conflict` 会在实际的头上重新预留再提交；撤回则直接以「已经向前走了」失败，不在别的快照上截断。

分支可以分叉和回退到某条事件：复制 JSONL 前缀，再按源分支头做比较交换。读是按页的。打开只读索引里的偏移，不把整份 JSONL 读进内存。界面的第一屏再用这些偏移只取尾部一窗载荷。

列表走 `inspect_sessions`，只查索引，不扫目录、不读载荷。索引标成损坏或身份校验失败的行进 `corrupt` 列表，调用方可以删掉它。数据库不存在时列表为空。没有索引的目录不会出现；导入包里的 JSONL 由 `index_imported_session` 补建索引。

删除时，服务先 `forget`（`Evict`）该会话的内存账本，避免在途写入把刚删的目录再造出来；`delete_session_index` 删索引行。和界面、取消的配合在 `mycode-app`。

## 运行时为什么这么重

账本写入经过一个会话 actor：准入、加载索引、动作分发。动作在阻塞线程池上碰磁盘（`spawn_blocking`），调用方的异步运行时不被 `block_on` 占住。准入有上限（同时最多 1024 个未完成操作）。generation fence 保证「发布这一代」和「卸下这一代」不会和仍在进行的活动交错——删除会话时需要这个，否则半次提交会留下一个永久报错的索引行。

账本是单写者。调用方看到的就是上面的预留、追加和期望头。

## 不放在这里的东西

选模型、拼 MCP、压缩用的那次摘要请求、把账本投影成聊天气泡，都在 `mycode-app`。循环只消费已经准备好的 `TurnEnv`。
