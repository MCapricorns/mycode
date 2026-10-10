//! One model turn: provider resolution, the tool registry, the ledger pump,
//! and per-turn cancellation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use mycode_agent::session::{BranchId, EventKind, HeadStamp, SessionId};
use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_config::{
    AppSettings, HomeLayout, ProviderSettings, read_app_settings, read_provider_secrets,
};
use mycode_core::Message;
use mycode_providers::{ReqwestTransport, ResolvedProvider, SseTransport, WireProvider};
use mycode_tools::{ToolDyn, ToolRegistry};
use tokio_util::sync::CancellationToken;

use crate::BridgeEvent;
use crate::ledger::{HeadWriter, head_spelling, ledger_history, render_error};
use crate::oauth::resolve_request_auth;
use crate::projection::{project_assistant_message, project_tool_result_message, project_usage};
use crate::protocol::CHAT_CANCELLED;
use crate::state::{CoreState, model_context_window, model_output_limit};
use crate::tool_hosts::{BridgeAskChannel, BridgeWebHost, register_ask};

/// One model turn: resolve the provider, stream the reply into the event
/// channel, and commit the assistant message to the session ledger.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn chat_turn(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: String,
    model: String,
    reasoning: Option<String>,
) {
    let session_id = session.as_str().to_owned();
    let cwd = state.project_dir(&session_id);
    if let Err(message) = run_chat_turn(
        &state,
        &events,
        &session_id,
        session,
        branch,
        expected_head,
        &provider_id,
        &model,
        reasoning.as_deref(),
        cwd,
    )
    .await
    {
        let _ = events.try_send(BridgeEvent::ChatFailed {
            session_id,
            message,
            head: String::new(),
        });
    }
}

/// Runs `/compact`: summarize the ledger now and store a checkpoint the next
/// turn will send instead of the full history.
pub(crate) async fn manual_compact(
    state: Arc<CoreState>,
    events: crate::BridgeEventTx,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: String,
    model: String,
) {
    let session_id = session.as_str().to_owned();
    let outcome = compact_session_now(
        &state,
        &session,
        &branch,
        &expected_head,
        &provider_id,
        &model,
    )
    .await;
    let (ok, message) = match outcome {
        Ok((message, Some(summary))) => {
            match show_compaction_summary(&state, &events, &session, &branch, &session_id, &summary)
                .await
            {
                Ok(()) => (true, message),
                Err(error) => (false, error),
            }
        }
        Ok((message, None)) if message == "empty" || message == "covered" => (true, message),
        Ok((_, None)) => (false, "compaction finished without a summary".to_owned()),
        Err(message) => (false, message),
    };
    let _ = events.try_send(BridgeEvent::CompactFinished {
        session_id,
        message,
        ok,
    });
}

/// Writes the summary into the ledger as a transcript row and tells the UI.
/// The model history skips this row; the checkpoint still builds the request.
async fn show_compaction_summary(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    session: &SessionId,
    branch: &BranchId,
    session_id: &str,
    summary: &str,
) -> Result<(), String> {
    let opened = state
        .service
        .open(session)
        .await
        .map_err(|_| "could not open the session".to_owned())?;
    let head = opened
        .heads
        .iter()
        .find(|head| &head.branch_id == branch)
        .map(|head| head.head.clone())
        .ok_or_else(|| "could not open the session".to_owned())?;
    let writer = HeadWriter::new(state.service.clone(), session.clone(), branch.clone(), head);
    publish_visible_summary(&writer, events, session_id, summary).await
}

/// Summaries produced inside `before_request`. The hook must not append a
/// ledger row: that write lands between the turn's tool calls and their
/// results and breaks the head the turn writer is committing. The pump
/// publishes the card after the assistant message (and usage) commit.
#[derive(Clone, Default)]
struct DeferredSummaries {
    pending: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl DeferredSummaries {
    async fn stash(&self, summary: Option<String>) {
        if let Some(summary) = summary {
            self.pending.lock().await.push(summary);
        }
    }

    async fn publish(&self, writer: &HeadWriter, events: &crate::BridgeEventTx, session_id: &str) {
        let summaries = {
            let mut pending = self.pending.lock().await;
            std::mem::take(&mut *pending)
        };
        for summary in summaries {
            let _ = publish_visible_summary(writer, events, session_id, &summary).await;
        }
    }
}

async fn publish_visible_summary(
    writer: &HeadWriter,
    events: &crate::BridgeEventTx,
    session_id: &str,
    summary: &str,
) -> Result<(), String> {
    let payload = crate::compaction::display_summary_text(summary);
    let event_id = writer
        .write(EventKind::Message, payload.as_bytes())
        .await
        .map_err(|_| "could not write the compaction summary".to_owned())?;
    let entry = crate::protocol::ConversationEntry {
        event_id,
        kind: crate::protocol::EntryKind::UserMessage,
        text: payload.into(),
        call_id: None,
        thinking: String::new(),
    };
    let _ = events.try_send(BridgeEvent::SummaryShown {
        session_id: session_id.to_owned(),
        entry,
    });
    Ok(())
}

async fn compact_session_now(
    state: &CoreState,
    session: &SessionId,
    branch: &BranchId,
    _expected_head: &HeadStamp,
    provider_id: &str,
    model: &str,
) -> Result<(String, Option<String>), String> {
    let opened = state
        .service
        .open(session)
        .await
        .map_err(|_| "could not open the session".to_owned())?;
    let expected_head = opened
        .heads
        .iter()
        .find(|head| &head.branch_id == branch)
        .map(|head| head.head.clone())
        .ok_or_else(|| "could not open the session".to_owned())?;
    let home = &state.home;
    let (settings, provider, stored_key) = turn_credentials(home, provider_id).await?;
    let (bearer, extra_headers) =
        resolve_request_auth(&state.home, &state.copilot, &provider, &stored_key).await?;
    let mut resolved =
        ResolvedProvider::resolve(&provider, model, &bearer, &settings.effective_user_agent())
            .map_err(|error| format!("provider setup failed: {error:?}"))?;
    resolved.headers.extend(extra_headers);
    let transport: Arc<dyn SseTransport> =
        Arc::new(ReqwestTransport::new().map_err(|_| "HTTP transport unavailable".to_owned())?);
    let wire = WireProvider::new(resolved, transport);
    let history = ledger_history(&state.service, session, branch, &expected_head)
        .await
        .map_err(render_error)?;
    if history.len() < 2 {
        return Ok(("empty".to_owned(), None));
    }
    let head_stamp_text = head_spelling(&expected_head);
    let context_window = model_context_window(state, &provider, model);
    let scope = crate::compaction::CompactScope {
        home,
        wire: &wire,
        model,
        session_id: session.as_str(),
        branch_id: branch.as_str(),
        head: &head_stamp_text,
        context_window,
    };
    let compacted = crate::compaction::compact_history(&scope, history, true).await;
    match compacted.status {
        crate::compaction::CompactStatus::Wrote => Ok(("compacted".to_owned(), compacted.summary)),
        crate::compaction::CompactStatus::Covered => Ok(("covered".to_owned(), None)),
        crate::compaction::CompactStatus::Unchanged => Ok(("empty".to_owned(), None)),
        crate::compaction::CompactStatus::Failed(message) => Err(message),
    }
}

/// Loads the provider row and its stored key for one turn. Saves dispatched
/// alongside the turn run as concurrent tasks, so a just-added provider may
/// not have reached the disk yet — the read settles with a short retry.
async fn turn_credentials(
    home: &HomeLayout,
    provider_id: &str,
) -> Result<(AppSettings, ProviderSettings, String), String> {
    let mut last_error = "provider not found or disabled in settings".to_owned();
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let settings = match read_app_settings(home) {
            Ok(settings) => settings,
            Err(error) => {
                last_error = crate::settings_io::render_config_error(&error);
                continue;
            }
        };
        let Some(index) = settings
            .providers
            .iter()
            .position(|provider| provider.id == provider_id && provider.enabled)
        else {
            last_error = "provider not found or disabled in settings".to_owned();
            continue;
        };
        let secrets = match read_provider_secrets(home) {
            Ok(secrets) => secrets,
            Err(error) => {
                last_error = crate::settings_io::render_config_error(&error);
                continue;
            }
        };
        let Some(key) = secrets.key(provider_id) else {
            last_error = "provider API key is not set".to_owned();
            continue;
        };
        let provider = settings.providers[index].clone();
        return Ok((settings, provider, key.to_owned()));
    }
    Err(last_error)
}

