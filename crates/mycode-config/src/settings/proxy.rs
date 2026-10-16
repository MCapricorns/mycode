//! Outbound proxy preference (`proxy` in `settings.json`).
//!
//! The stored value is one string: empty or `auto` for the smart default,
//! `direct` to ignore every proxy, or a proxy URL. A bare `host:port` (what
//! Clash shows as its port) reads as an `http://` proxy.

use super::AppSettings;

/// Stored value of the smart default. An empty field means the same.
pub const PROXY_AUTO: &str = "auto";
/// Stored value that turns every proxy off.
pub const PROXY_DIRECT: &str = "direct";
/// Proxy URL schemes reqwest connects through.
pub const PROXY_SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

/// How outbound HTTP leaves this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProxyPreference {
    /// The system proxy (environment or OS settings, which Clash's "system
    /// proxy" switch writes) when one covers the host. Otherwise direct, and
    /// a public host that cannot be reached directly is retried through a
    /// local Clash-style port when one is listening.
    #[default]
    Auto,
    /// Never use a proxy, including the system one.
    Direct,
    /// Send every public request through this normalized URL.
    Url(String),
}

impl ProxyPreference {
    /// Parses a stored or typed value. `None` when the value is not empty,
    /// `auto`, `direct`, or a proxy URL with a host.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case(PROXY_AUTO) {
            return Some(Self::Auto);
        }
        if value.eq_ignore_ascii_case(PROXY_DIRECT) {
            return Some(Self::Direct);
        }
        normalize_proxy_url(value).map(Self::Url)
    }
}

/// `scheme://[user:pass@]host[:port]` with a lowercase known scheme and no
/// path. A missing scheme reads as `http://`.
fn normalize_proxy_url(value: &str) -> Option<String> {
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return None;
    }
    let (scheme, rest) = match value.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("http".to_owned(), value),
    };
    if !PROXY_SCHEMES.contains(&scheme.as_str()) {
        return None;
    }
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains(['/', '?', '#']) {
        return None;
    }
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host_port.rsplit_once(':') {
        // `[::1]:7890` or `host:7890`; a bracketed IPv6 literal without a
        // port ends in `]`.
        Some((host, port)) if !host_port.ends_with(']') => {
            port.parse::<u16>().ok().filter(|port| *port != 0)?;
            host
        }
        _ => host_port,
    };
    if host.is_empty() || host == "[]" {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

impl AppSettings {
    /// The proxy preference this document asks for. An unparsable value
    /// reads as [`ProxyPreference::Auto`]; [`Self::validate`] rejects it
    /// before a document is published.
    #[must_use]
    pub fn proxy_preference(&self) -> ProxyPreference {
        ProxyPreference::parse(&self.proxy).unwrap_or_default()
    }

    pub(super) fn validate_proxy(&self) -> Result<(), &'static str> {
        super::bounded_text(&self.proxy, super::MAX_FIELD_BYTES)
            .map_err(|_| "proxy: too long or contains line breaks or NUL")?;
        ProxyPreference::parse(&self.proxy).map(|_| ()).ok_or(
            "proxy: must be empty, auto, direct, or a http, https, socks5, or socks5h proxy URL",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ProxyPreference;

    #[test]
    fn empty_and_auto_are_the_smart_default() {
        assert_eq!(ProxyPreference::parse(""), Some(ProxyPreference::Auto));
        assert_eq!(
            ProxyPreference::parse("  AUTO "),
            Some(ProxyPreference::Auto)
        );
        assert_eq!(
            ProxyPreference::parse("Direct"),
            Some(ProxyPreference::Direct)
        );
    }

    #[test]
    fn clash_ports_and_urls_normalize() {
        let url = |value: &str| ProxyPreference::Url(value.to_owned());
        assert_eq!(
            ProxyPreference::parse("127.0.0.1:7890"),
            Some(url("http://127.0.0.1:7890"))
        );
        assert_eq!(
            ProxyPreference::parse("SOCKS5H://127.0.0.1:7891/"),
            Some(url("socks5h://127.0.0.1:7891"))
        );
        assert_eq!(
            ProxyPreference::parse("http://user:pw@[::1]:7897"),
            Some(url("http://user:pw@[::1]:7897"))
        );
        assert_eq!(
            ProxyPreference::parse("https://proxy.example"),
            Some(url("https://proxy.example"))
        );
    }

    #[test]
    fn malformed_proxies_are_rejected() {
        for value in [
            "ftp://127.0.0.1:21",
            "http://",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:7890/path",
            "http://127.0.0.1 :7890",
            ":7890",
        ] {
            assert_eq!(ProxyPreference::parse(value), None, "{value}");
        }
    }
}
