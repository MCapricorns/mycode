#!/usr/bin/env python3
"""Move main forward to a release commit without force-pushing.

release-publish tags the built commit and uploads archives before calling
this script. When origin/main is still an ancestor of that commit, main is
fast-forwarded to it. When main moved during the platform builds, the
version bump is replayed onto the current tip and that new commit is
fast-forwarded. Conflicts are accepted only in Cargo.toml, Cargo.lock, and
CHANGELOG.md: the planned version wins, and Unreleased notes that landed on
main during the build stay. Any other conflict aborts the replay.

The local checkout is hard-reset. Run it on the disposable CI checkout.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import tempfile
from pathlib import Path

from release_plan import (
    BUMP_SUBJECT,
    REPO_URL,
    SHA_RE,
    WORKSPACE_PACKAGES,
    _next_boundary,
    _unreleased_span,
    _upsert_links,
    apply_bump,
    parse_stable,
    rewrite_changelog,
    rewrite_lock,
    rewrite_workspace_version,
    section_body,
    section_body_unreleased,
    workspace_version,
)

VERSION_FILES = ("Cargo.toml", "Cargo.lock", "CHANGELOG.md")
_SECTION_HEADING = re.compile(r"^#{3,6} \S")
_LOCAL_BRANCH = "ci-advance-main"


class _Rejected(Exception):
    """The remote rejected a push because it is not a fast-forward."""


def _novel_unreleased(main_body: str, promoted: str) -> str:
    """Return Unreleased lines from main that are not already in the release."""

    promoted_lines = {line.strip() for line in promoted.splitlines() if line.strip()}
    kept: list[str] = []
    pending: str | None = None
    emitted = False
    for line in main_body.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if _SECTION_HEADING.match(stripped):
            pending = stripped
            emitted = False
            continue
        if stripped in promoted_lines:
            continue
        if pending and not emitted:
            if kept:
                kept.append("")
            kept.append(pending)
            kept.append("")
            emitted = True
        kept.append(stripped)
    return "\n".join(kept).strip()


def _full_version_section(text: str, version: str) -> str:
    match = re.search(rf"(?m)^## \[{re.escape(version)}\][^\n]*\n", text)
    if not match:
        raise SystemExit(f"built CHANGELOG.md has no ## [{version}] section")
    end = _next_boundary(text, match.end())
    section = text[match.start() : end]
    if not section.endswith("\n"):
        section += "\n"
    if not section.endswith("\n\n"):
        section += "\n"
    return section


def merge_changelog(main_text: str, built_text: str, version: str) -> str:
    """Keep main's new Unreleased notes and the built release section.

    The built changelog is the one that was tagged. Notes promoted into
    ``version`` stay in that section. Bullets that showed up on main during
    the build and are not part of that section stay under Unreleased.
    """

    promoted = section_body(built_text, version)
    if not promoted.strip():
        raise SystemExit(f"built CHANGELOG.md has no notes for {version}")
    section = _full_version_section(built_text, version)
    if not main_text.endswith("\n"):
        main_text += "\n"
    if "## [Unreleased]" in main_text:
        _, body_start, body_end = _unreleased_span(main_text)
        novel = _novel_unreleased(main_text[body_start:body_end], promoted)
        replacement = f"\n{novel}\n\n" if novel else "\n"
        main_text = main_text[:body_start] + replacement + main_text[body_end:]
    match = re.search(rf"(?m)^## \[{re.escape(version)}\][^\n]*\n", main_text)
    if match:
        end = _next_boundary(main_text, match.end())
        main_text = main_text[: match.start()] + section + main_text[end:]
    elif "## [Unreleased]" in main_text:
        _, _, insert_at = _unreleased_span(main_text)
        main_text = main_text[:insert_at] + section + main_text[insert_at:]
    else:
        first = re.search(r"(?m)^## \[", main_text)
        insert_at = first.start() if first else len(main_text)
        main_text = main_text[:insert_at] + section + main_text[insert_at:]
    return _upsert_links(main_text, version)


def resolve_release_trees(
    cargo: str,
    lock: str,
    main_changelog: str,
    built_changelog: str,
    version: str,
) -> dict[str, str]:
    """Apply ``version`` onto main's manifests without downgrading them."""

    planned = parse_stable(version)
    current = parse_stable(workspace_version(cargo))
    if planned is None or current is None:
        raise SystemExit(
            f"cannot replay version {version!r} onto workspace version "
            f"{workspace_version(cargo)!r}"
        )
    if current < planned:
        cargo = rewrite_workspace_version(cargo, version)
        lock = rewrite_lock(lock, version)
    changelog = merge_changelog(main_changelog, built_changelog, version)
    return {
        "Cargo.toml": cargo,
        "Cargo.lock": lock,
        "CHANGELOG.md": changelog,
    }


