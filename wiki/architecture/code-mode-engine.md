<a id="code-mode-engine"></a>

# CodeMode 协议

MYCode 的 CodeMode 使用 rquickjs 嵌入 QuickJS-NG。模型提交同步 JavaScript，通过 call(name, args) 调用当前运行已注册的工具；长任务由 wait_code 收集同一个 cell。它与 magpie 提供的远端 CodeMode 服务是不同的执行环境。

<a id="model-contract"></a>

## 模型调用约定

主回合直接暴露平台 shell、run_code 和 wait_code。其他内置工具的参数定义进入系统提示的 Tools inside run_code；MCP schema 按需加载。子代理经过角色过滤，实际可用能力以当前注册表为准。脚本不能添加 shell 或写权限。

call 是同步调用。工具返回后才执行下一条 JavaScript，多次 call 不会并行。正常返回包含 content 与 is_error，文本读取 result.content[0].Text，注意大写 Text。先检查错误，再进行依赖操作：

```javascript
const result = call("read", { path: "Cargo.toml" });
if (result.is_error) throw new Error(result.content[0].Text);
complete(result.content[0].Text);
```

工具失败通常是结果数据，不自动抛出 JavaScript 异常。未知工具、调用前拒绝和 ToolError 还附带兼容字段 __error__。忽略内部 is_error 后正常结束，外层 cell 仍可能显示 completed；complete(result) 不会自动继承内部错误状态。

<a id="submit-and-collect"></a>

## 提交、等待与停止

| 工具 | 参数 | 约定 |
| --- | --- | --- |
| run_code | script | 同步 JavaScript 源码 |
| run_code | yield_time_ms | 默认 30,000 ms，最多 300,000 ms；0 立即交回控制权 |
| run_code | max_output_bytes | 默认 32 KiB，限制到 1 KiB–4 MiB |
| wait_code | cell_id | 继续收集实际返回的同一执行身份 |
| wait_code | yield_time_ms | 默认 30,000 ms，最多 300,000 ms |
| wait_code | terminate | true 请求取消并返回快照 |

等待到期只决定这次工具调用何时返回，脚本仍可继续。yield_control() 也不会暂停脚本来等待下一次 wait_code。重复等待观察后续 yield 或终态，不会因为旧的 yielded 状态而持续立即返回。

| cell 状态 | 意义 | 外层 is_error |
| --- | --- | --- |
| running、yielded | 仍在执行，返回身份与继续收集提示 | false |
| completed | 返回累计输出；无输出时为 (no output) | false |
| failed | 返回保留输出与可容纳的错误 | true |
| terminated | 返回保留输出与终止标记 | true |

running 和 yielded 不是完成。不要重新提交原脚本代替 wait_code，前面的写文件或启动任务可能已经发生。终态文本是累计快照，重复等待可能返回相同文本；图片每张只交付一次。

<a id="host-functions"></a>

## 脚本函数与输出

| 函数 | 作用 |
| --- | --- |
| call(name, args) | 顺序调用工具；省略 args 或传 null 时使用空对象 |
| log(value) | 追加累计文本并发布进度 |
| complete(value) | 追加最终值并结束求值 |
| yield_control() | 按宿主请求顺序交回控制权，脚本继续 |
| budget() | 返回调用数、操作计数、文本和内存预算 |

complete 会直接结束求值，包围它的 catch/finally 也不会继续。没有 complete 时，脚本最后的非 undefined 值会进入输出；中间工具结果不会自动输出。需要正文时，应明确选择 content 中的文本。

字符串原样输出，对象与数组按 JSON 输出。undefined 在桥接处转成 null；循环引用、BigInt、Promise 不能作为 JSON 结果传出。累计文本在 UTF-8 边界截断后加提示，提示本身可能超过预算；单条 log 实时进度另限制到 2,048 字节。实时进度不等于完整结果。

<a id="execution-and-limits"></a>

## 执行与资源预算

| 资源 | 上限 |
| --- | --- |
| call 次数 | 每个 cell 256 次，失败调用也计入 |
| QuickJS 中断轮询 | 100 次；不是 100 条 JavaScript 指令 |
| JavaScript 堆 | 32 MiB |
| 原生栈 | 256 KiB |
| 脚本源码 | 4 MiB，解析前检查 |
| 待交付原生图片 | 4 张，base64 合计 6 MiB |
| 进程内 cell 表 | 32 个，包括活跃与终态记录 |

系统提示的调用次数直接来自 MAX_HOST_CALLS，避免文案与执行预算漂移。budget() 提供 calls_used、max_calls、operations、max_operations、max_output_bytes、max_memory_bytes。operations 统计中断轮询，不能换算成固定指令数或运行时长。

脚本不支持 async/await、Promise 任务执行、模块导入。没有 Node.js 或浏览器的 require、process、fs、fetch、计时器。文件、命令和网络操作通过已注册工具完成。eval、Function、动态构造入口、Date、performance、Math.random 等非确定性或动态执行入口也被移除。

run_code / wait_code 不能从脚本递归调用。完成信号、调用预算耗尽和 VM 中断不能被脚本捕获后继续执行。

