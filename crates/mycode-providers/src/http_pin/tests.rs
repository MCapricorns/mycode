use super::{
    AddressClass, PinMode, RedirectStep, classify_addresses, connection_addresses, decide_redirect,
    is_public_ip, validate_hop,
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
        "https://127.0.0.1:11434/v1/chat/completions",
        &[ip([127, 0, 0, 1])],
    )
    .unwrap();
}

#[test]
fn checked_requests_never_use_plain_http() {
    let first = validate_hop(
        PinMode::CheckRedirect,
        0,
        "http://api.example.com/v1/chat/completions",
        &[ip([1, 1, 1, 1])],
    )
    .unwrap_err();
    assert!(first.contains("https"), "{first}");
    // A redirect cannot downgrade an https request.
    let downgrade = validate_hop(
        PinMode::CheckRedirect,
        1,
        "http://cdn.example/asset",
        &[ip([8, 8, 8, 8])],
    )
    .unwrap_err();
    assert!(downgrade.contains("https"), "{downgrade}");
    assert!(super::check_url(PinMode::CheckRedirect, "http://cdn.example/asset").is_err());
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
fn public_ip_rejects_reserved_ipv6_ranges() {
    for addr in [
        "ff02::1",
        "fec0::1",
        "fdfe::1",
        "2002:0a00:0001::1",
        "2001::1",
        "2001:5::1",
        "2001:db8::1",
        "100::1",
    ] {
        assert!(!is_public_ip(v6(addr)), "{addr}");
    }
    for addr in ["2001:10::1", "2002:0101:0101::1", "2606:4700::1111"] {
        assert!(is_public_ip(v6(addr)), "{addr}");
    }
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

#[test]
fn bracketed_and_dotted_local_hosts_are_not_public() {
    for host in [
        "[::1]",
        "[fd00::1]",
        "localhost.",
        "printer.local.",
        "127.0.0.1.",
    ] {
        assert!(!super::public_host(host), "{host}");
    }
    assert!(super::public_host("[2606:4700::1111]"));
    assert!(super::public_host("example.com."));
}

#[tokio::test]
async fn an_ipv6_literal_resolves_without_dns() {
    let addrs = super::lookup_addresses("https://[::1]:8443/v1")
        .await
        .unwrap();
    assert_eq!(addrs, [std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)]);
}