def _git(
    repo: Path,
    *args: str,
    check: bool = True,
    env: dict[str, str] | None = None,
    input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    run_env = os.environ.copy()
    run_env.setdefault("GIT_TERMINAL_PROMPT", "0")
    if env:
        run_env.update(env)
    result = subprocess.run(
        ["git", "--no-pager", *args],
        cwd=repo,
        check=False,
        text=True,
        encoding="utf-8",
        capture_output=True,
        env=run_env,
        input=input_text,
    )
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout or "").strip()
        raise SystemExit(f"git {' '.join(args)} failed: {detail}")
    return result


def _rev(repo: Path, ref: str) -> str:
    return _git(repo, "rev-parse", ref).stdout.strip().lower()


def _is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    result = _git(
        repo,
        "merge-base",
        "--is-ancestor",
        ancestor,
        descendant,
        check=False,
    )
    return result.returncode == 0


def _is_shallow(repo: Path) -> bool:
    return _git(repo, "rev-parse", "--is-shallow-repository").stdout.strip() == "true"


def _has_commit(repo: Path, sha: str) -> bool:
    return _git(repo, "cat-file", "-e", f"{sha}^{{commit}}", check=False).returncode == 0


def _is_non_ff(text: str) -> bool:
    lowered = text.lower()
    return (
        "non-fast-forward" in lowered
        or "fetch first" in lowered
        or "updates were rejected" in lowered
    )


def _blob(repo: Path, rev: str, path: str) -> str:
    return _git(repo, "show", f"{rev}:{path}").stdout


def _write(repo: Path, name: str, content: str) -> None:
    if content and not content.endswith("\n"):
        content += "\n"
    (repo / name).write_text(content, encoding="utf-8")


def _staged_empty(repo: Path) -> bool:
    result = _git(repo, "diff", "--cached", "--quiet", check=False)
    if result.returncode == 0:
        return True
    if result.returncode == 1:
        return False
    detail = (result.stderr or result.stdout or "").strip()
    raise SystemExit(f"git diff --cached failed: {detail}")


def _fetch_branch(repo: Path, remote: str, branch: str) -> None:
    """Fetch ``branch`` and enough history to see the built commit's parent.

    A depth-1 checkout of the built commit does not contain commits that
    landed on main during the platform builds. Unshallow that branch, and
    if the host rejects it, fetch a deep slice of the same ref.
    """

    refspec = f"+refs/heads/{branch}:refs/remotes/{remote}/{branch}"
    if _is_shallow(repo):
        result = _git(
            repo,
            "fetch",
            "--no-tags",
            "--unshallow",
            remote,
            refspec,
            check=False,
        )
        if result.returncode != 0:
            result = _git(
                repo,
                "fetch",
                "--no-tags",
                "--depth=1000",
                remote,
                refspec,
                check=False,
            )
        if result.returncode != 0:
            detail = (result.stderr or result.stdout or "").strip()
            raise SystemExit(f"failed to fetch {remote}/{branch}: {detail}")
        return
    result = _git(repo, "fetch", "--no-tags", remote, refspec, check=False)
    if result.returncode != 0:
        detail = (result.stderr or result.stdout or "").strip()
        raise SystemExit(f"failed to fetch {remote}/{branch}: {detail}")


