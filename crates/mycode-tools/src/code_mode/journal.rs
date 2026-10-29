//! Determinism record for one code-mode script.
//!
//! Every host-visible step is journalled as `(seq, kind, blake3(args),
//! blake3(result))`. The result hash is part of the record on purpose: a
//! replayed script must be able to notice "same call, different result", not
//! only "different call". Only the JSON projection the script sees is hashed
//! ([`crate::ToolResult::content`]); UI-only `details` never enters the
//! journal. See `wiki/architecture/code-mode-engine.md` §3.4.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What one journalled step was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptEventKind {
    /// A `call(name, args)` host invocation.
    Call,
    /// A `log(text)` line.
    Log,
    /// A `complete(value)` call.
    Complete,
    /// A `yield_control()` call.
    Yield,
}

/// One journalled script step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Zero-based position in the script's host-call order.
    pub seq: u64,
    /// What happened.
    pub kind: ScriptEventKind,
    /// blake3 of the canonical JSON arguments.
    pub args_hash: String,
    /// blake3 of the canonical JSON result; `None` when the step has no
    /// result (log/yield).
    pub result_hash: Option<String>,
}

/// Append-only journal of one script run.
#[derive(Debug, Default, Clone)]
pub struct ScriptJournal {
    entries: Vec<JournalEntry>,
}

impl ScriptJournal {
    /// An empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one step at the sequence number the script reserved for it.
    ///
    /// The number comes from [`crate::ScriptCell::next_seq`], not from the
    /// entry count: a rejected `call` still consumes a script-side number,
    /// and a journal whose `seq` was its position would hide that gap.
    pub fn record(
        &mut self,
        seq: u64,
        kind: ScriptEventKind,
        args: &Value,
        result: Option<&Value>,
    ) -> JournalEntry {
        let entry = JournalEntry {
            seq,
            kind,
            args_hash: hash_json(args),
            result_hash: result.map(hash_json),
        };
        self.entries.push(entry.clone());
        entry
    }

    /// Every recorded step, in order.
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Checks this run against an earlier journal from the same script.
    ///
    /// A divergence — a call the recording did not make, a different
    /// argument set, or the same call returning a different result — fails
    /// the resume instead of silently replaying different work.
    ///
    /// # Errors
    ///
    /// Returns the first divergence as a message naming its sequence.
    pub fn verify_replay(&self, recorded: &[JournalEntry]) -> Result<(), String> {
        for (index, replay) in recorded.iter().enumerate() {
            let Some(actual) = self.entries.get(index) else {
                return Err(format!(
                    "journal divergence at seq {index}: the resume recorded a \
                     {:?} step the script did not make",
                    replay.kind
                ));
            };
            if actual.seq != replay.seq || actual.kind != replay.kind {
                return Err(format!(
                    "journal divergence at seq {index}: recorded {:?}, replayed {:?}",
                    replay.kind, actual.kind
                ));
            }
            if actual.args_hash != replay.args_hash {
                return Err(format!(
                    "journal divergence at seq {index}: arguments changed"
                ));
            }
            if actual.result_hash != replay.result_hash {
                return Err(format!(
                    "journal divergence at seq {index}: the same call returned a different result"
                ));
            }
        }
        if self.entries.len() != recorded.len() {
            return Err(format!(
                "journal divergence: recorded {} steps, replayed {}",
                recorded.len(),
                self.entries.len()
            ));
        }
        Ok(())
    }
}

/// blake3 of a value's canonical JSON form.
///
/// Object keys are sorted at every level, because JavaScript objects and serde maps
/// do not promise one insertion order; hashing them as-is would flag a
/// deterministic replay as drift.
#[must_use]
pub fn hash_json(value: &Value) -> String {
    let mut text = String::new();
    canonical_json(value, &mut text);
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("a key always serializes"));
                out.push(':');
                canonical_json(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("JSON values serialize")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ScriptEventKind, ScriptJournal};

    #[test]
    fn object_key_order_does_not_change_the_hash() {
        let first = json!({"name": "read", "arguments": {"path": "a", "line": 1}});
        let second = json!({"arguments": {"line": 1, "path": "a"}, "name": "read"});
        assert_eq!(super::hash_json(&first), super::hash_json(&second));
        let other = json!({"name": "read", "arguments": {"path": "b"}});
        assert_ne!(super::hash_json(&first), super::hash_json(&other));
    }

    #[test]
    fn a_replayed_journal_checks_both_hashes() {
        let args = json!({"name": "read", "arguments": {"path": "a"}});
        let result = json!({"content": [{"Text": "alpha"}], "is_error": false});
        let mut recorded = ScriptJournal::new();
        recorded.record(0, ScriptEventKind::Call, &args, Some(&result));

        let mut same = ScriptJournal::new();
        same.record(0, ScriptEventKind::Call, &args, Some(&result));
        assert!(same.verify_replay(recorded.entries()).is_ok());

        // Same arguments, different result: the drift only the result hash
        // can see.
        let mut drifted = ScriptJournal::new();
        drifted.record(
            0,
            ScriptEventKind::Call,
            &args,
            Some(&json!({"content": [{"Text": "beta"}], "is_error": false})),
        );
        let error = drifted.verify_replay(recorded.entries()).unwrap_err();
        assert!(error.contains("different result"), "{error}");

        // Same result shape, different arguments.
        let mut changed = ScriptJournal::new();
        changed.record(
            0,
            ScriptEventKind::Call,
            &json!({"name": "read", "arguments": {"path": "b"}}),
            Some(&result),
        );
        let error = changed.verify_replay(recorded.entries()).unwrap_err();
        assert!(error.contains("arguments changed"), "{error}");

        // A shorter replay diverges on length.
        let empty = ScriptJournal::new();
        let error = empty.verify_replay(recorded.entries()).unwrap_err();
        assert!(error.contains("the script did not make"), "{error}");
        // A longer replay diverges on count.
        let mut longer = ScriptJournal::new();
        longer.record(0, ScriptEventKind::Call, &args, Some(&result));
        longer.record(1, ScriptEventKind::Log, &json!("x"), None);
        let error = longer.verify_replay(recorded.entries()).unwrap_err();
        assert!(error.contains("recorded 1 steps, replayed 2"), "{error}");
    }
}
