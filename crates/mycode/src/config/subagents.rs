//! Subagent role catalog: the delegation roles the `agent` tool can dispatch.
//!
//! A role is a bounded Markdown file with a small frontmatter block. Three
//! roles ship built in; discovery layers the user home and the session
//! workspace over them. Enabled state, models, and thinking are settings
//! ([`crate::config::SubagentSettings`]).

use std::path::{Path, PathBuf};

/// Maximum bytes read per role file.
pub(crate) const MAX_ROLE_BYTES: usize = 32 * 1024;
/// Maximum roles in one resolved catalog.
pub(crate) const MAX_ROLES: usize = 32;
/// Directory holding role files, under both the home and a workspace's dot dir.
pub(crate) const ROLE_DIR_NAME: &str = "agents";

/// Built-in role definitions, embedded so a fresh install has a full team.
const BUILTIN_ROLES: [(&str, &str); 3] = [
    ("scout", include_str!("../../agents/scout.md")),
    ("artisan", include_str!("../../agents/artisan.md")),
    ("sentinel", include_str!("../../agents/sentinel.md")),
];

/// Where a role's edits land.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoleIsolation {
    /// The session's own checkout. Writers serialize through it.
    #[default]
    Shared,
    /// A detached git worktree whose changes are handed back for integration.
    Worktree,
}

impl RoleIsolation {
    /// Parses the frontmatter or tool-argument spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "shared" => Some(Self::Shared),
            "worktree" => Some(Self::Worktree),
            _ => None,
        }
    }

    /// Returns the frontmatter spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Worktree => "worktree",
        }
    }
}

/// Reasoning effort a role asks for by default.
///
/// Spellings match models.dev `reasoning_options` plus `default` (inherit).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoleThinking {
    /// Leave the provider default alone.
    #[default]
    Default,
    /// Disable reasoning when the vendor accepts an off switch.
    Off,
    /// Shortest advertised effort.
    Minimal,
    /// Brief reasoning.
    Low,
    /// Balanced reasoning.
    Medium,
    /// Deep reasoning.
    High,
    /// Above high, when the catalog advertises `xhigh`.
    Xhigh,
    /// Vendor maximum effort.
    Max,
    /// Enable reasoning on toggle-only models.
    On,
}

impl RoleThinking {
    /// Parses a catalog or settings spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "off" | "none" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            "on" => Some(Self::On),
            _ => None,
        }
    }

    /// Returns the models.dev spelling for this level, or `None` for the
    /// provider default. Callers pass the token through a reasoning-level
    /// parse before it reaches a request.
    #[must_use]
    pub fn effort(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Off => Some("off"),
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh => Some("xhigh"),
            Self::Max => Some("max"),
            Self::On => Some("on"),
        }
    }
}

/// Where a role definition came from. Later sources win on equal names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RoleOrigin {
    /// Embedded in the application binary.
    Builtin,
    /// `<home>/agents/<name>.md`.
    User,
    /// `<workspace>/.mycode/agents/<name>.md`.
    Project,
}

/// Tools a child may keep even when the parent has more.
///
/// Read-only roles keep only these native tools. MCP capabilities are
/// filtered separately by the host using their read-only annotations.
const READ_ONLY_TOOLS: &[&str] = &["read", "grep", "find"];
/// Tools that would let a child delegate, control other runs, or talk to the user.
const PARENT_ONLY_TOOLS: &[&str] = &[
    "agent",
    "agent_status",
    "agent_stop",
    "agent_message",
    "ask_user",
];

/// `bash`, `zsh`, `sh`, `powershell`, and `cmd` are the model-facing names
/// of the one process tool. Role files and the parent allowlist may say any
/// of them, or the stable name `shell`.
fn canonical_tool_name(name: &str) -> &str {
    match name {
        "bash" | "zsh" | "sh" | "powershell" | "cmd" => "shell",
        other => other,
    }
}

/// One resolved delegation role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentRole {
    /// Lowercase portable role name; the `agent` argument of the `agent` tool.
    pub name: String,
    /// One-line routing description the parent model reads.
    pub description: String,
    /// Where this role's edits land by default.
    pub isolation: RoleIsolation,
    /// Reasoning effort this role asks for by default.
    pub thinking: RoleThinking,
    /// Tool names this role may use. `None` inherits the parent's set.
    pub tools: Option<Vec<String>>,
    /// The role's system prompt: everything after the frontmatter.
    pub prompt: String,
    /// Which layer supplied this definition.
    pub origin: RoleOrigin,
}

