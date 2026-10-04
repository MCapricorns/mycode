#!/usr/bin/env python3
"""Choose a new GitHub Release tag for one qualified push to main.

A qualified push is a push to main whose fmt and clippy gates already
passed. Pull requests never call this planner. The tag is ``v<workspace
version>`` when that tag is free. When the tag already exists, the patch
component is incremented until the tag is free, and Cargo.toml,
Cargo.lock, and CHANGELOG.md are rewritten so the tagged sources match
the binaries. Archives already uploaded for an older tag are not an
input and never suppress the release.
"""

from __future__ import annotations

import argparse
import os
import re
import secrets
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_URL = "https://github.com/MCapricorns/mycode"
ROOT = Path(__file__).resolve().parents[2]
WORKSPACE_PACKAGES = (
    "mycode-agent",
    "mycode-app",
    "mycode-config",
    "mycode-core",
    "mycode-desktop",
    "mycode-providers",
    "mycode-tools",
)
# Used only when a patch bump has nowhere else to take notes from.
# Commit history is intentionally not rendered into the release body.
DEFAULT_NOTES = "### Changed\n\n- 质量门通过的 `main` 推送自动发布。\n"
STABLE_RE = re.compile(r"(\d+)\.(\d+)\.(\d+)")
DATE_RE = re.compile(r"\d{4}-\d{2}-\d{2}")
SHA_RE = re.compile(r"[0-9a-fA-F]{40}")
BUMP_SUBJECT = "chore(release): bump to {version}"


@dataclass(frozen=True)
class ReleaseChoice:
    """What this push should publish."""

    publish: bool
    version: str
    tag: str
    bump: bool
    reason: str


def parse_stable(version: str) -> tuple[int, int, int] | None:
    match = STABLE_RE.fullmatch(version)
    if not match:
        return None
    return tuple(int(part) for part in match.groups())


def format_version(parts: tuple[int, int, int]) -> str:
    return f"{parts[0]}.{parts[1]}.{parts[2]}"


def bump_patch(parts: tuple[int, int, int]) -> tuple[int, int, int]:
    return (parts[0], parts[1], parts[2] + 1)


def parse_ls_remote(text: str) -> dict[str, str]:
    """Map tag names to commit SHAs. Peeled annotated tags win."""

    commits: dict[str, str] = {}
    objects: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        sha, ref = line.split()
        if not ref.startswith("refs/tags/"):
            continue
        name = ref.removeprefix("refs/tags/")
        if name.endswith("^{}"):
            commits[name[:-3]] = sha.lower()
        else:
            objects[name] = sha.lower()
    for name, sha in objects.items():
        commits.setdefault(name, sha)
    return commits


def merge_tag_names(
    commits: dict[str, str], names: list[str]
) -> dict[str, str | None]:
    """Union git tags with release tag names that may lack a commit SHA."""

    merged: dict[str, str | None] = dict(commits)
    for name in names:
        cleaned = name.strip()
        if cleaned:
            merged.setdefault(cleaned, None)
    return merged


def choose_release(
    workspace: str,
    tag_commits: dict[str, str | None],
    head_subject: str,
    head_sha: str,
) -> ReleaseChoice:
    """Pick a tag this push can create.

    Asset file names are deliberately absent. A complete upload for the
    current workspace version still yields a new tag.
    """

    current = parse_stable(workspace)
    if current is None:
        raise SystemExit(f"workspace version is not MAJOR.MINOR.PATCH: {workspace}")
    head = head_sha.lower()
    stable: dict[tuple[int, int, int], str | None] = {}
    for name, sha in tag_commits.items():
        if not name.startswith("v"):
            continue
        parsed = parse_stable(name[1:])
        if parsed is None:
            continue
        # Keep a known commit when the same tag appears twice.
        if parsed not in stable or stable[parsed] is None:
            stable[parsed] = sha.lower() if sha else None

    prepared = head_subject == BUMP_SUBJECT.format(version=workspace)
    higher = any(parsed > current for parsed in stable)
    existing = stable.get(current)
    if prepared and not higher:
        tag = f"v{workspace}"
        if existing is not None and existing == head:
            # This commit is already the tagged bump. Another patch would
            # loop if the bump push itself starts a workflow.
            return ReleaseChoice(
                False, workspace, tag, False, "already tagged release bump"
            )
        if existing is None:
            return ReleaseChoice(True, workspace, tag, False, "publish prepared bump")

    candidate = current
    for parsed in stable:
        if parsed > candidate:
            candidate = parsed
    if candidate in stable:
        hops = 0
        while candidate in stable:
            candidate = bump_patch(candidate)
            hops += 1
            if hops > 10000:
                raise SystemExit("no free patch version above existing tags")
    version = format_version(candidate)
    return ReleaseChoice(
        True,
        version,
        f"v{version}",
        version != workspace,
        "new tag",
    )