/// Workspace folders other than the session cwd. Missing UI state is empty.
pub(crate) fn workspace_extra_roots(
    home: &mycode_config::HomeLayout,
    cwd: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let Ok(state) = mycode_config::read_ui_state(home) else {
        return Vec::new();
    };
    let cwd_text = cwd.display().to_string();
    let mut roots: Vec<std::path::PathBuf> = state
        .workspace_roots
        .into_iter()
        .filter(|root| !same_dir(root, &cwd_text))
        .map(std::path::PathBuf::from)
        .collect();
    roots.sort();
    roots.truncate(mycode_config::MAX_WORKSPACE_ROOTS);
    roots
}

/// System prompt for one session. Skill order, the skill cap, and extra-root
/// order are stable across calls that see the same files.
pub(crate) fn session_system_prompt(
    home: &HomeLayout,
    cwd: &std::path::Path,
    registry: &ToolRegistry,
    mcp_note: Option<&str>,
    extra_roots: &[std::path::PathBuf],
    directive: &str,
    user_home: Option<&std::path::Path>,
) -> String {
    let resources = mycode_config::discover_resources(home, cwd);
    let mut system_prompt = String::from(
        "You are MYCode, a coding agent. Complete the user's request with the tools you have.",
    );
    for part in mycode_config::render_resource_prompt(&resources) {
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&part);
    }
    push_skill_catalog(
        &mut system_prompt,
        mycode_config::discover_skills(cwd, user_home),
    );
    if let Some(note) = mcp_note {
        system_prompt.push_str("\n\n<mcp>\n");
        system_prompt.push_str(note);
        system_prompt.push_str(
            "Built-in tools and direct MCP tools are called by name. For any other MCP tool, \
call `search_tool` with name \"list\", then with the exact name, then `use_tool`. Do not \
guess parameters. When the user also wants a subagent, emit `search_tool` in the same \
response as `agent`.\n</mcp>",
        );
    }
    system_prompt.push_str(
        "\n\nFor current facts, call `web_search`, then `fetch_content` on the URLs you will cite. Snippets are not evidence.",
    );
    let mut roots: Vec<&std::path::Path> = extra_roots.iter().map(PathBuf::as_path).collect();
    roots.sort();
    if !roots.is_empty() {
        system_prompt.push_str("\n\nWorkspace folders besides the session cwd:\n");
        for root in &roots {
            system_prompt.push_str(&format!("- {}\n", root.display()));
        }
        system_prompt.push_str(
            "Relative paths stay in the session cwd. For the other folders, pass an \
absolute path to `read`, `write`, `edit`, `find`, and `grep`, or an absolute \
path inside a `shell` script (`mode` `script`). `shell` starts in the session \
cwd for both script and program mode.",
        );
    }
    system_prompt.push_str("\n\n");
    system_prompt.push_str(&mycode_agent::build_system_prompt(registry));
    if !directive.is_empty() {
        system_prompt.push_str(directive);
    }
    system_prompt
}

/// Appends the skill catalog, keeping at most [`mycode_config::MAX_SKILLS`].
pub(crate) fn push_skill_catalog(prompt: &mut String, mut skills: Vec<mycode_config::SkillFile>) {
    let hidden = skills.len().saturating_sub(mycode_config::MAX_SKILLS);
    skills.truncate(mycode_config::MAX_SKILLS);
    let Some(mut catalog) = mycode_config::render_skill_catalog(&skills) else {
        return;
    };
    if hidden > 0
        && let Some(close) = catalog.rfind("\n</skills>")
    {
        catalog.insert_str(
            close,
            &format!("\n- and {hidden} more; read the matching SKILL.md by path"),
        );
    }
    prompt.push_str("\n\n");
    prompt.push_str(&catalog);
}