impl SubagentRole {
    /// Tools this role may use, intersected with the parent's live set.
    #[must_use]
    pub fn resolve_tools<S: AsRef<str>>(&self, parent_tools: &[S]) -> Vec<String> {
        let live = |name: &str| {
            let name = canonical_tool_name(name);
            !PARENT_ONLY_TOOLS.contains(&name)
                && parent_tools
                    .iter()
                    .any(|parent| canonical_tool_name(parent.as_ref()) == name)
        };
        let declared: Vec<String> = match &self.tools {
            Some(tools) => tools.iter().filter(|name| live(name)).cloned().collect(),
            None => parent_tools
                .iter()
                .map(AsRef::as_ref)
                .filter(|name| !PARENT_ONLY_TOOLS.contains(name))
                .map(str::to_owned)
                .collect(),
        };
        if !self.is_write_capable() {
            declared
                .into_iter()
                .filter(|name| READ_ONLY_TOOLS.contains(&name.as_str()))
                .collect()
        } else {
            declared
        }
    }

    /// Whether this role may write the tree (and therefore take a worktree).
    ///
    /// Scout and sentinel stay read-only across user/project overrides.
    #[must_use]
    pub fn is_write_capable(&self) -> bool {
        if matches!(self.name.as_str(), "scout" | "sentinel") {
            return false;
        }
        match &self.tools {
            None => true,
            Some(tools) => tools
                .iter()
                .any(|name| matches!(canonical_tool_name(name), "write" | "edit" | "shell")),
        }
    }

    /// One-line catalog entry for the parent prompt.
    #[must_use]
    pub fn catalog_line(&self) -> String {
        format!("- {}: {}", self.name, self.description)
    }
}

/// A resolved catalog of delegation roles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleCatalog {
    /// Roles in a stable order.
    pub roles: Vec<SubagentRole>,
}

impl RoleCatalog {
    /// Looks one role up by name.
    #[must_use]
    pub fn role(&self, name: &str) -> Option<&SubagentRole> {
        self.roles.iter().find(|role| role.name == name)
    }

    /// Lists the role names in catalog order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.roles.iter().map(|role| role.name.clone()).collect()
    }
}

/// Returns the embedded role definitions.
#[must_use]
pub fn builtin_roles() -> RoleCatalog {
    let mut catalog = RoleCatalog::default();
    for (name, text) in BUILTIN_ROLES {
        match parse_role(text, RoleOrigin::Builtin) {
            Ok(role) => catalog.roles.push(role),
            Err(detail) => {
                eprintln!("mycode-config: built-in role {name} skipped: {detail}");
            }
        }
    }
    catalog
}

/// Resolves the role catalog for one session workspace.
#[must_use]
pub fn discover_roles(
    home: &crate::config::HomeLayout,
    workspace_root: Option<&Path>,
) -> RoleCatalog {
    let mut catalog = builtin_roles();
    let mut layers: Vec<(RoleOrigin, PathBuf)> =
        vec![(RoleOrigin::User, home.root().join(ROLE_DIR_NAME))];
    if let Some(workspace) = workspace_root {
        layers.push((
            RoleOrigin::Project,
            workspace
                .join(crate::config::home::MYCODE_DIR_NAME)
                .join(ROLE_DIR_NAME),
        ));
    }
    for (origin, dir) in layers {
        for (path, text) in read_role_dir(&dir) {
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let source = path.to_string_lossy();
            match parse_role(&text, origin) {
                Ok(role) if role.name != stem => {
                    eprintln!(
                        "mycode-config: role {source} skipped: name \"{}\" does not match the file name",
                        role.name
                    );
                }
                Ok(role) => match catalog.roles.iter().position(|held| held.name == role.name) {
                    Some(index) => catalog.roles[index] = role,
                    None if catalog.roles.len() < MAX_ROLES => catalog.roles.push(role),
                    None => {
                        eprintln!(
                            "mycode-config: role {source} skipped: catalog is full at {MAX_ROLES} roles"
                        );
                    }
                },
                Err(detail) => {
                    eprintln!("mycode-config: role {source} skipped: {detail}");
                }
            }
        }
    }
    catalog.roles.sort_by(|a, b| a.name.cmp(&b.name));
    catalog
}

