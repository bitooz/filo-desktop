#!/bin/bash
# Publishes the Chromium engine chromium/package.sh made (in OUT) on GitHub, where Filo looks for it
# (src/browser/engine.rs): the zips as a release of their own, chromium-<version>, never marked the latest (Filo's own
# updates and the website's Download button read the latest release); then the feed, chromium-<protocol>.json, swapped
# into the release chromium-engine, which every Filo reads it from. Filos running Chromium fetch the new engine within
# 6 hours and switch to it the next time the engine starts. Needs gh, logged in with write access to REPO.
#
#   chromium/publish.sh
#
#   REPO   default bitooz/filo-desktop
#   OUT    default dist/chromium
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="${REPO:-bitooz/filo-desktop}"
OUT="${OUT:-dist/chromium}"
feed=$(ls "$OUT"/chromium-*.json | head -1)
version=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$feed")
zips=("$OUT"/Filo-Chromium-"$version"-*.zip)
[ -f "${zips[0]}" ] || { echo "publish.sh: no zips for $version in $OUT" >&2; exit 1; }
# Every zip the feed names must be the one here, byte for byte.
python3 - "$feed" "$OUT" <<'PY'
import hashlib, json, os, sys
feed, out = json.load(open(sys.argv[1])), sys.argv[2]
for arch, build in feed["builds"].items():
    path = os.path.join(out, os.path.basename(build["file"]))
    data = open(path, "rb").read()
    assert len(data) == build["size"] and hashlib.sha256(data).hexdigest() == build["sha256"], f"{path} isn't the feed's"
PY

notes="Filo's Chromium engine $version (Chromium $(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["chromium"])' "$feed")), for Settings › Browser."
gh release create "chromium-$version" --repo "$REPO" --title "Chromium engine $version" --notes "$notes" \
  --latest=false "${zips[@]}"
if ! gh release view chromium-engine --repo "$REPO" >/dev/null 2>&1; then
  gh release create chromium-engine --repo "$REPO" --title "Chromium engine feed" \
    --notes "Where Filo looks for its Chromium engine: chromium-<protocol>.json names the newest one." --latest=false
fi
gh release upload chromium-engine --repo "$REPO" --clobber "$feed"
echo "Published Chromium engine $version: $(basename "$feed") in chromium-engine"
