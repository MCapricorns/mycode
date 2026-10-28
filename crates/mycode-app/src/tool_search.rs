//! `tool_search`: BM25 search over the MCP tools that have connected.
//!
//! The corpus grows one ready server at a time, so the index never contains
//! a tool that has not completed its handshake. Results are the exact names
//! `use_tool` accepts, with the full schema, so one search can replace
//! several `search_tool` round trips. See
//! `wiki/architecture/code-mode-engine.md` §5.3.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bm25::{Embedder, EmbedderBuilder, Language, Scorer};
use mycode_agent::{ToolCtx, ToolDyn, ToolError, ToolResult, ToolSpec, ToolStream};
use parking_lot::RwLock;
use serde::Serialize;
use serde_json::{Value, json};

use crate::mcp_tools::{DynamicMcpTool, McpCatalog};

/// Most tools one `tool_search` call returns.
pub(crate) const MAX_RESULTS: usize = 8;

/// Incrementally grown BM25 index over the connected MCP tools.
#[derive(Default)]
pub(crate) struct ToolSearchIndex {
    state: RwLock<IndexState>,
}

#[derive(Default)]
struct IndexState {
    /// Catalog key → the text the index scores.
    documents: BTreeMap<String, String>,
    embedder: Option<Embedder>,
    scorer: Scorer<String>,
}

impl ToolSearchIndex {
    /// An empty index.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Adds the ready tools of one server, refitting the corpus once.
    ///
    /// A whole server arrives at the same time, so the O(corpus) refit runs
    /// per server rather than per tool; a large server would otherwise pay
    /// O(tools × corpus) inside one registration.
    pub(crate) fn insert_many<'a, I>(&self, entries: I)
    where
        I: IntoIterator<Item = (&'a str, &'a DynamicMcpTool)>,
    {
        let mut state = self.state.write();
        let mut changed = false;
        for (key, tool) in entries {
            let text = format!(
                "{key} {} {} {} {} {}",
                identifier_words(key),
                tool.server_id(),
                identifier_words(tool.server_id()),
                tool.description(),
                tool.snippet()
            );
            state.documents.insert(key.to_owned(), text);
            changed = true;
        }
        if changed {
            state.rebuild();
        }
    }

    /// The keys best matching `query`, best first.
    pub(crate) fn search(&self, query: &str, limit: usize) -> Vec<String> {
        let state = self.state.read();
        let Some(embedder) = &state.embedder else {
            return Vec::new();
        };
        let mut scored = state.scorer.matches(&embedder.embed(query));
        // The scorer gathers matches from a HashSet and leaves score ties
        // unordered. Break ties before limiting so repeated searches return
        // the same tools and serialized order.
        scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });
        scored
            .into_iter()
            .filter(|hit| hit.score > 0.0)
            .take(limit)
            .map(|hit| hit.id)
            .collect()
    }

    /// Number of indexed tools.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.state.read().documents.len()
    }
}

/// Adds words from snake_case/camelCase identifiers to the search corpus.
/// Keep the original identifier beside these words so exact-name queries
/// continue to match the same catalog key.
fn identifier_words(identifier: &str) -> String {
    let mut words = String::with_capacity(identifier.len());
    let mut chars = identifier.chars().peekable();
    let mut previous: Option<char> = None;
    while let Some(ch) = chars.next() {
        if !ch.is_alphanumeric() {
            words.push(' ');
        } else {
            if ch.is_uppercase()
                && previous.is_some_and(|prev| {
                    prev.is_lowercase()
                        || prev.is_numeric()
                        || (prev.is_uppercase()
                            && chars.peek().is_some_and(|next| next.is_lowercase()))
                })
            {
                words.push(' ');
            }
            words.push(ch);
        }
        previous = Some(ch);
    }
    words
}

impl IndexState {
    /// Refits the average document length to the whole corpus and rebuilds
    /// the scorer. Servers arrive a few times per turn at most, so a full
    /// rebuild keeps `with_fit_to_corpus` faithful without a background job.
    fn rebuild(&mut self) {
        let corpus: Vec<&str> = self.documents.values().map(String::as_str).collect();
        let embedder = EmbedderBuilder::with_fit_to_corpus(Language::English, &corpus).build();
        let mut scorer = Scorer::<String>::new();
        for (key, text) in &self.documents {
            scorer.upsert(key, embedder.embed(text));
        }
        self.embedder = Some(embedder);
        self.scorer = scorer;
    }
}