/// Reads the `<name>.md` files of one role directory in stable name order.
fn read_role_dir(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "md")
        })
        .collect();
    paths.sort();
    paths.truncate(MAX_ROLES);
    let mut files = Vec::new();
    for path in paths {
        let source = path.to_string_lossy();
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() as usize > MAX_ROLE_BYTES => {
                eprintln!(
                    "mycode-config: role {source} skipped: larger than {MAX_ROLE_BYTES} bytes"
                );
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("mycode-config: role {source} skipped: unreadable: {error}");
                continue;
            }
        }
        match std::fs::File::open(&path).and_then(read_role_bytes) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => files.push((path, text)),
                Err(_) => {
                    eprintln!("mycode-config: role {source} skipped: not valid UTF-8");
                }
            },
            Err(error) => {
                eprintln!("mycode-config: role {source} skipped: unreadable: {error}");
            }
        }
    }
    files
}

fn read_role_bytes(reader: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    reader
        .take(MAX_ROLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ROLE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "role exceeds the byte limit",
        ));
    }
    Ok(bytes)
}

fn parse_role(text: &str, origin: RoleOrigin) -> Result<SubagentRole, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let body = text
        .strip_prefix("---")
        .and_then(|rest| {
            rest.strip_prefix('\n')
                .or_else(|| rest.strip_prefix("\r\n"))
        })
        .ok_or_else(|| "missing the opening \"---\" frontmatter line".to_owned())?;
    let (header, prompt) = split_frontmatter(body)
        .ok_or_else(|| "missing the closing \"---\" frontmatter line".to_owned())?;

    let mut name = None;
    let mut description = None;
    let mut isolation = RoleIsolation::default();
    let mut thinking = RoleThinking::default();
    let mut tools = None;
    for line in header.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("frontmatter line is not \"key: value\": {line}"))?;
        let value = unquote(value.trim());
        match key.trim() {
            "name" => {
                if !crate::config::home::is_portable_role_name(value) {
                    return Err(format!(
                        "name \"{value}\": must be 1-64 lowercase letters, digits, dash, dot, or underscore"
                    ));
                }
                name = Some(value.to_owned());
            }
            "description" => {
                if value.is_empty() || value.len() > 512 {
                    return Err("description: must be 1-512 characters".to_owned());
                }
                description = Some(value.to_owned());
            }
            "isolation" => {
                isolation = RoleIsolation::parse(value)
                    .ok_or_else(|| format!("isolation \"{value}\": must be shared or worktree"))?;
            }
            "thinking" => {
                thinking = RoleThinking::parse(value).ok_or_else(|| {
                    format!(
                        "thinking \"{value}\": must be a models.dev option (default, off, none, on, minimal, low, medium, high, xhigh, max)"
                    )
                })?;
            }
            "tools" => {
                let value = value
                    .strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
                    .unwrap_or(value);
                let list: Vec<String> = value
                    .split(',')
                    .map(|entry| unquote(entry.trim()))
                    .filter(|entry| !entry.is_empty())
                    .map(str::to_owned)
                    .collect();
                tools = Some(list);
            }
            other => return Err(format!("unknown frontmatter key \"{other}\"")),
        }
    }

    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("the prompt body after the frontmatter is empty".to_owned());
    }
    Ok(SubagentRole {
        name: name.ok_or_else(|| "frontmatter is missing \"name\"".to_owned())?,
        description: description
            .ok_or_else(|| "frontmatter is missing \"description\"".to_owned())?,
        isolation,
        thinking,
        tools,
        prompt: prompt.to_owned(),
        origin,
    })
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

