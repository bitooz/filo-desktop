# Filo

Your own team of AI agents, on your Mac.

![A conversation in Filo](https://filo.im/img/blog/chat-glance-2.webp)

Filo is a native macOS app where you make agents, each with a name, a picture and instructions of its own, and talk with
them one at a time or all together in a group chat. The answers come from a coding tool you already use, [Claude
Code](https://claude.com/claude-code), [Codex](https://github.com/openai/codex), Muse Code,
[OpenCode](https://opencode.ai) or [pi](https://pi.dev), running on your Mac under your own account. Filo has no account of its own, and your
conversations, notes and agents stay in a folder on your Mac.

What's in it:

- **Agents and a group chat.** Each agent has its own conversation, memory and skills. In the group, the agents read
  each other's messages and answer when you @mention them.

  ![An agent's profile in Filo](https://filo.im/img/blog/agent-profile-3.webp)

- **Coding.** Hand a task to Claude Code, Codex, Muse Code, OpenCode or pi, from the Coding page or from any conversation.
  It runs in your project or in a git worktree of its own, and you can follow each step as it happens.
- **Artifacts.** Reports, pages, diagrams and small tools your agents make, with every version kept.
- **A browser.** Pages on macOS's WebKit that keep your logins, which agents read, click, type, upload and download
  in the background while you use other apps, and you can open and use too (Window › Browser). You see an agent work
  there and can pause it or take the page over. It can bring in your bookmarks, history and exported passwords from
  Helium or Chrome.
- **Scheduled work.** Reminders, routines that run on a schedule, and check-ins, even while Filo is closed.
- **Voice messages, a Feed and Notes,** plus apps (MCP servers) your agents can use, a menu bar item and a desktop pet.

![Pictures in a Filo conversation](https://filo.im/img/blog/chat-pictures-2.webp)

Built with [GPUI](https://www.gpui.rs/), the UI framework behind the [Zed](https://zed.dev) editor, and
[gpui-component](https://github.com/longbridge/gpui-component).

## Requirements

- **macOS 13 (Ventura) or later**, on Apple silicon or Intel. Computer use (an agent working in your other apps) needs
  macOS 14.
- **Rust 1.90 or later.** Install it with [rustup](https://rustup.rs).
- **Xcode or the Xcode Command Line Tools** (`xcode-select --install`), with Swift 6.1 or later: `scripts/app.sh`
  builds the computer use helper (`computer-use/`) with `swift build`. You don't need Xcode's Metal toolchain, as debug
  builds compile GPUI's shaders when the app starts.
- **At least one coding tool**, installed and signed in:
  [Claude Code](https://docs.claude.com/en/docs/claude-code/setup) (`claude`),
  [Codex](https://github.com/openai/codex) (`codex`), Muse Code (`muse`), [OpenCode](https://opencode.ai)
  (`opencode`) or [pi](https://pi.dev) (`pi`). Filo finds them on your login shell's `PATH`, and in `~/.local/bin`, `~/.claude/local`,
  `~/.opencode/bin`, `~/.npm-global/bin`, `~/.bun/bin`, `/opt/homebrew/bin` and `/usr/local/bin`.

A first build downloads and compiles about 600 crates. Leave it about 20 GB of free disk space for `target/`.

## Building and running

```bash
git clone https://github.com/bitooz/filo-desktop.git
cd filo-desktop
scripts/app.sh             # builds target/debug/Filo.app, signs it and opens it
scripts/app.sh --release   # the same, optimised (use this one to judge speed)
```

Run Filo through `scripts/app.sh` instead of plain `cargo run`. macOS asks for the microphone and shows notifications
only for an app bundle: run straight from a terminal, the binary counts as part of the terminal app. The script signs
the bundle with your Mac's "Developer ID Application" or "Apple Development" certificate if it has one
(`SIGN_IDENTITY=<name>` picks another). Without one it signs ad hoc, which runs fine but gets no notifications.
`FILO_LOG=<file>` collects the app's output.

`cargo run` still works for a quick look. So do the tests:

```bash
cargo test -p filo-local   # chat storage, the database, the MCP server and the coding tools' runner (no GPUI)
cargo test --bin filo      # the app's own tests
```

To build a release bundle and disk image as they're shipped (without the developer modes, the `dev` feature), see the
options at the top of `scripts/package.sh`:

```bash
BUILD=1 scripts/package.sh   # dist/Filo.app, a zip and a .dmg
```

Pull requests are checked with `cargo fmt`, clippy and the tests; signed releases are built by GitHub Actions
([docs/how-it-works.md](docs/how-it-works.md#checks-and-releases)).

## First launch

A short welcome tour checks which coding tools are on your Mac and signed in, helps you set one up if none is, and
makes your first agent. Nothing is asked for at launch: macOS asks for the microphone the first time you record a voice
message, and for notifications the first time Filo has one to show. Settings › Welcome tour shows the tour again.

## Your data

Everything Filo keeps is under `~/.config/filo/local`: a SQLite database (`filo.db`) with your agents, notes,
reminders, artifacts and the rest, the conversations' files, and the last three daily backups of the database. Keys
for apps you connect go in your login Keychain. Set `FILO_CONFIG_DIR=<folder>` to run Filo on another folder, for
example a throwaway one while you try a change.

A few features use Filo's own servers, and only when you use them:

- **Voice messages** stream the microphone's audio to Filo's relay to be turned into text
  (`src/live_transcription.rs`).
- **Sharing** an artifact or a coding result puts it at a link on filo.im, until you unshare it.
- **Report a problem** sends what it found (no chat text, no paths in your home folder, no keys) only when you click
  Send ([docs/problem-reports.md](docs/problem-reports.md)).

The release builds also send anonymous usage counts ([docs/telemetry.md](docs/telemetry.md)) and check for updates. A
build from source does neither unless it's given a PostHog key and an update feed at build time.

## How it's organised

- `src/`: the app. `main.rs` starts it, `app.rs` and `nav.rs` hold the sidebar and the pages, `screens/` has one
  module per page, and `local_backend.rs` answers every page's calls from the data on your Mac.
- `crates/filo-local`: everything with no UI. Chat storage and the database, Filo's MCP server (the tools agents get),
  and the runner that drives Claude Code, Codex, Muse Code, OpenCode and pi. It builds and tests on its own.
- `crates/filo-resample`: draws a zoomed-in picture sharp, at the screen's pixels.
- `computer-use/`: the Swift helper behind computer use.
- `vendor/gpui`, `vendor/gpui-component`: our copies of gpui 0.2.2 and gpui-component 0.5.1 with Filo's fixes. Each
  `FILO.md` lists what changed and why.
- `assets/`: icons, fonts, pictures and the colour themes. `web/`: the note editor and other pages shown in web views.
- [docs/how-it-works.md](docs/how-it-works.md) explains each part in detail: the database, the group chat, coding,
  artifacts, apps, scheduled work and the command-line checks.

## Licence

Filo's own code is released under the [MIT licence](LICENSE). The third-party code, fonts and pictures it
includes keep their own licences, listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
