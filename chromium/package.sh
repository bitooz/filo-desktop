#!/bin/bash
# Makes the Chromium engine's download, which Settings › Browser fetches (src/browser/engine.rs): for each
# architecture, Filo Chromium.app as chromium/build.sh builds and signs it, notarized and stapled when there's a key
# for it, zipped as Filo-Chromium-<version>-<arch>.zip; and the feed that names them, chromium-<protocol>.json, with each
# zip's size and SHA-256. <protocol> is the engine's kProtocol (chromium/filo_chromium.mm): a Filo reads the feed of
# the protocol it speaks, so an engine that changes it never reaches an older Filo. chromium/publish.sh puts them on
# GitHub; the folder served over HTTP works as a feed too (FILO_CHROMIUM_FEED, in `dev` builds).
#
#   SIGN_IDENTITY="Developer ID Application: …" ARCHS="arm64 x86_64" ENGINE_BUILD=2 chromium/package.sh
#
#   ARCHS           the architectures (default this Mac's)
#   ENGINE_BUILD, SIGN_IDENTITY, KEYCHAIN
#                   as chromium/build.sh takes them; Filo installs only an engine signed by the same developer as
#                   itself, so a release needs the Developer ID identity Filo is signed with
#   NOTARY_KEY_PATH, NOTARY_KEY_ID, NOTARY_ISSUER
#                   an App Store Connect API key: each app is notarized and stapled (as scripts/package.sh does Filo)
#   FILES_URL       where the zips are downloaded from: default the release chromium/publish.sh makes for this version
#                   (https://github.com/bitooz/filo-desktop/releases/download/chromium-<version>); empty, next to the
#                   feed (a local feed)
#   OUT             output folder (default dist/chromium)
set -euo pipefail
cd "$(dirname "$0")/.."
ARCHS="${ARCHS:-$(uname -m)}"
ENGINE_BUILD="${ENGINE_BUILD:-1}"
OUT="${OUT:-dist/chromium}"
cef=$(sed -n "s/^cef='\(.*\)'$/\1/p" chromium/build.sh)
version="${cef%%+*}.$ENGINE_BUILD"
chromium="${cef##*chromium-}"
protocol=$(sed -n 's/^constexpr int kProtocol = \([0-9]*\);$/\1/p' chromium/filo_chromium.mm)
FILES_URL="${FILES_URL-https://github.com/bitooz/filo-desktop/releases/download/chromium-$version}"
notarize=""
if [ -n "${NOTARY_KEY_PATH:-}" ] && [ "${SIGN_IDENTITY:--}" != "-" ]; then notarize=1; fi
mkdir -p "$OUT"

builds=""
for arch in $ARCHS; do
  case "$arch" in aarch64) arch=arm64 ;; esac
  app=$(ARCH="$arch" ENGINE_BUILD="$ENGINE_BUILD" chromium/build.sh | tail -1)
  zip="$OUT/Filo-Chromium-$version-$arch.zip"
  rm -f "$zip"
  ditto -c -k --keepParent "$app" "$zip"
  if [ -n "$notarize" ]; then
    xcrun notarytool submit "$zip" --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" --wait
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    rm "$zip" && ditto -c -k --keepParent "$app" "$zip"
  fi
  name=$(basename "$zip")
  file="${FILES_URL:+${FILES_URL%/}/}$name"
  size=$(stat -f %z "$zip")
  sha256=$(shasum -a 256 "$zip" | cut -d' ' -f1)
  builds="$builds${builds:+,}\"$arch\":{\"file\":\"$file\",\"size\":$size,\"sha256\":\"$sha256\"}"
  echo "$name: $((size / 1024 / 1024)) MB${notarize:+, notarized}"
done

feed="$OUT/chromium-$protocol.json"
printf '{"version":"%s","chromium":"%s","protocol":%s,"builds":{%s}}\n' "$version" "$chromium" "$protocol" "$builds" |
  python3 -m json.tool >"$feed"
echo "$feed"