def workspace_version(cargo_toml: str) -> str:
    match = re.search(r'(?ms)^\[workspace\.package\]\n.*?(?=^\[|\Z)', cargo_toml)
    if not match:
        raise SystemExit("Cargo.toml has no [workspace.package] section")
    version = re.search(r'(?m)^version = "([^"]+)"', match.group(0))
    if not version:
        raise SystemExit("Cargo.toml has no workspace version")
    return version.group(1)


def rewrite_workspace_version(cargo_toml: str, version: str) -> str:
    match = re.search(r'(?ms)^\[workspace\.package\]\n.*?(?=^\[|\Z)', cargo_toml)
    if not match:
        raise SystemExit("Cargo.toml has no [workspace.package] section")
    section, count = re.subn(
        r'(?m)^version = "[^"]+"',
        f'version = "{version}"',
        match.group(0),
        count=1,
    )
    if count != 1:
        raise SystemExit("Cargo.toml workspace version was not rewritten")
    return cargo_toml[: match.start()] + section + cargo_toml[match.end() :]


def rewrite_lock(lock_text: str, version: str) -> str:
    parts = lock_text.split("[[package]]")
    found: set[str] = set()
    rewritten = [parts[0]]
    for block in parts[1:]:
        name_match = re.search(r'(?m)^name = "([^"]+)"\n', block)
        name = name_match.group(1) if name_match else ""
        if name in WORKSPACE_PACKAGES:
            block, count = re.subn(
                r'(?m)^version = "[^"]+"',
                f'version = "{version}"',
                block,
                count=1,
            )
            if count != 1:
                raise SystemExit(f"Cargo.lock has no version for {name}")
            found.add(name)
        rewritten.append(block)
    missing = set(WORKSPACE_PACKAGES) - found
    if missing:
        raise SystemExit(f"Cargo.lock is missing workspace packages: {sorted(missing)}")
    return "[[package]]".join(rewritten)


def _next_boundary(text: str, start: int) -> int:
    match = re.search(r"(?m)^(## |\[[^\]]+\]: )", text[start:])
    if not match:
        return len(text)
    return start + match.start()


def section_body(text: str, version: str) -> str:
    match = re.search(rf"(?m)^## \[{re.escape(version)}\][^\n]*\n", text)
    if not match:
        return ""
    end = _next_boundary(text, match.end())
    return text[match.end() : end].strip()


def _unreleased_span(text: str) -> tuple[int, int, int]:
    match = re.search(r"(?m)^## \[Unreleased\][^\n]*\n", text)
    if not match:
        raise SystemExit("CHANGELOG.md is missing ## [Unreleased]")
    body_end = _next_boundary(text, match.end())
    return match.start(), match.end(), body_end


