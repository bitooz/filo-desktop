# gpui 0.2.2, patched for Filo

A copy of [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) 0.2.2 from crates.io (Apache-2.0,
LICENSE-APACHE), used through `[patch.crates-io]` in the app's Cargo.toml. Drop this copy when we move to a gpui release
with the fixes below.

Changes, all marked `Filo:` in the source:

- `src/elements/text.rs` (`TextLayout::layout`): truncated text (`text_ellipsis()`, `truncate()`) truncated its runs in
  place inside the measure closure, so every later measure of the same element started from the previous one's shorter
  runs. Measured narrow and then one character wider, the runs ended inside the 3-byte `…` and `MacTextSystem::layout_line`
  panicked ("byte index 136 is not a char boundary; it is inside '…'"). That panic happened while drawing, sometimes
  inside an AppKit callback (`window_did_change_key_status`), and the app aborted: two crashes on 2026-10-14 from the
  Agents list's Group row preview. Each measure now truncates a fresh copy of the runs.
- `src/platform/mac/window.rs` (`draw`, `shows_subview`), `src/platform/mac/metal_renderer.rs`
  (`set_holds_native_views`): native views over the window's view (glance pages, the note editor, artifact pages,
  videos) moved a frame apart from what GPUI drew around them, so scrolling the Feed or a chat made a glance's content
  wobble in its card. Those views are placed (`setFrame:`) in the frame's prepaint and AppKit shows the move with the
  main thread's Core Animation transaction, but the frame itself went to the screen whenever the GPU was done
  (`present_drawable` on the command buffer). While a subview of the window's view isn't hidden, a frame now presents
  with that transaction (`presentsWithTransaction`, `waitUntilScheduled`, then `present`), as upstream already does
  for a resize.
- `src/platform/mac/window.rs` (`handle_view_event`, `draw`, `take_keyboard`, `keyboard_left_nowhere`): typing in
  Filo's message boxes and its fn+⌃ voice shortcut stopped working until the app restarted. Keys and modifier changes
  go only to the window's first responder, and a web view over the window's view takes that when it's clicked (a
  glance, an artifact page, the note editor). When such a view is then hidden or taken out (a glance scrolled away, a
  page closed), AppKit leaves the window itself as the first responder, not this view. And this view doesn't take it
  back on a click (no `acceptsFirstResponder`), so nothing did. Now a mouse down this view hears makes it the first
  responder, and a frame drawn while the window itself (or its content view) is the first responder hands it back on
  the next pass of the run loop. `FILO_OPEN=page-keys-drive` checks both in the app.
- `src/window.rs` (`scene_index`, `painted_over`, `PaintedOver`): what GPUI painted over a native view. A native view
  over the window's view (a web view, a glance's page, a video) is over everything GPUI draws, so a menu, tooltip,
  notification or dialog GPUI draws over one was hidden behind it. A native view's element takes `scene_index` as it
  paints, and once the frame is drawn `painted_over` gives what the frame painted after that inside its box: paint order
  is GPUI's stacking order, so that is exactly what should show over it. Each shape's box comes clipped and merged with
  the ones it overlaps (the app cuts them out with the even-odd rule), with its largest corner radius. Left out:
  shadows, what draws nothing, and of a box with only a border (painted after what's in it) all but its ring. A
  see-through fill over the whole view (a dialog's dim) is a tint to lay over it, and a see-through gradient (a fade at
  an edge, like the one under a page's header) a fade to lay over it (`PaintedFade`: its line in the shown part's unit
  space, the shader's, and its stops).
- `Cargo.toml`: the default features are only `font-kit`, the one the macOS code reads. Upstream's `wayland` and `x11`
  turn on blade-graphics, blade-macros, blade-util and bytemuck, which have no target filter, so a Mac build compiled
  the Linux renderer (blade, a second naga, hidden-trait, syn 1, mint, codespan-reporting) without linking it. And
  `image` without its `avif` feature, an AVIF encoder (ravif, rav1e and about 20 crates): GPUI only decodes, and it
  can't decode AVIF without `avif-native` anyway. The other default formats stay.

Left out of the copy, as nothing here compiles them on a Mac: `examples/` (and their `[[example]]` blocks), `tests/` (its
`[[test]]` block), `docs/`, `resources/windows/`, `Cargo.lock`, `Cargo.toml.orig`, and the Linux, Windows and blade
platform code (`src/platform/{linux,windows,blade}` and `scap_screen_capture.rs`, whose `mod` lines are behind `cfg`s
that are false on macOS; the `macos-blade` feature needs them back). `README.md` stays: `src/gpui.rs` includes it as the
crate's doc.
