# Scheduled, routines, notes, memories and your name

**Older spec.** Written in October 2026 to port these pages from the phone app, and trimmed on 2026-10-13 to what
this app still does. The code is the source of truth: where they differ, the code is right.

## 0. How the pages get their data

- The pages call functions by name, as queries and mutations (`notes:mine`, `routines:updateMine`), through
  `backend::subscribe` and `backend::call` in `src/backend.rs`. `src/local_backend.rs` answers them on this Mac; the
  scheduling ones are in `src/local_backend/scheduled.rs`.
- The data is in Filo's database (`crates/filo-local/src/db.rs`): notes in `notes.rs`, memories in `memories.rs`,
  reminders, routines and check-ins in `schedule.rs`.
- A subscribed query runs again whenever the data changes, also when another process wrote it (Filo's tools in a turn,
  the background scheduler).
- Ids are UUID strings.
- A row leaves out an optional field that has no value. It never sends it as `null`.

## 1. Scheduled

`src/screens/scheduled.rs`. It opens from Settings › Personal › Scheduled, whose subtitle is "{n} upcoming" (pending
reminders plus active routines), and from the reminders under "Up next" in the menu bar.

### Data

| Function                 | Args              | Returns                                                                                       |
| ------------------------ | ----------------- | --------------------------------------------------------------------------------------------- |
| `reminders:myReminders`  | `{}`              | Pending reminders, the latest `dueAt` first, at most 100.                                     |
| `routines:myRoutines`    | `{}`              | Every routine, the oldest first.                                                              |
| `agenda:myWakes`         | `{}`              | Pending check-ins, the soonest first.                                                         |
| `reminders:completeMine` | `{ id }`          | `boolean`. A repeating reminder moves to its next time, at the clock time of its first one, and stays listed. Any other is finished. |
| `reminders:snoozeMine`   | `{ id, minutes }` | `boolean`. Due again `minutes` from now, in whole minutes, from 1 minute to 7 days.           |
| `reminders:cancelMine`   | `{ id }`          | `boolean`.                                                                                    |
| `agenda:cancelWakeMine`  | `{ id }`          | `boolean`.                                                                                    |

The reminder mutations return `true` for a reminder that's already done or cancelled, and `false` for one that's
gone. The rows are `Reminder`, `Routine` and `Wake` in `crates/filo-local/src/schedule.rs`, with the owning agent's
name added as `agentName` (`named` in `src/local_backend/scheduled.rs`). A routine without `schedule` runs weekly on
`days` at `minuteOfDay`, in this Mac's time zone. The page also reads `agents:list` for the owners' names, pictures and
colours: what's no agent's (no `agentId`) is the main agent's.

### Layout

- Header "Scheduled". A skeleton shows until reminders and routines have loaded; an error shows as a red banner.
- **Empty** (no reminders, routines or check-ins): a calendar-clock icon, "Nothing scheduled yet." and "Ask Filo to
  remind you of something, or to do a job on a schedule, like a morning inbox brief."
- **Owners.** Once an agent other than the main agent owns a reminder or routine, each owner gets a heading with its
  picture and name: the main agent first, then the others in the Agents tab's order, each with its routines and
  reminders under it. Otherwise there are no headings.
- **Routines** (section "Routines", when there are any): a repeat icon, the title, and a chevron; a click opens the
  routine's page. The subtitle is `format_routine_schedule` (`src/screens/routine_display.rs`), e.g. "Weekdays · 8:00
  AM", "Every 2 hours · 9:00 AM–5:00 PM" or "Once · Thu, Oct 2, 3:45 PM", with `Paused · ` in front when paused.
- **Reminders**, labelled "Reminders" only when routines are listed too. Each row has:
  - on the left, a 26 px ring ("Mark done"). A click fills it with a check, strikes the text through and dims it, then
    calls `reminders:completeMine` 420 ms later;
  - the title in bold, the note in at most two lines, and a caption `{when} · {delivery}`, red once `dueAt` has
    passed. `when` is `format::reminder_due` (`src/format.rs`), e.g. "Today · 3:07 PM", "Tomorrow · 9:00 AM", "12 min
    overdue · 2:50 PM" or "Overdue · Oct 2". `delivery` is "notification on this Mac", or "alarm on this Mac";
  - on the right, Snooze (a clock) and Cancel (an X).
