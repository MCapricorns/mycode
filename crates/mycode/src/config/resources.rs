//! Workspace and global resource files feeding the system prompt.
//!
//! Bounded markdown files discovered at fixed locations: the MYCode home
//! (`AGENTS.md`), then the session workspace (`AGENTS.md`, `MYCODE.md`,
//! `.mycode/agents.md`, `.agents/AGENTS.md`). Each file becomes one system
//! prompt contribution. Nothing enters the prompt unbounded or from arbitrary paths.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::config::{ConfigError, HomeLayout};

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
/// Order is stable: global defaults first, then workspace overrides in the
/// order AGENTS.md, MYCODE.md, .mycode/agents.md, .agents/AGENTS.md. Later
/// workspace contributions win conflicts at the same directory scope.
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
    // magpie's Library writes the home-level instructions file; the user
    // home's own `.agents` tree is not a source.
    push("AGENTS.md", home.root().join("AGENTS.md"), true);
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
    files
}

/// Maximum skills carrying full metadata in one system prompt.
///
/// Later enabled entries keep compact source pointers within the catalog budget.
pub const MAX_SKILLS: usize = 32;
/// Maximum characters in the on-demand skill catalog, including omission notices.
const MAX_SKILL_CATALOG_CHARS: usize = 32 * 1024;

/// One slash-command skill discovered under `.agents`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillFile {
    /// Command slug without the leading `/`.
    pub slug: String,
    /// One-line title from the first heading, the frontmatter `name`, or
    /// the slug derived from the file name.
    pub title: String,
    /// Frontmatter `description`: when to use the skill. Empty when absent.
    pub description: String,
    /// Absolute path of the skill markdown.
    pub path: PathBuf,
    /// Whether this file came from the user-global `.agents` tree.
    pub global: bool,
}

/// Host-global skill catalog roots for read-only tool anchoring.
///
/// Skills live only in magpie's Library folder (`<mycode_home>/skills`), so a
/// machine without a Library anchors nothing.
#[must_use]
pub fn skill_read_roots(mycode_home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = mycode_home {
        let skills = home.join("skills");
        if skills.is_dir() {
            roots.push(skills);
        }
    }
    roots
}

/// Discovers `/` skills from magpie's Library folder.
///
/// magpie installs skills as `<name>/SKILL.md` folders under
/// `<mycode_home>/skills`. Entries are sorted by path before they are read,
/// duplicate slugs keep the first hit, and the result is sorted by slug.
/// There is no discovery cap; prompt rendering bounds the detailed metadata and total text.
#[must_use]
pub fn discover_skills(mycode_home: Option<&Path>) -> Vec<SkillFile> {
    let mut skills = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if let Some(mycode_home) = mycode_home {
        let root = mycode_home.join("skills");
        collect_skills(&root, &mut skills, &mut seen, true);
        collect_skills(&root.join("skills"), &mut skills, &mut seen, true);
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
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
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
    let (title, description) = skill_meta(path, &slug);
    skills.push(SkillFile {
        slug,
        title,
        description,
        path: path.to_path_buf(),
        global,
    });
}

/// One skill's derived prompt metadata under its cache identity.
struct CachedSkillMeta {
    mtime: std::time::SystemTime,
    len: u64,
    title: String,
    description: String,
}

/// Skill-metadata cache capacity. Path keys carry no natural bound (unlike
/// a `TypeId`-keyed cache), so the map is explicitly capped; past the cap
/// it clears wholesale and rebuilds, which is a cold restart, not a
/// behavior change.
const MAX_SKILL_META_CACHE: usize = 512;

fn skill_meta_cache()
-> &'static std::sync::RwLock<std::collections::HashMap<PathBuf, CachedSkillMeta>> {
    static CACHE: std::sync::OnceLock<
        std::sync::RwLock<std::collections::HashMap<PathBuf, CachedSkillMeta>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()))
}

