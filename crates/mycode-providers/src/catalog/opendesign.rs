//! The OpenDesign gateway (`https://amr-link.open-design.ai/v1`).
//!
//! models.dev does not carry this gateway either, so the row is fully
//! hand-maintained below. The model data follows
//! `@lokeraar/pi-opendesign-bridge`: ids are measured against the gateway,
//! reasoning levels it rejects are left out, `off` is offered only where
//! `reasoning_effort: "none"` both passes and stops the thinking, and the
//! output cap is the lower of the gateway budget and the vendor maximum.
//! Update `CURATED` when the gateway gains or retires a model — the update
//! procedure lives in `docs/model-catalog-updates.md`.
use super::{CatalogDocument, CatalogModel, CatalogProvider, KIND_OPENAI_COMPLETIONS};

/// Provider id used in settings and the catalog.
pub const PROVIDER_ID: &str = "opendesign";
/// Display name.
pub const PROVIDER_NAME: &str = "OpenDesign";
/// OpenAI-compatible base URL of the gateway.
pub const BASE_URL: &str = "https://amr-link.open-design.ai/v1";

/// Tokens kept free under the window. The gateway rejects a ceiling equal to
/// the window: it counts messages + tools + `max_tokens` against it.
const PROMPT_RESERVE_TOKENS: u64 = 2_048;

/// One curated row: limits, image input, and the accepted effort tokens.
/// `none` in `efforts` is the measured Off switch.
struct Curated {
    id: &'static str,
    context: u64,
    output: u64,
    image: bool,
    efforts: &'static [&'static str],
}

const OFF_AND_ALL: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
/// GLM-5.3 accepts only `low` / `high` / `max` and rejects `none`.
const GLM_53: &[&str] = &["low", "high", "max"];
/// GPT-6 rejects `minimal`; Luna accepts `none`, Sol does not.
const GPT_6_LUNA: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
const GPT_61_SOL: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// K2.7 Code always thinks and rejects `max`.
const KIMI_K27_CODE: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];

/// Measured on 2026-10-01 against the gateway (pi-opendesign-bridge 0.2.9).
/// `context` is the gateway's `context_limit`; `output` is the lower of its
/// `context_budget` and the vendor's published maximum.
const CURATED: &[Curated] = &[
    Curated {
        id: "deepseek-v4-flash",
        context: 1_048_000,
        output: 232_000,
        image: false,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "deepseek-v4-flash-vision-exp",
        context: 1_048_000,
        output: 232_000,
        image: true,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "deepseek-v4-pro",
        context: 1_048_000,
        output: 232_000,
        image: false,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "deepseek-v4.1-flash",
        context: 1_048_000,
        output: 232_000,
        image: true,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "glm-5.3-flash",
        context: 1_048_000,
        output: 128_000,
        image: true,
        efforts: GLM_53,
    },
    Curated {
        id: "glm-5.3-flashx",
        context: 1_000_000,
        output: 128_000,
        image: true,
        efforts: GLM_53,
    },
    Curated {
        id: "gpt-6-luna",
        context: 1_050_000,
        output: 128_000,
        image: true,
        efforts: GPT_6_LUNA,
    },
    Curated {
        id: "gpt-6.1-sol",
        context: 1_050_000,
        output: 128_000,
        image: true,
        efforts: GPT_61_SOL,
    },
    Curated {
        id: "kimi-k2.7-code",
        context: 262_000,
        output: 262_000,
        image: true,
        efforts: KIMI_K27_CODE,
    },
    Curated {
        id: "mimo-v2.6-flash",
        context: 1_048_000,
        output: 131_072,
        image: true,
        efforts: OFF_AND_ALL,
    },
    Curated {
        id: "mimo-v2.6-pro",
        context: 1_048_000,
        output: 131_072,
        image: true,
        efforts: OFF_AND_ALL,
    },
];

/// An output cap the window can hold: a cap at or above the window always
/// fails, so `PROMPT_RESERVE_TOKENS` stay free (never below 1024).
fn clamp_output(output: u64, context: u64) -> u64 {
    if context == 0 {
        return output;
    }
    output.min(context.saturating_sub(PROMPT_RESERVE_TOKENS).max(1_024))
}

