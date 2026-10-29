//! Subagent role catalog: the delegation roles the `agent` tool can dispatch.
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
pub(crate) const MAX_ROLE_BYTES: usize = 32 * 1024;
/// Maximum roles in one resolved catalog.
pub(crate) const MAX_ROLES: usize = 32;
/// Directory holding role files, under both the home and a workspace's dot dir.
pub(crate) const ROLE_DIR_NAME: &str = "agents";

/// Built-in role definitions, embedded so a fresh install has a full team.
const BUILTIN_ROLES: [(&str, &str); 4] = [
    ("scout", include_str!("../agents/scout.md")),
    ("artisan", include_str!("../agents/artisan.md")),
    ("sentinel", include_str!("../agents/sentinel.md")),
    ("skeptic", include_str!("../agents/skeptic.md")),
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
    "agent_workflow",
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
    ///
    /// An omitted allowlist inherits the parent set for writers. Read-only
    /// roles keep only read/grep/find, even when an override lists shell or
    /// code-mode tools. The host may wrap this filtered set in code mode;
    /// scripts must dispatch through that same restricted registry.
    /// Parent-only tools never reach a child, so depth stays at one.
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
    /// Scout, sentinel, and skeptic stay read-only across user/project
    /// overrides. Other roles inherit writers when their allowlist is omitted;
    /// explicit lists need a known mutating tool to gain writer isolation.
    #[must_use]
    pub fn is_write_capable(&self) -> bool {
        if matches!(self.name.as_str(), "scout" | "sentinel" | "skeptic") {
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
/// project can replace `artisan` without copying `scout`. Only
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
            workspace
                .join(crate::home::MYCODE_DIR_NAME)
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
    // Windows editors often save with a byte-order mark.
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
        // Role files are often written as YAML, which quotes freely.
        let value = unquote(value.trim());
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

/// Drops one pair of matching `"` or `'` quotes around a value.
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
    fn discover_roles_adds_user_and_project_files_without_inventing_retired_ones() {
        let root = std::env::temp_dir().join(format!(
            "mycode-role-discover-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
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
        let project_agents = project.join(crate::home::MYCODE_DIR_NAME).join("agents");
        std::fs::create_dir_all(&project_agents).expect("project agents");
        std::fs::write(
            project_agents.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Project review.\n---\n\nProject body.\n",
        )
        .expect("project role");
        let home = crate::HomeLayout::from_root(root.clone()).expect("home");
        let catalog = super::discover_roles(&home, Some(project.as_path()));
        let names = catalog.names();
        assert!(names.iter().any(|name| name == "scout"));
        assert!(names.iter().any(|name| name == "artisan"));
        assert!(names.iter().any(|name| name == "sentinel"));
        assert!(names.iter().any(|name| name == "skeptic"));
        assert!(names.iter().any(|name| name == "reviewer"));
        assert!(!names.iter().any(|name| name == "other"));
        assert!(!names.iter().any(|name| name == "steward"));
        let reviewer = catalog.role("reviewer").expect("reviewer");
        assert_eq!(reviewer.origin, RoleOrigin::Project);
        assert_eq!(reviewer.description, "Project review.");
        assert_eq!(
            catalog.role("scout").expect("scout").origin,
            RoleOrigin::Builtin
        );
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
        assert_eq!(catalog.names(), ["scout", "artisan", "sentinel", "skeptic"]);
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
        let blob = catalog
            .roles
            .into_iter()
            .map(|role| format!("{}\n{}", role.description, role.prompt))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(blob.contains("then stop"));
        assert!(blob.contains("Do not stop at a first draft"));
        // The environment block names the one interpreter; the role does not
        // hard-code a shell.
        assert!(blob.contains("the shell tool the environment block names"));
        assert!(!blob.contains("`bash`"));
        assert!(!blob.contains("steward"));
        assert!(blob.contains("No findings alone is not PASS"));
        assert!(blob.contains("If no specific claim is supplied"));
        assert!(!blob.contains("`exec`"));
        assert!(!blob.contains("`task`"));
    }

    #[test]
    fn children_never_inherit_parent_control_tools() {
        let mut parent = vec!["read", "write", "shell"];
        parent.extend_from_slice(super::PARENT_ONLY_TOOLS);
        for allowlist in [
            None,
            Some(parent.iter().map(|name| (*name).to_owned()).collect()),
        ] {
            let artisan = role("artisan", allowlist);
            assert_eq!(artisan.resolve_tools(&parent), ["read", "write", "shell"]);
        }
        for name in [
            "agent",
            "agent_status",
            "agent_stop",
            "agent_message",
            "agent_workflow",
            "ask_user",
        ] {
            assert!(super::PARENT_ONLY_TOOLS.contains(&name));
        }
    }

    #[test]
    fn read_only_roles_reject_shell_aliases_and_code_mode_escape_tools() {
        let parent = [
            "read",
            "grep",
            "find",
            "write",
            "edit",
            "shell",
            "bash",
            "zsh",
            "sh",
            "powershell",
            "cmd",
            "run_code",
            "wait_code",
            "unknown",
            "remote_write",
            "agent",
            "agent_status",
            "agent_stop",
            "agent_message",
            "agent_workflow",
            "ask_user",
        ];
        for name in ["scout", "sentinel", "skeptic"] {
            for origin in [RoleOrigin::Builtin, RoleOrigin::User, RoleOrigin::Project] {
                for allowlist in [
                    None,
                    Some(parent.iter().map(|name| (*name).to_owned()).collect()),
                ] {
                    let mut role = role(name, allowlist);
                    role.origin = origin;
                    role.isolation = RoleIsolation::Worktree;
                    assert!(!role.is_write_capable(), "{name} {origin:?}");
                    assert_eq!(
                        role.resolve_tools(&parent),
                        ["read", "grep", "find"],
                        "{name} {origin:?}"
                    );
                    assert_eq!(
                        role.resolve_tools(&["shell", "run_code"]),
                        Vec::<String>::new()
                    );
                }
            }
        }
        let custom = role(
            "researcher",
            Some(vec![
                "read".into(),
                "run_code".into(),
                "remote_write".into(),
            ]),
        );
        assert!(!custom.is_write_capable());
        assert_eq!(custom.resolve_tools(&parent), ["read"]);
    }

    #[test]
    fn read_only_allowlists_still_intersect_the_live_parent_tools() {
        let catalog = super::builtin_roles();
        for name in ["scout", "sentinel", "skeptic"] {
            let role = catalog.role(name).expect("builtin");
            assert_eq!(role.resolve_tools(&["read", "shell"]), ["read"]);
            assert!(role.resolve_tools::<&str>(&[]).is_empty());
        }
    }

    #[test]
    fn builtin_roles_are_opt_out_and_disabling_does_not_change_the_catalog() {
        let catalog = super::builtin_roles();
        let mut settings = crate::SubagentSettings::default();
        for name in catalog.names() {
            assert!(settings.is_enabled(&name));
        }
        for name in ["sentinel", "skeptic"] {
            settings.role_mut(name).enabled = false;
        }
        let enabled: Vec<_> = catalog
            .roles
            .iter()
            .filter(|role| settings.is_enabled(&role.name))
            .map(|role| role.name.as_str())
            .collect();
        assert_eq!(enabled, ["scout", "artisan"]);
        for name in catalog.names() {
            settings.role_mut(&name).enabled = false;
            assert!(catalog.role(&name).is_some());
        }
        assert!(
            catalog
                .roles
                .iter()
                .all(|role| !settings.is_enabled(&role.name))
        );
        settings.role_mut("sentinel").enabled = true;
        assert!(settings.is_enabled("sentinel"));
        assert!(!settings.is_enabled("skeptic"));
    }

    #[test]
    fn user_and_project_overrides_replace_builtins_without_losing_read_only_boundaries() {
        let root = std::env::temp_dir().join(format!(
            "mycode-role-overrides-{}-{}",
            std::process::id(),
            crate::unix_now().as_nanos()
        ));
        let _cleanup = RemoveDir(&root);
        let project = root.join("project");
        let user_agents = root.join("agents");
        let project_agents = project.join(crate::home::MYCODE_DIR_NAME).join("agents");
        std::fs::create_dir_all(&user_agents).unwrap();
        std::fs::create_dir_all(&project_agents).unwrap();
        let home = crate::HomeLayout::from_root(&root).unwrap();
        let names = super::builtin_roles().names();
        for (origin, directory) in [
            (RoleOrigin::User, user_agents),
            (RoleOrigin::Project, project_agents),
        ] {
            for name in &names {
                std::fs::write(directory.join(format!("{name}.md")), format!(
                    "---\nname: {name}\ndescription: Override {origin:?}.\nisolation: worktree\ntools: read, shell, write, run_code, agent_workflow\n---\nCustom {origin:?} instructions.\n"
                )).unwrap();
            }
            let catalog = super::discover_roles(&home, Some(&project));
            assert_eq!(catalog.roles.len(), names.len());
            for name in &names {
                let role = catalog.role(name).expect("overridden builtin");
                assert_eq!(role.origin, origin);
                assert_eq!(role.prompt, format!("Custom {origin:?} instructions."));
                assert_eq!(role.isolation, RoleIsolation::Worktree);
                assert_eq!(role.is_write_capable(), name == "artisan");
                if name != "artisan" {
                    assert_eq!(
                        role.resolve_tools(&[
                            "read",
                            "shell",
                            "write",
                            "run_code",
                            "agent_workflow"
                        ]),
                        ["read"]
                    );
                }
            }
        }
    }

    #[test]
    fn declared_exec_is_dropped_when_the_parent_only_has_shell() {
        let narrow = role(
            "narrow",
            Some(vec![
                "read".into(),
                "grep".into(),
                "find".into(),
                "exec".into(),
                "shell".into(),
            ]),
        );
        let tools = narrow.resolve_tools(&["read", "grep", "find", "shell", "agent"]);
        assert_eq!(
            tools,
            vec![
                "read".to_owned(),
                "grep".to_owned(),
                "find".to_owned(),
                "shell".to_owned()
            ]
        );
    }

    #[test]
    fn powershell_in_a_role_matches_the_parent_shell_tool() {
        let role = role("artisan", Some(vec!["read".into(), "powershell".into()]));
        let tools = role.resolve_tools(&["read", "shell"]);
        assert_eq!(tools, vec!["read".to_owned(), "powershell".to_owned()]);
    }

    #[test]
    fn write_capability_requires_an_actual_mutating_tool() {
        assert!(!role("scout", None).is_write_capable());
        assert!(role("artisan", None).is_write_capable());
        assert!(!role("reviewer", Some(vec!["read".into(), "grep".into()])).is_write_capable());
        assert!(
            !role(
                "coordinator",
                Some(vec![
                    "read".into(),
                    "agent".into(),
                    "ask_user".into(),
                    "unknown".into()
                ])
            )
            .is_write_capable()
        );
        assert!(role("writer", Some(vec!["read".into(), "edit".into()])).is_write_capable());
        assert!(role("runner", Some(vec!["powershell".into()])).is_write_capable());
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
