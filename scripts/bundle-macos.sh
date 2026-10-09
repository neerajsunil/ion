#!/bin/sh
# Builds target/release/Ion.app (Apple Silicon) and zips it into dist/.
#
#   scripts/bundle-macos.sh
#
# Signs ad hoc by default, which Apple Silicon requires to run the app at all.
# Set ION_SIGN_IDENTITY to a "Developer ID Application: ..." identity to sign
# for distribution instead (notarize the zip afterwards).
set -eu

cd "$(dirname "$0")/.."
target=aarch64-apple-darwin
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)

cargo build -p ion --release --locked --target "$target"

app=target/release/Ion.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$target/release/ion" "$app/Contents/MacOS/ion"
cp crates/ion/resources/ion.icns "$app/Contents/Resources/ion.icns"
sed "s/@VERSION@/$version/g" crates/ion/resources/Info.plist > "$app/Contents/Info.plist"

identity=${ION_SIGN_IDENTITY:--}
if [ "$identity" = "-" ]; then
    codesign --force --sign - "$app"
else
    codesign --force --options runtime --timestamp --sign "$identity" "$app"
fi
codesign --verify --strict "$app"

name="ion-$version-macos-arm64"
mkdir -p dist
rm -f "dist/$name.zip"
# ditto keeps the bundle's metadata and signature intact.
ditto -c -k --keepParent "$app" "dist/$name.zip"
echo "dist/$name.zip"
