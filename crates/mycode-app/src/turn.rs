//! One model turn: provider resolution, the tool registry, and per-turn
//! setup. The submodules hold the turn's cancel and steer controls, the
//! ledger pump, `/compact` and the summary cards, and the system prompt.

mod compact;
mod control;
mod prompt;
mod pump;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use mycode_agent::session::{BranchId, SessionId};
use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_config::{
    AppSettings, HomeLayout, ProviderSettings, read_app_settings, read_provider_secrets,
};
use mycode_core::Message;
use mycode_providers::{ReqwestTransport, ResolvedProvider, SseTransport, WireProvider};
use mycode_tools::ToolRegistry;

use crate::BridgeEvent;
use crate::ledger::{HeadWriter, branch_head, head_spelling, ledger_history, render_error};
use crate::oauth::resolve_request_auth;
use crate::protocol::CHAT_CANCELLED;
use crate::state::{CoreState, model_context_window, model_output_limit};
use crate::tool_hosts::{BridgeAskChannel, BridgeWebHost};

pub(crate) use self::compact::manual_compact;
use self::compact::{DeferredSummaries, publish_visible_summary};
use self::control::TurnControl;
use self::prompt::session_system_prompt;
pub(crate) use self::prompt::{
    os_user_home, push_scratch_note, push_skill_catalog, split_scratch_root, workspace_extra_roots,
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
        if let Err(message) = outcome {
            // A cancel that lands during setup surfaces as setup failing;
            // report it as the quiet cancel the desktop expects.
            let message = if control.cancel.is_cancelled() {
                CHAT_CANCELLED.to_owned()
            } else {
                message
            };
            let _ = events.send(BridgeEvent::ChatFailed {
                session_id: session_id.clone(),
                message,
                head: String::new(),
            });
        }
        // A steer accepted after the agent's last drain was never sent.
        // Hand it back instead of dropping it with the inbox.
        let texts = control.retire_steers();
        if !texts.is_empty() {
            let _ = events.send(BridgeEvent::SteersReturned { session_id, texts });
        }
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
        let step_home = home.clone();
        let step_id = provider_id.to_owned();
        let loaded = tokio::task::spawn_blocking(move || {
            let settings = read_app_settings(&step_home)
                .map_err(|error| crate::settings_io::render_config_error(&error))?;
            let Some(index) = settings
                .providers
                .iter()
                .position(|provider| provider.id == step_id && provider.enabled)
            else {
                return Err("provider not found or disabled in settings".to_owned());
            };
            let secrets = read_provider_secrets(&step_home)
                .map_err(|error| crate::settings_io::render_config_error(&error))?;
            let Some(key) = secrets.key(&step_id).map(str::to_owned) else {
                return Err("provider API key is not set".to_owned());
            };
            let provider = settings.providers[index].clone();
            Ok((settings, provider, key))
        })
        .await
        .map_err(|error| format!("credentials task failed: {error}"))?;
        match loaded {
            Ok(ready) => return Ok(ready),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
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
) -> Result<(), String> {
    let home = &state.home;
    // Settings and keys are written by concurrently dispatched save
    // commands; a provider added moments ago may not have landed on disk
    // yet, so the lookup retries briefly before failing the turn.
    let (settings, provider, stored_key) = control
        .unless_cancelled(turn_credentials(home, provider_id))
        .await??;
    // Copilot stores its long-lived OAuth token where other providers keep
    // an API key; each turn exchanges it for a short-lived bearer. A refresh
    // rotates the stored token, so this step always runs to completion.
    let (bearer, extra_headers) =
        resolve_request_auth(&state.home, &state.copilot, &provider, &stored_key).await?;
    control.check()?;
    let mut resolved = ResolvedProvider::resolve(
        &provider,
        model,
        &bearer,
        &crate::user_agent::default_user_agent(),
    )
    .map_err(|error| format!("provider setup failed: {error:?}"))?;
    resolved.headers.extend(extra_headers);
    let transport: Arc<dyn SseTransport> = Arc::new(
        ReqwestTransport::new().map_err(|error| format!("HTTP transport unavailable: {error}"))?,
    );
    run_chat_turn_on(
        state, events, session_id, session, branch, model, reasoning, cwd, resolved, settings,
        provider, transport, control,
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
    model: &str,
    reasoning: Option<&str>,
    cwd: PathBuf,
    resolved: ResolvedProvider,
    settings: AppSettings,
    provider: ProviderSettings,
    transport: Arc<dyn SseTransport>,
    control: &TurnControl,
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
    // message) used to fail the turn and leave the session unable to send.
    // Those suffix messages stay in the ledger and are omitted from this
    // request so the session can continue. Checked before MCP connects.
    let (history, prompt) = split_latest_user(history)?;
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
    // that fails to connect is skipped, never a failed turn. Stdio servers
    // are spawned kill-on-drop, so a cancel may abandon a slow handshake.
    let (mcp_tools, mcp_warning) = control
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
        let session_id = session_id.to_owned();
        tokio::task::spawn_blocking(move || {
            (
                mycode_config::discover_roles(&home, Some(&cwd)),
                workspace_extra_roots(&home, &session_id, &cwd),
            )
        })
        .await
        .map_err(|error| format!("workspace setup task: {error}"))?
    };
    let registry = Arc::new({
        let registry = ToolRegistry::new();
        mycode_tools::register_builtins(&registry);
        // ask_user rides the same registry; its channel forwards questions
        // to the UI over the event channel and waits on the shared router.
        let channel: Arc<dyn mycode_tools::builtin::AskChannel> = Arc::new(BridgeAskChannel {
            session_id: session_id.to_owned(),
            events: events.clone(),
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
    let mcp_catalog = crate::mcp_tools::register_mcp_tools(&registry, mcp_tools, mcp_warning);

    // Owned for the compaction hook and the cancel registration.
    let session_id = session_id.to_owned();

    let context_window = model_context_window(state, &provider, model);
    let user_home = os_user_home();
    let read_roots = mycode_config::skill_read_roots(user_home.as_deref());
    let directive = crate::subagent::delegation_directive(&role_catalog, &settings.subagents);
    let mcp_note = mcp_catalog.as_ref().map(|catalog| catalog.prompt_note());
    let system_prompt = {
        let home = home.clone();
        let cwd = cwd.clone();
        let registry = Arc::clone(&registry);
        let extra_roots = extra_roots.clone();
        let user_home = user_home.clone();
        let skill_settings = settings.skills.clone();
        tokio::task::spawn_blocking(move || {
            session_system_prompt(
                &home,
                &cwd,
                &registry,
                mcp_note.as_deref(),
                &extra_roots,
                &directive,
                user_home.as_deref(),
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
        overhead_tokens,
        cancel: &control.cancel,
    };
    let compacted = crate::compaction::compact_history(&compact_scope, history, false).await;
    // A written checkpoint is already on disk, so its card is published even
    // when the user cancelled during the summary.
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
    let compact_home = home.clone();
    let compact_wire = wire.clone();
    let compact_model = model.to_owned();
    let compact_session = session_id.clone();
    let compact_branch = branch.as_str().to_owned();
    let compact_head = head_stamp_text.clone();
    let deferred = DeferredSummaries::default();
    let hook_deferred = deferred.clone();
    let hook_bus = agent_tx.clone();
    let hook_cancel = control.cancel.clone();
    let hooks = HookRunner::default().with_before_request(move |mut request| {
        let home = compact_home.clone();
        let wire = compact_wire.clone();
        let model = compact_model.clone();
        let session_id = compact_session.clone();
        let branch_id = compact_branch.clone();
        let head = compact_head.clone();
        let deferred = hook_deferred.clone();
        let bus = hook_bus.clone();
        let cancel = hook_cancel.clone();
        async move {
            // The turn token stops a summary the user no longer waits for;
            // the agent then sees the cancel right after this hook.
            let scope = crate::compaction::CompactScope {
                home: &home,
                wire: &wire,
                model: &model,
                session_id: &session_id,
                branch_id: &branch_id,
                head: &head,
                context_window,
                overhead_tokens: crate::compaction::estimate_prefix_tokens(
                    &request.system_prompt,
                    &request.tools,
                ),
                cancel: &cancel,
            };
            let compacted =
                crate::compaction::compact_history(&scope, request.messages, false).await;
            // Stash only. A ledger append here sits between this turn's
            // tool calls and their results.
            if deferred.stash(compacted.summary).await {
                // Bus order: the previous response is already queued, and
                // this request's deltas come after. The pump turns this
                // into PromptCacheWatch, so the warning attaches to the
                // prompt that actually lost the prefix.
                let _ = bus.send(mycode_core::events::AgentEvent::CacheDisrupted);
            }
            request.messages = compacted.messages;
            request
        }
    });
    let mut config = AgentConfig::new()
        .with_system_prompt(system_prompt)
        .with_prompt_cache_key(Some(session_id.clone()))
        .with_max_output_tokens(model_output_limit(state, &provider, model));
    if let Some(token) = reasoning {
        if let Some(level) = mycode_core::ReasoningLevel::parse(token) {
            config = config.with_reasoning(level);
        } else if mycode_config::is_valid_effort_token(token) {
            config = config.with_reasoning_token(Some(token.to_owned()));
        }
    }
    let mut agent = Agent::new(config);

    // The ledger pump owns the branch head for the rest of the turn.
    let pump = tokio::spawn(
        pump::LedgerPump {
            events: events.clone(),
            writer,
            session_id: session_id.clone(),
            deferred,
            cancel: control.cancel.clone(),
            usage_model,
            started: turn_started,
        }
        .run(agent_rx),
    );

    agent.seed_history(history);
    let env = mycode_agent::TurnEnv::new(&wire, &registry, &hooks)
        .with_cancel(control.cancel.clone())
        .with_events(agent_tx)
        .with_cwd(cwd)
        .with_extra_roots(extra_roots)
        .with_read_roots(read_roots)
        .with_steer(Some(Arc::clone(&control.steer)));
    let prompt_message = Message::User(prompt);
    let outcome = agent.prompt(prompt_message, &env).await;
    // Close the agent bus (the env and the hook each hold a sender) so a
    // turn that never started reaches the pump as a closed stream instead of
    // leaving it parked forever.
    drop(env);
    drop(hooks);
    // Drain the pump before returning. An agent error already became
    // ChatFailed inside the pump; returning Err here would send a second one.
    let pump_ended = pump.await;
    if outcome.is_err() && pump_ended.is_ok() {
        return Ok(());
    }
    outcome.map_err(|error| format!("turn failed: {error}"))?;
    pump_ended.map_err(|error| format!("the turn pump stopped: {error}"))?;
    Ok(())
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

fn user_has_text(user: &mycode_core::UserMessage) -> bool {
    user.content.iter().any(|block| match block {
        mycode_core::ContentBlock::Text(text) => !text.text.trim().is_empty(),
        _ => false,
    })
}
