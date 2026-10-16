//! URL and response guards for outbound web requests.

use std::net::IpAddr;

use mycode_providers::is_public_ip;

/// Maximum accepted response body bytes per request.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Per-page character cap applied before the aggregate byte budget.
/// Callers compare this to `chars().count()`, not to UTF-8 byte length.
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;
/// Maximum results accepted from one search response.
pub const MAX_SEARCH_RESULTS: usize = 32;
/// Maximum URLs accepted in one contents request.
pub const MAX_CONTENTS_URLS: usize = 8;
/// Default outbound timeout applied by the caller.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Querit `crawlTimeout` is seconds in `1..=60`, not milliseconds.
pub const CRAWL_TIMEOUT_SECS: u64 = 20;
/// HTTP deadline for a contents call. Stays above [`CRAWL_TIMEOUT_SECS`].
pub const CONTENTS_TIMEOUT_SECS: u64 = 45;

/// Rejects URLs the web client must not fetch.
///
/// Only `https://` URLs with a host that is neither an IP literal in a
/// private, loopback, link-local, or otherwise non-public range nor a
/// trivially local name are accepted. Port numbers must be empty or the
/// default 443 so guards cannot be routed around through odd ports.
#[must_use]
pub fn is_fetchable_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    if value.len() > 2048 || rest.is_empty() {
        return false;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // Split host and port; bracketed IPv6 literals may carry "]":port".
    let (host, port) = if host_port.ends_with(']') {
        (host_port, None)
    } else if let Some((prefix, port)) = host_port.rsplit_once("]:") {
        (&host_port[..prefix.len() + 1], Some(port))
    } else {
        match host_port.rsplit_once(':') {
            Some((prefix, port)) if !prefix.is_empty() => (prefix, Some(port)),
            _ => (host_port, None),
        }
    };
    if let Some(port) = port
        && !port.is_empty()
        && port != "443"
    {
        return false;
    }
    // The bracket trim above removes every leading `[` and trailing `]`,
    // so no further strip can change the value.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    if matches!(lower.as_str(), "localhost" | "localhost.localdomain") || lower.ends_with(".local")
    {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_public_ip(ip);
    }
    // Plain hostnames: require a plausible DNS name without whitespace or
    // scheme smuggling.
    host.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
        && !host.starts_with('-')
        && !host.contains("..")
}

/// Strips terminal escape sequences, control characters (keeping newline
/// and tab), and bidi overrides from untrusted remote text. Retrieved page
/// text is data, never terminal input.
pub fn sanitize_remote_text(value: &str) -> String {
    if value.chars().all(is_clean_remote_char) {
        return value.to_owned();
    }
    let mut chars = value.chars().peekable();
    let mut out = String::with_capacity(value.len());
    while let Some(current) = chars.next() {
        match current {
            '\u{1b}' => match chars.next() {
                Some('[') => skip_csi(&mut chars),
                Some(']') => skip_osc(&mut chars),
                Some('P' | 'X' | '^' | '_') => skip_control_string(&mut chars),
                Some(_) | None => {}
            },
            '\u{9b}' => skip_csi(&mut chars),
            '\u{9d}' => skip_osc(&mut chars),
            '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => skip_control_string(&mut chars),
            '\n' | '\t' => out.push(current),
            _ if !is_clean_remote_char(current) => {}
            _ => out.push(current),
        }
    }
    out
}

fn is_clean_remote_char(current: char) -> bool {
    current == '\n'
        || current == '\t'
        || ((current as u32) >= 0x20
            && !matches!(current as u32, 0x7f..=0x9f)
            && !is_bidi_control(current))
}

fn is_bidi_control(current: char) -> bool {
    matches!(current as u32,
        0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
}

fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for current in chars.by_ref() {
        if ('@'..='~').contains(&current) {
            break;
        }
    }
}

fn skip_osc(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(current) = chars.next() {
        if current == '\u{7}' || current == '\u{9c}' {
            break;
        }
        if current == '\u{1b}' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

fn skip_control_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(current) = chars.next() {
        if current == '\u{9c}' {
            break;
        }
        if current == '\u{1b}' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}
