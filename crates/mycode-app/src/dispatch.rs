//! The bridge command loop: receives [`crate::BridgeCommand`]s on the core
//! runtime, dispatches each to its owning module, and replies.

use std::sync::Arc;

use mycode_config::{HomeLayout, read_ui_state, replace_ui_state};

use crate::ledger::{
    delete_session, forget_session_bindings, inspect_summaries, open_conversation, recall_message,
    render_error, send_message,
};
use crate::mcp_tools::mcp_list_tools;
use crate::oauth::oauth_sign_in;
use crate::search::search_project_files;
use crate::settings_io::{load_settings, render_config_error, save_provider_key, save_settings};
use crate::state::{
    CoreState, UPDATE_USER_AGENT, refresh_catalog, set_project_dir, spawn_catalog_refresh,
    spawn_update_check,
};
use crate::tool_hosts::deliver_ask_answer;
use crate::turn::chat_turn;
use crate::{BridgeCommand, BridgeReply, CatalogInfo, WithReply, protocol::SessionSummary};

pub(crate) fn run_core(
    home: HomeLayout,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<WithReply>,
    events: crate::BridgeEventTx,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
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
        // The catalog cache is a multi-megabyte document; parse it off the
        // core thread so startup does not stall the command loop.
        let cached = tokio::task::spawn_blocking({
            let home = home.clone();
            move || mycode_providers::catalog::current(&home)
        })
        .await
        .unwrap_or_else(|_| mycode_providers::catalog::CachedCatalog {
            document: mycode_providers::catalog::bundled().clone(),
            fetched_at: 0,
            etag: None,
        });
        // `SessionService::new` spawns the actor on this runtime. Calling it
        // from `spawn_blocking` has no reactor and cannot start the worker.
        let state = CoreState::new(home.clone(), cached);
        let state = Arc::new(state);
        spawn_catalog_refresh(state.clone(), events.clone());
        spawn_update_check(state.clone(), events.clone());
        while let Some(with_reply) = commands.recv().await {
            match with_reply.command {
                BridgeCommand::ChatTurn {
                    session,
                    branch,
                    expected_head,
                    provider_id,
                    model,
                } => {
                    // One live turn per session. A second turn used to replace
                    // the cancel token without stopping the first, so both
                    // pumps appended to the same branch.
                    cancel_session_work(&state, session.as_str());
                    let task = chat_turn(
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
                    let key = format!("{session_id}:{call_id}");
                    let reply = with_reply.reply;
                    let cancels = state.subagent_cancels.clone();
                    tokio::spawn(async move {
                        let outcome = match cancels.lock() {
                            Ok(mut map) => match map.remove(&key) {
                                Some(token) => {
                                    token.cancel();
                                    Ok(())
                                }
                                None => Err("that subagent is not running".to_owned()),
                            },
                            Err(_) => Err("subagent registry locked".to_owned()),
                        };
                        let _ = reply.send(BridgeReply::SubagentCancelled(outcome));
                    });
                }
                BridgeCommand::CancelChat { session_id } => {
                    let reply = with_reply.reply;
                    let cancels = state.turn_cancels.clone();
                    tokio::spawn(async move {
                        let outcome = match cancels.lock() {
                            Ok(map) => match map.get(&session_id) {
                                Some(token) => {
                                    token.cancel();
                                    Ok(())
                                }
                                None => Err("no turn is running for this session".to_owned()),
                            },
                            Err(_) => Err("cancel registry locked".to_owned()),
                        };
                        let _ = reply.send(BridgeReply::ChatCancelled(outcome));
                    });
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
                BridgeCommand::CheckUpdate => {
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let _ = reply.send(BridgeReply::UpdateChecked(
                            crate::updates::latest_release(UPDATE_USER_AGENT).await,
                        ));
                    });
                }
                BridgeCommand::DownloadUpdate { offer } => {
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome =
                            crate::updates::download_update(UPDATE_USER_AGENT, &offer).await;
                        let _ = reply.send(BridgeReply::UpdateDownloaded(outcome));
                    });
                }
                BridgeCommand::StartOAuthSignIn {
                    provider_id,
                    models,
                } => {
                    let reply = with_reply.reply;
                    let task_state = state.clone();
                    let task_events = events.clone();
                    tokio::spawn(async move {
                        let outcome =
                            oauth_sign_in(task_state, task_events, provider_id, models).await;
                        let _ = reply.send(outcome);
                    });
                }
                command => {
                    // Handlers perform storage and config I/O; running them
                    // inline would serialize the command loop behind every
                    // await and every synchronous file operation.
                    let task_state = state.clone();
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome = handle(&task_state, &command).await;
                        let _ = reply.send(outcome);
                    });
                }
            }
        }
        state.service.clone().shutdown().await;
    });
}

