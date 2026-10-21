//! Parses magpie's `GET /v1/models` answer into the one provider row.
//!
//! The gateway lists every model the user turned on in magpie under
//! `provider/model`, with the metadata the picker needs: display name,
//! context and output limits, reasoning levels, and whether it takes images.

use serde_json::Value;

use super::{
    CatalogDocument, CatalogModel, CatalogProvider, KIND_OPENAI_COMPLETIONS,
    MAX_MODELS_PER_PROVIDER, clean_text, normalize_provider,
};

/// Provider id every magpie model is reached under.
pub const PROVIDER_ID: &str = "magpie";
/// Display name of the gateway row.
pub const PROVIDER_NAME: &str = "Magpie";

/// Builds the provider row from one `/v1/models` document.
///
/// # Errors
///
/// Returns a one-line reason when the body is not the gateway's list shape.
pub fn provider_from_models(body: &[u8], base_url: &str) -> Result<CatalogProvider, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "magpie /v1/models is not JSON".to_owned())?;
    let items = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| "magpie /v1/models has no data list".to_owned())?;
    let mut models = Vec::new();
    for item in items.iter().take(MAX_MODELS_PER_PROVIDER * 4) {
        if let Some(model) = model_from(item) {
            models.push(model);
        }
    }
    if models.is_empty() {
        return Err("magpie lists no models; pick one in magpie first".to_owned());
    }
    let provider = CatalogProvider {
        id: PROVIDER_ID.to_owned(),
        name: PROVIDER_NAME.to_owned(),
        kind: KIND_OPENAI_COMPLETIONS.to_owned(),
        base_url: base_url.to_owned(),
        models,
    };
    normalize_provider(provider).ok_or_else(|| "magpie listed no usable models".to_owned())
}

/// The whole catalog document: one provider row, no vendors of its own.
#[must_use]
pub fn document_from_provider(provider: CatalogProvider) -> CatalogDocument {
    CatalogDocument {
        providers: vec![provider],
    }
}

fn model_from(item: &Value) -> Option<CatalogModel> {
    let id = clean_text(item.get("id").and_then(Value::as_str)?)?;
    let name = item
        .get("display_name")
        .or_else(|| item.get("name"))
        .and_then(Value::as_str)
        .and_then(clean_text)
        .unwrap_or_else(|| id.clone());
    let mut efforts = Vec::new();
    if let Some(levels) = item.get("reasoning_levels").and_then(Value::as_array) {
        for level in levels {
            if let Some(level) = level.as_str().and_then(clean_text) {
                push_effort(&mut efforts, &level);
            }
        }
    }
    if let Some(levels) = item
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
    {
        for level in levels {
            let effort = level
                .get("effort")
                .and_then(Value::as_str)
                .or_else(|| level.as_str());
            if let Some(effort) = effort.and_then(clean_text) {
                push_effort(&mut efforts, &effort);
            }
        }
    }
    let reasoning = item
        .get("reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(!efforts.is_empty());
    let takes_images = item
        .get("modalities")
        .and_then(|modalities| modalities.get("input"))
        .and_then(Value::as_array)
        .is_some_and(|inputs| inputs.iter().any(|input| input.as_str() == Some("image")));
    Some(CatalogModel {
        id,
        name,
        reasoning,
        reasoning_toggle: false,
        reasoning_efforts: efforts,
        tool_call: true,
        attachment: takes_images,
        context: item
            .get("context_window")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: item
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cost_in: None,
        cost_out: None,
    })
}

/// Keeps one effort token, lowercased, without duplicates or junk.
fn push_effort(efforts: &mut Vec<String>, effort: &str) {
    let effort = effort.trim().to_ascii_lowercase();
    if effort.is_empty() || effort == "default" || effort.len() > 32 {
        return;
    }
    if !efforts.iter().any(|held| held == &effort) && efforts.len() < 16 {
        efforts.push(effort);
    }
}

#[cfg(test)]
mod tests {
    use super::{PROVIDER_ID, provider_from_models};

    #[test]
    fn the_gateway_list_becomes_one_provider_row() {
        let body = br#"{"object":"list","data":[
            {"id":"deepseek/deepseek-chat","display_name":"DeepSeek Chat","owned_by":"deepseek",
             "context_window":128000,"max_output_tokens":32000,
             "modalities":{"input":["text"]}},
            {"id":"claude/claude-opus-5","display_name":"Claude Opus 5","owned_by":"claude",
             "reasoning":true,"reasoning_levels":["low","high","max"],
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
             "context_window":1000000,"max_output_tokens":128000,
             "modalities":{"input":["text","image"]}}
        ]}"#;
        let provider = provider_from_models(body, "http://127.0.0.1:3425/v1").expect("provider");
        assert_eq!(provider.id, PROVIDER_ID);
        assert_eq!(provider.models.len(), 2);
        let opus = provider
            .models
            .iter()
            .find(|model| model.id == "claude/claude-opus-5")
            .expect("opus");
        assert_eq!(opus.name, "Claude Opus 5");
        assert_eq!(opus.context, 1_000_000);
        assert_eq!(opus.output, 128_000);
        assert!(opus.reasoning);
        assert!(opus.attachment);
        assert_eq!(opus.reasoning_levels(), ["low", "high", "max"]);
        let chat = provider
            .models
            .iter()
            .find(|model| model.id == "deepseek/deepseek-chat")
            .expect("chat");
        assert!(!chat.reasoning);
        assert!(!chat.attachment);
        assert!(chat.reasoning_levels().is_empty());
    }

    #[test]
    fn an_empty_or_unknown_list_is_an_error() {
        assert!(provider_from_models(br#"{"data":[]}"#, "http://localhost:1").is_err());
        assert!(provider_from_models(b"not json", "http://localhost:1").is_err());
    }
}
