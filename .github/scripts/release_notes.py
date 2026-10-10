#!/usr/bin/env python3
"""Build a GitHub Release body for one new tag.

The body starts with that version's CHANGELOG section. After it, the
pull requests merged since the previous stable tag are listed with
links, then any issues those pull requests closed, then the compare
link. The changelog text is never replaced by commit subjects.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from release_plan import REPO_URL, format_version, parse_ls_remote, parse_stable

PULL_URL_RE = re.compile(r"https://github\.com/[^/\s]+/[^/\s]+/pull/(\d+)")
MERGE_PR_RE = re.compile(r"\bMerge pull request #(\d+)\b")
SQUASH_PR_RE = re.compile(r"\(#(\d+)\)")
CLOSING_RE = re.compile(
    r"\b(?:close|closes|closed|fix|fixes|fixed|resolve|resolves|resolved)\s+"
    r"(?:https://github\.com/[^/\s]+/[^/\s]+/issues/|#)(\d+)\b",
    re.IGNORECASE,
)
FULL_CHANGELOG_RE = re.compile(r"(?m)^\*\*Full Changelog\*\*: .+$")
ISSUE_URL_RE = re.compile(r"/issues/(\d+)\b")


@dataclass(frozen=True)
class PullRequest:
    number: int
    title: str
    url: str
    author: str


@dataclass(frozen=True)
class FixedIssue:
    number: int
    title: str
    url: str


def previous_stable_tag(tag_names: dict[str, str | None], version: str) -> str | None:
    """Highest stable tag strictly below ``version``."""

    current = parse_stable(version)
    if current is None:
        raise SystemExit(f"version is not MAJOR.MINOR.PATCH: {version}")
    best: tuple[int, int, int] | None = None
    for name in tag_names:
        if not name.startswith("v"):
            continue
        parsed = parse_stable(name[1:])
        if parsed is None or parsed >= current:
            continue
        if best is None or parsed > best:
            best = parsed
    if best is None:
        return None
    return f"v{format_version(best)}"


def _unique(numbers: list[int]) -> list[int]:
    seen: set[int] = set()
    ordered: list[int] = []
    for number in numbers:
        if number in seen or number < 1:
            continue
        seen.add(number)
        ordered.append(number)
    return ordered


def extract_pr_numbers(text: str) -> list[int]:
    """Pull request numbers from GitHub notes, merge commits, and squash subjects."""

    found: list[int] = []
    for pattern in (PULL_URL_RE, MERGE_PR_RE, SQUASH_PR_RE):
        found.extend(int(match.group(1)) for match in pattern.finditer(text))
    return _unique(found)


def extract_fixed_issue_numbers(text: str) -> list[int]:
    """Issue numbers closed by a GitHub closing keyword, not conventional-commit subjects."""

    return _unique(int(match.group(1)) for match in CLOSING_RE.finditer(text))


def _one_line(text: str) -> str:
    return " ".join(text.split())


def render_fixed_issues(issues: list[FixedIssue], already: str) -> str:
    lines: list[str] = []
    seen: set[int] = set()
    listed = set(int(match.group(1)) for match in ISSUE_URL_RE.finditer(already))
    for issue in issues:
        if issue.number in seen or issue.number in listed:
            continue
        if issue.url and issue.url in already:
            continue
        title = _one_line(issue.title)
        if not title or not issue.url:
            continue
        seen.add(issue.number)
        lines.append(f"* {title} in {issue.url}")
    if not lines:
        return ""
    return "## Fixed issues\n\n" + "\n".join(lines)


def full_changelog_line(previous: str | None, tag: str) -> str:
    if previous:
        return f"**Full Changelog**: {REPO_URL}/compare/{previous}...{tag}"
    return f"**Full Changelog**: {REPO_URL}/commits/{tag}"


def render_changes(
    pulls: list[PullRequest],
    subjects: list[str],
    previous: str | None,
    tag: str,
) -> str:
    """Fallback notes when the generate-notes API does not return a body."""

    lines: list[str] = []
    if pulls:
        for pull in pulls:
            title = _one_line(pull.title)
            if not title or not pull.url:
                continue
            if pull.author:
                lines.append(f"* {title} by @{pull.author} in {pull.url}")
            else:
                lines.append(f"* {title} in {pull.url}")
    else:
        for subject in subjects:
            text = _one_line(subject)
            if not text or text.startswith("Merge "):
                continue
            lines.append(f"* {text}")
    parts: list[str] = []
    if lines:
        parts.append("## What's Changed\n\n" + "\n".join(lines))
    parts.append(full_changelog_line(previous, tag))
    return "\n\n".join(parts)


def compose_release_body(
    changelog: str,
    changes: str,
    issues: list[FixedIssue],
) -> str:
    """Changelog section, then linked pull requests, then fixed issues."""

    notes = changelog.strip()
    if not notes:
        raise SystemExit("changelog section is empty")
    linked = changes.strip()
    issue_block = render_fixed_issues(issues, linked)
    if issue_block and linked:
        match = FULL_CHANGELOG_RE.search(linked)
        if match:
            linked = (
                linked[: match.start()].rstrip()
                + "\n\n"
                + issue_block
                + "\n\n"
                + linked[match.start() :]
            )
        else:
            linked = linked.rstrip() + "\n\n" + issue_block
    elif issue_block:
        linked = issue_block
    if not linked:
        return notes + "\n"
    return notes + "\n\n" + linked.strip() + "\n"


def _transient(text: str) -> bool:
    lowered = text.lower()
    markers = (
        "timed out",
        "timeout",
        "connection reset",
        "could not resolve",
        "temporarily unavailable",
        "502",
        "503",
        "504",
        "ssl",
        "network",
        "not found",
        "422",
    )
    return any(marker in lowered for marker in markers)


def _gh(args: list[str], *, stdin: str | None = None) -> str:
    delay = 2
    detail = ""
    for attempt in range(1, 4):
        result = subprocess.run(
            ["gh", *args],
            input=stdin,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )
        if result.returncode == 0:
            return result.stdout
        detail = (result.stderr or result.stdout or "").strip()
        if attempt == 3 or not _transient(detail):
            raise SystemExit(f"gh {' '.join(args[:4])} failed: {detail}")
        print(f"gh failed ({attempt}/3): {detail}", file=sys.stderr, flush=True)
        time.sleep(delay)
        delay *= 2
    raise SystemExit(detail)


def _gh_json(args: list[str], *, stdin: str | None = None) -> object:
    text = _gh(args, stdin=stdin)
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"gh {' '.join(args[:4])} returned non-JSON: {exc}") from exc


def _repository(repo: str) -> tuple[str, str]:
    owner, separator, name = repo.partition("/")
    if not separator or not owner or not name or "/" in name:
        raise SystemExit(f"repository is not owner/name: {repo}")
    return owner, name


def fetch_generated_notes(repo: str, tag: str, sha: str, previous: str | None) -> str:
    payload: dict[str, str] = {"tag_name": tag, "target_commitish": sha}
    if previous:
        payload["previous_tag_name"] = previous
    body = _gh_json(
        [
            "api",
            "--method",
            "POST",
            f"repos/{repo}/releases/generate-notes",
            "--input",
            "-",
        ],
        stdin=json.dumps(payload),
    )
    if not isinstance(body, dict):
        raise SystemExit("generate-notes response was not an object")
    notes = body.get("body")
    if not isinstance(notes, str):
        raise SystemExit("generate-notes response has no body")
    return notes


def fetch_commit_messages(repo: str, previous: str | None, sha: str) -> list[str]:
    if not previous:
        return []
    text = _gh(
        [
            "api",
            f"repos/{repo}/compare/{previous}...{sha}",
            "--jq",
            ".commits[].commit.message | @json",
        ]
    )
    messages: list[str] = []
    for line in text.splitlines():
        if not line.strip():
            continue
        value = json.loads(line)
        if isinstance(value, str):
            messages.append(value)
    return messages


def _graphql(query: str) -> dict[str, object]:
    payload = _gh_json(
        ["api", "graphql", "--input", "-"],
        stdin=json.dumps({"query": query}),
    )
    if not isinstance(payload, dict):
        raise SystemExit("graphql response was not an object")
    data = payload.get("data")
    if not isinstance(data, dict):
        errors = payload.get("errors")
        raise SystemExit(f"graphql query failed: {errors}")
    return data


def _chunk(numbers: list[int], size: int) -> list[list[int]]:
    return [numbers[index : index + size] for index in range(0, len(numbers), size)]


def fetch_pull_requests(
    repo: str, numbers: list[int]
) -> tuple[list[PullRequest], list[FixedIssue]]:
    if not numbers:
        return [], []
    owner, name = _repository(repo)
    pulls: list[PullRequest] = []
    issues: list[FixedIssue] = []
    for group in _chunk(numbers, 20):
        fields = []
        for number in group:
            fields.append(
                f"pr_{number}: pullRequest(number: {number}) {{ "
                "number title url author { login } "
                "closingIssuesReferences(first: 20) { nodes { number title url } } "
                "}"
            )
        query = (
            "query { repository(owner: "
            f"{json.dumps(owner)}, name: {json.dumps(name)}) {{ "
            + " ".join(fields)
            + " } }"
        )
        data = _graphql(query)
        repository = data.get("repository")
        if not isinstance(repository, dict):
            raise SystemExit("graphql response has no repository")
        for number in group:
            node = repository.get(f"pr_{number}")
            if not isinstance(node, dict):
                continue
            author = ""
            author_node = node.get("author")
            if isinstance(author_node, dict) and isinstance(author_node.get("login"), str):
                author = author_node["login"]
            title = node.get("title")
            url = node.get("url")
            pr_number = node.get("number")
            if not isinstance(title, str) or not isinstance(url, str) or not isinstance(pr_number, int):
                continue
            pulls.append(PullRequest(pr_number, title, url, author))
            closing = node.get("closingIssuesReferences")
            if not isinstance(closing, dict):
                continue
            nodes = closing.get("nodes")
            if not isinstance(nodes, list):
                continue
            for issue in nodes:
                parsed = _issue_node(issue)
                if parsed is not None:
                    issues.append(parsed)
    return pulls, issues


def _issue_node(node: object) -> FixedIssue | None:
    if not isinstance(node, dict):
        return None
    number = node.get("number")
    title = node.get("title")
    url = node.get("url")
    if not isinstance(number, int) or not isinstance(title, str) or not isinstance(url, str):
        return None
    return FixedIssue(number, title, url)


def fetch_issues(repo: str, numbers: list[int]) -> list[FixedIssue]:
    if not numbers:
        return []
    owner, name = _repository(repo)
    found: list[FixedIssue] = []
    for group in _chunk(numbers, 20):
        fields = [
            f"i_{number}: issue(number: {number}) {{ number title url }}"
            for number in group
        ]
        query = (
            "query { repository(owner: "
            f"{json.dumps(owner)}, name: {json.dumps(name)}) {{ "
            + " ".join(fields)
            + " } }"
        )
        data = _graphql(query)
        repository = data.get("repository")
        if not isinstance(repository, dict):
            raise SystemExit("graphql response has no repository")
        for number in group:
            parsed = _issue_node(repository.get(f"i_{number}"))
            if parsed is not None:
                found.append(parsed)
    return found


def _dedupe_issues(issues: list[FixedIssue]) -> list[FixedIssue]:
    seen: set[int] = set()
    ordered: list[FixedIssue] = []
    for issue in issues:
        if issue.number in seen:
            continue
        seen.add(issue.number)
        ordered.append(issue)
    return ordered


def linked_changes(
    repo: str,
    *,
    tag: str,
    sha: str,
    previous: str | None,
) -> tuple[str, list[FixedIssue], bool]:
    """Return ``(notes, fixed issues, whether any commit was in range)``."""

    generated = ""
    try:
        generated = fetch_generated_notes(repo, tag, sha, previous)
    except SystemExit as exc:
        print(f"generate-notes unavailable, using compare data: {exc}", file=sys.stderr)
    messages: list[str] = []
    try:
        messages = fetch_commit_messages(repo, previous, sha)
    except SystemExit as exc:
        if not generated.strip():
            raise
        print(f"compare unavailable: {exc}", file=sys.stderr)
    pr_numbers = extract_pr_numbers(generated + "\n" + "\n".join(messages))
    try:
        pulls, closing = fetch_pull_requests(repo, pr_numbers)
    except SystemExit as exc:
        if not generated.strip():
            raise
        print(f"pull request lookup failed: {exc}", file=sys.stderr)
        pulls, closing = [], []
    keyword_numbers = [
        number
        for number in extract_fixed_issue_numbers("\n".join(messages))
        if number not in {issue.number for issue in closing}
    ]
    try:
        keyword_issues = fetch_issues(repo, keyword_numbers)
    except SystemExit as exc:
        if not generated.strip() and not pulls:
            raise
        print(f"issue lookup failed: {exc}", file=sys.stderr)
        keyword_issues = []
    issues = _dedupe_issues(closing + keyword_issues)
    changes = generated.strip()
    if not changes:
        changes = render_changes(pulls, [message.splitlines()[0] if message else "" for message in messages], previous, tag)
    if not changes.strip() and messages:
        raise SystemExit("could not build the pull request list since the previous tag")
    return changes, issues, bool(messages) or bool(generated.strip())


def enrich_notes(
    changelog: str,
    repo: str,
    *,
    tag: str,
    sha: str,
    previous: str | None,
) -> str:
    changes, issues, found = linked_changes(repo, tag=tag, sha=sha, previous=previous)
    if not changes.strip() and found:
        raise SystemExit("release notes are missing changes since the previous tag")
    return compose_release_body(changelog, changes, issues)


def _expect(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"self-test failed: {message}")


def _expect_previous_tag() -> None:
    tags = {
        "v0.9.25": "a" * 40,
        "v0.9.28": "b" * 40,
        "v0.10.1": "c" * 40,
        "v0.10.2-rc.1": "d" * 40,
        "not-a-release": "e" * 40,
    }
    _expect(previous_stable_tag(tags, "0.10.2") == "v0.10.1", "previous tag")
    _expect(previous_stable_tag(tags, "0.9.26") == "v0.9.25", "hole")
    _expect(previous_stable_tag({"v1.0.0": None}, "0.9.0") is None, "only newer")


def _expect_extract() -> None:
    text = "\n".join(
        [
            "* feat by @MCapricorns in https://github.com/MCapricorns/mycode/pull/86",
            "Merge pull request #85 from example/main",
            "fix(prompt): drop a sentence (#84)",
            "fixes #12 and also Fixes https://github.com/MCapricorns/mycode/issues/13",
            "fix(shell): this subject is not an issue reference",
        ]
    )
    _expect(extract_pr_numbers(text) == [86, 85, 84], str(extract_pr_numbers(text)))
    _expect(extract_fixed_issue_numbers(text) == [12, 13], str(extract_fixed_issue_numbers(text)))
    _expect(extract_fixed_issue_numbers("fix(shell): no number") == [], "conventional commit")


def _expect_compose() -> None:
    changelog = "### Changed\n\n- shipped the shell tool"
    generated = "\n".join(
        [
            "## What's Changed",
            "* feat(shell): one native shell tool per platform (v0.10.0) by @MCapricorns in https://github.com/MCapricorns/mycode/pull/86",
            "",
            "**Full Changelog**: https://github.com/MCapricorns/mycode/compare/v0.9.28...v0.10.1",
        ]
    )
    issues = [
        FixedIssue(12, "Window overflows", "https://github.com/MCapricorns/mycode/issues/12"),
        FixedIssue(12, "duplicate", "https://github.com/MCapricorns/mycode/issues/12"),
        FixedIssue(86, "already a pull", "https://github.com/MCapricorns/mycode/issues/86"),
    ]
    body = compose_release_body(changelog, generated, issues)
    _expect(body.startswith("### Changed\n"), body)
    _expect(body.index("shipped the shell tool") < body.index("## What's Changed"), body)
    _expect(body.index("## What's Changed") < body.index("## Fixed issues"), body)
    _expect(body.index("## Fixed issues") < body.index("**Full Changelog**"), body)
    _expect("issues/12" in body, body)
    _expect(body.count("issues/12") == 1, body)
    _expect("/pull/86" in body, body)
    _expect("not generated" not in body, body)

    already = generated + "\n* Window overflows in https://github.com/MCapricorns/mycode/issues/12\n"
    again = compose_release_body(changelog, already, issues[:1])
    _expect("## Fixed issues" not in again, again)

    fallback = render_changes(
        [PullRequest(86, "feat(shell): native", "https://github.com/MCapricorns/mycode/pull/86", "MCapricorns")],
        [],
        "v0.9.28",
        "v0.10.1",
    )
    _expect("by @MCapricorns in https://github.com/MCapricorns/mycode/pull/86" in fallback, fallback)
    _expect("compare/v0.9.28...v0.10.1" in fallback, fallback)
    commits_only = render_changes([], ["fix(prompt): drop the duplicate sentence"], None, "v0.9.25")
    _expect("fix(prompt): drop the duplicate sentence" in commits_only, commits_only)
    _expect("/commits/v0.9.25" in commits_only, commits_only)
    try:
        compose_release_body("  \n", generated, [])
    except SystemExit as exc:
        _expect("empty" in str(exc), str(exc))
    else:
        raise SystemExit("self-test failed: empty changelog was accepted")


def _expect_contract() -> None:
    root = Path(__file__).resolve().parents[2]
    readme = (root / "README.md").read_text(encoding="utf-8")
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    intro, _, _ = changelog.partition("## [Unreleased]")
    workflow = (root / ".github/workflows/ci.yml").read_text(encoding="utf-8")
    _expect("自上一个标签" in readme, "Chinese docs omit linked changes")
    _expect("since the previous tag" in readme, "English docs omit linked changes")
    _expect("not generated commit notes" not in readme, "English docs still withhold linked changes")
    _expect("不从提交记录生成" not in readme, "Chinese docs still withhold linked changes")
    _expect(readme.count("github-actions[bot]") >= 2, "docs omit the bot push guard")
    _expect("自上一个标签" in intro, "changelog policy omits linked changes")
    _expect("github-actions[bot]" in intro, "changelog policy omits the bot push guard")
    _expect("不从提交记录生成" not in intro, "changelog policy still withholds linked changes")
    _expect("release_notes.py" in workflow, "workflow does not build the release body")
    _expect("github.actor != 'github-actions[bot]'" in workflow, "bot push can plan another release")


def self_test() -> None:
    _expect_previous_tag()
    _expect_extract()
    _expect_compose()
    _expect_contract()
    print("release-notes self-test passed")


def _version_from_tag(tag: str) -> str:
    if not tag.startswith("v"):
        raise SystemExit(f"tag must start with v: {tag}")
    if parse_stable(tag[1:]) is None:
        raise SystemExit(f"tag is not vMAJOR.MINOR.PATCH: {tag}")
    return tag[1:]


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--notes-file")
    parser.add_argument("--remote-tags")
    parser.add_argument("--tag", default="")
    parser.add_argument("--sha", default="")
    parser.add_argument("--repo", default="")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.self_test:
        self_test()
        return 0
    missing = [
        name
        for name in ("notes_file", "remote_tags", "tag", "sha")
        if not getattr(args, name)
    ]
    if missing:
        raise SystemExit(f"missing arguments: {', '.join(missing)}")
    if not re.fullmatch(r"[0-9a-fA-F]{40}", args.sha):
        raise SystemExit(f"sha is not 40 hex digits: {args.sha}")
    version = _version_from_tag(args.tag)
    tags = parse_ls_remote(Path(args.remote_tags).read_text(encoding="utf-8"))
    previous = previous_stable_tag(tags, version)
    changelog = Path(args.notes_file).read_text(encoding="utf-8")
    repo = args.repo or _default_repo()
    body = enrich_notes(changelog, repo, tag=args.tag, sha=args.sha.lower(), previous=previous)
    Path(args.notes_file).write_text(body, encoding="utf-8")
    print(
        f"release notes for {args.tag} since {previous or 'the beginning'}: {len(body)} bytes",
        flush=True,
    )
    return 0


def _default_repo() -> str:
    import os

    repo = os.environ.get("GITHUB_REPOSITORY", "").strip()
    if repo:
        return repo
    return "MCapricorns/mycode"


if __name__ == "__main__":
    sys.exit(main())
