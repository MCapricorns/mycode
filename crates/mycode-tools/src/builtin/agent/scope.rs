//! Exact ownership claims coordinate writers; they do not restrict filesystem access.
use crate::tool::ToolError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentScope {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub symbols: Vec<AgentSymbolScope>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentSymbolScope {
    pub path: String,
    pub name: String,
}

impl AgentScope {
    pub(super) fn normalize(&self, cwd: &Path) -> Result<Self, ToolError> {
        let invalid = |message: &str| ToolError::InvalidArgs(message.into());
        let path = |value: &str| -> Result<String, ToolError> {
            let value = value.trim();
            if value.is_empty() || value.contains(['*', '?', '\0', '\n', '\r']) {
                return Err(invalid(
                    "scope requires exact paths without wildcards or control characters",
                ));
            }
            let input = Path::new(value);
            let mut normalized = PathBuf::new();
            for part in cwd.join(input).components() {
                match part {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        normalized.pop();
                    }
                    other => normalized.push(other.as_os_str()),
                }
            }
            let value = normalized.to_string_lossy().into_owned();
            Ok(if cfg!(windows) {
                value.to_lowercase()
            } else {
                value
            })
        };
        let mut out = Self::default();
        for value in &self.paths {
            let value = path(value)?;
            if out.paths.contains(&value) {
                return Err(invalid("duplicate scope path"));
            }
            out.paths.push(value);
        }
        for symbol in &self.symbols {
            let name = symbol.name.trim();
            if name.is_empty() || name.contains(['*', '?', '\0', '\n', '\r']) {
                return Err(invalid("scope symbol requires an exact name"));
            }
            let value = AgentSymbolScope {
                path: path(&symbol.path)?,
                name: name.into(),
            };
            if out.symbols.contains(&value) {
                return Err(invalid("duplicate scope symbol"));
            }
            out.symbols.push(value);
        }
        if out.paths.is_empty() && out.symbols.is_empty() {
            return Err(invalid("scope must contain paths or symbols"));
        }
        Ok(out)
    }
    pub(super) fn overlaps(&self, other: &Self) -> bool {
        let contains = |a: &str, b: &str| Path::new(b).starts_with(Path::new(a));
        self.paths.iter().any(|a| {
            other.paths.iter().any(|b| contains(a, b) || contains(b, a))
                || other.symbols.iter().any(|b| contains(a, &b.path))
        }) || other
            .paths
            .iter()
            .any(|a| self.symbols.iter().any(|b| contains(a, &b.path)))
            || self.symbols.iter().any(|a| other.symbols.contains(a))
    }
}

pub(super) fn validate_phase(phase: Option<&str>) -> Result<(), ToolError> {
    if let Some(p) = phase
        && (p.is_empty()
            || p.len() > 80
            || !p.as_bytes()[0].is_ascii_alphanumeric()
            || !p
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)))
    {
        return Err(ToolError::InvalidArgs(
            "phase_id must be 1-80 ASCII letters, digits, ._:-, starting with a letter or digit"
                .into(),
        ));
    }
    Ok(())
}
