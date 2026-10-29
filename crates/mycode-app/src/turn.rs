//! One model turn: provider resolution, the tool registry, and per-turn
//! setup. The submodules hold the turn's cancel and steer controls, the
//! ledger pump, `/compact` and the summary cards, and the system prompt.

mod compact;
mod control;
mod prompt;
mod pump;

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use mycode_agent::Message;
use mycode_agent::ToolRegistry;
use mycode_agent::session::{BranchId, HeadStamp, SessionId};
use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_agent::{AppSettings, ProviderSettings};
use mycode_agent::{ReqwestTransport, ResolvedProvider, SseTransport, WireProvider};

use crate::BridgeEvent;
use crate::ledger::{HeadWriter, branch_head, head_spelling, ledger_history, render_error};
use crate::protocol::CHAT_CANCELLED;
use crate::state::{CoreState, model_context_window, model_output_limit};
use crate::tool_hosts::BridgeAskChannel;

pub(crate) use self::compact::manual_compact;
use self::compact::{DeferredSummaries, publish_visible_summary};
use self::control::TurnControl;
use self::prompt::session_system_prompt;
pub(crate) use self::prompt::{
    push_scratch_note, push_workspace_skills, split_scratch_root, workspace_extra_roots,
};

/// One model turn: resolve the provider, stream the reply into the event
/// channel, and commit the assistant message to the session ledger.
///
/// The turn's cancel token and steer inbox are published before the returned
/// future is first polled. Setup (credentials, MCP connect, pre-turn
/// compaction) can take a long time, and a cancel or a second turn for the
/// same session must reach this one during it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chat_turn(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    session: SessionId,
    branch: BranchId,
    provider_id: String,
    model: String,
    reasoning: Option<String>,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let session_id = session.as_str().to_owned();
    let mut control = TurnControl::register(&state, &session_id);
    async move {
        let cwd = state.project_dir(&session_id);
        let (head_session, head_branch) = (session.clone(), branch.clone());
        let outcome = run_chat_turn(
            &state,
            &events,
            &session_id,
            session,
            branch,
            &provider_id,
            &model,
            reasoning.as_deref(),
            cwd,
            &control,
        )
        .await;
        let _ = end_turn(
            &state,
            &events,
            &session_id,
            &head_session,
            &head_branch,
            &mut control,
            outcome,
        )
        .await;
    }
}

/// Reports how a turn ended: its terminal event, then any steer the agent
/// never sent. Both go out after [`TurnControl::retire`], so a session the
/// UI reopens on hearing the end no longer lists this turn as running.
///
/// `outcome` is the terminal the pump returned, or why the turn failed
/// before the pump ran. Returns `outcome` without the event, for tests.
async fn end_turn(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    session_id: &str,
    session: &SessionId,
    branch: &BranchId,
    control: &mut TurnControl,
    outcome: Result<BridgeEvent, String>,
) -> Result<(), String> {
    let (terminal, reported) = match outcome {
        Ok(terminal) => (terminal, Ok(())),
        Err(message) => {
            // A cancel that lands during setup surfaces as setup failing;
            // report it as the quiet cancel the desktop expects.
            let shown = if control.cancel.is_cancelled() {
                CHAT_CANCELLED.to_owned()
            } else {
                message.clone()
            };
            // Setup can already have written: a pre-turn compaction card
            // lands before a cancel is noticed. Report the durable head so
            // the UI's next history page, recall, or `/compact` does not
            // fail the moved-head check against its own summary row.
            let head = branch_head(&state.service, session, branch)
                .await
                .map(|head| head_spelling(&head))
                .unwrap_or_default();
            let terminal = BridgeEvent::ChatFailed {
                session_id: session_id.to_owned(),
                message: shown,
                head,
            };
            (terminal, Err(message))
        }
    };
    // A steer accepted after the agent's last drain was never sent.
    // Hand it back instead of dropping it with the inbox.
    let texts = control.retire();
    let _ = events.send(terminal);
    if !texts.is_empty() {
        let _ = events.send(BridgeEvent::SteersReturned {
            session_id: session_id.to_owned(),
            texts,
        });
    }
    reported
}

