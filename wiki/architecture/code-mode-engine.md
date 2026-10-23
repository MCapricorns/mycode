# Code Mode Engine 与渐进式工具加载架构（v2，含评审修订）

> 目标：模型不再逐个发 `tool_call`，而是调用一个 `run_code` 工具提交一段
> 内嵌脚本；脚本里以一等公民方式调用已注册工具（builtin 与 MCP 皆可），由
> 一个运行在 Rust 进程内的引擎执行。同时，MCP 连接、skill 正文、工具
> schema 全部按「连接时机 × 披露时机」两个维度懒加载/渐进披露。
>
> 事实来源：本工作区源码（`crates/`），以及任务输入中 grok-build 与
> codex-cli 的调研摘录。所有行号均为本会话读取到的 `main` 分支内容。
>
> v2 修订：根据评审意见修正了 crate 归属（ToolSearchTool/CodeCells 落点）、
> `rhai` 必需 feature（`sync`）、`McpSlot` 三态所有权、subagent 适配、
> journal 双 hash、以及多处「现状 vs 变更」措辞。评审指出的 Step A 契约
> 变化（schema 进 `Request.tools`）在 §5.2 明确标注。

---

## 1. 引擎选型：内嵌 Rhai

**决定：Rhai（`rhai` crate），不是 V8、不是 Lua、不是自造 DSL。**

| 候选 | 结论 | 理由 |
|---|---|---|
| **Rhai** | ✅ 选用 | 纯 Rust（无 C/C++ 构建链），与 edition 2024、现有 `serde`/`schemars` 生态零摩擦；API 专为「宿主注册函数 + 沙箱上限」设计，grok-build 已在生产里用它跑 model-authored 脚本 |
| **mlua/Lua** | ❌ | 需链接 C Lua 或 LuaJIT，破坏纯 Rust 约束；async/await 桥接要手驱 coroutine，与 `ToolDyn::execute_dyn` 的 boxed-future 模型不贴合 |
| **Starlark** | ❌ | 语义偏「构建描述」而非通用脚本；宿主扩展点重 Bazel 式 module，胶水代码表达力弱 |
| **Rust DSL / proc-macro** | ❌ | 要求模型产出可过 `rustc` 的源码；无运行时沙箱可言 |
| **V8（codex 路线）** | ❌ | `rusty_v8` 引入二进制依赖与 FFI；跨进程 host 要再设计 gRPC，与本 workspace 单进程形态冲突 |

**硬性依赖（评审修订）：`rhai` 必须开 `sync` feature。** `rhai::Engine` 默认
`!Send`（docs.rs/rhai 明确 "Engine is neither Send nor Sync. Use the sync
feature"），而 §3.1 的执行模型依赖 `std::thread::spawn` 跑脚本线程——不开
`sync` 整个方案编译失败。依赖写作：

```toml
# workspace [workspace.dependencies]
rhai = { version = "1.27", features = ["serde", "sync"] }
```

（`version = "1.25"` 也行但偏旧；`"1.27"` 是当前 stable 线，`sync` 不可省。）

### 脚本契约（模型可见）

```rhai
// model-written code; every call below is a real tool call.
let issue = call("use_tool", #{ name: "gh_issue_get", arguments: #{ number: 42 } });
let body = issue.content[0].text;
call("write", #{ path: "notes/issue-42.md", content: body });
complete("wrote notes/issue-42.md");
```

`call` 是唯一工具入口。`use_tool`/`search_tool` 本身也是 registry 里的工具，
所以 code-mode 下的 MCP 调用与直接 tool_call 走**完全相同的 catalog 逻辑**，
不需要第二条 MCP 通道——但这也意味着 `call("use_tool")` 能否成功取决于
`use_tool` 是否已在同一 `ToolRegistry` 里注册（见 §4 与 §10 风险）。

---

## 2. Crate 落点：`mycode-tools`（引擎）+ `mycode-app`（元工具/cell 表）

评审指出的真实约束：`use_tool`/`search_tool`/`DynamicMcpTool`/`McpCatalog`
**全部在 `mycode-app`**（`mcp_tools.rs:769-940, 748-766`），`mycode-tools`
看不到它们。因此分两层：

- **引擎在 `mycode-tools`**：`RunCodeTool`、`WaitCodeTool`、`Engine`、
  `HostDispatch`、`ScriptJournal`。它们只依赖 `ToolDyn`/`prepare_tool_ctx`/
  `ToolCtx`/`ToolStream`/`ToolRegistry`，不依赖 `McpCatalog`。`RunCodeTool`
  持有 `Arc<ToolRegistry>` 做**运行期查表**——构造时不要求 `use_tool` 已
  注册，脚本 `call("use_tool")` 在 turn 内真正发生时 registry 已全装好
  （`run_chat_turn_on` 顺序见 §4）。
