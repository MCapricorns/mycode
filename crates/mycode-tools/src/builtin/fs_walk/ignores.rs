//! Ignore-layer engine and Git-boundary discovery for the handle-relative walk.
//!
//! Nested git roots drop outer Gitignore/GitExclude layers; `.ignore` layers
//! stay. Ignore state is a persistent `Arc` linked list so adding a layer is
//! O(1) and ancestor frames keep the previous head.
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use tokio_util::sync::CancellationToken;

use super::*;

/// Ignore-file class used by [`ignore::WalkBuilder`] precedence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IgnoreFileKind {
    /// `.ignore`, highest ignore-file precedence.
    Ignore,
    /// `.gitignore`, applied only after a `.git` ancestor is seen.
    Gitignore,
    /// `.git/info/exclude`.
    GitExclude,
}

#[derive(Clone, Debug)]
struct IgnoreLayer {
    base: PathBuf,
    /// Path from this ignore file's directory to the allowed root.
    ///
    /// Empty for layers at or below the session cwd. Ancestor Git layers
    /// prepend this so `repo/.gitignore` matches `subdir/file` when cwd
    /// is `repo/subdir`.
    ancestor_prefix: PathBuf,
    kind: IgnoreFileKind,
    matcher: Gitignore,
}

/// Persistent ignore-layer node. Pushing a layer is an `Arc` allocation of
/// one node; ancestors keep the previous head without copying the chain.
#[derive(Clone, Debug)]
struct IgnoreNode {
    layer: IgnoreLayer,
    parent: Option<Arc<IgnoreNode>>,
}

/// Shared ignore-layer chain. Cloning a frame is two `Arc` bumps.
#[derive(Clone, Debug, Default)]
pub(crate) struct IgnoreStack {
    git_enabled: bool,
    ignore_head: Option<Arc<IgnoreNode>>,
    git_head: Option<Arc<IgnoreNode>>,
}

