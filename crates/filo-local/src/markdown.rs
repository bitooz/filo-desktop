// Markdown as plain text, for the places a message shows in one line (the chat list's preview, notifications): the
// words without their syntax, as the phone app's `markdownToPreviewText` reads them. Headings, quotes, list markers,
// task boxes, rules, code fences and table pipes go; emphasis, code spans, links and pictures keep their text.

/// The text of `markdown` on one line, without its syntax.
pub fn plain_text(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    // A table's header is the row above its `|---|---|` line.
    let separators: Vec<bool> = lines.iter().map(|line| is_table_separator(line)).collect();
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<String> = None;
    let mut in_table = false;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // A code block's lines aren't text to read; nor its fences.
        if let Some(open) = &fence {
            if trimmed.starts_with(open.as_str()) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = ["```", "~~~"].into_iter().find(|open| trimmed.starts_with(open)) {
            let run = trimmed.chars().take_while(|c| open.starts_with(*c)).count();
            fence = Some(open[..1].repeat(run));
            continue;
        }
        if trimmed.is_empty() {
            in_table = false;
            continue;
        }
        if separators[index] {
            in_table = true;
            continue;
        }
        if is_rule(trimmed) {
            continue;
        }
        let header = separators.get(index + 1).copied().unwrap_or(false);
        if (in_table || header || trimmed.starts_with('|')) && trimmed.contains('|') {
            let cells: Vec<String> = split_cells(trimmed)
                .iter()
                .map(|cell| strip_inline(cell.trim()))
                .filter(|cell| !cell.is_empty())
                .collect();
            out.push(cells.join(" "));
            continue;
        }
        let text = strip_inline(strip_block_markers(trimmed));
        if !text.trim().is_empty() {
            out.push(text);
        }
    }
    out.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A heading's hashes, a quote's `>`s, a list item's marker and its task box.
fn strip_block_markers(line: &str) -> &str {
    let mut text = line;
    loop {
        let before = text;
        let hashes = text.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && text[hashes..].chars().next().is_none_or(char::is_whitespace) {
            text = text[hashes..].trim_start();
            // A closing run of hashes: `## Title ##`.
            let end = text.trim_end_matches('#');
            if end.len() < text.len() && (end.is_empty() || end.ends_with(char::is_whitespace)) {
                text = end.trim_end();
            }
        }
        if let Some(rest) = text.strip_prefix('>') {
            text = rest.trim_start();
        }
        if let Some(rest) = ["- ", "* ", "+ "].into_iter().find_map(|marker| text.strip_prefix(marker)) {
            text = rest.trim_start();
        }
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        if (1..=9).contains(&digits)
            && let Some(rest) = text[digits..].strip_prefix(". ").or_else(|| text[digits..].strip_prefix(") "))
        {
            text = rest.trim_start();
        }
        if let Some(rest) = ["[ ] ", "[x] ", "[X] "].into_iter().find_map(|task| text.strip_prefix(task)) {
            text = rest.trim_start();
        }
        if text == before {
            return text;
        }
    }
}

/// `---`, `***`, `___` (spaces allowed between), or a heading's `===` underline.
fn is_rule(line: &str) -> bool {
    let compact: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3 && ['-', '*', '_', '='].iter().any(|mark| compact.iter().all(|c| c == mark))
}

/// A table's `| --- | :---: |` line.
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('|')
        && trimmed.contains('-')
        && split_cells(trimmed).iter().all(|cell| {
            let cell = cell.trim();
            !cell.is_empty() && cell.trim_matches(':').chars().all(|c| c == '-') && cell.contains('-')
        })
}

/// A table row's cells, without the outer pipes (`\|` stays in its cell).
fn split_cells(row: &str) -> Vec<String> {
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = if row.ends_with('|') && !row.ends_with("\\|") { &row[..row.len() - 1] } else { row };
    let mut cells = vec![String::new()];
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => cells.last_mut().unwrap().push(chars.next().unwrap()),
            '|' => cells.push(String::new()),
            _ => cells.last_mut().unwrap().push(c),
        }
    }
    cells
}

