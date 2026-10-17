//! The OpenAI Codex subscription backend (`https://chatgpt.com/backend-api/codex`).
//!
//! models.dev does not carry the Codex rows, so the provider row is fully
//! hand-maintained in [`super::openai_codex_preset`]. Update that list and
//! its effort ladders when the backend gains or retires a model — the
//! update procedure lives in `docs/model-catalog-updates.md`.
use super::CatalogDocument;

/// Provider id used in settings and the catalog.
pub const PROVIDER_ID: &str = "openai-codex";

/// The GPT-5.6, GPT-6, and GPT-6.1 series are the only families the
/// subscription backend still serves usefully; older series are retired
/// from the preset. Kept for the tests that pin the preset to it.
#[cfg(test)]
pub(crate) fn usable_family(id: &str) -> bool {
    ["gpt-5.6-", "gpt-6-", "gpt-6.1-"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
        || matches!(id, "gpt-5.6" | "gpt-6")
}

/// Replaces or inserts the Codex row of `document` with the preset list.
pub fn attach(document: &mut CatalogDocument) {
    let row = super::openai_codex_preset();
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
    use super::{attach, usable_family};
    use crate::catalog::{CatalogDocument, CatalogProvider};

    #[test]
    fn usable_family_covers_the_current_series_only() {
        for id in [
            "gpt-5.6-sol",
            "gpt-5.6-luna",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6.1-sol",
            "gpt-6",
        ] {
            assert!(usable_family(id), "{id}");
        }
        for id in [
            "gpt-5.3-codex",
            "gpt-5.4",
            "gpt-5.5",
            "o3",
            "o3-pro",
            "gpt-60-x",
        ] {
            assert!(!usable_family(id), "{id}");
        }
    }

    #[test]
    fn attach_replaces_the_row_with_the_preset() {
        let mut document = CatalogDocument {
            providers: vec![CatalogProvider {
                id: "openai-codex".to_owned(),
                ..CatalogProvider::default()
            }],
        };
        attach(&mut document);
        let row = document.provider("openai-codex").expect("codex row");
        assert_eq!(
            row.models.len(),
            crate::catalog::openai_codex_preset().models.len()
        );
        assert_eq!(row.kind, crate::catalog::KIND_OPENAI_RESPONSES);
    }
}