impl IgnoreStack {
    /// Discovers the Git boundary above `allowed` and seeds ancestor layers.
    ///
    /// Walks parent directories through handle-relative `..` until a `.git`
    /// entry, the filesystem root, or [`MAX_GIT_PARENT_HOPS`]. A `.git` file
    /// is parsed for `gitdir:` and `commondir`. Failure to establish a
    /// boundary after a `.git` entry is seen, or after a parent hop is
    /// refused, is terminating.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when a parent cannot be opened, a `.git` file
    /// cannot be parsed, or `commondir` / `info/exclude` cannot be loaded.
    pub(crate) fn seed_git_boundary(
        &mut self,
        allowed: &File,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Err(stopped_error(limiter));
        }
        match probe_git_entry(allowed) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }

        let mut current = allowed.try_clone()?;
        let mut names_to_cwd: Vec<OsString> = Vec::new();
        let mut hops = 0usize;
        loop {
            hops += 1;
            if hops > MAX_GIT_PARENT_HOPS {
                return Err(io::Error::other(
                    "search ignore files cannot be loaded: git boundary is too deep",
                ));
            }
            let parent = match open_parent_directory(&current)? {
                ParentDirectory::FilesystemRoot => return Ok(()),
                ParentDirectory::Parent(parent) => parent,
            };
            if files_same_identity(&parent, &current)? {
                return Ok(());
            }
            let child_name = child_name_in_parent(&parent, &current, limiter, cancel)?;
            names_to_cwd.insert(0, child_name);
            match probe_git_entry(&parent) {
                Ok(Some(kind)) => {
                    apply_git_root(self, &parent, kind, &names_to_cwd, limiter, cancel)?;
                    return load_ancestor_gitignores(self, &parent, &names_to_cwd, limiter, cancel);
                }
                Ok(None) => {}
                Err(error) if is_not_found(&error) => {}
                Err(error) => return Err(error),
            }
            current = parent;
        }
    }

    pub(crate) fn ingest(
        &mut self,
        dir: &File,
        allowed_rel: &Path,
        limiter: &WalkLimiter,
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Err(stopped_error(limiter));
        }
        match probe_git_entry(dir) {
            Ok(Some(kind)) => {
                if self.git_enabled {
                    // Nested git root: WalkBuilder stops outer Gitignore and git
                    // exclude at this boundary. `.ignore` layers keep stacking.
                    self.git_head = None;
                }
                apply_git_root(self, dir, kind, &[], limiter, cancel)?;
            }
            Ok(None) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
        if self.git_enabled
            && let Some(text) = read_child_text(dir, ".gitignore", limiter, cancel)?
        {
            self.push_layer(
                allowed_rel.to_path_buf(),
                PathBuf::new(),
                &text,
                IgnoreFileKind::Gitignore,
                limiter,
            )?;
        }
        if let Some(text) = read_child_text(dir, ".ignore", limiter, cancel)? {
            self.push_layer(
                allowed_rel.to_path_buf(),
                PathBuf::new(),
                &text,
                IgnoreFileKind::Ignore,
                limiter,
            )?;
        }
        Ok(())
    }

    fn push_layer(
        &mut self,
        base: PathBuf,
        ancestor_prefix: PathBuf,
        text: &str,
        kind: IgnoreFileKind,
        limiter: &WalkLimiter,
    ) -> io::Result<()> {
        limiter.try_reserve_ignore_layer()?;
        let mut builder = GitignoreBuilder::new(".");
        for (index, line) in text.lines().enumerate() {
            let line = if index == 0 {
                line.trim_start_matches('\u{feff}')
            } else {
                line
            };
            limiter.try_reserve_ignore_rule()?;
            builder.add_line(None, line).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid ignore rule on line {}: {error}", index + 1),
                )
            })?;
        }
        let gitignore = builder.build().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ignore rules cannot be compiled: {error}"),
            )
        })?;
        if gitignore.is_empty() {
            limiter.release_ignore_layer();
            return Ok(());
        }
        let node = Arc::new(IgnoreNode {
            layer: IgnoreLayer {
                base,
                ancestor_prefix,
                kind,
                matcher: gitignore,
            },
            parent: match kind {
                IgnoreFileKind::Ignore => self.ignore_head.clone(),
                IgnoreFileKind::Gitignore | IgnoreFileKind::GitExclude => self.git_head.clone(),
            },
        });
        match kind {
            IgnoreFileKind::Ignore => self.ignore_head = Some(node),
            IgnoreFileKind::Gitignore | IgnoreFileKind::GitExclude => self.git_head = Some(node),
        }
        Ok(())
    }

    pub(super) fn is_ignored(&self, target_relative: &Path, walk_rel: &Path, is_dir: bool) -> bool {
        let allowed_rel = join_rel(target_relative, walk_rel);
        // Closest match wins within a kind; `.ignore` outranks `.gitignore`,
        // which outranks git exclude. A descendant whitelist therefore cannot
        // override a higher-precedence ancestor rule.
        layer_match(
            self.ignore_head.as_ref(),
            IgnoreFileKind::Ignore,
            &allowed_rel,
            is_dir,
        )
        .or(layer_match(
            self.git_head.as_ref(),
            IgnoreFileKind::Gitignore,
            &allowed_rel,
            is_dir,
        ))
        .or(layer_match(
            self.git_head.as_ref(),
            IgnoreFileKind::GitExclude,
            &allowed_rel,
            is_dir,
        ))
        .is_ignore()
    }
}

/// Hidden or ignore-excluded relative to the allowed root.
///
/// The empty path (the session cwd / selected root with no suffix) is never
/// skipped. Each on-disk component is checked so an explicit file target
/// and a Windows alias of that target use the same rule as walker children.
pub(crate) fn relative_is_skipped(ignores: &IgnoreStack, relative: &Path, is_dir: bool) -> bool {
    if relative.as_os_str().is_empty() {
        return false;
    }
    let mut acc = PathBuf::new();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        acc.push(component.as_os_str());
        let last = index + 1 == components.len();
        if name_is_hidden(component.as_os_str()) {
            return true;
        }
        if ignores.is_ignored(Path::new(""), &acc, if last { is_dir } else { true }) {
            return true;
        }
    }
    false
}

