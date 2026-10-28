//! The local magpie gateway: its model list and the one provider row MCode
//! runs on.
//!
//! magpie runs the gateway at `127.0.0.1` and writes `settings.magpie` through
//! its agent plugin. MCode ships no vendors of its own: the catalog, the key,
//! the model, and the MCP servers all come from magpie.

use std::time::Duration;

use mycode_agent::catalog::{CatalogDocument, CatalogProvider};
use mycode_agent::{MAGPIE_PROVIDER_ID, MagpieSettings, ProviderSettings};

/// Request timeout for the gateway's model list. It answers locally.
const MODELS_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest `/v1/models` body read.
const MAX_MODELS_BYTES: usize = 4 * 1024 * 1024;

/// The gateway binding one request runs against.
#[derive(Clone, Debug)]
pub(crate) struct Gateway {
    base_url: String,
    api_key: String,
}

/// The model list fetched from one gateway, with the time it was read.
pub(crate) struct FetchedModels {
    pub(crate) provider: CatalogProvider,
    pub(crate) fetched_at: u64,
}

impl Gateway {
    /// Binds one enabled magpie section.
    pub(crate) fn from_settings(magpie: &MagpieSettings) -> Result<Self, String> {
        if !magpie.is_active() {
            return Err("magpie is not connected: pick a model in magpie first".to_owned());
        }
        Ok(Self {
            base_url: magpie.endpoint_base().to_owned(),
            api_key: magpie.api_key.clone(),
        })
    }

    /// `GET /v1/models` as a provider row.
    pub(crate) async fn fetch_models(&self) -> Result<FetchedModels, String> {
        let client = reqwest::Client::builder()
            .timeout(MODELS_TIMEOUT)
            .build()
            .map_err(|error| format!("magpie client: {error}"))?;
        let url = format!("{}/models", self.base_url);
        let response = client
            .get(&url)
            .header("authorization", format!("Bearer {}", self.api_key))
            .header(
                "user-agent",
                format!("mycode-magpie/{}", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|error| {
                format!(
                    "magpie gateway is not answering at {}: {error}",
                    self.base_url
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("magpie /v1/models answered {status}"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("magpie /v1/models body: {error}"))?;
        if bytes.len() > MAX_MODELS_BYTES {
            return Err("magpie /v1/models is larger than 4 MiB".to_owned());
        }
        let provider = mycode_agent::catalog::magpie::provider_from_models(&bytes, &self.base_url)?;
        Ok(FetchedModels {
            provider,
            fetched_at: mycode_app_time(),
        })
    }
}

/// Reads `settings.json` off the core thread.
pub(crate) async fn load_settings(
    home: &mycode_agent::HomeLayout,
) -> Result<mycode_agent::AppSettings, String> {
    let read_home = home.clone();
    tokio::task::spawn_blocking(move || {
        mycode_agent::read_app_settings(&read_home)
            .map_err(|error| crate::settings_io::render_config_error(&error))
    })
    .await
    .map_err(|error| format!("settings task failed: {error}"))?
}

fn mycode_app_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The provider row every turn resolves against.
///
/// The gateway list wins; with no list yet (offline), the selected model is
/// still offered so the request fails with the gateway's own connection
/// error instead of "model not offered".
///
/// Frontends use the same projection after loading the binding and catalog.
#[must_use]
pub fn provider_row(
    magpie: &MagpieSettings,
    catalog: &CatalogDocument,
) -> Option<ProviderSettings> {
    if !magpie.is_active() {
        return None;
    }
    let mut models: Vec<String> = catalog
        .provider(MAGPIE_PROVIDER_ID)
        .map(|provider| {
            provider
                .models
                .iter()
                .map(|model| model.id.clone())
                .collect()
        })
        .unwrap_or_default();
    if !models.iter().any(|model| model == &magpie.model) {
        models.insert(0, magpie.model.clone());
    }
    Some(ProviderSettings::magpie(
        magpie.endpoint_base(),
        models,
        magpie.hidden_models.clone(),
    ))
}
