#!/bin/sh
# Checks that chats and agents left with something typed go on top of their lists (screens/chat/drafts.rs) in the real
# app with fixture data, offline: a sandbox (its own config dir with a made-up stored session, the query cache seeded
# with two chats, the two agents in local/agents.json), the debug build started on its own with FILO_OFFLINE=1 and
# FILO_OPEN=drafts-drive (smoke.rs `run_drafts_drive`), which types with real keys posted inside the app. Touches nothing
# of the user's: no backend, no sign-in, no runner, no shortcuts, menu bar item or pet; nothing can be sent.
#
# Usage: scripts/drafts-check.sh [out-dir]   (after `cargo build`)
# Exits non-zero when the run fails; its log (PASS/FAIL per check) and screenshots go to out-dir.
set -u
here="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$(mktemp -d /tmp/filo-drafts-out-XXXX)}"
mkdir -p "$out"
app="$here/target/debug/filo"
box="$(mktemp -d /tmp/filo-drafts-XXXX)"

box="$box" user="user_drafts_fixture" python3 - <<'EOF'
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
chats = [("chat_fixture_a", "Chat about plants", now - 600), ("chat_fixture_b", "Weekly review", now - 3600)]
query("voiceSessions:recent", {"limit": 20}, [
    {"_id": id, "title": title, "startedAt": at, "endedAt": at + 60} for id, title, at in chats])
query("voiceSessions:pinned", {}, [])
query("voiceSessions:archived", {"limit": 50}, [])
agents = [("agent_fixture_juno", "Juno", "fox-waving", "chat_fixture_juno"), ("agent_fixture_pip", "Pip", "panda-waving", "chat_fixture_pip")]
query("agents:list", {}, [{"_id": id, "name": name, "picture": picture, "instructions": "Helps with plans.", "sessionId": session,
                           "unseen": 0} for id, name, picture, session in agents])
write(f"{box}/local/agents.json", [{"id": id, "name": name, "instructions": "Helps with plans.", "picture": picture,
                                    "createdAt": i + 1.0} for i, (id, name, picture, _) in enumerate(agents)])
query("agents:group", {}, None)
query("users:current", {}, {"userName": "Sam"})
for id, title, at in chats + [(session, name, now - 7200) for _, name, _, session in agents]:
    query("voiceSessions:detail", {"id": id, "limit": 40.0}, {
        "session": {"_id": id, "title": title},
        "messages": [{"_id": f"{id}_m1", "_creationTime": at * 1000, "role": "user", "content": f"Hello from {title}."},
                     {"_id": f"{id}_m2", "_creationTime": at * 1000 + 1000, "role": "assistant", "content": "Hi, noted."}],
        "hasMore": False, "tasks": [], "calls": []})
    for path in ["voiceSessions:chatTurns", "coding:requestsForSession", "coding:sessionsForChat", "howIWork:proposalsForSession"]:
        query(path, {"sessionId": id}, [])
EOF

FILO_CONFIG_DIR="$box" FILO_ENV=development FILO_OFFLINE=1 FILO_WINDOW_FLOAT=1 \
  FILO_OPEN=drafts-drive FILO_DRIVE_SHOTS="$out" \
  FILO_DRIVE_CHAT_A=chat_fixture_a FILO_DRIVE_CHAT_B=chat_fixture_b \
  FILO_DRIVE_AGENT_ID=agent_fixture_pip FILO_DRIVE_AGENT_2=agent_fixture_juno \
  "$app" >"$out/run.log" 2>&1 &
pid=$!
# A stuck run is stopped after three minutes (and counts as failed).
(sleep 180; kill "$pid" 2>/dev/null) &
watchdog=$!
wait "$pid"
code=$?
kill "$watchdog" 2>/dev/null
grep '^drafts:' "$out/run.log" >"$out/checks.log"
echo "exit $code ($(grep -c ' PASS ' "$out/checks.log") passed, $(grep -c ' FAIL ' "$out/checks.log") failed)"
rm -rf "$box"
echo "log and screenshots: $out"
exit "$code"
