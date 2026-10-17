---
name: artifacts
description: "Make or change something for the user's Artifacts library in Filo: a report, a web page, a dashboard, a diagram or a small interactive tool, built as one HTML file from this skill's template and published with Filo's publish_artifact. Use whenever the user asks for a page, report, tracker, diagram or tool to keep, look at or use, to change one they have, or to have a coding task build one."
---

# Artifacts: pages the user keeps

An artifact is one HTML page in the user's Artifacts library in the Filo app on this Mac: a report, a page, a diagram or a small tool. The user opens it from its card in the chat or from the Artifacts tab, and every version is kept.

It works file first. You build the page as a file, edit that file with your file tools, and publish it with Filo's `publish_artifact`. The artifact remembers the file: publishing the same file again saves its next version (the user can restore any earlier one), and if they have it open it updates in front of them. Publishing a page that hasn't changed saves nothing new.

## 1. New, or a change?

- **A change to one you published in this conversation:** edit the same file and publish it again (step 5).
- **A change to one from elsewhere** (the user names it, or writes from its page): `list_artifacts` finds it, and `open_artifact` gives the file it's published from. An artifact made before it had a file gets one in `pages/` in your working folder. If open_artifact says the file has changes that aren't published, look at both before you go on.
- **New:** step 2.

## 2. Make the file

Copy `template.html`, which is in this skill's folder beside this file, to `pages/<short-name>.html` in your working folder, like `pages/q3-budget.html`. Name it for what it is, and keep that path for as long as it's this artifact: a file at another path makes another artifact. Don't use `/tmp` or another temporary folder, which may be gone before the next change, and don't use the user's own folders unless they ask.

Fill it in: delete the HOW TO USE comment and the parts you don't need, give the page a short `<title>` (the artifact's name by default), and write the content. Write and change the page with your file tools (Write and Edit in Claude Code, apply_patch in Codex) rather than a script that rewrites it, so each change is plain to see. Leave the `<head>` as it is. Its first `<style>` is the look outside Filo, the `<!--filo:head-->` marker is where the app puts its own theme and font, and the house style after it uses them.

## 3. Write it like Filo

- **The answer first.** The top of the page says what it is and what it found: the title, one sentence under it, then the status or the key numbers. Details come after.
- **Plain words.** Sentence-case headings that say something ("Groceries are 20% over"), short paragraphs, numbers with their units, dates written out. No filler.
- **Colours only from the theme's variables:** `--text`, `--textSecondary`, `--textTertiary`, `--background`, `--surface`, `--raised`, `--sunken`, `--border`, `--accent`, `--success`, `--warning`, `--danger`, and `--c1` to `--c4` for chart series. The page then works in light and dark mode. Never set `font-family` or load fonts: the app's font comes in at the marker.
- **Quiet layout.** One column up to 760px, with the template's cards, tables, pills (`done`, `now`, `wait`, `stop`), callouts and tabs. Avoid what makes a page look machine-made: everything centred, gradients, big hero banners, drop shadows, emoji as decoration, the same heavy rounding on everything.
- **Charts and diagrams as inline SVG** with a `viewBox` and no fixed width, labelled, with their point also said in words. Load the dataviz skill before you draw a chart.
- **Tools that really work.** Real inputs and buttons, and state kept in memory: the page has no storage (localStorage, sessionStorage, IndexedDB and cookies throw there). A button can ask Filo something about the page with `filo.chat("…")`, which the user confirms first.
- **Self-contained.** The page runs in a sandbox with no network. Scripts, styles and fonts from elsewhere don't load, and links open in the browser. Put them in the file: inline `<style>` and `<script>`, at most 2 MB in all. Pictures from the web (an `<img src="https://…">`, a CSS `url(https://…)`, or a `.png`/`.jpg` address in a script's string, like avatars) are fine: when you publish, Filo saves them into the artifact and the page shows those copies. Your own pictures go in as `data:` URLs.

## 4. Check for cut-off text and overflow

You don't know how wide the page will be: the artifact pane can be 360px wide or fill a large window. Read the page for these before you publish:

- Tables, code and wide diagrams scroll inside their own box (the template's `.scroll`), never the whole page.
- Long unbroken text (links, ids, file paths) can wrap. The template does this for links and code.
- No fixed widths or heights on boxes with text. No `overflow:hidden`, `white-space:nowrap` or `text-overflow:ellipsis` on text the reader needs.
- Children of a flex or grid row have `min-width:0` (the template's `.row`, `.cards` and `.status` do).

If you can run commands, `python3 <this skill's folder>/check_page.py <your file>` opens the page the way Filo shows it at 380, 720 and 1100px wide (it needs Chrome and takes about half a minute). It lists anything that sticks out or is cut off, and what Filo's sandbox would block. Add `--shots <folder>` to save screenshots you can look at. Fix what it lists and run it again.

## 5. Publish

Call `publish_artifact` with:

- `path`: the file.
- `title`: a short name for a new one. Leave it out later unless it should change.
- `summary`: one or two sentences for its card.
- `kind`: `report`, `page`, `diagram` or `tool`.
- `label`: a few words on this version, like "first draft", "added March" or "fixed totals".

Fix anything it lists under "Check:" and publish the file again.

## 6. Tell the user

Say what it is and what to look at in a line or two with `send_message`: the card in the chat opens it. Don't paste the page or its path. After a change, say only what changed.

## Changing it later

- Edit the file in place rather than starting again from the template, so its structure and ids stay. Start over only when its structure changes.
- Publish the same path, with a `label` saying what changed.
- For a page kept up to date (a status page, a tracker), keep an as-of date at the top, and keep the data it shows in the template's `<script type="application/json" id="page-data">` block. Next time, read the old data first, then tell the user what changed since then, not the whole story again.

## Big tools, and pages from a coding task

A tool with a lot of state or screens (a React app), or a page built from a project's code or data, can be handed to a coding task with `start_work`. In its notes, ask for one self-contained HTML file built from Filo's artifact template, which is `template.html` in this skill's folder (give its full path), at a path that stays the same from round to round, with the file's full path in its summary. It may use a bundler (Vite or Parcel with inlining), as long as the result is one file of at most 2 MB that loads nothing from the internet. When its round has finished, publish the file it names. After a later round, publish the same file again for the next version.

If you are that coding task, you don't have publish_artifact. Build and check the file as above, and give its full path in your summary: the assistant who handed you the task publishes it.

## Sharing outside Filo

Artifacts stay on this Mac. Share one to claude.ai only when the user asks and your instructions say sharing to claude.ai is on. Then follow those instructions, which use Filo's copy of the page rather than your file.

## Files

In this skill's folder:

- `template.html`: the page to start from, with the theme marker, the house style, the status line, cards, a table, pills, tabs, a figure and the data block.
- `check_page.py`: the overflow and sandbox check from step 4.
