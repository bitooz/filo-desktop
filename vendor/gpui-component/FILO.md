# gpui-component 0.5.1, patched for Filo

A copy of [gpui-component](https://github.com/longbridge/gpui-component) 0.5.1 (Apache-2.0, LICENSE-APACHE), used
through `[patch.crates-io]` in the app's Cargo.toml. Newer releases fix markdown properly but need `gpui-pre`
(weekly snapshots of Zed's main branch) instead of the released gpui 0.2.2; drop this copy when we move to a gpui
release they support.

Changes, all marked `Filo:` in the source:

- `src/text/node.rs`, `src/text/inline.rs`: `inline code` is no longer a run background (a square box the full line
  tall, in the body font). Paragraphs pass code ranges to `Inline`, which sets the theme's mono font family on those
  runs and paints a rounded chip behind each piece, per wrapped line: `secondary_active` (the app's surfaceStrong)
  outlined with `border`.
- `src/text/wrap.rs` (new), `src/text/inline.rs`: code and links at the ends of lines. gpui 0.2 wraps a paragraph itself
  and may break before any character that isn't a letter, a digit, a space or one of `-_.'$%@#^~,=:`, so the `)` after a
  commit hash that just fit went to the next line on its own. And where a line wrapped right at a link's end, gpui drew
  a stub of its underline at the next line's start, reaching half a glyph box left of the text, past the bubble's
  padding. Closing marks (`) ] } " ? ! ;`) right after code or a link (`wrap::kept_with`) are held in the layout by a
  letter about as wide (`stand_in`, measured once per font), drawn transparent with the mark painted over it, so the
  code and its marks move to the next line together; the text's byte offsets don't change. Links' underlines are drawn
  by `Inline`, a piece under each line (`wrap::pieces`, which the code chips use too), leaving out the spaces a wrapped
  line ends with. Plain words and marks that aren't ASCII (`”`) still wrap as gpui wraps them, and code whose last
  stretch with no break in it is about a line long still breaks where it overflows, which can be at its mark. Tests are
  in the app (`screens/chat/conversation.rs`); check with `FILO_OPEN=markdown-compare FILO_COMPARE=now` (or `group`)
  `FILO_COMPARE_SAMPLE=wrap` at several `FILO_WINDOW` widths.
- `src/text/node.rs`: code blocks use `secondary_active` with a `border` outline instead of `muted`, which the app
  maps to the bubble's own colour; list items get a small gap.

- `src/text/safety.rs` (new), `src/text/inline.rs`, `src/text/node.rs`: every link a text view opens goes through
  `safety::open_link`, which opens only http(s) and mailto addresses (not `file://`, custom schemes or relative paths).
  `TextViewStyle::untrusted(true)` (`src/text/style.rs`) cleans the parsed tree before it's drawn
  (`src/text/format/markdown.rs` calls `safety::sanitize_untrusted`): raw HTML becomes its text, pictures become links
  (nothing is fetched), and other addresses keep only their words. The app's coding results use it; the tests
  that walk the drawn tree are in the app (`screens/feed_common.rs`). `TextViewStyle::remote_pictures(false)` does
  only the pictures part (`safety::remote_pictures_as_links`): pictures at web addresses become links and raw HTML
  with an `<img>` becomes its text, while pictures on this Mac, links of any kind and other HTML stay. The app's chat,
  coding threads and question cards use it (`ui::markdown_style`), so a model can't make the app fetch an address.

- `src/text/node.rs` (`render_table`, `table_columns`): tables. 0.5.1 gave every cell `relative(characters)` of the
  width (a multiple of the table's own) and truncated it to one line, so long cells were cut off. Columns are now sized
  like a browser's auto table from the cells' measured text: at least the longest word (capped at 10em, so URLs and paths
  wrap), short columns on one line while there's room, the rest of the width to long columns. Cells wrap, rows fit their
  tallest cell, and a table whose narrowest columns don't fit scrolls sideways (overlay scrollbar, vertical swipes still
  scroll the page; a sideways swipe stops at the table, or a page that only scrolls vertically would scroll down on it
  too). Cells are blocks: as a flex item the text was measured on one line and then wrapped narrower, so rows
  overlapped. A centred or right column moves the text's box (no wider than its one line, auto margin) instead of using
  `text_align`: gpui 0.2's `TextLayout::position_for_index`/`index_for_position` ignore alignment, so selection, clicks
  and link underlines landed where left-aligned text would be. Check with `FILO_OPEN=markdown-compare FILO_COMPARE=page
  FILO_COMPARE_SAMPLE=tables` (or `table-edges`), and with `FILO_FIXTURE_SHOTS=<dir> FILO_COMPARE_DRIVE=scroll@X,Y` or
  `select@X1,Y1:X2,Y2` for scrolling and selection.

- `src/input/atom.rs` (new), `src/input/state.rs`, `src/input/movement.rs`, `src/input/element.rs`: atoms, pieces of an
  input's text drawn as a chip and edited as one unit (an attached file in the chat composer: its picture, name and
  size). The text stays as it is; the element paints a rounded, outlined chip behind each piece of it (one per line it
  covers), an image in the room its first bytes leave (spaces), and its text from `muted_from` in the muted colour.
  Every edit (typing, deleting, IME, paste) that cuts into an atom takes all of it, atoms after an edit move with it,
  and the cursor and selection never stop inside one (`previous_boundary`, `next_boundary`, `move_to`, `select_to`).
  A line never wraps inside one (`src/input/text_wrapper.rs`, `unbreakable`: a line with a chip is wrapped a piece at a
  time, each break moved to before the chip). `set_value` drops them; `set_atoms` puts them back. API: `insert_atom`, `atoms`, `atom_mut`, `set_atom_image`,
  `remove_atom`, `set_atoms`. The rules' tests are in the app (`screens/chat/attachments.rs`), since this copy's own
  tests need a workspace.
- `src/input/state.rs`, `src/input/element.rs`: `InputState::set_text_colors` takes a function that finds pieces of the
  text to draw in a colour of their own (the chat composer's @mentions); it runs on the text at every layout, so the
  pieces follow edits. Pieces inside a chip are left alone.
- `src/input/state.rs`: `InputState::on_first_row` says whether the cursor (or the selection's start) is on the first
  line drawn, wrapped lines counted, so ↑ there can go up to the chat's questions panel instead of moving the cursor.

- `src/text/commits.rs` (new), `src/text/format/markdown.rs`, `src/text/style.rs`, `src/text/safety.rs`: with
  `TextViewStyle::commit_links(true)` (the chat's Filo messages), a commit hash (7 to 40 lowercase hex digits with a
  digit and a letter, a whole word, or inline code that is only the hash) is a link to `commit:<hash>`; hashes inside
  links, code blocks, paths and versions are left alone. Every link a text view opens goes first to the app's handler
  (`safety::set_link_handler`), which takes `commit:` links (the app's `commit_links.rs` opens them on GitHub).
  `safety::markdown_targets` lists the links a style draws, for tests.
- `src/text/style.rs`, `src/text/node.rs`: with `TextViewStyle::mention_links(color)` (the group chat's messages), a
  link to a `mention:` address (an @name, which the app's link handler opens) is drawn in that colour, semibold and not
  underlined; bold around it stays bold.
- `src/text/format/markdown.rs`: a hard line break (two spaces or a backslash at a line's end) starts a new line;
  0.5.1 dropped it, so "From: a@b.com" then "To:" read "a@b.comTo:". Links inside bold, italic or struck-through text
  open (0.5.1 drew their words but dropped the link).
- `src/text/inline.rs`: a link opens when the left button goes down and comes up on it a few points apart, whether
  or not the view has a selection (0.5.1 only listened while nothing was selected, so a click that moved a pixel, or
  any click after selecting, did nothing). The pointer repaints the view only when it moves onto or off a link, not on
  every character. A double-click selects the word under it (`src/text/select.rs`, `word_at`), a triple-click the
  paragraph.
- `src/text/select.rs` (new), `src/text/text_view.rs`: a selection runs along the lines from one end to the other in
  reading order (`char_selected`), not over the rectangle between the ends. Only the left button selects (a right-click
  keeps it), shift extends it, a press and release a few points apart is a click, and presses count only where the
  view shows. `TextView::selection_actions` draws an element under a finished selection (the chat's Quote button),
  which reads it with `SelectedText::text`. A hitbox under the actions (`actions_hitbox`) hides what's beneath them from
  the mouse, so clicking Quote doesn't also click a row, a link or the text under it, whether it's drawn under the
  selection or snapped up over it near the window's bottom, and doesn't count as a press outside that clears it. A drag
  ends where the left button comes up, heard in the capture phase whether or not a frame has drawn since the press, or
  on the first move with the button up, so a release the view never heard doesn't leave the selection following the
  pointer. Check with the compare screen's `quick@`, `held@` and `hover@` drive steps. Tests
  for `commits`, `word_at` and `char_selected` are in the app (`commit_links.rs`, `screens/chat/conversation.rs`). Check with `FILO_OPEN=markdown-compare FILO_COMPARE=now
  FILO_COMPARE_SAMPLE=links` and `FILO_COMPARE_DRIVE='click@X,Y;dblclick@X,Y'`.
- `src/text/inline.rs`, `src/text/text_view.rs` (`open_link_menu`, `render_link_menu`): a right-click on a link the
  view would open (http(s) or mailto, `safety::is_safe_link`) opens a menu at the pointer with only "Copy link", which
  copies the whole address, not the link's words, and shows the "Copied" toast the message Copy button shows. A right-click anywhere else, or on an @name or a commit hash, does
  what it did. Check with `FILO_COMPARE_DRIVE='rclick@X,Y;click@X,Y'` (a click prints what's on the clipboard).

- `src/input/state.rs`, `src/input/input.rs`, `src/input/element.rs`: for the chat composer's growing text
  (`screens/chat/composer_layout.rs`). `InputState::scrollbar_show` sets how a multi-line input's scrollbar shows
  whatever the theme says (the composer's: thin, only while it scrolls); `set_max_rows` changes an auto-grow input's
  most rows as the window is resized; `shown_rows` and `scroll_offset` read how many lines it shows and how far it's
  scrolled. An auto-grow input scrolls straight to the cursor instead of a line per frame, and never shows room past
  its last line. A cursor in a line out of sight is found in that line (0.5.1 found it only from the next line on, so
  a cursor in the last line, after a long paste, was never scrolled to). `InputState::wrap_margin` sets the room kept at
  the right of each line, past which it wraps (0.5.1's fixed `RIGHT_MARGIN`, 10 points, unless set; the composer keeps 2,
  the caret's room, so its lines run to its edge). Check with `FILO_OPEN=chat-composer`.

Headings and paragraph spacing are set by the app (`ui::markdown_style`), not here.

- `src/one_menu.rs` (new), `src/menu/context_menu.rs`, `src/menu/dropdown_menu.rs`, `src/menu/popup_menu.rs`,
  `src/popover.rs`, `src/input/popovers/context_menu.rs`, `src/text/text_view.rs`, `src/text/inline.rs`, `src/root.rs`:
  one menu or popover open at a time, across the app. Every menu says when it opens (`one_menu::opened`, with how to
  close it) and closes (`closed`): a right-click menu, a popover or dropdown, a text field's right-click menu, a link's
  "Copy link" menu, and the app's own popups (its `escape.rs`). Opening one closes the one open before, in any window
  (the close runs later, never inside what opened the new one). The open one also closes on the scroll wheel anywhere
  but over a menu (each marks its box with `one_menu::surface()`; `Root` listens), when its window goes to the back
  (`Root`), and on Esc nothing else used (`close_in`, from the app's `escape.rs`); a click elsewhere closes each kind as
  before. 0.5.1 let menus stack: one right-click where an element with a menu was drawn over another opened both (each
  listened for any right-click in its bounds), and rows side by side whose `ContextMenu`s share the default id each drew
  the open menu. Now a right-click opens only the menu of what's on top (`ContextMenu` listens before its element paints,
  only where its element shows, and takes the click), a shared menu draws once a frame (`one_menu::frame`), and a
  right-click on an open menu's backdrop closes it and opens the menu under it (`pass_through`). A dropdown closed by
  another menu makes its items again next time. Tests for the rule are in the app (`escape.rs`); check it with real
  clicks with `FILO_OPEN=menus` (`screens/menus_fixture.rs`).
- `src/webview.rs` (`set_hole_cutter`, `WebViewElement::paint`), `src/root.rs`, `src/one_menu.rs` (`on_modal`):
  whatever GPUI draws over a web view shows whole and takes its clicks. A web view is a native view over everything GPUI
  draws, so a menu, tooltip, notification card, toast or dialog falling over one was cut off at its edge (a tooltip
  under an artifact page's header buttons showed only its top border; an in-app notification card only its title
  strip; a "Share this page?" dialog was under the page, which took its clicks). Each overlay used to mark its box for
  the web view to cut out (menus with `one_menu::surface`, tooltips with a `tip_surface`), and an overlay without the
  mark ended up behind the page. Now nothing is marked: as it paints, the web view notes how far the frame is
  (gpui's `Window::scene_index`), and once the frame is drawn it hands the app what GPUI painted after it, over it
  (`Window::painted_over`, vendor/gpui/FILO.md), which the app cuts out of the view (its `web_holes.rs`); a dialog's dim
  over the page lies over it as a tint. Paint order is GPUI's stacking order, so whatever is drawn over a page shows,
  this kind or any later one; what's drawn before it (a dialog holding a web view) is under it. `Root`'s `settle`,
  which drew once more so the holes caught up with the marks, went with them. A dialog or sheet opening takes the
  keyboard from a web view that has it (`on_modal`, set by the app), so Esc and Return reach the dialog. Check it with
  `FILO_OPEN=overlays-drive:<artifact id>,<video file>` (each overlay over an artifact page, the note editor, a glance
  opened larger and a video).
- `src/one_menu.rs` (`surface`), `src/scroll/scrollbar.rs`: the pointer over an open menu is the menu's alone. Nothing
  drawn under it hovers, shows a tooltip, changes the cursor, scrolls or takes a click there. `one_menu::surface()`, which
  goes first in every menu's box, puts a `BlockMouse` hitbox (GPUI's `occlude`) over the box's own and under the menu's
  items, so every menu blocks what's under it whether or not its box occludes itself. 0.5.1's `Popover` and the
  right-click menu's backdrop did, but a `PopupMenu` drawn without them (a text field's right-click menu) and the
  app's own popups (the chat's reaction picker, the artifact page's menu) didn't. The box's own hitbox is under it
  too, so a press on a menu's padding no longer focuses the box. A scrollbar went by its bounds alone, so one under a
  menu lit up under the pointer and a click on a menu item over its track jumped what it scrolls. Its bar, thumb and
  track now also ask the bar's hitbox (`bar_hitbox.is_hovered`). `FILO_OPEN=menus` checks a row under the View
  dropdown, the app's popup and a right-click menu.
- `src/notification.rs`: a notification with `on_click` (the app's card for a reply, `in_app_notice.rs`) is one click
  target that looks it: the pointer cursor and the `secondary_hover` background over the whole card. 0.5.1's close
  button let its click go on to the card's, so closing the card also opened its chat; it now stops there.
- `src/dialog.rs`: a dialog with only a title and buttons (Turn off the group?) leaves its content area out. 0.5.1
  laid out the empty area anyway, between two of the dialog's gaps, so a gap stood under the title.

Left out of the copy: `Cargo.lock`, `Cargo.toml.orig` and `README.md` (with the manifest's `readme` line), which no build
reads. `tests/fixtures/layout.json` stays, as the dock's state test includes it. `one_menu::open_key`, which nothing
called, is gone.