- **MCP 元工具与 cell 表在 `mycode-app`**：`ToolSearchTool`（BM25 语料需要
  `DynamicMcpTool.snippet`，`pub(crate)` 字段，`mcp_tools.rs:37`）、
  `CodeCells` 表挂 `CoreState`（与 `mcp_pool`/`subagent_cancels` 同级，
  `state.rs:47,105`）。

模块划分（`clippy.mod_module_files = "deny"`，`Cargo.toml:16-18`）：

```text
crates/mycode-tools/src/
├── code_mode.rs            # RunCodeTool + WaitCodeTool (Tool impl)
├── code_mode/
│   ├── engine.rs           # rhai::Engine 加固 + 宿主函数注册（须 sync feature）
│   ├── dispatch.rs         # HostDispatch: mpsc 派发到 Arc<ToolRegistry>
│   ├── journal.rs          # (seq, kind, blake3(args), blake3(result_json)) 日志
│   └── cell.rs             # ScriptCell/ScriptEvent/CodeCellState（类型在 tools，
│                           #   但 Arc<CodeCells> 实例由 app 层 CoreState 持有）
└── lib.rs                  # pub mod code_mode; pub use …

crates/mycode-app/src/
├── tool_search.rs          # ToolSearchTool（BM25，读 DynamicMcpTool.snippet）
└── state.rs                # CoreState.code_cells: Arc<CodeCells>
```

`RunCodeTool` 实现 `Tool`（typed `Args = {script, yield_time_ms?,
max_output_bytes?}`，schemars 出 schema）。`WaitCodeTool` 同理，参数
`{cell_id, yield_time_ms?, terminate?}`。

---

## 3. 执行模型与沙箱

### 3.1 脚本线程 ↔ 宿主 dispatch

```text
agent dispatch (tokio task, core new_current_thread runtime)
   │  RunCodeTool::execute(args, ctx, out)
   │     spawns: std::thread::spawn(move || run_script(engine, host_tx))
   │        └── rhai::Engine::eval_with_scope(script)
   │             └── call("read", #{path: ...})
   │                   └── host_tx.send(HostReq::Call { seq, name, args, reply })
   │                   └── reply_rx.blocking_recv()          ← script blocks
   │
   └── dispatch loop (same execute future, holds &ToolCtx + Arc<ToolRegistry>)
         └── while let Some(req) = host_rx.recv().await
               └── prepare_tool_ctx(...) → tool.execute_dyn(args, &ctx, &mut out)
                     └── oneshot::send(result)  → script resumes
```

- **脚本线程不是 tokio task**，也不能在里面 `tokio::spawn`/`spawn_blocking`/
  `.await`——它是裸 `std::thread`，只做 `eval` + `blocking_recv`。所有异步
  收敛在宿主侧 `execute` future 内的 `host_rx.recv().await` 循环。
- **`execute` future 里绝不可 `block_on`**：core runtime 是
  `tokio::runtime::Builder::new_current_thread`（`dispatch.rs:28-36`），
  在 runtime 线程上 `block_on` 会 panic；而且 `execute` future 被
  `dispatch_response_calls` `tokio::select!` 轮询，block_on 会卡死整个
  core。channel 全部走 `async` 的 `mpsc`/`oneshot`。
- `call()` 返回 `ToolResult.content` 的 JSON 投影（`Vec<ContentBlock>` →
  Rhai `Dynamic` map），不是 `ToolResultMessage`；code-mode 调用不单独发
  `ToolCompleted`，整个 `run_code` 是一个 call_id 的事件，脚本 `log()`
  与每次 `call` 的工具名经 `out.progress` 汇成 `ToolProgress`。
- 脚本内 `call` **串行**（脚本顺序即依赖表达）。`run_code` 本身
  `parallel_safe() = false`（`tool.rs:184-186`），所以
  `dispatch_response_calls` 会让它独占 batch（`agent.rs:357-437`）——同一
  response 里排在 `run_code` 之后的非 agent 调用要等它完成，30-300s 的
  cell 会阻塞后续调用。取舍可接受，但写进 `prompt_snippet()` 明示模型
  「run_code 是长调用，独占一个 cycle」。

### 3.2 引擎级沙箱

在 `code_mode/engine.rs` 固定加固（须 `sync` feature，见 §1）：