/// `(title, description)` for one SKILL.md, cached process-wide by
/// `(path, mtime, len)`.
///
/// Every turn, every subagent run, and every skills-page refresh runs
/// [`discover_skills`], and each skill file used to be read in full (up to
/// [`MAX_RESOURCE_BYTES`]) just to lift its frontmatter and first heading.
/// The cache removes that repeat I/O.
///
/// Boundaries, kept explicit:
/// - Only successful reads are cached. A failed read (including a
///   transient Windows sharing violation) keeps today's behavior — the
///   slug as title, no description — and writes nothing, so the next
///   discovery retries the file and self-heals.
/// - A file whose `fs::metadata()` or `modified()` fails has no cache
///   identity: it takes the uncached prefix read path and is never written.
/// - Accepted staleness: an edit that keeps the byte length and lands
///   within the filesystem's mtime granularity serves the old summary
///   until the next `mtime`/`len` change. Only the catalog line and the
///   skills page are affected — a SKILL.md body is never cached; the
///   model's `read` and the frontend's file view always read the disk.
fn skill_meta(path: &Path, slug: &str) -> (String, String) {
    let uncached =
        || read_skill_meta(path, slug).unwrap_or_else(|| (slug.to_owned(), String::new()));
    let Ok(metadata) = std::fs::metadata(path) else {
        return uncached();
    };
    let Ok(mtime) = metadata.modified() else {
        return uncached();
    };
    let len = metadata.len();
    if let Ok(cache) = skill_meta_cache().read()
        && let Some(hit) = cache.get(path)
        && hit.mtime == mtime
        && hit.len == len
    {
        return (hit.title.clone(), hit.description.clone());
    }
    let Some((title, description)) = read_skill_meta(path, slug) else {
        return (slug.to_owned(), String::new());
    };
    // A poisoned lock only means a panic elsewhere while it was held; the
    // map is still structurally valid.
    let mut cache = skill_meta_cache()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= MAX_SKILL_META_CACHE {
        cache.clear();
    }
    cache.insert(
        path.to_path_buf(),
        CachedSkillMeta {
            mtime,
            len,
            title: title.clone(),
            description: description.clone(),
        },
    );
    (title, description)
}

/// Reads one SKILL.md and lifts its `(title, description)`. `None` when
/// the read fails; that state is never cached.
fn read_skill_meta(path: &Path, slug: &str) -> Option<(String, String)> {
    use std::io::Read as _;

    // Only metadata belongs in discovery. A long skill body must not hide
    // valid frontmatter, and an incomplete final line is not metadata.
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_RESOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_RESOURCE_BYTES {
        bytes.truncate(MAX_RESOURCE_BYTES);
        let end = bytes.iter().rposition(|byte| *byte == b'\n')?;
        bytes.truncate(end + 1);
    }
    let text = String::from_utf8(bytes).ok()?;
    let (name, description) = skill_frontmatter(&text);
    let title = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .or(name)
        .unwrap_or_else(|| slug.to_owned());
    Some((
        truncate_skill_metadata(&title, MAX_SKILL_TITLE_CHARS),
        description.unwrap_or_default(),
    ))
}

/// Longest skill title carried into the prompt catalog and settings list.
const MAX_SKILL_TITLE_CHARS: usize = 120;
/// Longest skill description carried into the prompt catalog.
const MAX_SKILL_DESCRIPTION_CHARS: usize = 300;

fn truncate_skill_metadata(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

/// `name` and `description` from a SKILL.md frontmatter block. The format is
/// YAML; this reads the plain, quoted, and folded (`>` / `|`) scalar forms
/// skill files use and ignores every other key.
fn skill_frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|line| line.trim_end()) != Some("---") {
        return (None, None);
    }
    let Some(end) = lines[1..].iter().position(|line| line.trim_end() == "---") else {
        return (None, None);
    };
    let header = &lines[1..=end];
    let field = |key: &str| -> Option<String> {
        let start = header.iter().position(|line| {
            line.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with(':'))
        })?;
        let first = header[start][key.len() + 1..].trim();
        let value = if matches!(first, ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            header[start + 1..]
                .iter()
                .take_while(|line| line.starts_with([' ', '\t']) || line.trim().is_empty())
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            first
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .or_else(|| {
                    first
                        .strip_prefix('\'')
                        .and_then(|rest| rest.strip_suffix('\''))
                })
                .unwrap_or(first)
                .to_owned()
        };
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        (!value.is_empty()).then(|| truncate_skill_metadata(&value, MAX_SKILL_DESCRIPTION_CHARS))
    };
    (field("name"), field("description"))
}

/// Reads one resource file into bounded UTF-8 text.
///
/// # Errors
///
/// Returns [`ConfigError`] when the file cannot be read, exceeds the resource
/// byte cap, or is not UTF-8. A read failure is an authority rejection so a
/// prompt resource never surfaces a raw operating-system error.
pub(crate) fn read_resource(path: &Path) -> Result<String, ConfigError> {
    use std::io::Read as _;

    // Read one byte past the cap so an oversized file is detected without
    // pulling all of it into memory.
    let file = std::fs::File::open(path).map_err(|_| ConfigError::authority_rejection())?;
    let mut bytes = Vec::new();
    file.take(MAX_RESOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::authority_rejection())?;
    if bytes.len() > MAX_RESOURCE_BYTES {
        return Err(ConfigError::new(crate::config::ConfigErrorKind::Oversized));
    }
    String::from_utf8(bytes).map_err(|_| ConfigError::authority_rejection())
}

