//! `ToolRegistry` — name-keyed store of type-erased tools. See `docs/tools.md`.
//!
//! Registration is **last-wins** per name: a later registration replaces an
//! earlier tool under the same name. Specs are
//! served sorted by tool name so provider requests serialize
//! deterministically.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use mycode_core::tool::ToolSpec;

use crate::tool::ToolDyn;

struct RegistryInner {
    tools: BTreeMap<String, Arc<dyn ToolDyn>>,
    /// Built under the same lock as `tools`, so a registration cannot
    /// publish a spec list that no longer matches the map.
    specs: Option<Arc<[ToolSpec]>>,
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

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolDyn>> {
        self.read().tools.get(name).cloned()
    }

    /// Specs of all registered tools, sorted by tool name for stable
    /// provider serialization.
    pub fn specs(&self) -> Arc<[ToolSpec]> {
        if let Some(cached) = self.read().specs.clone() {
            return cached;
        }
        let mut inner = self.write();
        if let Some(cached) = inner.specs.clone() {
            return cached;
        }
        let built: Arc<[ToolSpec]> = inner.tools.values().map(|tool| tool.spec()).collect();
        inner.specs = Some(Arc::clone(&built));
        built
    }

    /// Names and optional prompt snippets of all registered tools, sorted by name.
    pub fn prompt_entries(&self) -> Vec<(String, Option<String>)> {
        self.read()
            .tools
            .iter()
            .map(|(name, tool)| {
                (
                    name.clone(),
                    tool.prompt_snippet_dyn().map(ToOwned::to_owned),
                )
            })
            .collect()
    }

    /// Names of all registered tools, sorted.
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
        self.inner.read().expect("tool registry lock poisoned")
    }

    fn write(&self) -> RwLockWriteGuard<'_, RegistryInner> {
        self.inner.write().expect("tool registry lock poisoned")
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
        sorted.sort();
        assert_eq!(names, sorted);
        let first = serde_json::to_string(&*specs).unwrap();
        let second = serde_json::to_string(&*registry.specs()).unwrap();
        assert_eq!(first, second);
    }
}