fn split_frontmatter(body: &str) -> Option<(&str, &str)> {
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some((&body[..offset], &body[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{RoleIsolation, RoleOrigin, RoleThinking, SubagentRole};

    fn role(name: &str, tools: Option<Vec<String>>) -> SubagentRole {
        SubagentRole {
            name: name.to_owned(),
            description: "role".into(),
            isolation: RoleIsolation::Shared,
            thinking: RoleThinking::Default,
            tools,
            prompt: "work".into(),
            origin: RoleOrigin::Builtin,
        }
    }

    #[test]
    fn discover_roles_layers_project_over_user_over_builtin() {
        let root = std::env::temp_dir().join(format!(
            "mycode-role-discover-{}-{}",
            std::process::id(),
            crate::core::unix_now().as_nanos()
        ));
        let _cleanup = RemoveDir(&root);
        let agents = root.join("agents");
        std::fs::create_dir_all(&agents).expect("agents dir");
        std::fs::write(
            agents.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews a diff.\nisolation: shared\nthinking: low\n---\n\nLook, then stop.\n",
        )
        .expect("user role");
        std::fs::write(
            agents.join("mismatch.md"),
            "---\nname: other\ndescription: Name does not match the file.\n---\n\nSkip me.\n",
        )
        .expect("mismatched role");
        let project = root.join("project");
        let project_agents = project
            .join(crate::config::home::MYCODE_DIR_NAME)
            .join("agents");
        std::fs::create_dir_all(&project_agents).expect("project agents");
        std::fs::write(
            project_agents.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Project review.\n---\n\nProject body.\n",
        )
        .expect("project role");
        let home = crate::config::HomeLayout::from_root(root.clone()).expect("home");
        let catalog = super::discover_roles(&home, Some(project.as_path()));
        let names = catalog.names();
        assert!(names.iter().any(|name| name == "scout"));
        assert!(names.iter().any(|name| name == "artisan"));
        assert!(names.iter().any(|name| name == "sentinel"));
        assert!(names.iter().any(|name| name == "reviewer"));
        assert!(!names.iter().any(|name| name == "other"));
        let reviewer = catalog.role("reviewer").expect("reviewer");
        assert_eq!(reviewer.origin, RoleOrigin::Project);
        assert_eq!(reviewer.description, "Project review.");
    }

    struct RemoveDir<'a>(&'a std::path::Path);
    impl Drop for RemoveDir<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }

    #[test]
    fn builtins_have_distinct_roles_and_only_artisan_can_write() {
        let catalog = super::builtin_roles();
        assert_eq!(catalog.names(), ["scout", "artisan", "sentinel"]);
        for role in &catalog.roles {
            assert_eq!(role.origin, RoleOrigin::Builtin);
            let writer = role.name == "artisan";
            assert_eq!(role.is_write_capable(), writer, "{}", role.name);
            assert_eq!(
                role.isolation,
                if writer {
                    RoleIsolation::Worktree
                } else {
                    RoleIsolation::Shared
                }
            );
            if !writer {
                assert_eq!(
                    role.resolve_tools(&["read", "grep", "find", "shell", "edit"]),
                    ["read", "grep", "find"]
                );
            }
        }
    }

    #[test]
    fn read_only_roles_and_children_filter_tools_accurately() {
        let parent = [
            "read",
            "grep",
            "find",
            "write",
            "edit",
            "shell",
            "powershell",
            "run_code",
            "wait_code",
            "agent",
            "agent_status",
            "agent_stop",
            "agent_message",
            "ask_user",
        ];
        for name in ["scout", "sentinel"] {
            let mut r = role(name, Some(parent.iter().map(|s| (*s).to_owned()).collect()));
            r.origin = RoleOrigin::Project;
            assert!(!r.is_write_capable());
            assert_eq!(r.resolve_tools(&parent), ["read", "grep", "find"]);
        }
        let artisan = role("artisan", None);
        assert!(artisan.is_write_capable());
        assert!(
            artisan
                .resolve_tools(&parent)
                .iter()
                .all(|t| !super::PARENT_ONLY_TOOLS.contains(&t.as_str()))
        );
        let ps = role("artisan", Some(vec!["read".into(), "powershell".into()]));
        assert_eq!(ps.resolve_tools(&["read", "shell"]), ["read", "powershell"]);
    }

    #[test]
    fn yaml_style_role_files_parse() {
        let text = "\u{feff}---\r\nname: \"reviewer\"\r\ndescription: 'Reviews a diff.'\r\ntools: [read, \"grep\"]\r\n---\r\nLook, then stop.\r\n";
        let parsed = super::parse_role(text, RoleOrigin::User).expect("role");
        assert_eq!(parsed.name, "reviewer");
        assert_eq!(parsed.description, "Reviews a diff.");
        assert_eq!(
            parsed.tools,
            Some(vec!["read".to_owned(), "grep".to_owned()])
        );
    }
}
