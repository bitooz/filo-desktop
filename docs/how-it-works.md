# How Filo works

The parts of Filo in detail, for anyone changing them: where each page's data lives, how turns, the group chat, coding,
artifacts, apps and scheduled work run, and the checks you can run from a terminal. [The README](../README.md) has the
overview and how to build it.

Filo is a native macOS app built with [GPUI](https://www.gpui.rs/) (Zed's GPU UI framework) and gpui-component, with no
account and no hosted backend. Each page calls named backend functions (`users:current`, `agents:list`,
`voiceSessions:detail`…), answered on this Mac. Conversations are answered by your own coding tool: [Claude
Code](https://claude.com/claude-code), [Codex](https://github.com/openai/codex), Muse Code, OpenCode or [pi](https://pi.dev).

```bash
cargo run                         # the app
cargo run --release               # judge speed with this one
cargo test -p filo-local          # chat storage, the database and notes, MCP server, CLI runner (no GPUI)
cargo test --bin filo             # the app's tests, the local backend's included
```

You need one of them (`claude`, `codex`, `muse`, `opencode` or `pi`) installed and signed in. Filo looks for them on your login
shell's PATH, then in `~/.local/bin`, `~/.claude/local`, `~/.opencode/bin`, `~/.npm-global/bin`, `~/.bun/bin`,
`/opt/homebrew/bin` and `/usr/local/bin` (crates/filo-local `engine::find_program`). Debug builds compile GPUI's Metal
shaders at startup (`runtime_shaders`), so no Metal toolchain is needed.

## First launch

The welcome tour shows once (`desktop-welcome.json`): Welcome (every coding tool on this Mac and where it stands), Set
up a tool (only when none is signed in), Make it yours (how coding runs, and whether the Feed and Notes pages show) and
First agent, which becomes your main agent; it ends in that agent's chat. Feed and Notes are optional pages (Settings ›
Pages), off on a new install.
Settings › Welcome tour shows it again. There's no sign-in to Filo anywhere.

Nothing is asked at launch. macOS asks for the microphone on the first voice message. Notifications are asked for the
first time Filo has something to show: a reply that comes in while Filo isn't in front, or a reminder, routine or
check-in made or coming due. A short note in the window says why, then macOS asks (once; Settings › Notifications can
ask while it's still unanswered). The background scheduler is installed with the first thing scheduled (below).

Filo sends anonymous usage counts (an install event, then one event a day it's opened, with a random install ID)
to PostHog. To turn them off, put `export FILO_TELEMETRY=0` in `~/.zshrc` and reopen Filo. [telemetry.md](telemetry.md) lists
exactly what's sent.

**Report a problem** (Settings › Help, or Report beside a failed reply or coding task) checks this Mac first: the
build, macOS, Claude Code and Codex (old versions and sign-in included, with one-click fixes), a tiny test reply, disk
and network. It then sends what it found to Filo's backend, with the error and the CLI's output, but only when you
click Send. Chat text, home-folder paths and keys are left out. [problem-reports.md](problem-reports.md)
lists what's sent.

## The backend on this Mac

`src/backend.rs` keeps the API every page uses (`subscribe`, `call`, `call_in`, `call_raw`, `fire`) and hands each call to
`src/local_backend.rs`, which answers from files under `~/.config/filo/local` (or `FILO_CONFIG_DIR`). A live query runs
again whenever the local data changes.

| File | What |
| --- | --- |
| `local/filo.db`: `profile` | Your name and your main agent (`mainAgentId`, see "The main agent" below) |
| `local/filo.db`: `agents`, `agent_pictures` | Your agents: name, instructions, picture, colour, the model they answer with (picked in the New agent form, used in every chat with them; none follows the default engine), their conversation. Fresh data starts with Filo (a general assistant) as one; while it's the only one and was never talked to, the welcome removes it once you've made your first agent (`agents:removeStandIn`). Once the file exists nothing is added again, except Filo coming back as the main agent when none was picked (or the one picked is gone). A new agent needs a coding tool plugged in on the Coding page (Claude Code, Codex, Muse Code or OpenCode; `agents:canCreate`); existing agents keep working either way |
| `local/chats/<id>/` | Filo's chats (`chat.json` + `messages.jsonl`, crates/filo-local's format) |
| `local/agents/chats/<id>/` | Each agent's conversation, with the agent as its instructions |
| `local/filo.db`: `seen`, `chat_marks` | How far each conversation is read; chats pinned or archived |
| `local/filo.db` | Filo's SQLite database: the rows above, notes, memories, reminders, routines and check-ins, the agents' group chat (below), the Feed, coding, apps, artifacts' details, versions and chat links. An older Filo's `agents.json`, `profile.json`, `seen.json`, `sessions.json` and `pictures.json` are copied in once and renamed `*.migrated` (crates/filo-local `app_data`) |
| `local/backups/` | The last 3 daily copies of filo.db, made the first time it's opened each day (crates/filo-local `backup`) |
| `local/skills/<name>/` | Your skills, a folder each (below) |
| `local/artifacts/<id>/` | Each artifact's pages: `v<n>.html` per version (never changed), `index.html` the current one |
| `local/workspace/pages/`, `local/agents/workspace/pages/` | The HTML files agents build and publish as artifacts (any path on the Mac works; these are where the artifacts skill puts them) |
| `local/scheduler.lock`, `local/scheduler.log` | Which process runs the schedule now; the background helper's log |
| `local/coding/shares/<task>-<round>.json` | A shared coding result's link on filo.im and the token that unshares it (below) |
| `local/artifacts/<id>/filo-link.json` | A shared artifact's link on filo.im, the version it shows, and the token that updates or unshares it (Artifacts, below) |
| `local/failures.jsonl` | The latest 30 failed replies and coding rounds, with the CLI's last stderr lines, for a problem report (below) |
| `local/workers/`, `local/coding.lock` | The turns and coding rounds running in workers (a file each, locked while it runs) and their log; which Filo window runs this Mac's coding |
| `local/awake/` | Only from older versions: lid mode's state is now this Mac user's, in `~/Library/Application Support/Filo/awake/` (below) |
| `desktop-local.json` | The engine that answers where a chat didn't pick its own: picked in a chat's header, or the tool the welcome's first agent runs on (an agent's chat uses the agent's model) |

A message starts a turn in a worker, a process of its own (`filo --worker turn …`, the app's own binary, in a session of
its own; `local_backend/workers.rs`) that runs crates/filo-local `turn::run`: the CLI runs with Filo's MCP server
plugged in (`filo --local-mcp …`) and replies with its `send_message` tool, which appends to the conversation's file.
Its `files` (up to 5 full paths on this Mac) go with the message (attachments.rs `sent`), and show as the user's
attachments do (pictures and videos as tiles, the rest as file chips), in its chat or in the group. A picture or video
up to 500 MB is kept as Filo's frozen copy in `local/sent/` (an APFS clone, else a copy, never a link), so the message
keeps showing it as it was sent while the agent draws v2 over the same file; that folder isn't among those a turn reads
without asking (turn.rs `readable`), and the agent is told the path it sent from (`told_at`, also in a new session's
seed and the group's transcript). Any other file, and a bigger video, shows where it is, with no copy (`in_place`: the
canonical path, so a link names its target, a macOS bookmark and its inode), and the agent is told to keep those
somewhere lasting, like its workspace. When such a file is no longer at its path, its bookmark finds it if it was
renamed or moved on the same disk and it's the same file (its inode; not one in a Trash) (`moved_to`), and the new path
is kept: in `chats/<id>/moved.json` beside a chat's messages (store.rs `find_moved_files`), in the post's row in the
group (group.rs `find_moved_files`); a file not found is looked up again less and less often. A file neither finds shows
as "No longer there", a tile or a file row with nothing to open. A path that isn't a full path, isn't there or is a
folder sends nothing and says why. A file named as a picture whose first bytes aren't one's is sent as a plain file.

Filo's copies (the user's in `local/attachments/`, agents' pictures and videos in `local/sent/`: attachments.rs
`own_copy`) are deleted with their chat when an agent is deleted (local_backend/shared.rs `delete_chat_files`, those no
other chat or the group has too), and from a chat's Shared list (the agent's profile, Group settings;
screens/chat/shared.rs). A right-click on a picture, video, file or recording opens its menu (`item_menu`): Show in Chat,
Show in Finder, Edit (a picture, in the chat's image editor), Select, and Delete (`chatShared:deleteFiles`) for Filo's
own copies and voice messages' recordings, none for a file shown in place. ⌘-click and Shift-click pick several; while
picking, each file Filo can delete shows a circle (a tile's at its top right, a row's at its end; `check_mark`), filled
with a tick once it's picked, and picked files show their size. The menu and the Delete key act on all of them, and Esc
lets them go. A delete that
fails says why in a toast. Select starts picking (`selecting`): a plain click then picks or lets go of a file instead
of opening it, until the last one is let go or Esc. Show in Chat asks how far back the message is (`chatShared:depth`, counted as
`voiceSessions:detail`'s `limit` counts, the group's by `seq`), and the chat loads back that far before it scrolls to
the message and flashes it (conversation.rs `reveal_message_at`, past `MAX_LOADED` when it's older; a page further
when newer messages came meanwhile). `FILO_OPEN=shared-show-drive` with `FILO_SHOW_CHATS=<session id>,…,group` checks
it on a copy of the user's data: each chat's newest, middle and oldest file.
Recordings are deleted with attachments.rs `delete_recording`, which notes each in
`local/attachments/deleted-recordings.txt`, so a recording just sent and not saved yet isn't taken for one deleted. A
message whose copy or recording was deleted shows it as "No longer kept". `FILO_OPEN=shared-delete-drive` clicks
through it in a test window. generate_images writes only new files (`O_EXCL`, not over a file or a
link at its path).
While turns run, a watcher notices the files growing and the conversation updates. A turn's state (working, done,
failed) is read from the file, so it survives restarts. Stop asks the worker to end the CLI. In an agent's conversation
the CLI is told it's that agent, with the agent's instructions.