fn same_dir(left: &str, right: &str) -> bool {
    let left = left.trim().trim_end_matches(['/', '\\']);
    let right = right.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_chat_turn(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    session_id: &str,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: &str,
    model: &str,
    reasoning: Option<&str>,
    cwd: PathBuf,
) -> Result<(), String> {
    let home = &state.home;
    // Settings and keys are written by concurrently dispatched save
    // commands; a provider added moments ago may not have landed on disk
    // yet, so the lookup retries briefly before failing the turn.
    let (settings, provider, stored_key) = turn_credentials(home, provider_id).await?;
    // Copilot stores its long-lived OAuth token where other providers keep
    // an API key; each turn exchanges it for a short-lived bearer.
    let (bearer, extra_headers) =
        resolve_request_auth(&state.home, &state.copilot, &provider, &stored_key).await?;
    let mut resolved =
        ResolvedProvider::resolve(&provider, model, &bearer, &settings.effective_user_agent())
            .map_err(|error| format!("provider setup failed: {error:?}"))?;
    resolved.headers.extend(extra_headers);
    let transport: Arc<dyn SseTransport> =
        Arc::new(ReqwestTransport::new().map_err(|_| "HTTP transport unavailable".to_owned())?);
    run_chat_turn_on(
        state,
        events,
        session_id,
        session,
        branch,
        expected_head,
        provider_id,
        model,
        reasoning,
        cwd,
        resolved,
        settings,
        provider,
        transport,
    )
    .await
}

/// Runs one prepared turn on `transport`. Tests pass a scripted transport;
/// production uses [`ReqwestTransport`].
#[allow(clippy::too_many_arguments)]
async fn run_chat_turn_on(
    state: &CoreState,
    events: &crate::BridgeEventTx,
    session_id: &str,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: &str,
    model: &str,
    reasoning: Option<&str>,
    cwd: PathBuf,
    resolved: ResolvedProvider,
    settings: AppSettings,
    provider: ProviderSettings,
    transport: Arc<dyn SseTransport>,
) -> Result<(), String> {
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
    // expected head otherwise surfaces as "the session moved on".
    let expected_head = match state.service.open(&session).await {
        Ok(opened) => opened
            .heads
            .iter()
            .find(|head| head.branch_id == branch)
            .map(|head| head.head.clone())
            .unwrap_or(expected_head),
        Err(_) => expected_head,
    };
    let history = ledger_history(&state.service, &session, &branch, &expected_head)
        .await
        .map_err(render_error)?;
    let usage_enabled = settings.usage.enabled;
    let usage_provider = provider_id.to_owned();
    let usage_model = model.to_owned();
    let head_stamp_text = head_spelling(&expected_head);
    let writer = HeadWriter::new(
        state.service.clone(),
        session.clone(),
        branch.clone(),
        expected_head,
    );
    // MCP servers connect here (spawn + handshake + tools/list): awaited on
    // the spawned turn task, so command processing never blocks. A server
    // that fails to connect is skipped, never a failed turn.
    let (mcp_tools, mcp_warning) =
        crate::mcp_tools::connect_mcp_tools(home, &settings, &state.mcp_pool, Some(&cwd)).await;
    let role_catalog = mycode_config::discover_roles(home, Some(&cwd));
    let registry = Arc::new({
        let registry = ToolRegistry::new();
        mycode_tools::register_builtins(&registry);
        // ask_user rides the same registry; its channel forwards questions
        // to the UI over the event channel and waits on the shared router.
        let ask_events = events.clone();
        let ask_session = session_id.to_owned();
        let answer_rx = register_ask(&ask_session);
        let channel: Arc<dyn mycode_tools::builtin::AskChannel> = Arc::new(BridgeAskChannel {
            session_id: ask_session,
            events: ask_events,
            answer: tokio::sync::Mutex::new(Some(answer_rx)),
        });
        registry.register(Arc::new(mycode_tools::builtin::AskTool::new(channel)));
        // `agent` delegates scoped work to a catalog role; slots, isolation,
        // and per-role model routes live in the host.
        if crate::subagent::any_role_enabled(&role_catalog, &settings.subagents) {
            registry.register(Arc::new(mycode_tools::builtin::AgentTool::new(Arc::new(
                crate::subagent::BridgeAgentHost::new(
                    resolved.clone(),
                    home.clone(),
                    cwd.clone(),
                    &settings,
                    session_id.to_owned(),
                    state.subagent_cancels.clone(),
                    state.mcp_pool.clone(),
                    state.copilot.clone(),
                ),
            ))));
        }
        // The model's web tools ride the same settings-configured backend.
        let web_host: Arc<dyn mycode_tools::builtin::WebHost> =
            Arc::new(BridgeWebHost { home: home.clone() });
        registry.register(Arc::new(mycode_tools::builtin::WebSearchTool::new(
            web_host.clone(),
        )));
        registry.register(Arc::new(mycode_tools::builtin::FetchContentTool::new(
            web_host,
        )));
        registry
    });
    let mcp_catalog = crate::mcp_tools::McpCatalog::from_tools_with_warning(mcp_tools, mcp_warning);
    if let Some(catalog) = mcp_catalog.clone() {
        for tool in catalog.direct_tools() {
            if registry.get(tool.spec().name.as_str()).is_none() {
                registry.register(tool);
            }
        }
        registry.register(Arc::new(crate::mcp_tools::SearchTool::new(Arc::clone(
            &catalog,
        ))));
        registry.register(Arc::new(crate::mcp_tools::UseTool::new(catalog)));
    }

    // The prompt is the latest user message. A trailing assistant or tool
    // result (an interrupted turn, or a write that landed after the user
    // message) used to fail the turn and leave the session unable to send.
    // Those suffix messages stay in the ledger and are omitted from this
    // request so the session can continue.
    let (history, prompt) = split_latest_user(history)?;
    // Owned for the compaction hook and the cancel registration.
    let session_id = session_id.to_owned();

    let context_window = model_context_window(state, &provider, model);
    // Codex-style checkpoint: 90% of the usable window, ~20k-token tail.
    // This pre-turn pass publishes its transcript card immediately. The
    // before-request hook later in this function only rewrites the
    // in-memory request; that card is published after the turn's tool
    // results and final reply commit, so a mid-turn summary cannot split
    // a tool call from its result.
    let compact_scope = crate::compaction::CompactScope {
        home,
        wire: &wire,
        model,
        session_id: &session_id,
        branch_id: branch.as_str(),
        head: &head_stamp_text,
        context_window,
    };
    let compacted = crate::compaction::compact_history(&compact_scope, history, false).await;
    if let Some(summary) = compacted.summary.as_deref() {
        let _ = publish_visible_summary(&writer, events, &session_id, summary).await;
    }
    let history = compacted.messages;

    // Grok Build call pattern: a short index, then the model loads the
    // body or schema itself. Full skill text and MCP schemas stay off this
    // prompt. Skill order and extra-root order are sorted so two turns share
    // a byte-identical prefix.
    let user_home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from);
    let extra_roots = workspace_extra_roots(home, &cwd);
    let directive = crate::subagent::delegation_directive(&role_catalog, &settings.subagents);
    let mcp_note = mcp_catalog.as_ref().map(|catalog| catalog.prompt_note());
    let system_prompt = session_system_prompt(
        home,
        &cwd,
        &registry,
        mcp_note.as_deref(),
        &extra_roots,
        &directive,
        user_home.as_deref(),
    );

    let turn_started = std::time::Instant::now();
    let (agent_tx, mut agent_rx) = tokio::sync::broadcast::channel(256);
    let compact_home = home.clone();
    let compact_wire = wire.clone();
    let compact_model = model.to_owned();
    let compact_session = session_id.clone();
    let compact_branch = branch.as_str().to_owned();
    let compact_head = head_stamp_text.clone();
    let deferred = DeferredSummaries::default();
    let hook_deferred = deferred.clone();
    let hooks = HookRunner::default().with_before_request(move |mut request| {
        let home = compact_home.clone();
        let wire = compact_wire.clone();
        let model = compact_model.clone();
        let session_id = compact_session.clone();
        let branch_id = compact_branch.clone();
        let head = compact_head.clone();
        let deferred = hook_deferred.clone();
        async move {
            let scope = crate::compaction::CompactScope {
                home: &home,
                wire: &wire,
                model: &model,
                session_id: &session_id,
                branch_id: &branch_id,
                head: &head,
                context_window,
            };
            let compacted =
                crate::compaction::compact_history(&scope, request.messages, false).await;
            // Stash only. A ledger append here sits between this turn's
            // tool calls and their results.
            deferred.stash(compacted.summary).await;
            request.messages = compacted.messages;
            request
        }
    });
    let cancel = CancellationToken::new();
    // Publish the token so an Escape-driven CancelChat can abort this turn;
    // the guard unpublishes it on every exit path.
    let _cancel_guard = CancelGuard::register(state.turn_cancels.clone(), &session_id, &cancel);
    let steer = Arc::new(mycode_agent::SteerInbox::new());
    let _steer_guard = SteerGuard::register(
        Arc::clone(&state.steer_inboxes),
        &session_id,
        Arc::clone(&steer),
    );
    let mut config = AgentConfig::new()
        .with_system_prompt(system_prompt)
        .with_prompt_cache_key(Some(session_id.clone()))
        .with_max_output_tokens(model_output_limit(state, &provider, model));
    if let Some(token) = reasoning {
        if let Some(level) = mycode_core::ReasoningLevel::parse(token) {
            config = config.with_reasoning(level);
        } else if effort_token_ok(token) {
            config = config.with_reasoning_token(Some(token.to_owned()));
        }
    }
    let mut agent = Agent::new(config);

    // The ledger pump owns the branch head: tool results commit as they
    // complete, the final assistant message commits at turn end.
    let pump_events = events.clone();
    let pump_session_id = session_id.to_owned();
    let pump_deferred = deferred;
    let pump = tokio::spawn(async move {
        let mut pending_assistant: Option<std::sync::Arc<mycode_core::Message>> = None;
        let mut last_step: Option<crate::protocol::ConversationEntry> = None;
        let mut turn_usage = TurnUsage::default();
        // The opening user message is already in the ledger. Later user
        // messages are steers injected into this same turn.
        let mut saw_opening_user = false;
        loop {
            let event = match agent_rx.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    emit_chat_failed(
                        &pump_events,
                        &writer,
                        &pump_session_id,
                        "agent event stream lagged",
                    )
                    .await;
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            match event {
                mycode_core::events::AgentEvent::MessageDelta(
                    mycode_core::events::MessageDelta::TextDelta(delta),
                ) => {
                    let _ = pump_events.try_send(BridgeEvent::ChatText {
                        session_id: pump_session_id.clone(),
                        delta,
                    });
                }
                mycode_core::events::AgentEvent::MessageDelta(
                    mycode_core::events::MessageDelta::ThinkingDelta(delta),
                ) => {
                    let _ = pump_events.try_send(BridgeEvent::ChatThinking {
                        session_id: pump_session_id.clone(),
                        delta,
                    });
                }
                mycode_core::events::AgentEvent::MessageDelta(
                    mycode_core::events::MessageDelta::ToolCallDelta { .. },
                ) => {}
                mycode_core::events::AgentEvent::ToolStarted {
                    call_id,
                    name,
                    target,
                } => {
                    let spelling = call_id.to_string();
                    // The ToolCall event must commit before its result; the
                    // ledger's ordering check rejects results for calls that
                    // were never opened.
                    if let Err(error) = writer.open_call(&spelling, &name, &target).await {
                        emit_chat_failed(
                            &pump_events,
                            &writer,
                            &pump_session_id,
                            render_error(error),
                        )
                        .await;
                        return;
                    }
                    let _ = pump_events.try_send(BridgeEvent::ToolStarted {
                        session_id: pump_session_id.clone(),
                        call_id: spelling,
                        name,
                        target,
                    });
                }
                mycode_core::events::AgentEvent::ToolProgress { call_id, message } => {
                    let _ = pump_events.try_send(BridgeEvent::ToolProgress {
                        session_id: pump_session_id.clone(),
                        call_id: call_id.to_string(),
                        name: String::new(),
                        message,
                    });
                }
                mycode_core::events::AgentEvent::ToolCompleted {
                    call_id,
                    result: tool_result,
                } => {
                    let Ok(payload) = serde_json::to_vec(&tool_result) else {
                        return;
                    };
                    match writer.close_call(call_id.as_str(), &payload).await {
                        Ok(event_id) => {
                            let entry = project_tool_result_message(&event_id, &tool_result);
                            let _ = pump_events.try_send(BridgeEvent::ToolCompleted {
                                session_id: pump_session_id.clone(),
                                entry,
                            });
                        }
                        Err(error) => {
                            emit_chat_failed(
                                &pump_events,
                                &writer,
                                &pump_session_id,
                                render_error(error),
                            )
                            .await;
                            return;
                        }
                    }
                }
                mycode_core::events::AgentEvent::MessageAdded(message) => {
                    if let mycode_core::Message::User(user) = message.as_ref() {
                        if !saw_opening_user {
                            saw_opening_user = true;
                            continue;
                        }
                        let text = plain_user_text(user);
                        if text.trim().is_empty() {
                            continue;
                        }
                        // A text-only assistant message is held until the turn
                        // ends. A steer continues that turn, so the held reply
                        // has to land before the steer or the ledger order flips.
                        if let Some(held) = pending_assistant.take()
                            && let mycode_core::Message::Assistant(assistant) = held.as_ref()
                        {
                            match serde_json::to_vec(assistant) {
                                Ok(payload) => {
                                    match writer.write(EventKind::Message, &payload).await {
                                        Ok(event_id) => {
                                            let _ =
                                                pump_events.try_send(BridgeEvent::AssistantStep {
                                                    session_id: pump_session_id.clone(),
                                                    entry: project_assistant_message(
                                                        &event_id, assistant,
                                                    ),
                                                });
                                        }
                                        Err(error) => {
                                            emit_chat_failed(
                                                &pump_events,
                                                &writer,
                                                &pump_session_id,
                                                render_error(error),
                                            )
                                            .await;
                                            return;
                                        }
                                    }
                                }
                                Err(_) => {
                                    emit_chat_failed(
                                        &pump_events,
                                        &writer,
                                        &pump_session_id,
                                        "assistant message could not be encoded",
                                    )
                                    .await;
                                    return;
                                }
                            }
                        }
                        match writer.write(EventKind::Message, text.as_bytes()).await {
                            Ok(event_id) => {
                                let entry = crate::protocol::ConversationEntry {
                                    event_id,
                                    kind: crate::protocol::EntryKind::UserMessage,
                                    text: text.into(),
                                    call_id: None,
                                    thinking: String::new(),
                                };
                                let _ = pump_events.try_send(BridgeEvent::SteerCommitted {
                                    session_id: pump_session_id.clone(),
                                    head: writer.head().await,
                                    entry,
                                });
                            }
                            Err(error) => {
                                emit_chat_failed(
                                    &pump_events,
                                    &writer,
                                    &pump_session_id,
                                    render_error(error),
                                )
                                .await;
                                return;
                            }
                        }
                        continue;
                    }
                    let mycode_core::Message::Assistant(assistant) = message.as_ref() else {
                        continue;
                    };
                    // Usage is reported per response cycle, so it has to be
                    // summed here: reading it off the closing message alone
                    // would bill a ten-step turn as one.
                    if let Some(usage) = assistant.usage.as_ref() {
                        turn_usage.fold(usage);
                        let _ = pump_events.try_send(BridgeEvent::UsageSnapshot {
                            session_id: pump_session_id.clone(),
                            model: usage_model.clone(),
                            input: turn_usage.input,
                            context: turn_usage.latest_input,
                            context_cache: turn_usage.latest_cache,
                            output: turn_usage.output,
                            cache: turn_usage.cache,
                            elapsed_ms: turn_started.elapsed().as_millis() as u64,
                        });
                    }
                    // A step that requests tools is not the end of the turn.
                    // Commit it now so the ledger and the transcript keep the
                    // model's real order instead of collapsing the turn into
                    // its last message.
                    let is_step = assistant
                        .blocks
                        .iter()
                        .any(|block| matches!(block, mycode_core::ContentBlock::ToolCall(_)));
                    if !is_step {
                        pending_assistant = Some(message);
                        continue;
                    }
                    let Ok(payload) = serde_json::to_vec(assistant) else {
                        emit_chat_failed(
                            &pump_events,
                            &writer,
                            &pump_session_id,
                            "assistant step could not be encoded",
                        )
                        .await;
                        return;
                    };
                    match writer.write(EventKind::Message, &payload).await {
                        Ok(event_id) => {
                            let entry = project_assistant_message(&event_id, assistant);
                            last_step = Some(entry.clone());
                            let _ = pump_events.try_send(BridgeEvent::AssistantStep {
                                session_id: pump_session_id.clone(),
                                entry,
                            });
                        }
                        Err(error) => {
                            emit_chat_failed(
                                &pump_events,
                                &writer,
                                &pump_session_id,
                                render_error(error),
                            )
                            .await;
                            return;
                        }
                    }
                }
                mycode_core::events::AgentEvent::TurnStarted => {}
                mycode_core::events::AgentEvent::TurnEnded(outcome) => {
                    if matches!(outcome, mycode_core::events::TurnOutcome::Aborted) {
                        // A cancel keeps the partial. Write it when it has
                        // not already been committed as a tool step, then
                        // fail quietly so the desktop can send the queue.
                        // Never ChatDone: that would look like a finished turn.
                        if let Some(message) = pending_assistant.take()
                            && let mycode_core::Message::Assistant(assistant) = message.as_ref()
                        {
                            match serde_json::to_vec(assistant) {
                                Ok(payload) => {
                                    match writer.write(EventKind::Message, &payload).await {
                                        Ok(event_id) => {
                                            let _ =
                                                pump_events.try_send(BridgeEvent::AssistantStep {
                                                    session_id: pump_session_id.clone(),
                                                    entry: project_assistant_message(
                                                        &event_id, assistant,
                                                    ),
                                                });
                                        }
                                        Err(error) => {
                                            emit_chat_failed(
                                                &pump_events,
                                                &writer,
                                                &pump_session_id,
                                                render_error(error),
                                            )
                                            .await;
                                            return;
                                        }
                                    }
                                }
                                Err(_) => {
                                    emit_chat_failed(
                                        &pump_events,
                                        &writer,
                                        &pump_session_id,
                                        "assistant message could not be encoded",
                                    )
                                    .await;
                                    return;
                                }
                            }
                        }
                        emit_chat_failed(&pump_events, &writer, &pump_session_id, CHAT_CANCELLED)
                            .await;
                        return;
                    }
                    let Some(message) = pending_assistant.take() else {
                        // A failed stream that already committed a tool step
                        // (or any earlier step) is done. Reporting "no
                        // assistant message" used to clear the live bubble
                        // after the step was the whole turn.
                        if let Some(entry) = last_step {
                            // Same accounting as a turn that ends with a
                            // message: this path's billing and context meter
                            // used to be silently dropped.
                            commit_turn_usage(
                                &writer,
                                &pump_events,
                                &pump_session_id,
                                &usage_provider,
                                &usage_model,
                                &turn_usage,
                                turn_started,
                                usage_enabled,
                            )
                            .await;
                            pump_deferred
                                .publish(&writer, &pump_events, &pump_session_id)
                                .await;
                            let _ = pump_events.try_send(BridgeEvent::ChatDone {
                                session_id: pump_session_id.clone(),
                                head: writer.head().await,
                                entry,
                            });
                            return;
                        }
                        emit_chat_failed(
                            &pump_events,
                            &writer,
                            &pump_session_id,
                            "the turn ended without an assistant message",
                        )
                        .await;
                        return;
                    };
                    let mycode_core::Message::Assistant(assistant) = message.as_ref() else {
                        return;
                    };
                    match serde_json::to_vec(assistant) {
                        Ok(payload) => {
                            match writer.write(EventKind::Message, &payload).await {
                                Ok(event_id) => {
                                    let entry = project_assistant_message(&event_id, assistant);
                                    commit_turn_usage(
                                        &writer,
                                        &pump_events,
                                        &pump_session_id,
                                        &usage_provider,
                                        &usage_model,
                                        &turn_usage,
                                        turn_started,
                                        usage_enabled,
                                    )
                                    .await;
                                    // The summary card follows the assistant
                                    // message and the usage row, so it cannot
                                    // split a tool call from its result. ChatDone
                                    // then carries the head that includes it.
                                    pump_deferred
                                        .publish(&writer, &pump_events, &pump_session_id)
                                        .await;
                                    let _ = pump_events.try_send(BridgeEvent::ChatDone {
                                        session_id: pump_session_id.clone(),
                                        head: writer.head().await,
                                        entry,
                                    });
                                }
                                Err(error) => {
                                    emit_chat_failed(
                                        &pump_events,
                                        &writer,
                                        &pump_session_id,
                                        render_error(error),
                                    )
                                    .await;
                                }
                            }
                        }
                        Err(_) => {
                            emit_chat_failed(
                                &pump_events,
                                &writer,
                                &pump_session_id,
                                "assistant message could not be encoded",
                            )
                            .await;
                        }
                    }
                    return;
                }
                mycode_core::events::AgentEvent::Error(error) => {
                    if let Some(message) = pending_assistant.take()
                        && let mycode_core::Message::Assistant(assistant) = message.as_ref()
                        && let Ok(payload) = serde_json::to_vec(assistant)
                        && let Ok(event_id) = writer.write(EventKind::Message, &payload).await
                    {
                        let _ = pump_events.try_send(BridgeEvent::AssistantStep {
                            session_id: pump_session_id.clone(),
                            entry: project_assistant_message(&event_id, assistant),
                        });
                    }
                    emit_chat_failed(
                        &pump_events,
                        &writer,
                        &pump_session_id,
                        format!("agent error: {error}"),
                    )
                    .await;
                    return;
                }
            }
        }
    });

    agent.seed_history(history);
    let env = mycode_agent::TurnEnv::new(&wire, &registry, &hooks)
        .with_cancel(cancel)
        .with_events(agent_tx)
        .with_cwd(cwd)
        .with_extra_roots(extra_roots)
        .with_steer(Some(steer));
    let prompt_message = Message::User(prompt);
    let outcome = agent.prompt(prompt_message, &env).await;
    // Drain the pump before returning. An agent error already became
    // ChatFailed inside the pump; returning Err here would send a second one.
    let pump_ended = pump.await;
    if outcome.is_err() && pump_ended.is_ok() {
        return Ok(());
    }
    outcome.map_err(|error| format!("turn failed: {error}"))?;
    pump_ended.map_err(|_| "the turn pump stopped".to_owned())?;
    Ok(())
}