/// One tool `tool_search` returned.
#[derive(Debug, Serialize)]
struct LoadableToolSpec {
    name: String,
    description: String,
    params_schema: Value,
}

/// Finds MCP tools by keyword and returns their full schemas.
pub(crate) struct ToolSearchTool {
    catalog: Arc<McpCatalog>,
    index: Arc<ToolSearchIndex>,
}

impl ToolSearchTool {
    pub(crate) fn new(catalog: Arc<McpCatalog>, index: Arc<ToolSearchIndex>) -> Self {
        Self { catalog, index }
    }
}

impl ToolDyn for ToolSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tool_search".to_owned(),
            description: "Search the connected MCP tools by keyword. Returns up to 8 tools with \
                          their full inputSchema; call one with use_tool. Prefer this over \
                          search_tool when you do not know the exact tool name."
                .to_owned(),
            params_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Keywords, e.g. \"github pull request\"."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS,
                              "description": "Maximum results (default 8)."}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Some(
            "tool_search: keyword search over connected MCP tools; returns up to 8 schemas for \
             use_tool.",
        )
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or_else(|| ToolError::InvalidArgs("query is required".to_owned()))?;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(MAX_RESULTS, |limit| limit as usize)
                .clamp(1, MAX_RESULTS);
            let mut results = Vec::new();
            for key in self.index.search(query, limit) {
                let Some((key, tool)) = self.catalog.get(&key) else {
                    continue;
                };
                results.push(LoadableToolSpec {
                    name: key.to_owned(),
                    description: tool.description().to_owned(),
                    params_schema: tool.schema().clone(),
                });
            }
            if results.is_empty() {
                let pending = self.catalog.connecting_hint();
                return Ok(ToolResult::text(match pending {
                    Some(hint) => format!("No MCP tool matches \"{query}\". {hint}"),
                    None => format!(
                        "No MCP tool matches \"{query}\". Call search_tool with name \"list\" to \
                         see every connected tool."
                    ),
                }));
            }
            let text = serde_json::to_string_pretty(&results)
                .map_err(|error| ToolError::Execution(format!("tool_search result: {error}")))?;
            Ok(ToolResult::text(text))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ToolSearchIndex;
    use crate::mcp_tools::stub_catalog_tool;

    #[test]
    fn identifier_words_are_searchable_without_description_help() {
        let index = ToolSearchIndex::new();
        let snake = stub_catalog_tool("docs", "fetch_issue", "Perform an action.");
        let camel = stub_catalog_tool("docs", "createPullRequest", "Perform an action.");
        index.insert_many([
            ("fetch_issue", snake.as_ref()),
            ("createPullRequest", camel.as_ref()),
        ]);
        assert_eq!(index.search("issue", 8), ["fetch_issue"]);
        assert_eq!(index.search("pull request", 8), ["createPullRequest"]);
        assert_eq!(index.search("fetch_issue", 8)[0], "fetch_issue");
    }

    #[test]
    fn equal_scores_have_stable_key_order() {
        let index = ToolSearchIndex::new();
        let tools: Vec<_> = (0..12)
            .map(|i| {
                let key = format!("tool{i:02}");
                let tool = stub_catalog_tool("docs", &key, "Common lookup.");
                (key, tool)
            })
            .collect();
        index.insert_many(
            tools
                .iter()
                .map(|(key, tool)| (key.as_str(), tool.as_ref())),
        );
        let expected: Vec<_> = tools.iter().take(8).map(|(key, _)| key.clone()).collect();
        assert_eq!(index.search("common", 8), expected);
    }

    #[test]
    fn an_empty_corpus_finds_nothing() {
        let index = ToolSearchIndex::new();
        assert!(index.search("github issue", 8).is_empty());
        assert_eq!(index.len(), 0);
    }
}