/// Reads the magpie binding and the provider row one turn runs on.
///
/// The row's model list is the gateway's published list, so a session pin
/// and the model magpie picked both resolve through the same check.
pub(crate) async fn turn_credentials(
    state: &CoreState,
    provider_id: &str,
) -> Result<(AppSettings, ProviderSettings, String), String> {
    if provider_id != mycode_agent::MAGPIE_PROVIDER_ID {
        return Err(format!(
            "provider '{provider_id}' is not the magpie gateway"
        ));
    }
    let settings = crate::magpie::load_settings(&state.home).await?;
    let provider = crate::magpie::provider_row(&settings.magpie, &state.models())
        .ok_or_else(|| "magpie is not connected: pick a model in magpie first".to_owned())?;
    let key = settings.magpie.api_key.clone();
    Ok((settings, provider, key))
}
#[allow(clippy::too_many_arguments)]
async fn run_chat_turn(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    session_id: &str,
    session: SessionId,
    branch: BranchId,
    provider_id: &str,
    model: &str,
    reasoning: Option<&str>,
    cwd: PathBuf,
    control: &TurnControl,
) -> Result<BridgeEvent, String> {
    let (settings, provider, bearer) = control
        .unless_cancelled(turn_credentials(state, provider_id))
        .await??;
    control.check()?;
    let resolved = ResolvedProvider::resolve(
        &provider,
        model,
        &bearer,
        &crate::user_agent::default_user_agent(),
    )
    .map_err(|error| format!("provider setup failed: {error}"))?;
    let transport: Arc<dyn SseTransport> = Arc::new(
        ReqwestTransport::new().map_err(|error| format!("HTTP transport unavailable: {error}"))?,
    );
    run_chat_turn_on(
        state, events, session_id, session, branch, model, reasoning, cwd, resolved, settings,
        provider, transport, control,
    )
    .await
}