| 加固项 | 值 |
|---|---|
| `set_max_operations` | 1_000_000 |
| `set_max_call_levels` | 64 |
| `set_max_expr_depths` | (64, 32) |
| `set_max_string_size` | 4 MiB |
| `set_max_array_size` / `set_max_map_size` | 65_536 |
| `disable_symbol("eval")` | — |
| `set_module_resolver(DummyResolver)` | — 禁 `import` |
| `on_progress` + turn `CancellationToken` | 每 N ops 检查；cancel → `Err("interrupted")` |
| `timestamp`/`sleep`/`exit`/`print`/`debug` | 打桩报错（`exit`/`print` 会静默吞后续宿主调用，journal 就乱了） |
| 宿主函数白名单 | `call`, `log`, `budget`, `complete`, `yield_control` |

`agent` 不在白名单：`agent` 本身是 registry 里的工具，脚本调它就走
`call("agent", …)`，被 registry 级并发/审批管住——不给 code-mode 开第二条
编排通道。`search_tool` 不是宿主函数而是 registry 工具（`call("search_tool")`），
不需要单列。

**无文件系统/网络/进程 API**：脚本拿不到 `std`/`reqwest`，唯一副作用通道
是 `call()` → `Arc<ToolRegistry>` → 工具自身的 `prepare_tool_ctx` 路径锚定
（`invoke.rs:27-63`）。

### 3.3 预算

- `yield_time_ms`（参数，默认 30_000、上限 300_000）：到点未完成返回
  `Script running with cell ID {cell_id}…`，模型用 `wait_code` 续收或
  `terminate`。
- `max_output_bytes`（默认 32 KiB）：`complete(value)` 与 `log()` 累加
  输出超界即截断注明。
- `max_host_calls = 256`（常量）：脚本内 `call()` 计数阀门，防
  `while true { call(...) }`。
- 每次 `call` 仍走 `prepare_tool_ctx` → `execute_dyn`（schema 校验、
  `repair_args`、取消、progress 全保留），`call_id` 取 `ctx.call_id` +
  `.{seq}` 便于对账。

### 3.4 journal 与确定性（评审修订）

- journal 记 `(seq, kind, blake3(args_json), blake3(result_content_json))`
  双 hash——不只 args，因为 `call` 结果含 `ToolResult.content`（`Vec<
  ContentBlock>`，`tool.rs:36-45`），resume 时重放要能发现「同 args 不同
  结果」的漂移。`result_content_json` 是 `call` 返回给脚本的投影 JSON，
  即 `serde_json::to_value(&result.content)`；UI-only `details`（
  `tool.rs:44`）不进投影也不进 journal。
- `Resume{cell_id}` 校验 seq + 双 hash，分歧即 `ErrorSystem` 终止。
- **确定性边界比 grok 弱**（评审要点）：grok 的宿主函数是纯数据
  （`budget`/`log`/`scratch`），本设计的 `call` 走完整工具链——
  `prepare_tool_ctx` 的 `read_roots`/`extra_roots` 由宿主启动参数决定，
  `run_code` 的 resume 只对 `ToolResult` 可 JSON 往返的工具有效；含
  `prepared_file`/`prepared_search` 句柄的结果（`ctx.rs:29-34`）不参与
  journal 重放——这些工具的 `call` 在 resume 时总是实跑一次，不校验
  result hash。

---

## 4. 调用进入 `ToolRegistry` 的路径

`ToolCtx` 不含 registry（`ctx.rs:17-41`），也不该加——`prepare_tool_ctx`
的调用方（`invoke.rs:27-63` + `turn.rs:499-507`）只传 cwd/roots/cancel/
call_id。`RunCodeTool::new(registry: Arc<ToolRegistry>, cells: Arc<CodeCells>)`
在注册时注入，持有整个 registry（含它自己）。

注册顺序（`run_chat_turn_on`，`turn.rs:279-311`）：

```text
builtins → AskTool → AgentTool → RunCodeTool → WaitCodeTool → register_mcp_tools
```

**RunCodeTool 构造时 `use_tool` 尚未注册**（`register_mcp_tools` 在最后），
这无碍——`call()` 是运行期 `registry.get(name)` 查表，脚本真正执行时
`use_tool`/`search_tool` 已在表里。但要在文档与 `prompt_snippet` 里写明
「`call` 的参数名即 registry 名」；若未来想把 `mycode-tools` 独立用在别的
宿主里，必须先注册 `search_tool`/`use_tool`，否则 `call("use_tool")` 命中
`unknown tool`（`turn.rs:489-494` 的报错路径）。

**拒绝自递归**：`call("run_code")`/`call("wait_code")` 直接
`ToolError::InvalidArgs`（codex 同款 `exec cannot invoke itself`）。

