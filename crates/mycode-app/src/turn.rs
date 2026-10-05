//! One model turn: provider resolution, the tool registry, the ledger pump,
//! and per-turn cancellation.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
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
use crate::state::{CoreState, model_context_window};
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
        cwd,
    )
    .await
    {
        let _ = events.try_send(BridgeEvent::ChatFailed {
            session_id,
            message,
        });
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

type CheckpointFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// Builds the before-tool hook that snapshots a mutating tool's target
/// before dispatch: `write`/`edit` paths resolve against `cwd` (an absolute
/// path replaces it), and the copy runs on the blocking pool so the
/// single-threaded core executor never stalls on file I/O. A checkpoint
/// error fails the tool; it is not skipped.
pub(crate) fn checkpoint_hook(
    home: HomeLayout,
    cwd: PathBuf,
    session: String,
    head: String,
) -> impl Fn(&str, &serde_json::Value) -> CheckpointFuture + Send + Sync + 'static {
    move |tool, args| {
        let raw_path = matches!(tool, "write" | "edit")
            .then(|| {
                args.get("path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
        let home = home.clone();
        let cwd = cwd.clone();
        let session = session.clone();
        let head = head.clone();
        Box::pin(async move {
            let Some(raw_path) = raw_path else {
                return Ok(());
            };
            let path = cwd.join(raw_path);
            match tokio::task::spawn_blocking(move || {
                mycode_config::checkpoint_file(&home, &session, &path, &head)
            })
            .await
            {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(error)) => Err(format!("checkpoint failed: {}", error.summary())),
                Err(_) => Err("checkpoint failed: snapshot task panicked".to_owned()),
            }
        })
    }
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
    state
        .workspace_roots
        .into_iter()
        .filter(|root| !same_dir(root, &cwd_text))
        .map(std::path::PathBuf::from)
        .take(mycode_config::MAX_WORKSPACE_ROOTS)
        .collect()
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
    cwd: PathBuf,
) -> Result<(), String> {
    let home = &state.home;
    // Settings and keys are written by concurrently dispatched save
    // commands; a provider added moments ago may not have landed on disk
    // yet, so the lookup retries briefly before failing the turn.
    let (settings, provider, stored_key) = turn_credentials(home, provider_id).await?;
    // Copilot stores its long-lived OAuth token where other providers keep
    // an API key; each turn exchanges it for a short-lived bearer.
    let (bearer, extra_headers) = resolve_request_auth(state, &provider, &stored_key).await?;
    let mut resolved =
        ResolvedProvider::resolve(&provider, model, &bearer, &settings.effective_user_agent())
            .map_err(|error| format!("provider setup failed: {error:?}"))?;
    resolved.headers.extend(extra_headers);
    let transport: Arc<dyn SseTransport> =
        Arc::new(ReqwestTransport::new().map_err(|_| "HTTP transport unavailable".to_owned())?);
    let wire = WireProvider::new(resolved.clone(), transport.clone());

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
    let mut history = ledger_history(&state.service, &session, &branch, &expected_head)
        .await
        .map_err(render_error)?;
    let usage_enabled = settings.usage.enabled;
    let usage_provider = provider_id.to_owned();
    let usage_model = model.to_owned();
    let head_stamp_text = head_spelling(&expected_head);
    // Tag every write/edit snapshot in this turn with the user-message head
    // so a later recall can select mutations that happened after that message.
    let checkpoint_head = match &expected_head {
        HeadStamp::Event(event) => event.as_str().to_owned(),
        HeadStamp::Empty => String::new(),
    };
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
        // task delegates scoped work to a catalog role; slots, isolation,
        // and per-role model routes live in the host.
        if crate::subagent::any_role_enabled(&role_catalog, &settings.subagents) {
            registry.register(Arc::new(mycode_tools::builtin::TaskTool::new(Arc::new(
                crate::subagent::BridgeTaskHost::new(
                    resolved.clone(),
                    home.clone(),
                    cwd.clone(),
                    &settings,
                    session_id.to_owned(),
                    state.subagent_cancels.clone(),
                    state.mcp_pool.clone(),
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

    // Split the last committed user message off as the prompt; everything
    // before it is replay history.
    let Some(last) = history.pop() else {
        return Err("the turn has no user message to answer".to_owned());
    };
    let prompt = match Arc::unwrap_or_clone(last) {
        Message::User(user) => user,
        _ => return Err("the turn has no user message to answer".to_owned()),
    };
    // session_id is borrowed by the checkpoint closure and later moved into
    // the pump; give each its own copy.
    let session_id = session_id.to_owned();

    let context_window = model_context_window(state, &provider, model);
    // Codex-style checkpoint: 90% of the usable window, ~20k-token tail.
    // Also installed as a before-request hook so tool-heavy mid-turn
    // cycles re-estimate after each durable tool result.
    let compact_scope = crate::compaction::CompactScope {
        home,
        wire: &wire,
        model,
        session_id: &session_id,
        branch_id: branch.as_str(),
        head: &head_stamp_text,
        context_window,
    };
    let history = crate::compaction::compact_history(&compact_scope, history).await;

    let resources = mycode_config::discover_resources(home, &cwd);
    let mut system_prompt = String::from(
        "You are MYCode, a coding agent. Complete the user's request with the tools you have.",
    );
    for part in mycode_config::render_resource_prompt(&resources) {
        system_prompt.push_str(
            "

",
        );
        system_prompt.push_str(&part);
    }
    // Grok Build call pattern: a short index, then the model loads the
    // body or schema itself. Full skill text and MCP schemas stay off this
    // prompt.
    let user_home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from);
    let mut skills = mycode_config::discover_skills(&cwd, user_home.as_deref());
    let hidden_skills = skills.len().saturating_sub(32);
    skills.truncate(32);
    if let Some(mut catalog) = mycode_config::render_skill_catalog(&skills) {
        if hidden_skills > 0
            && let Some(close) = catalog.rfind("\n</skills>")
        {
            catalog.insert_str(
                close,
                &format!("\n- and {hidden_skills} more; read the matching SKILL.md by path"),
            );
        }
        system_prompt.push_str("\n\n");
        system_prompt.push_str(&catalog);
    }
    if let Some(catalog) = mcp_catalog.as_ref() {
        system_prompt.push_str("\n\n<mcp>\n");
        system_prompt.push_str(&catalog.prompt_note());
        system_prompt.push_str(
            "Built-in tools and direct MCP tools are called by name. For any other MCP tool, \
call `search_tool` with name \"list\", then with the exact name, then `use_tool`. Do not \
guess parameters. When the user also wants a subagent, emit `search_tool` in the same \
response as `task`.\n</mcp>",
        );
    }
    system_prompt.push_str(
        "\n\nFor current facts, call `web_search`, then `fetch_content` on the URLs you will cite. Snippets are not evidence.",
    );
    let extra_roots = workspace_extra_roots(home, &cwd);
    if !extra_roots.is_empty() {
        system_prompt.push_str("\n\nWorkspace folders besides the session cwd:\n");
        for root in &extra_roots {
            system_prompt.push_str(&format!("- {}\n", root.display()));
        }
        system_prompt.push_str(
            "Relative paths stay in the session cwd. For the other folders, pass an \
absolute path to `read`, `write`, `edit`, `find`, and `grep`. `shell` and \
`exec` start in the session cwd.",
        );
    }
    system_prompt.push_str("\n\n");
    system_prompt.push_str(&mycode_agent::build_system_prompt(&registry));
    let directive = crate::subagent::delegation_directive(&role_catalog, &settings.subagents);
    if !directive.is_empty() {
        system_prompt.push_str(&directive);
    }

    let turn_started = std::time::Instant::now();
    let (agent_tx, mut agent_rx) = tokio::sync::broadcast::channel(256);
    let compact_home = home.clone();
    let compact_wire = wire.clone();
    let compact_model = model.to_owned();
    let compact_session = session_id.clone();
    let compact_branch = branch.as_str().to_owned();
    let compact_head = head_stamp_text.clone();
    let hooks = HookRunner::default()
        .with_before_request(move |mut request| {
            let home = compact_home.clone();
            let wire = compact_wire.clone();
            let model = compact_model.clone();
            let session_id = compact_session.clone();
            let branch_id = compact_branch.clone();
            let head = compact_head.clone();
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
                request.messages =
                    crate::compaction::compact_history(&scope, request.messages).await;
                request
            }
        })
        .with_before_tool(checkpoint_hook(
            home.clone(),
            cwd.clone(),
            session_id.clone(),
            checkpoint_head,
        ));
    let cancel = CancellationToken::new();
    // Publish the token so an Escape-driven CancelChat can abort this turn;
    // the guard unpublishes it on every exit path.
    let _cancel_guard = CancelGuard::register(state.turn_cancels.clone(), &session_id, &cancel);
    let mut config = AgentConfig::new().with_system_prompt(system_prompt);
    if let Some(level) = settings.reasoning_effort.as_deref() {
        let Some(level) = mycode_core::ReasoningLevel::parse(level) else {
            return Err(
                "settings reasoningEffort must be a models.dev option (off, on, minimal, low, medium, high, xhigh, max)"
                    .to_owned(),
            );
        };
        config = config.with_reasoning(level);
    }
    let mut agent = Agent::new(config);

    // The ledger pump owns the branch head: tool results commit as they
    // complete, the final assistant message commits at turn end.
    let pump_events = events.clone();
    let pump_session_id = session_id.to_owned();
    let pump = tokio::spawn(async move {
        let mut pending_assistant: Option<std::sync::Arc<mycode_core::Message>> = None;
        let mut turn_usage = TurnUsage::default();
        loop {
            let event = match agent_rx.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                        session_id: pump_session_id.clone(),
                        message: "agent event stream lagged".to_owned(),
                    });
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
                        let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                            session_id: pump_session_id.clone(),
                            message: render_error(error),
                        });
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
                            let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                                session_id: pump_session_id.clone(),
                                message: render_error(error),
                            });
                            return;
                        }
                    }
                }
                mycode_core::events::AgentEvent::MessageAdded(message) => {
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
                        let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                            session_id: pump_session_id.clone(),
                            message: "assistant step could not be encoded".to_owned(),
                        });
                        return;
                    };
                    match writer.write(EventKind::Message, &payload).await {
                        Ok(event_id) => {
                            let _ = pump_events.try_send(BridgeEvent::AssistantStep {
                                session_id: pump_session_id.clone(),
                                entry: project_assistant_message(&event_id, assistant),
                            });
                        }
                        Err(error) => {
                            let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                                session_id: pump_session_id.clone(),
                                message: render_error(error),
                            });
                            return;
                        }
                    }
                }
                mycode_core::events::AgentEvent::TurnStarted => {}
                mycode_core::events::AgentEvent::TurnEnded(outcome) => {
                    let Some(message) = pending_assistant.take() else {
                        // A cancelled mid-stream turn commits nothing; the
                        // UI resets quietly on the sentinel message.
                        let message =
                            if matches!(outcome, mycode_core::events::TurnOutcome::Aborted) {
                                CHAT_CANCELLED.to_owned()
                            } else {
                                "the turn ended without an assistant message".to_owned()
                            };
                        let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                            session_id: pump_session_id.clone(),
                            message,
                        });
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
                                    if usage_enabled && turn_usage.seen {
                                        let elapsed_ms = turn_started.elapsed().as_millis() as u64;
                                        let usage_payload = serde_json::json!({
                                            "provider": usage_provider,
                                            "model": usage_model,
                                            "input": turn_usage.input,
                                            "output": turn_usage.output,
                                            "cache": turn_usage.cache,
                                            "elapsed_ms": elapsed_ms,
                                        });
                                        if let Ok(bytes) = serde_json::to_vec(&usage_payload)
                                            && let Ok(usage_event) =
                                                writer.write(EventKind::Usage, &bytes).await
                                        {
                                            let _ =
                                                pump_events.try_send(BridgeEvent::UsageRecorded {
                                                    session_id: pump_session_id.clone(),
                                                    provider: usage_provider.clone(),
                                                    model: usage_model.clone(),
                                                    input: turn_usage.input,
                                                    output: turn_usage.output,
                                                    cache: turn_usage.cache,
                                                    elapsed_ms,
                                                    entry: project_usage(&usage_event, &bytes),
                                                });
                                        }
                                    }
                                    // Sent after the trailing usage write so the
                                    // head the UI receives is the ledger's final
                                    // head; a stale head fails the next append's
                                    // compare-and-swap as "session unavailable".
                                    let _ = pump_events.try_send(BridgeEvent::ChatDone {
                                        session_id: pump_session_id.clone(),
                                        head: writer.head().await,
                                        entry,
                                    });
                                }
                                Err(error) => {
                                    let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                                        session_id: pump_session_id.clone(),
                                        message: render_error(error),
                                    });
                                }
                            }
                        }
                        Err(_) => {
                            let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                                session_id: pump_session_id.clone(),
                                message: "assistant message could not be encoded".to_owned(),
                            });
                        }
                    }
                    return;
                }
                mycode_core::events::AgentEvent::Error(error) => {
                    let _ = pump_events.try_send(BridgeEvent::ChatFailed {
                        session_id: pump_session_id.clone(),
                        message: format!("agent error: {error}"),
                    });
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
        .with_extra_roots(extra_roots);
    let prompt_message = Message::User(prompt);
    let outcome = agent.prompt(prompt_message, &env).await;
    let _ = outcome
        .as_ref()
        .map_err(|error| format!("turn failed: {error}"))?;
    // The pump emits ChatDone/ChatFailed; wait for it to finish draining.
    let _ = pump.await;
    Ok(())
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

/// Token usage summed across every response cycle of one turn.
///
/// Providers report usage per cycle, so a turn that calls tools reports
/// several times. `seen` distinguishes "no usage reported" from a genuine
/// zero, which keeps the ledger from recording a usage event the provider
/// never sent.
#[derive(Debug, Default)]
struct TurnUsage {
    seen: bool,
    input: u64,
    output: u64,
    cache: Option<u64>,
}

impl TurnUsage {
    fn fold(&mut self, usage: &mycode_core::Usage) {
        self.seen = true;
        self.input = self.input.saturating_add(usage.input_tokens);
        self.output = self.output.saturating_add(usage.output_tokens);
        if let Some(cache) = usage.cache_read_tokens {
            self.cache = Some(self.cache.unwrap_or_default().saturating_add(cache));
        }
    }
}