def rewrite_changelog(text: str, version: str, date: str) -> tuple[str, str]:
    """Insert ``version`` and return ``(changelog, release notes)``."""

    if not DATE_RE.fullmatch(date):
        raise SystemExit(f"release date is not YYYY-MM-DD: {date}")
    if not text.endswith("\n"):
        text += "\n"
    existing = section_body(text, version)
    _, unreleased_body_start, unreleased_body_end = _unreleased_span(text)
    promoted = text[unreleased_body_start:unreleased_body_end].strip()
    if existing:
        notes = existing
        updated = text
    else:
        notes = promoted if promoted else DEFAULT_NOTES.strip()
        block = notes.strip() + "\n"
        heading = re.search(rf"(?m)^## \[{re.escape(version)}\][^\n]*\n", text)
        if heading:
            body_end = _next_boundary(text, heading.end())
            updated = text[: heading.end()] + "\n" + block + "\n" + text[body_end:]
            if promoted:
                _, body_start, promoted_end = _unreleased_span(updated)
                updated = updated[:body_start] + "\n" + updated[promoted_end:]
        else:
            updated = text
            if promoted:
                _, body_start, body_end = _unreleased_span(updated)
                updated = updated[:body_start] + "\n" + updated[body_end:]
            _, body_start, insert_at = _unreleased_span(updated)
            del body_start
            section = f"## [{version}] - {date}\n\n{block}\n"
            updated = updated[:insert_at] + section + updated[insert_at:]
    updated = _upsert_links(updated, version)
    final = section_body(updated, version)
    if not final:
        raise SystemExit(f"CHANGELOG.md has no notes for {version}")
    return updated, final


def _upsert_links(text: str, version: str) -> str:
    unreleased_line = f"[Unreleased]: {REPO_URL}/compare/v{version}...HEAD"
    version_line = f"[{version}]: {REPO_URL}/releases/tag/v{version}"
    lines = text.splitlines()
    footer_at = None
    for index, line in enumerate(lines):
        if re.fullmatch(r"\[[^\]]+\]: https://\S+", line):
            footer_at = index
            break
    if footer_at is None:
        body = "\n".join(lines).rstrip() + "\n"
        return f"{body}\n{unreleased_line}\n{version_line}\n"
    body = lines[:footer_at]
    footer = [
        line for line in lines[footer_at:] if not line.startswith(f"[{version}]:")
    ]
    rewritten: list[str] = []
    inserted = False
    replaced = False
    for line in footer:
        if line.startswith("[Unreleased]:"):
            rewritten.append(unreleased_line)
            replaced = True
            if version_line not in rewritten:
                rewritten.append(version_line)
                inserted = True
            continue
        rewritten.append(line)
    if not replaced:
        rewritten.insert(0, unreleased_line)
    if not inserted and version_line not in rewritten:
        insert_at = 1 if rewritten and rewritten[0].startswith("[Unreleased]:") else 0
        rewritten.insert(insert_at, version_line)
    trailing = "\n".join(body).rstrip() + "\n\n" + "\n".join(rewritten) + "\n"
    return trailing


def apply_bump(root: Path, version: str, date: str) -> str:
    cargo_path = root / "Cargo.toml"
    lock_path = root / "Cargo.lock"
    changelog_path = root / "CHANGELOG.md"
    cargo = cargo_path.read_text(encoding="utf-8")
    lock = lock_path.read_text(encoding="utf-8")
    changelog = changelog_path.read_text(encoding="utf-8")
    cargo_path.write_text(rewrite_workspace_version(cargo, version), encoding="utf-8")
    lock_path.write_text(rewrite_lock(lock, version), encoding="utf-8")
    updated, notes = rewrite_changelog(changelog, version, date)
    changelog_path.write_text(updated, encoding="utf-8")
    return notes


def release_notes_for_existing(root: Path, version: str) -> str:
    notes = section_body(
        (root / "CHANGELOG.md").read_text(encoding="utf-8"),
        version,
    )
    if not notes:
        raise SystemExit(f"CHANGELOG.md has no notes for {version}")
    return notes


