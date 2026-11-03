"""Exercise DMG mount ownership and failure cleanup without macOS tools."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
MOCK = '''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

command = Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["DMG_TEST_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps([command, *args]) + "\\n")
if command == "hdiutil":
    if args[0] == "attach":
        assert "-mountpoint" in args
        Path(args[args.index("-mountpoint") + 1], "mounted").touch()
    elif args[0] == "detach":
        if os.environ.get("DMG_TEST_FAILURE") == "detach":
            sys.exit(1)
        Path(args[1], "mounted").unlink()
    elif args[0] == "convert":
        Path(args[args.index("-o") + 1]).write_bytes(b"disk image")
elif command == "ditto":
    assert Path(args[1], "mounted").exists()
    if os.environ.get("DMG_TEST_FAILURE") == "copy":
        sys.exit(23)
'''


class MakeDmgTests(unittest.TestCase):
    def package(self, root, failure=""):
        binaries = root / "bin"
        binaries.mkdir()
        for command in ("codesign", "hdiutil", "ditto"):
            stub = binaries / command
            stub.write_text(MOCK, encoding="utf-8")
            stub.chmod(0o755)
        binary = root / "mycode-desktop"
        binary.write_bytes(b"executable")
        log = root / "calls.jsonl"
        env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                   TMPDIR=str(root), DMG_TEST_LOG=str(log), DMG_TEST_FAILURE=failure)
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/make_dmg.sh"), "--binary", str(binary),
             "--version", "1.2.3", "--output", str(root / "release.dmg")],
            env=env, capture_output=True, text=True, check=False,
        )
        calls = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
        attach = next(call for call in calls if call[:2] == ["hdiutil", "attach"])
        mount = Path(attach[attach.index("-mountpoint") + 1])
        self.assertTrue(mount.is_relative_to(root))
        self.assertIn(["ditto", str(mount.parent / "stage"), str(mount)], calls)
        self.assertIn(["hdiutil", "detach", str(mount)], calls)
        return result, calls, mount

    def test_success_uses_private_mount_and_removes_work_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, _, mount = self.package(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(mount.parent.exists())
            self.assertTrue((root / "release.dmg.sha256").is_file())

    def test_copy_failure_detaches_and_preserves_error(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, calls, mount = self.package(root, "copy")
            self.assertEqual(result.returncode, 23, result.stderr)
            self.assertFalse(mount.parent.exists())
            self.assertFalse(any(call[:2] == ["hdiutil", "convert"] for call in calls))

    def test_detach_failure_does_not_delete_mounted_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result, calls, mount = self.package(root, "detach")
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue((mount / "mounted").is_file())
            self.assertFalse(any(call[:2] == ["hdiutil", "convert"] for call in calls))


if __name__ == "__main__":
    unittest.main()
