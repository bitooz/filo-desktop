#!/bin/sh
# Checks the voice message shortcuts in a chat and in an agent's chat in the real app with fixture data, offline: a
# sandbox (its own config dir with a made-up stored session, the query cache seeded with a chat and an agent), the debug
# build started on its own with FILO_OFFLINE=1 and FILO_OPEN=voice-keys-drive (smoke.rs `run_voice_keys_drive`), which
# presses fn+⌃ and Esc as real key events posted inside the app and holds the open shortcut as hotkeys.rs does. Touches
# nothing of the user's: no backend, no sign-in, no runner, no shortcuts, menu bar item or pet; nothing can be sent.
#
# Usage: scripts/voice-keys-check.sh [out-dir]   (after `cargo build`)
# Exits non-zero when a check fails; the log (PASS/FAIL per check) goes to out-dir.
set -u
here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$(mktemp -d /tmp/filo-voice-keys-out-XXXX)}"
mkdir -p "$out"
box="$(mktemp -d /tmp/filo-voice-keys-XXXX)"
user="user_voice_keys_fixture"

box="$box" user="$user" python3 - <<'EOF'
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
query("notes:mine", {}, [])
chats = [("chat_fixture_a", "Chat about plants", now - 600)]
query("voiceSessions:recent", {"limit": 20}, [{"_id": id, "title": title, "startedAt": at, "endedAt": at + 60} for id, title, at in chats])
query("voiceSessions:pinned", {}, [])
agent_session = "chat_fixture_agent"
query("agents:list", {}, [{"_id": "agent_fixture_pip", "name": "Pip", "picture": "panda-waving", "instructions": "Helps with plans.",
                           "sessionId": agent_session, "unseen": 0}])
query("agents:group", {}, None)
query("users:current", {}, {"userName": "Sam"})
for id, title, at in chats + [(agent_session, "Pip", now - 7200)]:
    query("voiceSessions:detail", {"id": id, "limit": 40.0}, {
        "session": {"_id": id, "title": title},
        "messages": [{"_id": f"{id}_m1", "_creationTime": at * 1000, "role": "user", "content": f"Hello from {title}."},
                     {"_id": f"{id}_m2", "_creationTime": at * 1000 + 1000, "role": "assistant", "content": "Hi, noted."}],
        "hasMore": False, "tasks": [], "calls": []})
    for path in ["voiceSessions:chatTurns", "coding:requestsForSession", "coding:sessionsForChat", "howIWork:proposalsForSession"]:
        query(path, {"sessionId": id}, [])
EOF

FILO_CONFIG_DIR="$box" FILO_ENV=development FILO_OFFLINE=1 FILO_WINDOW_FLOAT=1 FILO_OPEN=voice-keys-drive \
  FILO_DRIVE_AGENT_ID=agent_fixture_pip FILO_DRIVE_CHAT_A=chat_fixture_a \
  "$here/target/debug/filo" >"$out/run.log" 2>&1 &
pid=$!
# A stuck run is stopped after two minutes (and counts as failed).
(sleep 120; kill "$pid" 2>/dev/null) &
watchdog=$!
wait "$pid"
code=$?
kill "$watchdog" 2>/dev/null; wait "$watchdog" 2>/dev/null
grep '^voice-keys:' "$out/run.log" | tee "$out/checks.log"
echo "exit $code ($(grep -c ' PASS ' "$out/checks.log") passed, $(grep -c ' FAIL ' "$out/checks.log") failed); log: $out/run.log"
rm -rf "$box"
exit "$code"
