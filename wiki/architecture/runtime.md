
<a id="runtime-architecture"></a>

# 运行时架构

MYCode 是使用 GPUI 的 Rust 原生代理应用。用户在桌面发送消息后，产品层处理动作，应用层装配回合，代理循环请求模型并执行工具，结果写入会话账本，再投影回界面。架构图把这些职责连接到当前快照内嵌的源码。

## 先了解实际操作

主回合向模型提供平台命令行工具、`run_code` 和 `wait_code`；文件操作、用户问答、子代理与 MCP 发现等工具通过同步 `call(name, args)` 使用。工具清单和参数以当次系统提示为准，详见[代码编排引擎](code-mode-engine.md)。

提交工具调用后，要收集到最终结果再判断任务完成。CodeMode 返回执行单元标识表示脚本仍可能运行；继续用同一个标识调用 `wait_code`。流程的 `waiting_integration` 表示还需人工整合，未完成迁移流程。停止操作也需要区分“已发出取消”与“写入任务已收尾并释放声明范围”。

回忆旧消息或切换对话分支只改变对话上下文；它们不会还原工作区文件。子代理工作树的改动也需要明确整合，应用不会自动合并或提交。

<a id="subagent-orchestration"></a>

## 子代理编排

内置角色包括：`scout` 检索，`artisan` 实施有界变更，`sentinel` 审查，`skeptic` 验证明确假设。`scout`、`sentinel`、`skeptic` 在角色文件被覆盖后仍保持内置工具的只读边界：只留下 `read`、`grep`、`find`，远端 MCP 另按只读注解过滤。宿主可把过滤后的工具注册表包装进 CodeMode，但脚本不能借此获得命令行工具或写工具。子代理不能调用父级委派、流程控制或 `ask_user`，因而不能继续嵌套派工。

`agent` 接收自足的 `prompt`、角色 `agent`，可带 `description`、`isolation`、`phase_id` 和 `scope`。子代理看不到父对话，任务说明应写明目标、相关路径、完成条件和检查。写角色默认使用分离检出的 Git 工作树；显式 `shared` 使用父工作区。`phase_id` 防止重复派发已保留的同阶段工作，`scope.paths` / `scope.symbols` 用确切声明协调活跃和待执行写入任务的重叠。声明范围是协作约定，不是文件系统权限或沙箱；省略它不能获得重叠检查的保护。

| 工具 | 实际操作 |
| --- | --- |
| `agent_status` | 省略 `id` 列本会话保留记录；按稳定的运行或流程标识，以及实际运行的 `call_id` 查询状态、结果及完整产物路径 |
| `agent_stop` | 请求取消；用状态查询确认完成收尾，写入任务在 `queued` / `running` / `settling` 时持续占用声明范围 |
| `agent_message` | 活跃任务在下一次模型循环边界接收引导指令；已完成的独立任务可启动新一轮运行，以摘要续接并同步等待 |
| `agent_workflow` | 启动固定依赖图，查询或停止；支持迁移流程手动整合后继续，以及显式返工 |

`agent_message` 当前只支持 `mode: "summary"`，缺省也是此模式。新一轮运行使用旧任务说明、结果摘要及产物引用，绝非恢复原会话。`full-session` 续接不受支持；失败、取消、收尾中或流程所属的任务不能独立续接。已完成的独立写入任务会重开原保留工作树，保留已跟踪、未跟踪及忽略文件的内容；路径缺失、恢复清单或仓库不符时失败，不从 HEAD 重建替代目录。

每个会话的进程内运行时最多保留 128 条运行记录、32 个流程。终态记录可能被淘汰；活跃流程及收尾中的工作影响淘汰和准入。完整结果另写到 `<home>/agent-results/<session_id>/`，快照中的结果会截短。状态可跨回合查询，应用重启后不恢复这些可执行运行时；账本重放得到的是显示快照，保留的工作树和文件产物也不等于恢复了模型历史。

### 固定流程与独立验收

