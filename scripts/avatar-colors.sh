#!/bin/sh
# Makes Filo's animals in every colour but blue (src/avatar_colors.rs) into the pictures the app bundles (src/assets.rs):
#   assets/images/filo/avatar-colors/<color>/<id>.webp
# Run from anywhere when the animals or the colours change, then commit what it wrote.
# It deletes only inside its own temporary folder: the new pictures are made there, the old folder is moved in there
# when they take its place, and the folder goes when the script ends. Any other path it is asked to delete, it refuses.
set -eu
cd "$(dirname "$0")/.."
repo="$(pwd -P)"
[ -f "$repo/Cargo.toml" ] && [ -f "$repo/src/avatar_colors.rs" ] && [ -d "$repo/assets/images/filo" ] \
  || { echo "not in Filo's checkout: $repo" >&2; exit 1; }
command -v cwebp >/dev/null || { echo "needs cwebp (brew install webp)" >&2; exit 1; }
dest="$repo/assets/images/filo/avatar-colors"

tmp="$(mktemp -d "${TMPDIR:-/tmp}/filo-avatar-colors.XXXXXX")"
tmp="$(cd "$tmp" && pwd -P)"
case "$tmp" in
  /?*/filo-avatar-colors.??????) ;;
  *) echo "unexpected temporary folder: $tmp" >&2; exit 1 ;;
esac
[ -z "$(ls -A "$tmp")" ] || { echo "temporary folder isn't new: $tmp" >&2; exit 1; }
# The one place anything is deleted: a path inside $tmp, or $tmp itself, never anything else.
remove() {
  case "$1" in
    */../*|*/..) echo "refusing to delete $1" >&2; exit 1 ;;
    "$tmp"|"$tmp"/?*) rm -rf -- "$1" ;;
    *) echo "refusing to delete $1: it isn't inside $tmp" >&2; exit 1 ;;
  esac
}
trap 'remove "$tmp"' EXIT
trap 'exit 1' HUP INT TERM
# The old folder goes into $tmp with a rename, which only works on the same disk (mv elsewhere copies, then deletes).
[ "$(stat -f %d "$tmp")" = "$(stat -f %d "$repo/assets/images/filo")" ] \
  || { echo "$tmp is on another disk than $repo, so the old pictures can't be moved there" >&2; exit 1; }

cargo build -q
FILO_CONFIG_DIR="$tmp/config" ./target/debug/filo --avatar-colors "$tmp/png"
mkdir "$tmp/webp"
for dir in "$tmp"/png/*/; do
  color="$(basename "$dir")"
  mkdir -p "$tmp/webp/$color"
  for png in "$dir"*.png; do
    # About the size of the bundled blue pictures (~20 KB each).
    cwebp -quiet -q 82 -alpha_q 90 "$png" -o "$tmp/webp/$color/$(basename "${png%.png}").webp"
  done
done
if [ -e "$dest" ]; then mv "$dest" "$tmp/old"; fi
mv "$tmp/webp" "$dest"
echo "wrote $(find "$dest" -name '*.webp' | wc -l | tr -d ' ') pictures ($(du -sh "$dest" | cut -f1)) in $dest"
