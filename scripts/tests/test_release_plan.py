"""Release regressions: empty notes must never create a bump or a second release."""

import argparse
from contextlib import redirect_stdout
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / ".github/scripts"))
import release_plan as plan  # noqa: E402
import release_notes as notes  # noqa: E402


class ReleasePlanTests(unittest.TestCase):
    def tree(self, root, unreleased=""):
        (root / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "1.0.0"\n', encoding="utf-8"
        )
        (root / "Cargo.lock").write_text(
            "version = 3\n\n" + "".join(
                f'[[package]]\nname = "{name}"\nversion = "1.0.0"\n\n'
                for name in plan.WORKSPACE_PACKAGES
            ), encoding="utf-8",
        )
        (root / "CHANGELOG.md").write_text(
            f"# Changelog\n\n## [Unreleased]\n\n{unreleased}\n\n"
            "## [1.0.0] - 2026-11-01\n\n- Initial release\n",
            encoding="utf-8",
        )

    def snapshot(self, root):
        return {name: (root / name).read_bytes()
                for name in ("Cargo.toml", "Cargo.lock", "CHANGELOG.md")}

    def git(self, root, *args):
        return subprocess.run(
            ["git", *args], cwd=root, text=True, encoding="utf-8",
            capture_output=True, check=True,
        ).stdout.strip()

    def test_missing_notes_do_not_modify_manifests(self):
        invalid = (
            "", " \n", "### Fixed\n\n### Changed", "<!-- TODO: write notes -->",
            "<!-- unfinished", "### Fixed\n-\n*\n+", "[issue]: https://example.com/1",
            "### Changed\n\n- " + plan.EMPTY_RELEASE_NOTE,
        )
        for body in invalid:
            with self.subTest(body=body), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.tree(root, body)
                before = self.snapshot(root)
                with self.assertRaisesRegex(SystemExit, "meaningful changes"):
                    plan.apply_bump(root, "1.0.1", "2026-11-03")
                self.assertEqual(self.snapshot(root), before)

    def test_lock_failure_does_not_modify_manifests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.tree(root, "- Fix startup crash")
            (root / "Cargo.lock").write_text("version = 3\n", encoding="utf-8")
            before = self.snapshot(root)
            with self.assertRaisesRegex(SystemExit, "missing workspace packages"):
                plan.apply_bump(root, "1.0.1", "2026-11-03")
            self.assertEqual(self.snapshot(root), before)

    def test_real_notes_are_promoted_once_and_match_release(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            body = "### Fixed\n\n- 修复启动崩溃，保留用户草稿。"
            self.tree(root, body)
            actual = plan.apply_bump(root, "1.0.1", "2026-11-03")
            self.assertEqual(actual, body)
            self.assertEqual(plan.release_notes_for_existing(root, "1.0.1"), body)
            log = (root / "CHANGELOG.md").read_text(encoding="utf-8")
            self.assertEqual(plan.section_body_unreleased(log), "")
            self.assertEqual(plan.rewrite_changelog(log, "1.0.1", "2026-11-04")[0], log)
            self.assertTrue(notes.compose_release_body(actual, "", []).startswith(body))

    def test_existing_placeholder_section_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.tree(root, "- Future change")
            log = root / "CHANGELOG.md"
            log.write_text(log.read_text(encoding="utf-8").replace(
                "- Initial release", "- " + plan.EMPTY_RELEASE_NOTE
            ), encoding="utf-8")
            with self.assertRaisesRegex(SystemExit, "meaningful changes"):
                plan.release_notes_for_existing(root, "1.0.0")

    def test_enrichment_cannot_hide_missing_changelog(self):
        generated = "## What's Changed\n\n- A merged pull request\n"
        with self.assertRaisesRegex(SystemExit, "meaningful changes"):
            notes.compose_release_body("- " + plan.EMPTY_RELEASE_NOTE, generated, [])

    def test_invalid_notes_do_not_emit_publish_output(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            output.write_text("previous=value\n", encoding="utf-8")
            with self.assertRaisesRegex(SystemExit, "meaningful changes"):
                plan.write_github_output(
                    output, publish="true", tag="v1.0.1", sha="a" * 40,
                    cleanup_ref="", notes="### Fixed\n\n**Full Changelog**: https://example.com",
                )
            self.assertEqual(output.read_text(encoding="utf-8"), "previous=value\n")

    def test_rerun_of_published_push_does_not_bump_again(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.tree(root, "### Fixed\n\n- Retain the draft")
            self.git(root, "init", "-b", "main")
            self.git(root, "config", "user.name", "Release Test")
            self.git(root, "config", "user.email", "release@example.com")
            self.git(root, "config", "commit.gpgsign", "false")
            self.git(root, "add", ".")
            self.git(root, "commit", "-m", "fix: retain the draft [release]")
            source = self.git(root, "rev-parse", "HEAD")
            plan.apply_bump(root, "1.0.1", "2026-11-03")
            self.git(root, "add", ".")
            self.git(root, "commit", "-m", plan.BUMP_SUBJECT.format(version="1.0.1"))
            published = self.git(root, "rev-parse", "HEAD")
            self.git(root, "tag", "v1.0.1")
            self.git(root, "checkout", "--detach", source)
            tags = root / "tags.txt"
            tags.write_text(f"{published}\trefs/tags/v1.0.1\n", encoding="utf-8")
            args = argparse.Namespace(
                head_sha=source, date="2026-11-03", remote_tags=str(tags),
                release_tags=None, subjects_file=None,
                head_subject="fix: retain the draft [release]", apply=True,
            )
            before = self.snapshot(root)
            with patch.object(plan, "ROOT", root), redirect_stdout(io.StringIO()):
                choice, body = plan.plan_from_args(args)
            self.assertFalse(choice.publish)
            self.assertFalse(choice.bump)
            self.assertEqual(body, "")
            self.assertEqual(self.snapshot(root), before)

    def test_replayed_bump_never_allocates_another_patch(self):
        for tags in ({"v1.0.1": "b" * 40}, {"v1.0.1": None}, {"v1.0.2": "b" * 40}):
            with self.subTest(tags=tags):
                choice = plan.choose_release(
                    "1.0.1", tags, plan.BUMP_SUBJECT.format(version="1.0.1"), "a" * 40
                )
                self.assertFalse(choice.publish)
                self.assertFalse(choice.bump)


if __name__ == "__main__":
    unittest.main()
