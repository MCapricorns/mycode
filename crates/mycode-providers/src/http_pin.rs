//! Pinned HTTP: resolve a host, check every address, then connect only to
//! those addresses. Redirects are followed manually and checked again.
//!
//! [`PinMode::PublicHttps`] is for web search and update downloads. Every hop
//! must be https on port 443 with only public addresses.
//! [`PinMode::CheckRedirect`] allows a first hop that is entirely public or
//! entirely private (local models and localhost MCP). Later hops must resolve
//! to public addresses, including a different host.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// How strictly each hop is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMode {
    /// https, port 443, public host, all resolved addresses public.
    PublicHttps,
    /// First hop all-public or all-private. Redirects must be all-public.
    CheckRedirect,
}

/// Classification of one DNS answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressClass {
    /// Every address is public.
    AllPublic,
    /// Every address is non-public.
    AllPrivate,
    /// The answer mixes public and non-public addresses.
    Mixed,
    /// DNS returned nothing.
    Empty,
}

/// What to do with one HTTP status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedirectStep {
    /// This response is not a redirect the client should follow.
    Stop,
    /// Follow `url`. `become_get` drops the body and switches to GET.
    Follow {
        /// Absolute next URL.
        url: String,
        /// 301, 302, and 303 become GET.
        become_get: bool,
    },
}

/// Body carried across a pinned request.
#[derive(Clone, Debug)]
pub enum PinnedBody {
    /// Raw bytes, sent as the request body.
    Bytes(Vec<u8>),
    /// Form fields, encoded by reqwest.
    Form(Vec<(String, String)>),
}

/// One pinned request.
pub struct PinnedRequest {
    /// HTTP method for the first hop.
    pub method: reqwest::Method,
    /// Absolute URL.
    pub url: String,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// Optional body.
    pub body: Option<PinnedBody>,
    /// Pin policy.
    pub mode: PinMode,
    /// Overall request timeout.
    pub timeout: Option<Duration>,
    /// User-Agent, when the caller has one.
    pub user_agent: Option<String>,
    /// Cancellation for the redirect loop.
    pub cancel: CancellationToken,
}

const MAX_REDIRECTS: u32 = 5;

/// Classifies resolved addresses. Mixed and empty answers are refused.
#[must_use]
pub fn classify_addresses(addrs: &[IpAddr]) -> AddressClass {
    if addrs.is_empty() {
        return AddressClass::Empty;
    }
    let public = addrs.iter().all(|ip| is_public_ip(*ip));
    let private = addrs.iter().all(|ip| !is_public_ip(*ip));
    if public {
        AddressClass::AllPublic
    } else if private {
        AddressClass::AllPrivate
    } else {
        AddressClass::Mixed
    }
}

/// Checks one hop before a connection is opened.
///
/// # Errors
///
/// Returns a visible reason when the URL or addresses violate `mode`.
pub fn validate_hop(mode: PinMode, hop: u32, url: &str, addrs: &[IpAddr]) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("url is invalid: {url}"))?;
    let class = classify_addresses(addrs);
    match mode {
        PinMode::PublicHttps => {
            if parsed.scheme() != "https" {
                return Err("public requests must use https".to_owned());
            }
            if parsed.port().is_some_and(|port| port != 443) {
                return Err("public requests must use port 443".to_owned());
            }
            let host = parsed.host_str().unwrap_or("");
            if !public_host(host) {
                return Err(format!("host is not public: {host}"));
            }
            if class != AddressClass::AllPublic {
                return Err("resolved addresses are not all public".to_owned());
            }
        }
        PinMode::CheckRedirect => {
            if hop == 0 {
                if !matches!(class, AddressClass::AllPublic | AddressClass::AllPrivate) {
                    return Err("resolved addresses must be all public or all private".to_owned());
                }
            } else if class != AddressClass::AllPublic {
                return Err("redirect resolved to a non-public address".to_owned());
            }
        }
    }
    Ok(())
}

/// Decides whether a response is a redirect and what the next request is.
///
/// # Errors
///
/// Returns a message when a redirect status has no usable location.
pub fn decide_redirect(
    status: u16,
    location: Option<&str>,
    current: &str,
) -> Result<RedirectStep, String> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return Ok(RedirectStep::Stop);
    }
    let Some(location) = location.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err("redirect is missing a location".to_owned());
    };
    let base = reqwest::Url::parse(current).map_err(|_| "current url is invalid".to_owned())?;
    let next = base
        .join(location)
        .map_err(|_| "redirect location is invalid".to_owned())?;
    Ok(RedirectStep::Follow {
        url: next.to_string(),
        become_get: matches!(status, 301..=303),
    })
}

/// Sends `request`, pinning DNS and re-checking every redirect.
///
/// # Errors
///
/// Returns a transport, DNS, pin, or redirect failure. The response body is
/// left unread so streaming callers can consume it.
pub async fn send_pinned(request: PinnedRequest) -> Result<reqwest::Response, String> {
    let mut method = request.method;
    let mut url = request.url;
    let mut body = request.body;
    for hop in 0..MAX_REDIRECTS {
        if request.cancel.is_cancelled() {
            return Err("request cancelled".to_owned());
        }
        let addrs = lookup_addresses(&url).await?;
        validate_hop(request.mode, hop, &url, &addrs)?;
        let response = tokio::select! {
            biased;
            () = request.cancel.cancelled() => return Err("request cancelled".to_owned()),
            sent = send_once(
                method.clone(),
                &url,
                &request.headers,
                body.as_ref(),
                &addrs,
                request.timeout,
                request.user_agent.as_deref(),
            ) => sent?,
        };
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        match decide_redirect(status, location.as_deref(), &url)? {
            RedirectStep::Stop => return Ok(response),
            RedirectStep::Follow {
                url: next,
                become_get,
            } => {
                url = next;
                if become_get {
                    method = reqwest::Method::GET;
                    body = None;
                }
            }
        }
    }
    Err("too many redirects".to_owned())
}