---

## 5. 渐进/懒加载设计

分开**连接时机**与**披露时机**——这正是 grok/codex 的共识，而本工作区
目前把两者耦合：MCP 工具从不进 `Request.tools`（`mcp_tools.rs:1-6`），
只靠 `search_tool`/`use_tool` 元工具物化，且 `connect_mcp_tools` 在 turn
开头阻塞式全握手。

### 5.0 现状基线（评审澄清，先写清再谈改动）

- MCP 工具**从不** `registry.register`——它们只活在 `McpCatalog` 内
  （`mcp_tools.rs:477-540`），`use_tool` 经 `catalog.get(name)` 直调
  `DynamicMcpTool.execute_dyn`（`mcp_tools.rs:910-937`）。Step A 是
  **真实的披露契约变化**，不是「照旧渐进」。
- `connect_mcp_tools` 在 turn setup `.await` 全部握手（`turn.rs:257-264`），
  server 间并行（`join_all`，`mcp_tools.rs:292-316`）但整体阻塞 turn，
  等于 grok 的 Blocking 且无 Progressive 选项。
- `connect_mcp_tools` 内部先 `spawn_blocking` 读 secrets + merge 三源
  配置（`mcp_tools.rs:246-253`）——「spawn 即返」不是免费，首个 cycle
  若不等 handshake 也至少要等这段；要么挪进 spawned future 要么预读，
  见 §5.2.0。

### 5.1 披露矩阵

| 资源 | 连接/发现时机 | Schema 披露时机 | 物化路径 |
|---|---|---|---|
| builtin 6 工具 | turn 构建 registry | 全量进 `Request.tools`（`turn.rs:88`） | — |
| `ask_user`/`agent`/`run_code`/`wait_code`/`search_tool`/`use_tool`/`tool_search` | turn 构建 registry | 同上 | — |
| **MCP 工具** | **Progressive**：握手后台化，首个 cycle 不等 | Step A：就绪即 `registry.register`（schema 进 `Request.tools`）；Step B：`into_deferred()` 只发 name+description | `search_tool`/`use_tool`（catalog 是唯一物化点） |
| **skill 正文** | turn 开头 `discover_skills`（已懒） | prompt 只放 `<skills>` 目录 | 模型 `read` SKILL.md |
| **deferred 原生工具** | — | 第一版无；增长后再迁 | `tool_search` |

### 5.2 MCP 连接：Blocking → Progressive

1. **`connect_mcp_tools` 拆分**
   - `McpPool::resolve(home, settings, project) -> (Secrets, MergedServers)`：
     `spawn_blocking` 读 secrets + merge（`mcp_tools.rs:246-253` 的现有
     逻辑原样保留）。这一步是 spawn 的前置，**首 cycle 前必须完成**
     （否则连要 spawn 哪些 server 都不知道），但它本身极快（几十 ms
     磁盘读），不是握手。
   - `McpPool::spawn_slots(resolved) -> Vec<McpSlot>`：对每 enabled server
     `tokio::spawn` 一个 handshake future，立即返回 slot 数组。
   - `drain_mcp_ready(pool, registry, catalog)`：后台协程，`JoinSet`
     里每完成一个就把 `DynamicMcpTool` 写进 catalog + registry。

2. **`McpSlot` 三态（评审要求写清所有权与跨 turn 语义）**

   ```rust
   enum McpSlot {
       /// In-flight handshake; JoinHandle owned by the POOL, not the turn.
       /// A turn's drain task only LISTENS; it does not own the handle.
       Connecting { server: Arc<McpServerInfo>, handle: JoinHandle<SlotOutcome> },
       Ready { tools: Vec<Arc<DynamicMcpTool>> },
       /// Failed to spawn/handshake/list, or Ready→连接运行期掉线（broken）。
       Failed { reason: String, retry_at: Instant },
   }
   ```

   - `JoinHandle` 归 `McpPool`（`CoreState.mcp_pool`）所有，不归 turn。
     turn 的 drain 协程只拿 `JoinSet` 的完成通知，不持有 handle；turn
     结束时 drop 协程即可，Connecting 态的 handshake 继续在 pool 里跑，
     **跨 turn 复用**（fingerprint 不变时）。
   - `TurnControl.cancel` 触发 → `pool.abort_connecting()` 对未完成
     handshake `JoinHandle::abort`（kill-on-drop 语义对齐现状注释
     `mcp_tools.rs:288-291`「a cancelled turn still drops (and kills)
     every half-started server」）。
   - `Failed{retry_at}` 取代现状全池 `retry_after`（`mcp_tools.rs:341-344`）
     ——现在重试粒度是 per-server，不是「任一失败整池冷却」。
   - `broken` 原子位只标 `Ready→Failed` 的运行期降级（`mcp_tools.rs:38-39`
     现有 `Arc<AtomicBool>` 保留）；Connecting/Failed 无 broken 概念。
   - **缓存早退分支**（`mcp_tools.rs:267-285` 现状「fingerprint 同 +
     slots 全 Some 即复用」）改为：fingerprint 同 → 复用 Connecting/
     Ready/Failed 原样；fingerprint 变 → 整个旧池 drop（JoinHandle abort
     随 Drop）。「全部 Ready 才早退」的语义保留，只是判定条件改成
     `slots.iter().all(|s| matches!(s, Ready|Failed))`——Failed 在
     `retry_at` 内不重连。

