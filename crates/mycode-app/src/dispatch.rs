//! The bridge command loop: receives [`crate::BridgeCommand`]s on the core
//! runtime, dispatches each to its owning module, and replies.

use std::sync::Arc;

use mycode_config::{HomeLayout, read_ui_state, replace_ui_state, update_ui_state};

use crate::ledger::{
    delete_session, forget_session_bindings, inspect_summaries, load_older, open_conversation,
    recall_message, render_error, send_message_with,
};
use crate::mcp_tools::mcp_list_tools;
use crate::search::search_project_files;
use crate::settings_io::{load_settings, render_config_error, save_provider_key, save_settings};
use crate::state::{
    CoreState, UPDATE_USER_AGENT, catalog_info, refresh_catalog, set_project_dir,
    spawn_catalog_refresh, spawn_update_check,
};
use crate::tool_hosts::{deliver_ask_answer, dismiss_ask};
use crate::turn::{chat_turn, manual_compact};
use crate::{BridgeCommand, BridgeReply, WithReply, protocol::SessionSummary};

pub(crate) fn run_core(
    home: HomeLayout,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<WithReply>,
    events: crate::BridgeEventTx,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        // Cap the blocking pool: the default ceiling of 512 threads would
        // let a pathological burst (runaway file IO, stalled TLS calls) grow
        // process memory by hundreds of stacks. Real concurrency here is a
        // handful of config and catalog tasks.
        .max_blocking_threads(32)
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            // Fail every pending request and stop; the UI surfaces the loss.
            while let Some(with_reply) = commands.blocking_recv() {
                let _ = with_reply
                    .reply
                    .send(error_reply(&with_reply.command, "core runtime unavailable"));
            }
            return;
        }
    };
    runtime.block_on(async move {
        // The state lives behind one Arc: `SessionService` clones share the
        // actor and fence, and only the last drop retires the publication,
        // so per-turn tasks may hold clones freely.
        // Subagent worktree leases from a crashed process are recovered
        // before any new turn can run; the git subprocess calls are blocking,
        // so they run off the core thread.
        let recovery_home = home.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::subagent::recover_agent_worktrees(&recovery_home)
        })
        .await;
        // `SessionService::new` spawns the actor on this runtime. Calling it
        // from `spawn_blocking` has no reactor and cannot start the worker.
        let state = CoreState::new(home.clone(), Vec::new());
        let state = Arc::new(state);
        // The catalog refresh and update check stay queued until the desktop
        // reports its first frame (`UiReady`): reqwest + Schannel pull in
        // several megabytes and the refresh does not belong on the critical
        // startup path.
        let mut ui_ready = false;
        let mut requests = tokio::task::JoinSet::<()>::new();
        loop {
            let with_reply = tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => command,
                    None => break,
                },
                Some(outcome) = requests.join_next(), if !requests.is_empty() => {
                    if let Err(error) = outcome {
                        eprintln!("core command task failed: {error}");
                    }
                    continue;
                }
            };
            match with_reply.command {
                BridgeCommand::ChatTurn {
                    session,
                    branch,
                    provider_id,
                    model,
                    reasoning,
                } => {
                    // One live turn per session: a second turn would replace
                    // the cancel token without stopping the first, and both
                    // pumps would append to the same branch. Only the turn token
                    // is cancelled: subagents are children of that token and
                    // stop with it. Sweeping the subagent map here also
                    // stopped children the user meant to steer.
                    cancel_turn(&state, session.as_str());
                    let task = chat_turn(
                        state.clone(),
                        events.clone(),
                        session,
                        branch,
                        provider_id,
                        model,
                        reasoning,
                    );
                    tokio::spawn(task);
                    let _ = with_reply.reply.send(BridgeReply::ChatStarted(Ok(())));
                }
                BridgeCommand::CompactSession {
                    session,
                    branch,
                    expected_head,
                    provider_id,
                    model,
                } => {
                    cancel_turn(&state, session.as_str());
                    let task = manual_compact(
                        state.clone(),
                        events.clone(),
                        session,
                        branch,
                        expected_head,
                        provider_id,
                        model,
                    );
                    tokio::spawn(task);
                    let _ = with_reply.reply.send(BridgeReply::ChatStarted(Ok(())));
                }
                BridgeCommand::CancelSubagent {
                    session_id,
                    call_id,
                } => {
                    // A short synchronous map edit, like `Steer`; no task needed.
                    let key = format!("{session_id}:{call_id}");
                    let token = state.subagent_cancels.lock().remove(&key);
                    let outcome = match token {
                        Some(token) => {
                            token.cancel();
                            Ok(())
                        }
                        None => Err("that subagent is not running".to_owned()),
                    };
                    let _ = with_reply
                        .reply
                        .send(BridgeReply::SubagentCancelled(outcome));
                }
                BridgeCommand::SummarizeTitle {
                    session,
                    provider_id,
                    model,
                    seed,
                } => {
                    // Title work is a background nicety beside the real turn:
                    // no turn cancel, no ledger writes, no queue position.
                    let task = crate::title::summarize_title(
                        state.clone(),
                        events.clone(),
                        session,
                        provider_id,
                        model,
                        seed,
                    );
                    tokio::spawn(task);
                    let _ = with_reply.reply.send(BridgeReply::TitleSummarized(Ok(())));
                }
                BridgeCommand::Steer { session_id, text } => {
                    let inboxes = state.steer_inboxes.lock();
                    let outcome = match inboxes.get(&session_id) {
                        Some(inbox) => {
                            inbox.push(text);
                            Ok(())
                        }
                        None => Err("no turn is running for this session".to_owned()),
                    };
                    drop(inboxes);
                    let _ = with_reply.reply.send(BridgeReply::Steered(outcome));
                }
                BridgeCommand::CancelChat { session_id } => {
                    let token = state.turn_cancels.lock().get(&session_id).cloned();
                    let outcome = match token {
                        Some(token) => {
                            token.cancel();
                            Ok(())
                        }
                        None => Err("no turn is running for this session".to_owned()),
                    };
                    let _ = with_reply.reply.send(BridgeReply::ChatCancelled(outcome));
                }
                BridgeCommand::RefreshCatalog => {
                    let task_state = state.clone();
                    let task_events = events.clone();
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome = refresh_catalog(&task_state, &task_events, true).await;
                        let _ = reply.send(outcome);
                    });
                }
                BridgeCommand::UiReady => {
                    if !ui_ready {
                        ui_ready = true;
                        spawn_catalog_refresh(state.clone(), events.clone());
                        spawn_update_check(state.clone(), events.clone());
                    }
                    let _ = with_reply.reply.send(BridgeReply::UiReady(Ok(())));
                }
                BridgeCommand::CheckUpdate => {
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let _ = reply.send(BridgeReply::UpdateChecked(
                            crate::updates::latest_release(UPDATE_USER_AGENT).await,
                        ));
                    });
                }
                BridgeCommand::TestProxy { proxy } => {
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome = match mycode_config::ProxyPreference::parse(&proxy) {
                            Some(preference) => Ok(mycode_providers::test_proxy(&preference).await),
                            None => {
                                Err("not a proxy URL: http, https, socks5, or socks5h".to_owned())
                            }
                        };
                        let _ = reply.send(BridgeReply::ProxyTested(outcome));
                    });
                }
                BridgeCommand::DownloadUpdate { offer } => {
                    let reply = with_reply.reply;
                    let task_state = state.clone();
                    let task_events = events.clone();
                    tokio::spawn(async move {
                        let outcome =
                            crate::updates::download_update(UPDATE_USER_AGENT, &offer).await;
                        let outcome = match outcome {
                            Err(error) => Err(error),
                            // Dev-tree runs never persist a pending payload:
                            // a binary under `target/` must not self-swap.
                            Ok(prepared) if crate::install::install_skipped() => Ok(prepared),
                            Ok(prepared) => {
                                let home = task_state.home.clone();
                                let staged = prepared.clone();
                                let persisted = tokio::task::spawn_blocking(move || {
                                    crate::updates::persist_pending_update(&home, &staged)
                                })
                                .await;
                                match persisted {
                                    Ok(Ok(persisted)) => Ok(persisted),
                                    // A failed durable copy still yields the
                                    // verified temp stage; a crash then loses
                                    // the swap, matching a non-persisted stage.
                                    Ok(Err(_)) | Err(_) => Ok(prepared),
                                }
                            }
                        };
                        let staged = outcome.is_ok();
                        let _ = reply.send(BridgeReply::UpdateDownloaded(outcome));
                        if staged {
                            // A staged update may change how the gateway is
                            // reached; read its model list again once.
                            let _ = refresh_catalog(&task_state, &task_events, true).await;
                        }
                    });
                }
                command => {
                    // Handlers perform storage and config I/O; running them
                    // inline would serialize the command loop behind every
                    // await and every synchronous file operation.
                    let task_state = state.clone();
                    let reply = with_reply.reply;
                    requests.spawn(async move {
                        let outcome = handle(&task_state, command).await;
                        let _ = reply.send(outcome);
                    });
                }
            }
        }
        // Accepted storage/config requests must finish before the shared
        // service retires. Runtime drop would otherwise abort queued writes
        // merely because the frontend stopped waiting for their replies.
        while let Some(outcome) = requests.join_next().await {
            if let Err(error) = outcome {
                eprintln!("core command task failed: {error}");
            }
        }
        state.service.clone().shutdown().await;
    });
}

