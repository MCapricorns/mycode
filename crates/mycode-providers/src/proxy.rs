//! Outbound proxy routing for pinned HTTP.
//!
//! The settings document chooses a [`ProxyPreference`]; the app installs it
//! with [`set_proxy_preference`] whenever settings load or save. Each hop
//! then asks [`route_for`] how to leave the machine:
//!
//! - `System` follows the system proxy (environment variables, or the OS
//!   setting Clash's "system proxy" switch writes) when it covers the host,
//!   and connects directly otherwise. Nothing is probed or retried behind
//!   the user's back.
//! - `Url` sends every public host through that proxy.
//!
//! Loopback, `.local`, and literal private addresses never use a proxy, so
//! local models keep working while a proxy is on.
//!
//! [`test_proxy`] answers "is the proxy actually working": it fetches a
//! fixed probe URL on the route a preference picks and reports the route,
//! the status, and the round trip.

use parking_lot::RwLock;
use std::time::{Duration, Instant};

use mycode_config::ProxyPreference;

/// Fetched by [`test_proxy`]. Answers 204 with no body, and is blocked in
/// the networks where a proxy is needed at all.
pub const PROXY_TEST_URL: &str = "https://www.google.com/generate_204";
/// Whole-request ceiling for one proxy test.
const PROXY_TEST_TIMEOUT: Duration = Duration::from_secs(10);

static PREFERENCE: RwLock<ProxyPreference> = RwLock::new(ProxyPreference::System);

/// Installs the proxy preference every later request follows.
pub fn set_proxy_preference(preference: ProxyPreference) {
    *PREFERENCE.write() = preference;
}

fn preference() -> ProxyPreference {
    PREFERENCE.read().clone()
}

/// How one hop leaves the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// No proxy; the hop connects to its pinned addresses.
    Direct,
    /// reqwest's own system-proxy detection, which matched this host. The
    /// matched proxy URI keys the client cache, so a changed system proxy
    /// builds a fresh client.
    System(String),
    /// This proxy URL.
    Via(String),
}

impl Route {
    pub(crate) fn proxied(&self) -> bool {
        !matches!(self, Self::Direct)
    }

    /// Points `builder` at this route.
    ///
    /// # Errors
    ///
    /// Returns a visible reason when the proxy URL is unusable.
    pub(crate) fn apply(
        &self,
        builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::ClientBuilder, String> {
        match self {
            Self::Direct => Ok(builder.no_proxy()),
            Self::System(_) => Ok(builder),
            Self::Via(url) => reqwest::Proxy::all(url.as_str())
                .map(|proxy| builder.proxy(proxy))
                .map_err(|error| format!("proxy is invalid: {error}")),
        }
    }
}

/// Route for a hop to `url` under the installed preference.
pub(crate) fn route_for(url: &reqwest::Url) -> Route {
    route_with(&preference(), url)
}

fn route_with(preference: &ProxyPreference, url: &reqwest::Url) -> Route {
    let Some(host) = url.host_str() else {
        return Route::Direct;
    };
    if !crate::http_pin::public_host(host) {
        return Route::Direct;
    }
    match preference {
        ProxyPreference::Url(proxy) => Route::Via(proxy.clone()),
        ProxyPreference::System => system_proxy(url).map_or(Route::Direct, Route::System),
    }
}

/// The proxy the environment or OS settings send `url` to, if any. This is
/// the same matcher reqwest's system-proxy detection uses.
fn system_proxy(url: &reqwest::Url) -> Option<String> {
    let uri = url.as_str().parse::<http::Uri>().ok()?;
    hyper_util::client::proxy::matcher::Matcher::from_system()
        .intercept(&uri)
        .map(|intercept| intercept.uri().to_string())
}

/// Which way a proxy test left the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyTestRoute {
    /// No proxy applied: the system has none for this host.
    Direct,
    /// The system proxy at this URI.
    System(String),
    /// The proxy URL the user typed.
    Manual(String),
}

/// Outcome of one [`test_proxy`] run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyTestReport {
    /// The route the request took.
    pub route: ProxyTestRoute,
    /// HTTP status and round trip on success, or why it failed.
    pub outcome: Result<(u16, Duration), String>,
}

/// Fetches [`PROXY_TEST_URL`] on the route `preference` picks, so the user
/// sees whether the proxy is in effect and actually reaches the internet.
/// ICMP ping cannot answer that: it never goes through an HTTP proxy.
pub async fn test_proxy(preference: &ProxyPreference) -> ProxyTestReport {
    let url = reqwest::Url::parse(PROXY_TEST_URL).expect("static probe url parses");
    let route = route_with(preference, &url);
    let reported = match &route {
        Route::Direct => ProxyTestRoute::Direct,
        Route::System(uri) => ProxyTestRoute::System(uri.clone()),
        Route::Via(proxy) => ProxyTestRoute::Manual(proxy.clone()),
    };
    let outcome = async {
        let client = route
            .apply(
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(crate::transport::CONNECT_TIMEOUT)
                    .timeout(PROXY_TEST_TIMEOUT),
            )?
            .build()
            .map_err(|error| format!("client failed to start: {error}"))?;
        let started = Instant::now();
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| format!("request failed: {error}"))?;
        Ok((response.status().as_u16(), started.elapsed()))
    }
    .await;
    ProxyTestReport {
        route: reported,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::{Route, route_with};
    use mycode_config::ProxyPreference;

    #[test]
    fn local_hosts_never_route_through_a_proxy() {
        let manual = ProxyPreference::Url("http://127.0.0.1:7890".to_owned());
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/",
            "http://192.168.1.4:1234/",
            "http://box.local/",
        ] {
            let parsed = reqwest::Url::parse(url).expect("url");
            assert_eq!(route_with(&manual, &parsed), Route::Direct, "{url}");
        }
    }

    #[test]
    fn a_manual_proxy_carries_every_public_host() {
        let manual = ProxyPreference::Url("socks5h://127.0.0.1:7891".to_owned());
        let parsed = reqwest::Url::parse("https://api.openai.com/v1").expect("url");
        assert_eq!(
            route_with(&manual, &parsed),
            Route::Via("socks5h://127.0.0.1:7891".to_owned())
        );
    }

    #[test]
    fn only_direct_skips_the_proxy() {
        assert!(!Route::Direct.proxied());
        assert!(Route::System("http://127.0.0.1:7890/".to_owned()).proxied());
        assert!(Route::Via("http://127.0.0.1:7890".to_owned()).proxied());
    }
}
