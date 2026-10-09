//! Pinned HTTP: resolve a host, check every address, then connect only to
//! those addresses. Redirects are followed manually and checked again.
//!
//! [`PinMode::PublicHttps`] is for web search and update downloads. Every hop
//! must be https on port 443. A normal host must resolve to public addresses
//! only. GitHub release hosts (`github.com`, `www.github.com`,
//! `api.github.com`, `codeload.github.com`, `uploads.github.com`, and
//! `*.githubusercontent.com`) may also return a non-public extra (DNS64, a
//! ULA, or a link-local address beside the real A/AAAA record). Those hops
//! connect only to the public addresses. An answer that is entirely the
//! benchmarking range `198.18.0.0/15` (local fake-ip DNS, including Clash
//! TUN) may connect to those addresses, for any host. Loopback, RFC1918,
//! link-local, and ULA are still refused, including a mix that contains one.
//! [`PinMode::CheckRedirect`] allows a first hop that is entirely public or
//! entirely private (local models and localhost MCP). Later hops must resolve
//! to public addresses, including a different host.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use bytes::Bytes;
use tokio_util::sync::CancellationToken;

/// How strictly each hop is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMode {
    /// https on port 443. A normal answer must be all public. GitHub release
    /// hosts may drop non-public extras. Any host may connect to a pure
    /// `198.18.0.0/15` fake-ip answer.
    PublicHttps,
    /// First hop all-public or all-private. Redirects must be all-public.
    CheckRedirect,
}

/// Classification of one DNS answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddressClass {
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
enum RedirectStep {
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
///
/// `Bytes` is reference-counted. A redirect that resends the body bumps the
/// count instead of copying the payload.
#[derive(Clone, Debug)]
pub enum PinnedBody {
    /// Raw bytes, sent as the request body.
    Bytes(Bytes),
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

/// Classifies one DNS answer as all public, all non-public, mixed, or empty.
#[must_use]
fn classify_addresses(addrs: &[IpAddr]) -> AddressClass {
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
#[cfg(test)]
fn validate_hop(mode: PinMode, hop: u32, url: &str, addrs: &[IpAddr]) -> Result<(), String> {
    connection_addresses(mode, hop, url, addrs).map(|_| ())
}

/// Addresses this hop may connect to.
///
/// GitHub release hosts drop non-public extras and keep the public ones.
/// An answer that is entirely fake-ip (`198.18.0.0/15`) may connect, for any
/// host. Loopback, RFC1918, link-local, and a mix that includes them are
/// never returned.
///
/// # Errors
///
/// Returns a visible reason when the URL or addresses violate `mode`.
pub fn connection_addresses(
    mode: PinMode,
    hop: u32,
    url: &str,
    addrs: &[IpAddr],
) -> Result<Vec<IpAddr>, String> {
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
            if class == AddressClass::AllPublic {
                return Ok(addrs.to_vec());
            }
            if github_release_host(host) {
                let public: Vec<IpAddr> = addrs
                    .iter()
                    .copied()
                    .filter(|ip| is_public_ip(*ip))
                    .collect();
                if !public.is_empty() {
                    return Ok(public);
                }
            }
            // Clash TUN and similar resolvers answer only from 198.18.0.0/15.
            // That is not a routable private LAN, so an all-fake-ip answer may
            // connect for any host. Loopback, RFC1918, link-local, and a mix
            // that includes one of them still fail closed.
            let fake: Vec<IpAddr> = addrs.iter().copied().filter(|ip| is_fake_ip(*ip)).collect();
            if !fake.is_empty() && fake.len() == addrs.len() {
                return Ok(fake);
            }
            Err(format!("resolved addresses are not all public: {host}"))
        }
        PinMode::CheckRedirect => {
            if hop == 0 {
                if !matches!(class, AddressClass::AllPublic | AddressClass::AllPrivate) {
                    return Err("resolved addresses must be all public or all private".to_owned());
                }
            } else if class != AddressClass::AllPublic {
                return Err("redirect resolved to a non-public address".to_owned());
            }
            Ok(addrs.to_vec())
        }
    }
}

/// Hosts that publish MYCode release bytes. A mixed DNS answer for one of
/// these still connects, but only to addresses [`is_public_ip`] accepts.
fn github_release_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    matches!(
        host.as_str(),
        "github.com"
            | "www.github.com"
            | "api.github.com"
            | "codeload.github.com"
            | "uploads.github.com"
    ) || host.ends_with(".githubusercontent.com")
}