| 流程名 | 必填 `inputs` | 执行顺序 |
| --- | --- | --- |
| `research` | `QUESTION` | 三条独立检索 `primary` / `practice` / `codebase`，之后 `synthesis`，再独立验收 |
| `code-review` | `TARGET` | `review` 生成审查报告，再由另一个验收任务检查报告 |
| `migration` | `TARGET`、`CONTRACT`、`CORE_CHANGE`、`ADAPTER_CHANGE`、`CORE_SCOPE`、`CORE_TEST_SCOPE`、`ADAPTER_SCOPE`、`ADAPTER_TEST_SCOPE` | `apply-core` 与 `apply-adapters` 返回工作树，手动整合后运行父工作区 `gate`，再独立验收 |

`agent_workflow` 内置工具的 `execute` 方法会等待这一轮到达终态或明确的整合交接点。通过 CodeMode 调用时，外层可提前交还控制权，但内部调用仍等待同一流程；须用 `wait_code` 收集同一执行单元，没有自动推送最终答案的另一条通道。

每个流程都要求启用只读 `sentinel` 作为独立 `workflow-acceptance`。只有所有节点完成，并且验收输出最后一行恰为 `WORKFLOW_ACCEPTANCE: PASS`，才判定成功。标记必须唯一且不能在其后追加正文；缺失、含糊或 `FAIL` 均失败。上游失败会阻塞依赖节点，取消不会继续启动新的依赖或验收。审查报告指出真实缺陷仍可以通过“报告已按要求完成”的验收。

迁移流程首轮在 `waiting_integration` 交回写入任务的工作树。父代理先检查并手动整合变更，再对原流程标识调用 `action: "resume"` 或 `action: "accept"`，使用原父 `cwd` 运行集成检查与验收。这里的 `accept` 是继续执行检查的别名，不会跳过验收。不会自动应用、合并或提交变更。

### 显式返工与版本

验收失败或产出节点需要修正时，对同一流程使用：

```javascript
const result = call("agent_workflow", {
    action: "revise",
    id: "workflow-...",
    node_id: "primary",
    feedback: "Verify current versions and correct the obsolete citations.",
    expected_revision: 0
});
if (result.is_error) {
    throw new Error(result.content[0].Text);
}
complete(result.content[0].Text);
```

示例标识和版本必须替换为 `agent_status` 查到的当前值。`node_id` 指向被修正的产出节点，不能选择 `workflow-acceptance`；`feedback` 必须非空，最多 8,000 个字符。返工重跑该节点及所有下游节点，清空本轮 `acceptance` 和 `result`，并重新独立验收。未受影响节点沿用原结果。

`AgentWorkflowSnapshot.revision` 为 `u32`，初始 0，带 `serde(default)` 兼容旧快照；每次合法接纳返工加 1，每个流程最多 16 次。每个重跑子任务使用新的运行 UUID 和调用身份，旧运行记录未被淘汰时继续可查，已有产物文件保留；上下文明确是原任务、旧摘要与证据路径、审查意见和父反馈。写入任务复用原工作树，重跑迁移流程写入任务后再次停在 `waiting_integration`，等待人工重新整合。

`failed`、`completed` 或 `waiting_integration` 的流程可在满足准入条件后返工。过期或重复版本、并行返工、运行中或 `settling`、取消的流程或源节点、失效的源记录及不可用的工作树都会拒绝；还要重新校验角色、原父 `cwd` 与写入任务的声明范围。没有隐式自动重试，也不会自动重启已取消任务。

<a id="sessions-and-worktrees"></a>

## 会话账本与工作树

SQLite `sessions.db` 是提交权威，事件正文保存在 JSONL。打开会话先按提交位置对齐索引和日志，处理未完成尾写；已提交内容无法对齐时明确报损坏。分页显示投影与模型请求使用的类型化历史分开，避免把界面文本当作服务商的工具协议重放。

回忆用户消息时，从该消息严格之前的账本前缀建立新分支；第一条消息会得到空分支，原分支保留。账本的预留凭证与账本头检查和前端的会话、分支和请求代次检查拒绝冲突或过期回复。成功后才把待编辑文本放回输入区，不回滚文件或自动重附附件。

