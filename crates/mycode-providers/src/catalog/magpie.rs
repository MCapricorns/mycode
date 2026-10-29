//! Parses magpie's `GET /v1/models` answer into the one provider row.
//!
//! The gateway lists every model the user turned on in magpie under
//! `provider/model`, with the metadata the picker needs: display name,
//! context and output limits, reasoning levels, and whether it takes images.
//! Bare ids with a matching explicit owner are kept as aliases of those routes.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    CatalogDocument, CatalogModel, CatalogProvider, KIND_OPENAI_COMPLETIONS,
    MAX_MODELS_PER_PROVIDER, clean_text, normalize_provider, valid_provider_id,
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
    // Keep the first row for each exact id, as normalization does. Sorting
    // before alias resolution also makes the bounded catalog order independent
    // of whether a route or its bare alias appeared first in the response.
    let mut entries = BTreeMap::new();
    for item in items.iter().take(MAX_MODELS_PER_PROVIDER * 4) {
        if let Some(model) = model_from(item) {
            let owner = item
                .get("owned_by")
                .and_then(Value::as_str)
                .and_then(clean_text)
                .filter(|owner| valid_provider_id(owner));
            entries.entry(model.id.clone()).or_insert((model, owner));
        }
    }
    if entries.is_empty() {
        return Err("magpie lists no models; pick one in magpie first".to_owned());
    }
    let aliases: Vec<_> = entries
        .iter()
        .filter_map(|(id, (_, owner))| {
            if id.contains('/') {
                return None;
            }
            let owner = owner.as_deref()?;
            let route = format!("{owner}/{id}");
            let (_, route_owner) = entries.get(&route)?;
            (route_owner.as_deref() == Some(owner)).then(|| (id.clone(), route))
        })
        .collect();
    for (alias, route) in aliases {
        if let Some((model, _)) = entries.get_mut(&route) {
            model.aliases.push(alias.clone());
        }
        entries.remove(&alias);
    }
    let provider = CatalogProvider {
        id: PROVIDER_ID.to_owned(),
        name: PROVIDER_NAME.to_owned(),
        kind: KIND_OPENAI_COMPLETIONS.to_owned(),
        base_url: base_url.to_owned(),
        models: entries.into_values().map(|(model, _)| model).collect(),
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
    // Missing or malformed metadata does not establish that images are
    // unsupported; let the gateway decide unless it publishes an input list.
    let takes_images = item
        .get("modalities")
        .and_then(|modalities| modalities.get("input"))
        .and_then(Value::as_array)
        .is_none_or(|inputs| inputs.iter().any(|input| input.as_str() == Some("image")));
    Some(CatalogModel {
        id,
        aliases: Vec::new(),
        source: item
            .get("owned_by")
            .and_then(Value::as_str)
            .and_then(clean_text)
            .unwrap_or_default(),
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
    use super::{MAX_MODELS_PER_PROVIDER, PROVIDER_ID, provider_from_models};

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
        assert_eq!(opus.source, "claude");
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
        assert_eq!(chat.source, "deepseek");
        assert!(!chat.reasoning);
        assert!(!chat.attachment);
        assert!(chat.reasoning_levels().is_empty());
    }

    #[test]
    fn qualified_routes_replace_bare_alias_rows_in_either_order() {
        let route = serde_json::json!({
            "id": "copilot/claude-opus-5", "owned_by": "copilot",
            "display_name": "Claude Opus 5", "reasoning": true,
            "reasoning_levels": ["high", "max"],
            "context_window": 1_000_000, "max_output_tokens": 128_000,
            "modalities": {"input": ["text", "image"]}
        });
        let alias = serde_json::json!({
            "id": "claude-opus-5", "owned_by": "copilot",
            "display_name": "Claude Opus 5", "reasoning": false,
            "context_window": 4096, "modalities": {"input": ["text"]}
        });
        let mut previous = None;
        for items in [[route.clone(), alias.clone()], [alias, route]] {
            let body = serde_json::to_vec(&serde_json::json!({"data": items})).expect("body");
            let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
            assert_eq!(
                provider.models.len(),
                1,
                "a route and its alias are one model"
            );
            assert_eq!(provider.models[0].aliases, ["claude-opus-5"]);
            assert_eq!(
                provider.model("claude-opus-5"),
                provider.model("copilot/claude-opus-5")
            );
            let document = super::document_from_provider(provider.clone());
            for id in ["copilot/claude-opus-5", "claude-opus-5"] {
                let model = document.model(PROVIDER_ID, id).expect("model by either id");
                assert_eq!(model.id, "copilot/claude-opus-5");
                assert_eq!(model.name, "Claude Opus 5");
                assert_eq!(model.context, 1_000_000);
                assert_eq!(model.output, 128_000);
                assert!(model.attachment);
                assert!(model.reasoning);
                assert_eq!(model.reasoning_levels(), ["high", "max"]);
                for (provider_id, base_url) in [
                    (PROVIDER_ID, None),
                    ("configured-gateway", Some("http://localhost:1/v1")),
                    ("unknown", None),
                ] {
                    assert_eq!(
                        document.model_for_endpoint(provider_id, base_url, id),
                        Some(model)
                    );
                }
            }
            if let Some(previous) = &previous {
                assert_eq!(
                    &provider, previous,
                    "canonical metadata ignores input order"
                );
            }
            previous = Some(provider);
        }
    }

    #[test]
    fn same_named_routes_from_different_owners_remain_distinct() {
        let items = vec![
            serde_json::json!({
                "id": "copilot/gpt-6.1-terra", "owned_by": "copilot",
                "display_name": "GPT 6.1 Terra", "context_window": 128_000,
                "modalities": {"input": ["text", "image"]}
            }),
            serde_json::json!({
                "id": "codex/gpt-6.1-terra", "owned_by": "codex",
                "display_name": "GPT 6.1 Terra", "context_window": 256_000,
                "modalities": {"input": ["text"]}
            }),
            serde_json::json!({
                "id": "gpt-6.1-terra", "owned_by": "copilot",
                "display_name": "Gateway alias label"
            }),
        ];
        let mut reversed = items.clone();
        reversed.reverse();
        let mut previous = None;
        for items in [items, reversed] {
            let body = serde_json::to_vec(&serde_json::json!({"data": items})).expect("body");
            let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
            assert_eq!(
                provider
                    .models
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                ["codex/gpt-6.1-terra", "copilot/gpt-6.1-terra"]
            );
            let copilot = provider
                .model("gpt-6.1-terra")
                .expect("explicit alias owner");
            assert_eq!(copilot.id, "copilot/gpt-6.1-terra");
            assert_eq!(copilot.name, "GPT 6.1 Terra");
            assert_eq!(copilot.context, 128_000);
            assert!(copilot.attachment);
            let codex = provider.model("codex/gpt-6.1-terra").expect("other route");
            assert_eq!(codex.context, 256_000);
            assert!(!codex.attachment);
            assert!(codex.aliases.is_empty());
            if let Some(previous) = &previous {
                assert_eq!(&provider, previous);
            }
            previous = Some(provider);
        }
    }

    #[test]
    fn bare_only_models_are_not_matched_by_display_name() {
        let body = br#"{"data":[
            {"id":"bare-only","owned_by":"copilot","display_name":"Same name"},
            {"id":"copilot/other-model","owned_by":"copilot","display_name":"Same name"}
        ]}"#;
        let provider = provider_from_models(body, "http://localhost:1/v1").expect("provider");
        assert_eq!(provider.models.len(), 2);
        assert!(provider.model("bare-only").is_some());
        assert!(provider.models.iter().all(|model| model.aliases.is_empty()));
    }

    #[test]
    fn alias_rows_need_matching_explicit_owners_on_both_rows() {
        let copilot = Some(serde_json::json!("copilot"));
        for (alias_owner, route_owner) in [
            (None, copilot.clone()),
            (Some(serde_json::Value::Null), copilot.clone()),
            (Some(serde_json::json!(42)), copilot.clone()),
            (copilot.clone(), None),
            (copilot.clone(), Some(serde_json::Value::Null)),
            (copilot.clone(), Some(serde_json::json!(42))),
            (copilot.clone(), Some(serde_json::json!("codex"))),
            (Some(serde_json::json!("codex")), copilot),
        ] {
            let mut alias = serde_json::json!({"id": "model", "display_name": "Same name"});
            let mut route = serde_json::json!({"id": "copilot/model", "display_name": "Same name"});
            if let Some(owner) = alias_owner {
                alias["owned_by"] = owner;
            }
            if let Some(owner) = route_owner {
                route["owned_by"] = owner;
            }
            let body =
                serde_json::to_vec(&serde_json::json!({"data": [alias, route]})).expect("body");
            let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
            assert_eq!(
                provider.models.len(),
                2,
                "owners must agree: {}",
                String::from_utf8_lossy(&body)
            );
            assert_eq!(provider.model("model").expect("bare row").id, "model");
            assert!(provider.models.iter().all(|model| model.aliases.is_empty()));
        }
    }

    #[test]
    fn invalid_owners_never_establish_alias_routes() {
        for owner in [
            String::new(),
            "team/name".to_owned(),
            "team:name".to_owned(),
            "co\npilot".to_owned(),
            "co pilot".to_owned(),
            "x".repeat(129),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({"data": [
                {"id": "model", "owned_by": owner},
                {"id": format!("{owner}/model"), "owned_by": owner}
            ]}))
            .expect("body");
            let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
            assert_eq!(provider.model("model").expect("bare row").id, "model");
            assert!(provider.models.iter().all(|model| model.aliases.is_empty()));
        }
    }

    #[test]
    fn qualified_ids_are_never_treated_as_bare_aliases() {
        let body = br#"{"data":[
            {"id":"team/model","owned_by":"copilot"},
            {"id":"copilot/team/model","owned_by":"copilot"}
        ]}"#;
        let provider = provider_from_models(body, "http://localhost:1/v1").expect("provider");
        assert_eq!(provider.models.len(), 2);
        assert!(provider.models.iter().all(|model| model.aliases.is_empty()));
    }

    #[test]
    fn duplicate_exact_ids_keep_the_first_metadata_and_owner() {
        for first_owner in ["copilot", "codex"] {
            let body = serde_json::to_vec(&serde_json::json!({"data": [
                {"id": "copilot/model", "owned_by": first_owner, "display_name": "First", "context_window": 128_000},
                {"id": "model", "owned_by": "copilot"},
                {"id": "copilot/model", "owned_by": "copilot", "display_name": "Duplicate", "context_window": 4096},
                {"id": "model", "owned_by": "codex"}
            ]})).expect("body");
            let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
            let route = provider.model("copilot/model").expect("first route");
            assert_eq!(route.name, "First");
            assert_eq!(route.context, 128_000);
            if first_owner == "copilot" {
                assert_eq!(provider.models.len(), 1);
                assert_eq!(route.aliases, ["model"]);
                assert_eq!(provider.model("model"), Some(route));
            } else {
                assert_eq!(provider.models.len(), 2);
                assert!(route.aliases.is_empty());
                assert_eq!(
                    provider.model("model").expect("unmatched bare row").id,
                    "model"
                );
            }
        }
    }

    #[test]
    fn alias_resolution_keeps_the_scan_and_catalog_limits() {
        let mut items = vec![serde_json::json!({"id": "000-bare", "owned_by": "copilot"})];
        items.extend(
            (1..MAX_MODELS_PER_PROVIDER * 4)
                .map(|index| serde_json::json!({"id": format!("model-{index:04}")})),
        );
        items.push(serde_json::json!({"id": "copilot/000-bare", "owned_by": "copilot"}));
        let body = serde_json::to_vec(&serde_json::json!({"data": items})).expect("body");
        let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
        assert_eq!(provider.models.len(), MAX_MODELS_PER_PROVIDER);
        assert!(provider.model("copilot/000-bare").is_none());
        assert_eq!(
            provider.model("000-bare").expect("bare row within scan").id,
            "000-bare"
        );
        assert!(
            provider
                .models
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
        );
    }

    #[test]
    fn unpublished_image_capability_is_left_to_the_gateway() {
        for mut item in [
            serde_json::json!({}),
            serde_json::json!({"modalities": null}),
            serde_json::json!({"modalities": {}}),
            serde_json::json!({"modalities": {"output": ["image"]}}),
            serde_json::json!({"modalities": {"input": null}}),
            serde_json::json!({"modalities": {"input": "image"}}),
        ] {
            item["id"] = serde_json::json!("fixture/model");
            let body = serde_json::to_vec(&serde_json::json!({"data": [item]})).expect("body");
            let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
            assert!(
                provider.models[0].attachment,
                "unknown input capability must not block images: {}",
                String::from_utf8_lossy(&body)
            );
        }
    }

    #[test]
    fn published_input_modalities_control_image_support() {
        for (inputs, takes_images) in [
            (serde_json::json!(["text", "image"]), true),
            (serde_json::json!(["image"]), true),
            (serde_json::json!(["text"]), false),
            (serde_json::json!(["text", "audio"]), false),
            (serde_json::json!([]), false),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({"data": [{
                "id": "fixture/model",
                "modalities": {"input": inputs, "output": ["image"]}
            }]}))
            .expect("body");
            let provider = provider_from_models(&body, "http://localhost:1/v1").expect("provider");
            assert_eq!(
                provider.models[0].attachment,
                takes_images,
                "only input modalities determine image support: {}",
                String::from_utf8_lossy(&body)
            );
        }
    }

    #[test]
    fn an_empty_or_unknown_list_is_an_error() {
        assert!(provider_from_models(br#"{"data":[]}"#, "http://localhost:1").is_err());
        assert!(provider_from_models(b"not json", "http://localhost:1").is_err());
    }
}
