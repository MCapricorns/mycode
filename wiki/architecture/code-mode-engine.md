<a id="code-mode-engine"></a>

# CodeMode：用 JavaScript 批量调用工具

CodeMode 让模型用一段 JavaScript 完成多步操作。例如，同时读取几个文件，整理结果，再修改其中一个文件。脚本在 MYCode 内置的 QuickJS 中运行，通过已注册的工具访问文件、执行命令或调用 MCP 服务。

支持顶层 `await`、async 函数和 Promise。没有 Node.js、浏览器、fetch、require 或模块导入。

<a id="model-contract"></a>

## 先跑通一个调用

将下面的代码作为 run_code 的 script 参数：


```javascript
const result = await callAsync("read", { path: "Cargo.toml" });
if (result.is_error) throw new Error(result.content[0].Text);
complete(result.content[0].Text);
```

callAsync 返回 Promise，await 等待工具结果。结果里的 is_error 表示工具是否失败；文本通常在 content[0].Text，注意 Text 的大写 T。complete 将内容返回给模型并结束脚本。

旧脚本可以继续使用同步 call(name, args)。它会等工具返回后才执行下一行。想同时启动多个调用，应使用 callAsync。

## 同时读取多个文件


```javascript
const paths = ["Cargo.toml", "README.md"];
const results = await Promise.all(
  paths.map(path => callAsync("read", { path }))
);
const text = results.map((result, index) => {
  if (result.is_error) throw new Error(result.content[0].Text);
  return paths[index] + "\n" + result.content[0].Text;
});
complete(text.join("\n\n"));
```

Promise.all 的结果顺序与输入顺序一致，不受哪个工具先返回影响。独立的读取和搜索可以同时运行，减少逐个等待的时间。若后一步依赖前一步，先 await 前一步并检查 is_error，再发起后一步。

<a id="ordered-dispatch"></a>

### 哪些调用能并发

MYCode 根据工具的 parallel_safe_call 声明调度。内置 read、find、grep 可以并发；MCP 工具需要声明 readOnlyHint。write、edit、shell、提问和子代理执行等独占操作会单独运行。

例如依次提交“读 A、读 B、写 C、读 D”：A 和 B 可以同时读；写 C 等二者结束；读 D 等写 C 结束。将调用放进 Promise.all 不会跳过这些顺序要求。

JavaScript 在自己的线程上执行。工具在应用的 Tokio runtime 中运行，等待一个工具不会阻塞其他可并发的工具。

<a id="submit-and-collect"></a>

## 任务没结束时，用 wait_code 继续等

run_code 默认最多等 30 秒。到时间后，如果脚本仍在运行，它会返回 cell_id。cell 是这一次脚本执行的编号。把同一个编号传给 wait_code，继续收集结果。

| 参数 | 用在哪个工具 | 含义 |
| --- | --- | --- |
| script | run_code | 要执行的 JavaScript |
| yield_time_ms | 两者 | 本次最多等多久；默认 30,000 ms，最多 300,000 ms；0 立即返回 |
| max_output_bytes | run_code | 文本输出预算；默认 32 KiB，可设为 1 KiB–4 MiB |
| cell_id | wait_code | run_code 返回的执行编号 |
| terminate | wait_code | 设为 true，请求停止这次脚本 |

**不要通过重新提交原脚本来继续等待。** 前一次执行可能已经写了文件或启动了任务，重新提交会再做一遍。

| 返回状态 | 接下来怎么做 |
| --- | --- |
| running、yielded | 脚本还在执行，用 wait_code 继续收集 |
| completed | 执行结束，读取结果 |
| failed | 执行失败，检查错误和已经产生的结果 |
| terminated | 已请求停止，检查已写入的文件和工具收尾情况 |

yield_control() 会让本次等待尽快返回，脚本继续执行。重复 wait_code 会等新的进展或结束，不会仅因为上一次 yielded 就反复立即返回。

<a id="host-functions"></a>

## 脚本里可以调用什么

| 函数 | 用途 |
| --- | --- |
| callAsync(name, args) | 提交工具调用，返回 Promise；用 await 取结果 |
| call(name, args) | 同步调用工具，直接取结果 |
| log(value) | 输出文本，同时向界面报告进度 |
| complete(value) | 输出最终结果，结束脚本并取消未完成的调用 |
| yield_control() | 让本次等待返回，脚本继续 |
| budget() | 查看已用调用次数、执行计数和资源上限 |

省略 args 或传 null 相当于传空对象。两个调用函数共享同一工具目录和次数上限。脚本不能调用 run_code 或 wait_code，也不能访问当前角色没有的工具。

**先 await 必须完成的操作，再调用 complete。** complete 会立即结束脚本，catch/finally 也不会继续；未完成的工具会收到取消请求，排队中的操作不会再启动。已经产生的文件修改不会撤销。

没有 complete 时，脚本会处理待完成的工具调用和 Promise 任务，然后返回最后一个表达式的值。最后一个值是 Promise 时会等待它。字符串原样输出，对象和数组转成 JSON；undefined 不产生最终输出。只有 log、complete 和最后一个表达式会显示，中间变量不会自动显示。

循环引用和 BigInt 不能转成 JSON。传给 log、complete 或工具参数的 Promise 必须先 await。没有任何工具或待执行任务能够解决的悬空 Promise 会报错，例如 await new Promise(() => {})。

## 出错后怎么处理