/// Reports a turn failure with the ledger head after whatever already committed.
async fn emit_chat_failed(
    events: &crate::BridgeEventTx,
    writer: &HeadWriter,
    session_id: &str,
    message: impl Into<String>,
) {
    let _ = events.try_send(BridgeEvent::ChatFailed {
        session_id: session_id.to_owned(),
        message: message.into(),
        head: writer.head().await,
    });
}

/// Removes one session's turn token from the cancel registry on scope exit.
struct CancelGuard {
    cancels: Arc<std::sync::Mutex<HashMap<String, Arc<CancellationToken>>>>,
    session_id: String,
    token: Arc<CancellationToken>,
}

impl CancelGuard {
    fn register(
        cancels: Arc<std::sync::Mutex<HashMap<String, Arc<CancellationToken>>>>,
        session_id: &str,
        token: &CancellationToken,
    ) -> Self {
        // The registry is keyed by session id only, so a second turn for the
        // same session replaces the entry; the guard keeps its own token to
        // avoid removing the successor's live token on drop.
        let token = Arc::new(token.clone());
        if let Ok(mut map) = cancels.lock() {
            map.insert(session_id.to_owned(), token.clone());
        }
        Self {
            cancels,
            session_id: session_id.to_owned(),
            token,
        }
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        // Remove the entry only while it still maps to this turn's token;
        // a contended lock leaks one stale entry, which the next turn for
        // the same session replaces.
        if let Ok(mut map) = self.cancels.try_lock()
            && map
                .get(&self.session_id)
                .is_some_and(|live| Arc::ptr_eq(live, &self.token))
        {
            map.remove(&self.session_id);
        }
    }
}

