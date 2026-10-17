# Shell, Feed, Artifacts, Extensions, Appearance and design tokens

**Older spec.** Written in October 2026 to port these pages from the phone app, and trimmed on 2026-10-13 to what
this app still does. The code is the source of truth: where they differ, the code is right.

The pages call the phone app's function names (`feed:mine`, `canvasArtifacts:get`…). `src/local_backend.rs` and the
files in `src/local_backend/` answer them in the app, from Filo's database and files on this Mac
(`crates/filo-local/src/`). There's no server, account or sign-in.

---

## A. App shell

- `AppShell` in `src/app.rs` is the window's root: the welcome tour (`src/screens/welcome.rs`) over everything the
  first time Filo opens, then a sidebar of tabs, one live view per tab, and a stack of pushed pages over the current
  tab. Filo opens on Agents.
- The tabs and pushed pages, with each tab's label and icon, are `Tab` and `Route` in `src/nav.rs`. `render_sidebar`
  in `src/app.rs` draws the tabs in order, with Settings at the bottom.
- The profile (`users:current`) is this Mac's, kept in filo.db's `profile` table (crates/filo-local `app_data`). Settings' profile
  row opens Your name (`src/screens/name.rs`), which saves it with `users:updateUserName`.

---

## B. Feed

- The tab is `src/screens/feed.rs`. Ask beside the posts is `src/screens/feed_chat.rs`, glance posts are
  `src/screens/feed_glance.rs`, the post page is `src/screens/feed_post.rs` and Feed settings is
  `src/screens/feed_settings.rs`. The reply shapes, labels, the notice and the post actions they share are in
  `src/screens/feed_common.rs`.
- `src/local_backend/feed.rs` answers the `feed:*` functions (its `FEED` list) from Filo's database
  (`crates/filo-local/src/feed.rs`). Agents publish with the MCP tools in `crates/filo-local/src/feed_mcp.rs`.

### Where posts come from

- Agents fill the Feed. Each topic is watched by one of the user's agents on a routine of its own, made when the topic
  is added: the main agent, once a day at 8:00, unless others are picked. Its runs search the web and publish posts.
  Any conversation can publish a post too, and a post says who shared it.
- Refresh on the Feed (`feed:refreshMine`) runs the topics' routines now. It skips a routine that's running and, with
  `ifStale`, one that finished in the last 10 minutes. A selected topic has its own Refresh (`feed:refreshTopicMine`).

### Post shape (`PublicPost`)

- The pages read a post in the shape of the phone server's `PublicPost`: camelCase fields, `_id` for the id, and
  optional fields left out rather than sent as `null`. `Post` in `crates/filo-local/src/feed.rs` is that shape, except:
  - It has no `rank`.
  - `detailsStatus` is always `ready`. There's no page reader: a post has what its author wrote.
  - It adds `updatedAt`, `author`, `noteId`, `reminderId` and `chatId`, and for a glance post (kind `glance`), `glance`
    and `glanceChoice`.