fn push_unique_repair(
    repairs: &mut Vec<mycode_config::DocumentRepair>,
    repair: mycode_config::DocumentRepair,
) {
    if repairs.iter().any(|existing| existing.path == repair.path) {
        return;
    }
    repairs.push(repair);
}

pub(crate) fn error_reply(command: &BridgeCommand, message: &str) -> BridgeReply {
    let message = message.to_owned();
    match command {
        BridgeCommand::ListSessions => BridgeReply::Sessions(Err(message)),
        BridgeCommand::CreateSession { .. } => BridgeReply::Created(Err(message)),
        BridgeCommand::OpenSession(_) => BridgeReply::Conversation(Err(message)),
        BridgeCommand::LoadOlder { .. } => BridgeReply::Older(Err(message)),
        BridgeCommand::SendMessage { session, text, .. } => BridgeReply::Sent {
            session: session.to_string(),
            text: text.clone(),
            outcome: Err(message),
        },
        BridgeCommand::LoadSettings => BridgeReply::Settings(Err(message)),
        BridgeCommand::SaveSettings { .. } => BridgeReply::SettingsSaved(Err(message)),
        BridgeCommand::SaveProviderKey { .. } => BridgeReply::ProviderKeySaved(Err(message)),
        BridgeCommand::ChatTurn { .. } | BridgeCommand::CompactSession { .. } => {
            BridgeReply::ChatStarted(Err(message))
        }
        BridgeCommand::SearchProjectFiles { .. } => BridgeReply::ProjectFiles(Err(message)),
        BridgeCommand::McpListTools { server } => BridgeReply::McpTools {
            server_id: server.id.clone(),
            outcome: Err(message),
        },
        BridgeCommand::RecallMessage { .. } => BridgeReply::Recalled(Err(message)),
        BridgeCommand::DeleteSession { .. } => BridgeReply::SessionDeleted(Err(message)),
        BridgeCommand::RemoveRecent { .. } => BridgeReply::UiStateSaved(Err(message)),
        BridgeCommand::ExportData { .. } => BridgeReply::Exported(Err(message)),
        BridgeCommand::ImportData { .. } => BridgeReply::Imported(Err(message)),
        BridgeCommand::ListResources { .. } => BridgeReply::Resources(Err(message)),
        BridgeCommand::AskAnswer { .. } | BridgeCommand::AskDismiss { .. } => {
            BridgeReply::AskAnswered(Err(message))
        }
        BridgeCommand::GetCatalog | BridgeCommand::RefreshCatalog => {
            BridgeReply::Catalog(Err(message))
        }
        BridgeCommand::LoadUiState => BridgeReply::UiState(Err(message)),
        BridgeCommand::SaveUiState { .. } => BridgeReply::UiStateSaved(Err(message)),
        BridgeCommand::SetProjectDir { .. } => BridgeReply::ProjectSet(Err(message)),
        BridgeCommand::UiReady => BridgeReply::UiReady(Err(message)),
        BridgeCommand::CheckUpdate => BridgeReply::UpdateChecked(Err(message)),
        BridgeCommand::DownloadUpdate { .. } => BridgeReply::UpdateDownloaded(Err(message)),
        BridgeCommand::TestProxy { .. } => BridgeReply::ProxyTested(Err(message)),
        BridgeCommand::CancelChat { .. } => BridgeReply::ChatCancelled(Err(message)),
        BridgeCommand::CancelSubagent { .. } => BridgeReply::SubagentCancelled(Err(message)),
        BridgeCommand::Steer { .. } => BridgeReply::Steered(Err(message)),
        BridgeCommand::SummarizeTitle { .. } => BridgeReply::TitleSummarized(Err(message)),
    }
}

