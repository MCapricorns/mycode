//! Native image admission and delivery through real QuickJS calls.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mycode_providers::message::{BinaryData, ContentBlock};
use serde_json::{Value, json};

use super::{CodeCells, RunCodeTool, WaitCodeTool};
use crate::{ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolResult, ToolSpec, ToolStream};

struct ImagesTool;

impl ToolDyn for ImagesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "images".to_owned(),
            description: "Returns small mock image payloads, optionally with a partial error."
                .to_owned(),
            params_schema: json!({"type": "object"}),
        }
    }

    fn execute_dyn<'a>(
        &'a self,
        args: Value,
        _ctx: &'a ToolCtx,
        _out: &'a mut ToolStream,
    ) -> Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            let mut result = if args["is_error"].as_bool().unwrap_or(false) {
                ToolResult::error("partial image tool failure")
            } else {
                ToolResult::text("images ready")
            };
            result
                .content
                .extend(args["images"].as_array().unwrap().iter().map(|data| {
                    ContentBlock::Image(BinaryData {
                        data: data.as_str().unwrap().to_owned(),
                        mime_type: "image/png".to_owned(),
                    })
                }));
            Ok(result)
        })
    }
}

fn text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(block) => Some(block.text.as_str()),
            _ => None,
        })
        .collect()
}

fn images(result: &ToolResult) -> Vec<&str> {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image(image) => Some(image.data.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn image_admission_errors_are_script_results_and_accepted_images_are_delivered_once() {
    for fail_script in [false, true] {
        let cells = Arc::new(CodeCells::new());
        let registry = Arc::new(ToolRegistry::new());
        registry.register(Arc::new(ImagesTool));
        let run = RunCodeTool::new(Arc::clone(&registry), Arc::clone(&cells), "s".to_owned());
        let ctx = ToolCtx::new(std::env::temp_dir());
        let ending = if fail_script {
            "throw new Error('after images');"
        } else {
            "complete('done');"
        };
        let script = format!(
            r#"
const accepted = call("images", {{ images: ["AAAA", "BBBB", "CCCC"] }});
const rejected = call("images", {{ images: ["DDDD", "EEEE"] }});
const partial = call("images", {{ images: ["FFFF"], is_error: true }});
const exhausted = call("images", {{ images: ["GGGG"], is_error: true }});
log({{ accepted, rejected, partial, exhausted }});
{ending}
"#
        );
        let result = run
            .execute_dyn(
                json!({"script": script, "yield_time_ms": 5_000}),
                &ctx,
                &mut ToolStream::closed(),
            )
            .await
            .unwrap();
        assert_eq!(result.is_error, fail_script, "{result:?}");
        assert_eq!(images(&result), vec!["AAAA", "BBBB", "CCCC", "FFFF"]);
        let output = text(&result);
        let responses: Value = serde_json::from_str(output.lines().next().unwrap()).unwrap();
        for key in ["rejected", "exhausted"] {
            let response = &responses[key];
            assert_eq!(response["is_error"], true);
            assert!(
                response["__error__"]
                    .as_str()
                    .unwrap()
                    .contains("count limit exceeded")
            );
            assert!(
                response["content"][0]["Text"]
                    .as_str()
                    .unwrap()
                    .contains("no images from this tool result were queued")
            );
            assert!(
                response["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|block| block.get("Image").is_none())
            );
        }
        assert_eq!(responses["accepted"]["is_error"], false);
        assert_eq!(responses["partial"]["is_error"], true);
        assert_eq!(
            responses["partial"]["content"][0]["Text"],
            "partial image tool failure"
        );
        for (key, count) in [("accepted", 3), ("partial", 1)] {
            let metadata: Vec<_> = responses[key]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|block| block.get("Image"))
                .collect();
            assert_eq!(metadata.len(), count);
            for image in metadata {
                assert_eq!(
                    image["delivery"],
                    "native image in run_code/wait_code result"
                );
                assert!(image.get("data").is_none());
            }
        }
        let details = result.details.as_ref().unwrap();
        let calls = details["inner_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 4);
        for (call, failed) in calls.iter().zip([false, true, true, true]) {
            assert_eq!(call["is_error"], failed);
            assert_eq!(call["phase"], if failed { "failed" } else { "completed" });
        }
        if fail_script {
            assert!(output.contains("after images"));
        }
        let wait = WaitCodeTool::new(Arc::clone(&cells), "s".to_owned());
        for _ in 0..2 {
            let repeated = wait
                .execute_dyn(
                    json!({"cell_id": details["cell_id"]}),
                    &ctx,
                    &mut ToolStream::closed(),
                )
                .await
                .unwrap();
            assert_eq!(repeated.is_error, fail_script);
            assert!(
                images(&repeated).is_empty(),
                "accepted images must not be resent"
            );
        }
    }
}
