#!/bin/sh
# Checks that a picture attached in the message box keeps its chip and its picture when an "@" is typed after it and a
# name picked from the list (smoke.rs `run_chip_mention_drive`), in the real app with fixture data, offline: a sandbox
# (its own config dir with a made-up stored session), the debug build started on its own with FILO_OFFLINE=1 and
# FILO_OPEN=chip-mention-drive in the agents' group chat, which types with real keys posted inside the app.
# Touches nothing of the user's: no backend, no sign-in, no runner, no shortcuts, menu bar item or pet; nothing can be
# sent.
#
# Usage: scripts/chip-mention-check.sh [out-dir]   (after `cargo build`)
# Exits non-zero when the run fails; its log (PASS/FAIL per check) and screenshots go to out-dir.
set -u
here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$(mktemp -d /tmp/filo-chip-mention-out-XXXX)}"
mkdir -p "$out"
app="${FILO_APP:-$here/target/debug/filo}"
box="$(mktemp -d /tmp/filo-chip-mention-XXXX)"
picture="$box/picture.png"
cp "$here/assets/images/filo/workers/mochi.png" "$picture"

box="$box" user="user_chip_mention_fixture" python3 - <<'EOF'
import hashlib, json, os, time
box, user = os.environ["box"], os.environ["user"]
def write(path, value):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(value, f, ensure_ascii=False)
write(f"{box}/credentials.json", {"environments": {"development": {"clerk": {
    "frontendApi": "clerk.fixture.invalid", "clientJwt": "fixture", "sessionId": "sess_fixture", "userId": user}}}})
write(f"{box}/desktop-welcome.json", {"seen": True})
write(f"{box}/desktop-companion.json", {"menuBar": False, "pet": False, "shortcuts": {"open": "", "new-chat": "", "pet": "", "screen": ""}})
safe = "".join(c if c.isalnum() or c in "_-" else "_" for c in user)
cache = f"{box}/cache/development/{safe}/queries"
def query(path, args, value):
    # query_cache.rs: sha256 of path, a newline and the arguments with sorted keys (serde_json, compact).
    key = hashlib.sha256((path + "\n" + json.dumps(args, sort_keys=True, separators=(",", ":"), ensure_ascii=False)).encode()).hexdigest()
    write(f"{cache}/{key}.json", value)
now = time.time()
query("voiceSessions:recent", {"limit": 20}, [])
query("voiceSessions:pinned", {}, [])
query("voiceSessions:archived", {"limit": 50}, [])
agents = [("agent_fixture_juno", "Juno", "fox-waving", "chat_fixture_juno"), ("agent_fixture_pip", "Pip", "panda-waving", "chat_fixture_pip")]
query("agents:list", {}, [{"_id": id, "name": name, "picture": picture, "instructions": "Helps with plans.", "sessionId": session,
                           "unseen": 0} for id, name, picture, session in agents])
query("agents:group", {}, None)
query("users:current", {}, {"userName": "Sam"})
for _, title, _, id in agents:
    at = now - 7200
    query("voiceSessions:detail", {"id": id, "limit": 40.0}, {
        "session": {"_id": id, "title": title},
        "messages": [{"_id": f"{id}_m1", "_creationTime": at * 1000, "role": "user", "content": f"Hello {title}."},
                     {"_id": f"{id}_m2", "_creationTime": at * 1000 + 1000, "role": "assistant", "content": "Hi, noted."}],
        "hasMore": False, "tasks": [], "calls": []})
    for path in ["voiceSessions:chatTurns", "coding:requestsForSession", "coding:sessionsForChat", "howIWork:proposalsForSession"]:
        query(path, {"sessionId": id}, [])
EOF

FILO_CONFIG_DIR="$box" FILO_ENV=development FILO_OFFLINE=1 FILO_WINDOW_FLOAT=1 \
  FILO_OPEN=chip-mention-drive FILO_DRIVE_SHOTS="$out" \
  FILO_DRIVE_AGENT_ID=group FILO_DRIVE_PICTURE="$picture" \
  "$app" >"$out/run.log" 2>&1 &
pid=$!
# A stuck run is stopped after three minutes (and counts as failed).
(sleep 180; kill "$pid" 2>/dev/null) &
watchdog=$!
wait "$pid"
code=$?
kill "$watchdog" 2>/dev/null
grep '^chip-mention:' "$out/run.log" >"$out/checks.log"
echo "exit $code ($(grep -c ' PASS ' "$out/checks.log") passed, $(grep -c ' FAIL ' "$out/checks.log") failed)"
rm -rf "$box"
echo "log and screenshots: $out"
exit "$code"