fn layer_match(
    head: Option<&Arc<IgnoreNode>>,
    kind: IgnoreFileKind,
    allowed_rel: &Path,
    is_dir: bool,
) -> ignore::Match<()> {
    let mut node = head;
    while let Some(current) = node {
        if current.layer.kind == kind {
            let prefixed;
            let suffix = if !current.layer.ancestor_prefix.as_os_str().is_empty() {
                prefixed = current.layer.ancestor_prefix.join(allowed_rel);
                prefixed.as_path()
            } else if current.layer.base.as_os_str().is_empty() {
                allowed_rel
            } else {
                match allowed_rel.strip_prefix(&current.layer.base) {
                    Ok(suffix) => suffix,
                    Err(_) => {
                        node = current.parent.as_ref();
                        continue;
                    }
                }
            };
            match current.layer.matcher.matched(suffix, is_dir) {
                ignore::Match::Ignore(_) => return ignore::Match::Ignore(()),
                ignore::Match::Whitelist(_) => return ignore::Match::Whitelist(()),
                ignore::Match::None => {}
            }
        }
        node = current.parent.as_ref();
    }
    ignore::Match::None
}

#[derive(Clone, Copy, Debug)]
enum GitEntryKind {
    Directory,
    File,
}

fn probe_git_entry(dir: &File) -> io::Result<Option<GitEntryKind>> {
    match open_child_file(dir, OsStr::new(".git"), None, NameMatch::Exact) {
        Ok(git) => {
            let metadata = git.metadata()?;
            if metadata.is_dir() {
                Ok(Some(GitEntryKind::Directory))
            } else if metadata.is_file() {
                Ok(Some(GitEntryKind::File))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "search ignore files cannot be loaded: .git is not a file or directory",
                ))
            }
        }
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn apply_git_root(
    stack: &mut IgnoreStack,
    worktree: &File,
    kind: GitEntryKind,
    names_to_cwd: &[OsString],
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<()> {
    stack.git_enabled = true;
    let git_dir = match kind {
        GitEntryKind::Directory => open_child_file(
            worktree,
            OsStr::new(".git"),
            Some(FsEntryKind::Directory),
            NameMatch::Exact,
        )?,
        GitEntryKind::File => open_gitdir_from_file(worktree, limiter, cancel)?,
    };
    let common = resolve_common_dir(&git_dir, limiter, cancel)?;
    ingest_exclude_from_common(stack, &common, names_to_cwd, limiter, cancel)
}

fn open_gitdir_from_file(
    worktree: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<File> {
    let Some(text) = read_child_text(worktree, ".git", limiter, cancel)? else {
        return Err(io::Error::other(
            "search ignore files cannot be loaded: .git file is empty",
        ));
    };
    let gitdir = parse_gitdir_file(&text)?;
    open_git_metadata_dir(worktree, &gitdir)
}

fn parse_gitdir_file(text: &str) -> io::Result<PathBuf> {
    for (index, line) in text.lines().enumerate() {
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}').trim()
        } else {
            line.trim()
        };
        let Some(path) = line.strip_prefix("gitdir:") else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "search ignore files cannot be loaded: gitdir path is empty",
            ));
        }
        return Ok(PathBuf::from(path));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "search ignore files cannot be loaded: .git file has no gitdir",
    ))
}

fn resolve_common_dir(
    git_dir: &File,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<File> {
    match read_child_text(git_dir, "commondir", limiter, cancel) {
        Ok(Some(text)) => {
            let path = text.lines().next().unwrap_or("").trim();
            if path.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "search ignore files cannot be loaded: commondir is empty",
                ));
            }
            open_git_metadata_dir(git_dir, Path::new(path))
        }
        Ok(None) => git_dir.try_clone(),
        Err(error) => Err(error),
    }
}

fn open_git_metadata_dir(base: &File, path: &Path) -> io::Result<File> {
    if path.is_absolute() {
        return open_directory_nofollow(path);
    }
    let mut current = base.try_clone()?;
    let mut hops = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                hops += 1;
                if hops > MAX_GIT_PARENT_HOPS {
                    return Err(io::Error::other(
                        "search ignore files cannot be loaded: gitdir path is too deep",
                    ));
                }
                current = match open_parent_directory(&current)? {
                    ParentDirectory::Parent(parent) => parent,
                    ParentDirectory::FilesystemRoot => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "search ignore files cannot be loaded: gitdir path escaped past filesystem root",
                        ));
                    }
                };
            }
            Component::Normal(name) => {
                current = open_child_file(
                    &current,
                    name,
                    Some(FsEntryKind::Directory),
                    NameMatch::Exact,
                )?;
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "search ignore files cannot be loaded: relative gitdir has a root component",
                ));
            }
        }
    }
    Ok(current)
}