3. **注册双轨，单一写入顺序**

   `McpCatalog` **保留**，改成：

   ```rust
   struct McpCatalog {
       by_name: RwLock<HashMap<String, Arc<DynamicMcpTool>>>,
       listed:  RwLock<Vec<(String, String)>>,   // (key, snippet), sorted on write
       servers: RwLock<Vec<(Arc<McpServerInfo>, ServerStatus)>>,
       native:  BTreeSet<String>,                 // built once, unchanged
   }
   ```

   `from_tools` 改为 `empty()` + `insert_ready(server, tools)`；每个 server
   就绪时：
   1. `catalog.insert_ready(...)` 先写 `by_name`/`listed`（加锁重排序）；
   2. 再 `registry.register(tool)`（Step A）或仅在 catalog 可查（Step B）。

   顺序固定「先 catalog 后 registry」：`use_tool` 查 catalog 能命中时
   registry 必然已有对应 spec（Step A），反向不成立（catalog 有而
   registry 无正是 Step B 的形态）。`SearchTool`/`UseTool` 仍只查
   catalog——**catalog 是唯一物化点**，registry 只承载「进 `Request.tools`
   的 spec」；priority 冲突不会发生，因为 Step A 下 catalog key 与
   registry name 由同一处 `insert_ready` 写入，重名先用
   `native`/`by_name` 检查加 `server__tool` 前缀（`mcp_tools.rs:514-521`
   现有逻辑原样搬进 `insert_ready`）。

4. **prompt 契约（评审修订）**

   `system_prompt` 在 cycle 1 由 `build_system_prompt(env.tools)` 固化
   （`mycode-agent/src/turn.rs:74-78`），`prompt_entries()` 不会随后台
   注册更新——cycle≥2 的 `Request.tools` 里会出现 prompt 没列过的
   工具名。workspace 没有 provider 校验此约束，但要显式接受这个不对称，
   并让 `<mcp>` note 的 cycle-1 文案**只列 Connecting 中的 server id**，
   不暗示工具已就绪：

   ```text
   <mcp>
   MCP servers connecting in the background: gh, fs.
   Their tools are not on the tool list yet; once connected they appear there,
   or use search_tool + use_tool to call them the moment they register.
   </mcp>
   ```

5. **Step A 的成本提示（评审新增）**

   `DynamicMcpTool.spec()` 透传 server 完整 `inputSchema`
   （`mcp_tools.rs:74-81`）。Step A 落地后，一个大 server（几十工具、
   schema 各几 KB）就绪那一轮，`registry.specs()` 缓存失效、下一轮
   `Request.tools` 整表带上全部新 schema——`stream_assistant` 每轮都
   `tools: env.tools.specs()` 重发（`turn.rs:85-93`），**可能瞬间 +几十
   条 spec 并废掉 prompt cache key**（`prompt_cache_key` 在
   `AgentConfig`，`agent.rs:47`）。若用户常见大 server，Step B 不是
   可选而是必要的——Step B 下就绪即注册的是 `into_deferred()` 形态
   （schema 清空），`Request.tools` 不暴涨。

6. **Step B（deferred spec）**

   `DynamicMcpTool` 增 `into_deferred()`：注册进 registry 的 `spec`
   只留 `name + description`，`params_schema = {"type":"object"}`；模型
   要靠 `search_tool` 物化完整 schema。catalog 里仍存完整 spec 供
   `use_tool` 前校验。这一步改动 `search_tool` 返回契约（返回结构化
   `LoadableToolSpec` 而非纯文本 `schema_text`）与 prompt 措辞，单独 PR。

### 5.3 `ToolSearchTool` 落点（评审修订）

语料要 `DynamicMcpTool.snippet`——`pub(crate)` 字段（`mcp_tools.rs:37`）。
两个选项：