<a id="ordered-dispatch"></a>

### 线程与调度

QuickJS 在独立 OS 线程内创建和使用。宿主请求由 Tokio dispatcher 接收，经共享 prepare_tool_ctx 检查，再调用工具；JavaScript 线程等待结果。RunCodeTool 仅弱引用 registry，运行中的 dispatcher 才保留强引用，避免 registry 与工具相互持有造成泄漏。

run_code 和 wait_code 在外层工具调度中各占一个顺序批次。已经 yield 的 cell 仍能执行，所以这不是应用全局写锁。子代理 scope 与工具自身的路径、取消检查仍然有效。

## 图片与远端结果

内置工具返回的 ContentBlock::Image 放入 cell 的待交付队列，在下一次 run_code / wait_code 结果中作为原生图片块传递，包括失败或终止时已经收到的图片。每张图片只取出一次，后续到达的图片由下一次收集交付。

JavaScript 只收到 Image 元数据：mime_type、data_hash、delivery，不收到 base64。单个工具结果超过图片数或总字节上限时，整批图片拒绝并返回错误；之前已经接受的图片保留。

当前 MCP 结果采取文本投影：text 拼正文，resource 读取文本或 URI，image/audio 转成文字占位；没有可用正文时才采用 structuredContent。因此 MCP 图片不走上述内置原生图片通道。服务端 isError 映射到 is_error。

<a id="mcp-discovery-and-use"></a>

## MCP 发现与调用

| 工具 | 输入与输出 |
| --- | --- |
| search_tool | name 精确加载 schema，names 批量加载前 10 项，name=list 列目录；关键词多匹配最多返回 25 项摘要 |
| tool_search | query 做 BM25 检索，limit 为 1–8，返回完整 schema |
| use_tool | 使用返回的确切 name 与匹配 schema 的 arguments |

search_tool 的 schema 是带 name、server、description、inputSchema 标签的文本；tool_search 返回 JSON 数组文本，包含 name、description、params_schema。两者都从 content[0].Text 读取。连接中的服务器仍在握手，不表示空目录。

```javascript
const result = call("tool_search", { query: "github issue", limit: 3 });
if (result.is_error) throw new Error(result.content[0].Text);
complete(result.content[0].Text);
```

完整 schema 可以复用；工具缺失或 schema 改变时再刷新。use_tool 不保存“是否发现过”的状态，先发现是模型调用约定。能够编译的 schema 先在本地验证，不能编译时交由远端校验。

目录优先使用原名，冲突时使用 server__tool，再冲突则附加编号。限定服务器的名称解析只在该服务器内查找，不会静默改投其他服务器。

<a id="progress-and-child-identity"></a>

## 进度与内部调用身份

内部生命周期通过 code_mode|JSON 发送：started 包含 call_id、name、seq、target，progress 包含原始 message，completed 包含 phase 与 is_error。外层工具仍保留父 call_id，内部调用通常使用 outer_call_id.seq。

实时广播容量为 256。每次等待单独订阅，cell 同时保留 inner_calls 快照，补齐收集间隙的状态。结果 details 包含 cell_id、phase、calls、journal、inner_calls 和 agent_states，仅用于宿主和界面，不作为脚本正文。

agent_states 通过工具结果和会话投影保存，支持历史重放。流程用 revision 区分返工轮次，见[子代理编排](runtime.md#subagent-orchestration)。

<a id="cell-lifetime-and-cancellation"></a>

## 生命周期与取消

cell 按会话范围查询；不能跨会话猜 ID 收集。进程重启不恢复执行，journal 也不是持久化执行检查点。

插入第 33 个 cell 时，优先淘汰最早创建的终态记录；若所有记录都活跃，则终止并淘汰最早创建者。结果可在淘汰前重复收集，不保证永久保留。

每个 cell 使用父回合的子取消令牌。终止 cell 不取消父回合或兄弟 cell，父回合取消仍会向下传播。丢弃等待中的 run_code/wait_code future 也会终止对应 cell。dispatcher panic 转成失败，内部调用守卫发布终态。

terminate=true 先标记 terminated 并请求取消；返回快照不承诺 OS 线程、进程或 worktree 交接已经结束。写入任务应通过 agent_status 确认收尾，范围在收尾前继续保留。取消不会回滚文件写入。

<a id="source-map"></a>

## 实现入口

- [工具参数、提交与等待](../../crates/mycode/src/tools/code_mode.rs)
- [cell、输出、图片和保留表](../../crates/mycode/src/tools/code_mode/cell.rs)
- [QuickJS 与宿主函数](../../crates/mycode/src/tools/code_mode/engine.rs)
- [统一分发与进度收尾](../../crates/mycode/src/tools/code_mode/dispatch.rs)
- [执行协议回归](../../crates/mycode/src/tools/code_mode/tests.rs)
- [取消和错误回归](../../crates/mycode/src/tools/code_mode/regressions.rs)
- [MCP 目录与工具包装](../../crates/mycode/src/app/mcp_tools.rs)
- [工具提示生成](../../crates/mycode/src/agent/prompt.rs)
