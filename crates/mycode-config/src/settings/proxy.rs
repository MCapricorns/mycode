//! Outbound proxy preference (`proxy` in `settings.json`).
//!
//! The stored value is one string: empty to follow the system proxy, or a
//! proxy URL the user typed. A bare `host:port` (what Clash shows as its
//! port) reads as an `http://` proxy. The retired `auto` and `direct`
//! values from older documents read as the system mode.

use super::AppSettings;

/// Stored values from older documents that now mean the system mode.
const LEGACY_SYSTEM_VALUES: [&str; 3] = ["auto", "direct", "system"];
/// Proxy URL schemes reqwest connects through.
pub const PROXY_SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

/// How outbound HTTP leaves this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProxyPreference {
    /// Exactly what the system says: the environment variables or the OS
    /// proxy setting (which Clash's "system proxy" switch writes) when one
    /// covers the host, otherwise direct.
    #[default]
    System,
    /// Send every public request through this normalized URL.
    Url(String),
}

impl ProxyPreference {
    /// Parses a stored or typed value. `None` when the value is neither
    /// empty, a legacy mode word, nor a proxy URL with a host.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty()
            || LEGACY_SYSTEM_VALUES
                .iter()
                .any(|legacy| value.eq_ignore_ascii_case(legacy))
        {
            return Some(Self::System);
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
    /// reads as [`ProxyPreference::System`]; [`Self::validate`] rejects it
    /// before a document is published.
    #[must_use]
    pub fn proxy_preference(&self) -> ProxyPreference {
        ProxyPreference::parse(&self.proxy).unwrap_or_default()
    }

    pub(super) fn validate_proxy(&self) -> Result<(), &'static str> {
        super::bounded_text(&self.proxy, super::MAX_FIELD_BYTES)
            .map_err(|_| "proxy: too long or contains line breaks or NUL")?;
        ProxyPreference::parse(&self.proxy).map(|_| ()).ok_or(
            "proxy: must be empty (system proxy) or a http, https, socks5, or socks5h proxy URL",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ProxyPreference;

    #[test]
    fn empty_and_legacy_modes_follow_the_system() {
        for value in ["", "  AUTO ", "Direct", "system"] {
            assert_eq!(
                ProxyPreference::parse(value),
                Some(ProxyPreference::System),
                "{value:?}"
            );
        }
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