def write_github_output(
    path: Path,
    *,
    publish: str,
    tag: str,
    sha: str,
    cleanup_ref: str,
    notes: str,
) -> None:
    """Write step outputs only after the version commit push has succeeded."""

    if publish not in {"true", "false"}:
        raise SystemExit(f"refusing unexpected publish flag {publish}")
    if not tag.startswith("v") or parse_stable(tag[1:]) is None:
        raise SystemExit(f"refusing unexpected tag {tag}")
    if not SHA_RE.fullmatch(sha):
        raise SystemExit(f"refusing unexpected sha {sha}")
    if cleanup_ref and not re.fullmatch(r"ci/release-\d+\.\d+\.\d+-\d+", cleanup_ref):
        raise SystemExit(f"refusing unexpected cleanup ref {cleanup_ref}")
    with path.open("a", encoding="utf-8") as handle:
        handle.write(f"publish={publish}\n")
        handle.write(f"tag={tag}\n")
        handle.write(f"sha={sha}\n")
        handle.write(f"cleanup_ref={cleanup_ref}\n")
        if publish != "true":
            return
        if not notes.strip():
            raise SystemExit("release notes are empty")
        delimiter = "NOTES_" + _token()
        handle.write(f"notes<<{delimiter}\n")
        handle.write(notes)
        if not notes.endswith("\n"):
            handle.write("\n")
        handle.write(f"{delimiter}\n")


def write_plan_env(path: Path, choice: ReleaseChoice) -> None:
    if parse_stable(choice.version) is None:
        raise SystemExit(f"refusing unexpected version {choice.version}")
    if choice.tag != f"v{choice.version}":
        raise SystemExit(f"refusing unexpected tag {choice.tag}")
    path.write_text(
        "\n".join(
            [
                f"PUBLISH={'true' if choice.publish else 'false'}",
                f"BUMPED={'true' if choice.bump else 'false'}",
                f"VERSION={choice.version}",
                f"TAG={choice.tag}",
                "",
            ]
        ),
        encoding="utf-8",
    )


def _token() -> str:
    return secrets.token_hex(8)


def _read_lines(path: Path | None) -> list[str]:
    if path is None:
        return []
    return path.read_text(encoding="utf-8").splitlines()


def plan_from_args(args: argparse.Namespace) -> tuple[ReleaseChoice, str]:
    if not SHA_RE.fullmatch(args.head_sha):
        raise SystemExit(f"head sha is not 40 hex digits: {args.head_sha}")
    if not DATE_RE.fullmatch(args.date):
        raise SystemExit(f"release date is not YYYY-MM-DD: {args.date}")
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    workspace = workspace_version(cargo)
    tags = merge_tag_names(
        parse_ls_remote(Path(args.remote_tags).read_text(encoding="utf-8")),
        _read_lines(Path(args.release_tags) if args.release_tags else None),
    )
    choice = choose_release(workspace, tags, args.head_subject, args.head_sha)
    print(
        f"{choice.reason}: publish={choice.publish} tag={choice.tag} bump={choice.bump}",
        flush=True,
    )
    if not choice.publish:
        return choice, ""
    if choice.bump:
        if not args.apply:
            raise SystemExit("refusing to bump without --apply")
        notes = apply_bump(ROOT, choice.version, args.date)
    else:
        notes = release_notes_for_existing(ROOT, choice.version)
    return choice, notes


def self_test() -> None:
    _expect_choice()
    _expect_changelog()
    _expect_manifests()
    _expect_remote_tags()
    _expect_output_writer()
    _expect_workflow_contract()
    print("release-plan self-test passed")


def _expect(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"self-test failed: {message}")


