//! Outbound proxy routing for pinned HTTP.
//!
//! The settings document chooses a [`ProxyPreference`]; the app installs it
//! with [`set_proxy_preference`] whenever settings load or save. Each hop
//! then asks [`route_for`] how to leave the machine:
//!
//! - `Direct` ignores every proxy.
//! - `Url` sends every public host through that proxy.
//! - `Auto` follows the system proxy (environment variables, or the OS
//!   setting Clash's "system proxy" switch writes) when it covers the host.
//!   Otherwise the hop goes direct, and a public host that cannot be reached
//!   (DNS failure, refused or reset connection, connect timeout) is retried
//!   through a local Clash-style port. A host that needed the proxy keeps
//!   using it for the rest of the process, until the proxy itself stops
//!   answering.
//!
//! Loopback, `.local`, and literal private addresses never use a proxy, so
//! local models keep working while a proxy is on.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use mycode_config::ProxyPreference;

/// Mixed/HTTP ports of common local proxy clients, probed in order:
/// Clash and Clash for Windows / ClashX (7890), Clash Verge Rev (7897),
/// v2rayN (10809).
const LOCAL_PROXY_PORTS: [u16; 3] = [7890, 7897, 10809];
/// How long one loopback probe may take. A listening port answers at once.
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
/// How long a probe result is reused before the ports are checked again.
const PROBE_TTL: Duration = Duration::from_secs(30);
/// Hosts remembered as needing the proxy. The map is cleared when full.
const MAX_LEARNED_HOSTS: usize = 256;

static PREFERENCE: RwLock<ProxyPreference> = RwLock::new(ProxyPreference::Auto);
static LEARNED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static PROBE: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);

/// Installs the proxy preference every later request follows.
pub fn set_proxy_preference(preference: ProxyPreference) {
    let mut current = PREFERENCE.write().unwrap_or_else(PoisonError::into_inner);
    if *current != preference {
        *current = preference;
        LEARNED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

fn preference() -> ProxyPreference {
    PREFERENCE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
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

/// First route for a hop to `url`.
pub(crate) fn route_for(url: &reqwest::Url) -> Route {
    let Some(host) = url.host_str() else {
        return Route::Direct;
    };
    if !crate::http_pin::public_host(host) {
        return Route::Direct;
    }
    match preference() {
        ProxyPreference::Direct => Route::Direct,
        ProxyPreference::Url(proxy) => Route::Via(proxy),
        ProxyPreference::Auto => {
            if let Some(proxy) = system_proxy(url) {
                Route::System(proxy)
            } else if let Some(proxy) = learned(host) {
                Route::Via(proxy)
            } else {
                Route::Direct
            }
        }
    }
}

/// The local proxy to retry a public host through after a direct hop could
/// not connect. Only `Auto` falls back. The host is remembered on success of
/// the probe, so later hops go straight to the proxy.
pub(crate) async fn fallback_for(host: &str) -> Option<String> {
    if preference() != ProxyPreference::Auto || !crate::http_pin::public_host(host) {
        return None;
    }
    let proxy = local_proxy().await?;
    let mut learned = LEARNED.lock().unwrap_or_else(PoisonError::into_inner);
    let hosts = learned.get_or_insert_with(HashMap::new);
    if hosts.len() >= MAX_LEARNED_HOSTS {
        hosts.clear();
    }
    hosts.insert(host.to_ascii_lowercase(), proxy.clone());
    Some(proxy)
}

/// Drops a remembered proxy for `host` after that proxy stopped answering,
/// so the next hop tries direct again.
pub(crate) fn forget(host: &str) {
    if let Some(hosts) = LEARNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        hosts.remove(&host.to_ascii_lowercase());
    }
    PROBE.lock().unwrap_or_else(PoisonError::into_inner).take();
}

fn learned(host: &str) -> Option<String> {
    LEARNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()?
        .get(&host.to_ascii_lowercase())
        .cloned()
}

/// The proxy the environment or OS settings send `url` to, if any. This is
/// the same matcher reqwest's system-proxy detection uses.
fn system_proxy(url: &reqwest::Url) -> Option<String> {
    let uri = url.as_str().parse::<http::Uri>().ok()?;
    hyper_util::client::proxy::matcher::Matcher::from_system()
        .intercept(&uri)
        .map(|intercept| intercept.uri().to_string())
}

/// The first listening local proxy port, cached for [`PROBE_TTL`].
async fn local_proxy() -> Option<String> {
    if let Some((at, found)) = PROBE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        && at.elapsed() < PROBE_TTL
    {
        return found.clone();
    }
    let mut found = None;
    for port in LOCAL_PROXY_PORTS {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if let Ok(Ok(_)) =
            tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(address)).await
        {
            found = Some(format!("http://127.0.0.1:{port}"));
            break;
        }
    }
    *PROBE.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), found.clone()));
    found
}

#[cfg(test)]
mod tests {
    use super::Route;

    #[test]
    fn local_hosts_never_route_through_a_proxy() {
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/",
            "http://192.168.1.4:1234/",
            "http://box.local/",
        ] {
            let parsed = reqwest::Url::parse(url).expect("url");
            assert_eq!(super::route_for(&parsed), Route::Direct, "{url}");
        }
    }

    #[test]
    fn only_direct_skips_the_proxy() {
        assert!(!Route::Direct.proxied());
        assert!(Route::System("http://127.0.0.1:7890/".to_owned()).proxied());
        assert!(Route::Via("http://127.0.0.1:7890".to_owned()).proxied());
    }
}
