use super::{PinMode, RedirectStep, decide_redirect, validate_hop};
use std::net::IpAddr;

fn ip(value: [u8; 4]) -> IpAddr {
    IpAddr::from(value)
}

#[test]
fn private_redirect_is_rejected() {
    let error = validate_hop(
        PinMode::CheckRedirect,
        1,
        "https://127.0.0.1/secret",
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
        "https://127.0.0.1:11434/v1/chat/completions",
        &[ip([127, 0, 0, 1])],
    )
    .unwrap();
}

#[test]
fn only_a_loopback_first_hop_may_use_plain_http() {
    // The magpie gateway answers on loopback without TLS.
    validate_hop(
        PinMode::CheckRedirect,
        0,
        "http://127.0.0.1:3425/v1/chat/completions",
        &[ip([127, 0, 0, 1])],
    )
    .unwrap();
    validate_hop(
        PinMode::CheckRedirect,
        0,
        "http://localhost:3425/v1/chat/completions",
        &[ip([127, 0, 0, 1]), v6("::1")],
    )
    .unwrap();
    validate_hop(
        PinMode::CheckRedirect,
        0,
        "http://[::1]:3425/v1/chat/completions",
        &[v6("::1")],
    )
    .unwrap();
    // A loopback name answered by a public address is not this machine, and
    // one answered by loopback plus anything else is not either.
    assert!(
        validate_hop(
            PinMode::CheckRedirect,
            0,
            "http://localhost:3425/v1/models",
            &[ip([1, 1, 1, 1])],
        )
        .is_err()
    );
    assert!(
        validate_hop(
            PinMode::CheckRedirect,
            0,
            "http://localhost:3425/v1/models",
            &[ip([127, 0, 0, 1]), ip([1, 1, 1, 1])],
        )
        .is_err()
    );
    assert!(
        validate_hop(
            PinMode::CheckRedirect,
            0,
            "http://localhost:3425/v1/models",
            &[ip([127, 0, 0, 1]), v6("fd00::1")],
        )
        .is_err()
    );
    // A non-loopback http endpoint still fails.
    assert!(
        validate_hop(
            PinMode::CheckRedirect,
            0,
            "http://192.168.1.20:8080/v1/models",
            &[ip([192, 168, 1, 20])],
        )
        .is_err()
    );
    // A redirect hop stays https even toward loopback.
    assert!(
        validate_hop(
            PinMode::CheckRedirect,
            1,
            "http://127.0.0.1:3425/v1/models",
            &[ip([127, 0, 0, 1])],
        )
        .is_err()
    );
    // The proxy-resolved URL check has no loopback exception: that path never
    // sees a loopback host.
    assert!(super::check_url(PinMode::CheckRedirect, "http://127.0.0.1:3425/v1/models").is_err());
}

fn v6(value: &str) -> IpAddr {
    value.parse().unwrap()
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

#[test]
fn a_redirect_to_another_origin_drops_credentials() {
    let headers = vec![
        ("Authorization".to_owned(), "Bearer sk-secret".to_owned()),
        ("x-api-key".to_owned(), "sk-secret".to_owned()),
        ("X-Context7-Key".to_owned(), "secret".to_owned()),
        ("Accept".to_owned(), "text/event-stream".to_owned()),
        ("content-type".to_owned(), "application/json".to_owned()),
    ];
    let kept = super::cross_origin_headers(&headers);
    assert_eq!(
        kept,
        vec![
            ("Accept".to_owned(), "text/event-stream".to_owned()),
            ("content-type".to_owned(), "application/json".to_owned()),
        ]
    );
    assert_eq!(
        super::origin_of("https://API.example.com/v1"),
        super::origin_of("https://api.example.com:443/other")
    );
    assert_ne!(
        super::origin_of("https://api.example.com/v1"),
        super::origin_of("https://cdn.example.com/v1")
    );
    assert_ne!(
        super::origin_of("https://api.example.com/v1"),
        super::origin_of("https://api.example.com:8443/v1")
    );
}