def _expect_choice() -> None:
    tags = {
        "v0.7.1": "a" * 40,
        "v0.7.2": "b" * 40,
    }
    choice = choose_release("0.7.2", tags, "ship linux", "c" * 40)
    _expect(choice.publish and choice.bump and choice.version == "0.7.3", str(choice))
    _expect(choice.tag == "v0.7.3" and choice.reason == "new tag", str(choice))

    occupied = dict(tags)
    occupied["v0.7.3"] = "d" * 40
    jumped = choose_release("0.7.2", occupied, "again", "e" * 40)
    _expect(jumped.version == "0.7.4" and jumped.bump, str(jumped))

    above_hole = dict(occupied)
    above_hole["v0.7.10"] = "9" * 40
    patched = choose_release("0.7.2", above_hole, "later", "e" * 40)
    _expect(patched.version == "0.7.11", str(patched))

    fresh = choose_release("0.8.0", tags, "minor", "f" * 40)
    _expect(fresh.publish and not fresh.bump and fresh.version == "0.8.0", str(fresh))

    first = choose_release("0.7.2", {}, "initial", "a" * 40)
    _expect(first.publish and not first.bump and first.tag == "v0.7.2", str(first))

    # A release name with no git sha still occupies the tag.
    named = {"v0.7.2": None}
    from_release = choose_release("0.7.2", named, "push", "a" * 40)
    _expect(from_release.version == "0.7.3", str(from_release))

    prepared = choose_release(
        "0.7.3",
        {},
        BUMP_SUBJECT.format(version="0.7.3"),
        "a" * 40,
    )
    _expect(prepared.publish and not prepared.bump and prepared.version == "0.7.3", str(prepared))

    already = choose_release(
        "0.7.3",
        {"v0.7.3": "a" * 40},
        BUMP_SUBJECT.format(version="0.7.3"),
        "A" * 40,
    )
    _expect(not already.publish and already.tag == "v0.7.3", str(already))

    elsewhere = choose_release(
        "0.7.3",
        {"v0.7.3": "b" * 40},
        BUMP_SUBJECT.format(version="0.7.3"),
        "a" * 40,
    )
    _expect(elsewhere.publish and elsewhere.version == "0.7.4", str(elsewhere))

    # v0.7.3 is published. The next qualified push of that workspace version
    # must take the following free patch, not reuse v0.7.3.
    published = {
        "v0.7.1": "a" * 40,
        "v0.7.2": "b" * 40,
        "v0.7.3": "c" * 40,
    }
    after_073 = choose_release("0.7.3", published, "qualified main push", "d" * 40)
    _expect(
        after_073.publish and after_073.bump and after_073.version == "0.7.4",
        str(after_073),
    )
    _expect(after_073.tag == "v0.7.4", str(after_073))

    try:
        choose_release("0.7.2-rc.1", {}, "bad", "a" * 40)
    except SystemExit as exc:
        _expect("MAJOR.MINOR.PATCH" in str(exc), str(exc))
    else:
        raise SystemExit("self-test failed: pre-release version was accepted")


def _expect_changelog() -> None:
    original = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    sample = original.replace(
        "## [Unreleased]\n",
        "## [Unreleased]\n\n### Changed\n\n- planner promotes this section\n",
        1,
    )
    updated, notes = rewrite_changelog(sample, "0.7.4", "2026-10-04")
    _expect("planner promotes this section" in notes, notes)
    _expect("## [0.7.4] - 2026-10-04" in updated, "missing heading")
    _expect("## [0.7.3] - 2026-10-04" in updated, "dropped 0.7.3")
    _expect("Windows ARM64" in updated, "dropped 0.7.3 notes")
    _expect("aarch64-pc-windows-msvc" in updated, "dropped ARM64 archive name")
    _expect(
        "[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.7.4...HEAD" in updated,
        "compare link",
    )
    _expect(
        "[0.7.4]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.4" in updated,
        "version link",
    )
    _expect(
        "[0.7.3]: https://github.com/MCapricorns/mycode/releases/tag/v0.7.3" in updated,
        "old link dropped",
    )
    unreleased_body = section_body_unreleased(updated)
    _expect("planner promotes" not in unreleased_body, unreleased_body)

    emptied = re.sub(
        r"(?ms)^## \[Unreleased\][^\n]*\n.*?(?=^## )",
        "## [Unreleased]\n\n",
        original,
        count=1,
    )
    empty_notes = rewrite_changelog(emptied, "0.7.4", "2026-10-04")[1]
    _expect("自动发布" in empty_notes, empty_notes)
    live_notes = rewrite_changelog(original, "0.7.4", "2026-10-04")[1]
    _expect("补丁号加一" in live_notes, live_notes)
    _expect("自动发布" not in live_notes, live_notes)
    again, again_notes = rewrite_changelog(updated, "0.7.4", "2026-10-05")
    _expect(again_notes == notes, "existing section was rewritten")
    _expect(again.count("## [0.7.4]") == 1, "duplicate heading")

    kept = original.replace(
        "## [Unreleased]\n",
        "## [Unreleased]\n\n### Changed\n\n- leave me here\n",
        1,
    )
    # Pretend the human already wrote the target section.
    kept = kept.replace(
        "## [0.7.3] - 2026-10-04\n",
        "## [0.7.4] - 2026-10-04\n\n### Added\n\n- hand written\n\n## [0.7.3] - 2026-10-04\n",
        1,
    )
    preserved, preserved_notes = rewrite_changelog(kept, "0.7.4", "2026-10-04")
    _expect(preserved_notes == "### Added\n\n- hand written", preserved_notes)
    _expect("leave me here" in preserved, "unreleased notes were consumed")
    _expect("Windows ARM64" in preserved, "ARM64 notes were dropped")
    _expect(
        "[Unreleased]: https://github.com/MCapricorns/mycode/compare/v0.7.4...HEAD"
        in preserved,
        "compare link was not moved",
    )


