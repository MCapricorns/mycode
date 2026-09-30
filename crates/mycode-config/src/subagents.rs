//! Subagent role catalog: the delegation roles the `task` tool can dispatch.
//!
//! A role is a bounded Markdown file with a small frontmatter block — the
//! same shape the reference pi-subagents extension uses — so a user can add
//! or replace one without an application build. Four roles ship built in;
//! discovery layers the user home and the session workspace over them.
//!
//! This module reads role definitions only. Which roles are enabled, which
//! model each one runs on, and how much thinking it gets are settings, and
//! live in [`crate::SubagentSettings`].

use std::path::{Path, PathBuf};

/// Maximum bytes read per role file.
pub const MAX_ROLE_BYTES: usize = 32 * 1024;
/// Maximum roles in one resolved catalog.
pub const MAX_ROLES: usize = 32;
/// Directory holding role files, under both the home and a workspace's dot dir.
pub const ROLE_DIR_NAME: &str = "agents";

/// Built-in role definitions, embedded so a fresh install has a full team.
const BUILTIN_ROLES: [(&str, &str); 4] = [
    ("scout", include_str!("../agents/scout.md")),
    ("artisan", include_str!("../agents/artisan.md")),
    ("steward", include_str!("../agents/steward.md")),
    ("sentinel", include_str!("../agents/sentinel.md")),
];

/// Where a role's edits land.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoleIsolation {
    /// The session's own checkout. Writers serialize through it.
    #[default]
    Shared,
    /// A disposable detached git worktree, integrated once the run settles.
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

    /// Returns the level spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
            Self::On => "on",
        }
    }

    /// Returns the wire reasoning token, or `None` for the provider default.
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

impl RoleOrigin {
    /// Returns the label the settings page shows.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "built-in",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// Tools a child may keep even when the parent has more.
///
/// Scout is a hard read-only boundary: mutating tools never reach it, even
/// when a project override lists them.
const READ_ONLY_TOOLS: &[&str] = &["read", "grep", "find", "web_search", "fetch_content"];
/// Tools that would let a child re-enter the parent or talk to the user.
const PARENT_ONLY_TOOLS: &[&str] = &["task", "ask_user"];

/// One resolved delegation role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentRole {
    /// Lowercase portable role name; the `agent` argument of the `task` tool.
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
    ///
    /// An omitted allowlist inherits the parent set. Scout is hard
    /// read-only. Parent-only tools (`task`, `ask_user`) never
    /// reach a child, so depth stays at one.
    #[must_use]
    pub fn resolve_tools(&self, parent_tools: &[String]) -> Vec<String> {
        let parent: Vec<String> = parent_tools
            .iter()
            .filter(|name| !PARENT_ONLY_TOOLS.contains(&name.as_str()))
            .cloned()
            .collect();
        let declared = match &self.tools {
            Some(tools) => tools
                .iter()
                .filter(|name| parent.iter().any(|live| live == *name))
                .cloned()
                .collect(),
            None => parent,
        };
        if self.name == "scout" {
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
    /// Scout is never write-capable. An omitted allowlist inherits writers.
    #[must_use]
    pub fn is_write_capable(&self) -> bool {
        if self.name == "scout" {
            return false;
        }
        match &self.tools {
            None => true,
            Some(tools) => tools
                .iter()
                .any(|name| !READ_ONLY_TOOLS.contains(&name.as_str())),
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
    /// Roles in stable name order.
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
///
/// Built-ins are part of the binary. A parse failure is a build defect and
/// is skipped so one bad role cannot take the application down.
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
///
/// Precedence is project over user over built-in on equal `name`, so a
/// project can replace `artisan` without copying the other three. Only
/// `<name>.md` files directly inside a role directory are read; the file stem
/// must match the frontmatter `name`, which keeps the on-disk layout and the
/// routing vocabulary from drifting apart.
#[must_use]
pub fn discover_roles(home: &crate::HomeLayout, workspace_root: Option<&Path>) -> RoleCatalog {
    let mut catalog = builtin_roles();
    let mut layers: Vec<(RoleOrigin, PathBuf)> =
        vec![(RoleOrigin::User, home.root().join(ROLE_DIR_NAME))];
    if let Some(workspace) = workspace_root {
        layers.push((
            RoleOrigin::Project,
            workspace.join(crate::MYCODE_DIR_NAME).join(ROLE_DIR_NAME),
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
        match std::fs::read(&path) {
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

/// Parses one role file: a `---` delimited `key: value` header, then the
/// prompt body.
///
/// The header is deliberately not YAML. It accepts exactly the five keys the
/// runtime honors, so a role file cannot smuggle in structure the dispatcher
/// would ignore.
fn parse_role(text: &str, origin: RoleOrigin) -> Result<SubagentRole, String> {
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
        let value = value.trim();
        match key.trim() {
            "name" => {
                if !crate::home::is_portable_role_name(value) {
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
                        "thinking \"{value}\": must be a models.dev option (default, off, on, minimal, low, medium, high, xhigh, max)"
                    )
                })?;
            }
            "tools" => {
                let list: Vec<String> = value
                    .split(',')
                    .map(str::trim)
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

/// Splits the header from the body at the first line that is exactly `---`.
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
