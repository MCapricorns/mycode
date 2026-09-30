//! Recovery: manifest planning, torn-tail discard, and chunked replay
//! verification for one session load.
//!
//! [`SessionCore::recovery_plan`] reads the strict manifest and lays out
//! per-branch byte/count/head expectations; [`SessionCore::recover_tails`]
//! truncates torn log tails and removes orphan staged payloads; the verify
//! loop re-decodes every committed record within the per-pull byte budget
//! and rebuilds the in-memory ledger, carrying the open tool-call set into
//! each assembled branch so results of calls committed before a restart
//! still resolve.

use std::collections::HashSet;

use mycode_config::read_owned_file;

use super::super::digest::format_digest;
use super::super::dto::{EventKind, HeadStamp, SessionError};
use super::super::fs;
use super::super::ids::{BranchId, SessionCallId, SessionEventId, SessionId};
use super::super::ledger::{BranchLedger, EventMeta};
use super::super::store::{self, MAX_MANIFEST_BYTES, SessionPaths};
use super::{BranchPlan, LoadState, OpFail, PlanError, SessionCore, VERIFY_BUDGET_BYTES};

impl SessionCore {
    /// Reads the manifest and builds the per-branch recovery plan.
    pub(super) fn recovery_plan(&self, session: &SessionId) -> Result<Vec<BranchPlan>, PlanError> {
        let paths = SessionPaths::new(&self.home, session);
        let bytes = match read_owned_file(&self.home, paths.manifest(), MAX_MANIFEST_BYTES) {
            Ok(Some(bytes)) => bytes.to_vec(),
            Ok(None) => return Err(PlanError::NotFound),
            Err(_) => return Err(PlanError::Corrupt),
        };
        let manifest = store::decode_manifest(&bytes).map_err(|_| PlanError::Corrupt)?;
        if manifest.session_id != session.as_str() {
            return Err(PlanError::Corrupt);
        }
        let mut plan = Vec::with_capacity(manifest.branches.len());
        for branch in &manifest.branches {
            let branch_id = BranchId::parse(&branch.branch_id).ok_or(PlanError::Corrupt)?;
            plan.push(BranchPlan {
                branch: branch_id,
                committed_bytes: branch.committed_bytes,
                event_count: branch.event_count,
                head: store::decode_head(&branch.head).ok_or(PlanError::Corrupt)?,
            });
        }
        Ok(plan)
    }

