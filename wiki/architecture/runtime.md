<a id="runtime-architecture"></a>

# 运行时架构

MYCode 将模型任务、工具执行、会话存储与桌面渲染分开。工作区只有两个 Cargo package：

```text
mycode-desktop
└── mycode
    ├── frontend   presentation state and interaction logic
    ├── app        application services and orchestration
    ├── agent      model loop and session ledger
    ├── tools      tools, processes and CodeMode
    ├── providers  model catalog, transport and wire protocols
    ├── config     settings, instructions and roles
    └── core       shared messages, events and contracts
```

内部模块按职责组织，调用者直接导入所属模块。旧的八包链式转导出已经移除；存储格式、工具名称、会话 ID、更新入口和平台支持保持兼容。

<a id="crate-boundaries"></a>

## 包与模块边界

| 位置 | 职责 | 主要入口 |
| --- | --- | --- |
| mycode-desktop | GPUI 窗口、布局、输入、剪贴板和平台集成 | main.rs、workspace.rs、ui.rs |
| mycode::frontend | 视图模型、动作归约、命令驱动、翻译、Git 状态 | frontend/driver.rs、frontend/view_model.rs |
| mycode::app | 核心线程、回合装配、子代理、MCP、更新 | app.rs 的 CoreBridge |
| mycode::agent | 模型与工具循环、提示装配、会话账本 | agent.rs、agent/turn.rs、agent/session.rs |
| mycode::tools | 工具协议、注册表、文件与命令工具、CodeMode | tools/registry.rs、tools/builtin.rs |
| mycode::providers | 模型目录、传输、三种流协议适配 | providers.rs、providers/transport.rs |
| mycode::config | 数据目录、受管配置事务、规则、技能、角色发现 | config/home.rs、config/resources.rs |
| mycode::core | 共享消息、事件、附件、provider 接口 | core/message.rs、core/provider.rs |

运行时不依赖 GPUI。全局 mimalloc 分配器与 Windows CRT 链接声明位于 mycode 的 lib.rs；桌面复用同一声明。合并 package 消除了中间依赖边和重复 manifest 声明，不能单凭 package 数推断 binary 或 RSS 的变化，测量方式见[性能与资源记录](../memory-report.md)。

<a id="a-chat-turn"></a>

## 一次对话回合

1. 桌面把输入转换为 frontend action，driver 向 CoreBridge 发送命令。
2. app 的核心线程使用 current-thread Tokio runtime 处理会话与后台任务。保存用户消息后，回合读取当前 durable head。
3. app 装配模型路由、指令资源、角色目录、工具注册表和取消令牌。请求通过 magpie 网关发往选定的模型来源。
4. agent 消费 provider stream，累积文本、思考与工具调用，完成参数后按工具的并行声明分批执行。
5. tools 对参数与执行上下文做检查，发布进度并返回结果。agent 将结果加入下一次模型请求，直至最终答复或取消。
6. app 将提交事件投影为前端数据，frontend 更新视图模型，desktop 渲染变化。

CoreBridge 的命令与事件 channel 不阻塞发送端，避免 UI 停顿卡死核心线程。附件、单条事件、驻留会话和 CodeMode 各自有局部上限；这些限制不是整个进程的总内存预算。

<a id="progress-to-the-interface"></a>

## 流式显示与进度

文本与思考 delta 先进入短帧队列，默认每 64 ms 应用一批。普通帧按字符数和 delta 数限流，较大积压直接追赶；失败和取消会刷新已经收到的部分内容并尽快结束状态。字符预算检查遇到阈值即停止，不重复扫描整段大型回复。工具启动与结果保持顺序，避免一帧中完全看不到正在执行的阶段。

CodeMode 用 code_mode|JSON 封装内部调用身份，子代理用 agent_state|JSON 发布结构化快照。实时消息与结果 details 都能补齐状态，账本里的 ConversationEntry 保留同一结构用于历史重放。流程 revision 防止旧快照覆盖返工后的新状态。

Markdown 缓存共享解析后的 block 数组；文本改变才重新解析。拖入图片与粘贴图片路径使用同一个大小、格式检查入口，避免两处规则漂移。

<a id="sessions-and-worktrees"></a>

## 会话、分支与文件

sessions.db 保存会话索引，sessions/ 保存按项目、会话和分支组织的事件记录、附件及压缩检查点。分支命令携带 expected head，提交与重放保留明确的事件顺序；账本恢复由 agent::session 处理，不使用配置文件的重置策略。

「撤回并编辑」回到目标消息之前，改变的是对话分支。它不修改 Git、不删除工作区文件，也不撤销工具已经产生的副作用。停止后的下一轮应先检查现有文件与可用结果，再完成剩余任务。

写入子代理通常在独立 Git worktree 中工作。含修改、未跟踪文件或忽略文件的 checkout 会保留，结果给出路径。主代理负责检查和整合，不会发生自动 merge、commit 或 push。

<a id="subagent-orchestration"></a>

## 子代理编排

角色来自内置 Markdown、全局 agents/ 和项目 .mycode/agents/，设置控制启停与模型覆盖。scout、sentinel、skeptic 的只读边界由工具过滤实现，不能通过角色文本放宽。无 shell 的审查角色只能检查给定 diff、源码和执行证据；需要运行的检查必须由父代理提供。

| 角色 | 独立任务 |
| --- | --- |
| scout | 检索一个有边界的问题，提供路径、行号或 URL |
| artisan | 在明确范围中实施并验证改动 |
| sentinel | 检查具体产物与要求，报告可复现缺陷或明确验收 |
| skeptic | 反驳或限定一个明确的假设 |