def section_body_unreleased(text: str) -> str:
    _, start, end = _unreleased_span(text)
    return text[start:end].strip()


def _expect_manifests() -> None:
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    bumped = rewrite_workspace_version(cargo, "0.7.9")
    _expect(workspace_version(bumped) == "0.7.9", bumped)
    _expect('jsonschema = { version = "0.52"' in bumped, "dependency version changed")
    _expect(bumped.count('version = "0.7.9"') == 1, "extra workspace versions")

    lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
    rewritten = rewrite_lock(lock, "0.7.9")
    for name in WORKSPACE_PACKAGES:
        pattern = rf'name = "{name}"\nversion = "0.7.9"\n'
        _expect(re.search(pattern, rewritten) is not None, name)
    _expect(
        re.search(r'name = "async-broadcast"\nversion = "0.7.2"\n', rewritten) is not None,
        "unrelated 0.7.2 package was rewritten",
    )


def _expect_remote_tags() -> None:
    text = "\n".join(
        [
            f"{'a' * 40}\trefs/tags/v0.7.2",
            f"{'b' * 40}\trefs/tags/v0.7.2^{{}}",
            f"{'c' * 40}\trefs/tags/not-a-release",
        ]
    )
    parsed = parse_ls_remote(text)
    _expect(parsed["v0.7.2"] == "b" * 40, str(parsed))
    merged = merge_tag_names(parsed, ["v0.7.1", "v0.7.2"])
    _expect(merged["v0.7.1"] is None, str(merged))
    _expect(merged["v0.7.2"] == "b" * 40, str(merged))


def _expect_output_writer() -> None:
    target = Path("/tmp/mycode-release-plan-output.txt")
    if target.exists():
        target.unlink()
    write_github_output(
        target,
        publish="true",
        tag="v0.7.3",
        sha="a" * 40,
        cleanup_ref="",
        notes="### Changed\n\n- shipped\n",
    )
    text = target.read_text(encoding="utf-8")
    _expect("publish=true\n" in text, text)
    _expect(f"sha={'a' * 40}\n" in text, text)
    _expect("cleanup_ref=\n" in text, text)
    _expect("shipped" in text, text)
    _expect(text.strip().splitlines()[-1].startswith("NOTES_"), text)
    try:
        write_github_output(
            target,
            publish="true",
            tag="v0.7.3",
            sha="a" * 40,
            cleanup_ref="main",
            notes="notes\n",
        )
    except SystemExit as exc:
        _expect("cleanup ref" in str(exc), str(exc))
    else:
        raise SystemExit("self-test failed: bad cleanup ref was accepted")