    /// Discards torn tails and orphan staged payloads.
    pub(super) fn recover_tails(&self, load: &LoadState) -> Result<(), OpFail> {
        let paths = SessionPaths::new(&self.home, &load.session);
        for plan in &load.plan {
            let relative = paths.branch_events(&plan.branch);
            let absolute = paths.absolute(&relative).map_err(|_| OpFail::Storage)?;
            match fs::file_len(&absolute) {
                Ok(length) => {
                    if length > plan.committed_bytes {
                        fs::truncate(&absolute, plan.committed_bytes)
                            .map_err(|_| OpFail::Storage)?;
                    } else if length < plan.committed_bytes {
                        return Err(OpFail::Domain(SessionError::Corrupt));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if plan.committed_bytes > 0 {
                        return Err(OpFail::Domain(SessionError::Corrupt));
                    }
                }
                Err(_) => return Err(OpFail::Storage),
            }
        }
        // Staged payloads are orphaned by any restart: reservations are
        // actor-local, so every remaining staged file is unreferenced.
        if let Ok(absolute) = paths.absolute(&paths.pending_dir())
            && let Ok(names) = fs::list_files(&absolute)
        {
            for name in names {
                fs::remove(&absolute.join(name));
            }
        }
        Ok(())
    }

    /// Verifies within this pull's budget; returns `true` when the whole
    /// plan finished.
    pub(super) fn verify_budget(&mut self, load: &mut LoadState) -> Result<bool, OpFail> {
        let mut spent = 0_u64;
        while load.index < load.plan.len() && spent < VERIFY_BUDGET_BYTES {
            self.verify_one(load, VERIFY_BUDGET_BYTES - spent, &mut spent)?;
        }
        Ok(load.index == load.plan.len())
    }

    /// Verifies exactly one record of the current branch.
    pub(super) fn verify_one(
        &mut self,
        load: &mut LoadState,
        budget: u64,
        spent: &mut u64,
    ) -> Result<(), OpFail> {
        let plan = &load.plan[load.index];
        let paths = SessionPaths::new(&self.home, &load.session);
        let corrupt = || OpFail::Domain(SessionError::Corrupt);
        if plan.committed_bytes == 0 {
            // An empty branch has no log file at all.
            if plan.event_count != 0 || !plan.head.is_empty() || !load.events.is_empty() {
                return Err(corrupt());
            }
            let branch = BranchLedger {
                head: plan.head.clone(),
                committed_bytes: 0,
                open_calls: std::collections::HashSet::new(),
                events: Vec::new(),
            };
            load.assembled.push((plan.branch.clone(), branch));
            load.index += 1;
            load.offset = 0;
            return Ok(());
        }
        let absolute = paths
            .absolute(&paths.branch_events(&plan.branch))
            .map_err(|_| OpFail::Storage)?;
        let remaining = plan.committed_bytes - load.offset;
        // Never read past the committed prefix: a remaining length below one
        // frame header means the committed length is not record-aligned and
        // is corruption, not a reason to probe beyond the durable boundary.
        // The header may overshoot the byte budget by at most the frame
        // header so a budget-boundary pull can still parse one record.
        let window = remaining.min(budget.saturating_sub(*spent).max(4));
        // EOF inside the committed prefix means the durable log is shorter
        // than the manifest claims: corruption, not a substrate failure.
        let bytes = match fs::read_range(&absolute, load.offset, window) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(corrupt());
            }
            Err(_) => return Err(OpFail::Storage),
        };
        let decoded = match store::decode_record(&bytes) {
            Ok(decoded) => decoded,
            Err(store::RecordError::Incomplete) => {
                // A short read at this point means the committed prefix
                // itself ends mid-header: not record-aligned.
                if bytes.len() < 4 {
                    return Err(corrupt());
                }
                let total =
                    u64::from(u32::from_be_bytes(bytes[..4].try_into().expect("header"))) + 4;
                if total > remaining {
                    return Err(corrupt());
                }
                let full = match fs::read_range(&absolute, load.offset, total) {
                    Ok(full) => full,
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Err(corrupt());
                    }
                    Err(_) => return Err(OpFail::Storage),
                };
                store::decode_record(&full).map_err(|_| corrupt())?
            }
            Err(store::RecordError::Corrupt) => return Err(corrupt()),
        };
        let offset = load.offset;
        load.offset += decoded.record_len;
        *spent += decoded.record_len;

        let event_id = SessionEventId::parse(&decoded.event_id).ok_or_else(corrupt)?;
        let kind = EventKind::from_tag(decoded.kind).ok_or_else(corrupt)?;
        let call_id = decoded.call_id.as_deref().and_then(SessionCallId::parse);
        if matches!(kind, EventKind::ToolCall | EventKind::ToolResult) != call_id.is_some() {
            return Err(corrupt());
        }
        match kind {
            EventKind::ToolCall => {
                if load.events.iter().any(|row| row.call_id == call_id) {
                    return Err(corrupt());
                }
                load.open_calls
                    .push(call_id.clone().expect("tool call carries an id"));
            }
            EventKind::ToolResult => {
                let call = call_id.clone().expect("tool result carries an id");
                let position = load
                    .open_calls
                    .iter()
                    .position(|open| *open == call)
                    .ok_or_else(corrupt)?;
                load.open_calls.swap_remove(position);
            }
            EventKind::Message | EventKind::Usage | EventKind::Task => {}
        }
        load.events.push(EventMeta {
            digest: format_digest(&decoded.payload_digest),
            bytes: decoded.payload_len,
            event_id,
            kind,
            call_id,
            offset,
            record_len: decoded.record_len,
        });

        if load.offset == plan.committed_bytes {
            if load.events.len() as u64 != plan.event_count {
                return Err(corrupt());
            }
            match (&plan.head, load.events.last()) {
                (HeadStamp::Empty, None) => {}
                (HeadStamp::Event(head), Some(last)) if &last.event_id == head => {}
                _ => return Err(corrupt()),
            }
            let events = std::mem::take(&mut load.events);
            // Collect the open-calls set before the per-branch reset: the
            // set must survive into the assembled branch so a ToolResult
            // committed after a restart can still resolve its ToolCall.
            let open_calls = std::mem::take(&mut load.open_calls);
            let branch = BranchLedger {
                head: plan.head.clone(),
                committed_bytes: plan.committed_bytes,
                open_calls: open_calls.into_iter().collect::<HashSet<_>>(),
                events,
            };
            let branch_id = plan.branch.clone();
            load.assembled.push((branch_id, branch));
            load.index += 1;
            load.offset = 0;
        }
        Ok(())
    }
}
