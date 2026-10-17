# Anonymous usage counts

Filo for Mac counts how many Macs install it and how many open it each day, by country. These counts are
anonymous and on by default, as in T3 Code. To turn them off, set `FILO_TELEMETRY=0` (see
[Turning it off](#turning-it-off)). The code is in `src/telemetry.rs`.

## What's sent

Filo sends two events to [PostHog](https://posthog.com):

| Event           | When                                                                                        |
| --------------- | ------------------------------------------------------------------------------------------- |
| `app_installed` | Once per install, the first time Filo opens with the counts on.                             |
| `app_opened`    | Once on each day Filo is open. A Filo left open across midnight sends one for the new day.  |

Each event carries only these fields:

| Field                    | Example                                  | What it is                                                              |
| ------------------------ | ---------------------------------------- | ----------------------------------------------------------------------- |
| `distinct_id`            | `3f0c…-…` (a UUID)                       | A random install ID, made on this Mac. It isn't tied to you or any account. |
| `platform`               | `desktop`                                | Always `desktop`. The PostHog project is shared with the phone and web apps, so this tells their events apart. |
| `channel`                | `stable` or `dev`                        | Which build this is: `stable` for one packaged by `scripts/package.sh`, or `dev` for a build made without it. |
| `app_version`            | `0.1.0`                                  | Filo's version.                                                         |
| `os`                     | `macOS`                                  | The operating system.                                                   |
| `arch`                   | `aarch64` or `x86_64`                    | The processor architecture.                                             |
| `$process_person_profile`| `false`                                  | Tells PostHog not to make a person profile for the ID.                  |

Each event also has its own random `uuid`, so PostHog can tell a retried send from a new one, and a `timestamp`.

Filo doesn't send your prompts, chats, notes, files, file paths, names, email or IP address. It also doesn't send
your Claude Code or Codex account IDs.

### Country

Country comes from PostHog's own GeoIP lookup on the IP address the request comes from. Filo doesn't put the IP
in the event and doesn't keep it. To stop PostHog storing it too, turn on **Discard client IP data** in the
PostHog project's settings: PostHog still looks up the country before it drops the IP.

### Kept on this Mac

`~/.config/filo/telemetry.json` (or `$FILO_CONFIG_DIR/telemetry.json` when that's set) holds the install ID, whether `app_installed` was sent, and the last day
`app_opened` was sent. The install ID is made the first time something is sent, so with the counts off from the
start, no ID is ever made. To forget an ID made earlier, delete the file after turning the counts off. If a send fails (for example, offline), nothing is
marked sent, and Filo tries again an hour later or the next time it opens.

## Turning it off

Set `FILO_TELEMETRY=0` in your shell profile, then quit and reopen Filo. For zsh, the macOS default:

```sh
echo 'export FILO_TELEMETRY=0' >> ~/.zshrc
```

Then quit Filo with ⌘Q (closing the window isn't enough while Filo is in the menu bar or the pet is out) and open
it again. `false`, `off` and `no` work as well as `0`.

Filo reads the variable when it opens. If it isn't set in Filo's own environment (a Filo opened from Finder or the
Dock has a bare one), Filo asks your login shell, the same way it finds `claude` and `codex` on your PATH: it runs
`$SHELL -ilc` once, which reads `~/.zprofile` and `~/.zshrc` for zsh, or `~/.bash_profile` for bash. A Filo
started from a terminal where the variable is set, or after `launchctl setenv FILO_TELEMETRY 0`, gets it directly.

Filo also sends nothing in these cases:

- Development (debug) builds, unless they're started with `FILO_TELEMETRY=1`.
- Smoke and fixture runs (`FILO_SMOKE`, `FILO_OPEN`, `FILO_FIXTURE_SHOTS`, `FILO_FIXTURE_DRIVE`).
- Builds made without a PostHog key.

In a build with a key, Filo logs why it sends nothing (`telemetry: off, …`) to its standard error.

## For builders

The PostHog key and host are read when building, not at run time:

```sh
FILO_POSTHOG_KEY=phc_… FILO_POSTHOG_HOST=https://eu.i.posthog.com BUILD=42 scripts/package.sh
```

`FILO_POSTHOG_HOST` defaults to `https://us.i.posthog.com`. Without `FILO_POSTHOG_KEY`, the build sends nothing
and doesn't run the login shell for it. Events go to `<host>/batch/`.

`channel` is `FILO_CHANNEL`, which `scripts/package.sh` sets to `stable` when building (the update checker reads it
too). A build without it, such as `cargo build` or `scripts/app.sh`, reports `dev`.

To count desktop users by country in PostHog, filter on `platform = desktop` (and `channel` if needed), then chart
unique users of `app_installed` or `app_opened`, broken down by `$geoip_country_code`.
