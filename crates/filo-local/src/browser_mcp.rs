// Filo's browser for agents: the `browser_*` tools a chat turn gets. The browser itself is in the app (src/browser.rs):
// web pages on macOS's WebKit that keep the user's logins, which agents work in the background and the user can open
// (Window › Browser). A tool call goes to the running app over a Unix socket in the local data folder
// (`browser.sock`): one JSON line asks, one JSON line answers (`{"ok":true,"text":…,"png":…}` or
// `{"ok":false,"error":…}`). Without the app open there's no browser, and the tools say so. The user sees an agent
// working in a tab and can pause it there or take the tab over: its calls then wait (and say so when they've waited
// long), and the answer after says the user handed it back.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::mcp::Target;

/// The socket's file in the local data folder.
pub const SOCKET: &str = "browser.sock";

/// How long `browser_ask_user` waits for the user at most, in seconds; the agent can call it again to wait more.
pub const ASK_WAIT_SECONDS: u64 = 300;

const NAMES: &[&str] = &[
    "browser_open",
    "browser_navigate",
    "browser_read",
    "browser_click",
    "browser_type",
    "browser_select",
    "browser_upload",
    "browser_screenshot",
    "browser_click_at",
    "browser_hover",
    "browser_drag",
    "browser_keys",
    "browser_scroll",
    "browser_wait",
    "browser_dialog",
    "browser_tabs",
    "browser_close",
    "browser_ask_user",
];

/// The socket of the app whose local data folder is `root`.
pub fn socket_path(root: &Path) -> PathBuf {
    root.join(SOCKET)
}

/// Whether `name` is one of these tools.
pub fn handles(name: &str) -> bool {
    NAMES.contains(&name)
}

fn tab_property() -> Value {
    json!({ "type": "string", "description": "The tab's id, like \"t1\" (browser_open and browser_tabs give it)." })
}

