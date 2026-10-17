// Note text helpers: a note's preview, toggling a task box, and bare links. They follow the note editor's
// JavaScript (web/note-editor.ts), matched by hand here (JS lookbehind doesn't port to the regex crate); JS `\s`,
// `\w`, `.` and `^`/`$` semantics are kept, so the results match the editor character for character.
// Offsets in the editing helpers are UTF-8 byte offsets into the body.

/// Longest body a note keeps, in UTF-16 units like JS `.length`.
pub const MAX_BODY_LENGTH: usize = 8_000;
pub const MAX_TITLE_LENGTH: usize = 120;
const NOTE_PREVIEW_MAX_CHARS: usize = 240;
const TRAILING_PUNCTUATION: &str = ".,:;!?'\"*_~|\u{200B}";
const SHIELD_OPEN: char = '\u{E000}';
const SHIELD_CLOSE: char = '\u{E001}';

/// JS `\s` (WhiteSpace and LineTerminator).
fn js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// Characters JS `.` doesn't match.
fn line_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// JS `\w`.
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `A-Za-z0-9À-￿`, as the editor's link pattern has it (astral characters are surrogates there, so they count).
fn url_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c as u32 >= 0xC0
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(js_ws)
}

pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn starts_with_ci(chars: &[char], at: usize, prefix: &str) -> bool {
    let mut index = at;
    for p in prefix.chars() {
        match chars.get(index) {
            Some(c) if c.to_ascii_lowercase() == p => index += 1,
            _ => return false,
        }
    }
    true
}

// --- Bare URLs ---

/// End of `(?:https?:\/\/|www\.)[^\s<>`]+` matched at `at`, or None.
fn bare_url_run(chars: &[char], at: usize) -> Option<usize> {
    let prefix = ["https://", "http://", "www."].into_iter().find(|prefix| starts_with_ci(chars, at, prefix))?;
    let start = at + prefix.len();
    let mut end = start;
    while end < chars.len() && !js_ws(chars[end]) && !matches!(chars[end], '<' | '>' | '`') {
        end += 1;
    }
    (end > start).then_some(end)
}

/// `^(?:https?://[WORD]|www\.[WORD-]+\.[WORD])`, case-insensitive.
fn valid_url(url: &[char]) -> bool {
    for prefix in ["https://", "http://"] {
        if starts_with_ci(url, 0, prefix) {
            return url.get(prefix.len()).is_some_and(|c| url_word(*c));
        }
    }
    if starts_with_ci(url, 0, "www.") {
        let mut index = 4;
        while index < url.len() && (url_word(url[index]) || url[index] == '-') {
            index += 1;
        }
        return index > 4 && url.get(index) == Some(&'.') && url.get(index + 1).is_some_and(|c| url_word(*c));
    }
    false
}

/// `matchBareUrl`: a bare URL starting at `index`, trimmed of the punctuation that ends a
/// sentence or wraps the URL. Returns the end index (in `chars`).
fn match_bare_url(chars: &[char], index: usize) -> Option<usize> {
    if index > 0 {
        let before = chars[index - 1];
        if url_word(before) || matches!(before, '/' | '.' | '@' | ':' | '+' | '-') {
            return None;
        }
    }
    let mut end = bare_url_run(chars, index).unwrap_or(index);
    while end > index {
        let raw = &chars[index..end];
        let last = raw[raw.len() - 1];
        let opener = match last {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        };
        let unbalanced = opener.is_some_and(|opener| {
            raw.iter().filter(|c| **c == opener).count() < raw.iter().filter(|c| **c == last).count()
        });
        if !TRAILING_PUNCTUATION.contains(last) && !unbalanced {
            break;
        }
        end -= 1;
    }
    let url: Vec<char> = chars[index..end].iter().copied().filter(|c| *c != '\u{200B}').collect();
    valid_url(&url).then_some(end)
}

// --- Checklists and tasks ---