/// Publishes one session's steer inbox for the life of its turn.
struct SteerGuard {
    inboxes: Arc<std::sync::Mutex<HashMap<String, Arc<mycode_agent::SteerInbox>>>>,
    session_id: String,
    inbox: Arc<mycode_agent::SteerInbox>,
}

impl SteerGuard {
    fn register(
        inboxes: Arc<std::sync::Mutex<HashMap<String, Arc<mycode_agent::SteerInbox>>>>,
        session_id: &str,
        inbox: Arc<mycode_agent::SteerInbox>,
    ) -> Self {
        if let Ok(mut map) = inboxes.lock() {
            map.insert(session_id.to_owned(), Arc::clone(&inbox));
        }
        Self {
            inboxes,
            session_id: session_id.to_owned(),
            inbox,
        }
    }
}

impl Drop for SteerGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.inboxes.try_lock()
            && map
                .get(&self.session_id)
                .is_some_and(|live| Arc::ptr_eq(live, &self.inbox))
        {
            map.remove(&self.session_id);
        }
    }
}

/// Token usage summed across every response cycle of one turn.
///
/// Providers report usage per cycle, so a turn that calls tools reports
/// several times. `seen` distinguishes "no usage reported" from a genuine
/// zero, which keeps the ledger from recording a usage event the provider
/// never sent.
/// Writes the turn's usage row after a finished turn. Both terminal paths —
/// the final assistant message and a turn whose last committed event was a
/// tool step — call this, so the accounting never depends on how the turn
/// ended.
#[expect(
    clippy::too_many_arguments,
    reason = "the fields mirror the usage row the turn already assembles"
)]
async fn commit_turn_usage(
    writer: &HeadWriter,
    events: &crate::BridgeEventTx,
    session_id: &str,
    provider: &str,
    model: &str,
    usage: &TurnUsage,
    started: std::time::Instant,
    usage_enabled: bool,
) {
    if !usage_enabled || !usage.seen {
        return;
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let usage_payload = serde_json::json!({
        "provider": provider,
        "model": model,
        "input": usage.input,
        "context": usage.latest_input,
        "context_cache": usage.latest_cache,
        "output": usage.output,
        "cache": usage.cache,
        "elapsed_ms": elapsed_ms,
    });
    if let Ok(bytes) = serde_json::to_vec(&usage_payload)
        && let Ok(usage_event) = writer.write(EventKind::Usage, &bytes).await
    {
        let _ = events.try_send(BridgeEvent::UsageRecorded {
            session_id: session_id.to_owned(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            input: usage.input,
            context: usage.latest_input,
            context_cache: usage.latest_cache,
            output: usage.output,
            cache: usage.cache,
            elapsed_ms,
            entry: project_usage(&usage_event, &bytes),
        });
    }
}

#[derive(Debug, Default)]
struct TurnUsage {
    seen: bool,
    input: u64,
    /// Most recent request's prompt tokens. The context meter uses this
    /// instead of `input`, which sums every tool round.
    latest_input: u64,
    output: u64,
    cache: Option<u64>,
    /// Cache-read tokens on the latest prompt. The context meter shows this
    /// beside the window, separate from [`Self::cache`], which is the sum.
    latest_cache: u64,
}

impl TurnUsage {
    fn fold(&mut self, usage: &mycode_core::Usage) {
        self.seen = true;
        self.input = self.input.saturating_add(usage.input_tokens);
        let prompt = if usage.prompt_tokens > 0 {
            usage.prompt_tokens
        } else {
            usage.input_tokens
        };
        if prompt > 0 {
            self.latest_input = prompt;
            self.latest_cache = usage.cache_read_tokens.unwrap_or(0);
        }
        self.output = self.output.saturating_add(usage.output_tokens);
        if let Some(cache) = usage.cache_read_tokens {
            self.cache = Some(self.cache.unwrap_or_default().saturating_add(cache));
        }
    }
}

/// Splits the latest non-empty user message off as the prompt.
///
/// Messages after it are dropped from this request only. Returning an error
/// here used to stick: the next send loaded the same tail and failed again.
fn split_latest_user(
    history: Vec<Arc<Message>>,
) -> Result<(Vec<Arc<Message>>, mycode_core::UserMessage), String> {
    let Some(index) = history.iter().rposition(
        |message| matches!(message.as_ref(), Message::User(user) if user_has_text(user)),
    ) else {
        return Err("the turn has no user message to answer".to_owned());
    };
    let prompt = match Arc::unwrap_or_clone(Arc::clone(&history[index])) {
        Message::User(user) => user,
        _ => return Err("the turn has no user message to answer".to_owned()),
    };
    let prior = history.into_iter().take(index).collect();
    Ok((prior, prompt))
}

fn effort_token_ok(token: &str) -> bool {
    let len = token.chars().count();
    (1..=32).contains(&len)
        && token.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

fn user_has_text(user: &mycode_core::UserMessage) -> bool {
    user.content.iter().any(|block| match block {
        mycode_core::ContentBlock::Text(text) => !text.text.trim().is_empty(),
        _ => false,
    })
}

fn plain_user_text(user: &mycode_core::UserMessage) -> String {
    let mut text = String::new();
    for block in &user.content {
        if let mycode_core::ContentBlock::Text(block) = block {
            text.push_str(&block.text);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use mycode_agent::session::{EventKind, HeadStamp, SessionId};
    use mycode_config::{
        AppSettings, AuthorityRevision, HomeLayout, ProviderSecrets, ProviderSettings,
        replace_app_settings, replace_provider_secrets,
    };
    use mycode_core::ProviderError;
    use mycode_providers::{ResolvedProvider, SseTransport, TransportCall};
    use mycode_tools::ToolRegistry;

    use super::{plain_user_text, session_system_prompt};
    use crate::protocol::EntryKind;
    use crate::state::CoreState;

    struct TurnTransport {
        kinds: Mutex<Vec<&'static str>>,
        /// How many times `COMPACTION SUMMARY` appears in the final model request.
        final_summaries: Mutex<Option<usize>>,
    }

    #[async_trait::async_trait]
    impl SseTransport for TurnTransport {
        async fn post(
            &self,
            call: TransportCall,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>,
            >,
            ProviderError,
        > {
            let body = String::from_utf8_lossy(&call.body);
            let (kind, sse) = if body.contains("CONTEXT CHECKPOINT COMPACTION") {
                let summary = format!(
                    "handoff notes. {}",
                    "The covered prefix is replaced by this checkpoint. ".repeat(6)
                );
                ("summary", text_sse(&summary, 80))
            } else if body.contains("\"role\":\"tool\"") {
                ("final", {
                    *self.final_summaries.lock().expect("summaries") =
                        Some(body.matches("COMPACTION SUMMARY").count());
                    text_sse("ok", 3)
                })
            } else {
                ("tools", tool_sse())
            };
            self.kinds.lock().expect("kinds").push(kind);
            Ok(Box::pin(futures_util::stream::once(async move {
                Ok(bytes::Bytes::from(sse))
            })))
        }
    }

    fn text_sse(text: &str, output_tokens: u64) -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {"content": text},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": output_tokens}
        });
        format!("data: {chunk}\n\ndata: [DONE]\n\n")
    }

    fn tool_sse() -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_read_a",
                            "function": {
                                "name": "read",
                                "arguments": serde_json::json!({"path": "a.txt"}).to_string()
                            }
                        },
                        {
                            "index": 1,
                            "id": "call_read_b",
                            "function": {
                                "name": "read",
                                "arguments": serde_json::json!({"path": "b.txt"}).to_string()
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        });
        format!("data: {chunk}\n\ndata: [DONE]\n\n")
    }

    fn scratch() -> (std::path::PathBuf, HomeLayout) {
        let root = std::env::temp_dir().join(format!(
            "mycode-turn-compact-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("scratch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let home = HomeLayout::from_root(&root).expect("home");
        (root, home)
    }

    #[tokio::test]
    async fn hook_compaction_waits_until_the_turn_commits() {
        let (root, home) = scratch();
        let mut settings = AppSettings::default();
        settings.providers.push(ProviderSettings {
            id: "local".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["test-model".to_owned()],
            enabled: true,
            // ~22k token auto threshold. The seeded prompt stays under it;
            // the two read results push the next hook over it.
            context_limit: Some(26_000),
            max_output: None,
        });
        replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("settings");
        replace_provider_secrets(
            &home,
            AuthorityRevision::ABSENT,
            &ProviderSecrets::new().with_key("local", Some("test-key")),
        )
        .expect("secrets");
        let cwd = home.root().join(mycode_config::SCRATCH_DIR);
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::write(
            cwd.join("a.txt"),
            format!("alpha-body\n{}", "a".repeat(20_000)),
        )
        .expect("a");
        std::fs::write(
            cwd.join("b.txt"),
            format!("beta-body\n{}", "b".repeat(20_000)),
        )
        .expect("b");

        let catalog = mycode_providers::catalog::current(&home);
        let state = CoreState::new(home, catalog, Vec::new());
        let created = state.service.create().await.expect("session");
        let mut head = HeadStamp::Empty;
        for text in ["prior work", &"P".repeat(72_000)] {
            let reservation = state
                .service
                .reserve_event(
                    &created.session_id,
                    &created.branch_id,
                    EventKind::Message,
                    None,
                    text.as_bytes(),
                )
                .await
                .expect("reserve");
            head = state
                .service
                .append(&created.session_id, &created.branch_id, &head, &reservation)
                .await
                .expect("append")
                .head;
        }

        let scripted = Arc::new(TurnTransport {
            kinds: Mutex::new(Vec::new()),
            final_summaries: Mutex::new(None),
        });
        let transport: Arc<dyn SseTransport> = scripted.clone();
        let provider = settings.providers[0].clone();
        let resolved = ResolvedProvider::resolve(&provider, "test-model", "test-key", "test-agent")
            .expect("resolve");

        let (tx, rx) = async_channel::unbounded();
        let collected = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Ok(event) = rx.recv().await {
                let done = matches!(
                    event,
                    crate::BridgeEvent::ChatDone { .. } | crate::BridgeEvent::ChatFailed { .. }
                );
                events.push(event);
                if done {
                    break;
                }
            }
            events
        });
        let session_id = created.session_id.as_str().to_owned();
        super::run_chat_turn_on(
            &state,
            &tx,
            &session_id,
            created.session_id.clone(),
            created.branch_id.clone(),
            head,
            "local",
            "test-model",
            None,
            cwd,
            resolved,
            settings,
            provider,
            transport,
        )
        .await
        .expect("turn");
        let kinds = scripted.kinds.lock().expect("kinds").clone();
        assert_eq!(
            kinds,
            ["tools", "summary", "final"],
            "compaction must fire in the hook, between the tool round and the final reply"
        );
        assert_eq!(
            *scripted.final_summaries.lock().expect("summaries"),
            Some(1),
            "the compacted request must carry the summary once"
        );
        drop(tx);
        let events = tokio::time::timeout(std::time::Duration::from_secs(20), collected)
            .await
            .expect("turn events timed out")
            .expect("collector");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, crate::BridgeEvent::ChatDone { .. })),
            "final reply was not committed: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, crate::BridgeEvent::ChatFailed { .. })),
            "turn failed: {events:?}"
        );

        // Reopen reads the ledger, the same path a restarted session uses.
        let opened = crate::ledger::open_conversation(
            &state.service,
            &SessionId::parse(&session_id).expect("session id"),
        )
        .await
        .expect("reopen");
        let entries = &opened.entries;
        let summary_at = entries
            .iter()
            .position(|entry| crate::compaction::is_display_only_summary(&entry.text))
            .expect("summary card");
        let reply_at = entries
            .iter()
            .position(|entry| {
                entry.kind == EntryKind::AssistantMessage && entry.text.contains("ok")
            })
            .expect("final reply");
        assert!(
            summary_at > reply_at,
            "summary card landed before the final reply"
        );
        let tool_calls: Vec<_> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.kind == EntryKind::ToolCall)
            .collect();
        assert_eq!(
            tool_calls.len(),
            2,
            "both read calls should be on the ledger"
        );
        for (index, call) in tool_calls {
            let call_id = call.call_id.as_deref().expect("call id");
            let result_at = entries[index + 1..]
                .iter()
                .position(|entry| {
                    entry.kind == EntryKind::ToolResult && entry.call_id.as_deref() == Some(call_id)
                })
                .map(|offset| index + 1 + offset)
                .expect("tool result");
            assert!(
                result_at < summary_at,
                "summary card split the tool call from its result"
            );
            assert!(
                entries[index + 1..result_at]
                    .iter()
                    .all(|entry| !crate::compaction::is_display_only_summary(&entry.text)),
                "a summary row was written between a tool call and its result"
            );
            let result = &entries[result_at];
            assert!(
                !result.text.starts_with("failed:"),
                "read did not finish: {}",
                result.text
            );
        }
        assert!(
            entries
                .iter()
                .any(|entry| entry.text.contains("alpha-body")),
            "first read result missing"
        );
        assert!(
            entries.iter().any(|entry| entry.text.contains("beta-body")),
            "second read result missing"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn session_system_prompt_is_identical_across_calls() {
        let root = std::env::temp_dir().join(format!(
            "mycode-prompt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _guard = TempDir(root.clone());
        fs::create_dir_all(root.join(".agents")).unwrap();
        for slug in ["zebra", "alpha", "middle"] {
            let dir = root.join(".agents").join(slug);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("SKILL.md"), format!("# {slug}\n")).unwrap();
        }
        let home = HomeLayout::from_root(&root).unwrap();
        let registry = ToolRegistry::new();
        mycode_tools::register_builtins(&registry);
        let forward = vec![root.join("b"), root.join("a")];
        let reverse = vec![root.join("a"), root.join("b")];
        let first = session_system_prompt(&home, &root, &registry, None, &forward, "", None);
        let second = session_system_prompt(&home, &root, &registry, None, &reverse, "", None);
        assert_eq!(first, second);
        let alpha = first.find("/alpha").unwrap();
        let middle = first.find("/middle").unwrap();
        let zebra = first.find("/zebra").unwrap();
        assert!(alpha < middle && middle < zebra);
        let a = first
            .find(&format!("- {}", root.join("a").display()))
            .unwrap();
        let b = first
            .find(&format!("- {}", root.join("b").display()))
            .unwrap();
        assert!(a < b);
    }

    #[tokio::test]
    async fn manual_compact_fails_when_the_session_cannot_open() {
        use mycode_agent::session::{BranchId, HeadStamp, SessionId};

        use super::manual_compact;

        let (root, home) = scratch();
        let catalog = mycode_providers::catalog::current(&home);
        let state = Arc::new(CoreState::new(home, catalog, Vec::new()));
        let (tx, rx) = async_channel::unbounded();
        manual_compact(
            state,
            tx,
            SessionId::generate().expect("session id"),
            BranchId::generate().expect("branch id"),
            HeadStamp::Empty,
            "missing".to_owned(),
            "missing-model".to_owned(),
        )
        .await;
        let event = rx.recv().await.expect("compact event");
        match event {
            crate::BridgeEvent::CompactFinished { message, ok, .. } => {
                assert!(!ok);
                assert!(message.contains("could not open the session"), "{message}");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    const PROMPT_A: &str = "PROMPT_A write a snake game";
    const REPLY_A: &str = "REPLY_A here is the game";
    const PROMPT_B: &str = "PROMPT_B also add a score";
    const PARTIAL_A: &str = "PARTIAL_A game loop";

    struct HistoryTransport {
        bodies: Mutex<Vec<String>>,
        hang_partial: bool,
        partial_ready: Arc<tokio::sync::Notify>,
    }

    fn reply_sse(text: &str) -> String {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {"content": text},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 20, "completion_tokens": 3}
        });
        format!("data: {chunk}\n\ndata: [DONE]\n\n")
    }

    fn partial_sse(text: &str) -> String {
        let chunk = serde_json::json!({
            "choices": [{"delta": {"content": text}}]
        });
        format!("data: {chunk}\n\n")
    }

    #[async_trait::async_trait]
    impl SseTransport for HistoryTransport {
        async fn post(
            &self,
            call: TransportCall,
            cancel: tokio_util::sync::CancellationToken,
        ) -> Result<
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, ProviderError>> + Send>,
            >,
            ProviderError,
        > {
            let body = String::from_utf8_lossy(&call.body).into_owned();
            self.bodies.lock().expect("bodies").push(body.clone());
            let is_b = body.contains(PROMPT_B);
            if !is_b && self.hang_partial {
                use futures_util::StreamExt as _;
                let partial = bytes::Bytes::from(partial_sse(PARTIAL_A));
                let ready = Arc::clone(&self.partial_ready);
                // The second poll runs only after the driver has taken the
                // partial chunk, so the cancel waits until that text is queued.
                let stream = futures_util::stream::once(async move { Ok(partial) }).chain(
                    futures_util::stream::once(async move {
                        ready.notify_one();
                        cancel.cancelled().await;
                        Err::<bytes::Bytes, ProviderError>(ProviderError::new(
                            mycode_core::ProviderErrorKind::Cancelled,
                        ))
                    }),
                );
                return Ok(Box::pin(stream));
            }
            let reply = if is_b { "REPLY_B ok" } else { REPLY_A };
            let sse = reply_sse(reply);
            Ok(Box::pin(futures_util::stream::once(async move {
                Ok(bytes::Bytes::from(sse))
            })))
        }
    }

    struct TurnFixture {
        root: PathBuf,
        state: Arc<CoreState>,
        settings: AppSettings,
        provider: ProviderSettings,
        resolved: ResolvedProvider,
        cwd: PathBuf,
        session: mycode_agent::session::SessionId,
        branch: mycode_agent::session::BranchId,
    }

    async fn turn_fixture() -> TurnFixture {
        let (root, home) = scratch();
        let mut settings = AppSettings::default();
        settings.providers.push(ProviderSettings {
            id: "local".to_owned(),
            kind: "openai-completions".to_owned(),
            base_url: "https://example.com/v1".to_owned(),
            models: vec!["test-model".to_owned()],
            enabled: true,
            context_limit: Some(200_000),
            max_output: None,
        });
        replace_app_settings(&home, AuthorityRevision::ABSENT, &settings).expect("settings");
        replace_provider_secrets(
            &home,
            AuthorityRevision::ABSENT,
            &ProviderSecrets::new().with_key("local", Some("test-key")),
        )
        .expect("secrets");
        let cwd = home.root().join(mycode_config::SCRATCH_DIR);
        std::fs::create_dir_all(&cwd).expect("cwd");
        let catalog = mycode_providers::catalog::current(&home);
        let state = Arc::new(CoreState::new(home, catalog, Vec::new()));
        let created = state.service.create().await.expect("session");
        let provider = settings.providers[0].clone();
        let resolved = ResolvedProvider::resolve(&provider, "test-model", "test-key", "test-agent")
            .expect("resolve");
        TurnFixture {
            root,
            state,
            settings,
            provider,
            resolved,
            cwd,
            session: created.session_id,
            branch: created.branch_id,
        }
    }

    fn parse_head(spelling: &str) -> HeadStamp {
        match mycode_agent::session::SessionEventId::parse(spelling) {
            Some(id) => HeadStamp::Event(id),
            None => HeadStamp::Empty,
        }
    }

    async fn run_scripted_turn(
        fixture: &TurnFixture,
        transport: Arc<dyn SseTransport>,
        head: HeadStamp,
    ) -> (Result<(), String>, Vec<crate::BridgeEvent>) {
        let (tx, rx) = async_channel::unbounded();
        let outcome = super::run_chat_turn_on(
            &fixture.state,
            &tx,
            fixture.session.as_str(),
            fixture.session.clone(),
            fixture.branch.clone(),
            head,
            "local",
            "test-model",
            None,
            fixture.cwd.clone(),
            fixture.resolved.clone(),
            fixture.settings.clone(),
            fixture.provider.clone(),
            transport,
        )
        .await;
        drop(tx);
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        (outcome, events)
    }

    fn prompt_b_body(transport: &HistoryTransport) -> String {
        transport
            .bodies
            .lock()
            .expect("bodies")
            .iter()
            .rev()
            .find(|body| body.contains(PROMPT_B))
            .cloned()
            .expect("B request was sent")
    }

    fn assert_cache_key(body: &str, session_id: &str) {
        assert!(
            body.contains(&format!("\"prompt_cache_key\":\"{session_id}\"")),
            "prompt cache key missing: {body}"
        );
    }

    /// Queue path: A finishes (ChatDone), then B is sent with that head.
    #[tokio::test]
    async fn queued_follow_up_after_chat_done_carries_history() {
        let fixture = turn_fixture().await;
        let transport = Arc::new(HistoryTransport {
            bodies: Mutex::new(Vec::new()),
            hang_partial: false,
            partial_ready: Arc::new(tokio::sync::Notify::new()),
        });
        let (head_a, _) = crate::ledger::send_message(
            &fixture.state.service,
            &fixture.session,
            &fixture.branch,
            &HeadStamp::Empty,
            PROMPT_A,
        )
        .await
        .expect("prompt A");
        let (outcome, events) =
            run_scripted_turn(&fixture, transport.clone(), parse_head(&head_a)).await;
        outcome.expect("turn A");
        let done_head = events
            .iter()
            .find_map(|event| match event {
                crate::BridgeEvent::ChatDone { head, .. } => Some(head.clone()),
                _ => None,
            })
            .expect("ChatDone");
        let (head_b, _) = crate::ledger::send_message(
            &fixture.state.service,
            &fixture.session,
            &fixture.branch,
            &parse_head(&done_head),
            PROMPT_B,
        )
        .await
        .expect("prompt B");
        let (outcome, _) =
            run_scripted_turn(&fixture, transport.clone(), parse_head(&head_b)).await;
        outcome.expect("turn B");
        let body = prompt_b_body(&transport);
        assert!(body.contains(PROMPT_A), "B lost A's prompt: {body}");
        assert!(body.contains(REPLY_A), "B lost A's reply: {body}");
        assert!(body.contains(PROMPT_B), "B lost its own prompt: {body}");
        assert_cache_key(&body, fixture.session.as_str());
        let _ = std::fs::remove_dir_all(&fixture.root);
    }

    /// Interrupt path: A streams a partial, the user cancels, then B is sent.
    #[tokio::test]
    async fn interrupt_then_send_keeps_partial_and_cache_key() {
        let fixture = turn_fixture().await;
        let transport = Arc::new(HistoryTransport {
            bodies: Mutex::new(Vec::new()),
            hang_partial: true,
            partial_ready: Arc::new(tokio::sync::Notify::new()),
        });
        let (head_a, _) = crate::ledger::send_message(
            &fixture.state.service,
            &fixture.session,
            &fixture.branch,
            &HeadStamp::Empty,
            PROMPT_A,
        )
        .await
        .expect("prompt A");
        let state = Arc::clone(&fixture.state);
        let session = fixture.session.clone();
        let branch = fixture.branch.clone();
        let cwd = fixture.cwd.clone();
        let resolved = fixture.resolved.clone();
        let settings = fixture.settings.clone();
        let provider = fixture.provider.clone();
        let running = transport.clone();
        let head = parse_head(&head_a);
        let session_id = session.as_str().to_owned();
        let turn_a = tokio::spawn(async move {
            let (tx, rx) = async_channel::unbounded();
            let outcome = super::run_chat_turn_on(
                &state,
                &tx,
                &session_id,
                session,
                branch,
                head,
                "local",
                "test-model",
                None,
                cwd,
                resolved,
                settings,
                provider,
                running,
            )
            .await;
            drop(tx);
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            (outcome, events)
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            transport.partial_ready.notified(),
        )
        .await
        .expect("partial text was not streamed");
        let token = fixture
            .state
            .turn_cancels
            .lock()
            .expect("cancels")
            .get(fixture.session.as_str())
            .cloned()
            .expect("turn token");
        token.cancel();
        let (outcome, events) = tokio::time::timeout(std::time::Duration::from_secs(10), turn_a)
            .await
            .expect("turn A did not unwind")
            .expect("turn task");
        outcome.expect("cancelled turn still returns");
        let failed = events.iter().find_map(|event| match event {
            crate::BridgeEvent::ChatFailed { message, head, .. } => {
                Some((message.clone(), head.clone()))
            }
            _ => None,
        });
        let (message, failed_head) = failed.expect("ChatFailed");
        assert_eq!(message, crate::protocol::CHAT_CANCELLED);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, crate::BridgeEvent::ChatDone { .. })),
            "a user cancel must not emit ChatDone"
        );
        assert!(
            !failed_head.is_empty(),
            "cancel must report the ledger head"
        );
        let history = crate::ledger::ledger_history(
            &fixture.state.service,
            &fixture.session,
            &fixture.branch,
            &parse_head(&failed_head),
        )
        .await
        .expect("ledger");
        let rendered = history
            .iter()
            .map(|message| match message.as_ref() {
                mycode_core::Message::Assistant(assistant) => assistant.text(),
                mycode_core::Message::User(user) => plain_user_text(user),
                mycode_core::Message::ToolResult(_) => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains(PARTIAL_A),
            "ledger dropped the partial: {rendered}"
        );
        assert!(
            rendered.contains("[error] the response was interrupted: interrupted by user"),
            "ledger dropped the interruption line: {rendered}"
        );
        let (head_b, _) = crate::ledger::send_message(
            &fixture.state.service,
            &fixture.session,
            &fixture.branch,
            &parse_head(&failed_head),
            PROMPT_B,
        )
        .await
        .expect("prompt B");
        let (outcome, _) =
            run_scripted_turn(&fixture, transport.clone(), parse_head(&head_b)).await;
        outcome.expect("turn B");
        let body = prompt_b_body(&transport);
        assert!(body.contains(PROMPT_A), "B lost A's prompt: {body}");
        assert!(body.contains(PARTIAL_A), "B lost the partial: {body}");
        assert!(
            body.contains("[error] the response was interrupted: interrupted by user"),
            "B lost the interruption marker: {body}"
        );
        assert!(body.contains(PROMPT_B), "B lost its own prompt: {body}");
        assert_cache_key(&body, fixture.session.as_str());
        let _ = std::fs::remove_dir_all(&fixture.root);
    }
}
