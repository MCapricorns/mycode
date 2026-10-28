//! Inner code-mode calls retain their host identity across run_code/wait_code.
use mycode_app::CodeModeProgress;

use super::*;

pub(super) fn apply_progress(state: &mut WorkspaceState, message: &str) -> bool {
    let Some(event) = CodeModeProgress::parse(message) else {
        return false;
    };
    match event {
        CodeModeProgress::Started {
            call_id,
            name,
            target,
            ..
        } => {
            if job_finished(state, &call_id) {
                return true;
            }
            if name == AGENT_PROGRESS_PREFIX {
                let (role, goal) = agent_target_parts(&target);
                if !state.live_jobs.iter().any(|job| job.call_id == call_id) {
                    upsert_live_job(state, &call_id, role, "starting");
                }
                if let Some(job) = live_job_mut(state, &call_id) {
                    goal.clone_into(&mut job.label);
                }
                claim_provider_call_id(state, &call_id, call_id.clone());
                refresh_subagent_status(state);
            } else if state.live_jobs.is_empty() {
                set_streaming_status(
                    state,
                    &format!(
                        "{} {}",
                        t("Running", "正在执行"),
                        tool_call_label(&name, &target)
                    ),
                );
            }
        }
        CodeModeProgress::Progress {
            call_id,
            name,
            message,
        } => {
            // The outer ledger/provider ids never reach the child cancel map.
            tool_progress(state, call_id.clone(), call_id, name, message);
        }
        CodeModeProgress::Completed { call_id, .. } => {
            drop_live_job(state, &call_id);
            refresh_subagent_status(state);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_model::{ActiveConversation, FrontendAction};
    use mycode_app::InnerCallPhase;

    fn event(state: &mut WorkspaceState, outer: &str, event: CodeModeProgress) {
        crate::view_model::reduce(
            state,
            FrontendAction::ToolProgress {
                call_id: format!("ledger-{outer}"),
                provider_call_id: outer.into(),
                name: "run_code".into(),
                message: event.encode(),
            },
        );
    }

    #[test]
    fn yielded_child_progress_resumes_after_the_previous_turn_ended() {
        let mut state = WorkspaceState {
            sending: true,
            ..Default::default()
        };
        event(
            &mut state,
            "outer",
            CodeModeProgress::Started {
                call_id: "outer.0".into(),
                name: "agent".into(),
                seq: 0,
                target: "scout inspect source".into(),
            },
        );
        crate::view_model::reduce::turn_state::end_turn(&mut state);
        assert!(state.live_jobs.is_empty());
        assert!(state.job_elapsed("outer.0").is_some());
        state.sending = true;
        event(
            &mut state,
            "later-wait",
            CodeModeProgress::Progress {
                call_id: "outer.0".into(),
                name: "agent".into(),
                message: "agent|scout|tool|read src/lib.rs".into(),
            },
        );
        assert_eq!(state.live_jobs.len(), 1);
        assert_eq!(state.live_jobs[0].provider_call_id, "outer.0");
    }

    #[test]
    fn a_completion_seen_without_a_start_still_rejects_stale_progress() {
        let mut state = WorkspaceState {
            sending: true,
            ..Default::default()
        };
        event(
            &mut state,
            "wait",
            CodeModeProgress::Completed {
                call_id: "outer.0".into(),
                name: "agent".into(),
                phase: InnerCallPhase::Completed,
                is_error: false,
            },
        );
        event(
            &mut state,
            "outer",
            CodeModeProgress::Started {
                call_id: "outer.0".into(),
                name: "agent".into(),
                seq: 0,
                target: "scout inspect source".into(),
            },
        );
        event(
            &mut state,
            "outer",
            CodeModeProgress::Progress {
                call_id: "outer.0".into(),
                name: "agent".into(),
                message: "agent|scout|step|stale".into(),
            },
        );
        assert!(state.live_jobs.is_empty());
    }

    #[test]
    fn parallel_children_keep_independent_cancel_ids_across_waits_and_completion() {
        let mut state = WorkspaceState {
            sending: true,
            active: Some(ActiveConversation {
                session_id: "ses".into(),
                branch_id: "br".into(),
                head: "empty".into(),
                entries: Vec::new(),
                older_before: None,
                streaming: None,
                running: true,
                compacting: false,
                context_used: 0,
                context_cache: 0,
            }),
            ..Default::default()
        };
        for (seq, role) in [(0, "scout"), (1, "artisan")] {
            event(
                &mut state,
                "outer",
                CodeModeProgress::Started {
                    call_id: format!("outer.{seq}"),
                    name: "agent".into(),
                    seq,
                    target: format!("{role} inspect source"),
                },
            );
        }
        assert_eq!(state.live_jobs.len(), 2);
        assert_eq!(state.live_jobs[0].provider_call_id, "outer.0");
        assert_eq!(state.live_jobs[1].provider_call_id, "outer.1");
        event(
            &mut state,
            "wait-call",
            CodeModeProgress::Progress {
                call_id: "outer.1".into(),
                name: "agent".into(),
                message: "agent|artisan|tool|read src/lib.rs".into(),
            },
        );
        assert_eq!(state.live_jobs[1].provider_call_id, "outer.1");
        assert!(state.live_jobs[1].step.ends_with("read src/lib.rs"));
        event(
            &mut state,
            "wait-call",
            CodeModeProgress::Completed {
                call_id: "outer.0".into(),
                name: "agent".into(),
                phase: InnerCallPhase::Cancelled,
                is_error: true,
            },
        );
        assert_eq!(state.live_jobs.len(), 1);
        assert_eq!(state.live_jobs[0].call_id, "outer.1");
        // Queued progress must not resurrect the child that just stopped.
        event(
            &mut state,
            "outer",
            CodeModeProgress::Progress {
                call_id: "outer.0".into(),
                name: "agent".into(),
                message: "agent|scout|step|stale".into(),
            },
        );
        assert_eq!(state.live_jobs.len(), 1);
        event(
            &mut state,
            "wait-call",
            CodeModeProgress::Completed {
                call_id: "outer.1".into(),
                name: "agent".into(),
                phase: InnerCallPhase::Completed,
                is_error: false,
            },
        );
        assert!(state.live_jobs.is_empty());
        assert!(
            !state
                .active
                .unwrap()
                .streaming
                .unwrap()
                .status
                .contains("code_mode|")
        );
    }
}