pub fn tools() -> Vec<Value> {
    let read_only =
        json!({ "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true });
    let acts =
        json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true });
    vec![
        json!({
            "name": "browser_open",
            "title": "Open a web page",
            "description": "Open a web page in Filo's browser, in a new tab of your own. It runs in the background: it doesn't take the user's screen, mouse or keyboard. The user can watch you there (Filo's Window › Browser shows you're working and each click), pause you or take the tab over; then your calls wait until they let you go on, and the next answer says so. The browser keeps the user's logins (cookies) between launches, so sites they signed in to there know them. Returns the tab's id and the page as text, with a ref like [e5] for each link, button and field: use them with browser_click and browser_type. Answers also say what else happened: a pop-up the page opened (a new tab of yours), a download (saved where it says) or a dialog waiting. Use it for sites that need the user's account or a real browser (sign-ins, forms, pages built by JavaScript); for public pages you only read, your own web tools are quicker.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The address, like https://example.com (https:// is added when it's left out)." }
                },
                "required": ["url"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_navigate",
            "title": "Go to a page",
            "description": "In one of the browser's tabs: go to an address, or back, forward or reload. Waits for the page to load and returns its title and address.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "url": { "type": "string", "description": "The address to go to." },
                    "go": { "type": "string", "enum": ["back", "forward", "reload"], "description": "Instead of an address." }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_read",
            "title": "Read a page",
            "description": "The page in a tab as text: headings, text, and each link, button, field, checkbox, menu and item the page lets you drag with a ref like [e5] (refs stay the same for the same element while the page is open). Fields show what's in them. A frame from another site (a CAPTCHA, a payment form) can't be read: it's listed with where it is, for browser_screenshot and browser_click_at. Long pages are cut: give `find` for only the lines with those words.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "find": { "type": "string", "description": "Only the lines containing this (any case)." }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": read_only
        }),
        json!({
            "name": "browser_click",
            "title": "Click on a page",
            "description": "Click a link, button or other element by its ref from browser_read. It's scrolled into view and clicked with real mouse events where it's on top, else clicked through the page. Waits for what the click loads, and returns the page's title and address. A menu (<select>) isn't clicked: use browser_select.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "ref": { "type": "string", "description": "The element's ref, like \"e5\"." }
                },
                "required": ["tab", "ref"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_type",
            "title": "Type into a field",
            "description": "Type text into a field (a text box, a search box, a text area or an editable area) by its ref from browser_read, with real key presses, replacing what's in it unless `clear` is false. `submit` presses Return after it (sends a form, runs a search). For a menu, use browser_select; for a file field, browser_upload.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "ref": { "type": "string", "description": "The field's ref, like \"e7\"." },
                    "text": { "type": "string" },
                    "clear": { "type": "boolean", "description": "Replace what's in the field (the default) or add to it." },
                    "submit": { "type": "boolean", "description": "Press Return after typing." }
                },
                "required": ["tab", "ref", "text"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_select",
            "title": "Pick in a menu",
            "description": "Pick an option in a menu by its ref from browser_read: a <select> (browser_read lists its options) is set through the page, as choosing does; a menu the page draws itself (a combo box, a list box, a button that opens a list) is opened with a real click and its option clicked. `options` picks several in a list that takes several. Never click a <select> or press keys on it: its menu would open where only the user can work it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "ref": { "type": "string", "description": "The menu's ref, like \"e5\"." },
                    "option": { "type": "string", "description": "The option's text as shown (or its value)." },
                    "options": { "type": "array", "items": { "type": "string" }, "description": "Several options, for a list that takes several." }
                },
                "required": ["tab", "ref"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_upload",
            "title": "Upload files",
            "description": "Give files from this Mac to a page's file field, in the background: no file panel opens. `ref` is the file field (a `file` line of browser_read) or the button or drop area that opens the page's file chooser (a hidden field behind it is found). `paths` are the files' full paths. Says what the field holds after. When a page asks for a file without this, the answer says it got none.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "ref": { "type": "string", "description": "The file field's or its button's ref, like \"e5\"." },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Full paths of the files, like \"/Users/me/Documents/cv.pdf\"." }
                },
                "required": ["tab", "ref", "paths"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_screenshot",
            "title": "Screenshot a page",
            "description": "A picture of what a tab shows now (its viewport), and its size. Its pixels are the page's coordinates: browser_click_at and browser_drag take a spot in it as x and y from its top left. Use it to see a page, and for what browser_read can't reach: frames from other sites, canvases, maps, CAPTCHAs. `full` takes the whole page from its top instead (up to 6000 pixels tall), to see a long page at once; its spots aren't the view's, so take a plain one before clicking at a spot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "full": { "type": "boolean", "description": "The whole page, not only what shows." }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": read_only
        }),
        json!({
            "name": "browser_click_at",
            "title": "Click at a spot",
            "description": "Click at a spot of a tab with real mouse events (the page sees a real click, inside frames from other sites too), x and y in the pixels of browser_screenshot's picture from its top left. Doesn't move the user's mouse. Waits for what the click loads.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "x": { "type": "number" },
                    "y": { "type": "number" },
                    "double": { "type": "boolean", "description": "A double-click." },
                    "button": { "type": "string", "enum": ["left", "right"], "description": "Left (the default) or right." }
                },
                "required": ["tab", "x", "y"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_hover",
            "title": "Point at something",
            "description": "Move the pointer onto an element (by its ref from browser_read) or a spot (x, y of browser_screenshot's picture) with a real mouse move, and leave it there: for menus that open on hover, tooltips and previews. Says what's new on the page after; doesn't move the user's mouse.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "ref": { "type": "string" },
                    "x": { "type": "number" },
                    "y": { "type": "number" }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_drag",
            "title": "Drag on a page",
            "description": "Drag in a tab with real mouse events: pressed at one spot, moved to another in small steps, and let go. For reordering tiles, cards or list items, sliders and handles, moving things on a board or map, and dropping onto a drop area. Each end is an element's ref from browser_read (its middle; with both by ref the page scrolls so both show) or a spot x, y of browser_screenshot's picture. When the page makes it a drag and drop (draggable items), it gets real drag events (dragstart, dragover, drop) too. Doesn't move the user's mouse or take their screen: never drag in Filo's browser on screen. Says whether the page took a drop; browser_read or browser_screenshot shows what changed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "from_ref": { "type": "string", "description": "The element to drag, like \"e5\" (or give from_x and from_y)." },
                    "from_x": { "type": "number" },
                    "from_y": { "type": "number" },
                    "to_ref": { "type": "string", "description": "The element to drop on, like \"e9\" (or give to_x and to_y)." },
                    "to_x": { "type": "number" },
                    "to_y": { "type": "number" }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_keys",
            "title": "Press keys",
            "description": "Real key presses in a tab, to whatever has the focus there (click a field first with browser_click_at, or use browser_type): `text` is typed as it is, then each of `keys` is pressed, like \"Enter\", \"Tab\", \"Escape\", \"Backspace\", \"ArrowDown\", \"PageDown\", \"Space\" or a combination like \"cmd+a\" or \"shift+Tab\". Doesn't use the user's keyboard.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "text": { "type": "string" },
                    "keys": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_scroll",
            "title": "Scroll a page",
            "description": "Scroll a tab's page by `y` pixels (down; negative goes up) and `x` (right), or to an element by its ref.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "y": { "type": "number" },
                    "x": { "type": "number" },
                    "ref": { "type": "string" }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_wait",
            "title": "Wait for a page",
            "description": "Wait in a tab until `text` shows on the page (or, with `gone`, until it's gone: a spinner, \"Loading…\"), until a download ends (`for`: \"download\"), or, with neither, until the page has loaded and stopped changing. Up to `seconds` (10 by default, 60 at most). Use it after an action that takes a while instead of reading again and again.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "text": { "type": "string", "description": "Words to wait for (any case)." },
                    "gone": { "type": "boolean", "description": "Wait until `text` is gone instead." },
                    "for": { "type": "string", "enum": ["download"] },
                    "seconds": { "type": "number" }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": read_only
        }),
        json!({
            "name": "browser_dialog",
            "title": "Answer a page's dialog",
            "description": "Answer the alert, confirm (\"Are you sure?\") or prompt a page shows: answers say when one is waiting, and the page's script stops until it's answered. `action` accept (OK, the default) or dismiss (Cancel); `text` is a prompt's answer. Accept only what the user asked for: a confirm usually guards something hard to undo.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "action": { "type": "string", "enum": ["accept", "dismiss"] },
                    "text": { "type": "string" }
                },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
        json!({
            "name": "browser_tabs",
            "title": "List tabs",
            "description": "The browser's open tabs: id, title, address, who opened each (you, another agent or the user), pop-ups and the tab that opened them, whether the user has it on screen, paused you there or took it over, and a dialog waiting. Files your tabs download go to Filo's downloads folder, and answers say where.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": read_only
        }),
        json!({
            "name": "browser_close",
            "title": "Close a tab",
            "description": "Close a tab you're done with. Logins stay.",
            "inputSchema": {
                "type": "object",
                "properties": { "tab": tab_property() },
                "required": ["tab"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "browser_ask_user",
            "title": "Ask the user to step in",
            "description": format!("Ask the user to do something in a tab you can't: a CAPTCHA or \"are you human\" check, a sign-in with their password or a code sent to their phone. Try first yourself (a check box can often be ticked with browser_screenshot and browser_click_at); ask only once that failed. The user gets a card in Filo that opens the tab in the browser, with your reason above it and a Done button. Waits up to {} minutes for them, and says whether they're done; when they aren't yet, call it again to keep waiting.", ASK_WAIT_SECONDS / 60),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tab": tab_property(),
                    "reason": { "type": "string", "description": "What you need them to do, in a short sentence, like \"Solve the CAPTCHA so I can search flights\"." }
                },
                "required": ["tab", "reason"],
                "additionalProperties": false
            },
            "annotations": acts
        }),
    ]
}