- **选 A（本设计采用）**：`ToolSearchTool` 放 `mycode-app`（新
  `tool_search.rs`），与 `SearchTool`/`UseTool` 同层，直接读 `snippet`。
  `mycode-tools` 里的 `run_code` 经 `call("tool_search")` 命中，同样走
  registry 查表。
- 选 B（备选）：`ToolSearchTool` 放 `mycode-tools`，语料改经
  `tool.prompt_snippet_dyn()` 取（`DynamicMcpTool` 已实现，
  `mcp_tools.rs:108-110`）——但 snippet 内容与 `DynamicMcpTool.snippet`
  字段是同一份，绕一圈只为 crate 边界不值得，且 BM25 索引建在
  `DynamicMcpTool` 上更自然（能直接用 `spec.name`/`server.id`）。

BM25 索引：corpus 每就绪一个 server 增量插入（tool name + server id +
description + snippet），`bm25` crate `EmbedderBuilder::with_fit_to_corpus`
+ `Scorer`（codex 同款，`codex-rs/core/src/tools/handlers/tool_search.rs`）。
返回至多 8 条 `{name, description, params_schema?}` JSON；`use_tool` 前
catalog 本地校验照旧。

### 5.4 subagent 路径（评审要求）

`spawn_subagent`/`drive_subagent` 也调 `connect_mcp_tools`
（`subagent.rs:418-427`），共享 `state.mcp_pool`；非写角色还做
`read_only` 过滤（`subagent.rs:428-435`）+ `child_registry_with(allowed)`
重建 registry（`subagent.rs:437-441`）。

适配规则：

- 子 agent **不等 handshake**——spawn/drain 后，子 agent 的
  `child_registry_with` 只注册 `Ready` 且 `read_only()` 满足的
  `DynamicMcpTool`；`connect_mcp_tools` 的 spawn 由父 turn 已触发，子
  agent 直接 drain 现成 slot。
- `read_only` 过滤放在 **drain 产出的 `Vec<Arc<DynamicMcpTool>>` 阶段**
  （`subagent.rs:428-435` 现有 filter 位置），不放在注册阶段——注册
  阶段只决定「进不进 child registry」，读不读得动是 catalog/registry
  之前的事。
- `mcp_note`（`subagent.rs:439-440`）的 `child_mcp_note` 同步改成
  「connecting/discoverable」措辞。
- 若子 agent 在父 turn 的 cycle 1 就 spawn，可能看到全 Connecting 的
  池——这是渐进代价，子 agent 的 `use_tool` 会拿到
  「server X is still connecting」错误提示，与主 turn 一致。

### 5.5 披露时序示例

```text
turn start
 ├─ spawn_blocking: resolve secrets + merged servers  (~ms)
 ├─ registry: builtins + ask + agent + run_code + wait_code + tool_search
 ├─ MCP: spawn_slots → JoinSet spawned (NOT awaited); search_tool + use_tool registered
 ├─ skills: discover_skills → <skills> dir in system prompt
 │
 ├─ cycle 1 request.tools = specs()   ← 无任何 MCP schema；prompt 只列 server id
 │   model: search_tool("issue")      ← catalog 空 → 提示 connecting / keywords
 │   model: run_code(script)          ← call("use_tool") 同等待遇
 │
 ├─ (background) server "gh" Ready
 │   → catalog.insert_ready(gh, tools)   →  registry.register(gh__*)   (Step A)
 │   → specs() cache invalidated
 │
 ├─ cycle 2 request.tools = specs()   ← 含 gh__* 完整 inputSchema（Step A）
 │                                    或 into_deferred() 名+描述（Step B）
 └─ model: use_tool("gh__issue_get", …) → catalog.get → DynamicMcpTool.execute_dyn
```

---

## 6. Cell 生命周期与归属（评审修订）

- `CodeCells` 表挂 **`CoreState`**（`state.rs:47` 与 `mcp_pool` 同级），
  类型定义在 `mycode-tools`（`code_mode/cell.rs`）但实例由 app 层持有：
  `CoreState.code_cells: Arc<CodeCells>`。`RunCodeTool::new` 的 `cells`
  参数就是 `state.code_cells.clone()`——registry 每 turn 重建
  （`turn.rs:496` 的 `env` 用 `&registry`），cells 跨 turn 存活，支撑
  `wait_code`/`Resume`。
- cell 落盘（可选）：`~/.mycode/sessions/<id>/cells/<cell_id>.rhai +
  journal`，对齐 grok 的 `script.rhai` + 修订副本。
