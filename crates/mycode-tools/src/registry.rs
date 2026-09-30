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

/// Thread-safe registry of tools, keyed by name.
pub struct ToolRegistry {
    tools: RwLock<BTreeMap<String, Arc<dyn ToolDyn>>>,
    /// Built once per registration. Provider requests reuse this instead of
    /// regenerating every JSON schema on each LLM round.
    specs: RwLock<Option<Arc<[ToolSpec]>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(BTreeMap::new()),
            specs: RwLock::new(None),
        }
    }

    /// Register a tool. If a tool with the same name is already
    /// registered, the new one replaces it (last-wins — plugins may
    /// override builtins).
    pub fn register(&self, tool: Arc<dyn ToolDyn>) {
        let name = tool.spec().name;
        self.write().insert(name, tool);
        *self.specs.write().expect("tool registry lock poisoned") = None;
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolDyn>> {
        self.read().get(name).cloned()
    }

    /// Specs of all registered tools, sorted by tool name for stable
    /// provider serialization.
    pub fn specs(&self) -> Arc<[ToolSpec]> {
        if let Some(cached) = self
            .specs
            .read()
            .expect("tool registry lock poisoned")
            .clone()
        {
            return cached;
        }
        let built: Arc<[ToolSpec]> = self.read().values().map(|tool| tool.spec()).collect();
        *self.specs.write().expect("tool registry lock poisoned") = Some(Arc::clone(&built));
        built
    }

    /// Names and optional prompt snippets of all registered tools, sorted by name.
    pub fn prompt_entries(&self) -> Vec<(String, Option<String>)> {
        self.read()
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
        self.read().keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    fn read(&self) -> RwLockReadGuard<'_, BTreeMap<String, Arc<dyn ToolDyn>>> {
        self.tools.read().expect("tool registry lock poisoned")
    }

    fn write(&self) -> RwLockWriteGuard<'_, BTreeMap<String, Arc<dyn ToolDyn>>> {
        self.tools.write().expect("tool registry lock poisoned")
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
