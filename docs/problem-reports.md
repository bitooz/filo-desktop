# Problem reports

A problem report is sent only when you click **Send**. You can open one from three places:

- **Report** next to a chat reply that failed (beside Retry).
- **Report** next to a coding task's failure, on the task's page.
- **Settings › Help › Report a problem**.

The page runs its checks first. A red check with a known fix has a button for it. You can add a few words under
**What happened?** One line says what Send will send, and **Show all** shows the whole body. Once Filo's backend has
the report, the page says **Sent ✓**. The code is in `src/report.rs` (checks, gathering, redaction, sending) and
`src/screens/report.rs` (the page).

## The checks

| Check | What it looks at | Red when | Fix |
| --- | --- | --- | --- |
| Filo | The build (`0.1.0 (42)` or `local build`) | never | |
| macOS | `sw_vers`: version and build | never | |
| Chip | Apple silicon or Intel; yellow when an Intel Filo runs under Rosetta | never | |
| Claude Code | Where `claude` is (on the login shell's PATH and the usual places) and `claude --version` | it's the engine in question and it isn't installed, or it's older than **2.1.286** | **Update** (`claude update` in Terminal), **Install** |
| Codex | Where `codex` is and `codex --version` | it's the engine in question and it isn't installed | **Install** |
| Muse Code | Where `muse` is and `muse --version` | it's the engine in question and it isn't installed | **Install** |
| OpenCode | Where `opencode` is and `opencode --version` | it's the engine in question and it isn't installed | **Install** |
| Signed in | `claude auth status` or `codex login status`; Muse Code's and OpenCode's own files (they have no status command) | not signed in | **Sign in…** (`claude auth login`, `codex login`, `muse login` or `opencode auth login` in Terminal) |
| Test reply | A one-line turn with no tools and no session kept (`claude -p … --tools ""`, `codex exec`, `muse exec` or `opencode run`), up to 60 s | no answer, or an error | **Sign in…** when the error says it isn't signed in |
| Disk space | Free space in the home folder's volume | under 2 GB (yellow under 10 GB) | **Free up space** (System Settings › Storage) |
| Network | A HEAD request to `dev.filo.im` and to the model's API (`api.anthropic.com`, `api.openai.com`, `api.meta.ai` or `opencode.ai`) | either can't be reached | |

"The engine in question" is the one the failed reply or task used. From Settings, it's the one that answers by
default. The test reply uses a little of that tool's plan, about as much as a one-word message.

## What's sent

`POST https://dev.filo.im/desktop/reports`, JSON, under 512 KB (the oldest log lines go first if it's bigger). In a `dev`
build, set `FILO_REPORT_URL` to send to a test server instead (the packaged app ignores it).

| Field | Type | What |
| --- | --- | --- |
| `build` | integer | The build number, or `0` for a local build |
| `version` | string | `0.1.0` |
| `channel` | string | `stable` (a packaged build) or `local` |
| `macos` | string | `26.5.1 (25F80)` |
| `arch` | string | `aarch64` or `x86_64` |
| `claudeCode` | string or null | Its version and where it is (`2.1.296 · ~/.local/…/claude`), or null when it isn't installed |
| `codex` | string or null | The same for Codex (Muse Code's and OpenCode's are in `extra`) |
| `checks` | array | `{ name, ok, detail }` for each check. `ok` is false only for a red check |
| `failedCheck` | object | The first red check's `{ name, detail }`. Left out when none is red |
| `error` | string | The failed reply's or task's error, as Filo shows it. Left out from Settings |
| `note` | string | What you wrote under What happened?. Left out when empty |
| `logs` | array of strings | `[stderr of the failed reply]` (or `round`) and the CLI's last stderr lines (up to 40), then `[workers.log]` and its last 60 lines |
| `extra` | object | See below |

`extra` holds:

| Field | What |
| --- | --- |
| `schema` | This body's version, now `1`. A change to the fields above means a new number |
| `reportId` | A random UUID made when the page opens. Send again reuses it, so the server can drop a repeat |
| `source` | `chat`, `task` or `settings` |
| `engine` | `claude`, `codex`, `muse` or `opencode`: the engine the checks looked at |
| `museCode`, `opencode` | Their version and where they are, as `claudeCode` above, or null |
| `appLabel` | The build as Settings shows it |
| `checks` | `{ id, status, fix }` for each check: `status` is `ok`, `warn`, `fail` or `skip`, and `fix` names the fix offered (`update-claude`, `sign-in-claude`, `sign-in-codex`, `sign-in-muse`, `sign-in-opencode`, `install-claude`, `install-codex`, `install-muse`, `install-opencode`, `free-disk`) or is null |
| `failure` | `{ kind, engine, model, at, stderrLines }` of the failed reply (`turn`) or task (`round`), or null |
| `recentFailures` | Up to 5 other recent failures: `{ kind, engine, at, error }`, without their stderr |

The server answers `201 { id, url }`, and the page shows **Sent ✓**. What it does with a report after that (where it's
kept, and for how long) is in the server's repository, not this one. When the send fails, the page says why and offers
**Send again** and **Copy report**:

- No connection: "Couldn't reach Filo's server."
- `429`: too many reports from here (5 a minute). The page says how long to wait (`retry-after`).
- `400 { error, field }`: the page shows both.
- `5xx`: "Try again in a few minutes."

## What's never sent

- No chat text. No messages are read. When a CLI wrote something instead of replying, an error includes it ("What it
  wrote instead: …"), and that part is cut.
- No paths in your home folder. `/Users/you` becomes `~`, and only a file's name is kept: `~/Desktop/project/src/x.rs`
  becomes `~/…/x.rs`. A tool's hidden folder stays, because it shows how the tool was installed (`~/.local/…/claude`).
  Other users' folders become `/Users/…`, and the temp folders become `…`.
- No keys or tokens. Anthropic, OpenAI, GitHub, Slack, AWS and Google keys, JWTs, `Bearer …`, `token=…`,
  `password: …` and long random strings all become `[key]`, `[token]` or `[hidden]`. UUIDs stay, because they name
  the reply or task.
- No email addresses. They become `[email]`.
- Your note gets the same treatment.

## Where failures come from

When a chat reply or a coding round fails, Filo writes its error and the CLI's last 40 stderr lines to
`~/.config/filo/local/failures.jsonl`. The file keeps the latest 30 (`crates/filo-local/src/failures.rs`). The chat
itself keeps only the error. A failure from before this was added, or from the agents' group chat, is reported with
the error the chat shows and no stderr.
