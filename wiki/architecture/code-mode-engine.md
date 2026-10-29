
<a id="code-mode-engine"></a>

# 代码编排引擎

MYCode 的内置 CodeMode 用 `rquickjs` 嵌入 QuickJS-NG，让模型以同步 JavaScript 编排已注册工具。日常操作是提交 `run_code`、检查工具结果；若返回仍在运行的执行单元，则用同一 `cell_id` 调用 `wait_code`，直到得到终态。等待时限只决定本次何时返回控制权。

<a id="model-contract"></a>

## 模型调用约定

主回合在 `Request.tools` 中暴露平台命令行工具、`run_code`、`wait_code`。其余内置工具和桥接工具通过同步 `call(name, args)` 调用；参数定义放在系统提示的 `Tools inside run_code` 部分。远端 MCP 参数定义按需发现。子代理使用按角色过滤的工具注册表，可用工具以当次提示为准。

每次 `call` 等工具返回后才继续执行下一条 JavaScript。适合把文件读取、数组或字符串变换、写入及结果汇总放在一段脚本里；构建、测试、Git、包管理器和用户程序仍通过已注册的命令行工具执行。

`call` 的正常返回形状是 `{ content, is_error }`，文本块读取为 `result.content[0].Text`，注意大写 `Text`。先检查错误，再做依赖它的工作：

```javascript
const result = call("read", { path: "Cargo.toml" });
if (result.is_error) {
    throw new Error(result.content[0].Text);
}
complete(result.content[0].Text);
```

工具失败通常是返回的数据，不会自动抛出 JavaScript 异常。未知工具、调用前检查拒绝或执行返回的 `ToolError` 还带兼容字段 `__error__`。若脚本忽略 `is_error` 并正常结束，外层执行单元仍可能是 `completed`；`complete(result)` 只会输出这个对象，不会继承内部工具的错误状态。

<a id="submit-and-collect"></a>

## 提交与收集

| 工具 | 参数 | 实际行为 |
| --- | --- | --- |
| `run_code` | `script` | 必填，同步 JavaScript 源码 |
| `run_code` | `yield_time_ms` | 默认 30,000 ms；最多 300,000 ms；0 表示不等待执行结束 |
| `run_code` | `max_output_bytes` | 默认 32 KiB；限制到 1 KiB–4 MiB |
| `wait_code` | `cell_id` | 必填，收集原来的执行单元，保留同一执行身份 |
| `wait_code` | `yield_time_ms` | 默认 30,000 ms；最多 300,000 ms |
| `wait_code` | `terminate` | `true` 请求终止执行单元，立即返回其快照 |

等待到期或 `yield_control()` 会让当前工具调用返回，脚本继续运行。`yield_control()` 不会暂停脚本来等待下一次 `wait_code`，也不会建立一个新执行。下一次等待观察下一次交还控制权事件或终态，之前的 `yielded` 状态不会让它反复立即返回。

返回结果按状态解释：

| 状态 | 返回的文本 | 外层错误标记 |
| --- | --- | --- |
| `running` / `yielded` | 执行单元标识、已用调用次数和继续等待或停止的提示；不含当前累计文本 | `false` |
| `completed` | 累计输出；没有输出时为 `(no output)` | `false` |
| `failed` | 已保留输出和可容纳的错误说明 | `true` |
| `terminated` | 已保留输出，附加 `(terminated)` | `true` |

