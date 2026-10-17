# Notes for coding agents

## Test windows and screenshots

- Test windows, smoke runs, drives, fixtures and the terminal checks (`--check`, `--record`, …) are the `dev` Cargo
  feature: debug builds and `cargo build --release` have it, the app scripts/package.sh packages doesn't. Put new
  test-only code behind `#[cfg(feature = "dev")]` (smoke/drives.rs is all `dev`), and check the packaged build stays
  free of warnings with `cargo check --no-default-features`.
- Test windows (`FILO_OPEN` fixtures, `FILO_SMOKE` runs, the drives, or any run with a `FILO_CONFIG_DIR` of its own)
  take screenshots only in the app: `FILO_FIXTURE_SHOTS=<folder>` for fixtures, `FILO_DRIVE_SHOTS=<folder>` for
  drives (every drive, the pet's too; they also take `FILO_FIXTURE_SHOTS`), `FILO_SHOT_AFTER=<seconds>:<file.png>` for a page opened with `FILO_OPEN` (without it, nothing is saved).
  They read the window's own frame (`src/window_shot.rs`) and never ask macOS for Screen Recording.
- Never run `screencapture`, ScreenCaptureKit or anything else that captures the screen for a test window: macOS
  shows the user a Screen Recording prompt for each one. A test run refuses those calls, and microphone,
  Accessibility, notification and administrator prompts, itself (`src/test_run.rs`, whose test fails when one is added
  without the check).
- Open at most a few test windows per check, one at a time, and let each quit before the next. Never loop over many
  window sizes: pick one or two widths (one of your own near 1000, e.g. `FILO_WINDOW=1004x760`, so another test run's
  fixture input doesn't land in your window).

## Docs

- README.md is for people who find Filo on GitHub: what it is, what it needs, how to build and run it. Keep it short
  and free of internal notes. How each part works in detail (tables, functions, files, test switches) goes in
  docs/how-it-works.md, under the part's own heading.
- THIRD_PARTY_NOTICES.md's list of crates is generated: run `python3 scripts/third-party-notices.py` after Cargo.lock
  changes (`--check` says whether it's out of date). Third-party code, fonts or pictures added by hand get their own
  entry above the generated part.