/// A tool call: asks the app and returns the tool's result (text, and a picture for a screenshot).
pub fn call(target: &Target, name: &str, arguments: &Value) -> Value {
    let root = target.db.parent().map(PathBuf::from).unwrap_or_default();
    let agent = crate::feed_mcp::origin_in_chat(&target.db, &target.chat_id).author;
    let mut request = match arguments {
        Value::Object(map) => Value::Object(map.clone()),
        _ => json!({}),
    };
    request["op"] = json!(name.trim_start_matches("browser_"));
    request["chat"] = json!(target.chat_id);
    request["agent"] = json!(agent);
    // An action waits up to 100 s while the user holds the tab, then does its part.
    let wait =
        if name == "browser_ask_user" { Duration::from_secs(ASK_WAIT_SECONDS + 60) } else { Duration::from_secs(180) };
    match ask(&socket_path(&root), &request, wait) {
        Ok(reply) => content(&reply),
        Err(error) => crate::mcp::tool_result(Err(error)),
    }
}

/// The tool's result from the app's reply.
fn content(reply: &Value) -> Value {
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = reply.get("error").and_then(Value::as_str).unwrap_or("The browser didn't answer.");
        return crate::mcp::tool_result(Err(error.to_string()));
    }
    let mut blocks =
        vec![json!({ "type": "text", "text": reply.get("text").and_then(Value::as_str).unwrap_or_default() })];
    if let Some(png) = reply.get("png").and_then(Value::as_str) {
        blocks.push(json!({ "type": "image", "data": png, "mimeType": "image/png" }));
    }
    crate::mcp::tool_content(blocks, false)
}

/// One request to the app's browser at `socket`, waiting up to `wait` for its answer.
pub fn ask(socket: &Path, request: &Value, wait: Duration) -> Result<Value, String> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|_| "Filo's browser isn't running: the Filo app has to be open for the browser tools.".to_string())?;
    stream.set_read_timeout(Some(wait)).map_err(|error| error.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(|error| error.to_string())?;
    let mut line = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|error| format!("Couldn't reach Filo's browser: {error}"))?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(|error| format!("Filo's browser didn't answer: {error}"))?;
    if reply.trim().is_empty() {
        return Err("Filo's browser closed without answering (the app may have quit).".into());
    }
    serde_json::from_str(&reply).map_err(|error| format!("Filo's browser gave a bad answer: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_handled_and_listed_once() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|tool| tool["name"].as_str()).collect();
        assert_eq!(names, NAMES);
        assert!(names.iter().all(|name| handles(name)));
    }

    #[test]
    fn a_reply_becomes_text_and_a_picture() {
        let reply = json!({ "ok": true, "text": "Viewport 10×10", "png": "iVBOR" });
        let result = content(&reply);
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["text"], "Viewport 10×10");
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["mimeType"], "image/png");
        let failed = content(&json!({ "ok": false, "error": "No tab t9." }));
        assert_eq!(failed["isError"], true);
        assert_eq!(failed["content"][0]["text"], "No tab t9.");
    }

    #[test]
    fn no_app_says_so() {
        let dir = std::env::temp_dir().join(format!("filo-browser-test-{}", std::process::id()));
        let error = ask(&dir.join(SOCKET), &json!({ "op": "tabs" }), Duration::from_secs(1)).unwrap_err();
        assert!(error.contains("has to be open"), "{error}");
    }
}
