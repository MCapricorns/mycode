"""Icon checks compare decoded content across zlib implementations."""
import importlib.util
import pathlib
import struct
import unittest
import zlib
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("make_icon", ROOT / "scripts/make_icon.py")
ICONS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ICONS)


class IconArtifactTests(unittest.TestCase):
    def test_compression_differences_do_not_make_icons_stale(self):
        images = {size: [b"\xff\x00\x00\xff" * size] * size for size in ICONS.ICO_SIZES}
        original = {size: ICONS.png_bytes(rows, size) for size, rows in images.items()}
        compress = zlib.compress
        with patch.object(ICONS.zlib, "compress", side_effect=lambda data, _: compress(data, 0)):
            alternate = {size: ICONS.png_bytes(rows, size) for size, rows in images.items()}
        for name, actual, expected in [
            ("icon.png", original[256], alternate[256]),
            ("icon.ico", ICONS.ico_bytes(images, original), ICONS.ico_bytes(images, alternate)),
        ]:
            self.assertNotEqual(actual, expected)
            self.assertTrue(ICONS.artifact_matches(name, actual, expected))
        # One PNG-bearing ICNS chunk is enough to exercise the container.
        def icns(png):
            return b"icns" + struct.pack(">I", len(png) + 16) + b"ic08" + struct.pack(">I", len(png) + 8) + png
        self.assertTrue(ICONS.artifact_matches("icon.icns", icns(original[256]), icns(alternate[256])))

    def test_changed_pixels_and_corrupt_data_stay_stale(self):
        first = ICONS.png_bytes([b"\xff\x00\x00\xff"], 1)
        second = ICONS.png_bytes([b"\x00\xff\x00\xff"], 1)
        self.assertFalse(ICONS.artifact_matches("icon.png", first, second))
        for corrupt in (b"", first[:-1], first + b"junk", first[:29] + b"bad!" + first[33:]):
            self.assertFalse(ICONS.artifact_matches("icon.png", corrupt, first))
        for name in ("icon.ico", "icon.icns"):
            self.assertFalse(ICONS.artifact_matches(name, b"invalid", b"invalid"))


if __name__ == "__main__":
    unittest.main()
