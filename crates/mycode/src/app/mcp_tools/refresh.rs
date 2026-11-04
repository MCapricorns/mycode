//! Request-boundary refresh for a turn's progressive MCP catalog.

use super::*;
use crate::config::HomeLayout;
use crate::tools::ToolRegistry;

pub(crate) struct McpRefresh {
    home: HomeLayout,
    pool: Arc<Mutex<Option<McpPool>>>,
    registry: Arc<ToolRegistry>,
    catalog: Arc<McpCatalog>,
    index: Arc<ToolSearchIndex>,
    cancel: CancellationToken,
    write_capable: bool,
    drain: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl McpRefresh {
    pub(crate) fn new(
        home: HomeLayout,
        pool: Arc<Mutex<Option<McpPool>>>,
        registry: Arc<ToolRegistry>,
        cancel: CancellationToken,
        write_capable: bool,
    ) -> Arc<Self> {
        let native = registry
            .names()
            .into_iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        let catalog = McpCatalog::new(None, native, []);
        // A bare name must never switch to another server after its previous
        // owner disappears or a faster handshake changes registration order.
        catalog.qualified_names.store(true, Ordering::Relaxed);
        let index = ToolSearchIndex::new();
        register_meta_tools(&registry, &catalog, Arc::clone(&index));
        Arc::new(Self {
            home,
            pool,
            registry,
            catalog,
            index,
            cancel,
            write_capable,
            drain: Mutex::new(None),
        })
    }

    /// Re-read configuration before a model request. Handshakes remain
    /// asynchronous; unchanged ready servers retain their existing tools.
    pub(crate) async fn refresh(&self) -> Option<String> {
        let mut drain = self.drain.lock().await;
        // Await cancellation before changing generations: no old completion
        // callback may reinstall removed tools after the reset below.
        if let Some(task) = drain.take() {
            task.abort();
            let _ = task.await;
        }
        let resolved = resolve(&self.home, &AppSettings::default(), None).await;
        let fingerprint = resolved.fingerprint;
        self.refresh_tool_lists(fingerprint).await;
        let mut connect = spawn_slots(
            resolved,
            &self.pool,
            Arc::new(self.home.root().join("mcp-results")),
        )
        .await;
        for (_, tools) in &mut connect.ready {
            *tools = filter_mcp_tools(std::mem::take(tools), self.write_capable);
        }
        if !self.matches(&connect) {
            self.replace(&connect);
        }
        if !connect.connecting.is_empty() {
            let registry = Arc::clone(&self.registry);
            let catalog = Arc::clone(&self.catalog);
            let failed = Arc::clone(&self.catalog);
            let index = Arc::clone(&self.index);
            let write_capable = self.write_capable;
            *drain = Some(tokio::spawn(drain_mcp_ready_bound(
                Arc::clone(&self.pool),
                connect
                    .ready
                    .iter()
                    .map(|(server, _)| server.id.clone())
                    .collect(),
                connect.failed.iter().map(|(id, _)| id.clone()).collect(),
                Some(self.cancel.clone()),
                Some(fingerprint),
                move |server, tools| {
                    register_ready_server(
                        &registry,
                        &catalog,
                        &index,
                        server,
                        filter_mcp_tools(tools, write_capable),
                    );
                },
                move |server, reason| failed.mark_failed(server, reason),
            )));
        }
        let note = self.catalog.prompt_note();
        (!note.trim().is_empty()).then_some(note)
    }

    /// Relist only servers that emitted `notifications/tools/list_changed`.
    /// Try-lock avoids waiting for another session's active call; its next
    /// request boundary will consume the notification after the call ends.
    async fn refresh_tool_lists(&self, fingerprint: u64) {
        let ready = {
            let guard = self.pool.lock().await;
            let Some(pool) = guard
                .as_ref()
                .filter(|pool| pool.fingerprint == fingerprint)
            else {
                return;
            };
            pool.slots
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    let McpSlot::Ready {
                        server,
                        client,
                        broken,
                        ..
                    } = slot
                    else {
                        return None;
                    };
                    Some((
                        index,
                        Arc::clone(server),
                        Arc::clone(client),
                        Arc::clone(broken),
                    ))
                })
                .collect::<Vec<_>>()
        };
        for (index, server, shared_client, broken) in ready {
            if self.cancel.is_cancelled() {
                return;
            }
            let Ok(mut client) = shared_client.try_lock() else {
                continue;
            };
            if broken.load(Ordering::Relaxed) || !client.take_tools_changed() {
                continue;
            }
            let mut exchange = InFlightExchange {
                broken: &broken,
                complete: false,
            };
            let listed = tokio::select! {
                biased;
                () = self.cancel.cancelled() => {
                    broken.store(true, Ordering::Relaxed);
                    return;
                }
                result = tokio::time::timeout(MCP_HANDSHAKE_TIMEOUT, client.list_tools()) => {
                    result.unwrap_or(Err(crate::app::mcp_client::McpError::Timeout))
                }
            };
            let next = match listed {
                Ok(listed) => {
                    // New identity makes schema-only and same-count changes
                    // invalidate every session's catalog on its next refresh.
                    let server = Arc::new(McpServerInfo {
                        id: server.id.clone(),
                        summary: server.summary.clone(),
                    });
                    let result_dir = Arc::new(self.home.root().join("mcp-results"));
                    let tools = listed
                        .into_iter()
                        .map(|tool| {
                            Arc::new(DynamicMcpTool::new(
                                Arc::clone(&server),
                                tool,
                                Arc::clone(&shared_client),
                                Arc::clone(&broken),
                                Arc::clone(&result_dir),
                            ))
                        })
                        .collect();
                    McpSlot::Ready {
                        server,
                        tools,
                        broken: Arc::clone(&broken),
                        client: Arc::clone(&shared_client),
                    }
                }
                Err(error) => {
                    broken.store(true, Ordering::Relaxed);
                    McpSlot::Failed {
                        server,
                        reason: format!("MCP tools/list refresh failed: {error}"),
                        retry_at: Instant::now() + MCP_RETRY_COOLDOWN,
                    }
                }
            };
            let mut guard = self.pool.lock().await;
            if let Some(pool) = guard
                .as_mut()
                .filter(|pool| pool.fingerprint == fingerprint)
                && let Some(slot @ McpSlot::Ready { .. }) = pool.slots.get_mut(index)
                && matches!(slot, McpSlot::Ready { client, .. } if Arc::ptr_eq(client, &shared_client))
            {
                *slot = next;
                pool.notify.notify_waiters();
            }
            exchange.complete = true;
        }
    }

    fn matches(&self, connect: &McpConnect) -> bool {
        let inner = self.catalog.inner.read();
        inner.warning == connect.warning
            && inner.pending == connect.connecting.iter().cloned().collect()
            && inner.servers.len() == connect.ready.len() + connect.failed.len()
            && connect.ready.iter().all(|(server, tools)| {
                inner.servers.iter().any(|(old, status)| {
                    Arc::ptr_eq(old, server) && *status == ServerStatus::Ready(tools.len())
                })
            })
            && connect.failed.iter().all(|(id, reason)| {
                inner.servers.iter().any(|(old, status)| {
                    old.id == *id && *status == ServerStatus::Failed(reason.clone())
                })
            })
    }

    fn replace(&self, connect: &McpConnect) {
        // Every previous key belongs to MCP, including collision-safe names.
        // Meta tools keep the same catalog Arc, so pending code-mode scripts
        // discover the new generation instead of holding the old endpoint map.
        let mut inner = self.catalog.inner.write();
        for key in inner.by_name.keys() {
            self.registry.unregister(key);
        }
        *inner = CatalogInner {
            warning: connect.warning.clone(),
            by_name: HashMap::new(),
            listed: Vec::new(),
            servers: Vec::new(),
            pending: connect.connecting.iter().cloned().collect(),
        };
        drop(inner);
        self.index.clear();
        for (id, reason) in &connect.failed {
            self.catalog
                .mark_failed(Arc::new(McpServerInfo::new(id, None)), reason);
        }
        for (server, tools) in &connect.ready {
            register_ready_server(
                &self.registry,
                &self.catalog,
                &self.index,
                Arc::clone(server),
                tools.clone(),
            );
        }
    }
}