def _parent_sha(repo: Path, sha: str) -> str:
    text = _git(repo, "cat-file", "-p", sha).stdout
    parents = [line.split()[1] for line in text.splitlines() if line.startswith("parent ")]
    if len(parents) != 1:
        raise SystemExit(f"{sha} is not a single-parent commit")
    return parents[0].lower()


def _ensure_parent(repo: Path, built: str, remote: str) -> str:
    parent = _parent_sha(repo, built)
    if _has_commit(repo, parent):
        return parent
    _git(repo, "fetch", "--no-tags", "--deepen=1000", remote, check=False)
    if not _has_commit(repo, parent):
        raise SystemExit(f"parent {parent} of {built} is not in this clone")
    return parent


def _changed_files(repo: Path, parent: str, built: str) -> list[str]:
    result = _git(repo, "diff", "--name-only", parent, built)
    names = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return [name for name in names if name not in VERSION_FILES]


def _apply_other_files(repo: Path, parent: str, built: str, files: list[str]) -> None:
    diff = _git(repo, "diff", parent, built, "--", *files)
    if not diff.stdout.strip():
        return
    result = _git(
        repo,
        "apply",
        "--index",
        "--whitespace=nowarn",
        "-",
        check=False,
        input_text=diff.stdout,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout or "").strip()
        raise SystemExit(
            "release bump changes files other than Cargo.toml, Cargo.lock, "
            "and CHANGELOG.md, and those changes do not apply onto current "
            f"main: {', '.join(files)}\n{detail}"
        )


def _checkout_commit(repo: Path, sha: str) -> None:
    _git(repo, "reset", "--hard", sha)
    _git(repo, "clean", "-fd")
    _git(repo, "checkout", "-B", _LOCAL_BRANCH, sha)


def _commit_as(repo: Path, built: str) -> None:
    name = _git(repo, "log", "-1", "--format=%an", built).stdout.strip()
    email = _git(repo, "log", "-1", "--format=%ae", built).stdout.strip()
    env = {
        "GIT_AUTHOR_NAME": name,
        "GIT_AUTHOR_EMAIL": email,
        "GIT_COMMITTER_NAME": name,
        "GIT_COMMITTER_EMAIL": email,
        "GIT_EDITOR": "true",
    }
    _git(repo, "-c", "commit.gpgsign=false", "commit", "-C", built, env=env)


def _assert_replay_tree(repo: Path, version: str) -> None:
    for name in VERSION_FILES:
        text = (repo / name).read_text(encoding="utf-8")
        if "<<<<<<<" in text or ">>>>>>>" in text:
            raise SystemExit(f"{name} still has conflict markers")
    cargo = (repo / "Cargo.toml").read_text(encoding="utf-8")
    current = parse_stable(workspace_version(cargo))
    planned = parse_stable(version)
    if current is None or planned is None or current < planned:
        raise SystemExit(
            f"replay left workspace version {workspace_version(cargo)} "
            f"behind planned {version}"
        )
    changelog = (repo / "CHANGELOG.md").read_text(encoding="utf-8")
    if f"## [{version}]" not in changelog:
        raise SystemExit(f"replayed CHANGELOG.md has no {version} section")