`running` / `yielded` 的外层 `is_error` 为 `false`，但这不代表工作已完成。终态文本是累计快照，重复等待可能重复返回同样文本；图片的交付规则不同，见[图片交付与远端协议边界](code-mode-engine.md#图片交付与远端协议边界)。

例如，在外层工具参数中提交脚本：

```json
{
  "script": "const r = call('read', { path: 'Cargo.toml' }); if (r.is_error) throw new Error(r.content[0].Text); complete(r.content[0].Text);",
  "yield_time_ms": 0
}
```

若返回仍在运行，下一次在外层调用 `wait_code`，传入实际返回的 `cell_id`。不要重新提交脚本来代替收集，否则其中的写操作会再次执行。需要中止时也对该标识调用 `wait_code` 并设置 `terminate: true`。

## 文本输出与图片交付

`log(value)` 追加累计文本并发布进度；`complete(value)` 追加最终值并立即结束求值，连包围它的 `catch` / `finally` 也不会继续。没有 `complete` 时，脚本非 `undefined` 的最后求值结果也会追加到输出。中间工具结果不会自动输出；若 `call(...)` 恰为最后求值表达式，会输出其投影对象。需要纯文本时应明确选择 `content` 中的正文。

字符串原样显示，对象和数组按 JSON 显示。桥接处的 `undefined` 转为 `null`；循环引用、`BigInt` 和 `Promise` 值不能作为这种 JSON 结果传出。JavaScript 数字遵循 IEEE-754 精度。累计文本截断在 UTF-8 字符边界，并追加截断提示；提示本身可使返回文本略超所设上限。每条由 `log` 产生的实时进度另截到 2,048 字节后加提示，不能把实时进度当作完整结果。

### 图片交付与远端协议边界

内部工具返回的原生图片块 `ContentBlock::Image` 会被放入执行单元待交付队列，在下一次 `run_code` 或 `wait_code` 结果中以原生图片块交付，即使这次结果仍在运行、已经失败或已终止。每张图片取出一次，后续等待不重发；稍后到达的图片留给下一次收集。模型能否实际接收图片还取决于所选服务商和模型。

脚本只拿到 `Image` 元数据，包括 `mime_type`、`data_hash`、`delivery`，不含 base64。两次交付之间最多暂存 4 张、合计 6 MiB 的 base64 文本。一个工具结果会让任一限额溢出时，该结果整批拒绝，返回 `is_error: true` 和错误说明；既有图片保留，被拒绝图片没有交付元数据。带错误的工具结果仍可交付其中已获准的图片。

当前 MCP 客户端采用另一种结果投影：文本块（`text`）拼接为正文，资源块（`resource`）读取文本或 URI，图片和音频块（`image` / `audio`）变成文字占位；没有可用正文时才使用 `structuredContent` 的 JSON。因此，远端 MCP 图片和音频当前不走上述原生图片透传路径。服务端 `isError` 映射为工具结果的 `is_error`，传输错误则走执行错误路径。

<a id="mcp-discovery-and-use"></a>

## 远端工具的发现与调用

MCP 连接由应用的共享连接池管理，CodeMode 不另建客户端。服务器在后台握手，就绪工具先进入目录，再进入搜索索引和工具注册表。首次模型请求不必等待全部握手；发现结果会说明仍在连接或连接失败的服务器。

| 工具 | 用法与结果 |
| --- | --- |
| `search_tool` | `name` 指定确切名称时返回参数定义；`names` 批量加载前 10 个名称；`name: "list"` 列出工具；关键词查找在单个匹配时直接返回参数定义，多个匹配时最多列出 25 项摘要 |
| `tool_search` | 用 `query` 做 BM25 检索；返回最多 8 个完整参数定义；`limit` 限制到 1–8 |
| `use_tool` | 以返回的确切 `name` 和匹配参数定义的 `arguments` 调用远端工具 |

```javascript
const result = call("tool_search", { query: "github issue", limit: 3 });
if (result.is_error) {
    throw new Error(result.content[0].Text);
}
complete(result.content[0].Text);
```

`search_tool` 的参数定义是带 `name`、`server`、`description`、`inputSchema` 标签的文本；`tool_search` 返回 JSON 数组文本，每项包含 `name`、`description`、`params_schema`。两者都从 `content[0].Text` 读取。拿到目录或多项摘要后还需加载所选参数定义；拿到完整参数定义后即可据此构造参数。

先发现再调用是模型调用约定，`use_tool` 不记录或强制校验“是否曾取过参数定义”。远端参数先按参数定义修复常见形状；参数定义能编译时本地验证，不能编译时由服务端兜底验证。内置工具如 `read` 直接用 `call("read", ...)`。

目录保留内置名称和三个发现工具的名称。冲突时使用 `server__tool`，仍冲突则从 `__2` 开始加后缀。查找先匹配确切目录键，再匹配无歧义的大小写变体或原工具名。`server.tool`、`server:tool`、`server/tool`、`server__tool`、`mcp__server__tool` 这类限定名称只在指定服务器内解析，未命中时不会改投别的服务器。

<a id="execution-and-limits"></a>

## 内部执行与资源上限

<a id="ordered-dispatch"></a>

### 顺序调度

`run_code` 和 `wait_code` 都不支持并行调度，外层代理调度器在模型给出的调用顺序中为它们分配独立批次。这保证本次分发的顺序；已交还控制权的执行单元仍可继续工作，不能把它理解为整个应用的写锁。

JavaScript 在独立操作系统线程求值，QuickJS 运行时和值始终留在线程内。Tokio 调度器收到宿主请求后，经 `prepare_tool_ctx` 做常规调用前检查，再执行同一份工具注册表中的工具；脚本线程同步等待回复。`RunCodeTool` 以 `Weak<ToolRegistry>` 避免引用环，运行中的调度器才持有升级后的强引用。

<a id="host-functions"></a>

### 脚本内置函数

| 函数 | 作用 |
| --- | --- |
| `call(name, args)` | 顺序执行工具注册表中的工具；省略参数或传入 `null` 时使用空对象 |
| `log(value)` | 追加有上限的文本并发布进度 |
| `complete(value)` | 追加值并终止求值 |
| `yield_control()` | 排在先前宿主请求之后交还控制权，脚本继续运行 |
| `budget()` | 查看 `calls_used`、`max_calls`、`operations`、`max_operations`、`max_output_bytes`、`max_memory_bytes` |

| 资源 | 当前上限 |
| --- | --- |
| `call` 调用预算 | 256 次；未知工具等失败调用也计入 |
| QuickJS 中断轮询 | 100；不是 100 条 JavaScript 指令 |
| JavaScript 堆 | 32 MiB，包含编译后的代码、字符串与容器 |
| JavaScript 原生栈 | 256 KiB |
| 脚本源码 | 4 MiB，解析前检查 |
| 执行单元待交付图片 | 4 张、合计 6 MiB base64 |
| 执行单元表 | 应用进程内合计 32 个，包含运行中和已结束的执行单元 |

`operations` 统计 QuickJS 中断回调。源码注释说明 QuickJS-NG 通常在约 10,000 次循环或函数检查后及部分内建操作内轮询；这不能换算为精确指令数或固定执行时长。轮询也检查取消；阻塞中的工具调用由调度器的取消竞争中断。工具自己的路径规则与权限检查继续生效。

### 不支持的运行方式

脚本按同步方式执行，不支持 `async` / `await`、`Promise` 任务执行或模块导入。文件、命令和网络操作通过 `call(name, args)` 调用已注册工具完成；引擎不提供 Node.js 或浏览器的 `require`、`process`、`fs`、`fetch` 和计时器接口。

`eval`、`Function` 及函数原型上的动态构造入口被移除，也不提供 `Date`、`performance`、`Math.random`、`print`、`debug`、`sleep`、`exit`、`timestamp`。脚本不能递归调用 `run_code` 或 `wait_code`。完成、宿主调用预算耗尽及虚拟机中断不能被脚本捕获后继续执行。

<a id="progress-and-child-identity"></a>

## 进度与子调用身份

内部工具生命周期以 `code_mode|<JSON>` 发到外层进度流。外层事件仍使用父调用标识；消息封装的 `call_id` 才是内部 `ToolCtx.call_id`，形如 `outer_call_id.seq`。

| 种类 | 字段与含义 |
| --- | --- |
| `started` | `call_id`、`name`、`seq`、`target`；在调用前检查开始前发布 |
| `progress` | `call_id`、`name`、原始 `message` |
| `completed` | `call_id`、`name`、终态 `phase`、`is_error` |

内部调用状态为 `running`、`completed`、`failed`、`cancelled`。子调用取消应使用真实子调用标识。`progress.message` 保留原始 `agent|role|phase|detail` 或 `agent_state|<JSON>` 消息，包括其中的竖线和换行。

实时广播通道有 256 项容量；每次外层等待单独订阅，返回后关闭本次流。执行单元保留 `inner_calls` 快照，后续结果可恢复子调用身份和终态；旧记录可缺少 `call_id`、`phase`。外层结果中仅供界面使用的 `details` 包含 `cell_id`、`phase`、`calls`、`journal`、`inner_calls` 和 `agent_states`，不作为脚本的 `call` 正文返回。

子代理结构化快照经 `ToolResult.details.agent_states` 和 `ConversationEntry.agent_states` 传递，覆盖实时投影和账本重放；普通答案文本无需嵌入隐藏 JSON。流程使用 `revision` 区分显式返工轮次，详见[子代理编排](runtime.md#子代理编排)。

<a id="cell-lifetime-and-cancellation"></a>

## 生命周期与取消

执行单元初始为 `running`，可进入仍在执行的 `yielded`，最后到 `completed`、`failed` 或 `terminated`。表在进程内保留，查找要求与创建时相同的会话范围；另一会话无法通过猜标识收集它。重启不恢复执行，也不能从执行日志重建执行单元。

插入第 33 个执行单元前优先淘汰最早创建的终态执行单元；若全部仍活跃，则终止并淘汰最早创建者。终态结果在被淘汰前可重复收集。不要依赖执行单元永久保留或跨进程存在。

每个执行单元使用父回合取消令牌的子令牌。终止该执行单元会取消内部工作，不会连带取消父回合或其他执行单元；父回合取消仍会传到它。取消或释放正在等待的 `run_code` / `wait_code` 异步调用也会终止对应执行单元。调度器触发 `panic` 会令执行单元失败，内部生命周期守卫在取消或栈展开时发布终态，先到达的终态不会被随后事件改写。

`terminate: true` 先标记 `terminated` 并发出取消，返回快照；这个返回不承诺操作系统线程、进程或子代理的所有收尾都已完成。子代理写入任务的声明范围要保留到监督任务和工作树交接完成收尾，需用 `agent_status` 确认。取消不回滚已经完成的文件写入。

执行日志按顺序记录宿主事件及参数、结果的哈希，只提供差异检查基础；不等于持久化执行检查点，也不保证重试写操作只执行一次。`wait_code` 始终收集原来的执行。

<a id="source-map"></a>

## 源码与回归依据

下列链接用于核对行为；测试文件的存在不代表本次文档审计运行过这些测试。

- [工具参数与外层返回](../../crates/mycode-tools/src/code_mode.rs)
- [累计输出、图片队列与保留策略](../../crates/mycode-tools/src/code_mode/cell.rs)
- [同步求值、宿主函数与预算](../../crates/mycode-tools/src/code_mode/engine.rs)
- [共享分发、结果投影与取消收尾](../../crates/mycode-tools/src/code_mode/dispatch.rs)
- [进度消息结构](../../crates/mycode-tools/src/code_mode/progress.rs)
- [运行、等待、语言限制与图片测试](../../crates/mycode-tools/src/code_mode/tests.rs)
- [取消与错误协议回归](../../crates/mycode-tools/src/code_mode/regressions.rs)
- [图片批量准入回归](../../crates/mycode-tools/src/code_mode/image_tests.rs)
- [子调用身份与终态回归](../../crates/mycode-tools/src/code_mode/progress_tests.rs)
- [远端目录、参数定义与冲突名称](../../crates/mycode-app/src/mcp_tools.rs)
- [远端结果文本投影](../../crates/mycode-app/src/mcp_client.rs)
- [远端检索及其回归](../../crates/mycode-app/src/tool_search.rs)
- [主回合工具装配](../../crates/mycode-app/src/turn.rs)
- [提示中的工具暴露规则及其回归](../../crates/mycode-agent/src/prompt.rs)
