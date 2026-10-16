use super::build_custom_provider;

#[test]
fn custom_provider_builder_rejects_duplicate_ids_and_invalid_urls() {
    let existing = vec!["openai".to_owned()];
    let duplicate = build_custom_provider(
        &existing,
        " openai ",
        "openai-completions",
        "https://api.openai.com/v1",
        "gpt-5",
        "",
        "",
    )
    .expect_err("duplicate provider id must be refused");
    assert!(duplicate.contains("already exists"), "{duplicate}");

    let bad_id = build_custom_provider(
        &existing,
        "Bad ID",
        "openai-completions",
        "https://api.openai.com/v1",
        "gpt-5",
        "",
        "",
    )
    .expect_err("uppercase/space id must be refused");
    assert!(bad_id.contains("invalid provider"), "{bad_id}");

    let bad_url = build_custom_provider(
        &existing,
        "custom-gw",
        "openai-completions",
        "http://api.example.com/v1",
        "gpt-5",
        "",
        "",
    )
    .expect_err("non-https base URL must be refused");
    assert!(bad_url.contains("https://"), "{bad_url}");

    let bad_tokens = build_custom_provider(
        &existing,
        "custom-gw",
        "openai-completions",
        "https://api.example.com/v1",
        "gpt-5",
        "128k",
        "",
    )
    .expect_err("non-numeric token override must be refused");
    assert!(bad_tokens.contains("plain numbers"), "{bad_tokens}");

    let valid = build_custom_provider(
        &existing,
        " custom-gw ",
        "anthropic-messages",
        " https://api.example.com/v1 ",
        " claude-sonnet-4-5 ",
        " 200000 ",
        " 8192 ",
    )
    .expect("valid custom provider");
    assert_eq!(valid.id, "custom-gw");
    assert_eq!(valid.kind, "anthropic-messages");
    assert_eq!(valid.base_url, "https://api.example.com/v1");
    assert_eq!(valid.models, vec!["claude-sonnet-4-5".to_owned()]);
    assert_eq!(valid.context_limit, Some(200_000));
    assert_eq!(valid.max_output, Some(8_192));
}
