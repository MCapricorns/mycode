//! URL and response guards for outbound web requests.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Maximum accepted response body bytes per request.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum one page body may contribute to the aggregate contents payload.
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
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
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

/// Reports whether the address is globally routable public Internet space.
#[must_use]
pub fn is_public_ip(ip: IpAddr) -> bool {
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
    // 0.0.0.0/8, 100.64.0.0/10 (CGNAT), 192.0.0.0/24, 198.18.0.0/15,
    // 240.0.0.0/4 reserved blocks.
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
    // IPv4-mapped and IPv4-compatible addresses fall back to v4 rules.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    // Unique-local fc00::/7, link-local fe80::/10, and the documentation,
    // discard-only, and Teredo prefixes are not public.
    !(segments[0] & 0xfe00 == 0xfc00
        || segments[0] & 0xffc0 == 0xfe80
        || segments[0] & 0xfffe == 0xfdfe
        || segments[0] == 0x2001 && segments[1] == 0x0db8
        || segments[0] == 0x100
        || segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0)
}

/// Strips terminal escape sequences, control characters (keeping newline
/// and tab), and bidi overrides from untrusted remote text. Retrieved page
/// text is data, never terminal input.
pub fn sanitize_remote_text(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        match current {
            '\u{1b}' => match chars.get(index + 1).copied() {
                Some('[') => index = skip_csi(&chars, index + 2),
                Some(']') => index = skip_osc(&chars, index + 2),
                Some('P' | 'X' | '^' | '_') => {
                    index = skip_control_string(&chars, index + 2);
                }
                Some(_) => index += 2,
                None => index += 1,
            },
            '\u{9b}' => index = skip_csi(&chars, index + 1),
            '\u{9d}' => index = skip_osc(&chars, index + 1),
            '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                index = skip_control_string(&chars, index + 1);
            }
            '\n' | '\t' => {
                out.push(current);
                index += 1;
            }
            _ if (current as u32) < 0x20 || matches!(current as u32, 0x7f..=0x9f) => {
                index += 1;
            }
            _ if is_bidi_control(current) => index += 1,
            _ => {
                out.push(current);
                index += 1;
            }
        }
    }
    out
}

fn is_bidi_control(current: char) -> bool {
    matches!(current as u32,
        0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
}

fn skip_csi(chars: &[char], start: usize) -> usize {
    let mut index = start;
    while index < chars.len() {
        let current = chars[index];
        index += 1;
        if ('@'..='~').contains(&current) {
            return index;
        }
    }
    index
}

fn skip_osc(chars: &[char], start: usize) -> usize {
    let mut index = start;
    while index < chars.len() {
        let current = chars[index];
        if current == '\u{7}' || current == '\u{9c}' {
            return index + 1;
        }
        if current == '\u{1b}' && chars.get(index + 1) == Some(&'\\') {
            return index + 2;
        }
        index += 1;
    }
    index
}

fn skip_control_string(chars: &[char], start: usize) -> usize {
    let mut index = start;
    while index < chars.len() {
        let current = chars[index];
        if current == '\u{9c}' {
            return index + 1;
        }
        if current == '\u{1b}' && chars.get(index + 1) == Some(&'\\') {
            return index + 2;
        }
        index += 1;
    }
    index
}