/// `CHECKLIST_LINE = /^\s*(?:[-*+]|\d+[.)])?\s*\[( |x|X)\]\s*(.*)$/` → (checked, trimmed text).
fn checklist_line(line: &str) -> Option<(bool, String)> {
    let c: Vec<char> = line.chars().collect();
    let n = c.len();
    let mut i = 0;
    while i < n && js_ws(c[i]) {
        i += 1;
    }
    if i < n && matches!(c[i], '-' | '*' | '+') {
        i += 1;
    } else {
        let start = i;
        while i < n && c[i].is_ascii_digit() {
            i += 1;
        }
        if !(i > start && i < n && matches!(c[i], '.' | ')')) {
            i = start;
        } else {
            i += 1;
        }
    }
    while i < n && js_ws(c[i]) {
        i += 1;
    }
    if i + 2 >= n || c[i] != '[' || !matches!(c[i + 1], ' ' | 'x' | 'X') || c[i + 2] != ']' {
        return None;
    }
    let checked = c[i + 1] != ' ';
    i += 3;
    while i < n && js_ws(c[i]) {
        i += 1;
    }
    let rest: String = c[i..].iter().collect();
    if rest.chars().any(line_term) {
        return None;
    }
    Some((checked, js_trim(&rest).to_string()))
}

/// Every non-blank line is a checklist item: the items, else None.
fn parse_checklist(body: &str) -> Option<Vec<(bool, String)>> {
    let mut items = Vec::new();
    for line in body.split('\n') {
        if js_trim(line).is_empty() {
            continue;
        }
        items.push(checklist_line(line)?);
    }
    (!items.is_empty()).then_some(items)
}

/// `TASK_LIST_LINE = /^(\s*(?:[-*+]|\d+[.)])\s*)\[( |x|X)\](.*)$/` → (prefix bytes, checked, rest start bytes).
fn task_line(line: &str) -> Option<(usize, bool, usize)> {
    let b = line.as_bytes();
    let n = b.len();
    let skip_ws = |mut i: usize| {
        while let Some(c) = line[i..].chars().next().filter(|c| js_ws(*c)) {
            i += c.len_utf8();
        }
        i
    };
    let mut i = skip_ws(0);
    if i < n && matches!(b[i], b'-' | b'*' | b'+') {
        i += 1;
    } else {
        let start = i;
        while i < n && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start || i >= n || !matches!(b[i], b'.' | b')') {
            return None;
        }
        i += 1;
    }
    let prefix = skip_ws(i);
    if prefix + 2 >= n || b[prefix] != b'[' || !matches!(b[prefix + 1], b' ' | b'x' | b'X') || b[prefix + 2] != b']' {
        return None;
    }
    let rest = prefix + 3;
    if line[rest..].chars().any(line_term) {
        return None;
    }
    Some((prefix, b[prefix + 1] != b' ', rest))
}

/// Flips the task box on the note's `line` (0-based, as markdown-it numbers source
/// lines for the note page's reader); any other line is left as it is.
pub fn toggle_task_line(body: &str, line: usize) -> String {
    let mut lines: Vec<String> = body.split('\n').map(str::to_string).collect();
    let Some(text) = lines.get_mut(line) else { return body.to_string() };
    // A task inside a quote (`> - [ ] x`) keeps its quote markers.
    let quote = text.len() - text.trim_start_matches(['>', ' ']).len();
    let Some((prefix, checked, rest)) = task_line(&text[quote..]) else { return body.to_string() };
    let (prefix, rest) = (quote + prefix, quote + rest);
    *text = format!("{}[{}]{}", &text[..prefix], if checked { ' ' } else { 'x' }, &text[rest..]);
    lines.join("\n")
}

/// `notePreview`: one line of plain text for the notes list, at most 240 UTF-16 units.
pub fn note_preview(body: &str) -> String {
    let full = full_note_preview(body);
    let mut units = 0;
    let mut out = String::new();
    for c in full.chars() {
        units += c.len_utf16();
        if units > NOTE_PREVIEW_MAX_CHARS {
            break;
        }
        out.push(c);
    }
    out
}

fn full_note_preview(body: &str) -> String {
    if let Some(items) = parse_checklist(body) {
        return items
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .take(4)
            .map(|(checked, text)| format!("{} {text}", if checked { '☑' } else { '☐' }))
            .collect::<Vec<_>>()
            .join("  ");
    }
    let chars: Vec<char> = body.chars().collect();
    let chars = numbered_task_glyphs(&bullet_task_glyphs(&chars));
    let text: String = chars.iter().collect();
    let stripped = text.split('\n').map(strip_block_line).collect::<Vec<_>>().join("\n");
    collapse_ws(&strip_inline_markdown(&stripped))
}

