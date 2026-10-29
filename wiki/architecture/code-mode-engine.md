# Code mode engine

Code mode uses embedded QuickJS-NG through `rquickjs` to compose registered tools with real JavaScript. It shares the normal tool registry, preflight checks, cancellation and progress pipeline. The runtime is created on its script thread and adds no Node.js/browser filesystem, network API or separate MCP client. `rquickjs` ships bindings for Windows MSVC and Intel/Apple Silicon macOS; Windows MSVC is labeled experimental upstream and requires a C compiler at build time.

## 1. Model contract

The main turn exposes the platform shell, `run_code` and `wait_code` in `Request.tools`. Other registered tools remain available inside the script. Native and bridge argument schemas appear under `Tools inside run_code` in the system prompt. Remote MCP schemas are discovery-only: registered remote tools marked `schema_on_demand()` are omitted from that bulk prompt section; retrieve their full schemas through `search_tool` or `tool_search`. Child roles build their own registry and exposure set; availability follows that role's permissions.

`script` accepts JavaScript only, using ordinary object literals, arrays, arrow functions, `JSON`, string methods, and synchronous `call(name, args)`. Each call waits for its tool result before evaluation continues. Async/await, Promise jobs and module imports are unsupported; the host does not drive a JavaScript job queue. There are no `require`, `process`, `fs`, `fetch`, timers or other Node.js/browser globals. Use registered tools for file and network work.

A normal result has `content` and `is_error`. Text blocks use the enum key `Text`, with a capital T. Dispatch refusals also carry the legacy `__error__` field. Check errors before dependent work.

```javascript
const result = call("read", { path: "Cargo.toml" });
if (result.is_error) {
    complete(result);
}
complete(result.content[0].Text);
```

`complete(value)` records the final output and immediately ends evaluation, including inside a function or `try`/`catch`/`finally`. Without it, the JavaScript script's non-`undefined` completion value becomes output. `log(value)` appends output and publishes progress. Strings render without quotes; objects/arrays use JSON, `undefined` at the bridge becomes `null`, and cyclic objects, BigInt and Promise values are rejected. JavaScript numbers follow IEEE-754 precision. Tool results are available to the script but do not automatically become its final text output.

Native images returned by inner tools are queued on the cell and emitted as `ContentBlock::Image` in the next `run_code` or `wait_code` result, including a running or failed result. Each image is consumed once on delivery; later waits do not resend it. A cell queues at most 4 images and 6 MiB of base64 between deliveries. A tool result that would exceed either limit is rejected as a whole with `is_error: true` and an explicit error; previously accepted images remain available. Rejected images receive no delivery metadata. Script JSON retains an `Image` metadata entry (`mime_type`, `data_hash`, `delivery`) without base64. This keeps image payloads on the provider's native image path even when the script returns a text summary or the image arrives after the initial call yielded.

Prefer in-process JavaScript plus `read`/`write`/`edit` for routine file/data transformations. Shell tools remain available for builds, tests, package managers, git and running user programs, including Python projects.

## 2. Submit and collect

| Tool | Argument | Behavior |
| --- | --- | --- |
| `run_code` | `script` | Required JavaScript source |
| `run_code` | `yield_time_ms` | Default 30,000 ms; capped at 300,000 ms; zero returns without waiting |
| `run_code` | `max_output_bytes` | Default 32 KiB; clamped to 1 KiB–4 MiB |
| `wait_code` | `cell_id` | Required ID returned by an unfinished script |
| `wait_code` | `yield_time_ms` | Default 30,000 ms; capped at 300,000 ms |
| `wait_code` | `terminate` | `true` stops the cell instead of waiting |

A wait deadline or `yield_control()` returns control to the model while the script keeps running. `wait_code` collects that same live cell; it does not rerun the source. Each waiter observes the next yield epoch, so an earlier `yielded` state cannot cause repeated immediate returns.

The returned text names an unfinished cell and explains how to wait or stop it. A completed cell returns its accumulated output. UI-only result details include `cell_id`, `phase`, `calls`, the journal and `inner_calls`.

## 3. Execution and limits

### 3.1 Ordered dispatch

`run_code` and `wait_code` are not parallel-safe. The outer agent dispatcher gives each its own batch, preserving the model's order around writes. Inside a script, calls run sequentially through `prepare_tool_ctx` and the same registered tool implementation used by ordinary dispatch.

The JavaScript evaluator runs on a named OS thread. Its QuickJS runtime and values stay on that thread. A Tokio task receives host requests and executes tools asynchronously; the evaluator waits for each reply. `RunCodeTool` holds a `Weak<ToolRegistry>` to avoid an ownership cycle. The running dispatcher upgrades and retains the registry until its work ends.

### 3.2 Host functions

| Function | Purpose |
| --- | --- |
| `call(name, args)` | Execute a registry tool; object arguments must match its schema |
| `log(value)` | Append bounded output and emit progress |
| `budget()` | Inspect calls used, interrupt polls (`operations`, `max_operations`), output cap and heap ceiling |
| `complete(value)` | Return a value and end evaluation |
| `yield_control()` | Hand control back after earlier queued host requests |

The engine rejects calls to `run_code` or `wait_code` from a script. Dynamic code entry points (`eval`, `Function`, function prototype constructors) are removed. There is no `print`, `debug`, `sleep`, `exit`, `timestamp`, `Date`, `performance` or `Math.random`; use `log` for observable output. Completion, exhausted host-call budgets and VM interrupts use uncatchable engine errors so JavaScript cannot suppress them.

