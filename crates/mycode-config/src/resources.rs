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

/// Maximum skills named in one system prompt.
///
/// [`discover_skills`] returns every match in a stable order. Prompt
/// builders truncate to this cap so the included set does not depend on
/// directory iteration order.
pub const MAX_SKILLS: usize = 32;

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
///
/// Directory entries are sorted by path before they are read, duplicate slugs
/// keep the first hit (workspace before global), and the result is sorted by
/// slug. There is no discovery cap; callers truncate to [`MAX_SKILLS`].
#[must_use]
pub fn discover_skills(workspace_root: &Path, user_home: Option<&Path>) -> Vec<SkillFile> {
    let mut skills = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push_root = |root: &Path, global: bool| {
        collect_skills(root, &mut skills, &mut seen, global);
        collect_skills(&root.join("skills"), &mut skills, &mut seen, global);
    };
    push_root(&workspace_root.join(".agents"), false);
    if let Some(user_home) = user_home {
        push_root(&user_home.join(".agents"), true);
    }
    skills.sort_by(|left, right| {
        left.slug
            .cmp(&right.slug)
            .then_with(|| left.global.cmp(&right.global))
            .then_with(|| left.path.cmp(&right.path))
    });
    skills
}

fn collect_skills(
    dir: &Path,
    skills: &mut Vec<SkillFile>,
    seen: &mut std::collections::BTreeSet<String>,
    global: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
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
/// Returns [`ConfigError`] for IO failures, oversized files, or non-UTF-8
/// content.
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::discover_skills;

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp(label: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "mycode-skills-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn skill(dir: &std::path::Path, slug: &str, title: &str) {
        let path = dir.join(slug);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("SKILL.md"), format!("# {title}\n")).unwrap();
    }

    #[test]
    fn skills_sort_by_slug_and_workspace_wins_duplicates() {
        let workspace = temp("workspace");
        let user = temp("user");
        for slug in ["zebra", "alpha", "middle"] {
            skill(&workspace.0.join(".agents"), slug, slug);
        }
        skill(&user.0.join(".agents"), "alpha", "global alpha");
        skill(&user.0.join(".agents"), "only-global", "global");
        let first = discover_skills(&workspace.0, Some(&user.0));
        let second = discover_skills(&workspace.0, Some(&user.0));
        assert_eq!(first, second);
        let slugs: Vec<_> = first.iter().map(|skill| skill.slug.as_str()).collect();
        assert_eq!(slugs, ["alpha", "middle", "only-global", "zebra"]);
        assert!(!first[0].global);
        assert_eq!(first[0].title, "alpha");
        assert!(
            first
                .iter()
                .any(|skill| skill.slug == "only-global" && skill.global)
        );
    }
}