def _replay(repo: Path, built: str, remote_sha: str, remote: str) -> str:
    parent = _ensure_parent(repo, built, remote)
    if not _is_ancestor(repo, parent, remote_sha):
        raise SystemExit(
            f"{built} does not fast-forward {remote_sha} and its parent is "
            "not on main; refusing to force-push"
        )
    version = workspace_version(_blob(repo, built, "Cargo.toml"))
    if parse_stable(version) is None:
        raise SystemExit(f"built workspace version is not MAJOR.MINOR.PATCH: {version}")
    subject = _git(repo, "log", "-1", "--format=%s", built).stdout.strip()
    expected = BUMP_SUBJECT.format(version=version)
    if subject != expected:
        raise SystemExit(
            f"refusing to replay {built} ({subject!r}); expected {expected!r}. "
            "The tag still points at the built commit."
        )
    _checkout_commit(repo, remote_sha)
    others = _changed_files(repo, parent, built)
    if others:
        _apply_other_files(repo, parent, built, others)
    resolved = resolve_release_trees(
        cargo=_blob(repo, "HEAD", "Cargo.toml"),
        lock=_blob(repo, "HEAD", "Cargo.lock"),
        main_changelog=_blob(repo, "HEAD", "CHANGELOG.md"),
        built_changelog=_blob(repo, built, "CHANGELOG.md"),
        version=version,
    )
    for name, content in resolved.items():
        _write(repo, name, content)
    _git(repo, "add", "--", *resolved.keys())
    _assert_replay_tree(repo, version)
    if _staged_empty(repo):
        return remote_sha
    _commit_as(repo, built)
    replayed = _rev(repo, "HEAD")
    if _parent_sha(repo, replayed) != remote_sha:
        raise SystemExit(
            f"replay parent is not {remote_sha}; refusing to push"
        )
    return replayed


def _push(repo: Path, remote: str, branch: str, sha: str) -> None:
    result = _git(repo, "push", remote, f"{sha}:refs/heads/{branch}", check=False)
    if result.returncode == 0:
        return
    detail = ((result.stderr or "") + (result.stdout or "")).strip()
    if _is_non_ff(detail):
        raise _Rejected(detail)
    raise SystemExit(f"push to {branch} failed:\n{detail}")


def _advance_once(repo: Path, built: str, remote: str, branch: str) -> str:
    if not _has_commit(repo, built):
        raise SystemExit(f"built commit {built} is not in this clone")
    _fetch_branch(repo, remote, branch)
    remote_sha = _rev(repo, f"{remote}/{branch}")
    if built == remote_sha or _is_ancestor(repo, built, remote_sha):
        print(f"{branch} already contains {built}", flush=True)
        return remote_sha
    if _is_ancestor(repo, remote_sha, built):
        print(f"fast-forward {branch} to {built}", flush=True)
        _push(repo, remote, branch, built)
        return built
    print(
        f"{branch} moved to {remote_sha}; replaying {built} without force",
        flush=True,
    )
    replayed = _replay(repo, built, remote_sha, remote)
    if replayed == remote_sha:
        print(f"{branch} already has the planned release version", flush=True)
        return remote_sha
    print(f"fast-forward {branch} to replayed {replayed}", flush=True)
    _push(repo, remote, branch, replayed)
    return replayed


def advance_release_main(
    repo: Path,
    *,
    built_sha: str,
    remote: str = "origin",
    branch: str = "main",
    retries: int = 3,
) -> str:
    """Return the commit ``branch`` points at after a non-force update."""

    if not SHA_RE.fullmatch(built_sha):
        raise SystemExit(f"built sha is not 40 hex digits: {built_sha}")
    if retries < 1:
        raise SystemExit("retries must be positive")
    built = built_sha.lower()
    last = ""
    for attempt in range(1, retries + 1):
        try:
            return _advance_once(Path(repo), built, remote, branch)
        except _Rejected as exc:
            last = str(exc)
            print(
                f"push rejected ({attempt}/{retries}); retrying without force",
                flush=True,
            )
            print(last, flush=True)
    raise SystemExit(f"could not update {branch} without a force-push: {last}")


def _expect(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"self-test failed: {message}")


def _cargo(version: str, extra: str = "") -> str:
    return (
        "[workspace]\n"
        "members = []\n"
        "\n"
        "[workspace.package]\n"
        f'version = "{version}"\n'
        'edition = "2021"\n'
        f"{extra}"
    )