fn curated_model(row: &Curated) -> CatalogModel {
    CatalogModel {
        id: row.id.to_owned(),
        name: row.id.to_owned(),
        reasoning: true,
        reasoning_toggle: false,
        reasoning_efforts: row
            .efforts
            .iter()
            .map(|effort| (*effort).to_owned())
            .collect(),
        tool_call: true,
        attachment: row.image,
        context: row.context,
        output: clamp_output(row.output, row.context),
        cost_in: None,
        cost_out: None,
    }
}

/// The curated model list of the provider row.
#[must_use]
pub fn curated_models() -> Vec<CatalogModel> {
    CURATED.iter().map(curated_model).collect()
}

/// Wraps a model list in the OpenDesign provider row.
#[must_use]
pub fn provider(mut models: Vec<CatalogModel>) -> CatalogProvider {
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    CatalogProvider {
        id: PROVIDER_ID.to_owned(),
        name: PROVIDER_NAME.to_owned(),
        kind: KIND_OPENAI_COMPLETIONS.to_owned(),
        base_url: BASE_URL.to_owned(),
        doc: None,
        auth: String::new(),
        models,
    }
}

/// Replaces or inserts the OpenDesign row of `document` with the curated
/// list.
pub fn attach(document: &mut CatalogDocument) {
    let row = provider(curated_models());
    match document
        .providers
        .iter_mut()
        .find(|existing| existing.id == PROVIDER_ID)
    {
        Some(existing) => *existing = row,
        None => {
            document.providers.push(row);
            document.providers.sort_by(|a, b| a.id.cmp(&b.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BASE_URL, PROVIDER_ID, attach, curated_models};
    use crate::catalog::{CatalogDocument, CatalogModel, CatalogProvider};

    #[test]
    fn curated_levels_follow_the_measured_gateway_behaviour() {
        let models = curated_models();
        let levels = |id: &str| {
            models
                .iter()
                .find(|model| model.id == id)
                .expect("curated id")
                .reasoning_levels()
        };
        // GLM-5.3 accepts low / high / max only and has no Off switch.
        assert_eq!(levels("glm-5.3-flash"), ["low", "high", "max"]);
        // `none` passes and disables thinking on DeepSeek: Off is offered.
        let deepseek = levels("deepseek-v4-flash");
        assert_eq!(deepseek.first().map(String::as_str), Some("off"));
        assert!(deepseek.iter().any(|level| level == "xhigh"));
        // K2.7 Code always thinks and rejects max.
        let kimi = levels("kimi-k2.7-code");
        assert!(!kimi.iter().any(|level| level == "off" || level == "max"));
        // GPT-6 rejects minimal.
        assert!(!levels("gpt-6.1-sol").iter().any(|level| level == "minimal"));
    }

    #[test]
    fn a_ceiling_equal_to_the_window_keeps_room_for_the_prompt() {
        let kimi = curated_models()
            .into_iter()
            .find(|model| model.id == "kimi-k2.7-code")
            .expect("kimi");
        assert_eq!(kimi.context, 262_000);
        assert_eq!(kimi.output, 262_000 - 2_048);
    }

    #[test]
    fn attach_replaces_the_row_and_keeps_providers_sorted() {
        let row = |id: &str| CatalogModel {
            id: id.to_owned(),
            ..CatalogModel::default()
        };
        let mut document = CatalogDocument {
            providers: vec![
                CatalogProvider {
                    id: "aaa-reseller".to_owned(),
                    models: vec![row("m")],
                    ..CatalogProvider::default()
                },
                CatalogProvider {
                    id: "openrouter".to_owned(),
                    models: vec![row("m")],
                    ..CatalogProvider::default()
                },
            ],
        };
        attach(&mut document);
        let ids: Vec<&str> = document.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["aaa-reseller", PROVIDER_ID, "openrouter"]);
        let provider = document.provider(PROVIDER_ID).expect("row");
        assert_eq!(provider.base_url, BASE_URL);
        assert_eq!(provider.models.len(), curated_models().len());
        // The row resolves through the normal endpoint lookup.
        assert!(
            document
                .model_for_endpoint("opendesign-2", Some(BASE_URL), "glm-5.3-flash")
                .is_some()
        );
    }
}
