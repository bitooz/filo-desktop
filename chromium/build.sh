#!/bin/bash
# Builds Filo Chromium (chromium/filo_chromium.mm), the test engine for Filo's browser: `dev` builds open their tabs in
# it with FILO_BROWSER_ENGINE=chromium (src/browser/chromium.rs). Everything goes in target/cef:
#
#   dist/                 CEF's minimal build for this Mac (downloaded once, checked against its SHA-1; ~340 MB)
#   wrapper-build/        CEF's C++ wrapper library, built with CMake and Ninja
#   Filo Chromium.app     the app, with the framework (an APFS clone of dist's, so no extra space) and its helpers
#
#   chromium/build.sh
set -euo pipefail
cd "$(dirname "$0")/.."
version='154.0.34+g14c5a08+chromium-154.0.8037.98'
archive="cef_binary_${version}_macosarm64_minimal.tar.bz2"
sha1='b7081f6609e5edccf07be96b5d7977329fbe1bdf'
out=target/cef
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
  cmake -S "$dist" -B "$out/wrapper-build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DPROJECT_ARCH=arm64 >/dev/null
  ninja -C "$out/wrapper-build" libcef_dll_wrapper >/dev/null
fi

flags=(-std=c++20 -ObjC++ -O2 -arch arm64 -mmacosx-version-min=12.0 -fno-exceptions -fno-rtti -fobjc-arc
  -DCEF_USE_SANDBOX -DNDEBUG -I "$dist" -Wall -Wno-unused-parameter)
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
  <key>CFBundleShortVersionString</key><string>${version%%+*}</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
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
for kind in "" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)"; do
  name="Filo Chromium Helper$kind"
  helper="$app/Contents/Frameworks/$name.app"
  suffix=$(echo "$kind" | tr -d ' ()' | tr 'A-Z' 'a-z')
  mkdir -p "$helper/Contents/MacOS"
  plist "$name" "$name" "im.filo.chromium.helper${suffix:+.$suffix}" >"$helper/Contents/Info.plist"
  cp "$out/obj/filo-chromium-helper" "$helper/Contents/MacOS/$name"
done
codesign --force --deep --sign - "$app" >/dev/null 2>&1
echo "$app"
