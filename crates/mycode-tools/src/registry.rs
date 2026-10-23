//! `ToolRegistry` — name-keyed store of type-erased tools. See `wiki/index.html`.
//!
//! Registration is **last-wins** per name: a later registration replaces an
//! earlier tool under the same name. Specs are
//! served sorted by tool name so provider requests serialize
//! deterministically.

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use mycode_providers::tool::ToolSpec;

use crate::tool::ToolDyn;

struct RegistryInner {
    tools: BTreeMap<String, Arc<dyn ToolDyn>>,
    /// Built under the same lock as `tools`, so a registration cannot
    /// publish a spec list that no longer matches the map.
    specs: Option<Arc<[ToolSpec]>>,
    /// Names the model may call directly. `None` exposes every registered
    /// tool; `Some` restricts `specs`/`prompt_entries`/`names`/`is_exposed`
    /// to the listed names — the rest stay callable only through
    /// `run_code`'s `call(name, args)`, which reads `tools` unfiltered.
    exposed: Option<BTreeSet<String>>,
}

/// Thread-safe registry of tools, keyed by name.
pub struct ToolRegistry {
    inner: RwLock<RegistryInner>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(RegistryInner {
                tools: BTreeMap::new(),
                specs: None,
                exposed: None,
            }),
        }
    }

    /// Register a tool. If a tool with the same name is already
    /// registered, the new one replaces it (last-wins — plugins may
    /// override builtins).
    pub fn register(&self, tool: Arc<dyn ToolDyn>) {
        let name = tool.spec().name;
        let mut inner = self.write();
        inner.tools.insert(name, tool);
        inner.specs = None;
    }

    /// Restrict the model-facing surface to `names`: `Request.tools`, the
    /// system-prompt list, and top-level dispatch see only those. Everything
    /// registered keeps dispatching through `run_code`'s `call(name, args)`.
    pub fn expose_only(&self, names: &[&str]) {
        let mut inner = self.write();
        inner.exposed = Some(names.iter().map(|name| (*name).to_owned()).collect());
        inner.specs = None;
    }

    /// Whether a top-level model call may name this tool.
    pub fn is_exposed(&self, name: &str) -> bool {
        self.read()
            .exposed
            .as_ref()
            .is_none_or(|exposed| exposed.contains(name))
    }

    /// The names a top-level call may use, sorted: the exposed set when one
    /// is configured, otherwise every registered tool.
    pub fn exposed_names(&self) -> Vec<String> {
        let inner = self.read();
        match &inner.exposed {
            Some(exposed) => exposed.iter().cloned().collect(),
            None => inner.tools.keys().cloned().collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolDyn>> {
        self.read().tools.get(name).cloned()
    }

    /// Specs of the exposed tools, sorted by tool name for stable provider
    /// serialization. Registered-but-hidden tools are left out.
    pub fn specs(&self) -> Arc<[ToolSpec]> {
        if let Some(cached) = self.read().specs.clone() {
            return cached;
        }
        let mut inner = self.write();
        if let Some(cached) = inner.specs.clone() {
            return cached;
        }
        let built: Arc<[ToolSpec]> = inner
            .tools
            .iter()
            .filter(|(name, _)| {
                inner
                    .exposed
                    .as_ref()
                    .is_none_or(|exposed| exposed.contains(*name))
            })
            .map(|(_, tool)| tool.spec())
            .collect();
        inner.specs = Some(Arc::clone(&built));
        built
    }

    /// Names and optional prompt snippets of the exposed tools, sorted by name.
    pub fn prompt_entries(&self) -> Vec<(String, Option<String>)> {
        let inner = self.read();
        inner
            .tools
            .iter()
            .filter(|(name, _)| {
                inner
                    .exposed
                    .as_ref()
                    .is_none_or(|exposed| exposed.contains(*name))
            })
            .map(|(name, tool)| {
                (
                    name.clone(),
                    tool.prompt_snippet_dyn().map(ToOwned::to_owned),
                )
            })
            .collect()
    }

    /// Names of all registered tools, sorted — including ones only `call()`
    /// reaches. Used for the unknown-tool hint and the MCP native snapshot.
    pub fn names(&self) -> Vec<String> {
        self.read().tools.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.read().tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().tools.is_empty()
    }

    fn read(&self) -> RwLockReadGuard<'_, RegistryInner> {
        self.inner.read()
    }

    fn write(&self) -> RwLockWriteGuard<'_, RegistryInner> {
        self.inner.write()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::ToolRegistry;

    #[test]
    fn specs_stay_sorted_and_byte_identical() {
        let registry = ToolRegistry::new();
        crate::builtin::register_builtins(&registry);
        let specs = registry.specs();
        let names: Vec<_> = specs.iter().map(|spec| spec.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let first = serde_json::to_string(&*specs).unwrap();
        let second = serde_json::to_string(&*registry.specs()).unwrap();
        assert_eq!(first, second);
    }
}
