//! Form parsing and validation shared by every frontend's settings UI.
//!
//! These helpers turn raw text-field contents into config rows — or refuse
//! them — so a desktop form and a TUI form cannot drift into accepting
//! different input.
use std::collections::BTreeMap;

/// The URL the custom proxy mode starts from: Clash's mixed port.
pub const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";

/// Trims an API key candidate for UI button enablement and save dispatch.
///
/// Saving an empty or whitespace-only key field is a no-op so the replace-key
/// flow cannot delete a stored credential.
pub fn normalized_key_input(raw: &str) -> Option<String> {
    let key = mycode_app::normalize_api_key(raw);
    (!key.is_empty() && !key.eq_ignore_ascii_case("bearer")).then_some(key)
}

/// Parses the stdio form's environment line: `KEY=VALUE` pairs separated by
/// commas or whitespace, values optionally quoted. Empty input is no env.
pub fn parse_env_line(line: &str) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for word in mycode_app::split_command_line(&space_separate_env(line)) {
        let Some((key, value)) = word.split_once('=') else {
            return Err(format!(
                "{} '{word}'",
                crate::i18n::t(
                    "Env entries must look like KEY=VALUE:",
                    "环境变量必须写成 KEY=VALUE：",
                )
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(crate::i18n::t(
                "An env entry has an empty variable name.",
                "有一个环境变量名为空。",
            )
            .to_owned());
        }
        env.insert(key.to_owned(), value.to_owned());
    }
    if env.len() > mycode_app::MAX_MCP_ENV_VARS {
        return Err(format!(
            "{} {}",
            crate::i18n::t(
                "Env entries per server, at most:",
                "每个服务器的环境变量上限："
            ),
            mycode_app::MAX_MCP_ENV_VARS
        ));
    }
    Ok(env)
}

/// Maps commas to spaces outside quotes so [`mycode_app::split_command_line`]
/// sees one word per `KEY=VALUE` while a quoted value keeps its commas.
fn space_separate_env(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(open) if ch == open => {
                quote = None;
                out.push(ch);
            }
            // Mirror split_command_line: a backslash escapes `"` and `\`
            // inside double quotes, so `\",` is not a comma outside quotes.
            Some('"') if ch == '\\' && matches!(chars.peek(), Some('"' | '\\')) => {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            Some(_) => out.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                out.push(ch);
            }
            None if ch == ',' => out.push(' '),
            None => out.push(ch),
        }
    }
    out
}

/// Builds the server row the custom MCP form describes, from its raw field
/// values. Pure validation and construction only; the caller decides what
/// to do with the result.
pub fn build_mcp_server(
    id: &str,
    transport: &str,
    endpoint: &str,
    command: &str,
    env_line: &str,
) -> Result<mycode_app::McpServerSettings, String> {
    let server = match transport {
        "http" => {
            if endpoint.is_empty() {
                return Err(
                    crate::i18n::t("http servers need an endpoint", "HTTP 服务器需要端点")
                        .to_owned(),
                );
            }
            mycode_app::McpServerSettings {
                id: id.to_owned(),
                enabled: true,
                transport: transport.to_owned(),
                command: None,
                args: Vec::new(),
                env: Default::default(),
                headers: Default::default(),
                endpoint: Some(endpoint.to_owned()),
                key_header: Some("bearer".to_owned()),
            }
        }
        "stdio" => {
            // The form takes the whole line the server docs publish
            // (`npx -y @scope/server --flag`); split it here so the
            // child gets a program plus argv, not one giant program name.
            let mut words = mycode_app::split_command_line(command).into_iter();
            let Some(program) = words.next().filter(|word| !word.is_empty()) else {
                return Err(
                    crate::i18n::t("stdio servers need a command", "stdio 服务器需要命令")
                        .to_owned(),
                );
            };
            let env = parse_env_line(env_line)?;
            mycode_app::McpServerSettings {
                id: id.to_owned(),
                enabled: true,
                transport: transport.to_owned(),
                command: Some(program),
                args: words.collect(),
                env,
                headers: Default::default(),
                endpoint: None,
                key_header: None,
            }
        }
        _ => {
            return Err(crate::i18n::t(
                "transport must be http or stdio",
                "传输方式必须是 http 或 stdio",
            )
            .to_owned());
        }
    };
    let probe = mycode_app::AppSettings {
        mcp_servers: vec![server.clone()],
        ..mycode_app::AppSettings::default()
    };
    probe
        .validate()
        .map_err(|error| format!("invalid MCP server: {}", error.summary()))?;
    Ok(server)
}