/// Runs one synchronous storage/config step off the core runtime thread.
/// File locks, staged writes, and directory walks must never run inline:
/// the bridge runtime is single-threaded, so any blocking syscall freezes
/// every queued command and in-flight turn.
async fn blocking<T: Send + 'static>(
    step: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(step)
        .await
        .map_err(|error| format!("background task failed: {error}"))?
}

/// Cancels and removes one session's live turn token.
///
/// Subagent tokens that are children of this token stop with it. The
/// subagent map is left alone so a follow-up turn does not cancel work
/// the user is steering.
fn cancel_turn(state: &CoreState, session_id: &str) {
    let mut turns = state.turn_cancels.lock();
    if let Some(token) = turns.remove(session_id) {
        token.cancel();
    }
}

/// Cancels one session's live turn and every subagent it started.
///
/// Subagent registry keys are `{session_id}:{call_id}`, so the prefix match
/// stops exactly that session's background work without touching others.
/// Session deletion uses this. A new chat turn uses [`cancel_turn`] only.
fn cancel_session_work(state: &CoreState, session_id: &str) {
    cancel_turn(state, session_id);
    let mut subagents = state.subagent_cancels.lock();
    let prefix = format!("{session_id}:");
    subagents.retain(|key, token| {
        if key.starts_with(&prefix) {
            token.cancel();
            false
        } else {
            true
        }
    });
}

