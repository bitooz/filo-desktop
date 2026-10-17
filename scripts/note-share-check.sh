#!/bin/sh
# Checks the Notes tab's share menu (right-click › Share) in the real app with fixture data, offline: a sandbox (its own
# config dir with a made-up stored session, the query cache seeded with two notes, two chats and an agent), the debug
# build started on its own with FILO_OFFLINE=1 and FILO_OPEN=note-share-drive (smoke.rs `run_note_share_drive`), which
# drives it with real clicks and keys posted inside the app. Touches nothing of the user's: no backend, no sign-in, no
# runner (the sandbox has its own run dir), no shortcuts, menu bar item or pet; nothing can be sent.
#
#   1. With a chat list (connected chats, the only mode): drafts typed into chat A, a new chat and the agent; a left click on a
#      note opens it and builds no menu; a right click opens the menu (new chat, the chats, the agent) and no page; the
#      keyboard shares note 1 to the new chat, note 2 to chat A and note 1 to the agent. Each shows its conversation
#      with its draft kept, one chip holding the note's whole text, nothing sent; the earlier ones keep theirs; the
#      chip's × takes the note off and leaves the draft; no note changed.
#
# Usage: scripts/note-share-check.sh [out-dir]   (after `cargo build`)
# Exits non-zero when a run fails; each run's log (PASS/FAIL per check) and screenshots go to out-dir/<mode>/.
set -u
here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$(mktemp -d /tmp/filo-note-share-out-XXXX)}"
mkdir -p "$out"
app="$here/target/debug/filo"
user="user_note_share_fixture"
failed=0

# The sandbox for one run: stored session, prefs and the query cache the screens read offline.
seed() {
  box="$1" mode="$2" user="$user" python3 - <<'EOF'
import hashlib, json, os, time
box, mode, user = os.environ["box"], os.environ["mode"], os.environ["user"]
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
note_1 = "Plan for the week ahead.\n\n" + "\n".join(f"- Item {i}: something to remember about step {i}." for i in range(1, 31)) + "\n\nLast line only the whole note has: tail-7731."
note_2 = "Kyoto in spring, then Osaka.\nBook the ryokan early."
notes = [
    {"_id": "note_fixture_1", "title": "Week plan", "body": note_1, "tags": [], "pinned": False, "updatedAt": now * 1000},
    {"_id": "note_fixture_2", "title": "Trip ideas", "body": note_2, "tags": [], "pinned": False, "updatedAt": now * 1000 - 60000},
]
write(f"{box}/bodies.json", {note["_id"]: note["body"] for note in notes})
query("notes:mine", {}, notes)
chats = [("chat_fixture_a", "Chat about plants", now - 600), ("chat_fixture_b", "Weekly review", now - 3600)]
query("voiceSessions:recent", {"limit": 20}, [
    {"_id": id, "title": title, "startedAt": at, "endedAt": at + 60} for id, title, at in chats])
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
}

run() {
  mode="$1"
  box="$(mktemp -d /tmp/filo-note-share-XXXX)"
  mkdir -p "$out/$mode"
  seed "$box" "$mode"
  FILO_CONFIG_DIR="$box" FILO_ENV=development FILO_OFFLINE=1 FILO_WINDOW_FLOAT=1 \
    FILO_OPEN=note-share-drive FILO_DRIVE_SHOTS="$out/$mode" FILO_DRIVE_BODIES="$box/bodies.json" \
    FILO_DRIVE_NOTE_1=note_fixture_1 FILO_DRIVE_NOTE_2=note_fixture_2 FILO_DRIVE_AGENT_ID=agent_fixture_pip \
    FILO_DRIVE_CHAT_A=chat_fixture_a \
    "$app" >"$out/$mode/run.log" 2>&1 &
  pid=$!
  # A stuck run is stopped after three minutes (and counts as failed).
  (sleep 180; kill "$pid" 2>/dev/null) &
  watchdog=$!
  wait "$pid"
  code=$?
  kill "$watchdog" 2>/dev/null
  grep '^note-share:' "$out/$mode/run.log" >"$out/$mode/checks.log"
  echo "$mode: exit $code ($(grep -c ' PASS ' "$out/$mode/checks.log") passed, $(grep -c ' FAIL ' "$out/$mode/checks.log") failed)" | tee -a "$out/summary.log"
  [ "$code" -eq 0 ] || failed=1
  rm -rf "$box"
}

run shared
echo "logs and screenshots: $out"
exit "$failed"