def _lock(version: str, extra: str = "") -> str:
    parts = ["# fixture\n", "version = 3\n", "\n"]
    for name in WORKSPACE_PACKAGES:
        parts.append(f'[[package]]\nname = "{name}"\nversion = "{version}"\n\n')
    parts.append('[[package]]\nname = "serde"\nversion = "1.0.0"\n\n')
    parts.append(extra)
    return "".join(parts)


def _changelog(unreleased: str) -> str:
    body = unreleased.strip()
    block = f"\n{body}\n\n" if body else "\n"
    return (
        "# Changelog\n"
        "\n"
        "intro\n"
        "\n"
        "## [Unreleased]\n"
        f"{block}"
        "## [0.9.0] - 2026-10-05\n"
        "\n"
        "- initial\n"
        "\n"
        f"[Unreleased]: {REPO_URL}/compare/v0.9.0...HEAD\n"
        f"[0.9.0]: {REPO_URL}/releases/tag/v0.9.0\n"
    )


def _expect_merge() -> None:
    built, _notes = rewrite_changelog(
        _changelog("### Changed\n\n- base note\n"),
        "0.9.1",
        "2026-10-05",
    )
    main = _changelog("### Changed\n\n- base note\n- later note\n")
    merged = merge_changelog(main, built, "0.9.1")
    unreleased = section_body_unreleased(merged)
    released = section_body(merged, "0.9.1")
    _expect("later note" in unreleased, unreleased)
    _expect("base note" not in unreleased, unreleased)
    _expect("base note" in released, released)
    _expect("later note" not in released, released)
    _expect(f"[0.9.1]: {REPO_URL}/releases/tag/v0.9.1" in merged, merged)
    _expect("v0.9.1...HEAD" in merged, merged)
    _expect("## [0.9.0]" in merged, merged)
    _expect("<<<<<<<" not in merged, merged)


def _expect_resolve() -> None:
    cargo = _cargo("0.9.0", extra='description = "kept"\n')
    lock = _lock("0.9.0", extra='[[package]]\nname = "tokio"\nversion = "1.0.0"\n\n')
    main_log = _changelog("### Changed\n\n- base note\n- later note\n")
    built_log, _notes = rewrite_changelog(
        _changelog("### Changed\n\n- base note\n"),
        "0.9.1",
        "2026-10-05",
    )
    resolved = resolve_release_trees(cargo, lock, main_log, built_log, "0.9.1")
    _expect(workspace_version(resolved["Cargo.toml"]) == "0.9.1", resolved["Cargo.toml"])
    _expect('description = "kept"' in resolved["Cargo.toml"], resolved["Cargo.toml"])
    _expect('name = "tokio"' in resolved["Cargo.lock"], resolved["Cargo.lock"])
    _expect(
        'name = "mycode-core"\nversion = "0.9.1"\n' in resolved["Cargo.lock"],
        resolved["Cargo.lock"],
    )
    _expect(
        'name = "serde"\nversion = "1.0.0"\n' in resolved["Cargo.lock"],
        "serde was rewritten",
    )
    unreleased = section_body_unreleased(resolved["CHANGELOG.md"])
    _expect("later note" in unreleased, unreleased)

    newer = resolve_release_trees(
        _cargo("0.9.2"),
        _lock("0.9.2"),
        _changelog(""),
        built_log,
        "0.9.1",
    )
    _expect(workspace_version(newer["Cargo.toml"]) == "0.9.2", newer["Cargo.toml"])
    _expect(
        'name = "mycode-core"\nversion = "0.9.2"\n' in newer["Cargo.lock"],
        newer["Cargo.lock"],
    )
    _expect("## [0.9.1]" in newer["CHANGELOG.md"], newer["CHANGELOG.md"])
    _expect(_is_non_ff("! [rejected] HEAD -> main (fetch first)"), "fetch first")
    _expect(_is_non_ff("non-fast-forward"), "non-ff")
    _expect(not _is_non_ff("protected branch hook declined"), "protected branch")