/// Runs one prepared turn on `transport` and returns its terminal event
/// unsent; [`end_turn`] reports it. Tests pass a scripted transport;
/// production uses [`ReqwestTransport`].
#[allow(clippy::too_many_arguments)]
async fn run_chat_turn_on(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    session_id: &str,
    session: SessionId,
    branch: BranchId,
    model: &str,
    reasoning: Option<&str>,
    cwd: PathBuf,
    resolved: ResolvedProvider,
    settings: AppSettings,
    provider: ProviderSettings,
    transport: Arc<dyn SseTransport>,
    control: &TurnControl,
) -> Result<BridgeEvent, String> {
    let home = &state.home;
    let wire = WireProvider::new(resolved.clone(), transport);

    // The tool working directory is the bound project (created on demand).
    let cwd_for_mkdir = cwd.clone();
    tokio::task::spawn_blocking(move || std::fs::create_dir_all(&cwd_for_mkdir))
        .await
        .map_err(|error| format!("workspace dir task: {error}"))?
        .map_err(|error| format!("workspace dir: {error}"))?;
    // Replay history is rebuilt from the ledger's typed events: display
    // entries flatten tool traffic into text, which breaks the
    // tool_use/tool_result pairing providers validate.
    // Follow the durable tip when the desktop snapshot is behind. A stale
    // expected head otherwise surfaces as "the session moved on". A missing
    // session or branch fails here instead of waiting until the first write.
    let expected_head = control
        .unless_cancelled(branch_head(&state.service, &session, &branch))
        .await?
        .map_err(render_error)?;
    let history = control
        .unless_cancelled(ledger_history(
            &state.service,
            &session,
            &branch,
            &expected_head,
        ))
        .await?
        .map_err(render_error)?;
    // The prompt is the latest user message. A trailing assistant or tool
    // result (an interrupted turn, or a write that landed after the user
    // message) would fail the turn and leave the session unable to send, so
    // those suffix messages stay in the ledger and are omitted from this
    // request. Checked before MCP connects.
    let (history, prompt) = split_latest_user(history)?;
    let usage_model = model.to_owned();
    let head_stamp_text = head_spelling(&expected_head);
    let writer = HeadWriter::new(
        state.service.clone(),
        session.clone(),
        branch.clone(),
        expected_head,
    );
    // MCP starts here without waiting for any handshake: `resolve` reads
    // secrets and merges the server sources (blocking pool, a few ms),
    // `spawn_slots` starts one background handshake per server, and each
    // server's tools are registered as it becomes ready. A server that fails
    // is skipped and retried after its own per-server cooldown.
    let mcp_connect = control
        .unless_cancelled(crate::mcp_tools::connect_mcp_tools(
            home,
            &settings,
            &state.mcp_pool,
            Some(&cwd),
        ))
        .await?;
    // Keep role discovery and workspace extra-root reads off the single
    // core runtime thread.
    let (role_catalog, extra_roots) = {
        let home = home.clone();
        let cwd = cwd.clone();
        tokio::task::spawn_blocking(move || {
            (
                mycode_agent::discover_roles(&home, Some(&cwd)),
                workspace_extra_roots(&home, &cwd),
            )
        })
        .await
        .map_err(|error| format!("workspace setup task: {error}"))?
    };
    let registry: Arc<ToolRegistry> = Arc::new({
        let registry = ToolRegistry::new();
        mycode_agent::register_builtins(&registry);
        // ask_user rides the same registry; its channel forwards questions
        // to the UI over the event channel and waits on the shared router.
        let channel: Arc<dyn mycode_agent::builtin::AskChannel> = Arc::new(BridgeAskChannel {
            session_id: session_id.to_owned(),
            events: events.clone(),
        });
        registry.register(Arc::new(mycode_agent::builtin::AskTool::new(channel)));
        // `agent` delegates scoped work to a catalog role; slots, isolation,
        // and per-role model routes live in the host.
        {
            let runtime = state
                .agent_runtimes
                .lock()
                .entry(session_id.to_owned())
                .or_insert_with(|| {
                    Arc::new(mycode_agent::builtin::AgentRuntime::new(
                        home.root().join("agent-results").join(session_id),
                    ))
                })
                .clone();
            let host: Arc<dyn mycode_agent::builtin::AgentHost> = Arc::new(
                crate::subagent::BridgeAgentHost::new(
                    resolved.clone(),
                    home.clone(),
                    cwd.clone(),
                    &settings,
                    session_id.to_owned(),
                    state.subagent_cancels.clone(),
                    state.mcp_pool.clone(),
                    Arc::clone(&state.code_cells),
                )
                .with_slots(runtime.slots(mycode_agent::SUBAGENT_CONCURRENCY as usize))
                .with_context(
                    Arc::clone(&state.catalog),
                    provider.clone(),
                    model.to_owned(),
                ),
            );
            if crate::subagent::any_role_enabled(&role_catalog, &settings.subagents) {
                registry.register(Arc::new(
                    mycode_agent::builtin::AgentTool::new(host.clone())
                        .with_runtime(runtime.clone()),
                ));
            }
            registry.register(Arc::new(mycode_agent::builtin::AgentStatusTool::new(
                runtime.clone(),
            )));
            registry.register(Arc::new(mycode_agent::builtin::AgentStopTool::new(
                runtime.clone(),
            )));
            registry.register(Arc::new(mycode_agent::builtin::AgentMessageTool::new(
                host.clone(),
                runtime.clone(),
            )));
            registry.register(Arc::new(mycode_agent::builtin::AgentWorkflowTool::new(
                host, runtime,
            )));
        }
        registry
    });
    // Code mode: `run_code` holds the registry so a script's `call(name, …)`
    // is a runtime lookup. It registers before the MCP meta tools, so the
    // native-name snapshot below already contains `run_code`/`wait_code`.
    // The cell table is process-wide, so both tools carry this session's id
    // and can only start or collect that session's cells.
    registry.register(Arc::new(mycode_agent::RunCodeTool::new(
        Arc::clone(&registry),
        Arc::clone(&state.code_cells),
        session_id.to_owned(),
    )));
    registry.register(Arc::new(mycode_agent::WaitCodeTool::new(
        Arc::clone(&state.code_cells),
        session_id.to_owned(),
    )));
    // Code-mode-only: the model's top-level list is the shell plus the two
    // code-mode tools; every other registered tool (read/write/edit/grep/
    // find/ask_user/agent and the MCP bridge) is reachable through `call`
    // inside a script. The names list already holds the platform shell.
    {
        let names = registry.names();
        let mut exposed = vec!["run_code", "wait_code"];
        exposed.extend(
            names
                .iter()
                .map(String::as_str)
                .filter(|name| mycode_agent::ShellKind::is_tool_name(name)),
        );
        registry.expose_only(&exposed);
    }
    // Progressive MCP: the meta tools exist from cycle 1, ready servers are
    // written catalog-first then registry, and the rest arrive through the
    // background drain. The drain task is aborted at the end of the turn —
    // that stops this turn's listener only; the pool keeps its handshakes.
    let mut _mcp_drain: Option<AbortTask> = None;
    let mcp_catalog = if mcp_connect.is_empty() {
        None
    } else {
        // Snapshot before the meta tools register, so the name checks below
        // never see `search_tool` / `use_tool` / `tool_search` themselves.
        let native: BTreeSet<String> = registry
            .names()
            .into_iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        let catalog = crate::mcp_tools::McpCatalog::new(
            mcp_connect.warning.clone(),
            native,
            mcp_connect.connecting.clone(),
        );
        let index = crate::tool_search::ToolSearchIndex::new();
        crate::mcp_tools::register_meta_tools(&registry, &catalog, Arc::clone(&index));
        // Failures known at turn start are visible to a `use_tool` miss, and
        // are seeded so the drain does not report them twice.
        let mut seen_failed = HashSet::new();
        for (id, reason) in &mcp_connect.failed {
            catalog.mark_failed(
                Arc::new(crate::mcp_tools::McpServerInfo::new(id, None)),
                reason,
            );
            seen_failed.insert(id.clone());
        }
        let mut seen = HashSet::new();
        for (server, tools) in mcp_connect.ready {
            seen.insert(server.id().to_owned());
            crate::mcp_tools::register_ready_server(&registry, &catalog, &index, server, tools);
        }
        let drain_registry = Arc::clone(&registry);
        let drain_catalog = Arc::clone(&catalog);
        let failed_catalog = Arc::clone(&catalog);
        _mcp_drain = Some(AbortTask(tokio::spawn(crate::mcp_tools::drain_mcp_ready(
            Arc::clone(&state.mcp_pool),
            seen,
            seen_failed,
            Some(control.cancel.clone()),
            move |server, tools| {
                crate::mcp_tools::register_ready_server(
                    &drain_registry,
                    &drain_catalog,
                    &index,
                    server,
                    tools,
                );
            },
            // A server that fails mid-turn stops reading as "connecting";
            // the next `use_tool` miss names the real failure and retry.
            move |server, reason| failed_catalog.mark_failed(server, reason),
        ))));
        Some(catalog)
    };

    // Owned for the compaction hook and the cancel registration.
    let session_id = session_id.to_owned();

    let context_window = model_context_window(state, &provider, model);
    let read_roots = mycode_agent::skill_read_roots(Some(home.root()));
    let directive = crate::subagent::delegation_directive(&role_catalog, &settings.subagents);
    let mcp_note = mcp_catalog
        .as_ref()
        .map(|catalog| catalog.prompt_note())
        .filter(|note| !note.trim().is_empty());
    let system_prompt = {
        let home = home.clone();
        let cwd = cwd.clone();
        let registry = Arc::clone(&registry);
        let extra_roots = extra_roots.clone();
        let skill_settings = settings.skills.clone();
        tokio::task::spawn_blocking(move || {
            session_system_prompt(
                &home,
                &cwd,
                &registry,
                mcp_note.as_deref(),
                &extra_roots,
                &directive,
                &skill_settings,
            )
        })
        .await
        .map_err(|error| format!("system prompt task: {error}"))?
    };
    let overhead_tokens = crate::compaction::estimate_prefix_tokens(
        std::slice::from_ref(&system_prompt),
        &registry.specs(),
    );
    let calibration = crate::compaction::Calibration::new(state.token_scale(&session_id));
    // Codex-style checkpoint: 90% of the usable window, a tail sized to the
    // window. This pre-turn pass publishes its transcript card immediately. The
    // before-request hook later in this function does not append a ledger
    // row; its card is published after the turn's tool results and final
    // reply commit, so a mid-turn summary cannot split a tool call from its
    // result.
    let compact_scope = crate::compaction::CompactScope {
        wire: &wire,
        model,
        checkpoint: Some(crate::compaction::CheckpointSlot {
            home,
            session_id: &session_id,
            branch_id: branch.as_str(),
            head: &head_stamp_text,
        }),
        context_window,
        overhead_tokens,
        // Nothing of this turn is measured yet; the scale is the one the
        // session's last turn measured.
        measure: crate::compaction::Measure {
            scale: calibration.scale(),
            tokens: None,
        },
        // No agent bus yet; nothing else of this turn is on the wire.
        on_summarize: Some(&|active| {
            let _ = events.send(BridgeEvent::ContextCompacting {
                session_id: session_id.clone(),
                active,
            });
        }),
        cancel: &control.cancel,
    };
    let compacted = crate::compaction::compact_history(
        &compact_scope,
        history,
        crate::compaction::CompactMode::Auto,
    )
    .await;
    // A written checkpoint is already on disk, so its card is published
    // before the post-compaction cancel check.
    if let Some(summary) = compacted.summary.as_deref() {
        let _ = publish_visible_summary(&writer, events, &session_id, summary, true).await;
    }
    control.check()?;
    let history = compacted.messages;

    // Grok Build call pattern: a short index, then the model loads the
    // body or schema itself. Full skill text and MCP schemas stay off this
    // prompt. Skill order and extra-root order are sorted so two turns share
    // a byte-identical prefix. The prompt was built above so compaction can
    // count it.

    let turn_started = std::time::Instant::now();
    let (agent_tx, agent_rx) = tokio::sync::broadcast::channel(256);
    let deferred = DeferredSummaries::default();
    // Built in a block so no clone of the agent bus outlives the hooks: a
    // cancel inside the first request's hook ends the prompt without
    // TurnEnded, and the pump then stops only when the bus closes.
    let hooks = {
        let compactor = crate::compaction::RequestCompactor {
            wire: wire.clone(),
            model: model.to_owned(),
            checkpoint: Some(crate::compaction::CheckpointKey {
                home: home.clone(),
                session_id: session_id.clone(),
                branch_id: branch.as_str().to_owned(),
                head: head_stamp_text.clone(),
            }),
            context_window,
            calibration: calibration.clone(),
            on_summarize: Some({
                let bus = agent_tx.clone();
                Arc::new(move |active| {
                    let _ = bus.send(mycode_agent::events::AgentEvent::Compacting(active));
                })
            }),
            cancel: control.cancel.clone(),
        };
        let deferred = deferred.clone();
        let bus = agent_tx.clone();
        // Stash only. A ledger append here sits between this turn's tool
        // calls and their results. Bus order: the previous response is
        // already queued, and this request's deltas come after. The pump
        // turns CacheDisrupted into PromptCacheWatch, so the warning
        // attaches to the prompt that actually lost the prefix.
        let compact_request = move |mode: crate::compaction::CompactMode| {
            let compactor = compactor.clone();
            let deferred = deferred.clone();
            let bus = bus.clone();
            move |mut request: mycode_agent::Request| {
                let compactor = compactor.clone();
                let deferred = deferred.clone();
                let bus = bus.clone();
                async move {
                    let summary = compactor.compact(&mut request, mode).await;
                    let summarized = summary.is_some();
                    if deferred.stash(summary).await {
                        let _ = bus.send(mycode_agent::events::AgentEvent::CacheDisrupted);
                    }
                    (request, summarized)
                }
            }
        };
        let before_request = compact_request(crate::compaction::CompactMode::Auto);
        let on_overflow = compact_request(crate::compaction::CompactMode::Overflow);
        HookRunner::default()
            .with_before_request(move |request| {
                let compacted = before_request(request);
                async move { compacted.await.0 }
            })
            .with_context_overflow(move |request| {
                let compacted = on_overflow(request);
                async move {
                    let (request, summarized) = compacted.await;
                    summarized.then_some(request)
                }
            })
    };
    let mut config = AgentConfig::new()
        .with_system_prompt(system_prompt)
        .with_prompt_cache_key(Some(session_id.clone()))
        .with_max_output_tokens(model_output_limit(state, &provider, model));
    if let Some(token) = reasoning {
        if let Some(level) = mycode_agent::ReasoningLevel::parse(token) {
            config = config.with_reasoning(level);
        } else if mycode_agent::is_valid_effort_token(token) {
            config = config.with_reasoning_token(Some(token.to_owned()));
        }
    }
    let mut agent = Agent::new(config);

    // The ledger pump owns the branch head for the rest of the turn. The
    // clone shares that head, for the summary filed after the pump drains.
    let final_head = writer.clone();
    let mut pump = AbortTask(tokio::spawn(
        pump::LedgerPump {
            events: events.clone(),
            writer,
            session_id: Arc::from(session_id.as_str()),
            deferred,
            cancel: control.cancel.clone(),
            usage_model,
            started: turn_started,
        }
        .run(agent_rx),
    ));

    agent.seed_history(history);
    let env = mycode_agent::TurnEnv::new(&wire, &registry, &hooks)
        .with_cancel(control.cancel.clone())
        .with_events(agent_tx)
        .with_cwd(cwd)
        .with_extra_roots(extra_roots)
        .with_read_roots(read_roots)
        .with_steer(Some(Arc::clone(&control.steer)));
    let prompt_message = Message::User(prompt);
    // Observe a failed pump while the agent is running. Awaiting the
    // prompt first would leave tools running after a pump panic.
    let (outcome, terminal) = {
        let mut prompt = Box::pin(agent.prompt(prompt_message, &env));
        tokio::select! {
            terminal = &mut pump.0 => {
                if terminal.is_err() {
                    control.cancel.cancel();
                }
                (prompt.await, Some(terminal))
            }
            outcome = &mut prompt => (outcome, None),
        }
    };
    state.remember_token_scale(&session_id, calibration.scale());
    // Close the agent bus (the env and the hook each hold a sender) so a
    // turn that never started reaches the pump as a closed stream instead of
    // leaving it parked forever.
    drop(env);
    drop(hooks);
    // Drain the pump before returning. An agent error already became the
    // pump's ChatFailed; returning Err here would report a second one.
    let terminal = match terminal {
        Some(terminal) => terminal,
        None => (&mut pump.0).await,
    };
    match terminal {
        Ok(terminal) => {
            // The ledger is complete only once the pump drained, and the
            // summary is filed before the turn reports its end: the next
            // turn starts from what this one stored.
            persist_hook_summary(
                state,
                &session,
                &branch,
                &final_head.stamp().await,
                &session_id,
                model,
                agent.state().messages(),
            )
            .await;
            Ok(terminal)
        }
        Err(error) => {
            outcome.map_err(|error| format!("turn failed: {error}"))?;
            Err(format!("the turn pump stopped: {error}"))
        }
    }
}