/// Renders discovered resources into ordered system prompt parts.
///
/// Each contribution names its source. Unreadable or omitted bodies leave an
/// explicit source pointer, so a missing workspace override is not mistaken
/// for absent instructions. Body text and notices share one prompt budget.
#[must_use]
pub fn render_resource_prompt(files: &[ResourceFile]) -> Vec<String> {
    let sources: Vec<_> = files
        .iter()
        .map(|file| {
            let scope = if file.global { "global" } else { "workspace" };
            let header = format!(
                "# {} ({scope})\nSource: {}\n\n",
                file.name,
                file.path.display(),
            );
            let notice = format!(
                "{header}Instructions were not included automatically because the source could \
not be read within the context limits. Read this exact source before work governed by it; \
if it remains unreadable, report that limitation."
            );
            (header, notice)
        })
        .collect();
    let mut reserved = sources
        .iter()
        .map(|(_, notice)| notice.chars().count())
        .sum::<usize>();
    if reserved > MAX_TOTAL_PROMPT_CHARS {
        return vec![
            "Instruction source paths exceed the context budget. Ask for the relevant instruction \
file paths before work that depends on those rules."
                .to_owned(),
        ];
    }
    let mut parts = Vec::new();
    let mut total = 0usize;
    for (file, (header, notice)) in files.iter().zip(sources) {
        reserved -= notice.chars().count();
        let rendered = read_resource(&file.path)
            .ok()
            .map(|text| format!("{header}{text}"))
            .filter(|text| total + text.chars().count() + reserved <= MAX_TOTAL_PROMPT_CHARS)
            .unwrap_or(notice);
        total += rendered.chars().count();
        parts.push(rendered);
    }
    parts
}