/// Merges picked choices and free text into the answers one ask card sends.
/// One slot per question preserves arbitrary text, including shell pipes,
/// and does not let an answer to one question occupy another's slot.
/// `Err(index)` names the first required question still unanswered.
pub fn collect_ask_answers(
    rows: &[mycode_app::AskPrompt],
    picks: &[String],
    free_text: &[String],
) -> Result<Vec<String>, usize> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let text = free_text
                .get(index)
                .map(|text| text.trim())
                .unwrap_or_default();
            let answer = if text.is_empty() {
                picks.get(index).map(|text| text.trim()).unwrap_or_default()
            } else {
                text
            };
            if answer.is_empty() && !row.optional {
                Err(index)
            } else {
                Ok(answer.to_owned())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{build_mcp_server, collect_ask_answers, normalized_key_input, parse_env_line};

    #[test]
    fn env_line_keeps_commas_inside_quoted_values() {
        let env = parse_env_line(r#"A=1, B="x,y", C='p,q' D=2"#).unwrap();
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("x,y"));
        assert_eq!(env.get("C").map(String::as_str), Some("p,q"));
        assert_eq!(env.get("D").map(String::as_str), Some("2"));
    }

    #[test]
    fn env_line_rejects_a_bare_word() {
        assert!(parse_env_line("A=1 stray").is_err());
        assert!(parse_env_line("=value").is_err());
    }

    #[test]
    fn blank_key_inputs_are_refused_and_bearer_prefixes_are_stripped() {
        assert_eq!(normalized_key_input(""), None);
        assert_eq!(normalized_key_input("   \t\n  "), None);
        assert_eq!(normalized_key_input("Bearer   "), None);
        assert_eq!(
            normalized_key_input("  sk-test-123  "),
            Some("sk-test-123".to_owned())
        );
        assert_eq!(
            normalized_key_input("Bearer sk-test-123"),
            Some("sk-test-123".to_owned())
        );
    }

    #[test]
    fn custom_mcp_builder_validates_id_endpoint_and_stdio_command() {
        let bad_id = build_mcp_server("Bad Id", "http", "https://mcp.example.com/mcp", "", "")
            .expect_err("invalid portable id must be refused");
        assert!(bad_id.contains("invalid MCP server"), "{bad_id}");

        let bad_url = build_mcp_server("my-mcp", "http", "http://mcp.example.com/mcp", "", "")
            .expect_err("non-https endpoint must be refused");
        assert!(bad_url.contains("https://"), "{bad_url}");

        let bad_cmd = build_mcp_server("my-mcp", "stdio", "", "npx|evil", "")
            .expect_err("shell metacharacters in executable must be refused");
        assert!(bad_cmd.contains("invalid MCP server"), "{bad_cmd}");

        let stdio = build_mcp_server(
            "fs-mcp",
            "stdio",
            "http://leftover.invalid",
            "npx -y @modelcontextprotocol/server-filesystem C:\\projects",
            "FOO=bar",
        )
        .expect("valid stdio MCP server");
        assert_eq!(stdio.id, "fs-mcp");
        assert_eq!(stdio.transport, "stdio");
        assert_eq!(stdio.command.as_deref(), Some("npx"));
        assert_eq!(
            stdio.args,
            vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
                "C:\\projects".to_owned(),
            ]
        );
        assert_eq!(stdio.env.get("FOO").map(String::as_str), Some("bar"));
        assert!(stdio.endpoint.is_none());
    }

    #[test]
    fn free_text_preserves_pipes_and_question_identity() {
        let question = |optional| mycode_app::AskPrompt {
            question: "Question?".to_owned(),
            choices: vec![],
            optional,
            multiple: false,
        };
        let questions = [question(false), question(false)];
        assert_eq!(
            collect_ask_answers(
                &questions,
                &["yes".to_owned()],
                &[String::new(), "a | b".to_owned()]
            ),
            Ok(vec!["yes".to_owned(), "a | b".to_owned()])
        );
        assert_eq!(
            collect_ask_answers(&questions[..1], &[], &["a|b|c".to_owned()]),
            Ok(vec!["a|b|c".to_owned()])
        );
        assert_eq!(
            collect_ask_answers(&questions, &[], &["only first".to_owned()]),
            Err(1)
        );
        assert_eq!(
            collect_ask_answers(&[question(true)], &[], &[]),
            Ok(vec![String::new()])
        );
    }
}