fn ingest_exclude_from_common(
    stack: &mut IgnoreStack,
    common: &File,
    names_to_cwd: &[OsString],
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let info = match open_child_file(
        common,
        OsStr::new("info"),
        Some(FsEntryKind::Directory),
        NameMatch::Exact,
    ) {
        Ok(info) => info,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Some(text) = read_child_text(&info, "exclude", limiter, cancel)? {
        stack.push_layer(
            PathBuf::new(),
            names_to_path(names_to_cwd),
            &text,
            IgnoreFileKind::GitExclude,
            limiter,
        )?;
    }
    Ok(())
}

fn load_ancestor_gitignores(
    stack: &mut IgnoreStack,
    git_root: &File,
    names_to_cwd: &[OsString],
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<()> {
    if let Some(text) = read_child_text(git_root, ".gitignore", limiter, cancel)? {
        stack.push_layer(
            PathBuf::new(),
            names_to_path(names_to_cwd),
            &text,
            IgnoreFileKind::Gitignore,
            limiter,
        )?;
    }
    let mut current = git_root.try_clone()?;
    for (index, name) in names_to_cwd.iter().enumerate() {
        if index + 1 == names_to_cwd.len() {
            break;
        }
        current = open_child_file(
            &current,
            name,
            Some(FsEntryKind::Directory),
            NameMatch::Exact,
        )?;
        if let Some(text) = read_child_text(&current, ".gitignore", limiter, cancel)? {
            stack.push_layer(
                PathBuf::new(),
                names_to_path(&names_to_cwd[index + 1..]),
                &text,
                IgnoreFileKind::Gitignore,
                limiter,
            )?;
        }
    }
    Ok(())
}

fn names_to_path(names: &[OsString]) -> PathBuf {
    let mut path = PathBuf::new();
    for name in names {
        path.push(name);
    }
    path
}

fn is_not_found(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

pub(super) fn read_child_text(
    parent: &File,
    name: &str,
    limiter: &WalkLimiter,
    cancel: &CancellationToken,
) -> io::Result<Option<String>> {
    let mut file = match open_child_file(
        parent,
        OsStr::new(name),
        Some(FsEntryKind::File),
        NameMatch::Exact,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
        return Err(stopped_error(limiter));
    }
    // Grow from empty. Never reserve `metadata.len()` bytes: a sparse file
    // reports a huge logical size and that reservation is the unbounded
    // allocation this cap exists to prevent. Do not fail closed on logical
    // size alone: a growing file can report a fitting size then exceed it.
    // Each kernel read requests at most remaining stored capacity plus one
    // probe byte so actual I/O cannot pass the declared cap by a chunk.
    let mut buf = Vec::new();
    let mut chunk = [0u8; IGNORE_READ_CHUNK];
    loop {
        if matches!(limiter.check(cancel), ignore::WalkState::Quit) {
            return Err(stopped_error(limiter));
        }
        let room = IGNORE_FILE_MAX_BYTES.saturating_sub(buf.len());
        let want = room.saturating_add(1).min(IGNORE_READ_CHUNK);
        crate::builtin::blocking::wait_for_worker_readable(&file)?;
        let read = file.read(&mut chunk[..want])?;
        if read == 0 {
            break;
        }
        if read > room {
            return Err(ignore_too_large());
        }
        limiter.add_ignore_stored(read)?;
        buf.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| io::Error::other("ignore file is not valid UTF-8"))
}

fn ignore_too_large() -> io::Error {
    io::Error::other("ignore file exceeds size limit")
}

fn stopped_error(limiter: &WalkLimiter) -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        limiter.stopped_reason().unwrap_or("search stopped"),
    )
}