- turn 结束未 `wait` 的 cell：脚本线程的 host channel 一断
  （`host_tx`/`host_rx` drop）`blocking_recv` 即 `Err`，Rhai runtime
  error 终止线程；显式 `terminate` 走 `CodeCells::terminate` 发
  `RuntimeCommand::Terminate`。

---

## 7. 结果如何回到 agent turn

`run_code` 的 `execute` 返回 `ToolResult`，走 `run_tool_call`
（`turn.rs:459-579`）既有路径：

1. `RunCodeTool::execute` 内部驱动 `engine` + `dispatch loop`，
   `out.progress` 把 `log()` 与每次 `call` 的工具名转发 `ToolProgress`；
   UI 只见一条 `run_code` 调用滚动。
2. 终值经 `out.terminal` 或 `Ok(result)` 返回，`run_tool_call` 照常包成
   `ToolResultMessage`、发 `ToolCompleted`、push history。
3. 下一个 `stream_assistant` 的 `Request.messages` 带上这条 tool result，
   `pair_tool_results`（`turn.rs:99`）保证 wire 上 call_id 配对。

对模型，`run_code` 只是会跑比较久的普通工具；`Feature::CodeMode` 不开
则根本不注册，`Request.tools` 无此项，fail-closed。

---

## 8. 与现状的差异清单（v2 修正版）

| 现状 | 改成 | 备注 |
|---|---|---|
| MCP 工具从不进 registry，只在 `McpCatalog` 内经 `search_tool`/`use_tool` 物化（`mcp_tools.rs:1-6, 748-766`） | Step A：`insert_ready` 双写 catalog + registry，schema 进 `Request.tools`；Step B：注册 `into_deferred()` 形态 | **契约变化**，文档 §5.0 明示 |
| `connect_mcp_tools` 在 turn setup `.await` 全部握手（`turn.rs:257`） | `resolve`（spawn_blocking，ms 级）→ `spawn_slots`（即返）→ `drain_mcp_ready` 协程；`TurnControl.cancel` abort Connecting | secrets 读取是 spawn 前置，不免费 |
| `McpPool.servers: Vec<Option<Vec<Arc<DynamicMcpTool>>>>`（`mcp_tools.rs:207`） | `Vec<McpSlot>{Connecting,Ready,Failed{retry_at}}` | 跨 turn 复用 Connecting 态；Failed per-server 冷却 |
| `register_mcp_tools` 只注册两个元工具（`mcp_tools.rs:748-766`） | 保留 + 每 server 就绪走 `insert_ready` 双写 | catalog 加 `RwLock`，单一写入顺序「先 catalog 后 registry」 |
| `Request.tools = specs()` 每轮全量（`turn.rs:88`） | 不变 | Step A 下大 server 就绪轮 schema 暴涨 + prompt cache 失效（成本明示） |
| `system_prompt` cycle 1 固化 | 不变 | 显式接受「prompt 未列但 tools 里有」的不对称 |
| skill 目录进 prompt，正文靠 `read` | 维持 | 已是懒加载 |
| 无 code mode | tools: `code_mode` 模块；app: `tool_search.rs` + `CoreState.code_cells` | `use_tool`/`search_tool`/`McpCatalog`/`DynamicMcpTool` 本就在 app 层 |

---

## 9. 实现步骤（v2）

1. `Cargo.toml` workspace deps：`rhai = { version = "1.27", features = ["serde", "sync"] }`；
   `mycode-tools` 引用之。
2. `mycode-tools/src/code_mode.rs` + `code_mode/{engine,dispatch,journal,cell}.rs`：
   `Engine::hardened()`（sync feature 下的 `Send` Engine）、`HostDispatch`
   channel、`ScriptJournal`（双 hash）、`CodeCells`/`ScriptCell` 类型。
3. `code_mode.rs`：`RunCodeTool`/`WaitCodeTool`（`Tool` impl，
   `new(registry: Arc<ToolRegistry>, cells: Arc<CodeCells>)`），显式拒绝
   `call("run_code")`/`call("wait_code")`；`prompt_snippet` 写明
   「`call(name, args)`，name 为 registry 工具名；run_code 独占 cycle」。
4. `mycode-tools/src/lib.rs`：`pub mod code_mode; pub use {RunCodeTool,
   WaitCodeTool, CodeCells}`。
5. `mycode-app/src/mcp_tools.rs`：`connect_mcp_tools` 拆
   `resolve`/`spawn_slots`/`drain_mcp_ready`；`McpPool.servers` 改
   `Vec<McpSlot>`；`Failed{retry_at}` per-server 冷却；`broken` 只标
   Ready→Failed；`abort_connecting` 挂 `TurnControl.cancel`。