普通 agent 调用由 dispatcher 顺序执行，CodeMode 的 call 也同步等待。固定 workflow 可以并行调度独立节点。没有全局并发数量上限；phase_id 防止重复阶段，scope.paths / scope.symbols 防止活跃写入任务争用。scope 用于协调，不是权限沙箱。

agent_status 查询保留结果，agent_stop 请求取消，即使所有角色已禁用，这两个控制入口仍可使用。禁用角色阻止新委派与续接。agent_message 对运行中的任务追加引导，对已完成的独立任务用明确摘要创建新运行；写入任务复用原 worktree，不从 HEAD 重建，不支持恢复完整模型会话。

### 固定流程

| 流程 | 节点与完成条件 |
| --- | --- |
| research | 三个独立证据方向 → 合成 → sentinel 验收 |
| code-review | sentinel 审查具体目标 → 独立 sentinel 验收报告 |
| migration | 两个独立 artisan 切片 → 等待人工整合 → artisan gate → sentinel 验收 |

所有流程要求独立验收的最后一行精确为 WORKFLOW_ACCEPTANCE: PASS。code-review 找到缺陷可以意味着 review 任务成功，不意味着被审代码已正确。缺少必要的 diff 或执行证据必须说明，不能将“没有发现问题”当成验收。

migration 的 waiting_integration 是中间状态。主代理整合保留的 worktree 后，显式 resume 或 accept，后续 gate 与验收才检查实际父目录。依赖失败或取消不会启动新的下游工作。

### 显式返工

revise 接收 workflow id、node_id、feedback、expected_revision，在原流程中建立新 revision。修改节点及受影响的依赖重新执行，旧结果和产物保留。写入节点复用原 checkout，必要时再次等待人工整合；取消的流程不能复活，未收尾的工作不能提前返工。

每个会话在进程内最多保留 128 条运行记录与 32 个 workflow。完整报告另写入 agent-results/。启动回执、running 状态和 CodeMode yield 都不是完成；父代理必须收集终态与实际产物。

<a id="progressive-mcp"></a>

## MCP 与工具暴露

MCP 连接池在后台握手。首个模型请求不等待全部服务器；发现结果会标明连接中或失败，不能把连接中误判为空目录。内置工具 schema 放入提示，远端 schema 按需通过 search_tool / tool_search 加载，然后用 use_tool 的 name 与 arguments 调用。

主回合直接暴露平台 shell、run_code、wait_code，其余能力通过同步 call 到达同一注册表。只读子代理只收到 readOnlyHint 为真的 MCP 工具。注册表存在什么能力，提示才应描述什么能力；脚本不能自行补出 shell 或写入权限。

详细协议、图片交付与资源预算见 [CodeMode](code-mode-engine.md#model-contract)。

<a id="settings-and-prompt-resources"></a>

## 设置与提示资源

HomeLayout 解析 MYCODE_HOME 或默认 ~/.mycode。settings.json 和 secrets.json 使用受管文件事务及 revision compare-and-swap；ui.json 使用最后写入者覆盖。合法 magpie 首次绑定可导入，损坏文档先备份再抢救有效字段。临时 I/O 错误不移动或清空健康文件。

规则先读取全局 AGENTS.md，再读取项目 AGENTS.md、MYCODE.md、.mycode/agents.md、.agents/AGENTS.md。每段标注来源；项目覆盖全局，同目录后读规则覆盖先读规则，用户明确指令优先。提示要求修改文件前读取适用的嵌套规则。

技能目录来自 magpie Library 的 skills/。每次提示构造重新读取规则，关闭的技能在目录上限计算前剔除。主代理与子代理使用相同的资源、技能和 MCP 调用约定，角色权限仍由注册表控制。

<a id="windows-background-execution"></a>

## 平台进程与取消

Windows 命令、MCP stdio 和更新 helper 使用各自的后台进程启动路径，普通控制台工作不弹新窗口；图形程序的默认显示行为保留。进程树取消等待终止与回收，worktree 写入范围直到收尾结束才释放。

CodeMode 等待返回、工具超时和用户停止是不同事件。wait_code 继续收集同一个 cell；停止会传播取消令牌，但不会回滚已经写入的文件。相关回归使用真实子进程、管道与 Job teardown 验证。

<a id="source-map"></a>

## 阅读入口

- [运行时模块与分配器](../../crates/mycode/src/lib.rs)
- [应用桥接与协议](../../crates/mycode/src/app.rs)
- [回合装配与工具暴露](../../crates/mycode/src/app/turn.rs)
- [主代理提示](../../crates/mycode/src/app/turn/prompt.rs)
- [工具提示与调用上限](../../crates/mycode/src/agent/prompt.rs)
- [子代理宿主与控制注册](../../crates/mycode/src/app/subagent.rs)
- [固定流程与独立验收](../../crates/mycode/src/tools/builtin/agent/workflow.rs)
- [保留 worktree 与恢复](../../crates/mycode/src/app/subagent/worktree.rs)
- [流式帧队列](../../crates/mycode/src/frontend/stream.rs)
- [视图模型与事件归约](../../crates/mycode/src/frontend/view_model.rs)
- [桌面入口](../../crates/mycode-desktop/src/main.rs)

Windows x64 的本地验证与其他平台的 CI 结果应分别报告。源码和测试定义行为；一次检查只证明它实际覆盖的目标和路径。