- `feed:postMine` adds `media`, the cover image when there is one (the phone's `PostDetail`). `feed:mine` and
  `feed:postMine` add the sharing agent's `authorAgentId`, `authorPicture` and `authorBackground` when it's one of the
  user's agents.
- The pages' side of the shape is `FeedPost` in `src/screens/feed_common.rs`.

### Rules

The limits are constants at the top of `crates/filo-local/src/feed.rs` and `src/local_backend/feed.rs`.

- There's no pagination. `feed:mine` returns the newest 40 posts, newest first, plus posts hidden in the last 12 hours
  (for the undo row), and the settings.
- A user follows up to 20 topics. A label is clipped to 80 characters, and a topic's key is its label in lowercase.
- A link is in the Feed once: publishing it again is refused.
- Saving a post makes a note tagged `feed` from it. Unsaving deletes that note.
- Remind works only for an event that hasn't started. The reminder is due 30 minutes before the start, and no sooner
  than a minute from now.
- Ask goes to the agent picked in Feed settings, else the main agent. On the post page, `feed:askAboutPost` sends a
  message about the post to that agent's conversation, with the post as context for the model, starts its turn and
  opens it. On the Feed tab, Ask opens that conversation beside the posts and sends nothing: the post goes with the
  user's next message, as context.

### Feed tab (`src/screens/feed.rs`)

- The header has Refresh and Feed settings. Under it, a row of bubbles: For you, Glances (when there are glance posts),
  one per topic (its newest picture, else its first letter), and Add, which opens Feed settings. A bubble filters the
  posts; a topic that's unfollowed stops filtering.
- Posts that arrive while the list is open wait behind an "N new posts" pill.
- A post shows its source, age and a "…" menu ("Why you're seeing this", "Less about {topic}", "Not interested"), then
  its cover or its title on a card, which opens the post page. Then Like, Save to Notes, Copy link and Open in browser
  (the last two only with a link), and Ask with the agent's name. Then the title, "Shared by {author}" with the note,
  More for the summary and points, and "Remind me" on an event.
- Like and save change at once and go back if the call fails. A post hidden while the list is open stays as an undo
  row.
- Short confirmations show at the bottom for 2.6 s (`Notice` in `feed_common.rs`). The end line, the empty states and
  the refresh messages are `feed_end_text`, `render_placeholder` and `refresh_message`.

### Post page (`src/screens/feed_post.rs`)

The post's pictures as a gallery, the title, source and age, who shared it with their note, the summary and points,
the event with "Remind me", then Open in browser, Ask, Like, Save and Copy link. A post that's gone shows "This post is
gone".

### Feed settings (`src/screens/feed_settings.rs`)

"Your feed": who answers Ask on posts, then the topics, each with its agent and how often it looks (every 30 minutes to
every 12 hours, or daily at 8:00). Clicking a topic renames it; its X removes it. The input at the bottom adds a topic
(at most 80 characters, newlines become spaces) with its agent and how often, or renames the one being edited.

---

## C. Artifacts

- The tab is `src/screens/artifacts.rs`, an artifact's page is `src/screens/artifact.rs` and the tab's settings are
  `src/screens/artifact_settings.rs`. `src/local_backend/artifacts.rs` answers `canvasArtifacts:*` and
  `artifactShare:*` (its `ARTIFACTS` list) from `crates/filo-local/src/artifacts.rs`. Agents make and change artifacts
  with the MCP tools in `crates/filo-local/src/artifacts_mcp.rs`.
- An artifact is an HTML page an agent wrote. Each version is its own file beside Filo's database
  (`artifacts/<id>/v<n>.html`), written once. Restoring a version saves its page again as a new version.

### Grid

- The tab asks `canvasArtifacts:list` for 100. The backend clamps the limit to 1–100 (default 50) and orders by
  `updatedAt`, newest first; the tab sorts by `createdAt`, newest first. There's no pagination.
- Two to four columns, by window width. A card shows the summary (or a page icon), the title and the day it was made.
  One never opened (no `openedAt`) has a dot and "· New".
- With none, the tab says "A home for finished work."

### Artifact page

- Opening it calls `canvasArtifacts:markOpened` when `openedAt` is missing.
- One menu at the top right picks Preview or Source, and a version (`canvasArtifacts:listVersions`, the latest 50).
  With an older version shown, it adds Show current version and "Restore this version…", which asks "Restore version
  {n}?" first. "Showing the latest {n} versions." shows when there are more.
- The header also has Open in browser, Show in Finder, Open on claude.ai (once published there), Download as HTML and
  Share (a link on filo.im, below); once shared, a bar under the header has the link and Unshare.
- Preview is a sandboxed web view: no network, and a link opens in the browser. A page with the `<!--filo:head-->`
  marker gets the app's theme and font there (`themed_page` in `src/screens/chat/glance.rs`) and is rebuilt when the
  theme changes. Pages without it show as they are.
- A page's `filo.chat(text)` (or the phone's `{ type: 'filo:chat', text }` message), clipped to 2,000 characters, asks
  the user first ("Send this to Filo?"). Then `canvasArtifacts:sendArtifactMessage` sends it to the conversation the
  artifact came from, else the main agent's, and the app opens it.
