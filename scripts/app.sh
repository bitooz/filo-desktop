#!/bin/bash
# Builds target/<profile>/Filo.app around the filo binary and opens it.
# macOS asks for the microphone (voice messages) only on behalf of an app:
# started from a terminal, the binary counts as part of the terminal app,
# which can't ask for it. The same goes for notifications. Run Filo this way.
#
#   scripts/app.sh [--release] [filo args…]     FILO_* switches pass through; FILO_LOG=<file> gets its output
#
# A test run (FILO_OPEN, FILO_SMOKE, a drive or shot switch) gets a data folder of its own when it names none, so it
# never runs on ~/.config/filo next to the user's Filo.
set -e
cd "$(dirname "$0")/.."
profile=debug
# --release builds with scripts/package.sh's path flags (but keeps the `dev` feature), so the two share target/release.
remap="build.rustflags = ['--remap-path-prefix=$HOME=~', '--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo', '--remap-path-prefix=$(pwd -P)=filo-desktop']"
if [ "$1" = "--release" ]; then profile=release; shift; cargo build --release --config "$remap"; else cargo build; fi
app="target/$profile/Filo.app"
mkdir -p "$app/Contents/MacOS"
cp Info.plist "$app/Contents/Info.plist"
cp "target/$profile/filo" "$app/Contents/MacOS/filo"
# The app icon the user picked as the bundle's icon (Finder, a Dock icon kept while Filo is closed, banners), from
# the Dock icon the app saved last time it ran (src/app_icon.rs; "local": Filo keeps its data on this Mac). While it
# runs, the app sets the current one on the Dock itself.
icon="${FILO_CONFIG_DIR:-$HOME/.config/filo}/app-icon-local.png"
if [ -f "$icon" ]; then
  iconset="$(mktemp -d)/AppIcon.iconset"
  mkdir -p "$iconset" "$app/Contents/Resources"
  for size in 16 32 128 256 512; do
    sips -z $size $size "$icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$icon" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
  /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string AppIcon" "$app/Contents/Info.plist"
fi
# macOS only gives notifications to apps signed with an Apple-issued certificate: sign with this Mac's
# "Developer ID Application" identity, else its "Apple Development" one (SIGN_IDENTITY picks another),
# else ad hoc, which runs but never shows banners.
identities="$(security find-identity -v -p codesigning 2>/dev/null)"
identity="${SIGN_IDENTITY:-$(printf '%s\n' "$identities" | grep -oE '"Developer ID Application[^"]*"' | head -1 | tr -d '"')}"
identity="${identity:-$(printf '%s\n' "$identities" | grep -oE '"Apple Development[^"]*"' | head -1 | tr -d '"')}"
# Computer use: the Swift helper a chat turn's use_computer starts (computer-use/, crates/filo-local/src/computer.rs),
# as Contents/Resources/filo-computer-use, with the bundle of Filo's cursor picture it looks for beside itself (it stops
# without it). Signed before the app, which seals it.
swift build -c release --package-path computer-use >/dev/null
bridge_dir="$(swift build -c release --package-path computer-use --show-bin-path)"
mkdir -p "$app/Contents/Resources"
cp "$bridge_dir/FiloComputerUse" "$app/Contents/Resources/filo-computer-use"
rm -rf "$app/Contents/Resources/FiloComputerUse_KWWKComputerUseCore.bundle"
cp -R "$bridge_dir/FiloComputerUse_KWWKComputerUseCore.bundle" "$app/Contents/Resources/"
codesign --force --sign "${identity:--}" --identifier im.filo.desktop.computer-use "$app/Contents/Resources/filo-computer-use" >/dev/null 2>&1
if [ -n "$identity" ]; then
  codesign --force --sign "$identity" "$app" >/dev/null
else
  echo "Signing ad hoc: no Apple Development certificate on this Mac, so no notifications (see README)." >&2
  codesign --force --sign - "$app" >/dev/null 2>&1
fi
# The switches test_run.rs counts as a test run.
for switch in FILO_SMOKE FILO_OPEN FILO_FIXTURE_SHOTS FILO_FIXTURE_DRIVE FILO_FIXTURE_FRAMES FILO_FIXTURE_PLAN \
  FILO_DRIVE_SHOTS FILO_SHOT_AFTER FILO_PET_DRIVE FILO_QUIT_DRIVE FILO_WELCOME_DRIVE FILO_COMPARE_DRIVE; do
  if [ -n "${!switch}" ] && [ -z "$FILO_CONFIG_DIR" ]; then
    FILO_CONFIG_DIR="$(mktemp -d -t filo-app)"
    export FILO_CONFIG_DIR
    echo "A test run: its data folder is $FILO_CONFIG_DIR" >&2
  fi
done
# `open` starts the app with launchd's environment, not this shell's: every FILO_* variable goes along by name.
env_args=()
while IFS= read -r name; do
  [ "$name" = FILO_LOG ] || env_args+=(--env "$name=${!name}")
done < <(compgen -e | grep '^FILO_' || true)
set -- -n "$app" "${env_args[@]}" --args "$@"
if [ -n "$FILO_LOG" ]; then set -- --stdout "$FILO_LOG" --stderr "$FILO_LOG" "$@"; fi
open "$@"