impl Drop for McpRefresh {
    fn drop(&mut self) {
        if let Some(task) = self.drain.get_mut().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::mcp_client::{JsonRpcChannel, McpClient, McpError, McpTool};
    use crate::tools::RunCodeTool;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    struct CatalogChannel {
        tools: std::sync::Mutex<Vec<McpTool>>,
        changed: AtomicBool,
        list_requests: AtomicUsize,
        block_list: AtomicBool,
        list_started: Notify,
        change_during_list: AtomicBool,
    }

    impl JsonRpcChannel for CatalogChannel {
        fn request<'a>(
            &'a self,
            _id: u64,
            method: &'a str,
            _params: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send + 'a>> {
            Box::pin(async move {
                assert_eq!(method, "tools/list");
                self.list_requests.fetch_add(1, Ordering::Relaxed);
                self.list_started.notify_one();
                if self.block_list.load(Ordering::Relaxed) {
                    std::future::pending::<()>().await;
                }
                if self.change_during_list.swap(false, Ordering::Relaxed) {
                    self.changed.store(true, Ordering::Relaxed);
                }
                let tools = self
                    .tools
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "inputSchema": tool.input_schema,
                            "annotations": {"readOnlyHint": tool.read_only},
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"tools": tools}))
            })
        }

        fn notify<'a>(
            &'a self,
            _method: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn take_tools_changed(&self) -> bool {
            self.changed.swap(false, Ordering::Relaxed)
        }
    }

    struct Fixture {
        root: PathBuf,
        refresh: Arc<McpRefresh>,
        channel: Arc<CatalogChannel>,
    }

    impl Fixture {
        async fn new(write_capable: bool) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mycode-mcp-refresh-{}-{}",
                std::process::id(),
                crate::core::unix_now().as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("mcp.json"),
                r#"{"mcpServers":{"docs":{"command":"mycode-missing-test-command"}}}"#,
            )
            .unwrap();
            let home = HomeLayout::from_root(&root).unwrap();
            let resolved = resolve(&home, &AppSettings::default(), None).await;
            let channel = Arc::new(CatalogChannel {
                tools: std::sync::Mutex::new(vec![listed_tool("lookup", true)]),
                changed: AtomicBool::new(false),
                list_requests: AtomicUsize::new(0),
                block_list: AtomicBool::new(false),
                list_started: Notify::new(),
                change_during_list: AtomicBool::new(false),
            });
            let client = Arc::new(Mutex::new(McpClient::new(channel.clone())));
            let server = Arc::new(McpServerInfo::new("docs", None));
            let broken = Arc::new(AtomicBool::new(false));
            let tools = channel
                .tools
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .map(|tool| {
                    Arc::new(DynamicMcpTool::new(
                        Arc::clone(&server),
                        tool,
                        Arc::clone(&client),
                        Arc::clone(&broken),
                        Arc::new(root.clone()),
                    ))
                })
                .collect();
            let mut pool = McpPool::new(resolved.fingerprint, &resolved.servers, None);
            pool.slots[0] = McpSlot::Ready {
                server,
                tools,
                broken,
                client,
            };
            let registry = Arc::new(ToolRegistry::new());
            registry.register(Arc::new(crate::tools::builtin::ReadTool));
            let refresh = McpRefresh::new(
                home,
                Arc::new(Mutex::new(Some(pool))),
                registry,
                CancellationToken::new(),
                write_capable,
            );
            Self {
                root,
                refresh,
                channel,
            }
        }

        fn change_tools(&self, tools: Vec<McpTool>) {
            *self.channel.tools.lock().unwrap() = tools;
            self.channel.changed.store(true, Ordering::Relaxed);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn listed_tool(name: &str, read_only: bool) -> McpTool {
        McpTool {
            name: name.to_owned(),
            description: Some("Look up docs".to_owned()),
            input_schema: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            read_only,
        }
    }

    #[tokio::test]
    async fn removed_servers_disappear_from_registry_discovery_and_running_code_bridge() {
        let fixture = Fixture::new(true).await;
        let refresh = &fixture.refresh;
        refresh.refresh().await;
        assert!(refresh.registry.get("docs__lookup").is_some());
        assert_eq!(refresh.index.len(), 1);
        let code = RunCodeTool::new(
            Arc::clone(&refresh.registry),
            Arc::new(crate::tools::CodeCells::new()),
            "refresh".to_owned(),
        );
        std::fs::write(fixture.root.join("mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
        assert!(refresh.refresh().await.is_none());
        assert!(refresh.registry.get("read").is_some());
        assert!(refresh.registry.get("docs__lookup").is_none());
        assert!(refresh.catalog.get("docs.lookup").is_none());
        assert_eq!(refresh.index.len(), 0);
        let result = code
            .execute_dyn(
                json!({"script": r#"
const result = call("use_tool", {name: "docs__lookup", arguments: {}});
if (!result.is_error) throw new Error("removed endpoint remains callable");
const direct = call("docs__lookup", {});
if (!direct.is_error) throw new Error("removed tool remains registered");
complete("removed");
"#}),
                &ToolCtx::new(&fixture.root),
                &mut ToolStream::closed(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{result:?}");
    }

    #[tokio::test]
    async fn endpoint_change_removes_old_tools_before_new_handshake_finishes() {
        let fixture = Fixture::new(true).await;
        fixture.refresh.refresh().await;
        std::fs::write(
            fixture.root.join("mcp.json"),
            r#"{"mcpServers":{"docs":{"command":"mycode-other-missing-command"}}}"#,
        )
        .unwrap();
        fixture.refresh.refresh().await;
        assert!(fixture.refresh.registry.get("docs__lookup").is_none());
        assert_eq!(fixture.refresh.index.len(), 0);
    }

    #[tokio::test]
    async fn repairing_an_empty_config_clears_its_cached_warning() {
        let fixture = Fixture::new(true).await;
        std::fs::write(fixture.root.join("mcp.json"), "{invalid").unwrap();
        assert!(fixture.refresh.refresh().await.is_some());
        std::fs::write(fixture.root.join("mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
        assert!(fixture.refresh.refresh().await.is_none());
    }

    #[tokio::test]
    async fn tool_list_notifications_refresh_schemas_and_empty_catalogs_without_polling() {
        let fixture = Fixture::new(true).await;
        let refresh = &fixture.refresh;
        refresh.refresh().await;
        refresh.refresh().await;
        assert_eq!(fixture.channel.list_requests.load(Ordering::Relaxed), 0);
        let mut tool = listed_tool("lookup", true);
        tool.input_schema =
            json!({"type": "object", "properties": {"changedField": {"type": "integer"}}});
        fixture.change_tools(vec![tool]);
        refresh.refresh().await;
        assert!(
            refresh
                .registry
                .get("docs__lookup")
                .unwrap()
                .spec()
                .params_schema["properties"]["changedField"]
                .is_object()
        );
        fixture.change_tools(vec![]);
        refresh.refresh().await;
        assert!(refresh.registry.get("docs__lookup").is_none());
        assert_eq!(refresh.index.len(), 0);
        fixture.change_tools(vec![listed_tool("new_lookup", true)]);
        refresh.refresh().await;
        assert!(refresh.registry.get("docs__new_ulookup").is_some());
        assert_eq!(refresh.index.len(), 1);
        refresh.refresh().await;
        assert_eq!(fixture.channel.list_requests.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn readonly_children_filter_tools_again_after_list_changes() {
        let fixture = Fixture::new(false).await;
        fixture.refresh.refresh().await;
        assert!(fixture.refresh.registry.get("docs__lookup").is_some());
        fixture.change_tools(vec![
            listed_tool("lookup", false),
            listed_tool("safe", true),
        ]);
        fixture.refresh.refresh().await;
        assert!(fixture.refresh.registry.get("docs__lookup").is_none());
        assert!(fixture.refresh.registry.get("docs__safe").is_some());
        assert_eq!(fixture.refresh.index.len(), 1);
    }

    #[tokio::test]
    async fn a_drain_from_an_old_configuration_cannot_register_new_pool_tools() {
        let fixture = Fixture::new(true).await;
        let wrong_fingerprint = fixture
            .refresh
            .pool
            .lock()
            .await
            .as_ref()
            .unwrap()
            .fingerprint
            .wrapping_add(1);
        drain_mcp_ready_bound(
            Arc::clone(&fixture.refresh.pool),
            HashSet::new(),
            HashSet::new(),
            None,
            Some(wrong_fingerprint),
            |_, _| panic!("cross-generation ready callback"),
            |_, _| panic!("cross-generation failed callback"),
        )
        .await;
    }
    #[tokio::test]
    async fn dropping_an_inflight_refresh_invalidates_the_shared_connection() {
        let fixture = Fixture::new(true).await;
        fixture.refresh.refresh().await;
        let broken = {
            let guard = fixture.refresh.pool.lock().await;
            match &guard.as_ref().unwrap().slots[0] {
                McpSlot::Ready { broken, .. } => Arc::clone(broken),
                _ => panic!("ready fixture"),
            }
        };
        fixture.channel.block_list.store(true, Ordering::Relaxed);
        fixture.change_tools(vec![]);
        let refresh = Arc::clone(&fixture.refresh);
        let task = tokio::spawn(async move { refresh.refresh().await });
        fixture.channel.list_started.notified().await;
        task.abort();
        let _ = task.await;
        assert!(broken.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn notifications_arriving_during_relisting_are_not_lost() {
        let fixture = Fixture::new(true).await;
        fixture.refresh.refresh().await;
        fixture.change_tools(vec![listed_tool("changed", true)]);
        fixture
            .channel
            .change_during_list
            .store(true, Ordering::Relaxed);
        fixture.refresh.refresh().await;
        fixture.refresh.refresh().await;
        assert_eq!(fixture.channel.list_requests.load(Ordering::Relaxed), 2);
    }
    #[tokio::test]
    async fn canonical_tool_names_do_not_rebind_after_reordering_or_removal() {
        let fixture = Fixture::new(true).await;
        let first = super::super::stub_catalog_tool("a__b", "c", "First");
        let second = super::super::stub_catalog_tool("a", "b__c", "Second");
        let ready = vec![
            (Arc::clone(&first.server), vec![first]),
            (Arc::clone(&second.server), vec![second]),
        ];
        let first_key = qualified_tool_key("a__b", "c");
        let second_key = qualified_tool_key("a", "b__c");
        assert_ne!(first_key, second_key);
        let mut connect = McpConnect {
            ready,
            connecting: vec![],
            failed: vec![],
            warning: None,
        };
        fixture.refresh.replace(&connect);
        assert_eq!(
            fixture
                .refresh
                .catalog
                .get(&first_key)
                .unwrap()
                .1
                .server_id(),
            "a__b"
        );
        connect.ready.reverse();
        fixture.refresh.replace(&connect);
        assert_eq!(
            fixture
                .refresh
                .catalog
                .get(&first_key)
                .unwrap()
                .1
                .server_id(),
            "a__b"
        );
        connect.ready.pop();
        fixture.refresh.replace(&connect);
        assert!(fixture.refresh.catalog.get(&first_key).is_none());
        assert_eq!(
            fixture
                .refresh
                .catalog
                .get(&second_key)
                .unwrap()
                .1
                .server_id(),
            "a"
        );
        // Encoded old keys must also not become friendly aliases to a server
        // whose literal ID happens to equal an encoded component.
        let alias = super::super::stub_catalog_tool("a_ub", "c", "Alias");
        connect.ready = vec![(Arc::clone(&alias.server), vec![alias])];
        fixture.refresh.replace(&connect);
        assert!(
            fixture
                .refresh
                .catalog
                .get(&qualified_tool_key("a_b", "c"))
                .is_none()
        );
    }
}
