<a id="runtime-architecture"></a>

# MYCode 如何运行

MYCode 的工作分成两部分：mycode 负责模型、工具和数据，mycode-desktop 负责窗口和用户交互。开发时，改模型请求、文件工具或会话存储看前者；改布局、键盘、剪贴板看后者。

<a id="crate-boundaries"></a>

## 代码放在哪里

| 目录 | 负责什么 | 常用入口 |
| --- | --- | --- |
| crates/mycode-desktop | 打开窗口，显示对话和设置，处理键盘与剪贴板 | src/main.rs、workspace.rs、ui.rs |
| crates/mycode/src/frontend | 保存界面当前状态，把用户操作交给后台，把后台结果更新到界面 | driver.rs、view_model.rs |
| crates/mycode/src/app | 连接各项功能：发消息、压缩上下文、调用 MCP、管理子代理和更新 | app.rs 中的 CoreBridge |
| crates/mycode/src/agent | 反复请求模型、执行工具、保存会话记录 | agent.rs、agent/turn.rs、agent/session.rs |
| crates/mycode/src/tools | 读取、搜索、编辑文件，运行命令和 CodeMode 脚本 | registry.rs、builtin.rs、code_mode.rs |
| crates/mycode/src/providers | 转换不同模型 API 的请求和流式回复 | providers.rs、transport.rs |
| crates/mycode/src/config | 读取和保存设置，发现规则、技能和角色 | home.rs、settings.rs、resources.rs |
| crates/mycode/src/core | 各模块共用的消息、事件、附件和接口 | message.rs、provider.rs、attachments.rs |

frontend、app 等是 mycode 内部模块，不是独立的 Cargo package。mycode 不依赖 GPUI，因此多数业务逻辑可以直接测试。mimalloc 分配器和 Windows CRT 链接声明只在 mycode/src/lib.rs 中设置一次。

<a id="a-chat-turn"></a>

## 从按下发送到收到回复

1. 桌面读取草稿和附件，将“发送消息”交给 frontend driver。
2. driver 通过 CoreBridge 把命令发送给后台线程。后台先保存用户消息，再准备模型、项目规则和可用工具。
3. 请求经本机 magpie 网关发往选定的模型服务。provider 模块将不同 API 的回复转成同一套消息格式。
4. 如果模型要求调用工具，agent 执行工具，把结果加入对话，再请求模型。这个循环持续到模型答完或用户停止。
5. 后台不断把文本、工具进度和已保存的消息发给 frontend。desktop 根据更新后的状态刷新窗口。

后台使用 current-thread Tokio runtime。网络和异步等待不会阻塞界面线程；CodeMode 的 JavaScript 另有执行线程。文件操作中的阻塞工作交给专门的工作线程。

<a id="progress-to-the-interface"></a>

## 为什么回复能逐步显示

模型发来的小段文本先短暂排队，默认每 64 ms 合并一批交给界面，避免每个字符都触发刷新。积压很多时会加快处理；失败或取消时，会先显示已经收到的内容再结束状态。工具开始和结束事件保持顺序。

Markdown 解析结果会缓存，文本不变时直接复用。CodeMode 每个内部调用都有自己的编号，界面可以分别显示读文件、搜索和子代理的进度。重新打开历史对话时，保存的工具结果也能恢复这些显示。

<a id="sessions-and-worktrees"></a>

## 对话记录与项目文件是两回事

sessions.db 保存会话索引和已提交记录的位置，sessions/ 保存消息正文、附件和上下文摘要。启动或打开会话时，MYCode 会核对数据库和日志；未完成的尾部写入不会直接当成有效消息使用，损坏会报告出来。

「撤回并编辑」会从目标消息之前另开一条对话分支。它只改变后续送给模型的对话，不执行 Git 回退，也不会撤销模型已经修改的文件。停止后继续任务时，应先查看当前文件和已完成的工具结果。

会话提交带有 expected head，即调用者认为的“最后一条已保存记录”。如果会话已经变化，旧请求不能悄悄覆盖新记录。

写代码的子代理通常使用独立 Git worktree。任务结束后，含修改、未跟踪文件或忽略文件的目录会保留，并在结果中给出路径。主代理负责检查和整合；子代理不会自行 merge、commit 或 push。

<a id="subagent-orchestration"></a>

## 子代理怎么分工

| 角色 | 做什么 | 可以修改文件吗 |
| --- | --- | --- |
| scout | 查找代码、文档和事实，附上位置或链接 | 不可以，也没有 shell |
| artisan | 完成明确范围的修改并运行验证 | 可以，默认在独立 worktree 中工作 |
| sentinel | 检查改动是否满足要求、报告具体缺陷，或对特定假设寻找反例 | 不可以，也没有 shell |

角色提示来自内置 Markdown、全局 agents/ 和项目 .mycode/agents/。设置可以启停角色和指定模型。修改提示词不能给只读角色增加写入权限。需要执行命令的审查，必须由有执行权限的代理提供结果。