工作树交接在子任务和原生工作线程收尾之后捕获差异、报告路径。清理只移除干净工作树及其租约；存在已跟踪文件的改动、未跟踪文件、忽略文件或检查失败时，都会保留检出目录和恢复清单。取消或下次启动恢复时也遵守此边界。返回的差异有大小上限，不能替代检查保留目录中的实际文件。

<a id="crate-boundaries"></a>

## 各包职责

| 包 | 职责 |
| --- | --- |
| `mycode-desktop` | GPUI 窗口、对话、设置和输入，渲染产品状态并发出动作 |
| `mycode-frontend` | 独立于界面框架的视图模型、状态归约器、驱动、翻译和快捷键 |
| `mycode-app` | CoreBridge 命令与事件边界、回合装配、MCP、子代理、配置和更新 |
| `mycode-agent` | 模型与工具循环、提示装配、会话账本和取消 |
| `mycode-tools` | 工具注册表、文件与搜索工具、命令行工具、CodeMode 和子代理控制协议 |
| `mycode-providers` | OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 适配 |
| `mycode-config` | 数据目录布局、设置权威文档、资源发现和受管文件事务 |
| `mycode-core` | 共享消息、事件、工具规格、服务商接口和附件上限 |

当前 Cargo 依赖形成 `desktop → frontend → app → agent → tools → providers → config → core` 的链。职责划分和依赖可达性是两个角度；跨层类型可通过重导出使用。

<a id="a-chat-turn"></a>

## 一次对话回合的内部路径

桌面发出 `FrontendAction`，前端状态归约器更新产品状态，驱动转为 CoreBridge 命令。应用层先提交用户消息，装配会话、模型路由、提示资源、工具注册表和回合控制，再进入代理循环。

代理请求服务商，消费流事件、分发工具，并把已收到的内容提交到账本。应用投影和前端事件流转为视图模型动作，GPUI 渲染状态。取消结束当前回合，已提交内容保留。

应用当前从 magpie 网关绑定获取模型路由与模型列表；三个服务商协议适配器不等于界面可以绕过网关任意配置路由。工具的实际可用性也由当前工具注册表决定，不能仅凭包内存在某个实现推定可调用。

<a id="progressive-mcp"></a>

## 渐进连接远端工具

共享 MCP 连接池在后台启动标准输入输出或 Streamable HTTP 连接，回合不阻塞等待所有握手。就绪服务器先写入目录，再发布 BM25 索引与工具注册表；目录是发现与 `use_tool` 的名称解析依据。回合持有就绪通知监听器，共享连接池持有握手和可复用连接。

