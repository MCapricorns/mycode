# MYCode application icon

The mark combines the M in MYCode with square code brackets: each upright has
a short outward tab at the top and bottom, and the diagonals meet in a deep V.
It uses a paper monogram (`#FAF6EE`) on a red stamp (`#C72E38`), with transparent
outer corners. Ink (`#14131B`) is the dark surface in the preview. All fills
are flat; the artwork contains no font, external image, gradient, or shadow.

`icon.svg` is the editable vector source, suitable for reuse on the wiki. Its
256-unit square contains a rounded rectangle at `(16,16)`, width and height
224, corner radius 48. The paper polygon is symmetric about `x=128`, spans
`x=48..208` and `y=64..192`, and uses 32-unit stems with 16-unit outward tabs.
The upper V ends at `(128,104)` and its lower edge at `(128,160)`. The SVG
contains the exact polygon coordinates; reuse that shape rather than a font M.
At 16px, the stems are 2px wide and the tabs extend 1px.

From the repository root, regenerate with Python 3.9 or later; no packages
need to be installed:

```sh
python scripts/make_icon.py
python scripts/make_icon.py --check
```

The generator locates assets relative to its own file, so an absolute script
path also works from another directory. It reads the SVG and rasterizes every
size independently with 4x supersampling. It writes straight RGBA and averages
edge samples before unpremultiplying, so transparent red edges stay clean on
light and dark surfaces. Output contains no timestamp or host-dependent font
rendering.
`--check` regenerates in memory and fails if any committed artifact is missing
or differs from its expected bytes.

| File | Contents |
| --- | --- |
| `icon.ico` | 16, 24, 32, 48, 64, 128px uncompressed 32-bit DIBs with transparency masks; 256px PNG |
| `icon.icns` | PNG entries `icp4`/`icp5`/`icp6` (16/32/64px), `ic07`/`ic08`/`ic09` (128/256/512px), and Retina `ic11`/`ic12`/`ic13`/`ic14`/`ic10` (32/64/256/512/1024px) |
| `icon.png` | Transparent 1024px export |
| `icon-preview.png` | 1024 x 800 proof sheet on paper and ink surfaces |

The preview has three rows in each half: 256px; native 16, 24, 32, 48 and
128px; then nearest-neighbor 4x enlargements of the 16, 24 and 32px versions.
Enlargements expose the actual small-size pixels rather than a scaled vector.
The ICO and ICNS use those same rendered pixels.

Keep the artifact names: `../build.rs` embeds `assets/icon.ico` as Windows
resource 1, `../src/main.rs` exposes it as `brand/icon.ico`, and
`../../../scripts/make_dmg.sh` copies `icon.icns` to the macOS bundle and names
it in `CFBundleIconFile`. Local icon verification does not require a Rust
build. macOS and ARM64 build and platform validation belong to GitHub Actions.