def _expect_workflow_contract() -> None:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    lowered = workflow.lower()
    _expect("gh release view" not in lowered, "asset lookup still decides the plan")
    _expect("already has every" not in lowered, "skip-if-complete wording remains")
    _expect(".assets[].name" not in workflow, "asset name check remains")
    _expect("release_plan.py" in workflow, "planner is not wired into ci.yml")
    _expect(
        "github.event_name == 'push' && github.ref == 'refs/heads/main'" in workflow,
        "release is not limited to main pushes",
    )
    _expect(
        workflow.count(
            "needs.release-plan.result == 'success' && needs.release-plan.outputs.publish == 'true'"
        )
        >= 2,
        "a failed plan can still build or publish",
    )
    _expect("windows-11-arm" in workflow, "Windows ARM64 runner missing")
    _expect("native_image_launches" in workflow, "exec smoke test missing")
    for target in (
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ):
        _expect(target in workflow, f"missing platform {target}")
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    _expect("才跳过打包" not in readme, "Chinese skip rule remains")
    _expect("四个平台" in readme, "Chinese docs dropped the fourth platform")
    _expect("Windows ARM64" in readme, "Chinese docs dropped Windows ARM64")
    _expect("skips packaging" not in readme, "English skip rule remains")
    _expect("four platform" in readme, "English docs dropped the fourth platform")
    _expect("Windows ARM64" in readme, "English docs dropped Windows ARM64")
    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    _expect("还没有带齐三个平台压缩包" not in changelog, "three-platform skip rule remains")
    _expect("还没有带齐四个平台压缩包" not in changelog, "four-platform skip rule remains")
    _expect("aarch64-pc-windows-msvc" in changelog, "ARM64 archive missing from changelog")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--remote-tags")
    parser.add_argument("--release-tags")
    parser.add_argument("--head-subject", default="")
    parser.add_argument("--head-sha", default="")
    parser.add_argument("--date", default="")
    parser.add_argument("--github-output")
    parser.add_argument("--plan-env")
    parser.add_argument("--notes-file")
    parser.add_argument("--emit-output", action="store_true")
    parser.add_argument("--sha", default="")
    parser.add_argument("--cleanup-ref", default="")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.self_test:
        self_test()
        return 0
    if args.emit_output:
        emit_saved_plan(args)
        return 0
    if not args.head_subject:
        args.head_subject = os.environ.get("MYCODE_RELEASE_SUBJECT", "")
    if not args.head_sha:
        args.head_sha = os.environ.get("MYCODE_RELEASE_SHA", "")
    if not args.date:
        args.date = os.environ.get("MYCODE_RELEASE_DATE", "")
    required = ("remote_tags", "head_sha", "date", "plan_env", "notes_file")
    missing = [name for name in required if not getattr(args, name)]
    if missing:
        raise SystemExit(f"missing arguments: {', '.join(missing)}")
    choice, notes = plan_from_args(args)
    notes_path = Path(args.notes_file)
    notes_path.write_text(notes if notes.endswith("\n") or notes == "" else notes + "\n", encoding="utf-8")
    write_plan_env(Path(args.plan_env), choice)
    return 0


def emit_saved_plan(args: argparse.Namespace) -> None:
    required = ("plan_env", "notes_file", "github_output", "sha")
    missing = [name for name in required if not getattr(args, name)]
    if missing:
        raise SystemExit(f"missing arguments: {', '.join(missing)}")
    values = {}
    for line in Path(args.plan_env).read_text(encoding="utf-8").splitlines():
        key, separator, value = line.partition("=")
        if separator:
            values[key] = value
    notes = Path(args.notes_file).read_text(encoding="utf-8")
    write_github_output(
        Path(args.github_output),
        publish=values.get("PUBLISH", ""),
        tag=values.get("TAG", ""),
        sha=args.sha,
        cleanup_ref=args.cleanup_ref,
        notes=notes,
    )


if __name__ == "__main__":
    sys.exit(main())
