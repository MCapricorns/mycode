//! Determinism record for one code-mode script.
//!
//! Every host-visible step is journalled as `(seq, kind, blake3(args),
//! blake3(result))`. Only the JSON projection the script sees is hashed
//! ([`crate::tools::ToolResult::content`]); UI-only `details` never enters the
//! journal. The journal is diagnostic data, not a checkpoint: `wait_code`
//! collects the same running cell without replaying its script.

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

/// Journal of one script run, ordered by submission sequence.
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
    /// The number comes from [`crate::tools::ScriptCell::next_seq`], not from the
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
        let index = self.entries.partition_point(|recorded| recorded.seq < seq);
        self.entries.insert(index, entry.clone());
        entry
    }

    /// Every recorded step, in order.
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
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

    #[test]
    fn object_key_order_does_not_change_the_hash() {
        let first = json!({"name": "read", "arguments": {"path": "a", "line": 1}});
        let second = json!({"arguments": {"line": 1, "path": "a"}, "name": "read"});
        assert_eq!(super::hash_json(&first), super::hash_json(&second));
        let other = json!({"name": "read", "arguments": {"path": "b"}});
        assert_ne!(super::hash_json(&first), super::hash_json(&other));
    }
}