/// Stores a summary the request hook produced mid-turn, checked against
/// the ledger at the pump's final `head`; see
/// [`crate::compaction::persist_turn_summary`]. Best effort, like every
/// compaction step: a failure keeps the older checkpoint.
async fn persist_hook_summary(
    state: &CoreState,
    session: &SessionId,
    branch: &BranchId,
    head: &HeadStamp,
    session_id: &str,
    model: &str,
    memory: &[Arc<Message>],
) {
    // Every turn of a compacted session opens with the stored summary;
    // only a newer one is worth replaying the ledger for.
    let unstored = {
        let home = state.home.clone();
        let (session_id, branch_id) = (session_id.to_owned(), branch.as_str().to_owned());
        let first = memory.first().cloned();
        tokio::task::spawn_blocking(move || {
            crate::compaction::opens_with_unstored_summary(
                &home,
                &session_id,
                &branch_id,
                first.as_deref(),
            )
        })
        .await
        .unwrap_or(false)
    };
    if !unstored {
        return;
    }
    let Ok(ledger) = ledger_history(&state.service, session, branch, head).await else {
        return;
    };
    let home = state.home.clone();
    let memory = memory.to_vec();
    let (session_id, branch_id, head, model) = (
        session_id.to_owned(),
        branch.as_str().to_owned(),
        head_spelling(head),
        model.to_owned(),
    );
    let _ = tokio::task::spawn_blocking(move || {
        let target = crate::compaction::TurnCheckpoint {
            session_id: &session_id,
            branch_id: &branch_id,
            head: &head,
            model: &model,
        };
        crate::compaction::persist_turn_summary(&home, &target, &ledger, &memory)
    })
    .await;
}