While a turn runs the watcher looks every 300 ms, and any change runs every live query again (some 25 of them: each
open chat's messages, turns and coding rows, the Agents list, unseen replies). So that this stays cheap with long
chats (an agent's `messages.jsonl` grows to several MB):

- `Store::shared_messages` (crates/filo-local store.rs) keeps each chat's messages as parsed; a later read parses only
  the lines added since (files are only appended to), and a file written anew (shorter, or other bytes where it was
  read up to) is read whole again. `messages` is the same, copied. The chat's page, turns, the Agents list, unseen
  replies and the chat list read it without copying.
- A chat's coding rows read each task's latest step alone (`coding::latest_event_not_from`), not its whole history,
  with the per-task statements kept prepared.
- Screens subscribed to the same query (`agents:list`, `voiceSessions:unseen`) share its answer after a change
  (backend.rs `Shared`): the first runs it, the others take its answer, at the same change and within a second.

`FILO_OPEN=chat-timing` (`dev`) measures this on a copy of the user's data in a `FILO_CONFIG_DIR` of its own (the
copy's routines, coding tasks and group turns don't run): with `FILO_TIMING_CHATS=<session id>,<session id>` it opens
each chat and switches between them (time until its messages are in the list, and drawn), then refreshes with nothing
new and with a reply appended, logging each live query's time and CPU (backend.rs `query_times`), and runs one
refresh's queries one at a time. `FILO_TIMING_DUMP=<file>` instead saves what the chats' queries answer and quits, for
comparing two builds.

Drawing a long chat stays cheap the same way. GPUI draws a view again when it's notified and draws the views around it
with it, so:

- The Agents list is a view of its own kept as drawn (agents.rs `AgentList`, `AnyView::cached`), drawn again when the
  Agents screen changes, a chat's draft or reply starts or ends, and every half minute (its times). Scrolling a chat
  drew all its rows every frame, about half of each frame. Drives draw it afresh each frame (`smoke::cache_views`).
- Spinners and working rings turn on the GPU (`ui::turning`, vendor/gpui `Svg::with_spin`): at each of the ticker's
  steps the window presents its last frame again with them turned further, and no view is drawn. Drawing the views they
  were in, a working ring in the Agents list (even one scrolled out of sight) drew the list and the chat twelve times a
  second, about a fifth of a core in a debug build while any agent worked; now about 6%, most of it the desktop pet. The
  ticker still draws again the views of what moves otherwise (the pet's bob, the thinking dots, pictures coming into
  view), only those views and not one kept as drawn beside them; a step of one the Agents screen draws leaves the list
  as it was.
- A page of older history is taken in by splicing only the rows that changed (feed_rows.rs `splices`, the rows both
  have at the start and the end left out of the comparison), and the message on screen keeps its place
  (`sync_list_anchored`): only a message's row holds the place (conversation.rs `holds_place`), not the "loading" row,
  a coding card or a time mark, which stay on top or move up above the same day's older messages.
- Dragging the timeline up near the oldest messages loaded asks for the page before them, and a page that comes in
  under the drag leaves the chat where it is: the drag goes on from there (timeline.rs `from_here`), older messages up
  the strip above the pointer, newer below it.

`FILO_OPEN=chat-scroll-timing` (`dev`) checks this on a copy of the user's data as `chat-timing` does: with
`FILO_TIMING_CHATS=<session id>` it opens that chat at its newest message and turns the wheel up through all its history
(`FILO_SCROLL_STEP` points a frame, 60), or with `FILO_TIMING_SCRUB=1` drags the timeline fast up and down across the
pages as they load. It logs each frame's draw and the main thread's CPU in it, the time between frames, the CPU used,
how long each page took in, whether each page kept the message on screen, any jump against the input and any frame
with messages out of order. `FILO_DRIVE_VIDEO=<file.mp4>` records the window from its own frames meanwhile (window_shot.rs
`Recording`, through `ffmpeg`; it slows the window), and `FILO_TIMING_TRACE=1` logs every frame's place.
`FILO_TIMING_IDLE=<seconds>` instead leaves the chat alone that long and logs the CPU used (with `ZED_MEASUREMENTS=1`
GPUI logs each frame it draws; with `FILO_DRIVE_VIDEO` every frame presented is recorded).

A chat runs one turn at a time. A message sent while one works goes into it (crates/filo-local `steer`, turn.rs
`Steering`): the turn's CLI runs as a session it's kept talking to, as a coding task's does (coding_session.rs), so the
message reaches it at its next step: one more user message on Claude Code's stdin, a `turn/steer` on Codex's
app-server (`codex app-server`, which Codex chats run on; `codex exec` takes nothing mid-turn) and on Muse Code's
`muse serve` (`expectedTurnId`, the running turn), a prompt delivered as `steer` on OpenCode's server (in its inbox
until `session.inbox.delivered`), a `steer` on pi's RPC (queued until pi delivers it as a user message after the
tool calls under way). Every engine's events go through one piece of code (coding_session.rs `Notes`): a
message is the turn's once the CLI has it (Claude Code replays it, the steer is accepted, the inbox or pi delivers it) and
has done something since, or once a turn it was started with is under way. A CLI that has it but ends its turn first
hasn't answered it: Claude Code and OpenCode take it in a turn of their own, which is waited for (10 s); for Codex and
Muse Code a new turn in the same session starts with it, and on pi, whose run settled with it still queued, it's
taken back (`clear_queue`) and the next prompt carries it. A steer refused mid-turn is offered again every 2 s; refused
for 6 s, the running turn is interrupted (`turn/interrupt`, `POST …/interrupt`, pi's `abort`) and a new turn starts with the message,
told it was stopped, so it carries on after what it already did and posted. One refused because the turn had just ended
(Muse Code's `already_terminal`), or that never reached the CLI, interrupts nothing. The message shows in the chat as
sent, and the replies from then on answer it. Its own turn waits behind the running one and ends without running once
that one has ended (`chats/<id>/steered.json`); if that turn failed, or was killed, the message's turn runs after all,
answering it in the session the failed one left. It waits and runs after the turn as before for what Filo's app sends
on its own (a routine's run, a coding task's news, the group's asks), for a message the user sends an agent's own chat
while a turn the group gave it runs there (that one answers in the group), and when the CLI doesn't take it in time.
Messages go in in the order they were sent.

A chat whose session can't be resumed (it went cold, its engine changed, or the CLI lost it) starts a fresh session
seeded with its own newest messages, word for word (crates/filo-local `reseed`, turn.rs): nothing is summarized, and
the chat marks the place "New session from here, with the recent messages". So does one nearly full, idle or not: within
150k tokens of its model's context window (850k of 1M; 325k of the 475k Codex counts for gpt-6.1-sol), or past half of
a smaller window (100k of 200k: `compaction::reseed_due`), ahead of where Claude Code (about the window less 33k: 967k,
167k) and Codex (90% of its model's window, 450k there) would summarize it themselves. A session is full only once it
has grown a quarter of the window past its first call (since it started, or since its CLI last compacted it), so one
whose first call (the instructions, tools and notes, and the seed) is big on a small window isn't started afresh again
at its next turn; and always 50k short of the window, so on 200k it starts afresh by 150k whatever it started at. On a
small window the seed is a tenth of it (20k to 30k tokens on 200k, 40k to 60k otherwise: `reseed::sizes`). The window is
the one its CLI said for the model on an earlier turn of any chat (Codex's rollout, Claude Code's `modelUsage` for the
model its `init` names, not a side model's), kept for the whole app by engine and model (`agents/context-windows.json`,
`compaction::Windows`, changed under a lock since each turn runs in its own process); else 1M for a current Claude
model (`compaction::known_window`). A model neither knows is taken for 200k only right after the chat switched to it;
on the model its session already ran on it isn't judged full until its CLI has said the window, after that turn. (T3
Code and OpenCode leave a full session to the CLI's own summary instead.) A
conversation's turns are told where its whole history is (`chats/<id>/messages.jsonl`), so the exact words are a Read or Grep away. A coding task's session that
went cold is compacted before it's resumed (crates/filo-local `compaction`): when it has been idle longer than the hour
Claude Code and Codex keep its prompt cache on this Mac, and is over 150k tokens, its CLI first summarizes it (Claude
Code's `/compact`, Codex's `thread/compact/start`), told to keep what the user asked for, the decisions made and the
open work. The task's thread says "Catching up on the session so far", then how much smaller it got. Sessions in use
grow freely.

Quitting or restarting Filo leaves the workers going while **Keep coding after Filo quits** is on (the Coding page,
under Computers; on by default): the reopened app picks up the ones still running (each holds a locked file in
`local/workers/`), so their turns show as working and their tasks as running, with Stop working again, and it gets
their results as they land (a coding round's report goes to its conversation then). With it off, quitting stops them as
Stop would. A crash, a `kill` or the updater's restart never stops them. Routine and check-in turns and the group's
turns still run in the app, and quitting stops them. Only one Filo window runs this Mac's coding (`local/coding.lock`):
another opened on the same data (a scripted check) waits for it rather than stopping its tasks or running them again,
and the one that has it also picks up workers another process started on the same data folder (`filo --check coding:startTask …`).

**Keep Mac awake while tasks run** (Coding › Computers, off by default) prevents idle system sleep during coding
rounds, routines and chat replies, including group replies. Each process running work holds an IOKit assertion until
its work ends; macOS releases it if the process crashes. The display can still sleep. Workers read setting changes
within two seconds.

**Keep running with the lid closed (plugged in only)** is off by default and needs keep-awake on. Enabling it asks
for an administrator's password to install access to exactly two commands: `pmset -a disablesleep 1` and
`pmset -a disablesleep 0`. Filo disables sleep only while work holds an assertion, on external power, below critical
thermal state, and for 12 hours at most in one go. It restores sleep when the last hold ends, or within two seconds
after switching to battery, a critical thermal state or the 12 hours; with the lid closed by then, the Mac goes to
sleep. Sleep is the whole Mac's, so its journal, lock and holders' files are this user's, whichever Filo runs:
`~/Library/Application Support/Filo/awake/` (outside the folders macOS guards, so launchd can reach them). A launchd
backstop (`im.filo.desktop.lid-backstop`) checks every minute and at login, and restores sleep after all holders
crash, on battery, or after 12 hours; turning the option on waits until it has run once, and the Coding page warns
when it stops running or a `pmset` keeps failing. `disablesleep` is kept in the system's power settings, so it
survives a restart: the backstop restores it at login, but nothing of Filo's runs at the login window before that.
Sleep already disabled by the user is left alone. A Filo with its own data folder (`FILO_CONFIG_DIR`) never uses lid
mode unless `FILO_LAUNCH_AGENT=1`. Turning lid mode off restores sleep and removes its backstop and access, with a
second password prompt (which also restores sleep if `sudo` stopped allowing it); nothing is removed while another
Filo's tasks keep sleep off. If removal is cancelled or setup is incomplete, **Remove access** stays available, even
with the main keep-awake switch off. Before deleting Filo, run `Filo.app/Contents/MacOS/filo --uninstall-lid` (or
`./target/debug/filo --uninstall-lid`): it turns the option off, restores sleep and removes the backstop and
`/etc/sudoers.d/filo` (asking for the password).

When the Mac sleeps during work keeping it awake, Filo waits for the user to return before notifying them of the
duration, the sleep reason when macOS supplies a known one, and how many interrupted tasks are still running. Dark
wakes are combined into one interruption notice.

Your skills (the Skills page, yours and the ones Filo learned) are folders in `local/skills/<name>/` (a `SKILL.md` and
whatever else it holds), the only copy (crates/filo-local `skills`). Each is also a real Claude Code and Codex skill: a
copy of its folder in the folder the CLIs run in (`~/.config/filo/local/workspace` and `…/agents/workspace`), in
`.claude/skills/<name>/` and `.agents/skills/<name>/`, made when a skill is saved, edited or removed and again when
Filo opens and before each turn. Filo's own skills go beside them (crates/filo-local `skills::FILO_SKILLS`):
`desktop-pet`, and `artifacts` with its `template.html` and `check_page.py` (see Artifacts). A chat turn sees only
those and the ones for PDFs, Word, Excel, PowerPoint and charts: Claude Code through its `initialize` request's skills
allow-list (pdf, docx, xlsx, pptx, dataviz, desktop-pet, artifacts), Codex with its own skills turned off, every other
skill folder's named off, its primary runtime's four document plugins on and Filo's own `dataviz` skill. A message
naming skills (`@skills:weekly-report` or `/weekly-report`) carries their instructions with it. One naming `@browser`
or `@computer` (crates/filo-local `mentions`, which the composer's "@" list takes them from: the browser always, the
computer while its switch is on) gets a line after it on which tools to use: Filo's browser's `browser_*`, found with
tool search when they aren't loaded, or `use_computer`.

What your agents remember about you (the Memories page) goes with every turn too, in Filo's chats and the agents': each
message is sent with the memories in front of it (newest first, up to 8000 characters, each with its id), so one added
or edited on the page counts from the next message. The MCP server's `save_memory`, `update_memory`, `forget_memory` and
`search_memories` tools let the coding tool keep them; a memory saved in a chat says so ("conversation", the chat's
id) and is "Not yet confirmed" unless you said it outright. While a turn runs, the watcher also notices the database
changing, so the Memories page shows a memory as soon as it's saved.

Moved to this Mac (`local_backend::MOVED`):

- **Profile:** `users:*` (current, name, `users:setMainAgent`).
- **Agents:** `agents:list/create/update/remove/openChat`, `agents:scheduledCount`, and the group chat: `agents:group`,
  `agents:openGroup` and the conversation functions for its session (see "The agents' group chat" below).
- **Conversations (agents'):** `voiceSessions:detail/chatTurns/sendMessage/startChatTurn/retryChatTurn/dispatchChatTurn/stopChatTurns/markSeen/ensureTextSession/setReasoning/setModel/reactToMessage`, and what was shared in a chat (`chatShared:list`).
- **Chat list and counts:** `voiceSessions:recent/pinned/archived/archivedCount/setPinned/setArchived/unseen/working`.
- **Notes:** `notes:mine/getMine/createMine/updateMine/setPinnedMine/deleteMine`, plus `notes:search` (`{query, tag?}`)
  and `notes:tags` (each tag with how many notes have it). Every conversation's coding tool has them too,
  through the MCP tools in crates/filo-local `notes_mcp`: `list_notes` (`query`, `tag`), `read_note`, `create_note`,
  `update_note` (only what's given changes) and `delete_note` (kept 30 days, as from the tab).
- **Memories:** `memories:list` (`{paginationOpts: {numItems, cursor}}`, newest first), `memories:create` (`{content,
  type?}`), `memories:update` (`{id, content, type?}`) and `memories:remove` (forgets it for good).
- **Feed (`local_backend/feed.rs`, its own `FEED` list):** `feed:mine/postMine/topicsMine`,
  topics (`addTopicMine/renameTopicMine/updateTopicMine/removeTopicMine`), posts (`setLikedMine/setSavedMine/hideMine/unhideMine/
  remindMine/askAboutPost`), settings (`settingsMine/updateSettingsMine`), checks (`refreshMine/refreshTopicMine`) and
  `voiceSessions:startChatTurn` (the Feed's Ask panel). See "The Feed" below.
- **Artifacts (`local_backend/artifacts.rs`, its own `ARTIFACTS` list):** `canvasArtifacts:list/get/listVersions/
  getVersion/markOpened/publish/restoreVersion/sendArtifactMessage` (`publish` shares to claude.ai while Share to
  claude.ai is on), `artifactShare:settings/setOn`, and `glances:get/saveToArtifacts`. See "Artifacts" below.
- **Skills:** `skills:list`, `skills:save` (creates a skill or replaces the one with its name), `skills:import` (a
  skill's folder or SKILL.md, copied in) and `skills:remove`.
- **Scheduled (also the pet's and menu bar's reminders):** `reminders:myReminders/createMine/completeMine/snoozeMine/
  cancelMine`, `routines:myRoutines/myRoutine/agentRoutines/createMine/updateMine/deleteMine/runNowMine`,
  `agenda:myWakes/addWakeMine/cancelWakeMine` (`local_backend/scheduled.rs`, its own `SCHEDULED` list).
- **Coding (`local_backend/coding.rs`, its own `CODING` list):** `coding:overview/tasks/projects/thread`, the settings
  (`setPlugged/setToolMode/setToolModel/setToolEffort/setWorkspace`), tasks
  (`startTask/stopTask/continueTask`), projects (`addProject/removeProject/refreshProjects`) and `coding:enabledTools`
  (each tool enabled, installed, signed in and ready, for anything that needs one). A conversation's coding tasks
  (`local_backend/handoff.rs`, its own `HANDOFF` list): `coding:sessionsForChat/requestsForSession/answerRequest/
  roundResult/workingChats/trayTasks/shareResult/unshareResult`. See "Coding" below.
- **Apps (`local_backend/apps.rs`, its own `APPS` list):** `apps:list/connectPreset/add/update/setEnabled/setEngine/setAgentAccess/
  setAllAgents/sessionAgent/setToolEnabled/setToolsEnabled/setApiKey/connect/cancelConnect/check/refresh/
  signOut/remove`, and "Your computer"'s
  `computer:appsOn/setAppsOn` (kept in `local/computer.json`). See "Apps" below.
- **Voice messages (`local_backend/voice.rs`):** `voiceRecordings:sendLive/createUpload/send`, in any conversation and
  the group, and the upload route as `Backend::upload_voice`. See "Voice messages" below.
- **Custom avatars:** `customAvatars:list/picked/remove` (the characters agents drew, crates/filo-local `characters`).

A file sent with a message is kept on this Mac by `Backend::save_attachment`. Anything else answers "This isn't on this
Mac yet (<function>)", and the page shows that as its error.

## The main agent

One of your agents is the main agent (`mainAgentId` in filo.db's `profile`, crates/filo-local `main_agent`): Filo until
you pick another from an agent's menu on the Agents tab (right-click its row, or "…" in its chat's header: Make
main agent); the main agent's row has a Main badge. It leads the group chat (what names no one comes to it when the
latest poster passes it on), answers Ask on Feed posts
unless Feed settings picks another agent, watches the Feed's topics carried over from before, and owns what belongs to
no agent: reminders, routines and check-ins made on the Scheduled page or in Filo's own chats are its to see and change
from its conversation, and a deleted agent's are given to it. It can't be deleted while it's main (`agents:remove`
refuses): deleting it asks who takes over first. When no main agent was picked (data from before) or the one picked is
gone, Filo is the main agent, brought back as an agent if it was removed (`ensure_main_agent`).

Its name, picture and colour are its own, set in its form like any agent's: what's no agent's shows them (the owners on
Scheduled, a coding task's thread, the pet's tray), and the desktop pet is its picture unless Settings › Desktop pet
picks another character (an animal or one of the avatars agents made; a photo or a helper's picture has no poses, so
the pet is the panda then).

## The agents' group chat

The Group row on top of Agents is one conversation with your agents, kept in `local/filo.db`
(crates/filo-local `group`: `group_chat`, `group_messages`, `group_turns`), so it's there after a restart. The Agents
tab makes it (`agents:openGroup`) once there's an agent; the Filo agent fresh data starts with is Filo in the group
(speaker `filo`, its instructions and its own chat), not a second member, and Filo is in it while it's an agent. The main
agent leads it, listed first.

- **Who answers:** whoever your message names with @Name. A message naming no one goes first to whoever posted the
  group's latest message, who answers it if it's for them and otherwise passes it on to the main agent (straight to the
  main agent when there's no such member); `@everyone` reaches every member at once. Whoever speaks can name others
  (`@Chef, what's for dinner?`), and they answer next in the same thread, up to 8 turns a thread.
- **Turns:** queued in the database and run one at a time by the app (`local_backend/group.rs`; the background scheduler
  leaves them alone), so each speaker reads what the others just said. A member's turn runs in its own chat, on that
  chat's session and engine like any message there: a message with why it's speaking and the group's latest 30
  messages, with your memories and skills as in any turn (only a speaker with no chat of its own runs on a fresh
  session). It posts with `send_message`, which the MCP server (`--group-speaker <id>`) adds to the group in its name;
  the post reads in full in the member's own chat, under "Posted in the group". One only asked
  to chime in may stay quiet; one you wrote to has to answer, else the turn fails and can be retried.
- **Own chats:** a member's message naming others leaves a line in their own chats ("Asked Chef in the group: …",
  "Called by Filo in the group: …") that opens the group at that message.
- **Asking from a chat:** in Filo's chats and each agent's own conversation, the MCP server's `ask_agents` (offered while
  there's someone to ask; not in the group, where members just write @Name) posts the message in the group in the
  asker's name (Filo for Filo's chats and the Filo agent's), as a thread of its own, and queues whoever it names
  (crates/filo-local `group::ask`, kept in `group_asks`). The asked agents' chats get the "Called by …" line. When
  nobody is left to speak in that thread, the app gives the asker a turn back in the chat it asked from, with what the
  group said, to tell the user (it waits while that chat has a turn running). The group's turns run while the app is
  open, so an ask made while it's closed is answered when it opens.
- **What a turn makes:** an artifact made, changed or opened in a group turn is a card in the group (not the member's
  own chat), and its page's "ask Filo" goes to the group. A check-in a member plans there comes back as that member's
  turn in the group.
- Stop ends the answer under way and drops the turns waiting. Quitting stops the running turn; when Filo opens again
  it's marked failed (retry runs it), and turns still waiting run. Removing an agent drops its waiting turns.

## Voice messages

Hold the mic in a chat (or click it), or hold the voice shortcut in any app, and talk: the words show as they're heard,
and letting go sends them as your message. The turn reads them like typed words, and a new chat is titled by them. The
recording stays on this Mac (`local/attachments/<id>.m4a`, named by the message's `voice`, or `group_messages.voice` in
the group) and plays on the message's bubble and under an agent's Shared › Voice. From another app, a message that
starts with an agent's or the group's name ("Milo, …") goes there (`voice_route.rs`), and the pet shows the words and
who they go to while you hold.

In Filo's window, fn+⌃ held in a chat records there (`AppShell::on_modifiers`, `screens/chat/voice.rs`). It starts at
once: the recording row shows on the next frame, and the microphone opens behind it (`VoiceState::Opening`). macOS takes
0.1–0.3 s to open it, and the window doesn't draw meanwhile. A press under 0.3 s is dropped when it's let go. So is one
with another key pressed while it's down, as that's part of another shortcut. When it can't record (text in the box, no
microphone access), it says why only after the keys have stayed down 0.25 s, so brushing them shows nothing. Each
recording logs how long it took to start ("voice: recording … ms after it was asked for"). Like typing,
it needs GPUI's view to have the window's keyboard. A web view over it (a glance, an artifact page, the note editor)
takes the keyboard when it's clicked. A click on Filo's own content takes it back, and so does the next frame once the
page that had it is hidden or closed (vendor/gpui/FILO.md). `FILO_OPEN=page-keys-drive`, in a test window, clicks a page,
then hides it or takes it out, and checks that typing and fn+⌃ still reach the main agent's message box.

The words come from Filo's relay at dev.filo.im (`src/live_transcription.rs`; the relay itself isn't part of this
repository). While you talk, the microphone's audio streams to it as 16 kHz PCM (`recorder::MicStream`: an audio queue
on the input, next to the recorder's own; AVAudioEngine's input set up a device pairing the microphone with the
speakers, which took ~0.5 s each time and stopped when the output changed) and
on to Microsoft's MAI-Transcribe-2-Streaming, and the text comes back. It's the one part of a conversation that leaves
the Mac, and it needs no account: the relay is told only that this is Filo for Mac and which build. It has a daily
allowance per network and one for every Mac together, both back at 00:00 UTC. When they're used up or you're offline,
the composer says so and when voice messages are back (from another app, the pet and a note in Filo's window), and a
recording made in a chat stays there to send again. If the live stream fails midway, Send turns the saved recording
into text through the relay instead (`voiceRecordings:send`). In `dev` builds, `FILO_WORKER_ORIGIN` points Filo at another relay.

## Sounds

An audio file (mp3, m4a, wav, aac, ogg, flac and the like) in a chat's or the group's message, or saved by a coding
round, plays where it is, in one row shared by both (`src/audio_row.rs`): a round play button, its waveform (the part
played in the accent colour; a click or a drag along it goes there), the time played and its length, and a speed button
(1×, 1.5×, 2×, for every sound), with the file's name under them. A click anywhere on the row (or Tab) gives it the
keyboard, and Space then plays or pauses it (an accent ring shows it after Tab, never after a click); a letter typed
while it has the keyboard goes back to the window's message box while one is drawn (it says so as it draws), letter and
all, and keys typed meanwhile follow in order. It plays from its file on this Mac, so a big file attached without a copy
plays from where it is. Which files are sounds is one check, `media::is_audio` (the content type, or the name by
crates/filo-local `attachments::content_type_for`, the one table of types), used by the chat, the result page, Shared
and artifact pages' files.

Sounds play through the video player's AVFoundation player, with no picture (`video_player.rs` `open_audio`), and one at
a time across the app (`media.rs` `Audio`): starting one stops the last and pauses a video playing in the viewer, a
video playing pauses the sound where it is, and nothing plays while the microphone records (`recorder::recording`).
Voice messages play the same way. While a sound plays, only the views that show its row are drawn again, and only when
its clock or its played part moves. A sound ends when AVFoundation says it played to its end (a FLAC can, before its
length) or the player stops within a moment of its length; stopped anywhere else, it stays paused where it was. The
waveform is made on two threads of their own: macOS's `afconvert` turns it into 4,000 8-bit samples a second in a
temporary file of its own (stopped after half a minute plus a second for each 2 MB, three minutes at most), read in
pieces, and each bar is the loudest of its stretch; one whose row isn't drawn by its turn is skipped. It's kept in a
temporary folder and in memory by the file's path, size and time (`media::file_key`, shared with the videos' posters),
looked up on a thread of its own and again every ten seconds, so a changed file is made again; one that couldn't be read
isn't tried again until it changes, and one that ran out of time is tried once more after five minutes (an hour of mp3
takes about five seconds the first time). Its length is the samples', since AVFoundation's can run past a sound's end.
Artifact pages play their own `<audio>` in their web view (`screens/artifact_media.rs`), which also draws AVIF pictures
the rest of the app can't.

`FILO_FIXTURE_AUDIO=<file>,<file>` with `FILO_OPEN=coding-cards` ends that chat fixture with those files sent, played
muted (every test window's sounds are), and the plan's `audio` step logs the sound playing or paused.

## The Feed

The Feed is an optional page (Settings › Pages, off on a new install; Notes too). Topics, posts and the Feed's settings are in `local/filo.db` (crates/filo-local `feed`: `feed_topics`, `feed_posts`,
`feed_settings`; `feed_runs` is left from the Feed's old check, unused). The Feed is filled by agents only: posts are
published through the MCP tools in crates/filo-local `feed_mcp` (`publish_feed_post`, `update_feed_post`,
`list_feed_posts`, `remove_feed_post`, `list_feed_topics`, which says who watches each topic). They act on this Mac's
Feed only. Each post says who shared it (the agent, with its picture).

- **From a conversation:** every Filo chat and agent conversation has the tools; a post says who shared it (the agent,
  or Filo) and keeps the chat it came from. A link is in the Feed once; publishing it again names the post that has it.
- **From a coding tool outside Filo:** `filo --feed-mcp` serves just the Feed's tools on stdio (`--author
  <name>` for who posts say shared them, `--db <filo.db>` for another database):
  `claude mcp add filo-feed -- <path to filo> --feed-mcp --author "Claude Code"` or
  `codex mcp add filo-feed -- <path to filo> --feed-mcp --author Codex` (`filo --feed-mcp` follows `FILO_CONFIG_DIR`;
  `filo-local feed-mcp` always opens ~/.config/filo/local's database unless given `--db`).
- **Topics:** each one is watched by an agent on a routine of its own ("Feed: <topic>", crates/filo-local `schedule`,
  `feed_topics.routine_id`). Adding a topic in Feed settings picks the agent (the main agent by default) and how often
  (every 30 minutes to 12 hours, or daily at 8:00), and sets up the routine; each topic's row shows its agent and
  interval, with pickers to change either. A run is a turn in the agent's own conversation that searches the web and
  publishes up to 5 posts on the topic, then says in one line what it published. Runs of an agent's topics wait for one
  another (and for a turn under way there). Renaming a topic renames its routine, removing it deletes the routine; a
  topic whose routine was deleted (on the Scheduled page, or with its agent) says nobody watches it until an agent is
  picked again. Topics from when the Feed ran a check of its own went to the main agent, on that check's schedule.
- **Refresh:** Refresh, a topic's Refresh and Check now run those routines now (Refresh with `ifStale` leaves ones that
  ran in the last 10 minutes); the routines run on their own too, from the schedule loop (the app, or `filo
  --scheduler` while it's closed), so they use your CLI's usage. The Feed's "Updated…" line and a topic's last refresh
  come from its routine's runs.
- **Ask:** Ask <agent> on a post goes to an agent's own conversation: the one Feed settings picks (the main agent
  unless another is). On the post page it sends there and opens it; on the Feed it opens that conversation beside the
  posts, whose header switches the chat to another agent. The post goes to the engine with the message, not into the
  chat.
- **The user's marks:** like, hide (kept 12 hours as an undo row), save (a note tagged `feed`; unsaving trashes it) and
  Remind me on an event (a reminder 30 minutes before).

## Coding

The Coding page runs your own Claude Code, Codex, Muse Code, OpenCode or pi on this Mac; everything is in `local/filo.db` (crates/filo-local
`coding`: `coding_settings`, `coding_tools`, `coding_projects`, `coding_tasks`, `coding_events`).

- **Settings** are saved as they change: each tool plugged in or not (on until you turn it off), its mode, model and
  reasoning level; where tasks work ("Own worktree" or "Your checkout"); Keep coding after Filo quits
  (on unless you turn it off).
- **Plan usage:** on each signed-in tool's name line, after its chip, a pill for the 5-hour window and the week whose
  account reports it ("5h 96%", "Week 92%"), its faint fill as wide as what's left; at 25% or less it turns warning
  (10% or less danger) and adds its reset ("Week 20% · Mon"), and its tooltip says what's left and when it resets. A
  window whose reset has passed shows nothing until it's asked again. Asked when the page opens and every 2 minutes while it's open (`coding:usage`,
  crates/filo-local `plan_usage`, kept 30 seconds). The pills never pop in: the last answer is saved in
  `desktop-plan-usage.json`, so the page shows it from its first frame, after a restart too, dimmed with a small spinner
  after the pills while the page's ask is on its way (not when the backend still keeps an answer under 30 seconds old);
  the answer then updates them in place. With nothing saved yet, or a window whose reset has passed, a shimmering
  placeholder of a pill's size holds its room meanwhile, for Claude Code and Codex, the tools whose accounts report it. Claude Code's comes from its account's usage
  (`api.anthropic.com/api/oauth/usage`), with the sign-in Claude Code keeps in the Keychain, read with `/usr/bin/security`
  as Claude Code reads it (an expired token is left for Claude Code to refresh); Codex's from `account/rateLimits/read`
  on `codex app-server`, started in the temporary folder (started disclaimed in a Desktop checkout, macOS would ask for
  the Desktop). Muse Code's host reports usage only after a model call in that session, and OpenCode's and pi's providers have no
  plan windows, so none of them shows one. In `dev` builds `FILO_PLAN_USAGE=<json>` (as `coding:usage` answers) stands in
  for the accounts', to see the pills run low, and `FILO_PLAN_USAGE_AFTER=<seconds>` holds the answer back that long,
  to see them while it's on its way.
- **Tool updates** (local_backend/tool_updates.rs): about once a day (on the first overview after the last look is a
  day old, saved in `local/tool-updates.json`; a look that finds nothing tries again after an hour) Filo looks up each
  installed tool's newest version where its own updater does: Claude Code's release channel
  (`downloads.claude.ai/claude-code-releases/latest`, or `stable` when `autoUpdatesChannel` says so), Codex's
  (`releases.openai.com/codex/channels/latest`), Muse Code's (`api.meta.ai/muse-code/channels/muse-stable`, its build
  number dropped), OpenCode's (`opencode.ai/update/api/latest/cli/npm`; a 1.x install the `opencode-ai` npm package) and
  pi's (`pi.dev/api/installer/releases/latest`). When it's newer than the CLI's `--version`, the row gets an accent
  "Update to vX" pill after its chip (`latestVersion` in `coding:overview`). Nothing updates by itself: a click
  (`coding:updateTool`) runs the tool's own updater, `claude update`, `codex update`, `opencode upgrade`, `pi update`
  or Muse Code's installer again (`curl -fsSL https://dev.meta.ai/install.sh | bash`), from the home folder with the
  login shell's PATH, given 10 minutes; the pill reads "Updating…" with a spinner meanwhile (`updating`). Then the tool
  is read again: its line shows the new version, or a red "Couldn't update: …" line has the updater's last lines
  (`updateError`, kept until the next try). Running rounds and turns keep the process they started with.
  `FILO_OPEN=coding-update-drive FILO_DRIVE_TOOLS="pi codex"` shows the pills and clicks those tools' only when they're
  stand-ins in the first folder of `FILO_LOGIN_PATH`, so a check never updates a real tool.
- **Chat turns follow the tools' modes:** Filo's, an agent's, a group member's and a routine's turn works as its
  engine's tool does here (`coding::turn_mode`). Full access: Claude Code's shell is allowed outright (`--permission-mode
  dontAsk`) and Codex runs with `danger-full-access`. Auto: Claude Code's shell goes to `--permission-mode auto`, and
  Codex is sandboxed to the turn's folder without network, its reviewer approving routine requests to go further
  (`workspace-write`, `on-request`, `approvals_reviewer="auto_review"`). The turn's instructions say which.
- **Projects:** a walk of the home folder (`FILO_REPOS_ROOT` for another; five levels, skipping hidden folders,
  Library and dependencies) finds git repos when the app opens and every 10 minutes, or at once with the refresh
  button. Add folder adds any folder, git or not; a plain folder is worked in as it is.
- **Tasks:** `coding:startTask` (a brief, a project, a tool; the page has no form for it) starts a round on a thread of its own: `claude -p --output-format
  stream-json` or `codex exec --json`, in the project folder or, for a git project with "Own worktree", in a new
  worktree under `local/coding/worktrees/<task>` on a `filo/<title>` branch. The mode becomes Claude Code's
  `--permission-mode` (Full access: `--dangerously-skip-permissions`) or Codex's sandbox; nobody approves steps mid-task.
  Each message, command, file change, plan and search is saved as it comes and the thread updates live. Stop ends the
  tool's process group; a finished task takes a follow-up, which resumes the tool's session. Each round runs in a
  worker (`filo --worker round …`), so it carries on when Filo quits with Keep coding after Filo quits on (see "The
  backend on this Mac"); a task left running with no worker (Filo crashed mid-round before this, the Mac restarted) is
  marked stopped when Filo opens again.

### Coding from a conversation

Filo and your agents hand coding work to Claude Code, Codex, Muse Code, OpenCode or pi from any chat, an agent's conversation or the group,
with the MCP tools in crates/filo-local `coding_mcp` (every conversation's server has them). The task belongs to that
conversation (crates/filo-local `coding_handoff`: `coding_handoffs`, `coding_requests`, `coding_notes`,
`coding_rounds`, `coding_wakes` in `local/filo.db`), which hears what it asks and how each round ends. There is no
cloud worker: everything runs on this Mac.

- **Tools:** `list_coding_projects` (tools plugged in and installed, the projects), `start_work` (title, goal, done
  when, `changes: false` to only look, notes, a project by name or any folder by its full path, `tool` claude-code or
  codex, else the conversation's own engine; model, effort and workspace only when the user asked; `continues` adds a
  round to a task instead), `watch_coding_task`, `tell_coding_task` (a note while it works, the next round once it's
  done; after 5 rounds only with `userSaidSo`), `stop_coding_task`, `answer_coding_request` and
  `ask_user_about_coding_request`. A brief that shouts (NOT, MUST…) or restates workspace or git rules is refused with
  why, and so is a second copy of work under way in the conversation unless `separate`.
- **Reasoning level:** a task starts with its tool's level from the Coding page (also when `start_work` names only a
  model), or the `effort` asked for. `effort` with `continues` or `tell_coding_task` changes it from the task's next
  round (a round under way keeps its own; `default` is the Coding page's again); both tools say to pass it only when
  the user asked, as a change can break the tool's prompt cache. Each round runs at the nearest level its model takes
  (`Local::chat_effort`, saved on the task), and OpenCode, given a level without a model of the task's own, sets it as
  the variant of its session's model. A task with no level reads as the one its tool runs the model at (Claude Code's
  `modelSettings`/`effortLevel`, Codex's `model_reasoning_effort` or its catalogue's, Muse Code's, OpenCode's and pi's
  model default), else "Default".
- **Runs:** the MCP server only writes rows; the app's loop (`local_backend/handoff.rs`, every 0.7 s) starts queued
  tasks, applies Stop and sends wakes. A round keeps the tool talking (crates/filo-local `coding_session`): Claude Code
  with `--input-format stream-json --permission-prompt-tool stdio` (its `can_use_tool` requests and AskUserQuestion),
  Codex through `codex app-server` (its approval and `requestUserInput` requests, `turn/steer` for notes). The task
  keeps its tool's mode from the Coding page (Auto or Full access), and `changes: false` takes
  away the edit tools (Claude Code) or gives a read-only sandbox (Codex). Each round gets a proof folder,
  `local/coding/proof/<task>/round-<n>`, where the tool is told to save screenshots, recordings or logs. A round has no
  time limit: it's stopped only when nothing has come from the tool for 30 minutes (`turn::HUNG_AFTER`, "seemed
  stuck"), a clock that doesn't run while the tool waits on an answer or on its background work.
- **Background work:** a turn that ends with work still running in the background (a build, a Monitor) leaves the task
  Monitoring and the tool running; it picks the task back up when that work ends. A command that never ends on its own
  (`engine::endless_watcher`: `tail -f`, `log stream`, `journalctl -f`…, run in the background or as a Monitor) is a
  watcher, which nothing waits on: with only watchers left, Claude Code is asked after a minute whether its work is
  done. DONE stops them (`stop_task`) and the round ends Done with the report it gave; an answer that it still waits
  on what they watch is asked again after 10 minutes.
- **Questions:** what the tool asks is saved and the conversation is woken: in a Filo or agent chat, a turn of its own
  started by a message only the engine sees (store.rs `Message::hidden`; such a turn may end in silence), one at a time
  (a wake waits while a turn runs there); in the group, the member who started it gets a turn there. It answers with
  `answer_coding_request`, or hands the question to the user, who sees a card with Allow / Allow always / Deny (or the
  options) in the chat; a request nobody decided shows there after 90 seconds anyway. The tool waits meanwhile.
- **Results:** when a round ends, the conversation is woken with the tool's final message and the files it saved, and
  reports in its own words; that reply carries the round's Open proof (`codingResult`), whose page
  (`coding:roundResult`) shows the final message and the proof files from this Mac. The wake (`coding_handoff::
  round_message`) asks for plain news of what changed, with what only the user can decide, any part of the brief not
  done and a gap in the checks that changes what they'd do next, and says why: the task's card names the tool, and the
  result page keeps the report and the commit. A long report is cut in the middle, keeping its start and its end, where
  what wasn't checked or done is (`clipped_report`); in the group it gets the room left of a turn's 4,000-character
  reason, so the wake's own end isn't cut. A round that ended without a report is told after a look at the task's
  thread. `FILO_REAL_DB=<a filo.db copy> FILO_REAL_TASK=<id> FILO_REAL_CHAT=<its chat's folder>
  FILO_REAL_DIR=<new folder> cargo test -p filo-local --test turn_e2e real_round_report -- --ignored --nocapture`
  (`FILO_REAL_ENGINE=codex` for Codex) wakes a copy of the chat with a real round's news and prints the reply. A failed round is reported too; a
  stopped one isn't. The chat lists its tasks (status, what it's doing, "Needs you"), and the Agents list shows a chat
  as working while its task runs.
- **Sharing a result:** the result page's Share puts a copy of the round's report and proof on a page at
  `filo.im/results/<uuid>` that anyone with the link can open (unlisted, not indexed), and copies the link. That page is
  drawn like the result page here, in the sharer's Appearance settings (theme, light or dark, typeface and size). The
  result page then shows it's shared, with Copy link and Unshare, which deletes it there (`coding:shareResult`,
  `coding:unshareResult`, `local_backend/result_share.rs`). The copy leaves this Mac's paths out: paths in the proof
  folder and the project become relative, any other home or temp folder path is cut to its file's name, and keys are
  hidden; that goes for its text files too. Pictures and recordings go as they are; other files that aren't text, and
  whatever doesn't fit in 30 MB, stay here and the page says so. The server side is Filo's Worker at dev.filo.im (not
  part of this repository), with `filo.im/results/*` routed to it; no
  account, a per-IP limit, and the share's token is the only way to delete it. `FILO_OPEN=coding-result-share-drive:
  <task>@<round>` clicks Share and Unshare on a round's page and checks the link with curl in between.
- A task started while the app is closed (a routine's turn in `filo --scheduler`) stays queued and starts when Filo
  opens. `cargo test -p filo-local real_ -- --ignored --nocapture` runs a Supervised round of each installed coding
  tool that has to ask (`FILO_REAL=claude`, `codex`, `muse`, `opencode` or `pi` for one).

### pi

[pi](https://pi.dev) runs as `pi --mode rpc`, JSON lines both ways (pi's docs/rpc.md; crates/filo-local `coding_pi`),
for chat turns and coding rounds alike, through the same session code as the other engines (coding_session.rs). The
message is a `prompt`, a note a `steer`; what it does comes as events (`tool_execution_start`, each message's
`message_end`) and the run is over at `agent_settled`. The instructions go in as `--append-system-prompt` on every
run, a session carries on with `--session <id>` (one pi can't find, "No session found matching", counts as gone, so a
chat starts afresh with its recent messages), and a new session's id comes from `get_state`. The Coding page's model
list is pi's `get_available_models` (the providers it's signed in to), ids `provider/model`, with the thinking levels
each takes (`--thinking`); signed in means a provider in pi's `auth.json` (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`),
read, never written.

What pi doesn't have comes from Filo's extension (crates/filo-local `pi_extension.ts`, written to the temporary folder
and loaded with `-e`), told what it needs in `FILO_PI`:

- **MCP servers.** A chat's (Filo's and the apps on for it) go to `pi.registerMcpServer` on a pi that has MCP of its
  own, with their tools declared to the model; pi 0.84 has none, so there the extension starts each server, lists its
  tools and registers each as `mcp__<server>__<tool>`, passing calls on as `tools/call`. The user's own `mcp.json` and
  `PI_CODING_AGENT_DIR` (their login) are left alone.