/// A line's text without emphasis, code ticks, link targets or escapes.
fn strip_inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        // `\*` is a star.
        if c == '\\' && chars.get(index + 1).is_some_and(char::is_ascii_punctuation) {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        // A code span's text as written.
        if c == '`' {
            let run = run_length(&chars, index);
            if let Some(close) = find_run(&chars, index + run, '`', run) {
                let code: String = chars[index + run..close].iter().collect();
                out.push_str(code.trim());
                index = close + run;
                continue;
            }
            out.extend(&chars[index..index + run]);
            index += run;
            continue;
        }
        // `[text](url)` and `![alt](url)`: the text.
        let image = c == '!' && chars.get(index + 1) == Some(&'[');
        if c == '[' || image {
            let open = if image { index + 1 } else { index };
            if let Some((label, end)) = link(&chars, open) {
                out.push_str(&strip_inline(&label));
                index = end;
                continue;
            }
        }
        // `<https://…>`: the address.
        if c == '<'
            && let Some(close) = chars[index + 1..].iter().position(|c| *c == '>').map(|offset| index + 1 + offset)
        {
            let inner: String = chars[index + 1..close].iter().collect();
            if (inner.starts_with("http://") || inner.starts_with("https://") || inner.starts_with("mailto:"))
                && !inner.contains(char::is_whitespace)
            {
                out.push_str(&inner);
                index = close + 1;
                continue;
            }
        }
        // Emphasis: `*`, `_`, `**`, `__`, `~~`, but not `2 * 3`, `snake_case` or `~5 min`.
        if matches!(c, '*' | '_' | '~') {
            let run = run_length(&chars, index);
            let before = index.checked_sub(1).map(|i| chars[i]);
            let after = chars.get(index + run).copied();
            let space = |c: Option<char>| c.is_none_or(char::is_whitespace);
            let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
            let literal =
                (space(before) && space(after)) || (c == '_' && word(before) && word(after)) || (c == '~' && run != 2);
            if !literal {
                index += run;
                continue;
            }
            out.extend(&chars[index..index + run]);
            index += run;
            continue;
        }
        out.push(c);
        index += 1;
    }
    out
}

/// How many of `chars[at]` in a row start there.
fn run_length(chars: &[char], at: usize) -> usize {
    chars[at..].iter().take_while(|c| **c == chars[at]).count()
}

/// Where the next run of exactly `length` `mark`s from `from` starts.
fn find_run(chars: &[char], from: usize, mark: char, length: usize) -> Option<usize> {
    let mut index = from;
    while index < chars.len() {
        if chars[index] == mark {
            let run = run_length(chars, index);
            if run == length {
                return Some(index);
            }
            index += run;
        } else {
            index += 1;
        }
    }
    None
}

/// The label of a `[label](target)` opening at `open`, and where it ends.
fn link(chars: &[char], open: usize) -> Option<(String, usize)> {
    let mut depth = 0;
    let mut close = None;
    for (offset, c) in chars[open..].iter().enumerate() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut depth = 0;
    for (offset, c) in chars[close + 1..].iter().enumerate() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[open + 1..close].iter().collect(), close + 1 + offset + 1));
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_reply_with_a_list_as_text() {
        assert_eq!(
            plain_text("Sent. Claude Code will:\n- **Remove \"Try again\"** from the menu\n- Keep `Copy`"),
            "Sent. Claude Code will: Remove \"Try again\" from the menu Keep Copy"
        );
    }

    #[test]
    fn drops_block_syntax() {
        assert_eq!(
            plain_text("# Title ##\n\n> quoted *text*\n\n1. one\n2) two\n- [x] done\n\n---\n\nend"),
            "Title quoted text one two done end"
        );
        assert_eq!(plain_text("Heading\n===\nBody"), "Heading Body");
        assert_eq!(plain_text("#hashtag stays"), "#hashtag stays");
    }

    #[test]
    fn keeps_link_and_picture_text() {
        assert_eq!(
            plain_text("See [the **docs**](https://x.y/a_(b)) and ![a chart](c.png) at <https://x.y>"),
            "See the docs and a chart at https://x.y"
        );
        assert_eq!(plain_text("[not a link] here"), "[not a link] here");
    }

    #[test]
    fn skips_code_blocks_and_keeps_code_spans() {
        assert_eq!(plain_text("Run this:\n```sh\nrm -rf **/*.pyc\n```\nthen ``a `b` c``"), "Run this: then a `b` c");
        assert_eq!(plain_text("~~~\ncode\n~~~\nafter"), "after");
    }

    #[test]
    fn reads_tables_cell_by_cell() {
        assert_eq!(
            plain_text("| Name | **Age** |\n|:---|---:|\n| Bo | 3 |\n| A \\| B | 4 |\n\nAfter"),
            "Name Age Bo 3 A | B 4 After"
        );
        assert_eq!(plain_text("Name | Age\n--- | ---\nBo | 3"), "Name Age Bo 3");
    }

    #[test]
    fn leaves_plain_punctuation_alone() {
        assert_eq!(
            plain_text("2 * 3 = 6, snake_case_name, ~5 min, a - b"),
            "2 * 3 = 6, snake_case_name, ~5 min, a - b"
        );
        assert_eq!(plain_text("__init__ and ~~gone~~ and \\*stars\\*"), "init and gone and *stars*");
        assert_eq!(plain_text("Hello there"), "Hello there");
    }
}