/// Decides whether a response is a redirect and what the next request is.
///
/// # Errors
///
/// Returns a message when a redirect status has no usable location.
fn decide_redirect(
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
        let looked_up = lookup_addresses(&url).await?;
        let addrs = connection_addresses(request.mode, hop, &url, &looked_up)?;
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
        .connect_timeout(crate::transport::CONNECT_TIMEOUT);
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

/// `198.18.0.0/15`, the benchmarking range local fake-ip DNS uses.
fn is_fake_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            octets[0] == 198 && (octets[1] & 0xfe) == 18
        }
        IpAddr::V6(_) => false,
    }
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
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    let segments = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    // 6to4 embeds an IPv4 address. A private embedded address is not a
    // public target, even when the IPv6 prefix looks global.
    if segments[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            (segments[1] & 0xff) as u8,
            (segments[2] >> 8) as u8,
            (segments[2] & 0xff) as u8,
        );
        return is_public_ipv4(embedded);
    }
    // Unique-local fc00::/7, link-local fe80::/10, site-local fec0::/10,
    // discard 100::/8, documentation 2001:db8::/32, and Teredo 2001::/32.
    !(segments[0] & 0xfe00 == 0xfc00
        || segments[0] & 0xffc0 == 0xfe80
        || segments[0] & 0xffc0 == 0xfec0
        || segments[0] == 0x100
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x2001 && segments[1] == 0))
}

#[cfg(test)]
mod tests {
    use super::{
        AddressClass, PinMode, RedirectStep, classify_addresses, connection_addresses,
        decide_redirect, validate_hop,
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

    fn v6(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn github_release_hosts_keep_public_addresses_from_a_mixed_answer() {
        let mixed = [
            ip([140, 82, 113, 5]),
            v6("2606:50c0:8000::154"),
            v6("fd00::1"),
            v6("fe80::1"),
            ip([10, 0, 0, 1]),
        ];
        for url in [
            "https://api.github.com/repos/MCapricorns/mycode/releases/latest",
            "https://github.com/MCapricorns/mycode/releases/download/v0.9.18/app.zip",
            "https://objects.githubusercontent.com/github-production-release-asset/app.zip",
            "https://release-assets.githubusercontent.com/github-production-release-asset/app.zip",
            "https://github-releases.githubusercontent.com/app.zip",
            "https://codeload.github.com/MCapricorns/mycode/legacy.tar.gz/refs/tags/v0.9.18",
            "https://uploads.github.com/asset",
            "https://www.github.com/MCapricorns/mycode/releases",
        ] {
            let selected = connection_addresses(PinMode::PublicHttps, 0, url, &mixed).unwrap();
            assert_eq!(
                selected,
                vec![ip([140, 82, 113, 5]), v6("2606:50c0:8000::154")],
                "{url}"
            );
        }
    }

    #[test]
    fn github_release_host_with_only_private_addresses_is_rejected() {
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://api.github.com/repos/MCapricorns/mycode/releases/latest",
            &[ip([10, 1, 2, 3]), v6("fe80::1"), ip([127, 0, 0, 1])],
        )
        .unwrap_err();
        assert!(error.contains("not all public"), "{error}");
        let error = validate_hop(
            PinMode::PublicHttps,
            0,
            "https://objects.githubusercontent.com/asset",
            &[ip([192, 168, 1, 1])],
        )
        .unwrap_err();
        assert!(error.contains("not all public"), "{error}");
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://github-releases.githubusercontent.com/app.zip",
            &[ip([100, 64, 0, 1])],
        )
        .unwrap_err();
        assert!(error.contains("not all public"), "{error}");
    }