- Source shows "HTML · version {n}", then the page in the monospace font from Appearance.
- A missing artifact shows "This artifact moved." and "Filo may have archived or replaced it."

### Sharing

An artifact's page has Share, which puts its current page at `filo.im/artifacts/<id>` for anyone with the link
(unlisted; Share again after a change updates it, Unshare deletes it; `src/local_backend/artifact_link.rs`). Besides
that, the tab's settings, shown only while Claude Code is signed in to claude.ai, have "Share to
claude.ai", off by default. While it's on, `canvasArtifacts:publish` has Claude Code publish the current version to the
user's claude.ai account; the link is copied and kept on the artifact (`claudeLink`). `shareSlug` is always empty and
`isPublic` is always false.

---

## D. Extensions (the Connectors tab)

- The tab is called Extensions; the code keeps "connectors" (`Tab::Connectors`). It has two pages: Apps
  (`src/screens/connectors.rs`, with an app's page in `connector.rs`, the add and edit form in `connector_form.rs` and
  "Your computer" in `computer_connector.rs`) and Skills (`src/screens/skills.rs`).
- Apps are the user's MCP servers, local or remote, which the coding tools get in every chat and agent conversation.
  They're kept in Filo's database (`crates/filo-local/src/apps.rs`, ready-made ones in its `PRESETS`), with keys and
  sign-ins in the Keychain. `src/local_backend/apps.rs` answers `apps:*` and `computer:*`.
- Each of an app's tools is Allow or Block for the agents. An app with more than 6 tools gets a search field over the
  list.

---

## E. Appearance (`src/screens/appearance.rs`)

- The settings are `Prefs` in `src/config.rs` (defaults in `Prefs::default`), kept on this Mac in `desktop.json` in the
  config folder (`~/.config/filo`, or `FILO_CONFIG_DIR`).
- The page has a live preview, then:
  - Mode: Auto, Light or Dark. Auto follows macOS.
  - Contrast: Auto, Standard or High. macOS has no contrast setting to follow, so Auto means Standard.
  - Typography: the interface font and size, and the monospace font and size (code blocks and artifact source), with
    "Reset to defaults".
  - Theme: one row per family, with a light and a dark swatch, its label and description.
- `apply` in `src/theme.rs` resolves the palette from `Prefs` and the system appearance.
- The families' labels, descriptions and palettes are in `assets/themes.json`, in the order `families()` gives. The
  phone app's theme engine made the file (`scripts/dump-themes.ts`, which runs in the phone app's code).
- Settings › App › Appearance opens the page. Its subtitle is "{family} · {mode}", plus "· High contrast" when contrast
  is high.

---

## F. Design tokens

- `src/theme.rs` has the spacing (`space`), radii (`radius`), layout widths (`MAX_CONTENT_WIDTH`, `PAGE_PADDING`,
  `HEADER_HEIGHT`), the palette roles (`Palette`) and the type scale (`TypeVariant`). `assets/themes.json` has each
  family's palettes: standard and high contrast, light and dark. The components are in `src/ui.rs`.
- The type scale keeps the phone's names (largeTitle to micro) at desktop sizes (body 14), and scales with the
  interface size picked in Appearance.
- The default typeface, Atkinson Hyperlegible Next, ships one file per weight in `assets/fonts`: Regular, Italic,
  Medium, SemiBold and Bold.
- `action` is for the one control that commits, such as a primary button or Send. `accent` is for state: selected,
  liked, saved.
- Colours come from the palette and type from `Typography`, never hard-coded.
