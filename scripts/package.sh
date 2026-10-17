#!/bin/sh
# Builds a Filo for Mac release: the app bundle, signed, zipped for the in-app updater (Filo-<build>.zip), a disk
# image for people to download (Filo.dmg), and the latest.json that announces them (src/updater.rs reads it from FEED
# and fetches the zip). .github/workflows/release.yml runs it and publishes the three as a GitHub Release.
#
#   BUILD=42 scripts/package.sh
#
#   BUILD           the build number; the app updates to any higher one
#   FEED            where the app looks for updates: the newest GitHub Release's downloads (default:
#                   https://github.com/bitooz/filo-desktop/releases/latest/download), which the website's Download
#                   button links into too (…/Filo.dmg)
#   RELEASE_URL     the downloads of the release being made (…/releases/download/<tag>): latest.json then links the
#                   zip there, so an app that read it still gets this build's zip once a newer release is out; without
#                   it, latest.json names the zip next to itself
#   TARGETS         Rust targets, lipo'd together (releases: aarch64 and x86_64); default this Mac's,
#                   built into target/<profile> like a plain cargo build
#   PROFILE         release (default) or debug, for quick local tries
#   SIGN_IDENTITY   codesign identity (name or SHA-1); default ad hoc, which can't update itself
#   KEYCHAIN        the keychain holding it (it must be in the keychain search list too)
#   NOTARY_KEY_PATH, NOTARY_KEY_ID, NOTARY_ISSUER
#                   an App Store Connect API key: with a Developer ID identity, signs with the hardened
#                   runtime, notarizes and staples the app and the disk image, so they open without Gatekeeper's prompt
#   OUT             output folder (default dist)
#   FILO_POSTHOG_KEY, FILO_POSTHOG_HOST
#                   the PostHog project for the anonymous usage counts (docs/telemetry.md); without a key
#                   the build sends none. Cargo reads them from the environment when building.
set -eu
cd "$(dirname "$0")/.."

: "${BUILD:?BUILD=<number>}"
case "$BUILD" in
  '' | *[!0-9]*) echo "BUILD is a whole number, not '$BUILD'" >&2; exit 2 ;;
esac
name=Filo
bundle_id=im.filo.desktop
FEED="${FEED:-https://github.com/bitooz/filo-desktop/releases/latest/download}"
zip_file="Filo-$BUILD.zip"
if [ -n "${RELEASE_URL:-}" ]; then zip_file="${RELEASE_URL%/}/$zip_file"; fi
PROFILE="${PROFILE:-release}"
OUT="${OUT:-dist}"
TARGETS="${TARGETS:-}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
commit=$(git rev-parse HEAD)

echo "Building $name $version ($BUILD) from $commit for ${TARGETS:-this Mac}"
# The app without the test runs, fixtures and developer modes (the `dev` feature, Cargo.toml). A release has no paths
# from this Mac either: the source files its panic messages name start at the checkout, Cargo's home or the home folder
# (not in a debug build, where new flags would rebuild every crate the other debug builds share).
remap="build.rustflags = ['--remap-path-prefix=$HOME=~', '--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo', '--remap-path-prefix=$(pwd -P)=filo-desktop']"
build() {
  if [ "$PROFILE" = release ]; then set -- --release --config "$remap" "$@"; fi
  FILO_CHANNEL=stable FILO_BUILD="$BUILD" FILO_UPDATE_FEED="$FEED" \
    cargo build --locked --no-default-features "$@"
}
binaries=""
if [ -z "$TARGETS" ]; then
  build
  binaries="target/$PROFILE/filo"
fi
for target in $TARGETS; do
  build --target "$target"
  binaries="$binaries target/$target/$PROFILE/filo"
done

rm -rf "$OUT" && mkdir -p "$OUT"
app="$OUT/$name.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# shellcheck disable=SC2086 # one path per target
lipo -create $binaries -output "$app/Contents/MacOS/filo"
# The licences and copyright notices of what the app is built from go with it (and Filo's own, once there's a LICENSE).
cp THIRD_PARTY_NOTICES.md "$app/Contents/Resources/"
if [ -f LICENSE ]; then cp LICENSE "$app/Contents/Resources/"; fi

plist="$app/Contents/Info.plist"
cp Info.plist "$plist"
set_key() { /usr/libexec/PlistBuddy -c "Set :$1 $2" "$plist" 2>/dev/null || /usr/libexec/PlistBuddy -c "Add :$1 string $2" "$plist"; }
set_key CFBundleIdentifier "$bundle_id"
set_key CFBundleName "$name"
set_key CFBundleDisplayName "$name"
set_key CFBundleShortVersionString "$version"
set_key CFBundleVersion "$BUILD"
set_key CFBundleIconFile AppIcon
set_key LSApplicationCategoryType public.app-category.productivity

# Filo's own icon (the mark on dark), drawn by the app itself; once running, Filo shows the icon the user picked
# on the Dock (src/app_icon.rs).
iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$iconset"
# On an empty data folder: the build being packaged never opens the user's ~/.config/filo.
FILO_CONFIG_DIR="$(dirname "$iconset")/config" "$app/Contents/MacOS/filo" --app-icon "$iconset/icon.png" filo
for size in 16 32 128 256 512; do
  sips -z $size $size "$iconset/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$iconset/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
rm "$iconset/icon.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$(dirname "$iconset")"