    #[test]
    fn github_filter_drops_ula_link_local_teredo_and_private_six_to_four() {
        let mixed = [
            ip([185, 199, 108, 133]),
            v6("2606:50c0:8001::154"),
            v6("fd7a:115c:a1e0::53"),
            v6("fe80::1"),
            v6("fec0::1"),
            v6("100::1"),
            v6("2001:db8::1"),
            v6("2001::1"),
            v6("ff02::1"),
            v6("2002:0a00:0001::"),
            v6("::ffff:10.1.2.3"),
            ip([198, 18, 0, 1]),
        ];
        let selected = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://objects.githubusercontent.com/github-production-release-asset/app.zip",
            &mixed,
        )
        .unwrap();
        assert_eq!(
            selected,
            vec![ip([185, 199, 108, 133]), v6("2606:50c0:8001::154")]
        );
        assert_eq!(
            classify_addresses(&[v6("2002:0808:0808::")]),
            AddressClass::AllPublic
        );
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://api.github.com/repos/MCapricorns/mycode/releases/latest",
            &[v6("2002:0a00:0001::"), v6("fe80::1")],
        )
        .unwrap_err();
        assert!(error.contains("not all public"), "{error}");
    }

    #[test]
    fn github_fake_ip_answer_can_connect_and_private_still_cannot() {
        let selected = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://api.github.com/repos/MCapricorns/mycode/releases/latest",
            &[ip([198, 18, 0, 7]), ip([198, 19, 1, 2])],
        )
        .unwrap();
        assert_eq!(selected, vec![ip([198, 18, 0, 7]), ip([198, 19, 1, 2])]);
        let selected = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://release-assets.githubusercontent.com/app.zip",
            &[ip([140, 82, 113, 5]), ip([198, 18, 4, 4]), v6("fe80::1")],
        )
        .unwrap();
        assert_eq!(selected, vec![ip([140, 82, 113, 5])]);
        assert_eq!(
            classify_addresses(&[v6("2a0a:a440::1"), ip([143, 55, 64, 1])]),
            AddressClass::AllPublic
        );
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://objects.githubusercontent.com/asset",
            &[ip([198, 18, 0, 1]), ip([10, 0, 0, 1])],
        )
        .unwrap_err();
        assert!(error.contains("not all public"), "{error}");
        let selected = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://example.com/search",
            &[ip([198, 18, 0, 1])],
        )
        .unwrap();
        assert_eq!(selected, vec![ip([198, 18, 0, 1])]);
    }

    #[test]
    fn public_https_allows_an_all_fake_ip_answer_for_any_host() {
        let selected = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://api.anysearch.com/v1/search",
            &[ip([198, 18, 0, 1]), ip([198, 19, 255, 9])],
        )
        .unwrap();
        assert_eq!(selected, vec![ip([198, 18, 0, 1]), ip([198, 19, 255, 9])]);
        let selected = connection_addresses(
            PinMode::PublicHttps,
            1,
            "https://cdn.example/page",
            &[ip([198, 18, 0, 1])],
        )
        .unwrap();
        assert_eq!(selected, vec![ip([198, 18, 0, 1])]);
    }

    #[test]
    fn public_https_still_rejects_loopback_rfc1918_and_link_local() {
        for (url, addrs) in [
            (
                "https://api.anysearch.com/v1/search",
                vec![ip([127, 0, 0, 1])],
            ),
            (
                "https://api.anysearch.com/v1/search",
                vec![ip([10, 1, 2, 3])],
            ),
            (
                "https://api.anysearch.com/v1/search",
                vec![ip([172, 16, 0, 4])],
            ),
            (
                "https://api.anysearch.com/v1/search",
                vec![ip([192, 168, 0, 8])],
            ),
            ("https://example.com/search", vec![ip([169, 254, 1, 1])]),
            ("https://example.com/search", vec![v6("fe80::1")]),
            ("https://example.com/search", vec![v6("fd00::1")]),
        ] {
            let error = connection_addresses(PinMode::PublicHttps, 0, url, &addrs).unwrap_err();
            assert!(
                error.starts_with("resolved addresses are not all public:"),
                "{url} {error}"
            );
            let host = url
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap_or("");
            assert!(error.contains(host), "{error}");
        }
    }

    #[test]
    fn public_https_rejects_a_mixed_answer_that_includes_a_private_address() {
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://api.anysearch.com/v1/search",
            &[ip([198, 18, 0, 1]), ip([10, 0, 0, 1])],
        )
        .unwrap_err();
        assert_eq!(
            error,
            "resolved addresses are not all public: api.anysearch.com"
        );
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://api.anysearch.com/v1/search",
            &[ip([198, 18, 0, 1]), ip([127, 0, 0, 1])],
        )
        .unwrap_err();
        assert!(
            error.contains("not all public: api.anysearch.com"),
            "{error}"
        );
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://example.com/search",
            &[ip([1, 1, 1, 1]), ip([192, 168, 1, 1])],
        )
        .unwrap_err();
        assert_eq!(error, "resolved addresses are not all public: example.com");
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://example.com/search",
            &[ip([1, 1, 1, 1]), ip([169, 254, 9, 9])],
        )
        .unwrap_err();
        assert!(error.contains("not all public: example.com"), "{error}");
    }

    #[test]
    fn non_github_mixed_answer_stays_rejected() {
        let error = connection_addresses(
            PinMode::PublicHttps,
            0,
            "https://example.com/search",
            &[ip([1, 1, 1, 1]), ip([10, 0, 0, 1])],
        )
        .unwrap_err();
        assert!(error.contains("not all public"), "{error}");
        let error = validate_hop(
            PinMode::PublicHttps,
            0,
            "https://evil.githubusercontent.com.example/asset",
            &[ip([1, 1, 1, 1]), ip([10, 0, 0, 1])],
        )
        .unwrap_err();
        assert!(error.contains("not all public"), "{error}");
    }

    #[test]
    fn private_literal_and_link_local_are_not_public_targets() {
        let error = validate_hop(
            PinMode::PublicHttps,
            0,
            "https://10.0.0.1/secret",
            &[ip([10, 0, 0, 1])],
        )
        .unwrap_err();
        assert!(error.contains("not public"), "{error}");
        assert_eq!(
            classify_addresses(&[v6("2606:50c0:8003::154"), ip([185, 199, 108, 133])]),
            AddressClass::AllPublic
        );
        assert_eq!(
            classify_addresses(&[v6("fd7a:115c:a1e0::1"), ip([169, 254, 1, 1])]),
            AddressClass::AllPrivate
        );
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
