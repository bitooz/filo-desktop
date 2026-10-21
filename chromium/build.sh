#!/bin/bash
# Builds Filo Chromium (chromium/filo_chromium.mm), the Chromium engine Filo's browser can run on instead of WebKit
# (src/browser/chromium.rs): people download it from Settings › Browser (src/browser/engine.rs), as the zip
# chromium/package.sh makes from this build. Everything goes in target/cef/<arch>:
#
#   dist/                 CEF's minimal build (downloaded once, checked against its SHA-1; ~340 MB)
#   wrapper-build/        CEF's C++ wrapper library, built with CMake and Ninja
#   Filo Chromium.app     the app, with the framework (an APFS clone of dist's, so no extra space) and its helpers
#
#   chromium/build.sh
#   ARCH=x86_64 SIGN_IDENTITY="Developer ID Application: …" ENGINE_BUILD=2 chromium/build.sh
#
#   ARCH            arm64 or x86_64 (default: this Mac's)
#   SIGN_IDENTITY   codesign identity (name or SHA-1). With one, every binary is signed for notarization: the hardened
#                   runtime, a secure timestamp, and each kind of process's entitlements (chromium/entitlements/).
#                   Default ad hoc, for trying it on this Mac.
#   KEYCHAIN        the keychain holding the identity
#   ENGINE_BUILD    the engine's own build number: its version is CEF's and this (154.0.34.1), and Filo updates to
#                   any higher one (default 1)
#
# The helper processes run in Chromium's sandbox (CefScopedSandboxContext, the framework's libcef_sandbox.dylib).
set -euo pipefail
cd "$(dirname "$0")/.."
cef='154.0.34+g14c5a08+chromium-154.0.8037.98'
ARCH="${ARCH:-$(uname -m)}"
case "$ARCH" in
  arm64 | aarch64) ARCH=arm64; platform=macosarm64; sha1='b7081f6609e5edccf07be96b5d7977329fbe1bdf' ;;
  x86_64) platform=macosx64; sha1='51d90349e24360b8e0daecc2d026ad150c620db5' ;;
  *) echo "build.sh: ARCH is arm64 or x86_64, not $ARCH" >&2; exit 2 ;;
esac
ENGINE_BUILD="${ENGINE_BUILD:-1}"
case "$ENGINE_BUILD" in
  '' | *[!0-9]*) echo "build.sh: ENGINE_BUILD is a whole number, not '$ENGINE_BUILD'" >&2; exit 2 ;;
esac
version="${cef%%+*}.$ENGINE_BUILD"
identity="${SIGN_IDENTITY:--}"
archive="cef_binary_${cef}_${platform}_minimal.tar.bz2"
out="target/cef/$ARCH"
dist="$out/dist"
app="$out/Filo Chromium.app"
mkdir -p "$out"

if [ ! -d "$dist/Release/Chromium Embedded Framework.framework" ]; then
  free_kb=$(df -k "$out" | awk 'NR==2 {print $4}')
  if [ "$free_kb" -lt 1500000 ]; then
    echo "build.sh: CEF needs about 500 MB while it unpacks, and there's $((free_kb / 1024)) MB free" >&2
    exit 1
  fi
  download="$(mktemp -d "$out/download.XXXXXX")"
  trap 'rm -rf "$download"' EXIT
  curl -fsSL -o "$download/cef.tar.bz2" "https://cef-builds.spotifycdn.com/${archive//+/%2B}"
  echo "$sha1  $download/cef.tar.bz2" | shasum -c - >/dev/null
  tar xjf "$download/cef.tar.bz2" -C "$download"
  rm -rf "$dist"
  mv "$download"/cef_binary_* "$dist"
  rm -rf "$download"
fi

wrapper="$out/wrapper-build/libcef_dll_wrapper/libcef_dll_wrapper.a"
if [ ! -f "$wrapper" ]; then
  cmake -S "$dist" -B "$out/wrapper-build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DPROJECT_ARCH="$ARCH" \
    -DCMAKE_OSX_ARCHITECTURES="$ARCH" >/dev/null
  ninja -C "$out/wrapper-build" libcef_dll_wrapper >/dev/null
fi

flags=(-std=c++20 -ObjC++ -O2 -arch "$ARCH" -mmacosx-version-min=12.0 -fno-exceptions -fno-rtti -fobjc-arc
  -DCEF_USE_SANDBOX -DNDEBUG -DFILO_ENGINE_VERSION="\"$version\"" -I "$dist" -Wall -Wno-unused-parameter)
libs=("$wrapper" -framework AppKit -framework Cocoa -framework IOSurface)
mkdir -p "$out/obj"
clang++ "${flags[@]}" chromium/filo_chromium.mm "${libs[@]}" -o "$out/obj/filo-chromium"
clang++ "${flags[@]}" -DFILO_HELPER chromium/filo_chromium.mm "${libs[@]}" -o "$out/obj/filo-chromium-helper"

plist() { # name, executable, id
  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$1</string>
  <key>CFBundleExecutable</key><string>$2</string>
  <key>CFBundleIdentifier</key><string>$3</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSCameraUsageDescription</key><string>A web page in Filo's browser asks for the camera.</string>
  <key>NSMicrophoneUsageDescription</key><string>A web page in Filo's browser asks for the microphone.</string>
</dict>
</plist>
PLIST
}

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks"
plist "Filo Chromium" "Filo Chromium" im.filo.chromium >"$app/Contents/Info.plist"
cp "$out/obj/filo-chromium" "$app/Contents/MacOS/Filo Chromium"
cp -cR "$dist/Release/Chromium Embedded Framework.framework" "$app/Contents/Frameworks/"
# Chromium starts its processes from helper apps named after the main one, one per kind.
kinds=("" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)")
for kind in "${kinds[@]}"; do
  name="Filo Chromium Helper$kind"
  helper="$app/Contents/Frameworks/$name.app"
  suffix=$(echo "$kind" | tr -d ' ()' | tr 'A-Z' 'a-z')
  mkdir -p "$helper/Contents/MacOS"
  plist "$name" "$name" "im.filo.chromium.helper${suffix:+.$suffix}" >"$helper/Contents/Info.plist"
  cp "$out/obj/filo-chromium-helper" "$helper/Contents/MacOS/$name"
done

# Signed from the inside out: the framework's libraries, the framework, each helper with its kind's entitlements (the
# plain and alerts helpers have none), then the app. A real identity signs for notarization.
sign() { # path, entitlements (optional), identifier (optional)
  local args=(--force --sign "$identity")
  if [ -n "${KEYCHAIN:-}" ]; then args+=(--keychain "$KEYCHAIN"); fi
  if [ "$identity" != "-" ]; then args+=(--options runtime --timestamp); fi
  if [ -f "chromium/entitlements/${2:-none}.plist" ]; then args+=(--entitlements "chromium/entitlements/$2.plist"); fi
  if [ -n "${3:-}" ]; then args+=(--identifier "$3"); fi
  codesign "${args[@]}" "$1" >/dev/null 2>&1 || codesign "${args[@]}" "$1"
}
framework="$app/Contents/Frameworks/Chromium Embedded Framework.framework"
find "$framework" -type f -name '*.dylib' | while read -r library; do sign "$library"; done
sign "$framework"
for kind in "${kinds[@]}"; do
  suffix=$(echo "$kind" | tr -d ' ()' | tr 'A-Z' 'a-z')
  sign "$app/Contents/Frameworks/Filo Chromium Helper$kind.app" "helper${suffix:+-$suffix}"
done
sign "$app" main
codesign --verify --deep --strict "$app"
echo "$app"