6. `mycode-app/src/mcp_tools.rs`：`McpCatalog` 字段改 `RwLock`，
   `from_tools` → `empty` + `insert_ready`；`register_mcp_tools` 拆
   `register_meta_tools`（search/use/tool_search）与
   `register_ready_server`（catalog + registry 双写，固定顺序）。
7. `mycode-app/src/tool_search.rs`：`ToolSearchTool`（BM25 索引建在
   `DynamicMcpTool` 上，增量插入；≤8 条 `LoadableToolSpec` JSON）。
8. `mycode-app/src/state.rs`：`CoreState.code_cells: Arc<CodeCells>`；
   `turn.rs` 注册 `RunCodeTool::new(registry.clone(), state.code_cells.clone())`
   与 `WaitCodeTool`；spawn `drain_mcp_ready` 协程。
9. `mycode-app/src/subagent.rs`：`drive_subagent` 改走
   `spawn_slots`/`drain_mcp_ready`；`read_only` 过滤留在 drain 产出的
   `Vec<Arc<DynamicMcpTool>>` 阶段；`child_mcp_note` 改 connecting 文案。
10. `mycode-app/src/turn/prompt.rs`：`<mcp>` note 改「connecting server id
    列表」措辞；`session_system_prompt` 在 MCP 未就绪时也产出 note。
11. 测试：`journal` 双 hash replay 分歧、`run_code`/`wait_code` 自调用
    拒绝、`call("read")` 走通、`use_tool` 在 Connecting 期报错、subagent
    read_only 过滤位置、`McpSlot` 跨 turn 复用（fingerprint 同）。
12. `cargo test --workspace --locked` + `cargo clippy --workspace
    --all-targets --locked -- -D warnings`（CI gate，`.github/workflows/
    ci.yml:95-142`）。

---

## 10. 风险与硬约束（v2）

- **edition 2024**：`rhai` 1.27 支持；`mlua` 拉 C 依赖故排除。
- **`rhai` `sync` feature 不可省**：`Engine` 默认 `!Send`，
  `std::thread::spawn` 编译失败；文档 §1 已写死 `features = ["serde",
  "sync"]`。
- **core 是 `new_current_thread` 单线程 runtime**（`dispatch.rs:28-36`）：
  `RunCodeTool::execute` future 里 `block_on`/`Handle::block_on` 会 panic；
  脚本线程不是 tokio 任务，不能在里面 `tokio::spawn`/`spawn_blocking`/
  `.await`——所有异步收敛在 `execute` future 的 `host_rx.recv().await`。
- **`Arc<ToolRegistry>` 自引用**：`RunCodeTool` 持有整个 registry（含
  自己），必须显式拒绝 `run_code`/`wait_code` 自调用。
- **`ToolCtx` 不含 registry**：`prepare_tool_ctx` 的调用方
  （`invoke.rs:27-63`、`turn.rs:499-507`、`subagent.rs` 子 agent 路径）
  都不传 registry；只能经 `RunCodeTool::new` 注入。
- **`use_tool`/`search_tool`/`DynamicMcpTool`/`McpCatalog` 在 `mycode-app`**，
  `RunCodeTool` 在 `mycode-tools`：`call("use_tool")` 依赖 app 层注册
  顺序；若 tools crate 未来独立使用须先注册元工具，否则 `unknown tool`。
- **`McpSlot` 跨 turn 语义**：Connecting 的 `JoinHandle` 归 pool 不归
  turn，fingerprint 变即整池 drop（abort 随 Drop）；「全部 Ready|Failed
  才早退」保留。这是实现最易出 bug 的点（评审点名）。
- **Step A 成本**：server 就绪轮 `request.tools` 可能暴涨几十条 spec
  并废掉 prompt cache；大 server 常见则 Step B 非可选。
- **journal 确定性边界弱于 grok**：`call` 走完整工具链，`read_roots`
  等宿主参数会变；resume 只对 `ToolResult` 可 JSON 往返的工具有效，
  `prepared_file`/`prepared_search` 句柄结果不入 journal 重放。
- **`run_code` `parallel_safe=false`**：同 response 中其后非 agent 调用
  串行等待，30-300s cell 会阻塞——取舍可接受，`prompt_snippet` 明示。
- **prompt/tools 不对称**：`system_prompt` cycle 1 固化，后台注册的工具
  名不进 prompt 但进 `Request.tools`；无 provider 校验，显式接受。
- **未跑过的 gate**：本会话只实跑 `cargo test -p mycode-tools --locked`
  （153 项过）；完整 `cargo test --workspace --locked` 与 clippy
  `-D warnings` 未在本会话执行，实现后必须跑。