def _configure(repo: Path) -> None:
    _git(repo, "config", "user.name", "Release Test")
    _git(repo, "config", "user.email", "release-test@example.com")
    _git(repo, "config", "commit.gpgsign", "false")
    _git(repo, "config", "protocol.file.allow", "always")
    _git(repo, "config", "core.autocrlf", "false")


def _make_repo(tmp: Path) -> tuple[Path, Path]:
    bare = tmp / "origin.git"
    work = tmp / "work"
    _git(tmp, "init", "--bare", "-b", "main", str(bare))
    _git(tmp, "init", "-b", "main", str(work))
    _configure(work)
    _git(work, "remote", "add", "origin", str(bare))
    return bare, work


def _write_tree(work: Path, unreleased: str, *, cargo_extra: str = "", lock_extra: str = "") -> None:
    (work / "Cargo.toml").write_text(_cargo("0.9.0", cargo_extra), encoding="utf-8")
    (work / "Cargo.lock").write_text(_lock("0.9.0", lock_extra), encoding="utf-8")
    (work / "CHANGELOG.md").write_text(_changelog(unreleased), encoding="utf-8")


def _commit(work: Path, *messages: str) -> str:
    _git(work, "add", "-A")
    _git(work, "commit", *[arg for message in messages for arg in ("-m", message)])
    return _rev(work, "HEAD")


def _tip(bare: Path) -> str:
    return _rev(bare, "main")


def _show(bare: Path, spec: str) -> str:
    return _git(bare, "show", spec).stdout


def _seed_base(work: Path) -> str:
    _write_tree(work, "### Changed\n\n- base note\n")
    base = _commit(work, "base")
    _git(work, "push", "origin", "main")
    return base


def _bump(work: Path, *extra: tuple[str, str]) -> str:
    _git(work, "checkout", "-B", "release-tip", "main")
    apply_bump(work, "0.9.1", "2026-10-05")
    for name, content in extra:
        (work / name).write_text(content, encoding="utf-8")
    return _commit(
        work,
        "chore(release): bump to 0.9.1",
        "Record the version published for this qualified main push.",
    )


def _expect_fast_forward() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        built = _bump(work)
        advance_release_main(work, built_sha=built)
        _expect(_tip(bare) == built, f"main {_tip(bare)} did not fast-forward to {built}")


def _expect_already_contained() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        base = _seed_base(work)
        _git(work, "commit", "--allow-empty", "-m", "later")
        _git(work, "push", "origin", "main")
        before = _tip(bare)
        advance_release_main(work, built_sha=base)
        _expect(_tip(bare) == before, "ancestor commit rewound main")