/// Compact on-demand skill catalog for the system prompt.
///
/// Lists slug, title, description when present, and path. Skill bodies stay
/// on disk until the model reads the named file or the user inserts a `/slug`
/// pointer.
#[must_use]
pub fn render_skill_catalog(files: &[SkillFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut out = String::from(
        "<skills>\nThis catalog lists enabled skills by purpose and source path; it does not \
contain their instructions. Before acting, select skills whose stated scope matches the task; \
a message starting with `/slug` explicitly selects that skill. Read the selected SKILL.md at \
its exact listed path with `read`, following the tool calling convention above, or another \
available file reader. Apply the instructions before the work they govern. Resolve relative \
references from that skill's directory and read them only when needed. Reuse content already \
read while it remains current; re-read a changed source. The catalog refreshes before model \
requests, so use the latest enabled entries. Later entries may contain only a source pointer; \
inspect that file's metadata before selecting it. If a selected skill is unavailable or unreadable, \
report the specific gap and continue independent work; do not infer its instructions from its \
title or description.",
    );
    let omission = |count| {
        format!(
            "\n{count} more enabled skills exceed the catalog budget. If an explicitly requested \
skill is missing, use its user-supplied exact path or ask for that path; do not guess a path \
or scan unrelated or disabled skills."
        )
    };
    let reserved = omission(files.len()).chars().count() + "\n</skills>".len();
    let mut used = out.chars().count();
    for (index, skill) in files.iter().enumerate() {
        let mut entry = format!("\n- /{}", skill.slug);
        if index < MAX_SKILLS {
            let _ = write!(entry, " — {}", skill.title);
            if !skill.description.is_empty() {
                let _ = write!(entry, ": {}", skill.description);
            }
        }
        let _ = write!(entry, " (`{}`)", skill.path.display());
        let chars = entry.chars().count();
        if used + chars + reserved > MAX_SKILL_CATALOG_CHARS {
            out.push_str(&omission(files.len() - index));
            break;
        }
        used += chars;
        out.push_str(&entry);
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
            crate::core::unix_now().as_nanos()
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
    fn global_rules_precede_project_overrides_and_name_every_source() {
        let root = temp("instructions");
        let home_root = root.0.join("home");
        let workspace = root.0.join("project");
        fs::create_dir_all(&home_root).unwrap();
        fs::create_dir_all(workspace.join(".mycode")).unwrap();
        fs::create_dir_all(workspace.join(".agents")).unwrap();
        let paths = [
            home_root.join("AGENTS.md"),
            workspace.join("AGENTS.md"),
            workspace.join("MYCODE.md"),
            workspace.join(".mycode").join("agents.md"),
            workspace.join(".agents").join("AGENTS.md"),
        ];
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, format!("instruction-{index}")).unwrap();
        }
        let home = crate::config::HomeLayout::from_root(&home_root).unwrap();
        let resources = super::discover_resources(&home, &workspace);
        assert_eq!(
            resources.iter().map(|file| &file.path).collect::<Vec<_>>(),
            paths.iter().collect::<Vec<_>>()
        );
        assert!(resources[0].global);
        assert!(resources[1..].iter().all(|file| !file.global));
        let rendered = super::render_resource_prompt(&resources);
        assert_eq!(rendered.len(), paths.len());
        for (index, part) in rendered.iter().enumerate() {
            assert!(part.contains(&format!("Source: {}", paths[index].display())));
            assert!(part.ends_with(&format!("instruction-{index}")));
        }
        let shared_root = super::discover_resources(&home, &home_root);
        assert_eq!(shared_root.len(), 1);
        assert!(shared_root[0].global);
        fs::write(&paths[0], "updated global rule").unwrap();
        assert!(
            super::render_resource_prompt(&super::discover_resources(&home, &workspace))[0]
                .ends_with("updated global rule")
        );
    }

    #[test]
    fn prompt_budget_preserves_rules_and_identifies_the_body_that_does_not_fit() {
        let root = temp("instruction-budget");
        let home_root = root.0.join("home");
        let workspace = root.0.join("project");
        fs::create_dir_all(&home_root).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            home_root.join("AGENTS.md"),
            "g".repeat(super::MAX_RESOURCE_BYTES),
        )
        .unwrap();
        fs::write(
            workspace.join("AGENTS.md"),
            "p".repeat(super::MAX_RESOURCE_BYTES),
        )
        .unwrap();
        fs::write(workspace.join("MYCODE.md"), "small project rule").unwrap();
        let home = crate::config::HomeLayout::from_root(&home_root).unwrap();
        let parts = super::render_resource_prompt(&super::discover_resources(&home, &workspace));
        assert_eq!(parts.len(), 3);
        assert!(parts[0].starts_with("# AGENTS.md (global)"));
        assert!(parts[1].contains(&workspace.join("AGENTS.md").display().to_string()));
        assert!(parts[1].contains("Instructions were not included automatically"));
        assert!(parts[2].ends_with("small project rule"));
        assert!(
            parts.iter().map(|part| part.chars().count()).sum::<usize>()
                <= super::MAX_TOTAL_PROMPT_CHARS
        );
    }

    #[test]
    fn oversized_instruction_file_keeps_its_source_and_requires_explicit_reading() {
        let root = temp("large-instructions");
        let path = root.0.join("AGENTS.md");
        fs::write(&path, "x".repeat(super::MAX_RESOURCE_BYTES + 1)).unwrap();
        let home = crate::config::HomeLayout::from_root(&root.0).unwrap();
        let parts = super::render_resource_prompt(&super::discover_resources(&home, &root.0));
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains(&path.display().to_string()));
        assert!(parts[0].contains("Read this exact source"));
    }

    #[test]
    fn read_roots_list_the_library_and_drop_missing() {
        assert!(super::skill_read_roots(None).is_empty());
        let home = temp("roots");
        assert!(super::skill_read_roots(Some(&home.0)).is_empty());
        fs::create_dir_all(home.0.join("skills")).unwrap();
        let roots = super::skill_read_roots(Some(&home.0));
        assert_eq!(roots, vec![home.0.join("skills")]);
    }

    #[test]
    fn magpie_library_skills_are_discovered_and_anchored() {
        let home = temp("magpie-home");
        skill(&home.0.join("skills"), "magpie-skill", "Magpie Skill");
        let found = discover_skills(Some(&home.0));
        assert_eq!(found.len(), 1);
        assert!(found[0].global);
        assert_eq!(
            super::skill_read_roots(Some(&home.0)),
            vec![home.0.join("skills")]
        );
        assert!(discover_skills(None).is_empty());
    }

    #[test]
    fn a_flat_skill_file_matches_its_extension_in_any_case() {
        let home = temp("flat-case");
        let skills = home.0.join("skills");
        fs::create_dir_all(&skills).unwrap();
        fs::write(skills.join("upper.MD"), "# upper\n").unwrap();
        fs::write(skills.join("lower.md"), "# lower\n").unwrap();
        fs::write(skills.join("notes.txt"), "not a skill\n").unwrap();
        let found = discover_skills(Some(&home.0));
        let slugs: Vec<_> = found.iter().map(|skill| skill.slug.as_str()).collect();
        assert_eq!(slugs, ["lower", "upper"]);
    }

    #[test]
    fn skills_sort_by_slug() {
        let home = temp("sort");
        for slug in ["zebra", "alpha", "middle"] {
            skill(&home.0.join("skills"), slug, slug);
        }
        let first = discover_skills(Some(&home.0));
        let second = discover_skills(Some(&home.0));
        assert_eq!(first, second);
        let slugs: Vec<_> = first.iter().map(|skill| skill.slug.as_str()).collect();
        assert_eq!(slugs, ["alpha", "middle", "zebra"]);
        assert!(first[0].global);
    }

    #[test]
    fn catalog_lists_the_skill_command() {
        let home = temp("catalog");
        skill(&home.0.join("skills"), "review", "Review");
        let found = discover_skills(Some(&home.0));
        let catalog = super::render_skill_catalog(&found).expect("catalog");
        assert!(catalog.contains("/review"), "{catalog}");
    }

    #[test]
    fn frontmatter_description_reaches_the_catalog() {
        let root = temp("frontmatter");
        let dir = root.0.join("skills").join("pdf");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: pdf-tools\ndescription: >\n  Fill and merge PDF forms.\n  Use for any .pdf task.\nlicense: MIT\n---\n\nSteps.\n",
        )
        .unwrap();
        let skills = discover_skills(Some(&root.0));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].title, "pdf-tools");
        assert_eq!(
            skills[0].description,
            "Fill and merge PDF forms. Use for any .pdf task."
        );
        let catalog = super::render_skill_catalog(&skills).unwrap();
        assert!(catalog.contains("/pdf — pdf-tools: "), "{catalog}");
    }

    #[test]
    fn large_skill_keeps_frontmatter_when_the_metadata_prefix_cuts_utf8() {
        let root = temp("large-metadata");
        let dir = root.0.join("skills").join("large");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        let mut body =
            String::from("---\nname: large-skill\ndescription: Inspect a large project.\n---\n\n");
        body.push_str(&"x".repeat(super::MAX_RESOURCE_BYTES - body.len() - 1));
        body.push_str("🦀\nMore instructions.");
        fs::write(&path, body).unwrap();
        let skills = discover_skills(Some(&root.0));
        assert_eq!(skills[0].title, "large-skill");
        assert_eq!(skills[0].description, "Inspect a large project.");
        assert!(
            super::read_resource(&path).is_err(),
            "full instruction limits stay strict"
        );
    }

    #[test]
    fn skill_catalog_has_a_total_budget_and_reports_omitted_entries() {
        let skills: Vec<_> = (0..200)
            .map(|index| super::SkillFile {
                slug: format!("skill-{index}"),
                title: "A bounded title".into(),
                description: "A bounded description".into(),
                path: PathBuf::from(format!(
                    "/skills/{}/skill-{index}/SKILL.md",
                    "x".repeat(200)
                )),
                global: true,
            })
            .collect();
        let catalog = super::render_skill_catalog(&skills).unwrap();
        assert!(catalog.chars().count() <= super::MAX_SKILL_CATALOG_CHARS);
        assert!(catalog.contains("more enabled skills exceed the catalog budget"));
        assert!(catalog.contains("/skill-32 (`/skills/"));
        assert!(catalog.ends_with("\n</skills>"));
    }

    /// The metadata cache sits between the directory scan and the prompt:
    /// an unchanged file serves the cached summary, a real edit (length
    /// change) is picked up on the next discovery.
    #[test]
    fn skill_metadata_is_cached_until_the_file_changes() {
        let root = temp("meta-cache");
        let dir = root.0.join("skills").join("pdf");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        // No `# ` heading: the frontmatter name is the title.
        fs::write(
            &path,
            "---\nname: pdf-tools\ndescription: Fill and merge forms.\n---\n\nSteps.\n",
        )
        .unwrap();
        let first = discover_skills(Some(&root.0));
        assert_eq!(
            (first[0].title.as_str(), first[0].description.as_str()),
            ("pdf-tools", "Fill and merge forms.")
        );
        let second = discover_skills(Some(&root.0));
        assert_eq!(first, second, "an unchanged file serves the same summary");
        // A rewrite with a different length invalidates the cache entry
        // through `len`, whatever the filesystem's mtime granularity did
        // with the rewrite.
        fs::write(
            &path,
            "---\nname: pdf-tools-v2\ndescription: Fill, merge, and stamp form fields.\n---\n\nSteps.\n",
        )
        .unwrap();
        let third = discover_skills(Some(&root.0));
        assert_eq!(
            (third[0].title.as_str(), third[0].description.as_str()),
            ("pdf-tools-v2", "Fill, merge, and stamp form fields.")
        );
    }
}