async fn handle(state: &CoreState, command: BridgeCommand) -> BridgeReply {
    match command {
        BridgeCommand::ListSessions => BridgeReply::Sessions(
            inspect_summaries(state.home.clone())
                .await
                .map_err(render_error),
        ),
        BridgeCommand::CreateSession { project } => {
            match state.service.create(project.as_deref()).await {
                Ok(created) => BridgeReply::Created(Ok(SessionSummary {
                    session_id: created.session_id.as_str().to_owned(),
                    root_branch_id: created.branch_id.as_str().to_owned(),
                    title: String::new(),
                    updated_at_ms: u64::try_from(crate::unix_now().as_millis())
                        .ok()
                        .filter(|&ms| ms > 0),
                    event_count: 0,
                    active: true,
                    corrupt: false,
                })),
                Err(error) => BridgeReply::Created(Err(render_error(error))),
            }
        }
        BridgeCommand::OpenSession(session) => BridgeReply::Conversation(
            open_conversation(&state.service, &session)
                .await
                .map(|mut conversation| {
                    // A reopen can land while the session's turn or
                    // `/compact` is still in flight; the registry only holds
                    // live work, and drops it before the work reports its end.
                    let live = state.live_work(session.as_str());
                    conversation.running = live == Some(crate::state::WorkKind::Turn);
                    conversation.compacting = live == Some(crate::state::WorkKind::Compaction);
                    conversation
                })
                .map_err(render_error),
        ),
        BridgeCommand::LoadOlder {
            session,
            branch,
            expected_head,
            before,
        } => BridgeReply::Older(
            load_older(&state.service, &session, &branch, &expected_head, &before).await,
        ),
        BridgeCommand::SendMessage {
            session,
            branch,
            expected_head,
            text,
            attachments,
        } => {
            let outcome = send_message_with(
                &state.service,
                &session,
                &branch,
                &expected_head,
                &text,
                &attachments,
            )
            .await
            .map_err(render_error);
            BridgeReply::Sent {
                session: session.to_string(),
                text,
                outcome,
            }
        }
        BridgeCommand::LoadSettings => {
            let home = state.home.clone();
            let pending = state.startup_repairs_snapshot();
            BridgeReply::Settings(
                blocking(move || load_settings(&home))
                    .await
                    .map(|mut loaded| {
                        for repair in pending {
                            push_unique_repair(&mut loaded.repairs, repair);
                        }
                        crate::SettingsSnapshot {
                            settings: loaded.settings,
                            revision: loaded.revision,
                            provider_keys: loaded.provider_keys,
                            mcp_keys: loaded.mcp_keys,
                            repairs: loaded.repairs,
                        }
                    }),
            )
        }
        BridgeCommand::SaveSettings {
            expected_revision,
            settings,
        } => {
            let home = state.home.clone();
            BridgeReply::SettingsSaved(
                blocking(move || save_settings(&home, expected_revision, &settings)).await,
            )
        }
        BridgeCommand::SaveProviderKey {
            provider_id,
            api_key,
        } => {
            let home = state.home.clone();
            BridgeReply::ProviderKeySaved(
                blocking(move || save_provider_key(&home, &provider_id, &api_key)).await,
            )
        }
        BridgeCommand::ChatTurn { .. } | BridgeCommand::CompactSession { .. } => {
            BridgeReply::ChatStarted(Err("chat turns run as concurrent tasks".to_owned()))
        }
        BridgeCommand::CancelChat { .. } => {
            BridgeReply::ChatCancelled(Err("chat cancels run as concurrent tasks".to_owned()))
        }
        BridgeCommand::Steer { .. } => {
            BridgeReply::Steered(Err("steers are applied on the core thread".to_owned()))
        }
        BridgeCommand::SummarizeTitle { .. } => {
            BridgeReply::TitleSummarized(Err("title summaries run as concurrent tasks".to_owned()))
        }
        BridgeCommand::CancelSubagent { .. } => BridgeReply::SubagentCancelled(Err(
            "subagent cancels run as concurrent tasks".to_owned(),
        )),
        BridgeCommand::SearchProjectFiles { session_id, query } => {
            let root = state.project_dir(&session_id);
            BridgeReply::ProjectFiles(
                blocking(move || Ok(search_project_files(&root, &query))).await,
            )
        }
        BridgeCommand::McpListTools { server } => BridgeReply::McpTools {
            server_id: server.id.clone(),
            outcome: mcp_list_tools(&state.home, &server).await,
        },
        BridgeCommand::RecallMessage {
            session,
            branch,
            expected_head,
            to_event,
            edit,
        } => {
            // `handle` already runs as its own task; an inner spawn only
            // added a JoinError layer the match below then unwrapped.
            match recall_message(
                &state.home,
                &state.service,
                &session,
                &branch,
                &expected_head,
                &to_event,
            )
            .await
            {
                Ok(conversation) => BridgeReply::Recalled(Ok((Box::new(conversation), edit))),
                Err(message) => BridgeReply::Recalled(Err(message)),
            }
        }
        BridgeCommand::DeleteSession { session_id } => {
            // Stop the session's live work first, then evict the actor's
            // cached ledger: a turn that survives the delete would append
            // into a session the index no longer names. The eviction is
            // best-effort — a dead actor cannot resurrect anything either —
            // so the durable delete always runs.
            cancel_session_work(state, &session_id);
            state.forget_token_scale(&session_id);
            if let Some(id) = mycode_agent::session::SessionId::parse(&session_id) {
                let _ = state.service.forget(&id).await;
            }
            let home = state.home.clone();
            let forgotten = session_id.clone();
            let deleted = blocking(move || {
                delete_session(&home, &forgotten)?;
                forget_session_bindings(&home, &forgotten)
            })
            .await;
            // Drop the in-memory project binding only after the directory is
            // gone. Leaving it made tool cwd and file search keep using a
            // session the user had already deleted.
            if deleted.is_ok() {
                state.projects.lock().remove(session_id.as_str());
            }
            BridgeReply::SessionDeleted(deleted)
        }
        BridgeCommand::RemoveRecent { project } => {
            let home = state.home.clone();
            BridgeReply::UiStateSaved(
                blocking(move || {
                    // Repair first; the locked edit rejects a damaged file.
                    read_ui_state(&home).map_err(|error| render_config_error(&error))?;
                    update_ui_state(&home, |ui_state| ui_state.remove_recent(&project))
                        .map_err(|error| render_config_error(&error))
                })
                .await,
            )
        }
        BridgeCommand::ExportData { path } => {
            let home = state.home.clone();
            let outcome = blocking(move || crate::export::export_to_file(&home, &path)).await;
            BridgeReply::Exported(outcome)
        }
        BridgeCommand::ImportData {
            path,
            confirm_mcp_commands,
        } => {
            let home = state.home.clone();
            let outcome = blocking(move || {
                crate::export::import_from_file(&home, &path, confirm_mcp_commands)
            })
            .await;
            BridgeReply::Imported(outcome)
        }
        BridgeCommand::ListResources { session_id } => {
            let home = state.home.clone();
            let workspace = state.project_dir(&session_id);
            BridgeReply::Resources(
                blocking(move || {
                    let files = mycode_config::discover_resources(&home, &workspace);
                    Ok(files
                        .iter()
                        .map(|file| {
                            (
                                file.name.clone(),
                                file.path.as_os_str().to_string_lossy().into_owned(),
                            )
                        })
                        .collect())
                })
                .await,
            )
        }
        BridgeCommand::AskAnswer {
            session_id,
            answers,
        } => BridgeReply::AskAnswered(deliver_ask_answer(&session_id, answers)),
        BridgeCommand::AskDismiss { session_id } => {
            BridgeReply::AskAnswered(dismiss_ask(&session_id))
        }
        BridgeCommand::GetCatalog => BridgeReply::Catalog(Ok(catalog_info(state))),
        BridgeCommand::LoadUiState => {
            let home = state.home.clone();
            let pending = state.startup_repairs_snapshot();
            BridgeReply::UiState(
                blocking(move || {
                    let (ui_state, repair) = mycode_config::read_ui_state_with_repair(&home)
                        .map_err(|error| render_config_error(&error))?;
                    let mut repairs = pending;
                    if let Some(repair) = repair {
                        push_unique_repair(&mut repairs, repair);
                    }
                    Ok((ui_state, repairs))
                })
                .await,
            )
        }
        BridgeCommand::SaveUiState { state: ui_state } => {
            let home = state.home.clone();
            // Held across the write: snapshots replace the whole document,
            // so they must land in dispatch order.
            let _ordered = state.ui_state_writes.lock().await;
            BridgeReply::UiStateSaved(
                blocking(move || {
                    replace_ui_state(&home, &ui_state).map_err(|error| render_config_error(&error))
                })
                .await,
            )
        }
        BridgeCommand::SetProjectDir { session_id, path } => {
            BridgeReply::ProjectSet(set_project_dir(state, &session_id, path.as_deref()))
        }
        BridgeCommand::RefreshCatalog => {
            BridgeReply::Catalog(Err("this request runs as a concurrent task".to_owned()))
        }
        BridgeCommand::UiReady => BridgeReply::UiReady(Err(
            "the UI-ready signal is handled on the command loop".to_owned(),
        )),
        BridgeCommand::CheckUpdate => {
            BridgeReply::UpdateChecked(Err("this request runs as a concurrent task".to_owned()))
        }
        BridgeCommand::DownloadUpdate { .. } => {
            BridgeReply::UpdateDownloaded(Err("downloads run as concurrent tasks".to_owned()))
        }
        BridgeCommand::TestProxy { .. } => {
            BridgeReply::ProxyTested(Err("this request runs as a concurrent task".to_owned()))
        }
    }
}