identity="${SIGN_IDENTITY:--}"
notarize=""
if [ -n "${NOTARY_KEY_PATH:-}" ] && [ "$identity" != "-" ]; then notarize=1; fi

# Computer use: the Swift helper a chat turn's use_computer starts (computer-use/, crates/filo-local/src/computer.rs),
# for the same architectures as the app, with the bundle of Filo's cursor picture it looks for beside itself.
case "$TARGETS" in
  *x86_64*aarch64* | *aarch64*x86_64*) swift_arch="--arch arm64 --arch x86_64" ;;
  *x86_64*) swift_arch="--arch x86_64" ;;
  *aarch64*) swift_arch="--arch arm64" ;;
  *) swift_arch="" ;;
esac
# Built outside the checkout: SwiftPM writes the folder it built the picture's bundle in into the helper (which looks
# beside itself first, where the bundle is), and that shouldn't be a path from this Mac's home folder.
swift_build=/tmp/filo-computer-use
# shellcheck disable=SC2086 # one flag per architecture
swift build -c release --package-path computer-use --scratch-path "$swift_build" $swift_arch
# shellcheck disable=SC2086
bridge_dir="$(swift build -c release --package-path computer-use --scratch-path "$swift_build" $swift_arch --show-bin-path)"
cp "$bridge_dir/FiloComputerUse" "$app/Contents/Resources/filo-computer-use"
cp -R "$bridge_dir/FiloComputerUse_KWWKComputerUseCore.bundle" "$app/Contents/Resources/"
codesign --force --sign "$identity" ${KEYCHAIN:+--keychain "$KEYCHAIN"} --identifier "$bundle_id.computer-use" \
  ${notarize:+--options runtime --timestamp} "$app/Contents/Resources/filo-computer-use"

echo "Signing with ${SIGN_IDENTITY:-an ad-hoc signature}${notarize:+, hardened runtime for notarization}"
codesign --force --sign "$identity" ${KEYCHAIN:+--keychain "$KEYCHAIN"} --identifier "$bundle_id" \
  ${notarize:+--options runtime --timestamp --entitlements entitlements.plist} "$app"
codesign --verify --deep --strict "$app"

# Staples the notarization ticket to $1 and checks it's there, both over the network to Apple (api.apple-cloudkit.com):
# tried up to 5 times, a minute apart, so a dropped connection (NSURLErrorDomain -1005, stapler's exit 68) doesn't
# fail a release that's already been notarized.
staple() {
  tries=0
  until xcrun stapler staple "$1" && xcrun stapler validate "$1"; do
    tries=$((tries + 1))
    if [ "$tries" -ge 5 ]; then echo "Couldn't staple $1 after $tries tries" >&2; return 1; fi
    echo "Stapling $1 failed (try $tries of 5), trying again in a minute" >&2
    sleep 60
  done
}

zip="$OUT/Filo-$BUILD.zip"
ditto -c -k --keepParent "$app" "$zip"
if [ -n "$notarize" ]; then
  xcrun notarytool submit "$zip" --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" --wait
  staple "$app"
  rm "$zip" && ditto -c -k --keepParent "$app" "$zip"
fi

# The disk image people download: the app beside an Applications shortcut to drag it onto. It holds the stapled
# app, so it's made after the zip's notarization, then signed, notarized and stapled itself. LZMA (ULMO) opens on
# macOS 10.15 and later, well under the app's minimum, and makes the smallest image. Its name has no build number, so
# …/releases/latest/download/Filo.dmg is always the newest.
dmg="$OUT/Filo.dmg"
staging="$(mktemp -d)"
ditto "$app" "$staging/$name.app"
ln -s /Applications "$staging/Applications"
# On GitHub's Mac runners hdiutil create now and then fails with "Resource busy" (something still holds the image or
# the staging folder): tried up to 5 times, 20 s apart, with its own error shown (no -quiet, which hid it).
tries=0
until hdiutil create -volname "$name" -srcfolder "$staging" -fs HFS+ -format ULMO -ov "$dmg"; do
  tries=$((tries + 1))
  if [ "$tries" -ge 5 ]; then echo "Couldn't make $dmg after $tries tries" >&2; exit 1; fi
  echo "hdiutil create failed (try $tries of 5), trying again in 20 s" >&2
  sleep 20
done
rm -rf "$staging"
codesign --force --sign "$identity" ${KEYCHAIN:+--keychain "$KEYCHAIN"} --identifier "$bundle_id.dmg" \
  ${notarize:+--timestamp} "$dmg"
if [ -n "$notarize" ]; then
  xcrun notarytool submit "$dmg" --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" --wait
  staple "$dmg"
fi

size=$(stat -f %z "$zip")
sha256=$(shasum -a 256 "$zip" | cut -d' ' -f1)
dmg_size=$(stat -f %z "$dmg")
dmg_sha256=$(shasum -a 256 "$dmg" | cut -d' ' -f1)
cat > "$OUT/latest.json" <<EOF
{
  "channel": "stable",
  "version": "$version",
  "build": $BUILD,
  "commit": "$commit",
  "publishedAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "file": "$zip_file",
  "size": $size,
  "sha256": "$sha256",
  "dmg": {
    "file": "Filo.dmg",
    "size": $dmg_size,
    "sha256": "$dmg_sha256"
  }
}
EOF
echo "Packaged $zip ($size bytes) and $dmg ($dmg_size bytes)"
cat "$OUT/latest.json"