| Resource | Limit |
| --- | --- |
| QuickJS interrupt polls | 100 |
| Host calls | 256 |
| JavaScript heap (compiled code, strings, arrays, objects) | 32 MiB total |
| JavaScript native stack | 256 KiB |
| Script source | 4 MiB |
| Pending native images per cell | 4 images, 6 MiB of base64 between deliveries |
| Retained live and finished cells | 32 across the process |

`operations` counts QuickJS interrupt callbacks, not JavaScript instructions. QuickJS-NG normally polls after approximately 10,000 loop/function checks and within selected native builtins. Cancellation is checked at every poll, while the existing dispatcher cancellation also interrupts blocked tool calls. Heap/stack limits replace the previous engine's individual string/container/depth limits. Output truncation respects UTF-8 boundaries and includes a truncation notice. These limits bound evaluation; tools retain their usual permissions and path rules.

## 4. MCP discovery and use

MCP connections start in the shared pool without blocking the first model cycle on every handshake. Each ready server enters the catalog, then the BM25 index and registry. Pending and failed servers are described honestly in discovery responses. Ending a turn drops its drain listener; the shared pool owns the connection lifecycle.

In the main code-mode turn, MCP tools and the three discovery tools are reached through `call`. Use the discovery response's exact name and schema.

| Tool | Use |
| --- | --- |
| `search_tool` | Load a schema by exact `name`, batch up to ten exact `names`, or find matches by keyword; `name: "list"` lists connected tools |
| `tool_search` | BM25 `query` search returning up to eight JSON entries with `name`, `description` and `params_schema`; optional `limit` is 1–8 |
| `use_tool` | Invoke the returned `name` with schema-matching `arguments` |

```javascript
const schemas = call("search_tool", { name: "list" });
complete(schemas.content[0].Text);
```

Inspect discovery output before constructing `use_tool` arguments. If you started with `name: "list"` or a keyword-only match, load the selected exact name's schema first. `tool_search` already returns full schemas, so it needs no second schema fetch. `search_tool` returns readable text labeled `name`, `server`, `description` and `inputSchema`; `tool_search` returns a JSON array as text. Both are carried in `result.content[0].Text`. Native tools such as `read` use `call("read", ...)` directly.

Catalog insertion reserves native names and `search_tool`, `use_tool`, `tool_search`. A collision becomes `server__tool`; if that candidate is also taken, numeric suffixes start at `__2`. The registry dispatches the chosen catalog key to its matching server tool.

Lookup checks the exact key first, then an unambiguous case-insensitive key. Qualified forms such as `server.tool`, `server:tool`, `server/tool`, `server__tool` and `mcp__server__tool` resolve only within the named server. A qualified miss never falls back to a different server. Ambiguous names require discovery rather than guessing.

## 5. Progress and child identity

Inner tools report lifecycle events through the outer tool's progress stream. Each message starts with `code_mode|` followed by a JSON `CodeModeProgress` envelope. The outer event keeps the outer call ID; the envelope carries the actual inner `ToolCtx.call_id`, formed as `outer_call_id.seq`.

| Envelope kind | Fields and meaning |
| --- | --- |
| `started` | Inner `call_id`, tool `name`, journal `seq`, one-line `target`; emitted before preflight |
| `progress` | Inner `call_id`, tool `name`, original `message` |
| `completed` | Inner `call_id`, tool `name`, terminal `phase`, `is_error` |

Inner phases are `running`, `completed`, `failed` and `cancelled`. Consumers use the inner call ID for child identity and cancellation. Raw messages such as `agent|role|phase|detail` remain intact inside `progress.message`, including pipes and newlines. A `completed` envelope carries the terminal child state.

A bounded broadcast channel forwards live progress without retaining the outer `ToolStream` after a yielded call returns. Each cell also retains inner-call snapshots with IDs and phases, so a later wait or final result can restore the UI state when no listener was present. Older summaries may omit `call_id` and `phase`.

## 6. Cell lifetime and cancellation

Cells transition from `running` to `yielded`, then continue until `completed`, `failed` or `terminated`. They are process-local and scoped to the originating session. Another session cannot collect a guessed cell ID. A process restart does not resume a cell.

Each cell owns a child cancellation token. Terminating a cell stops its inner work without cancelling unrelated cells. Cancelling or dropping an active `run_code` or `wait_code` future terminates its cell. Dispatcher panics settle the cell as failed, and inner-call lifecycle guards publish a terminal state during cancellation or unwinding.

The process keeps at most 32 live and finished cells. On insertion it evicts the oldest finished cell first; if all cells are live, it terminates and evicts the oldest. Finished results remain collectable until eviction or session cleanup.

The journal records ordered host events and argument/result hashes. It provides divergence checking primitives, not a persisted execution checkpoint or an exactly-once replay guarantee. `wait_code` observes the original execution.

## 7. Source map

- [Tool arguments and lifecycle](../../crates/mycode-tools/src/code_mode.rs)
- [Cell table and snapshots](../../crates/mycode-tools/src/code_mode/cell.rs)
- [QuickJS limits and host functions](../../crates/mycode-tools/src/code_mode/engine.rs)
- [Shared dispatch and preflight](../../crates/mycode-tools/src/code_mode/dispatch.rs)
- [Progress envelope](../../crates/mycode-tools/src/code_mode/progress.rs)
- [MCP catalog and collision handling](../../crates/mycode-app/src/mcp_tools.rs)
- [Main turn assembly](../../crates/mycode-app/src/turn.rs)
- [System prompt exposure](../../crates/mycode-agent/src/prompt.rs)