/// `[-*+]\s*\[ \]` → ☐ and `[-*+]\s*\[x\]` (any case) → ☑, anywhere.
fn bullet_task_glyphs(s: &[char]) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if matches!(s[i], '-' | '*' | '+') {
            let mut j = i + 1;
            while j < n && js_ws(s[j]) {
                j += 1;
            }
            if j + 2 < n && s[j] == '[' && s[j + 2] == ']' {
                let glyph = match s[j + 1] {
                    ' ' => Some('☐'),
                    'x' | 'X' => Some('☑'),
                    _ => None,
                };
                if let Some(glyph) = glyph {
                    out.push(glyph);
                    i = j + 3;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `^\s*\d+[.)]\s*\[ \]` → ☐ and `^\s*\d+[.)]\s*\[[xX]\]` → ☑, at line starts (`m` flag).
fn numbered_task_glyphs(s: &[char]) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if i == 0 || line_term(s[i - 1]) {
            let mut j = i;
            while j < n && js_ws(s[j]) {
                j += 1;
            }
            let digits = j;
            while j < n && s[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits && j < n && matches!(s[j], '.' | ')') {
                j += 1;
                while j < n && js_ws(s[j]) {
                    j += 1;
                }
                if j + 2 < n && s[j] == '[' && s[j + 2] == ']' {
                    let glyph = match s[j + 1] {
                        ' ' => Some('☐'),
                        'x' | 'X' => Some('☑'),
                        _ => None,
                    };
                    if let Some(glyph) = glyph {
                        out.push(glyph);
                        i = j + 3;
                        continue;
                    }
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn skip_ws(l: &[char], mut k: usize) -> usize {
    while k < l.len() && js_ws(l[k]) {
        k += 1;
    }
    k
}

/// `stripBlockMarkdown` for one line: heading, quote, bullet and number markers, fence and rule lines.
fn strip_block_line(line: &str) -> String {
    let mut l: Vec<char> = line.chars().collect();
    // ^#{1,6}\s+
    let hashes = l.iter().take_while(|c| **c == '#').count();
    if (1..=6).contains(&hashes) && l.get(hashes).is_some_and(|c| js_ws(*c)) {
        l.drain(..skip_ws(&l, hashes));
    }
    // ^>\s?
    if l.first() == Some(&'>') {
        let k = if l.get(1).is_some_and(|c| js_ws(*c)) { 2 } else { 1 };
        l.drain(..k);
    }
    // ^\s*[-*+]\s+
    let k = skip_ws(&l, 0);
    if l.get(k).is_some_and(|c| matches!(c, '-' | '*' | '+')) && l.get(k + 1).is_some_and(|c| js_ws(*c)) {
        l.drain(..skip_ws(&l, k + 1));
    }
    // ^\s*\d+[.)]\s+
    let k = skip_ws(&l, 0);
    let mut m = k;
    while m < l.len() && l[m].is_ascii_digit() {
        m += 1;
    }
    if m > k && l.get(m).is_some_and(|c| matches!(c, '.' | ')')) && l.get(m + 1).is_some_and(|c| js_ws(*c)) {
        l.drain(..skip_ws(&l, m + 1));
    }
    // ^\s*(`{3,}|~{3,}).*$
    let k = skip_ws(&l, 0);
    if let Some(&fence) = l.get(k).filter(|c| matches!(c, '`' | '~')) {
        let run = l[k..].iter().take_while(|c| **c == fence).count();
        if run >= 3 && !l[k + run..].iter().any(|c| line_term(*c)) {
            l.clear();
        }
    }
    // ^\s*([-*_])(\s*\1){2,}\s*$
    let k = skip_ws(&l, 0);
    if let Some(&rule) = l.get(k).filter(|c| matches!(c, '-' | '*' | '_')) {
        let mut count = 1;
        let mut m = k + 1;
        loop {
            m = skip_ws(&l, m);
            if m < l.len() && l[m] == rule {
                count += 1;
                m += 1;
            } else {
                break;
            }
        }
        if m == l.len() && count >= 3 {
            l.clear();
        }
    }
    l.into_iter().collect()
}

/// `LINK_DESTINATION`: `\((?:<[^>\n]*>|(?:[^()\s]|\([^()\s]*\))*)(?:\s+"[^"\n]*")?\)` at `i`; returns the end.
fn link_dest(s: &[char], i: usize) -> Option<usize> {
    let n = s.len();
    if s.get(i) != Some(&'(') {
        return None;
    }
    let close = |k: usize| -> Option<usize> {
        let m = skip_ws(s, k);
        if m > k && s.get(m) == Some(&'"') {
            let mut q = m + 1;
            while q < n && s[q] != '"' && s[q] != '\n' {
                q += 1;
            }
            if q < n && s[q] == '"' && s.get(q + 1) == Some(&')') {
                return Some(q + 2);
            }
        }
        (s.get(k) == Some(&')')).then_some(k + 1)
    };
    if s.get(i + 1) == Some(&'<') {
        let mut q = i + 2;
        while q < n && s[q] != '>' && s[q] != '\n' {
            q += 1;
        }
        if q < n
            && s[q] == '>'
            && let Some(end) = close(q + 1)
        {
            return Some(end);
        }
    }
    let mut k = i + 1;
    loop {
        if k < n && s[k] != '(' && s[k] != ')' && !js_ws(s[k]) {
            k += 1;
            continue;
        }
        if k < n && s[k] == '(' {
            let mut q = k + 1;
            while q < n && s[q] != '(' && s[q] != ')' && !js_ws(s[q]) {
                q += 1;
            }
            if q < n && s[q] == ')' {
                k = q + 1;
                continue;
            }
        }
        break;
    }
    close(k)
}

/// `![alt](dest)` → alt (image) or `[label](dest)` → label.
fn strip_links(s: &[char], image: bool) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let label_start = if image && s[i] == '!' && s.get(i + 1) == Some(&'[') {
            Some(i + 2)
        } else if !image && s[i] == '[' {
            Some(i + 1)
        } else {
            None
        };
        if let Some(start) = label_start {
            let mut q = start;
            while q < n && s[q] != ']' {
                q += 1;
            }
            if q < n
                && let Some(end) = link_dest(s, q + 1)
            {
                out.extend_from_slice(&s[start..q]);
                i = end;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `<((?:https?|mailto):[^\s<>]+)>` → the URL, case-insensitive.
fn strip_angle_links(s: &[char]) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if s[i] == '<' {
            let scheme = ["https:", "http:", "mailto:"].into_iter().find(|scheme| starts_with_ci(s, i + 1, scheme));
            if let Some(scheme) = scheme {
                let start = i + 1 + scheme.len();
                let mut q = start;
                while q < n && !js_ws(s[q]) && s[q] != '<' && s[q] != '>' {
                    q += 1;
                }
                if q > start && q < n && s[q] == '>' {
                    out.extend_from_slice(&s[i + 1..q]);
                    i = q + 1;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// Sets bare URLs aside as `\u{E000}n\u{E001}` so their `_` and `*` aren't read as emphasis.
fn shield_urls(s: &[char], urls: &mut Vec<Vec<char>>) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if let Some(end) = bare_url_run(s, i) {
            match match_bare_url(s, i) {
                Some(url_end) => {
                    urls.push(s[i..url_end].to_vec());
                    out.push(SHIELD_OPEN);
                    out.extend((urls.len() - 1).to_string().chars());
                    out.push(SHIELD_CLOSE);
                    out.extend_from_slice(&s[url_end..end]);
                }
                None => out.extend_from_slice(&s[i..end]),
            }
            i = end;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `(?<!\w)(\*\*|__)(.+?)\1(?!\w)` → the content.
fn strip_strong(s: &[char]) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    'scan: while i < n {
        let delimiter = s[i];
        if matches!(delimiter, '*' | '_') && s.get(i + 1) == Some(&delimiter) && (i == 0 || !word(s[i - 1])) {
            let start = i + 2;
            let mut j = start + 1;
            while j < n && !line_term(s[j - 1]) {
                if s[j] == delimiter && s.get(j + 1) == Some(&delimiter) && !s.get(j + 2).is_some_and(|c| word(*c)) {
                    out.extend_from_slice(&s[start..j]);
                    i = j + 2;
                    continue 'scan;
                }
                j += 1;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `(?<!\w)(D)([^D\n]+?)\1(?!\w)` → the content, for D = `*` or `_`.
fn strip_emphasis(s: &[char], delimiter: char) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if s[i] == delimiter && (i == 0 || !word(s[i - 1])) {
            let mut q = i + 1;
            while q < n && s[q] != delimiter && s[q] != '\n' {
                q += 1;
            }
            if q > i + 1 && q < n && s[q] == delimiter && !s.get(q + 1).is_some_and(|c| word(*c)) {
                out.extend_from_slice(&s[i + 1..q]);
                i = q + 1;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `~~(.+?)~~` → the content.
fn strip_strike(s: &[char]) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    'scan: while i < n {
        if s[i] == '~' && s.get(i + 1) == Some(&'~') {
            let start = i + 2;
            let mut j = start + 1;
            while j < n && !line_term(s[j - 1]) {
                if s[j] == '~' && s.get(j + 1) == Some(&'~') {
                    out.extend_from_slice(&s[start..j]);
                    i = j + 2;
                    continue 'scan;
                }
                j += 1;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `` `([^`\n]*)` `` → the content.
fn strip_code(s: &[char]) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if s[i] == '`' {
            let mut q = i + 1;
            while q < n && s[q] != '`' && s[q] != '\n' {
                q += 1;
            }
            if q < n && s[q] == '`' {
                out.extend_from_slice(&s[i + 1..q]);
                i = q + 1;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn restore_urls(s: &[char], urls: &[Vec<char>]) -> Vec<char> {
    let n = s.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if s[i] == SHIELD_OPEN {
            let mut q = i + 1;
            while q < n && s[q].is_ascii_digit() {
                q += 1;
            }
            if q > i + 1 && q < n && s[q] == SHIELD_CLOSE {
                let index: String = s[i + 1..q].iter().collect();
                if let Some(url) = index.parse::<usize>().ok().and_then(|index| urls.get(index)) {
                    out.extend_from_slice(url);
                    i = q + 1;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn strip_inline_markdown(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut urls = Vec::new();
    let shielded = shield_urls(&strip_angle_links(&strip_links(&strip_links(&chars, true), false)), &mut urls);
    let plain = strip_code(&strip_strike(&strip_emphasis(&strip_emphasis(&strip_strong(&shielded), '*'), '_')));
    restore_urls(&plain, &urls).into_iter().collect()
}

/// `.replace(/\s+/g, ' ').trim()`
fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if js_ws(c) {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

// --- Editing ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_checklists() {
        assert_eq!(note_preview("- [ ] Milk\n- [x] Eggs"), "☐ Milk  ☑ Eggs");
        assert_eq!(note_preview("- [ ] Milk\n- [ ] \n- [x] Eggs"), "☐ Milk  ☑ Eggs");
        assert_eq!(note_preview("Shopping - [ ] Milk - [x] Eggs"), "Shopping ☐ Milk ☑ Eggs");
        assert_eq!(note_preview("  Hello\nworld  "), "Hello world");
    }

    #[test]
    fn preview_clips() {
        assert_eq!(utf16_len(&note_preview(&"word ".repeat(1000))), NOTE_PREVIEW_MAX_CHARS);
        let emoji = note_preview(&format!("{}😀 tail", "a".repeat(NOTE_PREVIEW_MAX_CHARS - 1)));
        assert_eq!(emoji, "a".repeat(NOTE_PREVIEW_MAX_CHARS - 1));
    }

    #[test]
    fn preview_markdown() {
        assert_eq!(note_preview("# Title\n> quoted\n- bullet\n1. numbered"), "Title quoted bullet numbered");
        assert_eq!(note_preview("Some **bold**, *italic*, and ~~done~~"), "Some bold, italic, and done");
        assert_eq!(note_preview("***both*** and `code`"), "both and code");
        assert_eq!(note_preview("[Filo](https://filo.im) and ![cat](https://x/y.png)"), "Filo and cat");
        assert_eq!(note_preview("```js\nconst a = 1\n```"), "const a = 1");
        assert_eq!(note_preview("my_var_name stays"), "my_var_name stays");
        assert_eq!(note_preview("Intro\n2. [ ] second"), "Intro ☐ second");
        assert_eq!(
            note_preview("[Wiki](https://en.wikipedia.org/wiki/Python_(programming_language)) page"),
            "Wiki page"
        );
        assert_eq!(note_preview("[Doc](<https://x.com/a b>) and <https://x.com/c_d>"), "Doc and https://x.com/c_d");
        assert_eq!(
            note_preview("see https://example.com/_foo_/*bar* and _this_"),
            "see https://example.com/_foo_/*bar* and this"
        );
        assert_eq!(note_preview("**https://x.com/a_b**"), "https://x.com/a_b");
        assert_eq!(note_preview("---\nafter"), "after");
    }

    #[test]
    fn toggles_the_task_on_a_line() {
        let body = "# Tasks\n- [ ] one\n> - [x] quoted\nplain";
        assert_eq!(toggle_task_line(body, 1), "# Tasks\n- [x] one\n> - [x] quoted\nplain");
        assert_eq!(toggle_task_line(body, 2), "# Tasks\n- [ ] one\n> - [ ] quoted\nplain");
        assert_eq!(toggle_task_line(body, 3), body);
        assert_eq!(toggle_task_line(body, 9), body);
    }
}