def _expect_replay() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        built = _bump(work, ("FROM_BUMP.txt", "from bump\n"))
        _git(work, "checkout", "main")
        changelog = (work / "CHANGELOG.md").read_text(encoding="utf-8")
        changelog = changelog.replace("- base note\n", "- base note\n- later note\n", 1)
        (work / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
        cargo = (work / "Cargo.toml").read_text(encoding="utf-8")
        cargo = cargo.replace('edition = "2021"\n', 'edition = "2021"\ndescription = "kept"\n', 1)
        (work / "Cargo.toml").write_text(cargo, encoding="utf-8")
        lock = (work / "Cargo.lock").read_text(encoding="utf-8")
        lock += '[[package]]\nname = "tokio"\nversion = "1.0.0"\n\n'
        (work / "Cargo.lock").write_text(lock, encoding="utf-8")
        (work / "FEATURE.txt").write_text("landed on main\n", encoding="utf-8")
        feature = _commit(work, "feature landed during the build")
        _git(work, "push", "origin", "main")
        advance_release_main(work, built_sha=built)
        tip = _tip(bare)
        _expect(tip != built, "replay reused the built commit and rewound later work")
        _expect(_parent_sha(bare, tip) == feature, "replay was not based on current main")
        _expect(_is_ancestor(work, feature, tip), "feature commit was lost")
        _expect(not _is_ancestor(work, built, tip), "built commit was force-merged")
        cargo_out = _show(bare, "main:Cargo.toml")
        lock_out = _show(bare, "main:Cargo.lock")
        log_out = _show(bare, "main:CHANGELOG.md")
        _expect(workspace_version(cargo_out) == "0.9.1", cargo_out)
        _expect('description = "kept"' in cargo_out, cargo_out)
        _expect('name = "tokio"' in lock_out, lock_out)
        _expect('name = "mycode-core"\nversion = "0.9.1"\n' in lock_out, lock_out)
        _expect('name = "serde"\nversion = "1.0.0"\n' in lock_out, lock_out)
        unreleased = section_body_unreleased(log_out)
        released = section_body(log_out, "0.9.1")
        _expect("later note" in unreleased, unreleased)
        _expect("base note" not in unreleased, unreleased)
        _expect("base note" in released, released)
        _expect(_show(bare, "main:FEATURE.txt") == "landed on main\n", "feature file dropped")
        _expect(_show(bare, "main:FROM_BUMP.txt") == "from bump\n", "bump file dropped")
        message = _git(bare, "log", "-1", "--format=%B", "main").stdout
        _expect(message.startswith("chore(release): bump to 0.9.1\n"), message)
        _expect("Record the version published" in message, message)
        _expect("<<<<<<<" not in log_out, log_out)


def _expect_unrelated_replay_refused() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        _git(work, "checkout", "-B", "side")
        (work / "SIDE.txt").write_text("side\n", encoding="utf-8")
        side = _commit(work, "not a release bump")
        _git(work, "checkout", "main")
        (work / "MAIN.txt").write_text("main\n", encoding="utf-8")
        _commit(work, "main moved")
        _git(work, "push", "origin", "main")
        before = _tip(bare)
        try:
            advance_release_main(work, built_sha=side)
        except SystemExit as exc:
            _expect("refusing to replay" in str(exc), str(exc))
        else:
            raise SystemExit("self-test failed: unrelated commit was replayed")
        _expect(_tip(bare) == before, "refused replay still moved main")


def _expect_other_file_conflict_refused() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        built = _bump(work, ("SHARED.txt", "from bump\n"))
        _git(work, "checkout", "main")
        (work / "SHARED.txt").write_text("from main\n", encoding="utf-8")
        _commit(work, "main edited the same extra file")
        _git(work, "push", "origin", "main")
        before = _tip(bare)
        try:
            advance_release_main(work, built_sha=built)
        except SystemExit as exc:
            _expect("do not apply" in str(exc), str(exc))
        else:
            raise SystemExit("self-test failed: conflicting extra file was pushed")
        _expect(_tip(bare) == before, "failed replay still moved main")


def _hook(bare: Path, script: str) -> None:
    path = bare / "hooks" / "pre-receive"
    path.write_text(script, encoding="utf-8")
    path.chmod(0o755)


def _expect_retry_then_fast_forward() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        built = _bump(work)
        _hook(
            bare,
            "#!/bin/sh\n"
            'marker="$GIT_DIR/reject-once"\n'
            'if [ ! -f "$marker" ]; then\n'
            '  touch "$marker"\n'
            '  echo "! [rejected] main (fetch first)" >&2\n'
            "  exit 1\n"
            "fi\n"
            "exit 0\n",
        )
        advance_release_main(work, built_sha=built, retries=3)
        _expect(_tip(bare) == built, "retry did not fast-forward main")
        _expect((bare / "reject-once").is_file(), "rejection hook did not run")


def _expect_protected_branch_is_not_retried() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        built = _bump(work)
        _hook(
            bare,
            "#!/bin/sh\n"
            'count="$GIT_DIR/push-count"\n'
            "n=0\n"
            'if [ -f "$count" ]; then n=$(cat "$count"); fi\n'
            "n=$((n + 1))\n"
            'echo "$n" > "$count"\n'
            'echo "protected branch hook declined" >&2\n'
            "exit 1\n",
        )
        try:
            advance_release_main(work, built_sha=built, retries=3)
        except SystemExit as exc:
            _expect("protected branch" in str(exc), str(exc))
        else:
            raise SystemExit("self-test failed: protected branch push succeeded")
        _expect((bare / "push-count").read_text(encoding="utf-8").strip() == "1", "retried a protected push")


def _expect_shallow_replay() -> None:
    """Depth-1 checkout of the built commit, with main moved during the build."""

    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        built = _bump(work)
        _git(work, "push", "origin", f"{built}:refs/heads/ci/release-0.9.1-1")
        _git(work, "checkout", "main")
        changelog = (work / "CHANGELOG.md").read_text(encoding="utf-8")
        changelog = changelog.replace("- base note\n", "- base note\n- later note\n", 1)
        (work / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
        (work / "FEATURE.txt").write_text("landed on main\n", encoding="utf-8")
        feature = _commit(work, "feature landed during the build")
        _git(work, "push", "origin", "main")
        shallow = tmp / "shallow"
        _git(
            tmp,
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--depth",
            "1",
            "--branch",
            "ci/release-0.9.1-1",
            str(bare),
            str(shallow),
        )
        _configure(shallow)
        advance_release_main(shallow, built_sha=built)
        tip = _tip(bare)
        _expect(tip != built, "shallow replay rewound main to the built commit")
        _expect(_parent_sha(bare, tip) == feature, "shallow replay was not based on current main")
        log_out = _show(bare, "main:CHANGELOG.md")
        _expect("later note" in section_body_unreleased(log_out), log_out)
        _expect("base note" in section_body(log_out, "0.9.1"), log_out)
        _expect(workspace_version(_show(bare, "main:Cargo.toml")) == "0.9.1", "version was not landed")
        _expect(_show(bare, "main:FEATURE.txt") == "landed on main\n", "feature file dropped")


def _expect_shallow_fast_forward() -> None:
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        bare, work = _make_repo(tmp)
        _seed_base(work)
        built = _bump(work)
        _git(work, "push", "origin", f"{built}:refs/heads/ci/release-0.9.1-1")
        shallow = tmp / "shallow"
        _git(
            tmp,
            "-c",
            "protocol.file.allow=always",
            "clone",
            "--depth",
            "1",
            "--branch",
            "ci/release-0.9.1-1",
            str(bare),
            str(shallow),
        )
        _configure(shallow)
        advance_release_main(shallow, built_sha=built)
        _expect(_tip(bare) == built, "shallow clone did not fast-forward main")


def _expect_script_source() -> None:
    source = Path(__file__).read_text(encoding="utf-8")
    _expect("refs/heads/{branch}" in source, "push refspec missing")


def self_test() -> None:
    _expect_merge()
    _expect_resolve()
    _expect_script_source()
    _expect_fast_forward()
    _expect_already_contained()
    _expect_replay()
    _expect_unrelated_replay_refused()
    _expect_other_file_conflict_refused()
    _expect_retry_then_fast_forward()
    _expect_protected_branch_is_not_retried()
    _expect_shallow_replay()
    _expect_shallow_fast_forward()
    print("advance-main self-test passed")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--branch", default="main")
    parser.add_argument("--built-sha", default="")
    parser.add_argument("--retries", type=int, default=3)
    args = parser.parse_args(argv)
    if args.self_test:
        self_test()
        return 0
    if not args.built_sha:
        raise SystemExit("missing --built-sha")
    final = advance_release_main(
        Path(args.repo),
        built_sha=args.built_sha,
        remote=args.remote,
        branch=args.branch,
        retries=args.retries,
    )
    print(f"main is {final}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