工具失败一般返回 is_error: true，不会直接让 Promise 拒绝。因此 Promise.all 结束后仍需检查每个工具结果。未知工具、参数被拒绝等情况还带有 __error__，供旧脚本读取。

JavaScript 异常、未处理的 Promise 拒绝、内存或执行预算耗尽会让整个脚本失败，并取消尚未完成的调用。能处理的 JavaScript 错误可以用 try/catch 或 Promise.allSettled；执行预算和 complete 的结束信号不能被捕获后继续执行。

已经输出的内容会保留。文本超出 max_output_bytes 时会截断并标注；单条实时 log 进度最多 2,048 字节。完整输出与界面上的单条进度长度可能不同。

<a id="mcp-discovery-and-use"></a>

## 调用 MCP 工具

先查工具说明，再按返回的确切名称和参数调用：


```javascript
const found = await callAsync("tool_search", { query: "github issue", limit: 3 });
if (found.is_error) throw new Error(found.content[0].Text);
complete(found.content[0].Text);
```

看完返回的 schema 后，在后续脚本里使用 callAsync("use_tool", { name, arguments })。已有完整 schema 时可以复用；工具消失或参数变化时再查询。

| 工具 | 适合的情况 |
| --- | --- |
| search_tool | 已知名称时精确查询；name=list 查看目录；也接受 names 批量查询 |
| tool_search | 不知道名称时按关键词搜索，最多返回 8 个带完整 schema 的结果 |
| use_tool | 按查到的 name 和 arguments 调用 MCP 工具 |

服务器还在连接时，目录可能暂不完整。限定了服务器的名称不会改投另一个服务器。MYCode 没有内置网页搜索，需要接入提供搜索能力的 MCP 服务。

## 图片如何返回

内置 read 读取图片后，模型会收到原生图片块。脚本只看到 mime_type、data_hash 和 delivery 等说明，不会拿到 base64 图片正文。每张图片只交付一次，后续 wait_code 不会重复发送。

单个工具结果如果超过图片数量或总大小预算，会整批拒绝；之前已接收的图片保留。即使脚本随后失败或停止，已接收的图片仍可交付。

MCP 图片和音频目前只显示文字占位，不走内置 read 的原生图片通道。

<a id="execution-and-limits"></a>

## 当前限制

| 项目 | 上限 |
| --- | --- |
| 工具调用 | 每次脚本合计 256 次 call / callAsync，失败调用也计数 |
| JavaScript 堆 | 32 MiB |
| JavaScript 原生栈 | 256 KiB |
| 脚本源码 | 4 MiB |
| 日志 | 每次脚本累计 4 MiB UTF-8 文本、1024 条；空日志也计数 |
| Promise 任务 | 每次脚本最多执行 10,000 个任务，防止无限微任务链 |
| QuickJS 中断检查 | 100 次；用于限制执行量，不等于 100 条 JavaScript 指令 |
| 待交付图片 | 4 张，base64 合计 6 MiB |
| 保留的执行记录 | 进程内最多 32 个 cell |

日志预算在进入宿主队列前检查，读取日志不会重置累计用量。超限会终止脚本并取消未完成调用，不能通过 JavaScript 的 catch 继续执行。budget() 返回 logs_used、max_logs、log_bytes_used 和 max_log_bytes，分别表示日志条数和字节数的已用量与上限。

没有 setTimeout、Date、performance 或 Math.random。eval、Function 及动态构造入口也不可用。文件、进程和网络访问都通过注册工具完成；这些资源限制不等于操作系统沙箱。

<a id="progress-and-child-identity"></a>

## 界面如何显示进度

每个内部调用都有自己的 call_id，界面因此能分别显示哪个文件在读、哪个工具结束。实时进度通过 code_mode|JSON 传递；返回结果的 details 同时保存 journal、inner_calls 和 agent_states，补上等待间隙错过的状态。

journal 和 inner_calls 按提交顺序排列，即使工具完成顺序不同也不会打乱。journal 只记录调用类型和参数、结果的哈希，不保存可恢复执行的检查点。

<a id="cell-lifetime-and-cancellation"></a>

## 停止和重启

cell 只能在所属会话中查询。停止一个 cell 会取消它的工具，不会取消父回合或另一个 cell；停止父回合则会取消它下面的全部工作。

取消请求发出后，进程退出、文件工作线程和子代理 worktree 可能还在收尾。涉及子代理写入时，通过 agent_status 确认收尾结束。停止不会回滚文件。

应用重启后不能恢复正在运行的脚本。最多保留 32 个 cell；新增记录时先淘汰最早的已结束记录，全部仍在运行时会停止并淘汰最早的一次。需要的结果应及时收集。

<a id="source-map"></a>

## 对应源码

- [run_code / wait_code 参数和入口](../../crates/mycode/src/tools/code_mode.rs)
- [JavaScript 执行与宿主函数](../../crates/mycode/src/tools/code_mode/engine.rs)
- [Promise 任务与工具回复](../../crates/mycode/src/tools/code_mode/engine/promises.rs)
- [并发调度与调用顺序](../../crates/mycode/src/tools/code_mode/dispatch.rs)
- [执行状态、图片和记录保留](../../crates/mycode/src/tools/code_mode/cell.rs)
- [并发、错误和取消测试](../../crates/mycode/src/tools/code_mode/tests/async_calls.rs)
- [可重复运行的耗时对比](../../crates/mycode/examples/code_mode_latency.rs)