- **Check-ins** (section "Check-ins", below everything else, when there are any; footer "Times Filo and your agents
  planned to come back to a conversation."): a bell icon, the title, and `{who} · {when}`, with ` · {note}` when there
  is one. Its X asks "Cancel check-in?" with the title as the message ("Cancel check-in" or "Keep"), then calls
  `agenda:cancelWakeMine`.
- The time the rows compare against refreshes every 30 s.

### Actions

- Snooze opens "Snooze reminder" / "When should Filo remind you again?" with Cancel, "10 minutes" and "1 hour".
- Cancel asks "Cancel reminder?" with the title as the message: "Cancel reminder" (destructive) or "Keep".
- While a reminder is being changed, its ring shows a spinner and its buttons are off. Done doesn't start while another
  reminder is busy.
- Any answer but `true` shows "Couldn’t update reminder" with "Filo couldn’t complete this reminder. Please try
  again.", "Filo couldn’t snooze this reminder. Please try again." or "Filo couldn’t cancel this reminder. Please try
  again."
- The page makes nothing. Filo and the agents make reminders, routines and check-ins in a conversation, with Filo's
  scheduling tools (`crates/filo-local/src/schedule_mcp.rs`). Feed topics are routines too (`src/local_backend/feed.rs`).

### When something comes due

The loop is `start` in `src/local_backend/scheduled.rs`; what's due is `claim_due` in
`crates/filo-local/src/schedule.rs`.

- The app runs the schedule while it's open. While it's closed, the background helper does (`filo --scheduler`, kept
  running by launchd, `src/launch_agent.rs`). Each item is handed out once.
- A reminder becomes a notification: its title, with its note (or "Reminder", or "Alarm") as the body. The pet shows it
  as a bubble with done and snooze (10 minutes), and the menu bar lists the next three under "Up next"
  (`src/companion.rs`).
- A routine's run is a turn: in the agent's conversation for an agent's routine, else in the routine's own chat, made
  on its first run. It waits up to an hour for that conversation to be free.
- A run more than 3 hours late is skipped ("Missed its time."), and so is one whose previous run is still working ("The
  previous run was still working.").
- A check-in is a turn in the conversation it was planned in.

## 2. Routine page

`src/screens/routine.rs`, opened from a routine's row on Scheduled.

### Data

- `routines:myRoutine` `{ id }` returns `{ routine, runs }`, or `null` when the routine is gone. `runs` are its runs,
  the newest first, at most 20 (`Run` in `crates/filo-local/src/schedule.rs`). A run's `state` is `queued`, `active`,
  `completed` or `failed`; `summary` is what it sent the user (at most 4000 characters) and `error` what went wrong.
- `routines:updateMine` `{ id, title?, instructions?, days?, minuteOfDay?, status?, schedule? }` returns `boolean`
  (`update_routine`):
  - title: one line, 1–120 characters;
  - instructions: trimmed, 1–3000 characters;
  - days: whole numbers 0–6 (0 is Sunday), at least one, repeats dropped;
  - minuteOfDay: a whole number 0–1439;
  - status: `active` or `paused`;
  - schedule: as `routines:createMine` takes it, or `null` for weekly again.

  The next run is worked out again when the days, time, schedule or status change, and a paused routine has none. On a
  once or interval routine, days and time change only the stored weekly fields.
- `routines:runNowMine` `{ id }` returns the run's id, or `null` when the routine is gone. While a run is going it
  throws "The previous run was still working."
- `routines:deleteMine` `{ id }` returns `boolean`. It deletes the routine and its runs; the conversations they ran in
  stay.

### Layout

- Header: the routine's title, or "Routine" while it loads. A skeleton shows while loading; `null` shows "This routine
  isn’t available." / "It may have been deleted."
- **Schedule** section, with the footer "Skipped last time: {lastSkipReason}" when there's one:
  - "Active", with a switch that sends `status`. Its subtitle is "Next run " and `reminder_due(nextRunAt)` with the
    first letter lowercased (e.g. "Next run today · 8:00 AM"), or "Paused".
  - Seven day chips, Monday first, lettered S M T W T F S. A click toggles the day and sends `days`; taking off the
    last day does nothing.
  - "Time", with the time. A click opens hour and minute steppers (minutes in fives), AM/PM, Cancel and "Set time",
    which sends `minuteOfDay` if it changed. Esc cancels.
- **Instructions** section, footer "Filo follows these each time the routine runs. It only sends, deletes or shares
  things when they say so.": a "Name" field (at most 120 characters), an instructions field ("What should Filo do?",
  at most 3000), and "Save changes" when either differs from the routine, trimmed. It's off while either is blank.
- **Run now** (a play icon) calls `routines:runNowMine`.
- **Runs** section, footer "Results show up here after the first run." while there are none. A row's title is when
  the run started (`format::weekday_date_time`, e.g. "Thu, Oct 2, 3:45 PM"). Its subtitle is the state ("Ready",
  "Working…" or "Failed": `routine_run_label` in `routine_display.rs`) and the first line of the summary or error. An
  agent's run opens the agent's conversation. Filo's own chats have no page, so a run of Filo's opens in place and
  shows what it sent, or the error.
- **Delete routine** (danger, a trash icon) asks Delete “{title}”? / "Past results stay available." ("Delete" or
  "Cancel"), then deletes it and goes back.
- A failure shows "Couldn’t update routine" with "Filo couldn’t change this routine. Please try again." (Active),
  "Filo couldn’t change the days. Please try again.", "Filo couldn’t change the time. Please try again.", "Filo
  couldn’t save your changes. Please try again." or "Filo couldn’t delete this routine. Please try again.". Run now
  shows the reason it threw, or "Routine is no longer available".

### Making a routine

Not on this page. `routines:createMine` `{ title, instructions, days?, minuteOfDay?, schedule?, agentId? }` returns
the routine; it, Filo's `create_routine` tool and the Feed's topics all go through `create_routine` in
`crates/filo-local/src/schedule.rs`.

- `schedule` is `{ kind: 'once', at }` or `{ kind: 'interval', everyMinutes, fromMinute?, toMinute? }`. A once time
  must be in the future. `everyMinutes` is a whole number from 30 to 10080. `fromMinute` and `toMinute` go together.
- `days` and `minuteOfDay` are checked as in `routines:updateMine`. They're needed without a `schedule`; with one and
  without them, the days are every day and the time is midnight.
- There can be 30 routines: 10, plus one per Feed topic (`MAX_ROUTINES`). Past that it throws "Routine limit reached
  (30). Delete one first."

## 3. Notes

The Notes tab is `src/screens/notes.rs`. The note page is `src/screens/note.rs`, which shows a web page in the app's
web view (`src/screens/note_page.rs`, `web/note.html`, `web/note-editor.ts` built into `assets/web/note-editor.js`).

### Data

`crates/filo-local/src/notes.rs`. A note is `{ _id, title, body, tags, pinned, createdAt, updatedAt }`; `tags` (a
list) and `pinned` are always there.

| Function              | Args                                 | Returns                                                                    |
| --------------------- | ------------------------------------ | -------------------------------------------------------------------------- |
| `notes:mine`          | `{}`                                 | The notes not deleted, the most recently changed first, at most 500.      |
| `notes:getMine`       | `{ id }`                             | The note, or `null` when it's deleted or unknown.                          |
| `notes:createMine`    | `{ title, body, tags?, pinned? }`    | The new note's id.                                                         |
| `notes:updateMine`    | `{ id, title, body, tags?, pinned? }` | `boolean`. Replaces the title, body and pin (missing is off) and moves the note to the top. Tags missing or empty leave its tags as they are. |
| `notes:setPinnedMine` | `{ id, pinned }`                     | `boolean`. Doesn't change `updatedAt`.                                     |
| `notes:deleteMine`    | `{ id }`                             | `boolean`. Marks the note deleted. Once 30 days have passed, it's dropped for good the next time Filo or its background helper starts (`purge_deleted`). |
| `notes:search`        | `{ query, tag? }`                    | The notes whose title, body or tags contain `query` (case and runs of spaces don't matter), with `tag` if given. No page calls it today. |
| `notes:tags`          | `{}`                                 | `[{ tag, count }]`, the most used first. No page calls it today.           |

`false` means there's no such note. Every note is cleaned up when it's saved:

- title: on one line (runs of spaces and line breaks become one space), trimmed, at most 120 characters;
- body: `\r\n` and `\r` become `\n`, trimmed, at most 8000 UTF-16 units (as the note page counts);
- tags: lowercased, on one line, at most 24 characters each, no empty ones or repeats, at most 6.

Filo and the agents read and change the same notes in a conversation with Filo's notes tools (`list_notes`,
`read_note`, `create_note`, `update_note`, `delete_note`: `crates/filo-local/src/notes_mcp.rs`).

How the note page's saves reach the database is in `src/screens/note.rs`.

### Notes tab

- Header "Notes" with Search and New (a plus). Search swaps the header for a field ("Search notes", with a clear
  button) and "Cancel", which clears and closes it; Esc does the same.
- Pinned notes come first, then the rest, each group the most recently changed first.
- Search runs the query's spaces together and lowercases it. A note matches when its title, body and tags, joined
  with line breaks and lowercased, contain it.
- A pin shows as soon as it's clicked. That shown pin goes once the stored note agrees or is gone, and is taken back
  when the call doesn't return `true`.
- A row opens the note page:
  - line 1: the heading in bold, on one line: the title, else `note_preview(body)`, else "Untitled". On the right, the
    date: `format::short_date_time(updatedAt)`, e.g. "Oct 2, 3:45 PM";
  - line 2: the preview, in at most two lines, when there is one and it isn't the title;
  - line 3: the tags, as `#tag` pills;
  - on the right, the pin ("Pin note" or "Unpin note"), filled when pinned.
- `note_preview` (`src/screens/note_text.rs`) is one line of plain text, at most 240 UTF-16 units. A body of only
  checklist lines shows its first four items as "☐ text" or "☑ text", two spaces apart. Any other body loses its
  Markdown.
- Right-click: "Share to agent", with the agents, when there are any. The agent's conversation opens with the note
  above its composer.
- While loading, skeleton rows. With no notes: a notebook-pen icon, "A place to put things down." and "Jot anything you
  want to keep — ideas, lists, thoughts. Your notes will appear here." With no search hits: No notes match “{query}”.

### Note page

A new note starts empty; an existing one is read with `notes:getMine`.

- **Header:** Back ("Back to notes"); in read mode, the save's status; Pin; Delete (a trash icon), once the note
  exists; and "Edit". While editing, "Edit" is "Save", which reads "Saving…" or "Saved" as autosave goes.
- **Banner:** "This note is too long to save. Shorten it a little to keep your changes."
- **Unavailable:** "Note unavailable" / "This note was deleted or couldn't be loaded."
- **Read mode** (an existing note opens in it): the title with a rule under it (neither when the title is empty), and
  the body as Markdown (markdown-it, no raw HTML, URLs that have a scheme linked).
  - A task item has a box. Clicking it flips the box on that source line (`toggle_task_line` in `note_text.rs`) and
    saves.
  - Clicking a link opens it when it's http, https or mailto.
  - A double-click, not on a link or box, starts editing.
  - An empty body says "Nothing here yet. Double-click to start writing."
- **Edit mode** (a new note starts in it, with the title focused):
  - the title: "Title", at most 120 characters. Enter goes to the body;
  - the body: a rich-text editor (TipTap, with react-native-enriched-html's formats) that reads and writes Markdown,
    with the placeholder "Start writing…". A body it can't hold as rich text is edited as plain Markdown. Typing `- `,
    `* `, `1. `, `[] ` or `> ` at the start of a line makes a list, a checklist or a quote;
  - a toolbar over a selection: Bold, Italic, Strikethrough, Link, Heading, Bullet list, Numbered list and Task list.
    Link asks for one ("Paste or type a link"); a link without a scheme gets `https://`. ⌘-click opens a link;
  - a body over 8000 UTF-16 units isn't taken: the too-long banner shows and the last body stays;
  - "Save" saves and goes back to read mode, except while the body is too long.
- **Autosave** (`note.rs`):
  - a save runs 1.5 s after the last change, and at most 10 s after the first unsaved one;
  - also when focus leaves the page, when the window goes to the background, and on leaving the page;
  - there's no save when the title and body are both empty, or when nothing changed since the last one (title, body,
    pin and tags).
- **Pin:** shows at once, and doesn't count as a change, so autosave doesn't send it again. It's taken back if the call
  fails.
- **Delete:** asks "Delete note?" with the title, or the first 60 UTF-16 units of the preview, as the message ("Delete"
  or "Keep"). The note goes at once and the page goes back.
- Leaving a new note that was saved and is now empty deletes it.

## 4. Memories

The page is `src/screens/memories.rs`, opened from Settings › Personal › Memories. The data is in
`crates/filo-local/src/memories.rs`.

### Data

```ts
type Memory = {
  _id: string;
  _creationTime: number; // the same as createdAt
  type: 'preference' | 'fact' | 'goal' | 'relationship' | 'life_event' | 'ongoing_situation' | 'other';
  content: string; // one line, 1–500 characters
  provenance: {
    source: 'user_explicit' | 'conversation' | 'correction' | 'import';
    sourceId?: string; // the conversation it was saved in
    explanation: string; // one line, at most 200 characters
  };
  confirmation: 'inferred' | 'confirmed' | 'corrected';
  createdAt: number;
  updatedAt: number;
};
```

An unknown `type` is saved as `other`, an unknown `confirmation` as `inferred`.

| Function          | Args                                        | Returns                                                                             |
| ----------------- | ------------------------------------------- | ----------------------------------------------------------------------------------- |
| `memories:list`   | `{ paginationOpts: { numItems, cursor } }` | `{ page, isDone, continueCursor }`, the most recently changed first. `numItems` is 1–200; `cursor` is `null` or `""` for the first page. `continueCursor` is `<updatedAt>:<id>` of the page's last memory, `""` when done. |
| `memories:create` | `{ content, type? }`                        | The id. Saved as the user's own: `user_explicit`, "You added this", `confirmed`.     |
| `memories:update` | `{ id, content, type? }`                    | `boolean`. Marks it `corrected` and brings it to the top.                           |
| `memories:remove` | `{ id }`                                    | `boolean`. Deletes it outright: nothing is kept.                                    |

The same statement saved again (case and spacing aside) isn't a second memory: the one there comes to the top and
keeps the stronger confirmation.

Each turn is told the memories, newest first, up to about 8000 characters, with their ids (`prompt_block`). Filo
saves, changes, forgets and looks them up with its memory tools (`save_memory`, `update_memory`, `forget_memory`,
`search_memories`: `crates/filo-local/src/mcp.rs`).

### Layout

- Header "What your agents remember", with a plus ("Add a memory").
- Intro: "Each memory keeps an explanation of where it came from. You can ask your agents to correct one in
  conversation, or add, edit and remove them here."
- Memories come 30 at a time. "Show older memories" loads 30 more while there are more, with a spinner while it loads.
  Every loaded page stays live.
- A row, at least 82 px tall: the content, semibold; the caption `{explanation, or "Saved from a conversation"} ·
  {"Not yet confirmed" when inferred, else "Confirmed"}`; Edit (a pencil); and Forget (a red trash icon), which calls
  `memories:remove` at once, with no confirmation.
- The editor takes the row's place, or sits at the top for a new memory: "Something your agents should remember about
  you", a `{n}/500` count, Cancel, and "Save" ("Remember" for a new one), which is off until the trimmed text isn't
  empty and has changed. Enter saves; Esc cancels.
- With no memories: "Your agents haven’t saved anything about you yet."
- A loading error shows as a red banner. Failures show "Couldn’t forget this memory" or "Couldn’t save this memory";
  an edit to a memory forgotten meanwhile says "It was forgotten in the meantime."

## 5. Your name

`src/screens/name.rs`, opened from the name row at the top of Settings (a letter avatar, the name, and "On this Mac,
no account").

- `users:current` `{}` returns the profile (`profile` in `src/local_backend.rs`). Until a name is saved, `userName` is
  this Mac's login name with a capital first letter, or "You".
- `users:updateUserName` `{ userName }` returns `null` and saves the name trimmed. It doesn't check the length; the
  page does.
- A skeleton shows until `users:current` answers; then the field holds the name, focused.
- Header "Your name" with a Save button; a "Name" label; the field, which is an agent's name field on the Agents page
  (`name_input`, `name_field` and `NAME_MAX` in `src/screens/agents.rs`), with the placeholder "Your name".
- Save (the button or Enter) works when the trimmed name is 1–`NAME_MAX` characters, differs from the saved one, and
  nothing is saving.
- On success the page goes back. On failure it says "Couldn’t save your name. Try again." in red.
