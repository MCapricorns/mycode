#!/usr/bin/env bash
# Packs the built mycode-desktop binary into MYCode.app and a UDZO .dmg.
#
# macOS only: needs codesign, hdiutil, and shasum. The macOS job in ci.yml
# and release.yml calls this after `cargo build --release`; it also works
# locally on a Mac the same way:
#
#   scripts/make_dmg.sh \
#     --binary target/aarch64-apple-darwin/release/mycode-desktop \
#     --version 0.9.20 \
#     --output mycode-desktop-v0.9.20-aarch64-apple-darwin.dmg
#
# The bundle uses assets/icon.icns (committed; regenerate with
# scripts/make_icon.py). The app is ad-hoc signed (`codesign -s -`): the
# repository has no Developer ID certificate, so Gatekeeper still flags the
# download and users right-click → Open or clear the quarantine attribute.
# Real signing slots in at the codesign call when a certificate secret
# exists. The dmg carries MYCode.app plus an /Applications symlink made by
# hdiutil; there is no signed-notarized path to keep dependency-free.

set -euo pipefail

binary=""
version=""
output=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)  binary="$2";  shift 2 ;;
    --version) version="$2"; shift 2 ;;
    --output)  output="$2";  shift 2 ;;
    *)
      echo "unexpected argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${binary}" || -z "${version}" || -z "${output}" ]]; then
  echo "usage: $0 --binary <mycode-desktop> --version <semver> --output <name.dmg>" >&2
  exit 2
fi
if [[ ! -f "${binary}" ]]; then
  echo "binary not found: ${binary}" >&2
  exit 1
fi
if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "unexpected version ${version}" >&2
  exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
icon="${root}/crates/mycode-desktop/assets/icon.icns"
if [[ ! -f "${icon}" ]]; then
  echo "icon not found: ${icon} (run scripts/make_icon.py)" >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

stage="${work}/stage"
app="${stage}/MYCode.app"
mkdir -p "${app}/Contents/MacOS" "${app}/Contents/Resources"
cp "${binary}" "${app}/Contents/MacOS/mycode-desktop"
chmod 755 "${app}/Contents/MacOS/mycode-desktop"
cp "${icon}" "${app}/Contents/Resources/icon.icns"

cat > "${app}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>MYCode</string>
    <key>CFBundleExecutable</key>
    <string>mycode-desktop</string>
    <key>CFBundleIconFile</key>
    <string>icon.icns</string>
    <key>CFBundleIdentifier</key>
    <string>com.mcapricorns.mycode</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>MYCode</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "${app}"

ln -s /Applications "${stage}/Applications"
mkdir -p "$(dirname "${output}")"
hdiutil create \
  -volname "MYCode" \
  -srcfolder "${stage}" \
  -ov \
  -format UDZO \
  -imagekey zlib-level=9 \
  "${output}"

hash="$(shasum -a 256 "${output}" | awk '{print $1}')"
printf '%s  %s\n' "${hash}" "$(basename "${output}")" > "${output}.sha256"