- **Modes.** Full access lets everything run. Auto lets reading run, and changes to files in the task's folders; a
  change elsewhere, and in a coding round a command that isn't plainly a look (`ls`, `git status`, `rg`… with no
  redirection), is asked first, as an `extension_ui_request` select with Allow, Allow always and Deny: the round's
  permission card (`Host::ask`), answered with `extension_ui_response`. Allow always lets the same program, or folder,
  through for the rest of the run. In a chat nobody answers mid-turn, so its shell isn't asked about and a change
  outside its folders is refused. Work that only looks can't write or edit.
- **Nothing holds a turn.** pi asks nothing in RPC mode, the extension leaves `project_trust` undecided (saved trust
  decisions apply; without one pi's default ignores the project's own `.pi` files), and any dialog that isn't Filo's
  card is dismissed at once, with a line in the thread.

A chat runs with `--no-extensions --no-skills` and its skills given one by one (`--skill`, from the workspace's
`.agents/skills`); a coding round loads the user's own extensions and skills. `PI_OFFLINE=1` keeps update checks out
of every run. `cargo test -p filo-local real_pi_scripted -- --ignored` (and `--test turn_e2e real_pi_scripted`) runs the
installed pi with a scripted model of the tests' own (tests/pi/scripted-provider.ts: no sign-in, nothing called): the
permission card, a steer mid-run, a resumed session, and a chat turn replying through Filo's real MCP server.
`FILO_OPEN=coding-pickers-drive FILO_DRIVE_TOOL=pi` drives its model picker on the Coding page.

