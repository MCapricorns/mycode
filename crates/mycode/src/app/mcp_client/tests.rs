use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{JsonRpcChannel, MAX_TOOL_PAGES, MAX_TOOLS, McpClient, McpError};

#[derive(Default)]
struct Pages {
    replies: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<Value>>,
}

impl JsonRpcChannel for Pages {
    fn request<'a>(
        &'a self,
        _id: u64,
        method: &'a str,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpError>> + Send + 'a>> {
        Box::pin(async move {
            assert!(matches!(method, "tools/list" | "tools/call"));
            self.requests.lock().unwrap().push(params);
            Ok(self.replies.lock().unwrap().pop_front().expect("next page"))
        })
    }

    fn notify<'a>(
        &'a self,
        _method: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), McpError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

fn client(pages: Vec<Value>) -> (McpClient, Arc<Pages>) {
    let channel = Arc::new(Pages {
        replies: Mutex::new(pages.into()),
        ..Pages::default()
    });
    (McpClient::new(channel.clone()), channel)
}

#[tokio::test]
async fn follows_opaque_cursors_and_keeps_every_tool() {
    let (mut client, channel) = client(vec![
        json!({ "tools": [{ "name": "first" }], "nextCursor": "opaque + /?=" }),
        json!({ "tools": [{ "name": "second", "annotations": { "readOnlyHint": true } }] }),
    ]);
    let tools = client.list_tools().await.unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(tools[1].read_only);
    assert_eq!(
        *channel.requests.lock().unwrap(),
        vec![json!({}), json!({ "cursor": "opaque + /?=" })]
    );
}

#[tokio::test]
async fn accepts_a_single_page_and_an_empty_intermediate_page() {
    let (mut single, channel) = client(vec![json!({ "tools": [{ "name": "only" }] })]);
    assert_eq!(single.list_tools().await.unwrap().len(), 1);
    assert_eq!(channel.requests.lock().unwrap().len(), 1);

    let (mut paged, _) = client(vec![
        json!({ "tools": [], "nextCursor": "next" }),
        json!({ "tools": [{ "name": "last" }], "nextCursor": null }),
    ]);
    assert_eq!(paged.list_tools().await.unwrap()[0].name, "last");
}

#[tokio::test]
async fn enforces_the_tool_limit_across_pages() {
    let first: Vec<Value> = (0..MAX_TOOLS)
        .map(|index| json!({ "name": format!("tool-{index}") }))
        .collect();
    let (mut client, _) = client(vec![
        json!({ "tools": first, "nextCursor": "overflow" }),
        json!({ "tools": [{ "name": "one-too-many" }] }),
    ]);
    assert_eq!(client.list_tools().await, Err(McpError::Oversized));
}

#[tokio::test]
async fn rejects_repeated_and_invalid_cursors() {
    let (mut repeated, channel) = client(vec![
        json!({ "tools": [], "nextCursor": "again" }),
        json!({ "tools": [], "nextCursor": "again" }),
    ]);
    assert!(matches!(
        repeated.list_tools().await,
        Err(McpError::Protocol(_))
    ));
    assert_eq!(channel.requests.lock().unwrap().len(), 2);

    let (mut invalid, _) = client(vec![json!({ "tools": [], "nextCursor": 7 })]);
    assert!(matches!(
        invalid.list_tools().await,
        Err(McpError::Protocol(_))
    ));
}

#[tokio::test]
async fn bounds_empty_pages_with_ever_changing_cursors() {
    let pages = (0..MAX_TOOL_PAGES)
        .map(|index| json!({ "tools": [], "nextCursor": format!("page-{index}") }))
        .collect();
    let (mut client, channel) = client(pages);
    assert_eq!(client.list_tools().await, Err(McpError::Oversized));
    assert_eq!(channel.requests.lock().unwrap().len(), MAX_TOOL_PAGES);
}

#[tokio::test]
async fn tool_results_preserve_structured_data_beside_status_text_and_resource_links() {
    let (mut client, _) = client(vec![json!({
        "content": [
            {"type": "text", "text": "Action completed."},
            {"type": "resource_link", "uri": "file:///report.csv", "name": "report", "mimeType": "text/csv"}
        ],
        "structuredContent": {"rows": [{"id": 7}]},
        "isError": true
    })]);
    let output = client.call_tool("report", json!({})).await.unwrap();
    assert!(output.text.contains("Action completed."));
    assert!(output.text.contains("file:///report.csv"));
    assert!(output.text.contains(r#""rows":[{"id":7}]"#));
    assert!(output.is_error);
}

#[tokio::test]
async fn structured_compatibility_text_is_not_duplicated() {
    let (mut client, _) = client(vec![json!({
        "content": [{"type": "text", "text": "{ \"value\": 7 }"}],
        "structuredContent": {"value": 7}
    })]);
    assert_eq!(
        client.call_tool("report", json!({})).await.unwrap().text,
        "{ \"value\": 7 }"
    );
}