fn error_reply(command: &BridgeCommand, message: &str) -> BridgeReply {
    let message = message.to_owned();
    match command {
        BridgeCommand::ListSessions => BridgeReply::Sessions(Err(message)),
        BridgeCommand::CreateSession => BridgeReply::Created(Err(message)),
        BridgeCommand::OpenSession(_) => BridgeReply::Conversation(Err(message)),
        BridgeCommand::SendMessage { .. } => BridgeReply::Sent(Err(message)),
        BridgeCommand::LoadSettings => BridgeReply::Settings(Err(message)),
        BridgeCommand::SaveSettings { .. } => BridgeReply::SettingsSaved(Err(message)),
        BridgeCommand::SaveProviderKey { .. } => BridgeReply::ProviderKeySaved(Err(message)),
        BridgeCommand::ChatTurn { .. } => BridgeReply::ChatStarted(Err(message)),
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
        BridgeCommand::AskAnswer { .. } => BridgeReply::AskAnswered(Err(message)),
        BridgeCommand::GetCatalog | BridgeCommand::RefreshCatalog => {
            BridgeReply::Catalog(Err(message))
        }
        BridgeCommand::LoadUiState => BridgeReply::UiState(Err(message)),
        BridgeCommand::SaveUiState { .. } => BridgeReply::UiStateSaved(Err(message)),
        BridgeCommand::SetProjectDir { .. } => BridgeReply::ProjectSet(Err(message)),
        BridgeCommand::CheckUpdate => BridgeReply::UpdateChecked(Err(message)),
        BridgeCommand::DownloadUpdate { .. } => BridgeReply::UpdateDownloaded(Err(message)),
        BridgeCommand::StartOAuthSignIn { .. } => BridgeReply::CopilotSignInStarted(Err(message)),
        BridgeCommand::CancelChat { .. } => BridgeReply::ChatCancelled(Err(message)),
        BridgeCommand::CancelSubagent { .. } => BridgeReply::SubagentCancelled(Err(message)),
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

/// Cancels one session's live turn and every subagent it started.
///
/// Subagent registry keys are `{session_id}:{call_id}`, so the prefix match
/// stops exactly that session's background work without touching others.
fn cancel_session_work(state: &CoreState, session_id: &str) {
    if let Ok(mut turns) = state.turn_cancels.lock()
        && let Some(token) = turns.remove(session_id)
    {
        token.cancel();
    }
    if let Ok(mut subagents) = state.subagent_cancels.lock() {
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
}

async fn handle(state: &CoreState, command: &BridgeCommand) -> BridgeReply {
    match command {
        BridgeCommand::ListSessions => BridgeReply::Sessions(
            inspect_summaries(&state.service, state.home.clone())
                .await
                .map_err(render_error),
        ),
        BridgeCommand::CreateSession => match state.service.create().await {
            Ok(created) => BridgeReply::Created(Ok(SessionSummary {
                session_id: created.session_id.as_str().to_owned(),
                root_branch_id: created.branch_id.as_str().to_owned(),
                title: String::new(),
                event_count: 0,
                active: true,
                corrupt: false,
            })),
            Err(error) => BridgeReply::Created(Err(render_error(error))),
        },
        BridgeCommand::OpenSession(session) => BridgeReply::Conversation(
            open_conversation(&state.service, session)
                .await
                .map_err(render_error),
        ),
        BridgeCommand::SendMessage {
            session,
            branch,
            expected_head,
            text,
        } => BridgeReply::Sent(
            send_message(&state.service, session, branch, expected_head, text)
                .await
                .map_err(render_error),
        ),
        BridgeCommand::LoadSettings => {
            let home = state.home.clone();
            BridgeReply::Settings(blocking(move || load_settings(&home)).await)
        }
        BridgeCommand::SaveSettings {
            expected_revision,
            settings,
        } => {
            let home = state.home.clone();
            let settings = settings.clone();
            let expected_revision = *expected_revision;
            BridgeReply::SettingsSaved(
                blocking(move || save_settings(&home, expected_revision, &settings)).await,
            )
        }
        BridgeCommand::SaveProviderKey {
            provider_id,
            api_key,
        } => {
            let home = state.home.clone();
            let provider_id = provider_id.clone();
            let api_key = api_key.clone();
            BridgeReply::ProviderKeySaved(
                blocking(move || save_provider_key(&home, &provider_id, &api_key)).await,
            )
        }
        BridgeCommand::StartOAuthSignIn { .. } => BridgeReply::CopilotSignInStarted(Err(
            "device sign-in runs as a concurrent task".to_owned(),
        )),
        BridgeCommand::ChatTurn { .. } => {
            BridgeReply::ChatStarted(Err("chat turns run as concurrent tasks".to_owned()))
        }
        BridgeCommand::CancelChat { .. } => {
            BridgeReply::ChatCancelled(Err("chat cancels run as concurrent tasks".to_owned()))
        }
        BridgeCommand::CancelSubagent { .. } => BridgeReply::SubagentCancelled(Err(
            "subagent cancels run as concurrent tasks".to_owned(),
        )),
        BridgeCommand::SearchProjectFiles { session_id, query } => {
            let root = state.project_dir(session_id);
            let query = query.clone();
            BridgeReply::ProjectFiles(
                blocking(move || Ok(search_project_files(&root, &query))).await,
            )
        }
        BridgeCommand::McpListTools { server } => BridgeReply::McpTools {
            server_id: server.id.clone(),
            outcome: mcp_list_tools(&state.home, server).await,
        },
        BridgeCommand::RecallMessage {
            session,
            branch,
            expected_head,
            to_event,
            edit,
        } => {
            let service = state.service.clone();
            let session = session.clone();
            let branch = branch.clone();
            let expected_head = expected_head.clone();
            let to_event = to_event.clone();
            let edit = edit.clone();
            let task = tokio::spawn(async move {
                recall_message(&service, &session, &branch, &expected_head, &to_event)
                    .await
                    .map(|conversation| (Box::new(conversation), edit))
            });
            match task.await {
                Ok(Ok(payload)) => BridgeReply::Recalled(Ok(payload)),
                Ok(Err(message)) => BridgeReply::Recalled(Err(message)),
                Err(error) => BridgeReply::Recalled(Err(error.to_string())),
            }
        }
        BridgeCommand::DeleteSession { session_id } => {
            // Stop the session's live work first, then evict the actor's
            // cached ledger: a turn that survives the directory delete would
            // recreate `pending/` on its next reservation and then fail
            // fatally on the missing branch log, taking the whole session
            // service down with it. The eviction is best-effort — a dead
            // actor cannot resurrect anything either — so the durable
            // delete always runs.
            cancel_session_work(state, session_id);
            if let Some(id) = mycode_agent::session::SessionId::parse(session_id) {
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
            if deleted.is_ok()
                && let Ok(mut projects) = state.projects.lock()
            {
                projects.remove(session_id.as_str());
            }
            BridgeReply::SessionDeleted(deleted)
        }
        BridgeCommand::RemoveRecent { project } => {
            let home = state.home.clone();
            let project = project.clone();
            BridgeReply::UiStateSaved(
                blocking(move || {
                    let mut ui_state =
                        read_ui_state(&home).map_err(|error| render_config_error(&error))?;
                    ui_state.remove_recent(&project);
                    replace_ui_state(&home, &ui_state).map_err(|error| render_config_error(&error))
                })
                .await,
            )
        }
        BridgeCommand::ExportData { path } => {
            let home = state.home.clone();
            let path = path.clone();
            let outcome =
                tokio::task::spawn_blocking(move || crate::export::export_to_file(&home, &path))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|outcome| outcome);
            BridgeReply::Exported(outcome)
        }
        BridgeCommand::ImportData { path } => {
            let home = state.home.clone();
            let path = path.clone();
            let outcome =
                tokio::task::spawn_blocking(move || crate::export::import_from_file(&home, &path))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|outcome| outcome);
            BridgeReply::Imported(outcome)
        }
        BridgeCommand::ListResources { session_id } => {
            let home = state.home.clone();
            let workspace = state.project_dir(session_id);
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
        } => BridgeReply::AskAnswered(deliver_ask_answer(session_id, answers.clone())),
        BridgeCommand::GetCatalog => BridgeReply::Catalog(Ok(state
            .catalog
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| CatalogInfo {
                document: Arc::new(mycode_providers::catalog::bundled().clone()),
                fetched_at: 0,
            }))),
        BridgeCommand::LoadUiState => {
            let home = state.home.clone();
            BridgeReply::UiState(
                blocking(move || read_ui_state(&home).map_err(|error| render_config_error(&error)))
                    .await,
            )
        }
        BridgeCommand::SaveUiState { state: ui_state } => {
            let home = state.home.clone();
            let ui_state = ui_state.clone();
            BridgeReply::UiStateSaved(
                blocking(move || {
                    replace_ui_state(&home, &ui_state).map_err(|error| render_config_error(&error))
                })
                .await,
            )
        }
        BridgeCommand::SetProjectDir { session_id, path } => {
            BridgeReply::ProjectSet(set_project_dir(state, session_id, path.as_deref()))
        }
        BridgeCommand::RefreshCatalog | BridgeCommand::CheckUpdate => {
            BridgeReply::UpdateChecked(Err("this request runs as a concurrent task".to_owned()))
        }
        BridgeCommand::DownloadUpdate { .. } => {
            BridgeReply::UpdateDownloaded(Err("downloads run as concurrent tasks".to_owned()))
        }
    }
}
