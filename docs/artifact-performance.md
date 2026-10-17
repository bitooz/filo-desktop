# Artifact preview and loading checks

The Preview dropdown renders in the trusted WKWebView host, outside the artifact's opaque-origin iframe, once the preview is ready. Opening and dismissing it does not hide, resize or reload the preview. While the preview is still loading, or if it failed or never reports ready, the app draws its own dropdown (as in Source) and hides the web view under it, so the dropdown always opens. Host menu messages and iframe messages have separate envelopes, so an artifact cannot impersonate the host menu.

Each preview page numbers its messages (`load`). That lets one web view load another page (another version, or another artifact) without stale messages from the last page counting. Pointing at cards in the list moves the single preloaded page from card to card: its web view loads each one in turn instead of WebKit building a new one per hover. Clicking a card that wasn't preloaded takes over the preloaded page's web view the same way.

The shell keeps the three most recently opened artifact pages while Artifacts is the open tab. Each keeps its current preview and one other ready preview, preserving scroll and JavaScript state when returning to a version. Switching to another tab drops them and the preload. When Filo's window goes out of view (hidden, minimized or fully covered), only the artifact page on screen keeps its web view; the others reload when shown again. Opening a cached artifact again (from the list or a link) starts on the current version in Preview with the menu closed. Only the page's scroll and state carry over.

Readiness is the iframe's load followed by two animation frames while the view is shown. WebKit can hold animation frames back (for example in a window behind others), so a 250 ms timer reports readiness if the frames don't come. The timing traces (`FILO_SMOKE=artifacts`) exist only in debug builds.

A page is drawn only once it's ready. The web view is transparent (no WebKit white behind the page), and each time it starts loading a page it draws nothing (alpha 0) until the page reports ready, so the app's background shows until the artifact appears. That applies to first opens, preloaded and reused web views, older versions and theme changes. Without it, WebKit's blank white page (or the page the web view showed before) appeared for several frames before the artifact. The web view stays shown to WebKit at alpha 0, so its animation frames keep running and readiness takes as long as before.

## Measurements on this Mac, October 11, 2026

The isolated copies used were three artifacts of a few hundred KB each: a long report with three versions (opened and timed), a page mock and a diagram (hovered and opened). Runs used a focused 1100 × 760 window and the debug binary. “Before” is the code at the start of this round. “After” is this round. Times are from the click to the iframe load plus two animation frames, unless noted.

| Measurement | Before | After |
| --- | ---: | ---: |
| First open, click right after Filo starts (WebKit not running yet) | 615–4,011 ms | 954–4,003 ms |
| First open, click 300 ms after the list appears | 627 ms | 1,118 ms (one run) |
| First open, after 2 s on the list (first card preloaded) | 21–26 ms | 36 ms |
| First open of a hovered card | not timed (each hover built a web view) | 27–38 ms |
| First open of a card not hovered, WebKit already running | not timed | 304–310 ms |
| Main-thread time per hover after the first | 160–250 ms (new web view each time) | 0.9–1.0 ms (load into the same web view) |
| Repeat open (route) | 0.14–0.49 ms | 0.23–0.42 ms |
| Repeat open including two GPUI display frames | 23–31 ms | 23–33 ms |
| First look at an older version | 188–265 ms | 176–433 ms |
| Return to a cached version | 12–18 ms | 3–21 ms |

The paths this round didn't change (direct cold click, 300 ms) vary widely from run to run with the state of the Mac. The first run after a pause was always the slowest. Treat their before/after differences as noise. The Mac's disk filled up during the measurements, which cut the second set of runs short, so most rows have one or two samples per side.

A separate experiment built a hidden blank web view 5 s before the first click and dropped it again. That brought a direct cold click from 823–4,269 ms down to 377–1,875 ms (kept alive: 404–755 ms). It is not in the app: it costs 200–450 ms of main-thread time while Filo starts, plus WebKit processes running in the background.

## What still limits opening

- **Starting WebKit.** The first web view in a freshly started Filo waits for WebKit's processes: up to 3.5 s of a 4 s open passed before the host page started. Opening from the list hides this when the user spends a second or two on the list: the list preloads its first card as soon as it appears. In the real app, chat glances and notes also start WebKit earlier.
- **Building a web view on the main thread.** 100–490 ms whenever no web view can be reused: the list's first preload, the first older version, and a card clicked after the preload was used. The UI is frozen for that time.
- **The page itself.** Parsing, fonts and the artifact's scripts until the iframe loads: about 35–390 ms per artifact once WebKit is running.
- **Display.** Two animation frames before “ready” and two GPUI frames to show it: about 25–35 ms, the floor for every open, including cached ones.

Cache eviction, theme changes or changed content require loading again. Measurements come from a debug build. A release build mostly changes the Rust-side work (themed page, builder), which is under 2 ms here.

## Rechecking

Build, then run the repeatable check on a visible Mac desktop. Choose a multi-version document long enough to scroll 420 px as the first ID, plus two other artifacts for the hover checks:

```sh
cargo build --bin filo
python3 scripts/artifact-check.py \
  --artifact-id e930b2bc-abe9-473d-8fdc-8ca4ad0747a0 \
  --artifact-id 7ab8238f-ee1a-41ee-9a37-01e8a17b6902 \
  --artifact-id 0284daeb-53cb-4aa2-a518-48573c81902c \
  --prepare-ms 0 --output-dir target/artifact-proof
```

The script reads the source SQLite database in read-only mode and copies only the selected artifacts' rows and files into a temporary config under `target`. It adds a scroll/load-token probe to those copies and runs a separate test instance. It starts no work from the user's chats or schedule and removes its temporary config afterwards. `--prepare-ms` is the time on the list before the first click (0 is a direct cold open). `--binary` selects an already-built binary. `--theme dark|light` sets the test instance's theme. `--record` also records the window at 60 frames a second with `scripts/flash-recorder.swift` (only where Screen Recording is already allowed: it never asks, and stops otherwise) and writes `frames.tsv` (near-white and pure-white share of the area below the top bar, per frame) and `whitest.png` to the output folder.

The check verifies the following:

- the same iframe load token and 420 px scroll through menu open/close, Escape, outside dismissal, choosing Source, and version switches;
- the dropdown's version rows (newest first, the shown one checked, the current one marked, with when each was made, no Versions entry): picking one switches the artifact in place, and Show current version brings the current page back with its scroll;
- reopening from the list after leaving on an older version in Source;
- the app's dropdown, with the web view hidden, while a preview is still loading, and the page's own dropdown once it's ready;
- one web view reused across hovered cards;
- the preload going when another tab opens.
- the web view not drawing a page until it's ready, and drawing it once it is.

The check's window pictures are taken in the app (`FILO_DRIVE_SHOTS`, `src/window_shot.rs`), so it never asks macOS for Screen Recording; `--record` runs only where Screen Recording is already allowed. When a picture is missing, the log is the evidence.

On October 11, 2026, recordings of the full check showed WebKit's white page covering 91% of the preview area in 4–9 frames per run before the fix (dark and light themes, direct and preloaded first opens), and no frame over 0.2% near-white after it. That remaining 0.2% is text.
