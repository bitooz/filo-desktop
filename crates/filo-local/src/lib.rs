// Filo's local chats: no Filo account or hosted backend, the user's own coding tool (Claude Code, Codex, Muse Code or
// OpenCode) answers.
//
// - `coding`: the Coding page in it: tool settings, project folders (found and added), tasks and their history, and
//   the command lines and steps of a coding task's Claude Code, Codex, Muse Code or OpenCode.
// - `coding_handoff`: coding tasks a conversation handed off (start_work) and owns: what the tool asks, notes for it,
//   each round's result and proof, and what the conversation is told; `coding_mcp`: the tools that start and follow
//   them; `coding_session`: a round of such a task with its tool kept talking to, so it can ask (Muse Code's protocol
//   in `coding_muse`, OpenCode's in `coding_opencode`).
// - `group`: the group chat of Filo and the user's agents in it: its messages, its queue of turns, who the user's @Name or a member's ask list asks,
//   and a member's turn of its coding tool; `group_route`: the quick call to the model that picks who answers a post
//   naming no one; `agent_asks`: an agent asking another while the group is off, each in its own chat.
// - `db`: Filo's SQLite database on this Mac (`filo.db` beside the chats) and its migrations, for the pages' data.
// - `notes`: the notes in it (the Notes tab); `notes_mcp`: the tools that list, read, write, change and delete them.
// - `pages`: whether the optional pages, Feed and Notes, are on (Settings › Pages): off, their agent tools go too.
// - `feed`: the Feed in it: topics (each watched by an agent's routine), posts and settings; `feed_mcp`: the tools that
//   publish and change posts (in every conversation, or on their own for a coding tool's session).
// - `skills`: the user's skills, a folder each beside it (the Skills page), and what a turn gets of them.
// - `memories`: what Filo remembers about the user in it (the Memories page, and every turn's prompt).
// - `models`: the composer model picker's rows in it, as the CLIs last named their models and defaults.
// - `schedule`: reminders, routines and Filo's own check-ins in it, and what comes due when; `schedule_mcp`: the
//   tools that make and change them.
// - `app_data`: the app's own small data in it: the agents, the profile, read markers, pinned and archived chats and
//   uploaded pictures, a row each, copied in once from the JSON files they were; `backup`: its daily copies.
// - `main_agent`: who the user's main agent is (the profile's pick, in it): it leads the group, answers Ask on Feed posts
//   and owns what belongs to no agent.
// - `markdown`: a message's Markdown as one line of plain text (the chat list's preview, notifications).
// - `store`: chats and their messages as files on this Mac (`~/.config/filo/local`, or `FILO_LOCAL_DIR`).
// - `attachments`: files sent with a message, saved beside the chats (an agent's shown where they are, found again
//   with their `bookmark` when they move), and what a turn's prompt says of them.
// - `mcp`: the stdio MCP server the CLI starts for each turn, with Filo's tools (the list is at its top). A reply
//   reaches the chat only through its `send_message`.
// - `engine`: runs a turn's CLI (`claude -p`, `codex app-server`, Muse Code's or OpenCode's) with that server plugged in,
//   reads its JSON events and stops it on request.
// - `apps`: the user's apps in it (Extensions › Apps): MCP servers, local or remote, that every turn's Claude Code or
//   Codex gets; `apps_client` reaches them (checks, and the proxy a turn runs for each), `apps_oauth` signs in to an
//   account, `secrets` keeps their keys and tokens in the Keychain.
// - `computer`: the `use_computer` tool chat turns get while "Your computer" is on, on a server of its own, which works
//   the apps on this Mac through Filo's Swift helper.
// - `artifacts`: the reports, pages, diagrams and tools agents make, in it and beside it as HTML files (the Artifacts
//   tab), with their versions and the conversations they came from; `artifacts_mcp`: the tools that make and change them;
//   `artifact_share`: the optional "Share to claude.ai", which lets Claude Code publish one to the user's account;
//   `artifact_images`: the web pictures a page names, saved beside it so the viewer and its shares show them.
// - `browser_mcp`: the `browser_*` tools, which work web pages in the app's own browser (src/browser.rs) through a
//   socket in the local data folder, while the app is open; `browser_import`: the user's bookmarks, history and
//   exported logins brought in from Helium or Chrome.
// - `mentions`: "@computer" and "@browser", which a message can name like an app, and what a turn is told of them.
// - `characters`: the user's own characters, desktop pets an agent drew (the app's custom avatars); `characters_mcp`:
//   the `generate_images` and `save_pet` tools that make them.
// - `glances`: the small visuals Filo shows above its reply in a chat, in it; `glances_mcp`: the `show_glance` tool
//   that makes them; `glance_document`: a glance's page, as the app shows it and Artifacts keeps it.
// - `chat_activity`: what a running turn is doing ("Running a command"), from its CLI's events, for the line beside
//   the chat's typing dots.
// - `compaction`: when a session went cold (idle past the CLIs' prompt cache, and big): its size and age, and the
//   compaction of a coding task's before it's resumed, so the round doesn't resend all of it at full price.
// - `reseed`: the fresh session a cold chat, a changed engine or a lost session starts instead, seeded with the chat's
//   own newest messages word for word.
// - `failures`: the recent failed turns and coding rounds with the CLI's stderr, for the app's problem reports.
// - `turn`: one turn end to end (prompt, run, session kept for the next turn, errors saved in the chat), shared by
//   the app and the `filo-local` command.
// - `steer`: messages the user sends while a turn answers, taken into it rather than waiting.
// - `plan_usage`: how much of Claude Code's and Codex's plan is left (the 5-hour window and the week), from their
//   accounts, for the Coding page.
// - `turn_state`: how a chat's turns ended (a failure stands until a turn there goes through), and when a usage
//   limit's failure can be tried again.

pub mod agent_asks;
pub mod app_data;
pub mod apps;
pub mod apps_client;
pub mod apps_oauth;
pub mod artifact_images;
pub mod artifact_share;
pub mod artifacts;
mod artifacts_mcp;
pub mod attachments;
pub mod backup;
mod bookmark;
pub mod browser_import;
pub mod browser_mcp;
pub mod characters;
mod characters_mcp;
pub mod chat_activity;
pub mod coding;
pub mod coding_handoff;
mod coding_mcp;
pub mod coding_muse;
pub mod coding_opencode;
pub mod coding_session;
pub mod compaction;
pub mod computer;
pub mod db;
pub mod disclaim;
pub mod engine;
pub mod failures;
pub mod feed;
pub mod feed_mcp;
pub mod files;
pub mod glance_document;
pub mod glances;
mod glances_mcp;
pub mod group;
pub mod group_route;
pub mod lock;
pub mod main_agent;
mod markdown;
pub mod mcp;
pub mod memories;
pub mod mentions;
pub mod models;
pub mod notes;
mod notes_mcp;
pub mod pages;
pub mod plan_usage;
pub mod reseed;
pub mod schedule;
mod schedule_mcp;
pub mod secrets;
pub mod skills;
pub mod steer;
pub mod store;
#[cfg(test)]
mod test_support;
pub mod turn;
pub mod turn_state;

pub use engine::Engine;
pub use store::{Chat, ChatSummary, Message, Role, Store};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
