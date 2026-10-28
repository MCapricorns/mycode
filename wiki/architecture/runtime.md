# Runtime architecture

MYCode is a Rust native agent with a GPUI desktop interface. UI state, application orchestration, model protocols and tools live in separate crates. The architecture graph links these boundaries to the source embedded in this wiki snapshot.

## Crate boundaries

| Crate | Responsibility |
| --- | --- |
| `mycode-desktop` | GPUI windows, chat, settings and input; renders product state and emits actions |
| `mycode-frontend` | UI-independent view model, action reducer, driver, translations and shortcuts |
| `mycode-app` | CoreBridge command/event boundary, turn assembly, MCP, subagents, configuration and updates |
| `mycode-agent` | Model/tool cycle, prompt assembly, session ledger and cancellation |
| `mycode-providers` | OpenAI Chat Completions, OpenAI Responses and Anthropic Messages adapters |
| `mycode-tools` | Registry, filesystem/search/shell tools and Rhai code mode |
| `mycode-core` | Shared messages, events, tool specifications and provider interfaces |
| `mycode-config` | Home layout, authority documents, resource discovery and owned-file transactions |

## A chat turn

The desktop sends a `FrontendAction`. The frontend reducer updates product state, and its driver translates the action into a CoreBridge command. The application core assembles the session, model route, prompt resources, tool registry and turn control.

The agent sends a provider request, consumes the stream, dispatches tools and appends events to the session ledger. Application projection and the frontend stream translate these events back into view-model actions. GPUI renders that state; it does not own the model loop.

The main model sees the platform shell, `run_code` and `wait_code`. Hidden native and bridge tool schemas still reach the system prompt; remote MCP schemas remain discovery-only through `search_tool` or `tool_search`. A Rhai script can call native tools, `ask_user`, allowed subagents and MCP discovery using the same registry. See [Code mode engine](code-mode-engine.md).

## Progressive MCP

The shared MCP pool resolves configuration and starts server connections in the background. The first model cycle can proceed while servers are connecting. Each ready server enters the catalog before search-index and registry publication. The catalog is the discovery and `use_tool` authority; ready tools also have registered dispatch entries.

`search_tool` retrieves exact schemas or keyword matches. `tool_search` ranks connected tools with BM25 and returns up to eight full schemas. `use_tool` calls the selected catalog key. Native and bridge names remain reserved; collisions get a server-qualified key and, when necessary, a numeric suffix. Qualified lookup never dispatches to another server on a miss.

The prompt distinguishes ready, connecting and failed servers. The turn owns a drain listener; the shared pool owns handshakes and reusable connections. Read-only child roles filter available tools before constructing their registry and catalog.

## Progress to the interface

Code mode wraps inner lifecycle and progress as `code_mode|<JSON>`. The outer event keeps its call ID, while each envelope carries the child tool's exact `call_id`. The frontend uses that child identity and terminal phase to render real tool rows and route cancellation.

The `progress` envelope preserves the original message, including subagent messages such as `agent|role|phase|detail`. Result snapshots retain `inner_calls` with IDs and phases so a yielded script can later restore child state. See [Progress and child identity](code-mode-engine.md#progress-and-child-identity).

## Sessions and worktrees

The SQLite session index is the commit authority, with conversation events stored as JSONL. Session open reconciles the committed position with the event log, handles an incomplete tail and reports corruption when the committed data cannot be reconciled.

Write-capable subagents acquire a detached git worktree and return their result with a captured diff. They do not automatically merge or commit it. Cleanup removes only clean leases. Tracked changes, untracked files and ignored files preserve the checkout and its recovery manifest, including after cancellation or recovery on a later start.

## Settings and prompt resources

Settings use strict JSON authority documents with revisions and owned-file publication. A valid magpie bootstrap with an active gateway binding and no envelope metadata is imported into a canonical revision-1 document. That supported first-create format is not reported as a repair and does not create a broken-document backup. Partial or invalid envelopes follow normal validation and repair rules.

Prompt resources load global defaults first, then workspace overrides. Contributions name their source paths and scope; project instructions take precedence over global defaults at the same scope. Discovery checks the MYCode home's `AGENTS.md`, then the workspace's `AGENTS.md`, `MYCODE.md`, `.mycode/agents.md` and `.agents/AGENTS.md`. Files and total prompt contribution sizes are bounded. Skill discovery uses the MYCode home's magpie Library `skills` directory.

## Source map

- [Application turn assembly](../../crates/mycode-app/src/turn.rs)
- [Frontend event projection](../../crates/mycode-frontend/src/stream.rs)
- [Agent model/tool cycle](../../crates/mycode-agent/src/turn.rs)
- [MCP catalog](../../crates/mycode-app/src/mcp_tools.rs)
- [Subagent worktree leases](../../crates/mycode-app/src/subagent/worktree.rs)
- [Settings bootstrap import](../../crates/mycode-config/src/settings.rs)
- [Prompt resource discovery](../../crates/mycode-config/src/resources.rs)