## Artifacts

Reports, web pages, diagrams and small interactive tools agents make for you, each one HTML page kept on this Mac with
every version (crates/filo-local `artifacts`: `artifacts`, `artifact_versions`, `artifact_links` in `local/filo.db`, the
pages under `local/artifacts/<id>/`). Nothing leaves the Mac until you share one: the page's Share gives it a link on
filo.im (below), and Share to claude.ai (the Artifacts tab's settings) lets Claude Code publish one to your claude.ai
account.

- **Sharing a link:** the artifact page's Share puts its current version at `filo.im/artifacts/<uuid>`, which anyone
  with the link can open (unlisted, not indexed), and copies the link; the page then shows it's shared, with Copy
  link, Update once there's a newer version (the same link, the new version) and Unshare, which deletes it there
  (`canvasArtifacts:share`, `canvasArtifacts:unshare`, `local_backend/artifact_link.rs`, sent like a coding result by
  `result_share.rs`). What goes is the page as claude.ai gets it (`artifact_share::share_page`: Filo's Default theme
  at its marker, light or dark with the viewer's), not rewritten. The server side is Filo's Worker at dev.filo.im (not
  part of this repository), with `filo.im/artifacts/*` routed to it:
  the link is a frame of Filo's own with no scripts, and the page runs in it from dev.filo.im, sandboxed as the app
  sandboxes it (scripts in an opaque origin, no storage or network, pictures and fonts as data: URLs), never on
  filo.im's origin. `FILO_OPEN=artifact-share-drive:<id>` clicks Share, Update (after restoring version 1) and Unshare
  and checks the link and its frame with curl (`FILO_DRIVE_CHROME=<Chrome binary>` draws the link at each version;
  `FILO_ARTIFACTS_URL` for a test server; the shares' test servers, `FILO_RESULTS_URL` and `FILO_ARTIFACTS_URL`, and
  `FILO_UPDATE_FEED` at run time are read in `dev` builds only).

- **Making them, file first** (like Claude's own Artifact tool and the project-artifact skill): an agent builds the page
  as an HTML file, edits it with its file tools and publishes it with `publish_artifact` (crates/filo-local
  `artifacts_mcp`: `path`, `title`, `summary`, `kind` report, page, diagram or tool, and `label`, a few words on the
  version). The artifact keeps the file's path (`artifacts.source`, a column added beside the migrations), so the first
  publish of a file makes a new artifact and each publish after it saves the next version; a page the same as the
  current version saves none. The file is read by Filo (`artifacts::read_page_file`: .html or .htm, 2 MB at most,
  checked before it's read; a path that isn't full is in the turn's working folder), and the reply lists what the
  sandbox would block (`artifacts::page_warnings`). `open_artifact` gives an artifact's details and its file (an
  artifact made before has its current page written to `pages/<name>.html` in the working folder, which becomes its
  file), and `list_artifacts` finds them. An artifact keeps who made it and the conversation and turn.
- **The artifacts skill** (`crates/filo-local/src/artifacts_skill`, written into every turn's skills): the workflow
  (a stable `pages/<name>.html`, the template, a check, publish, republish with a label), `template.html` (Filo's
  Default theme for outside the app, the `<!--filo:head-->` marker, the house style: status line, cards, scrolling
  tables, pills, tabs, a figure and a data block for pages kept up to date) and `check_page.py`, which opens the page
  in a sandboxed frame at 380, 720 and 1100px in headless Chrome and lists what sticks out, is cut off or would be
  blocked. Claude Code chat turns have Write and Edit for it (outright in Full access, in the working folder
  otherwise); Codex writes with its shell.
- **From a coding task:** a task's instructions point it to the skill when it's asked for a page; it builds the file in
  its folder at a path that stays the same between rounds and names it in its summary, and the conversation publishes
  it once the round has finished (the same file after a later round is the next version).
- **In the conversation:** what a turn made, changed or opened shows as the finished-work card (About …, its summary,
  Open), one per artifact and turn, from the chat links (`voiceSessions:detail`'s `tasks`), so they're there after a
  restart. While a turn runs, the turn watcher notices the database changing, so cards, the tab and an open artifact
  update live.
- **The tab and the page:** `canvasArtifacts:list/get/listVersions/getVersion/markOpened/restoreVersion/
  sendArtifactMessage` (`local_backend/artifacts.rs`; `publish` shares to claude.ai while that's on). The page has one menu with
  Preview, Source and its versions (newest first, with when each was made; picking one shows it in place, and with an
  older one shown the menu adds Show current version and Restore, which saves the old page again as a new version and
  writes it to the artifact's file too, unless that file has changes not published yet),
  Open in browser, Show in Finder (its folder) and Download.
- **The preview is sandboxed** (`screens/artifact.rs` `sandboxed_page`): the page runs in a frame with scripts but no
  origin, under a policy with no network (no fetch, external scripts, styles, fonts, pictures or frames), in a web view
  with its own data store that keeps nothing; navigation is refused (a link opens in the browser, at most one a
  second). So it can't read the Mac's files, the app's sign-ins or anything online, and it works offline. A page's
  `filo.chat(text)` asks Filo something only after you press Send; it goes to the conversation the artifact came from
  (else Filo's), with the artifact named for the engine.
- **Pictures from the web** (crates/filo-local `artifact_images`): before a version is saved (`artifacts::create`,
  `update`, `publish`), the web pictures its page names are downloaded into `local/artifacts/<id>/images/` (named by
  their contents), with `images.json` saying which address is which file: addresses in `src`, `srcset`, `poster` and
  CSS `url()`, and anywhere else (a script's string) those with a picture's extension or `format=`. Only PNG, JPEG,
  GIF, WebP and AVIF by their first bytes, 5 MB each and 16 MB a save, from hosts that aren't this Mac or its network;
  a failed download leaves the address. The version's file stays as published; the preview shows the page with those
  addresses as `filo-media://<id>/images/<file>` (served by `screens/artifact_media.rs` from the artifact's folder,
  so the policy stays), a shared link and the claude.ai copy get them inline as `data:` addresses, and
  `publish_artifact` no longer warns about them. Artifacts made before get theirs once, in the background at the
  app's start (`save_existing`, noted in `app_state`).

## Apps

Extensions › Apps connects MCP servers once for both CLIs: every chat, agent conversation and group turn of Claude Code
or Codex gets the apps that are on for that engine. Nothing is written to `~/.claude*` or `~/.codex/config.toml`. Codex's
app-server reads that file, so a chat turns off every MCP server it sets up, however it's written (crates/filo-local
engine.rs `codex_user_config_off`, read with a TOML parser), and Filo's own servers and the apps go by another name
when the file has one of the same name, which Codex would merge with them (`codex_own_name`).

- **What an app is** (crates/filo-local `apps`, the `apps` table in `local/filo.db`): a local server (a command Filo
  runs, stdio, with an environment) or a remote one (a URL: Streamable HTTP, falling back to HTTP+SSE for older
  servers), signing in not at all, with an account (OAuth) or with an API key (a header for a remote server, an
  environment variable for a local one). Keys, sign-in tokens and client secrets are in the login Keychain
  (crates/filo-local `secrets`, service `im.filo.desktop.apps`); the row only says one is there. A debug build is a new
  program each time it's built, so macOS may ask once to let it read them ("Always Allow").
- **In a turn** (engine.rs `McpServer`): each app is one more stdio server named `filo-<slug>`, the app's own binary as a
  proxy (`filo --local-mcp --app <id> --db <filo.db>`, crates/filo-local `apps_client::proxy_main`). Claude Code gets it
  in `--mcp-config` with `mcp__filo-<slug>` allowed; Codex gets `-c mcp_servers.filo-<slug>.…` (approved, 60 s to start,
  10 minutes per call). The proxy reads the secrets, refreshes the sign-in when it runs out or the server refuses it
  (under a file lock in `local/apps/`, so turns at once don't each spend a rotating refresh token) and passes messages
  through. Coding tasks keep their own permission modes and don't get apps.
- **Which agents:** each app is open to every agent of the Agents tab (new ones too), or only to those switched on
  (`agent_scope`, `agents`). A turn speaks as one agent (turn.rs `Runtime::agent`, set in `local_backend.rs`
  `start_turn_then` by `turn_agent`): an agent's conversation and its routines' and check-ins' runs are that agent;
  Filo's own chats (a routine with no agent, `filo-local send`, which writes to `local/chats`) are the Filo agent (`apps::FILO_AGENT`, the id the
  default Filo agent is made with, listed even when it was removed); in the group the speaker is (`group::apps_agent`,
  `filo` being the Filo agent). Only apps open to that agent are plugged in, and the proxy refuses to start for an agent
  it isn't open to (`--agent`). The composer's "@" list offers only those apps too (`apps:sessionAgent`).
- **Which engines:** separately, each app's Claude Code and Codex switches decide which CLI may run it, whichever agent
  speaks.
- **Tool permissions:** an app connected now starts with every tool on, those its server adds later too
  (`App::all_tools`); the user turns off any they don't want. An app connected before that keeps its tools as they
  were (the column came in off for it): there a tool is on only if the server marks it read-only (`readOnlyHint`), it's
  one a preset lists as checked to only read while the server marks nothing (`Preset::read_tools`: DeepWiki's three),
  or the user turned it on, and a name alone never counts. Either way the user's own choice wins. The proxy enforces
  it: other tools are left out of `tools/list` and their calls answered with an error the model reads. Turning on a
  tool that isn't read-only asks first, since nobody approves a step while a turn runs. Each app also has its own On
  switch.
- **Account sign-in, no Filo account** (crates/filo-local `apps_oauth`): MCP's authorization spec as a native app. The
  server's 401 names its protected-resource metadata, which names its authorization server; Filo registers itself there
  (dynamic client registration, a public client with a `http://127.0.0.1:<port>/callback` redirect, reused while that
  port is free), opens the browser with PKCE S256, a state and the server as `resource`, and takes the redirect on a
  one-shot loopback listener. A service with no dynamic registration (GitHub, Google, Asana…) can't be signed in to
  this way: the app says so, and takes an OAuth client the user makes there (redirect
  `http://127.0.0.1:33418/callback`) or an API key instead.
- **Status:** adding, editing, a new key or a sign-in checks the app (start or reach it, `initialize`, `tools/list`),
  and the page opening re-checks remote apps not checked for 10 minutes. Ready, sign-in needed, or couldn't connect
  (with why) is kept in the row with the tools.
- **Presets** (`apps::PRESETS`) are only servers anyone can connect from this Mac, checked when they were added:
  Linear, Notion, Sentry, Vercel, ClickUp, Stripe, Upsy and Executor (account sign-in, dynamic registration), GitHub (a personal access
  token), Context7, DeepWiki, Hugging Face and Cloudflare Docs (no sign-in), and Playwright (local, shown only
  when `npx` is installed). Each has one Connect, like a connector directory (`apps:connectPreset`): an account one is
  added with its own fixed settings and its sign-in opens in the browser at once; one with no sign-in is added and
  checked; GitHub asks only for its key. Connecting a preset again reuses its app, with
  its access as it was. A custom server (any URL or command, with its sign-in, key header, client and scopes) and an
  app's address, engines, Check again and Edit are under Advanced.

## The browser

Filo has a browser of its own (src/browser.rs, src/browser/), on the WebKit view the rest of the app uses (lb-wry). The
user opens it from Window › Browser (⇧⌘B); agents work it in the background with the `browser_*` tools.

- **Logins:** every tab uses one WebKit data store kept on disk (`WKWebsiteDataStore` by id, macOS 14 and later), its id
  in `local/browser.json`, so cookies and site storage last between launches. A data folder of its own (a test run's)
  has its own store. As in Safari, a cookie with no expiry (a "session" cookie) doesn't outlast Filo. The user agent is
  Safari's, with this Mac's Safari version: some sign-ins turn away a web view that doesn't say so.
- **Pages in the background** (browser/native.rs): each tab's WKWebView lives in a borderless window of its own far off
  the screen (`Park`), ordered in, not hidden with the app and left out of Mission Control and the Window menu. Its class
  (`FiloBrowserPark`) says it's the key window, so WebKit counts the page as focused (it takes keys, shows a caret,
  `document.hasFocus()`). WebKit's check of whether the window is covered is off (`_setWindowOcclusionDetectionEnabled:`,
  read again by hiding and showing the view once), background throttling is off (`inactiveSchedulingPolicy` none), and
  App Nap is held off while tabs are open (`Awake`). So a background page is visible to itself and runs as fast as one on
  screen. wry calls `NSApp activate` whenever it makes a web view; tabs are made with that call stubbed out
  (`without_activation`), so an agent opening a page doesn't bring Filo forward.
- **The browser window** (browser/window.rs) lists the tabs (an agent's carry its name), with back, forward, reload or
  stop, an address bar (words search Google) and +. The shown tab's page is moved into the window's view and back to its
  own window when another shows or the window closes. A link to a new window, or a pop-up (a sign-in's), opens as a tab of
  the same owner, made with its opener's configuration so `window.opener` works; the agent's next answer about the opener
  names the new tab, and a pop-up that calls `window.close()` closes its tab (the opener's next answer says so). The
  windows tabs are parked in answer the main screen for `screen` (`FiloBrowserPark`): off every screen it was nil, and
  wry unwrapped it when a background page opened a pop-up, which crashed the app.
- **The user's keys in the window:** the page is a native view inside GPUI's, so GPUI never sees a click in it and its
  focus stayed where it was (the address bar, after typing an address). A key the page leaves unused goes up the
  responder chain once the page is done with it (WebKit sends it on), and in GPUI's view to whatever has GPUI's focus: a
  Return Google's sign-in acts on from its own script went on to the address bar, which opened its address again and
  reloaded the page, and ⌘A and ⌘V (which GPUI is offered before the menus) went to the address bar too. So the page's
  next responder is `FiloBrowserKeyStop` (`show_in`), where keyDown and keyUp end, as in a browser, and a click in the
  window (`ClickWatch`, an AppKit monitor) or the 300 ms refresh that finds the page with the keyboard takes GPUI's
  focus away (`page_took_keys`); ⌘ keys then go to the Edit menu, which sends them to the page.
- **Agents' tools** (crates/filo-local `browser_mcp`, in every chat turn's server): `browser_open`, `browser_navigate`,
  `browser_read`, `browser_click`, `browser_type`, `browser_select`, `browser_upload`, `browser_screenshot`,
  `browser_click_at`, `browser_hover`, `browser_drag`, `browser_keys`, `browser_scroll`, `browser_wait`,
  `browser_dialog`, `browser_tabs`, `browser_close` and `browser_ask_user` (`NAMES`, which a test holds to the list
  `tools` gives). The MCP server asks the app over a Unix socket,
  `local/browser.sock` (mode 0600), one JSON line each way; the app answers on its main thread (`browser::init`,
  `handle`). A second Filo on the same folder finds the socket answering and leaves it; on quit, Filo removes only the
  socket file it bound itself (`BOUND`, its device and inode). Without the app open, the tools say Filo has to be open. A screenshot comes back as an image block. Every
  engine's instructions say the browser is always theirs (engine.rs `capabilities`), and the composer's "@" list offers
  "@browser" (see the skills above).
- **Reading and acting** (browser/page.js): runs in a JavaScript world of Filo's own (`callAsyncJavaScript` in
  `WKContentWorld.defaultClientWorld`), which shares the page's DOM but not its scripts. `browser_read` gives the page as
  text with a ref per link, button, field, checkbox and menu (`[e5]`, the same while the page is open), and per item the
  page lets be dragged (`draggable="true"`: a short line, then what's in it), what's in each field, and same-site frames inline; a frame from another site is listed with its box, for a screenshot and a click at
  its spot. `browser_click` scrolls the element into view and clicks it with real mouse events where it's on top, else
  through the page. `browser_type` focuses the field (selecting what's in it) and types with real key presses, checks
  the field got the text and otherwise sets it through the page.
- **Menus** (`browser_select`): a `<select>` is set through the page (page.js `choose`: the option by its text or
  value, several for a `multiple` list, with input and change events). A real click on one, or Space, Return or an
  arrow with the focus on it, opens AppKit's menu, which tracks on the main thread where only the user can work it: so
  `browser_click`, `browser_click_at` and `browser_keys` refuse a `<select>` and point to `browser_select`. A menu the
  page draws (a combo box, a list box, a button with a list) is opened with a real click, and its option (role option,
  menuitem, a listbox's items, found by its text, `option`) clicked for real.
- **Files** (`browser_upload`, browser/delegate.rs): the agent gives full paths; page.js `file_input` finds the file field
  for the ref (the field itself, one inside it, its label's, its form's, or the page's only one: a drop area's hidden
  field), which is clicked through the page (`showPicker`, else `click`, from a script WebKit runs as a user gesture),
  else the ref itself with a real click. WebKit then asks the UI delegate for files
  (`runOpenPanelWithParameters:`), and the staged paths are its answer (only the first when the field takes one): no
  open panel. A page that asks for files when none were staged, while an agent works it or off the screen, gets none,
  and the agent is told; the user's page on screen gets wry's open panel as before.
- **Downloads:** wry's download handlers: an agent's tab saves to `local/downloads` (numbered when the name is taken),
  the user's to Downloads as wry chose. The next answer about the tab says "Downloading …" and then "Downloaded name
  (size) to path", or that it failed; `browser_wait` with `for: "download"` waits for it. wry downloads only what WebKit
  can't show, so the navigation delegate gets a subclass too, whose `decidePolicyForNavigationResponse:` downloads a
  response with `Content-Disposition: attachment` (a PDF or an export link), as a browser does.
- **Dialogs** (`browser_dialog`): wry's UI delegate had no alert, confirm or prompt, so WebKit answered them itself
  (nothing shown, confirm false). Each tab's UI delegate is moved to a subclass of Filo's (its class changed in place,
  as KVO does, and set on the web view again so WebKit reads its methods; other web views of the app keep wry's), which
  keeps the dialog and its completion handler on the tab. The page's script waits meanwhile, so `page` stops waiting
  for a script when a dialog shows, and every answer about the tab says it's there until it's answered; the browser
  window shows it as a bar with OK (and Cancel) for the user. A dialog left unanswered is dismissed when its tab closes,
  as WebKit requires. The same subclass takes `webViewDidClose:` (above).
- **Hover** (`browser_hover`): a real mouse move to the ref's middle or a spot, left there; the answer lists the lines
  of the page's text that weren't there before. Every move an agent makes goes to the web view as `mouseDragged:` with
  no button held (`native::mouse`): WebKit dropped the same event sent as `mouseMoved:` for a page off the screen, so
  pages got no mousemove, mouseenter or `:hover`. `browser_click` moves the pointer onto the element first and finds it
  again before pressing, since what shows on hover can move it.
- **Waiting** (`browser_wait`): until `text` shows (or, with `gone`, is gone) in the page's text or fields, same-site
  frames too, polled four times a second; until the tab's downloads end; or, with neither, until it has loaded and its
  DOM hasn't changed for half a second (page.js `quiet`). Up to `seconds` (10, at most 60). Reading or screenshotting
  a pop-up just opened waits for its page to begin first (`ready`).
- **Full-page screenshots** (`browser_screenshot` with `full`): WebKit's snapshot draws only what shows, so the page is
  scrolled a view at a time from its top, each view snapshotted and put where it was, then scrolled back; at most 6000
  pixels tall (the answer says when it's cut). A bar that stays on screen shows in each piece.
- **Real input** (`native::mouse`, `native::key`): AppKit events built in the app and handed to the web view itself
  (`mouseDown:`, `keyDown:`…), so the page gets trusted events, inside frames from other sites too, without moving the
  user's pointer, using their keyboard, Filo coming forward or the Accessibility permission. Keys use a US keyboard's key
  codes; a ⌘ key goes to the page as a key equivalent, and ⌘A/C/X/V/Z run the edit command the page's window has no menu
  for. Screenshots are WebKit's snapshot of the view at one pixel per CSS pixel, so `browser_click_at` and `browser_drag`
  take a spot in the picture as it is. While WebKit turns an agent's mouse event into the page's, `[NSEvent
  pressedMouseButtons]` answers the buttons that event holds (`with_buttons`), not the real mouse's: with none held, a
  press had `buttons` 0 and the page got no pointerdown or pointerup.
- **Dragging** (`browser_drag`, browser.rs `drag`, browser/drag.rs): each end is a ref's middle (with both by ref the
  page scrolls so both show) or a spot of the screenshot. The pointer is pressed at the start, moved there in steps (2,
  5 and 9 points first, then about every 12, `drag_path`), held a moment and let go, all events handed to the web view
  (`mouseDragged:` in between). When the page starts an HTML5 drag and drop, WebKit asks AppKit for a drag session
  (`dragImage:at:offset:event:pasteboard:source:slideBack:`, or `beginDraggingSessionWithItems:event:source:` for an
  attachment), which would follow the user's real pointer over the screen. For a view an agent is dragging in, the web
  view's class takes those calls itself (`hook`, put in once per class; other views' go on to AppKit) and carries the
  drag the way AppKit would, as WebKit's own tests do: `draggingEntered:` and `draggingUpdated:` with a dragging info
  of Filo's (`FiloBrowserDragInfo`, on the drag pasteboard WebKit wrote the page's data to), then
  `performDragOperation:` where the page said it takes the drop (else `draggingExited:`) and the source's ended call.
  So the page gets WebKit's own dragstart, dragover, drop and dragend with their data. A mouse-up after it lets WebKit
  release the pointer it cancelled at dragstart (AppKit's session swallows the real one; without it the next press had
  no pointerdown). page.js watches for the page's drop event, so the answer says whether the page took the drop.
- **Watching and stepping in** (browser/activity.rs): each tab keeps the agent working there (its calls going on, and
  when the last ended: it counts as working until 6 s after) and who has the tab (`Control`: the agent, paused, or
  taken over by the user). Each call is a step in the agent's trail, in words from its op and answer ("Clicked
  “Orders”", "Typed into “Email”": never what was typed), 30 kept per agent. While an agent works in the shown tab, the
  browser window shows a bar over its page, "Scout is working" and its last step, with Pause and Take over, and a button
  that opens its last 12 steps; paused, Resume and Take over; taken over, the page gets the keyboard and the bar has
  Hand back. The tab strip has a blue dot on a tab an agent works in, orange where the user holds it, a dialog waits or
  the agent asked for help. An action call (anything but reading, screenshots, waiting, the tab list and the ask)
  waits while the user holds its tab, up to 100 s (the MCP server waits 180), then says the user paused it or took the
  tab over; once let go on, its answer begins with what happened (taken over: read the page again). Where an agent
  clicks, drags or points on a page the user has on screen, a ring grows and fades (`native::flash`: a layer-backed view
  over the page in the browser window, so neither the page nor an agent's screenshot sees it). The agent's chat header
  has a badge ("Scout is working in the browser", paused, or the user has its page) that opens the tab in the browser
  window (screens/agents.rs `browser_badge`). It takes what the header leaves after the name's least
  (working_panel.rs `side_badge_room`): without room for its words it says "Working in the browser", then cuts that
  with "…" (`fit_label`, which cuts the work indicator's words too; GPUI's own `truncate` slices them at the edge).
  `FILO_OPEN=coding-cards:agent FILO_FIXTURE_BROWSER=<name>` shows one. Filo's windows redraw as a call starts and ends, and once more when the
  agent stops counting as working.
- **Asking the user** (`browser_ask_user`): for what the agent can't do after trying (a CAPTCHA, a password, a code
  sent to a phone). The tab gets the reason and Done / Not now in the browser window and an orange dot in the tab strip,
  Filo's main window a card that opens it (and the Dock bounces with Filo in the background). The tool waits up to five
  minutes for an answer, and can be called again to wait more.
- **Bringing in a profile** (crates/filo-local `browser_import`, browser/import.rs): the browser window's download
  button (or the empty window's Import from Helium or Chrome) shows the import panel in the page's place. It lists the
  profiles of Helium (`~/Library/Application Support/net.imput.helium`) and Chrome (`…/Google/Chrome`) from their
  `Local State` (`profile.info_cache`, the one used last first). Import reads the profile's `Bookmarks` (JSON, every
  folder) and `History` (SQLite, locked while the browser runs, so it and its WAL are copied to `local/tmp`, read and the copies
  removed: the newest 20,000 pages that aren't hidden) and merges them by address into `local/browser-places.json`
  (bookmarks first, at most 30,000). Nothing in the other browser's folder is written, and its `Cookies` and
  `Login Data` are never read (they're encrypted with that browser's own key).
- **Passwords** come only from a CSV the user exports from the browser's password manager
  (chrome://password-manager/settings, Export passwords): Chrome's and Helium's `name,url,username,password,note`, and
  Safari's and Firefox's headers too. The logins (address, username, password; app rows and ones without a password
  left out) are kept as JSON in one Keychain item of Filo's own, service `im.filo.desktop.browser`, account `logins`,
  merged by site and username, and the CSV is deleted once they're saved. The panel can remove them all. A debug build
  is a new program after each rebuild, so macOS may ask once to read the item ("Always Allow").
- **Address bar suggestions:** typing in the address bar lists up to 8 of those places in a band under the toolbar
  (the page is a native view over the window, so nothing of GPUI's can cover it): every word must be in the address or
  title; bookmarks, addresses that start with what's typed and often and lately visited pages first. ↑ and ↓ pick one,
  Return or a click goes there, Esc closes the list.
- **Offering a login:** the logins are read from the Keychain once, off the main thread, when a page first needs
  them. When the shown page has finished loading on a site with a saved login (the same host, `www.` or not, and over
  https unless the login was saved for http), page.js's `login_form` looks for a password field shown, or a username or
  email one a sign-in's first step has (again a few times a second apart, for forms drawn late). Then a bar offers
  Fill <username> for each (up to three) and Not now. Fill hands the login to page.js's `fill_login`, which fills the
  fields through the page (so its scripts see the input) only if the page is still on that site. Agents never get the
  logins: no tool reads them, and `browser_read` shows a password field's value as dots.
- **Check:** `FILO_OPEN=browser-check` on a data folder of its own (browser/check.rs) serves a small test site on
  127.0.0.1 and works it through the socket and the real MCP server with another app in front: reading, clicking and
  typing by ref, frames and timers in the background against the same page on screen, a screenshot, real clicks and keys
  at a spot (a frame from another host too), whether Filo stayed in the background, signing in and the ask. Run it again
  with `FILO_BROWSER_CHECK=again` on the same folder: the sign-in's cookie must be back, and that run removes the
  folder's WebKit data store at its end (`remove_test_store`). `FILO_FIXTURE_SHOTS=<dir>` saves the agent's screenshot
  and a picture of the browser window.
- **Keys check:** `FILO_OPEN=browser-keys-check` on a data folder of its own (browser/keys_check.rs): the test site's
  `/chat` in the browser window, with clicks and keys queued for the app as the mouse and keyboard send them
  (`as_user`), each case after the address bar had the focus. Return must submit a form, send a chat box and press a
  button the page acts on from its script (leaving the key unused) without reloading, ⌘A must select the page's field,
  an agent's keys must reach the page and not the address bar, and Return in the address bar must still open what's
  typed. `FILO_FIXTURE_SHOTS=<dir>` saves a picture of the window.
- **Drag check:** `FILO_OPEN=browser-drag-check` on a data folder of its own (browser/drag_check.rs): the test site's
  `/drag` in a tab in the background, dragged through the socket. Draggable tiles (HTML5 drag and drop) by ref must get
  trusted dragstart, drop with the dragstart's data and dragend and reorder, with no click; cards reordered by pointer
  events, by spot, must get pointerdown, moves with the button held and pointerup; a range slider must reach its end; a
  drop where the page takes none must drop nothing; a click after them must be an ordinary one; and the user's pointer
  must not move nor Filo come forward. `FILO_FIXTURE_SHOTS=<dir>` saves the agent's screenshot after the drags.
- **Tools check:** `FILO_OPEN=browser-tools-check` on a data folder of its own (browser/tools_check.rs): the test site's
  `/tools` in a tab in the background, through the socket: a file field and a drop area's hidden field get the agent's
  files with no panel, a chooser with none staged gets none and says so, a download is saved in `local/downloads` and
  reported, a `<select>` (one option, then several) and a combo box the page draws are picked and a click on a
  `<select>` refused, hover shows a `:hover` menu with a trusted mouseenter, confirm (dismissed, accepted), prompt
  and alert are held and answered, a pop-up opens as a tab and closes itself, `browser_wait` sees text go and come and
  says when it didn't, a full-page screenshot reaches the page's bottom, and Pause and Take over hold the agent's click
  until let go, with the agent told; the trail has its steps and never the prompt's text. `FILO_BROWSER_REAL=1` then
  works the same tools on the-internet.herokuapp.com's `/upload`, `/download`, `/dropdown`, `/hovers`,
  `/javascript_alerts`, `/windows` and `/dynamic_loading/2`, and a full-page screenshot of Wikipedia's WebKit article.
  `FILO_FIXTURE_SHOTS=<dir>` saves the browser window paused and with the agent working and its trail open, and the
  full-page pictures.
- **Import check:** `FILO_OPEN=browser-import-check` on a data folder of its own, with `FILO_BROWSER_IMPORT_ROOT=<dir>`
  standing in for Application Support (browser/import_check.rs): it makes a Chrome profile there, imports its bookmarks
  and history from the panel, then a passwords CSV it writes, and checks that the CSV is gone, that no file in the
  profile changed, that typing "exa" suggests the bookmark first and ↓ picks it, and that the test site's `/signin`
  gets the saved login offered and filled. A test run's logins go to `local/test-secrets` (or `FILO_SECRETS_DIR`), never
  the Keychain, and its WebKit data store is removed at the end. `FILO_FIXTURE_SHOTS=<dir>` saves the panel, the
  suggestions and the login bar.
- **Chromium, a test engine** (`dev` builds only; browser/chromium.rs, chromium/): with `FILO_BROWSER_ENGINE=chromium`
  new tabs open in Chromium through CEF in windowless mode instead of WebKit, and WebKit stays the default.
  `chromium/build.sh` downloads CEF's minimal build (checked against its SHA-1) into `target/cef`, builds its C++
  wrapper and `Filo Chromium.app` (an ObjC++ app, chromium/filo_chromium.mm, with Chromium's helper apps; ~325 MB,
  `FILO_CHROMIUM_APP` names another). Filo starts it with the first such tab, its profile in `local/chromium`, and
  talks to it over its standard input and output, a JSON line each way. A tab is a `Tab` as WebKit's are, with a view of
  Filo's own (`FiloChromiumView`) where the web view would be: parked off the screen or moved into the browser window,
  showing the frames Chromium draws into shared memory and handing the user's mouse and keys to the page. native.rs asks
  `chromium::of` first: page.js runs in an isolated world through DevTools (`Page.createIsolatedWorld`,
  `Runtime.evaluate`), clicks and keys are DevTools input (trusted in the page; keys carry the Mac editing commands
  Chromium leaves to its own view, as Playwright sends them), and a screenshot is the last frame. Dialogs, file
  choosers, downloads and pop-ups come back as events to the same places WebKit's do. `browser-check` and
  `browser-tools-check` pass with it; HTML5 drag and drop isn't done. `FILO_OPEN=chromium-check`
  (browser/chromium_check.rs) measures memory per tab in both engines, tries Google's sign-in in both
  (`FILO_CHECK_GOOGLE_EMAIL`), and with `FILO_CHROMIUM_COOKIES=<a copy of a WebKit Cookies.binarycookies>` puts its
  cookies into Chromium's profile and says which of `FILO_CHROMIUM_SITES` are signed in. `FILO_CHROMIUM_SCALE=1` draws
  pages at 1× (less memory, soft on a Retina screen).

## Scheduled work: reminders, routines and check-ins

They're made by chatting: the MCP server's `create_reminder`, `create_routine` and `schedule_check_in` tools (with
`list_…`, `update_routine`, `complete_reminder`, `snooze_reminder`, `cancel_…`/`delete_routine`) let the coding tool
set them up from a conversation, in the Mac's local time (the tool list tells it the time now). In an agent's
conversation the agent owns what it makes and sees only its own. The Filo agent (the default one in Agents) owns what it
makes too, and also sees and manages what belongs to no agent (made on the Scheduled page or in Filo's own chats, or
given to Filo when its agent was deleted), but not the other agents' items. The Scheduled page lists them all, with done, snooze
and cancel for reminders, a page per routine (active, days, time, instructions, Run now, past runs) and the check-ins
planned. `reminders:createMine` and `routines:createMine` make them from a terminal too (below).

They fire with or without Filo's window:

- **The app**, while it's open, looks every second (`local_backend/scheduled.rs`). It holds `local/scheduler.lock`.
- **A launchd agent**, `~/Library/LaunchAgents/im.filo.desktop.scheduler.plist`, keeps `filo --scheduler` running: the
  app's own binary with no window, which opens the same data and fires what's due whenever the app isn't holding the
  lock (it looks every 5 seconds, or sooner when something is due). The app installs it once something is scheduled
  (the first reminder, routine or check-in, however it was made), so a fresh install adds no background item; once
  it's there, the app reinstalls it when its binary changed, so the helper runs the same code;
  `filo --scheduler status|install|uninstall` manage it.
  It isn't installed with a `FILO_CONFIG_DIR` (checks, tests) unless `FILO_LAUNCH_AGENT=1`, and then under a label of its
  own; `FILO_NO_LAUNCH_AGENT=1` turns it off.

Each due item is claimed in one SQLite write (an IMMEDIATE transaction that moves it on), so it fires once even when
both look at the same moment. A **reminder** is a notification (and the pet's bubble while the app is open) and stays on
the Scheduled page until it's marked done; a repeating one (daily, weekdays, weekly) then moves to its next time at the
same wall-clock time. A **routine** (weekly on days at a time, once, or every 30 minutes to 7 days, optionally within
hours of the day) runs a turn: Filo's in a chat of its own with the routine's instructions as the message (its page
shows what each run sent), an agent's in the agent's conversation. A run more than 3 hours late (the Mac was off) is
skipped and says so, and so is one whose previous run is still working; a run whose process died is marked failed. A
**check-in** is a turn in the conversation that planned it (one planned in the group is the planner's turn there, run
by the app); one missed by more than 6 hours expires. Its message is marked as Filo's (`Message::check_in`; the chat
still shows it), so like a routine's run it may end without sending anything when there's nothing new, and its message
says so. When a scheduled
turn sends something, a notification shows it. Notifications use macOS's notification centre, which only serves a signed
app bundle: run Filo as Filo.app (`scripts/app.sh`, signed with the Mac's Developer ID or Apple Development certificate).
Only Filo.app installs the launchd agent, so the helper runs the bundle's binary and its banners are Filo's too; the
bare binary shows none.

## The database

`local/filo.db` is one SQLite file (crates/filo-local `db`), in WAL mode so the MCP server or a terminal check can use
it while the app is open. Opening it runs the migrations it hasn't had yet (`db::MIGRATIONS`, counted in `PRAGMA
user_version`); a newer Filo's database is refused rather than misread. Notes (crates/filo-local `notes`) are its first
table: cleaned up as the server did (title on one line, 120 characters; body 8000; at most 6 lowercased tags), and a
deleted note is kept 30 days, then dropped when Filo opens; notes an older Filo kept waiting to be saved
(`offline/local/local/notes.json`) are moved into it once, when the app opens. Skills (crates/filo-local `skills`) aren't
in it: they're folders (`local/skills/<name>/`), named in the Agent Skills format (lowercase letters, digits and dashes,
64 at most: "Weekly Report" is saved as `weekly-report`), with when to use it in up to 1024 characters and instructions
up to 20 000; an older Filo's `skills` table is moved into folders once. Memories (crates/filo-local `memories`)
are one line of at most 500 characters with their kind, where they came from and whether you confirmed them, shaped
like the server's; the same statement saved twice is one memory, and forgetting one deletes it. Reminders, routines
(with their runs) and check-ins are in crates/filo-local `schedule`. The Feed's tables are in crates/filo-local `feed`, the artifacts' in crates/filo-local `artifacts`, the apps' in crates/filo-local `apps`. To move another page here: add a module like `notes.rs` with its `SCHEMA`, append that to `MIGRATIONS` (never change a shipped entry),
answer its functions in `local_backend.rs` through `self.db()` / `self.write_db(…)` so live queries rerun, and list them
in `MOVED`.

## Updates

Packaged builds (scripts/package.sh) know their build number and feed: the newest GitHub Release's downloads,
`https://github.com/bitooz/filo-desktop/releases/latest/download`, whose `latest.json` names the newest build and links
its zip in that release (`src/updater.rs`; Release, below, publishes them). Filo checks it 8 s after launch, every hour, after the Mac wakes (the clock
jumping past the updater's 30 s tick) and when the window becomes active, at most every 10 minutes; the sidebar's
button above Settings checks it too. A newer build downloads in the background into `.filo-update-<build>/` next to the
app, where its size and SHA-256 are checked and `codesign` checks that it's signed by the same key as the running app.
The button then says "Restart to update", and one silent banner per build says it's ready, with a Restart button
(notifications.rs `show_update`; `updates.json` in the config folder keeps the last build announced and the switch
below). A restart waits for chat replies under way (`coding:keepingAwake`'s turns), swaps the bundle and opens the new
one; coding tasks and replies in workers carry on and the new Filo picks them up. Quit with a build waiting and the
app starts `filo --install-update` as it quits: in a session of its own, it waits for the app to go and for reply
workers to finish (at most 2 h), then swaps the bundle, unless Filo opened again meanwhile (`local/coding.lock`); it
logs to `updates.log` in the config folder. Settings › App › "Download updates automatically" turns the background
download off: the button then says "Update", and the banner says "Filo 0.1.0 (N) is available" with an Update button; either
downloads, installs and restarts at once. It's one banner per build either way: a build announced as available isn't
announced again once it's downloaded (`updater.rs` `announce`).

`FILO_SMOKE=update` (a bundle built with `FILO_CHANNEL`, `FILO_BUILD` and `FILO_UPDATE_FEED` pointing at a test feed)
waits for the launch check's download, takes the picture `update-ready` and clicks Restart (`FILO_UPDATE_THEN=quit`
quits through the menu instead; `banner-update`, with automatic downloads off in `updates.json`, waits for the build to
be found and presses the banner's Update through the channel the banner uses, `notifications::update_action`); `FILO_UPDATE_NEXT_QUIT=<ms>` has the restarted Filo quit itself that long after it opens.
A restart passes the run's `FILO_*` variables on, except `FILO_SMOKE` and `FILO_OPEN`, so a test run stays on its own
data folder; give a test bundle a bundle id of its own, or `open` may bring up the user's Filo instead.

## Checks and releases

GitHub Actions run two workflows (`.github/workflows`), neither on a schedule.

**Checks** (`checks.yml`) runs on every pull request and every push to main, and needs no secret: `cargo fmt --all
--check` (rustfmt.toml: 120 columns), `python3 scripts/third-party-notices.py --check`, `cargo clippy --workspace
--all-targets -- -D warnings` and the same without `dev` (`--no-default-features`, the packaged app's build), and `cargo
test --workspace`. Tests that need a signed-in coding tool, the user's data or a key are `#[ignore]`d and stay out of
it. The lints the workspace allows (shape, not mistakes: `too_many_arguments`, `type_complexity`, …) are in Cargo.toml's
`[workspace.lints.clippy]`. Both workflows use one Rust release (`RUST_TOOLCHAIN`, 1.90.0): clippy's lints change from
one release to the next, so moving to a newer one is a change of its own, with whatever its new lints ask for. A few of
filo-local's tests go by the clock (fake CLIs given a second or less in engine.rs, two chats made in the same
millisecond in `store::tests::chats_round_trip_and_sort_by_activity`) and can fail on a busy machine; run them again on
their own. filo-local turns on serde_json's `preserve_order`, as the app's build does anyway (gpui does, and Cargo
unifies features): its tests see JSON's keys in the order they're written whether it's built alone (`-p filo-local`) or
with the app (`--workspace`), so the fake coding tools can match what's sent as text.

**Release** (`release.yml`) runs scripts/package.sh on a macOS runner for Apple silicon and Intel in one app, signed with
the Developer ID identity, notarized and stapled, and publishes `Filo.dmg`, `Filo-<build>.zip` and `latest.json` as a
GitHub Release, marked as the latest. That release is what people get: installed apps read
`…/releases/latest/download/latest.json` (the feed package.sh builds in) and download the zip it links in its own
release (`RELEASE_URL`), and the website's Download button (and filo.im/download) is
`…/releases/latest/download/Filo.dmg`, which is why the disk image's name has no build number. Start it from Actions ›
Release › Run workflow with a build number (the release is tagged `v<version>-<build>`, e.g. `v0.1.0-21`, on that
commit), or push a tag of that form. Installed apps update to any higher build, so each release takes a number above
the last one published. Stapling the notarization ticket (and package.sh's check that it's there) goes over the network
to Apple, so package.sh tries it up to five times, a minute apart, before failing the release. Before publishing, the
workflow checks that the app is universal, signed, stapled and accepted
by Gatekeeper, and that `latest.json` names this build and its zip in this release. The downloads need the repository
to be public: GitHub serves a private repository's release files only to people signed in to it.

The release needs these repository secrets (Settings › Secrets and variables › Actions, or `gh secret set <name>`):

| Secret | What it is, and how to make it |
| --- | --- |
| `MACOS_SIGNING_P12` | The Developer ID Application certificate with its private key, base64: in Keychain Access, select the certificate under My Certificates, File › Export Items… as .p12 with a password, then `base64 -i Filo.p12 \| pbcopy`. It must be the identity earlier builds were signed with: an installed app only takes an update signed with the same key. |
| `MACOS_SIGNING_P12_PASSWORD` | The password given to that export. |
| `APPLE_NOTARY_KEY` | An App Store Connect API key, base64: App Store Connect › Users and Access › Integrations › App Store Connect API › Team Keys, add a key with Developer access, download its `AuthKey_<id>.p8` (it downloads only once), then `base64 -i AuthKey_<id>.p8 \| pbcopy`. |
| `APPLE_NOTARY_KEY_ID` | That key's Key ID, from the same page. |
| `APPLE_NOTARY_ISSUER` | The Issuer ID shown above the keys on that page. |
| `FILO_POSTHOG_KEY`, `FILO_POSTHOG_HOST` | Optional: the PostHog project for the anonymous usage counts ([telemetry.md](telemetry.md)). Without a key the release sends none. |

The workflow stops with a message when one of the first five is missing or the .p12 holds no Developer ID Application
identity.

## From a terminal

```bash
export FILO_CONFIG_DIR="$(mktemp -d)"                                       # a data folder of its own (or a copy of ~/.config/filo)
cargo run -- --check agents:list                                            # any backend function, answered locally
cargo run -- --check agents:create '{"name":"Tutor","instructions":"…"}'
cargo run -p filo-local -- send --engine claude "hello"                     # one turn from a terminal
cargo run -- --check reminders:createMine '{"title":"Stretch","dueAt":1791000000000}'
cargo run -- --scheduler status                                             # the background helper (also install|uninstall)
cargo run -- --record clip.m4a                                              # what Filo's relay hears in a recording (clip.wav: streamed live)
```

`--check`, `--record`, `--update` and `--avatar-colors`, the scripted checks below and the test windows are the `dev`
feature (Cargo.toml): every build has them but the packaged app's, which scripts/package.sh builds without it (and, for a
release, without paths from this Mac and without symbols). `--check`, `--record` and `--app-icon` for an avatar run only
on a `FILO_CONFIG_DIR` other than ~/.config/filo, and say so otherwise: opening the data migrates filo.db to the build's
schema, which a build of another commit mustn't do to the user's. `--update`, `--avatar-colors` and `--app-icon` for one
of the icons to pick open no data. An option the app doesn't know stops with a message rather than open the app.
`scripts/app.sh` passes every `FILO_*` variable to the app, and gives a test run (`FILO_OPEN`, `FILO_SMOKE`, a drive
or shot switch) a fresh data folder when it names none.

Scripted checks: `FILO_OPEN=agent:<id>`, `chat:<id>` or `group` (the agents' group chat, made if need be) opens that
conversation, and with `FILO_SEND="text"` sends a
message in it through the backend, as the composer does. In `dev` builds, `FILO_WELCOME_KEYS="right 1 enter"` presses
those keys in the tour, a second and a half apart, `FILO_TOOL_STATUS="claude=missing codex=signin"` shows the tools in
those states, and `FILO_WINDOW_FLOAT=1` keeps the window above others without taking the focus. `FILO_OPEN=<page>` with `FILO_SHOT_AFTER=4:<file.png>` takes a picture of a page and quits; several, `;` between
(`3:loading.png;9:loaded.png`), picture it as it changes.
Test windows take their pictures in the app, from the window's own frame (`src/window_shot.rs`): `FILO_FIXTURE_SHOTS`
and `FILO_DRIVE_SHOTS` name the folder. A test run (any of these switches, or a `FILO_CONFIG_DIR` of its own) never
captures the screen or asks macOS for a permission (`src/test_run.rs`; AGENTS.md has the rules for coding agents).
Smaller knobs, each documented where it's read: `FILO_DRIVE_PAUSE` and `FILO_DRIVE_AWAY` (feed-drive),
`FILO_SEARCH_MODES` (search-button-drive), `FILO_SCROLL_DEBUG` and `FILO_SCROLL_ROWS` (chat-scroll) and
`FILO_LOCAL_HUNG_AFTER` (`filo-local send`). An unknown `FILO_OPEN` name opens nothing and says so.

## Layout

- `src/main.rs`: `--local-mcp`, the terminal checks, then the window. `src/app.rs`, `src/nav.rs`: the tab sidebar,
  pushed pages and the tour over them.
- `src/backend.rs`, `src/local_backend.rs`: the backend on this Mac (above); `src/local_backend/scheduled.rs` and `src/launch_agent.rs`: scheduled work and its background helper. `crates/filo-local`: chat storage, the MCP
  server with Filo's tools (`send_message`, the memory tools, …) and the Claude Code / Codex runner.
- `src/screens/`: one module per page; `chat/` holds the conversation the Agents tab shows.
- `src/companion.rs`, `src/menu_bar.rs`, `src/pet.rs`, `src/hotkeys.rs`: Filo outside its window.
- `src/theme.rs`, `src/ui.rs`: palettes (`assets/themes.json`), type scale and the design-system components.
- `src/web_holes.rs`: the native views in the window (artifact pages, the note editor, a file in the viewer, videos;
  glance pages do the same in `screens/chat/glance.rs`) are over everything GPUI draws. After each frame that drew
  one, it's cut where GPUI painted after it, over it (gpui's `Window::painted_over`): a menu, tooltip, notification
  card, toast, dialog or anything added later shows whole there and takes its clicks, with no mark of its own. A
  dialog's dim lies over the page as a tint that takes the clicks too, and a dialog opening takes the keyboard from a
  page, so Esc reaches it. A see-through gradient lies over it as that gradient (a `CAGradientLayer`) and lets the
  clicks through: the fade under a page's header (`ui::sub_page`'s `ui::header_fade`) shows over an artifact's page as it scrolls, as
  over any other page. The artifact page's Shared bar, which doesn't scroll, is part of its top, above the fade. `FILO_OPEN=overlays-drive:<artifact id>,<video file>` opens each kind of overlay over each
  kind of view and checks it shows, AppKit sends a click there to GPUI, and real clicks land.
- `vendor/gpui-component`, `vendor/gpui`: our copies of gpui-component 0.5.1 and gpui 0.2.2 with Filo's fixes (each
  has a `FILO.md`). `computer-use/`: the Swift helper behind `use_computer`, with its own vendored code.
- `assets/`: icons, fonts, and Filo's avatars (`assets/images/filo/{avatars,workers,avatar-colors}`).
