//! Workspace and global resource files feeding the system prompt.
//!
//! Bounded markdown files discovered at fixed locations: the session
//! workspace (`AGENTS.md`, `MYCODE.md`) and the MYCode home (`AGENTS.md`).
//! Each file becomes one system prompt contribution. Nothing enters the
//! prompt unbounded or from arbitrary paths.

use std::path::{Path, PathBuf};

use crate::{ConfigError, HomeLayout};

/// Maximum bytes read per resource file.
pub(crate) const MAX_RESOURCE_BYTES: usize = 64 * 1024;
/// Maximum resources in one catalog.
pub(crate) const MAX_RESOURCES: usize = 16;
/// Maximum total prompt characters across all resources.
pub(crate) const MAX_TOTAL_PROMPT_CHARS: usize = 96 * 1024;

/// One discovered resource file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceFile {
    /// Stable contribution title shown in the prompt header.
    pub name: String,
    /// Absolute file path.
    pub path: PathBuf,
    /// Whether this file is the global home-level resource.
    pub global: bool,
}

/// Discovers resource files for one session workspace.
///
/// Order is stable: the workspace files first, then the global home file.
/// Duplicates (the workspace root equal to the home root) are collapsed.
#[must_use]
pub fn discover_resources(home: &HomeLayout, workspace_root: &Path) -> Vec<ResourceFile> {
    let mut files = Vec::new();
    let mut push = |name: &str, path: PathBuf, global: bool| {
        if files.len() < MAX_RESOURCES
            && path.is_file()
            && !files.iter().any(|file: &ResourceFile| file.path == path)
        {
            files.push(ResourceFile {
                name: name.to_owned(),
                path,
                global,
            });
        }
    };
    push("AGENTS.md", workspace_root.join("AGENTS.md"), false);
    push("MYCODE.md", workspace_root.join("MYCODE.md"), false);
    push(
        "AGENTS.md",
        workspace_root.join(".mycode").join("agents.md"),
        false,
    );
    push(
        "AGENTS.md",
        workspace_root.join(".agents").join("AGENTS.md"),
        false,
    );
    push("AGENTS.md", home.root().join("AGENTS.md"), true);
    if let Some(user_home) = home.root().parent() {
        push(
            "AGENTS.md",
            user_home.join(".agents").join("AGENTS.md"),
            true,
        );
    }
    files
}

/// One slash-command skill discovered under `.agents`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillFile {
    /// Command slug without the leading `/`.
    pub slug: String,
    /// One-line title from the first heading or file stem.
    pub title: String,
    /// Absolute path of the skill markdown.
    pub path: PathBuf,
    /// Whether this file came from the user-global `.agents` tree.
    pub global: bool,
}

/// Discovers `/` skills from the workspace and the user-global `.agents` tree.
#[must_use]
pub fn discover_skills(workspace_root: &Path, user_home: Option<&Path>) -> Vec<SkillFile> {
    const MAX_SKILLS: usize = 24;
    let mut skills = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push_root = |root: &Path, global: bool| {
        collect_skills(root, &mut skills, &mut seen, MAX_SKILLS, global);
        collect_skills(
            &root.join("skills"),
            &mut skills,
            &mut seen,
            MAX_SKILLS,
            global,
        );
    };
    push_root(&workspace_root.join(".agents"), false);
    if let Some(user_home) = user_home {
        push_root(&user_home.join(".agents"), true);
    }
    skills
}

fn collect_skills(
    dir: &Path,
    skills: &mut Vec<SkillFile>,
    seen: &mut std::collections::BTreeSet<String>,
    max: usize,
    global: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if skills.len() >= max {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            let skill = path.join("SKILL.md");
            if skill.is_file() {
                push_skill(&skill, path.file_name(), skills, seen, global);
            }
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md") || name.ends_with(".md"))
        {
            push_skill(&path, path.file_stem(), skills, seen, global);
        }
    }
}

fn push_skill(
    path: &Path,
    stem: Option<&std::ffi::OsStr>,
    skills: &mut Vec<SkillFile>,
    seen: &mut std::collections::BTreeSet<String>,
    global: bool,
) {
    let Some(stem) = stem.and_then(|stem| stem.to_str()) else {
        return;
    };
    let slug = stem
        .trim()
        .trim_start_matches('.')
        .replace([' ', '_'], "-")
        .to_ascii_lowercase();
    if slug.is_empty() || slug == "agents" || !seen.insert(slug.clone()) {
        return;
    }
    let title = read_resource(path)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
                .map(str::to_owned)
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| slug.clone());
    skills.push(SkillFile {
        slug,
        title,
        path: path.to_path_buf(),
        global,
    });
}

/// Reads one resource file into bounded UTF-8 text.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file cannot be read, exceeds the resource
/// byte cap, or is not UTF-8. A read failure is an authority rejection so a
/// prompt resource never surfaces a raw operating-system error.
pub(crate) fn read_resource(path: &Path) -> Result<String, ConfigError> {
    let bytes = std::fs::read(path).map_err(|_| ConfigError::authority_rejection())?;
    if bytes.len() > MAX_RESOURCE_BYTES {
        return Err(ConfigError::new(crate::ConfigErrorKind::Oversized));
    }
    String::from_utf8(bytes).map_err(|_| ConfigError::authority_rejection())
}

/// Renders discovered resources into ordered system prompt parts.
///
/// Each contribution carries a header naming its source. Oversized or
/// unreadable files are skipped; the total stays within the bounded prompt
/// budget.
#[must_use]
pub fn render_resource_prompt(files: &[ResourceFile]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut total = 0usize;
    for file in files {
        let Ok(text) = read_resource(&file.path) else {
            continue;
        };
        let scope = if file.global { "global" } else { "workspace" };
        let rendered = format!("# {name} ({scope})\n\n{text}", name = file.name);
        let chars = rendered.chars().count();
        if total + chars > MAX_TOTAL_PROMPT_CHARS {
            break;
        }
        total += chars;
        parts.push(rendered);
    }
    parts
}

/// Compact on-demand skill catalog for the system prompt.
///
/// Lists slug, title, and path only. Skill bodies stay on disk until the
/// model reads the named file or the user inserts a `/slug` pointer.
#[must_use]
pub fn render_skill_catalog(files: &[SkillFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut out = String::from(
        "<skills>\nSkills (on demand). Scan this list before you act. When a \
task matches one, `read` that SKILL.md and follow it before building or \
answering. Do not wait for the user to name the skill. Do not paste skill \
bodies into the prompt.",
    );
    for skill in files {
        out.push_str(&format!(
            "\n- /{slug} — {title} (`{path}`)",
            slug = skill.slug,
            title = skill.title,
            path = skill.path.display()
        ));
    }
    out.push_str("\n</skills>");
    Some(out)
}