/// Aborts a background task when its turn driver goes away.
///
/// The MCP drain task only listens to the pool; aborting it leaves the
/// handshakes (owned by the pool) running for the next turn. The ledger
/// pump must likewise stop if the turn task is dropped.
struct AbortTask<T = ()>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Splits the latest non-empty user message off as the prompt.
///
/// Messages after it are dropped from this request only. An error here would
/// stick: the next send would load the same tail and fail again.
fn split_latest_user(
    history: Vec<Arc<Message>>,
) -> Result<(Vec<Arc<Message>>, mycode_agent::UserMessage), String> {
    let Some(index) = history.iter().rposition(
        |message| matches!(message.as_ref(), Message::User(user) if user_has_content(user)),
    ) else {
        return Err("the turn has no user message to answer".to_owned());
    };
    let mut prior = history;
    // The split-off tail owns the prompt, so an unshared message moves out
    // without a copy.
    let prompt = prior.split_off(index).swap_remove(0);
    let Message::User(prompt) = Arc::unwrap_or_clone(prompt) else {
        return Err("the turn has no user message to answer".to_owned());
    };
    Ok((prior, prompt))
}

/// Whether this message asks for an answer. A picture counts on its own: an
/// attachments-only prompt has an empty text block but something to look at,
/// and treating it as empty made the model answer the previous prompt again.
fn user_has_content(user: &mycode_agent::UserMessage) -> bool {
    user.content.iter().any(|block| match block {
        mycode_agent::ContentBlock::Text(text) => !text.text.trim().is_empty(),
        mycode_agent::ContentBlock::Image(_) => true,
        _ => false,
    })
}

#[cfg(test)]
mod selection_tests {
    use mycode_agent::{BinaryData, ContentBlock, TextBlock, UserMessage};

    use super::user_has_content;

    #[test]
    fn an_attachments_only_message_is_a_prompt() {
        let images_only = UserMessage {
            content: vec![
                ContentBlock::Text(TextBlock::new("")),
                ContentBlock::Image(BinaryData {
                    data: "aGVsbG8=".to_owned(),
                    mime_type: "image/png".to_owned(),
                }),
            ],
        };
        assert!(user_has_content(&images_only));
        assert!(!user_has_content(&UserMessage::text("")));
        assert!(!user_has_content(&UserMessage::text("  \n ")));
        assert!(user_has_content(&UserMessage::text("hello")));
    }
}
