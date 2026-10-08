#!/usr/bin/env bash
set -euo pipefail

# Produce an ad-hoc-signed, drag-to-install DMG. A Developer ID signature and
# notarization are still required before distributing the DMG to other Macs.
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(awk -F'"' '/^version\.workspace = true/ { next } /^version = / { print $2; exit }' "$project_root/Cargo.toml")"
version="${version:-0.1.0}"
release_dir="$project_root/target/release"
app_bundle="$release_dir/SQL Pro.app"
staging_dir="$release_dir/dmg-staging"
dist_dir="$project_root/dist"
dmg_path="$dist_dir/SQL-Pro-${version}-macos.dmg"

SQLPRO_OPEN_APP=0 "$project_root/scripts/run-macos-app.sh" release

# Ad-hoc signing makes the local bundle structurally valid without claiming it
# is notarized or trusted by an Apple Developer ID certificate.
codesign --force --deep --sign - "$app_bundle"
codesign --verify --deep --strict "$app_bundle"

rm -rf "$staging_dir"
mkdir -p "$staging_dir" "$dist_dir"
ditto "$app_bundle" "$staging_dir/SQL Pro.app"
ln -s /Applications "$staging_dir/Applications"

rm -f "$dmg_path"
hdiutil create \
    -volname "SQL Pro" \
    -srcfolder "$staging_dir" \
    -format UDZO \
    -ov \
    "$dmg_path" >/dev/null

echo "Created $dmg_path"
