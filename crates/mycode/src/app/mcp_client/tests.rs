use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{JsonRpcChannel, McpClient, McpError};

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