agent 调用按顺序执行。活跃写入任务的路径或符号范围不能重叠；这用于避免任务互相覆盖，不是操作系统沙箱。

agent_status 用于查看进度和结果，agent_stop 用于停止任务。关闭全部角色后，这两个工具仍可管理已有任务。agent_message 可以给运行中的任务补充说明；任务结束后，可用摘要明确发起续接。续接会创建新的模型运行，写入任务复用原 worktree；已取消的任务不能续接。

每个会话在内存中最多保留 128 条子代理运行记录，完整报告另存到 agent-results/。收到启动编号、running 状态或 CodeMode 的 yielded 状态，都需要继续收集到任务结束。

<a id="progressive-mcp"></a>

## MCP 什么时候可用

MCP 服务在后台连接，支持标准输入输出和 Streamable HTTP。第一轮模型请求不用等所有服务连接完成；工具查询会说明某个服务仍在连接或已经失败。

模型先用 search_tool 或 tool_search 查看工具名称与参数，再用 use_tool 调用。内置工具说明直接放入提示，MCP 工具的完整 schema 按需查询，避免每轮请求都附上整个远端工具目录。

每次模型请求前重新协调 MCP 配置；新增服务后台握手，移除或改配的服务不再留在后续目录中，已经开始的调用不重放。已收到的 notifications/tools/list_changed 会触发目录重取。连接没有独立的空闲通知读取循环，因此服务端通知在下一次协议交互时被发现。

主回合直接看到平台 shell、run_code、wait_code，其他工具通过脚本里的 call 或 callAsync 调用。read、find、grep 和声明只读的 MCP 调用可以通过 Promise.all 并发。只读子代理也只能获得声明为只读的 MCP 工具。

脚本示例和完整调用规则见 [CodeMode](code-mode-engine.md#model-contract)。

<a id="settings-and-prompt-resources"></a>

## 设置、规则和技能从哪里来

默认数据目录是 ~/.mycode，可以在启动前用绝对路径 MYCODE_HOME 改写。settings.json 保存主要设置，ui.json 保存窗口、项目和界面选择，secrets.json 保存应用管理的 MCP 密钥。

保存主要设置和密钥时，会检查文件版本，防止旧编辑覆盖外部修改。配置文件损坏时先备份，再尝试保留可用字段；临时读取错误不会把健康文件清空。ui.json 的界面状态采用最后一次写入的内容。

规则读取顺序是：全局 AGENTS.md，然后是项目 AGENTS.md、MYCODE.md、.mycode/agents.md、.agents/AGENTS.md。项目规则覆盖全局规则，同目录后读的规则覆盖前面的冲突项，用户明确要求优先。修改子目录文件前，还应读取适用的嵌套规则。

magpie Library 提供 skills/ 和 MCP 配置。关闭的技能不出现在可用目录中。主代理和子代理采用同一套规则读取方式，各角色的工具权限仍然分别控制。

界面监听 Library 文件变化并合并短时间内的通知，自动刷新设置、技能与 MCP 目录。运行中的代理在后续模型请求前重新读取规则、技能元数据和启停状态；技能正文仍按需读取。新派遣的子代理使用最新设置与角色文件，已运行子代理的角色权限保持不变。主回合启动时未注册的派遣能力在下一回合生效。

<a id="windows-background-execution"></a>

## 停止操作会做什么

停止会向模型请求、工具、CodeMode 和子代理传播取消请求。命令工具负责终止并回收子进程；子代理的写入范围直到收尾结束才释放。文件已经写入的部分不会自动回滚。

CodeMode 的等待超时只表示“这次先返回”，脚本仍可继续；terminate 或用户停止才请求取消。应用重启后可以读取已保存的会话和报告，不能恢复之前的 JavaScript 执行。

Windows 的命令、MCP 后台进程和更新助手通常不弹出控制台窗口；图形程序仍按正常方式显示。相关测试会启动真实子进程，检查管道、取消和进程回收。

<a id="source-map"></a>

## 按问题查源码

- [窗口和桌面启动](../../crates/mycode-desktop/src/main.rs)
- [界面事件如何改变状态](../../crates/mycode/src/frontend/view_model.rs)
- [后台命令与事件入口](../../crates/mycode/src/app.rs)
- [模型与工具的一轮执行](../../crates/mycode/src/app/turn.rs)
- [工具说明与模型调用规则](../../crates/mycode/src/agent/prompt.rs)
- [子代理启动与控制](../../crates/mycode/src/app/subagent.rs)
- [worktree 创建、保留和清理](../../crates/mycode/src/app/subagent/worktree.rs)
- [流式文字的批量刷新](../../crates/mycode/src/frontend/stream.rs)

构建和测试命令见 [README](../../README.md#参与开发)，耗时与体积测量见 [性能记录](../memory-report.md)。