MCP 参数定义按需披露，主 CodeMode 回合通过 `call("search_tool", ...)` 或 `call("tool_search", ...)` 获取，再用 `call("use_tool", ...)` 调用。名称冲突、限定查找及远端结果格式见[远端工具的发现与调用](code-mode-engine.md#远端工具的发现与调用)。

<a id="progress-to-the-interface"></a>

## 进度如何进入界面

CodeMode 通过 `code_mode|<JSON>` 封装内部的开始、进度和完成消息（`started` / `progress` / `completed`），父事件保持外层标识，内部消息封装保存真实子调用标识和终态。`inner_calls` 快照可补齐交还控制权期间错过的实时进度，详见[进度与子调用身份](code-mode-engine.md#进度与子调用身份)。

子代理进度既兼容旧的 `agent|role|phase|detail`，也传递 `agent_state|<JSON>` 结构化事件。稳定的运行标识用于识别保留记录，`call_id` 对应真正的子任务控制身份。结果中的 `details.agent_states` 投影为 `ConversationEntry.agent_states: Option<Arc<[AgentStateEvent]>>`，实时结果和账本重放走同一类型字段；它不靠隐藏文本附带状态。

前端按会话保存结果，终态运行记录不会被旧进度复活。流程的较旧 `revision` 被忽略，同轮已结束节点和验收结论保持单调；更高版本可以替换前轮终态、节点与验收，旧运行记录仍有“以往结果”入口。重放快照只恢复显示，不重新启动后台任务。

对话是否跟随末尾由用户滚动意图决定。桌面在预绘制阶段处理引导卡、问答卡或输入区改变视口后的末尾固定；切换会话或分支从末尾开始，同分支刷新保留阅读位置。

<a id="windows-background-execution"></a>

## Windows 后台执行

控制台工具和 Windows 标准输入输出宿主使用隐藏窗口的启动配置，同时保留重定向管道和进程树取消。命令行工具脚本还设置隐藏启动窗口状态，处理解释器稍后创建控制台的情况。更新助手使用 `CREATE_NO_WINDOW`，避免会覆盖它的 `DETACHED_PROCESS`；有意启动图形界面程序和重启桌面仍保留正常的图形界面行为。

<a id="settings-and-prompt-resources"></a>

## 设置与提示资源

设置采用严格 JSON 权威文档，以修订号比较交换并通过受管文件发布。有效的 magpie 首次创建格式若包含活跃网关绑定且完全没有文档封装元数据，会导入修订号为 1 的标准文档，不报告为损坏修复，也不生成损坏备份。部分或非法文档封装仍按验证与修复规则处理。

数据目录默认是 `~/.mycode`，可由 `MYCODE_HOME` 替换整个根目录。全局 `AGENTS.md` 先作为默认规则，随后加载工作区的 `AGENTS.md`、`MYCODE.md`、`.mycode/agents.md`、`.agents/AGENTS.md`；相同作用域发生冲突时项目规则优先，较后的项目贡献覆盖较前者。贡献标出来源，每个资源文件最多读取 64 KiB，总贡献最多 96 Ki 个字符。

技能从 MYCode 数据目录的 magpie Library `skills/` 发现，提示最多列 32 项。发现元数据和实际读取技能正文是不同操作；前者缓存不会替代模型按路径读取正文。

<a id="source-map"></a>

## 源码与回归依据

下列源码和测试为本文事实依据；测试文件提供回归定位，不代表本次文档审计执行过全部测试。

- [应用回合装配](../../crates/mycode-app/src/turn.rs)
- [模型与工具循环](../../crates/mycode-agent/src/turn.rs)
- [前端流事件适配](../../crates/mycode-frontend/src/stream.rs)
- [子代理角色与只读边界](../../crates/mycode-config/src/subagents.rs)
- [子代理进程与工作树宿主](../../crates/mycode-app/src/subagent.rs)
- [保留记录、准入与收尾](../../crates/mycode-tools/src/builtin/agent/runtime.rs)
- [状态、停止与摘要续接](../../crates/mycode-tools/src/builtin/agent/control.rs)
- [固定流程与验收](../../crates/mycode-tools/src/builtin/agent/workflow.rs)
- [显式返工实现](../../crates/mycode-tools/src/builtin/agent/rework.rs)
- [流程与取消回归](../../crates/mycode-tools/src/builtin/agent/runtime_tests.rs)
- [返工与保留工作树回归](../../crates/mycode-tools/src/builtin/agent/rework_tests.rs)
- [工作树租约与恢复](../../crates/mycode-app/src/subagent/worktree.rs)
- [结构化结果投影](../../crates/mycode-app/src/projection.rs)
- [实时与重放投影回归](../../crates/mycode-app/src/projection/agent_states_tests.rs)
- [会话隔离与版本归并](../../crates/mycode-frontend/src/view_model/subagents.rs)
- [版本归并回归](../../crates/mycode-frontend/src/view_model/reduce/jobs/snapshots/tests/revisions.rs)
- [流程详情与以往结果](../../crates/mycode-desktop/src/ui/context/subagents.rs)
- [会话回忆与历史重放](../../crates/mycode-app/src/ledger.rs)
- [设置导入与修复回归](../../crates/mycode-config/src/settings.rs)
- [提示资源发现](../../crates/mycode-config/src/resources.rs)
