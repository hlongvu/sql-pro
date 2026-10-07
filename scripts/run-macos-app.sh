#!/usr/bin/env bash
set -euo pipefail

# Build a real macOS application bundle so Dock and Finder use SQL Pro's
# database icon. `cargo run` launches a bare executable, which macOS displays
# with its generic `exec` icon instead.
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_profile="${1:-debug}"
if [[ "$build_profile" == "release" ]]; then
    build_command=(cargo build -p sqlpro --release)
else
    build_command=(cargo build -p sqlpro)
    build_profile="debug"
fi
binary_dir="$project_root/target/$build_profile"
app_bundle="$binary_dir/SQL Pro.app"
resources_dir="$app_bundle/Contents/Resources"
iconset_dir="$resources_dir/sql-pro.iconset"
source_icon="$project_root/assets/sql-pro-icon-yellow.png"

cd "$project_root"
"${build_command[@]}"

rm -rf "$app_bundle"
mkdir -p "$iconset_dir" "$app_bundle/Contents/MacOS"

for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$source_icon" --out "$iconset_dir/icon_${size}x${size}.png" >/dev/null
    double_size=$((size * 2))
    sips -z "$double_size" "$double_size" "$source_icon" --out "$iconset_dir/icon_${size}x${size}@2x.png" >/dev/null
done

iconutil --convert icns "$iconset_dir" --output "$resources_dir/sql-pro.icns"
cp "$project_root/crates/app/macos/Info.plist" "$app_bundle/Contents/Info.plist"
cp "$binary_dir/sqlpro" "$app_bundle/Contents/MacOS/sqlpro"

open "$app_bundle"