async fn lookup_addresses(url: &str) -> Result<Vec<IpAddr>, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("url is invalid: {url}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "url has no host".to_owned())?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let mut addrs = Vec::new();
    let looked_up = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("dns lookup failed for {host}: {error}"))?;
    for socket in looked_up {
        if !addrs.contains(&socket.ip()) {
            addrs.push(socket.ip());
        }
    }
    Ok(addrs)
}

async fn send_once(
    method: reqwest::Method,
    url: &str,
    headers: &[(String, String)],
    body: Option<&PinnedBody>,
    addrs: &[IpAddr],
    timeout: Option<Duration>,
    user_agent: Option<&str>,
) -> Result<reqwest::Response, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("url is invalid: {url}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "url has no host".to_owned())?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let sockets: Vec<SocketAddr> = addrs.iter().map(|ip| SocketAddr::new(*ip, port)).collect();
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10));
    if let Some(user_agent) = user_agent {
        builder = builder.user_agent(user_agent);
    }
    if !sockets.is_empty() {
        builder = builder.resolve_to_addrs(host, &sockets);
    }
    let client = builder
        .build()
        .map_err(|error| format!("http client unavailable: {error}"))?;
    let mut request = client.request(method, url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    request = match body {
        Some(PinnedBody::Bytes(bytes)) => request.body(bytes.clone()),
        Some(PinnedBody::Form(fields)) => request.form(fields),
        None => request,
    };
    request
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))
}

fn public_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    if lower.is_empty()
        || matches!(lower.as_str(), "localhost" | "localhost.localdomain")
        || lower.ends_with(".local")
    {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_public_ip(ip);
    }
    true
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
    {
        return false;
    }
    !(octets[0] == 0
        || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 198 && (octets[1] & 0xfe) == 18)
        || octets[0] >= 240)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    let segments = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    !(segments[0] & 0xfe00 == 0xfc00
        || segments[0] & 0xffc0 == 0xfe80
        || segments[0] == 0x2001 && segments[1] == 0xdb8)
}

#[cfg(test)]
mod tests {
    use super::{
        AddressClass, PinMode, RedirectStep, classify_addresses, decide_redirect, validate_hop,
    };
    use std::net::IpAddr;

    fn ip(value: [u8; 4]) -> IpAddr {
        IpAddr::from(value)
    }

    #[test]
    fn private_redirect_is_rejected() {
        let error = validate_hop(
            PinMode::CheckRedirect,
            1,
            "http://127.0.0.1/secret",
            &[ip([127, 0, 0, 1])],
        )
        .unwrap_err();
        assert!(error.contains("non-public"), "{error}");
    }

    #[test]
    fn public_different_host_is_allowed() {
        validate_hop(
            PinMode::PublicHttps,
            1,
            "https://objects.githubusercontent.com/asset",
            &[ip([1, 1, 1, 1])],
        )
        .unwrap();
        validate_hop(
            PinMode::CheckRedirect,
            1,
            "https://cdn.example/asset",
            &[ip([8, 8, 8, 8])],
        )
        .unwrap();
    }

    #[test]
    fn mixed_and_empty_answers_fail() {
        assert_eq!(
            classify_addresses(&[ip([1, 1, 1, 1]), ip([10, 0, 0, 1])]),
            AddressClass::Mixed
        );
        assert_eq!(classify_addresses(&[]), AddressClass::Empty);
        assert!(
            validate_hop(
                PinMode::CheckRedirect,
                0,
                "https://example.com",
                &[ip([1, 1, 1, 1]), ip([10, 0, 0, 1])],
            )
            .is_err()
        );
        assert!(validate_hop(PinMode::PublicHttps, 0, "https://example.com", &[]).is_err());
    }

    #[test]
    fn https_only_rejects_http_and_odd_ports() {
        assert!(
            validate_hop(
                PinMode::PublicHttps,
                0,
                "http://example.com",
                &[ip([1, 1, 1, 1])]
            )
            .is_err()
        );
        assert!(
            validate_hop(
                PinMode::PublicHttps,
                0,
                "https://example.com:8443/x",
                &[ip([1, 1, 1, 1])],
            )
            .is_err()
        );
    }

    #[test]
    fn first_hop_may_be_private() {
        validate_hop(
            PinMode::CheckRedirect,
            0,
            "http://127.0.0.1:11434/v1/chat/completions",
            &[ip([127, 0, 0, 1])],
        )
        .unwrap();
    }

    #[test]
    fn redirect_statuses_choose_method_and_url() {
        match decide_redirect(302, Some("/next"), "https://example.com/a").unwrap() {
            RedirectStep::Follow { url, become_get } => {
                assert_eq!(url, "https://example.com/next");
                assert!(become_get);
            }
            RedirectStep::Stop => panic!("302 should follow"),
        }
        match decide_redirect(
            307,
            Some("https://cdn.example/file"),
            "https://example.com/a",
        )
        .unwrap()
        {
            RedirectStep::Follow { url, become_get } => {
                assert_eq!(url, "https://cdn.example/file");
                assert!(!become_get);
            }
            RedirectStep::Stop => panic!("307 should follow"),
        }
        assert!(matches!(
            decide_redirect(200, None, "https://example.com").unwrap(),
            RedirectStep::Stop
        ));
        assert!(decide_redirect(301, None, "https://example.com").is_err());
    }
}
