#!/bin/sh
# Builds target/release/DSV Reader.app: the release binary, Info.plist with the Cargo version,
# AppIcon.icns made from icon.png, and an ad-hoc signature. It installs nothing; drag the app to Applications if you want it there.
set -eu
root=$(dirname "$0")
cargo build --release --manifest-path "$root/Cargo.toml"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
app="$root/target/release/DSV Reader.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
tmp=$(mktemp -d)
icons="$tmp/AppIcon.iconset"
mkdir "$icons"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$root/icon.png" --out "$icons/icon_${s}x${s}.png" > /dev/null
  sips -z "$((s * 2))" "$((s * 2))" "$root/icon.png" --out "$icons/icon_${s}x${s}@2x.png" > /dev/null
done
iconutil -c icns "$icons" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$tmp"
# -X: no extended attributes. The bare binary can carry a Finder icon (below), and codesign refuses that in a bundle.
cp -X "$root/target/release/dsv-reader" "$app/Contents/MacOS/dsv-reader"
sed "s/@VERSION@/$version/g" "$root/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
# A Finder icon on the bare binary too, from the .icns. It lives in extended attributes, so the next
# cargo build that links the binary again removes it; this script sets it again. Not in CI (no Finder there).
[ -n "${CI:-}" ] || osascript -l JavaScript -e 'ObjC.import("AppKit"); function run(a) { const icon = $.NSImage.alloc.initWithContentsOfFile(a[0]); return $.NSWorkspace.sharedWorkspace.setIconForFileOptions(icon, a[1], 0) ? "Finder icon set on " + a[1] : "Finder icon NOT set on " + a[1]; }' "$app/Contents/Resources/AppIcon.icns" "$root/target/release/dsv-reader"
echo "built $app ($version)"
