//! Render Jira wiki markup for the terminal.
//!
//! Jira Data Center / Server (REST API v2) returns descriptions and comment
//! bodies as wiki markup strings. This renders the common subset: headings,
//! lists, tables, `{code}`/`{noformat}`, `{quote}`/`bq.`, panels, rules,
//! `{{mono}}`, `*bold*`, `_italic_`, `-strike-`, `+underline+`, links,
//! mentions and images. Anything else passes through as text: the goal is
//! readable output, not a faithful renderer.

use unicode_width::UnicodeWidthStr;

/// Text attributes of a span, mapped to ANSI codes or TUI styles.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Style {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// One output line; empty for a blank line.
pub type Line = Vec<Span>;

const MIN_WIDTH: usize = 20;
/// Rules stop here, so an unbounded width (piped output) stays sane.
const RULE_MAX: usize = 80;
const BULLETS: [&str; 3] = ["•", "◦", "▪"];
const QUOTE_MACROS: [&str; 6] = ["quote", "panel", "info", "note", "warning", "tip"];

/// The wiki markup of a description or comment body: API v2 returns a string,
/// Cloud ADF an object. `None` for ADF and with `--raw-markup`, callers then
/// show the plain text.
pub fn source(value: Option<&serde_json::Value>) -> Option<&str> {
    match value {
        Some(serde_json::Value::String(s)) if !crate::output::raw_markup() => Some(s),
        _ => None,
    }
}

/// Render wiki markup to terminal text, wrapped at `width` columns. With
/// `color` false no escape codes are emitted and `{{mono}}` keeps backticks.
pub fn render(src: &str, width: usize, color: bool) -> String {
    render_lines(src, width, color)
        .iter()
        .map(|line| paint(line, color))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render wiki markup to styled lines, wrapped at `width` columns. With
/// `styled` false the text carries the markup cues itself: backticks around
/// `{{mono}}` and rule lines under h1/h2.
pub fn render_lines(src: &str, width: usize, styled: bool) -> Vec<Line> {
    let src = src.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = src.lines().collect();
    let r = Renderer { styled };
    let mut out = Vec::new();
    r.blocks(&lines, width.max(MIN_WIDTH), &mut out);
    while out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    let start = out.iter().position(|l| !l.is_empty()).unwrap_or(out.len());
    out.split_off(start)
}

struct Renderer {
    styled: bool,
}

impl Renderer {
    fn blocks(&self, lines: &[&str], width: usize, out: &mut Vec<Line>) {
        let mut counters: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();
            if line.is_empty() {
                blank(out);
                counters.clear();
                i += 1;
                continue;
            }

            if let Some((name, params, rest)) = macro_open(line, &["code", "noformat"]) {
                let (body, after) = collect_macro(lines, &mut i, rest, name);
                if let Some(title) = macro_title(params) {
                    blank(out);
                    out.push(vec![span(title.to_string(), bold())]);
                }
                self.code_block(&body, width, out);
                self.trailing_text(after, width, out);
                counters.clear();
                continue;
            }

            if let Some((name, params, rest)) = macro_open(line, &QUOTE_MACROS) {
                let (body, after) = collect_macro(lines, &mut i, rest, name);
                let body: Vec<&str> = body.iter().map(String::as_str).collect();
                let mut inner = Vec::new();
                if let Some(title) = macro_title(params) {
                    inner.push(vec![span(title.to_string(), bold())]);
                }
                self.blocks(&body, width.saturating_sub(2), &mut inner);
                self.quoted(inner, out);
                self.trailing_text(after, width, out);
                counters.clear();
                continue;
            }

            if let Some(rest) = line.strip_prefix("bq. ") {
                let mut inner = Vec::new();
                self.paragraph(rest, width.saturating_sub(2), &mut inner);
                self.quoted(inner, out);
                counters.clear();
                i += 1;
                continue;
            }

            if let Some((level, text)) = heading(line) {
                self.heading(level, text, width, out);
                counters.clear();
                i += 1;
                continue;
            }

            if line.len() >= 4 && line.chars().all(|c| c == '-') {
                out.push(vec![span("─".repeat(width.min(RULE_MAX)), dim())]);
                counters.clear();
                i += 1;
                continue;
            }

            if line.starts_with('|') {
                let start = i;
                while i < lines.len() && lines[i].trim_start().starts_with('|') {
                    i += 1;
                }
                let rows: Vec<&str> = lines[start..i].iter().map(|l| l.trim()).collect();
                self.table(&rows, width, out);
                counters.clear();
                continue;
            }

            if let Some((marker, text)) = list_marker(line) {
                self.list_item(marker, text, &mut counters, width, out);
                i += 1;
                continue;
            }

            counters.clear();
            self.paragraph(line, width, out);
            i += 1;
        }
    }

    /// Text after a closing macro tag on the same line.
    fn trailing_text(&self, after: Option<String>, width: usize, out: &mut Vec<Line>) {
        if let Some(after) = after.filter(|a| !a.trim().is_empty()) {
            self.paragraph(after.trim(), width, out);
        }
    }

    fn paragraph(&self, text: &str, width: usize, out: &mut Vec<Line>) {
        let spans = self.inline(text, Style::default());
        out.extend(wrap(&spans, width));
    }

    fn heading(&self, level: usize, text: &str, width: usize, out: &mut Vec<Line>) {
        blank(out);
        let style = Style {
            bold: true,
            underline: self.styled && level <= 2,
            ..Style::default()
        };
        let lines = wrap(&self.inline(text, style), width);
        let text_width = lines.iter().map(|l| line_width(l)).max().unwrap_or(0);
        out.extend(lines);
        if !self.styled && level <= 2 {
            let rule = (if level == 1 { "=" } else { "-" }).repeat(text_width);
            out.push(vec![span(rule, Style::default())]);
        }
    }

    fn code_block(&self, body: &[String], width: usize, out: &mut Vec<Line>) {
        let indent = body
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let style = Style {
            mono: true,
            ..Style::default()
        };
        blank(out);
        for line in body {
            let text = line.get(indent..).unwrap_or(line.trim_start()).trim_end();
            // code is never reflowed, long lines are cut at the width
            for chunk in chunks(text, width.saturating_sub(2).max(1)) {
                out.push(vec![
                    span("  ".into(), Style::default()),
                    span(chunk, style),
                ]);
            }
        }
        blank(out);
    }

    fn quoted(&self, inner: Vec<Line>, out: &mut Vec<Line>) {
        for line in inner {
            let mut quoted = vec![span("│ ".into(), dim())];
            quoted.extend(line);
            out.push(quoted);
        }
    }

    fn list_item(
        &self,
        marker: &str,
        text: &str,
        counters: &mut Vec<usize>,
        width: usize,
        out: &mut Vec<Line>,
    ) {
        let depth = marker.chars().count();
        counters.resize(depth, 0);
        let bullet = if marker.ends_with('#') {
            counters[depth - 1] += 1;
            format!("{}.", counters[depth - 1])
        } else {
            counters[depth - 1] = 0;
            BULLETS[(depth - 1) % BULLETS.len()].to_string()
        };
        let indent = 2 * (depth - 1);
        let hang = indent + bullet.width() + 1;
        let spans = self.inline(text, Style::default());
        for (n, line) in wrap(&spans, width.saturating_sub(hang)).iter().enumerate() {
            let prefix = if n == 0 {
                format!("{}{bullet} ", " ".repeat(indent))
            } else {
                " ".repeat(hang)
            };
            let mut item = vec![span(prefix, Style::default())];
            item.extend(line.iter().cloned());
            out.push(item);
        }
    }

    fn table(&self, rows: &[&str], width: usize, out: &mut Vec<Line>) {
        let rows: Vec<Vec<(bool, Line)>> = rows
            .iter()
            .map(|row| {
                split_row(row)
                    .into_iter()
                    .map(|(header, text)| {
                        let style = Style {
                            bold: header,
                            ..Style::default()
                        };
                        (header, self.inline(text.trim(), style))
                    })
                    .collect()
            })
            .collect();
        let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }

        let mut widths = vec![1usize; ncols];
        for row in &rows {
            for (c, (_, cell)) in row.iter().enumerate() {
                widths[c] = widths[c].max(line_width(cell));
            }
        }
        // shrink the widest columns until the table fits, cells then wrap
        let available = width.saturating_sub(3 * ncols + 1);
        while widths.iter().sum::<usize>() > available {
            let (c, &w) = widths
                .iter()
                .enumerate()
                .max_by_key(|&(_, w)| *w)
                .expect("ncols > 0");
            // below this the table cannot fit anyway, let it overflow
            if w <= 1 {
                break;
            }
            widths[c] = w - 1;
        }

        let border = |l: &str, m: &str, r: &str| {
            let segs: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            vec![span(format!("{l}{}{r}", segs.join(m)), dim())]
        };
        let bar = span("│".into(), dim());

        let wrapped: Vec<Vec<Vec<Line>>> = rows
            .iter()
            .map(|row| {
                (0..ncols)
                    .map(|c| {
                        row.get(c)
                            .map_or_else(Vec::new, |(_, s)| wrap_hard(s, widths[c]))
                    })
                    .collect()
            })
            .collect();
        // separate rows only when some cell wraps, compact otherwise
        let multiline = wrapped.iter().flatten().any(|cell| cell.len() > 1);

        out.push(border("┌", "┬", "┐"));
        for (r, (row, cells)) in rows.iter().zip(&wrapped).enumerate() {
            let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for n in 0..height {
                let mut line = vec![bar.clone()];
                for (c, cell) in cells.iter().enumerate() {
                    let content = cell.get(n).map_or_else(Vec::new, Clone::clone);
                    let pad = widths[c].saturating_sub(line_width(&content));
                    line.push(span(" ".into(), Style::default()));
                    line.extend(content);
                    line.push(span(" ".repeat(pad + 1), Style::default()));
                    line.push(bar.clone());
                }
                out.push(line);
            }
            let header = !row.is_empty() && row.iter().all(|(h, _)| *h);
            let next_is_body = rows
                .get(r + 1)
                .is_some_and(|next| !next.iter().all(|(h, _)| *h));
            if (header && next_is_body) || (multiline && r + 1 < rows.len()) {
                out.push(border("├", "┼", "┤"));
            }
        }
        out.push(border("└", "┴", "┘"));
    }

    /// Parse inline markup into styled spans. `\n` in a span is a hard break.
    fn inline(&self, text: &str, base: Style) -> Vec<Span> {
        let chars: Vec<char> = text.chars().collect();
        let mut spans: Vec<Span> = Vec::new();
        let mut p = 0;
        while p < chars.len() {
            let c = chars[p];
            let prev = p.checked_sub(1).map(|q| chars[q]);
            let at_word_start = prev.is_none_or(|ch| ch.is_whitespace() || "([{\"'".contains(ch));

            // \\ is a line break, \x escapes x
            if c == '\\' && p + 1 < chars.len() {
                if chars[p + 1] == '\\' {
                    push(&mut spans, "\n", base);
                    p += 2;
                    continue;
                }
                // only markup characters are escapable, `C:\Users` stays
                if !chars[p + 1].is_alphanumeric() {
                    push(&mut spans, &chars[p + 1].to_string(), base);
                    p += 2;
                    continue;
                }
            }

            if c == '{'
                && chars.get(p + 1) == Some(&'{')
                && let Some(end) = find(&chars, p + 2, &['}', '}'])
            {
                let inner: String = chars[p + 2..end].iter().collect();
                if self.styled {
                    push(&mut spans, &inner, Style { mono: true, ..base });
                } else {
                    push(&mut spans, &format!("`{inner}`"), base);
                }
                p = end + 2;
                continue;
            }

            // {color:x}...{color} and {anchor:x}: keep the text, drop the tags
            if c == '{'
                && ["color", "anchor"].iter().any(|n| {
                    starts_with(&chars[p + 1..], n)
                        && matches!(chars.get(p + 1 + n.len()), Some('}' | ':'))
                })
                && let Some(end) = chars[p..].iter().position(|&ch| ch == '}')
            {
                p += end + 1;
                continue;
            }

            if c == '['
                && let Some(len) = self.link(&chars[p + 1..], base, &mut spans)
            {
                p += len + 2;
                continue;
            }

            if c == '!'
                && at_word_start
                && let Some(end) = chars[p + 1..].iter().position(|&ch| ch == '!')
            {
                let inner: String = chars[p + 1..p + 1 + end].iter().collect();
                let file = inner.split('|').next().unwrap_or("");
                if !file.is_empty() && file.contains('.') && !file.contains(char::is_whitespace) {
                    push(
                        &mut spans,
                        &format!("[image: {file}]"),
                        Style { dim: true, ..base },
                    );
                    p += end + 2;
                    continue;
                }
            }

            if at_word_start
                && (starts_with(&chars[p..], "http://") || starts_with(&chars[p..], "https://"))
            {
                let mut end = p;
                while end < chars.len() && !chars[end].is_whitespace() {
                    end += 1;
                }
                while end > p && ".,;:)]!?".contains(chars[end - 1]) {
                    end -= 1;
                }
                // jira escapes markup characters in urls, e.g. merge\_requests
                let url = unescape(&chars[p..end]);
                push(
                    &mut spans,
                    &url,
                    Style {
                        underline: true,
                        ..base
                    },
                );
                p = end;
                continue;
            }

            if let Some(style) = emphasis(c, base)
                // strike and underline leave no cue without color: keep the markers
                && (self.styled || !"-+".contains(c))
                && at_word_start
                && chars
                    .get(p + 1)
                    .is_some_and(|n| !n.is_whitespace() && *n != c)
                && let Some(end) = closing(&chars, p + 1, c)
            {
                let inner: String = chars[p + 1..end].iter().collect();
                spans.extend(self.inline(&inner, style));
                p = end + 1;
                continue;
            }

            push(&mut spans, &c.to_string(), base);
            p += 1;
        }
        spans
    }

    /// `[~user]`, `[text|url]`, `[url]`. `body` starts after `[`; returns the
    /// length of the content between the brackets, `None` if this is no link.
    fn link(&self, body: &[char], base: Style, spans: &mut Vec<Span>) -> Option<usize> {
        let len = body.iter().position(|&c| c == ']')?;
        let inner: String = body[..len].iter().collect();
        if inner.is_empty() || inner.contains('[') {
            return None;
        }
        if let Some(user) = inner.strip_prefix('~') {
            push(spans, &format!("@{user}"), Style { bold: true, ..base });
            return Some(len);
        }
        let (text, target) = match inner.split_once('|') {
            Some((text, target)) => (Some(text), target),
            None => (None, inner.as_str()),
        };
        if !is_link_target(target) {
            return None;
        }
        let link = Style {
            underline: true,
            ..base
        };
        match text.filter(|t| !t.is_empty() && *t != target) {
            Some(text) => {
                spans.extend(self.inline(text, link));
                push(spans, &format!(" <{target}>"), Style { dim: true, ..base });
            }
            None => push(spans, target, link),
        }
        Some(len)
    }
}

fn paint(spans: &[Span], color: bool) -> String {
    let mut s = String::new();
    for sp in spans {
        let codes = sgr(sp.style);
        if color && !codes.is_empty() {
            s.push_str(&format!("\x1b[{codes}m{}\x1b[0m", sp.text));
        } else {
            s.push_str(&sp.text);
        }
    }
    s
}

fn sgr(st: Style) -> String {
    let mut codes = Vec::new();
    for (on, code) in [
        (st.bold, "1"),
        (st.dim, "2"),
        (st.italic, "3"),
        (st.underline, "4"),
        (st.strike, "9"),
        (st.mono, "36"),
    ] {
        if on {
            codes.push(code);
        }
    }
    codes.join(";")
}

fn span(text: String, style: Style) -> Span {
    Span { text, style }
}

fn bold() -> Style {
    Style {
        bold: true,
        ..Style::default()
    }
}

/// Cut `text` into pieces of at most `width` columns.
fn chunks(text: &str, width: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut cur_w = 0;
    for ch in text.chars() {
        let w = ch.to_string().width();
        if cur_w + w > width && cur_w > 0 {
            out.push(String::new());
            cur_w = 0;
        }
        out.last_mut().expect("non-empty").push(ch);
        cur_w += w;
    }
    out
}

fn dim() -> Style {
    Style {
        dim: true,
        ..Style::default()
    }
}

fn blank(out: &mut Vec<Line>) {
    if out.last().is_some_and(|l| !l.is_empty()) {
        out.push(Vec::new());
    }
}

/// Append text, merging with the previous span when the style matches.
fn push(spans: &mut Vec<Span>, text: &str, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => spans.push(span(text.to_string(), style)),
    }
}

/// Drop the backslash of `\x` escapes.
fn unescape(chars: &[char]) -> String {
    let mut s = String::new();
    let mut it = chars.iter().peekable();
    while let Some(&c) = it.next() {
        if c == '\\' && it.peek().is_some_and(|n| !n.is_alphanumeric()) {
            continue;
        }
        s.push(c);
    }
    s
}

fn starts_with(chars: &[char], prefix: &str) -> bool {
    let mut it = chars.iter();
    prefix.chars().all(|p| it.next() == Some(&p))
}

/// Index of the first occurrence of `pat` at or after `from`.
fn find(chars: &[char], from: usize, pat: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(pat.len() - 1)).find(|&i| chars[i..i + pat.len()] == *pat)
}

fn emphasis(c: char, base: Style) -> Option<Style> {
    match c {
        '*' => Some(Style { bold: true, ..base }),
        '_' => Some(Style {
            italic: true,
            ..base
        }),
        '-' => Some(Style {
            strike: true,
            ..base
        }),
        '+' => Some(Style {
            underline: true,
            ..base
        }),
        _ => None,
    }
}

/// Closing emphasis marker: preceded by a non-space, not followed by a word
/// character, so `snake_case` and `a-b-c` in paths stay literal.
fn closing(chars: &[char], from: usize, marker: char) -> Option<usize> {
    (from + 1..chars.len()).find(|&i| {
        chars[i] == marker
            && !chars[i - 1].is_whitespace()
            && chars
                .get(i + 1)
                .is_none_or(|n| !n.is_alphanumeric() && *n != marker)
    })
}

fn is_link_target(t: &str) -> bool {
    t.contains("://")
        || t.starts_with("mailto:")
        || t.starts_with('^')
        || t.starts_with('#')
        || is_issue_key(t)
}

fn is_issue_key(t: &str) -> bool {
    t.split_once('-').is_some_and(|(p, n)| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && p.starts_with(|c: char| c.is_ascii_uppercase())
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
    })
}

/// `{name}` or `{name:params}` at line start; returns the name and the text
/// after the tag.
fn macro_open<'a>(
    line: &'a str,
    names: &[&'static str],
) -> Option<(&'static str, &'a str, &'a str)> {
    let tag = line.strip_prefix('{')?;
    let name = names.iter().find(|n| {
        tag.strip_prefix(**n)
            .is_some_and(|r| r.starts_with('}') || r.starts_with(':'))
    })?;
    let close = line.find('}')?;
    let params = line[..close].split_once(':').map_or("", |(_, p)| p);
    Some((name, params, &line[close + 1..]))
}

/// `title=x` from macro parameters (`{panel:title=x|borderStyle=dashed}`).
fn macro_title(params: &str) -> Option<&str> {
    params
        .split('|')
        .find_map(|kv| kv.strip_prefix("title="))
        .filter(|t| !t.is_empty())
}

/// Collect a macro body up to the closing `{name}`. `first` is the text after
/// the opening tag. Leaves `i` on the line after the closing tag; returns the
/// body lines and any text after the closing tag.
fn collect_macro(
    lines: &[&str],
    i: &mut usize,
    first: &str,
    name: &str,
) -> (Vec<String>, Option<String>) {
    let close = format!("{{{name}}}");
    let mut body = Vec::new();
    let mut cur = first.to_string();
    let mut on_first = true;
    loop {
        if let Some(pos) = cur.find(&close) {
            let before = &cur[..pos];
            if !before.trim().is_empty() {
                body.push(before.to_string());
            }
            *i += 1;
            return (body, Some(cur[pos + close.len()..].to_string()));
        }
        if !(on_first && cur.trim().is_empty()) {
            body.push(cur);
        }
        on_first = false;
        *i += 1;
        if *i >= lines.len() {
            return (body, None);
        }
        cur = lines[*i].to_string();
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let b = line.as_bytes();
    if b.len() >= 4 && b[0] == b'h' && (b'1'..=b'6').contains(&b[1]) && b[2] == b'.' && b[3] == b' '
    {
        Some(((b[1] - b'0') as usize, line[4..].trim()))
    } else {
        None
    }
}

/// `*`, `#`, `-` list markers (nesting by repetition, `*#` mixes allowed).
fn list_marker(line: &str) -> Option<(&str, &str)> {
    let end = line.find(|c: char| !"*#-".contains(c))?;
    let marker = &line[..end];
    if marker.is_empty() || !line[end..].starts_with(' ') {
        return None;
    }
    if marker.contains('-') && marker != "-" {
        return None;
    }
    Some((marker, line[end..].trim()))
}

/// Split a table row into (is_header, text) cells. `|` inside `[...]` and
/// `{{...}}` and escaped `\|` do not split.
fn split_row(row: &str) -> Vec<(bool, String)> {
    let chars: Vec<char> = row.chars().collect();
    let mut cells = Vec::new();
    let mut cur: Option<(bool, String)> = None;
    let mut p = 0;
    while p < chars.len() {
        let c = chars[p];
        let next = chars.get(p + 1).copied();
        if c == '\\' && next == Some('|') {
            if let Some((_, t)) = cur.as_mut() {
                t.push_str("\\|");
            }
            p += 2;
            continue;
        }
        if c == '|' {
            if let Some(cell) = cur.take() {
                cells.push(cell);
            }
            let header = next == Some('|');
            cur = Some((header, String::new()));
            p += if header { 2 } else { 1 };
            continue;
        }
        // a `[...]` link or `{{...}}` span shields its pipes, but only when it
        // is closed: a stray `[` must not swallow the rest of the row
        let close = match (c, next) {
            ('[', _) => find(&chars, p + 1, &[']']).map(|e| e + 1),
            ('{', Some('{')) => find(&chars, p + 2, &['}', '}']).map(|e| e + 2),
            _ => None,
        };
        let end = close.unwrap_or(p + 1);
        if let Some((_, t)) = cur.as_mut() {
            t.extend(&chars[p..end]);
        }
        p = end;
    }
    if let Some(cell) = cur.filter(|(_, t)| !t.trim().is_empty()) {
        cells.push(cell);
    }
    cells
}

fn line_width(line: &[Span]) -> usize {
    line.iter().map(|s| s.text.width()).sum()
}

/// Wrap for table cells: words longer than the column are broken, so the
/// borders stay aligned.
fn wrap_hard(spans: &[Span], width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for line in wrap(spans, width) {
        if line_width(&line) <= width {
            out.push(line);
            continue;
        }
        // an over-long line holds a single word, see wrap()
        let mut cur: Line = Vec::new();
        let mut cur_w = 0;
        for sp in &line {
            for ch in sp.text.chars() {
                let w = ch.to_string().width();
                if cur_w + w > width && cur_w > 0 {
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                push(&mut cur, &ch.to_string(), sp.style);
                cur_w += w;
            }
        }
        out.push(cur);
    }
    out
}

/// Greedy word wrap. Words longer than `width` (URLs) get a line of their own.
fn wrap(spans: &[Span], width: usize) -> Vec<Line> {
    let width = width.max(1);
    let mut lines: Vec<Line> = vec![Vec::new()];
    let mut cur_w = 0;
    let mut word: Line = Vec::new();
    let mut space = false;

    let mut prev: Option<Style> = None;
    let flush = |lines: &mut Vec<Line>,
                 word: &mut Line,
                 cur_w: &mut usize,
                 space: &mut bool,
                 prev: &mut Option<Style>| {
        if word.is_empty() {
            return;
        }
        let w = line_width(word);
        if *cur_w > 0 && *cur_w + usize::from(*space) + w > width {
            lines.push(Vec::new());
            *cur_w = 0;
        } else if *cur_w > 0 && *space {
            // the gap inside a styled run carries the style (underline, strike)
            let style = match (*prev, word.first()) {
                (Some(a), Some(b)) if a == b.style => a,
                _ => Style::default(),
            };
            push(lines.last_mut().expect("non-empty"), " ", style);
            *cur_w += 1;
        }
        let line = lines.last_mut().expect("non-empty");
        *prev = word.last().map(|sp| sp.style);
        for sp in word.drain(..) {
            push(line, &sp.text, sp.style);
        }
        *cur_w += w;
        *space = false;
    };

    for sp in spans {
        for ch in sp.text.chars() {
            match ch {
                '\n' => {
                    flush(&mut lines, &mut word, &mut cur_w, &mut space, &mut prev);
                    lines.push(Vec::new());
                    cur_w = 0;
                    space = false;
                    prev = None;
                }
                ' ' | '\t' => {
                    flush(&mut lines, &mut word, &mut cur_w, &mut space, &mut prev);
                    space = true;
                }
                _ => push(&mut word, &ch.to_string(), sp.style),
            }
        }
    }
    flush(&mut lines, &mut word, &mut cur_w, &mut space, &mut prev);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(src: &str) -> String {
        render(src, 60, false)
    }

    #[test]
    fn headings_and_paragraphs() {
        assert_eq!(plain("h2. Goal\r\n\r\nShip it."), "Goal\n----\n\nShip it.");
        assert_eq!(plain("h3. Notes"), "Notes");
    }

    #[test]
    fn nested_and_numbered_lists() {
        let out = plain("* one\n** sub\n* two\n# first\n# second\n## inner");
        assert_eq!(
            out,
            "• one\n  ◦ sub\n• two\n1. first\n2. second\n  1. inner"
        );
    }

    #[test]
    fn bold_line_is_not_a_list() {
        assert_eq!(plain("*Defect:* broken"), "Defect: broken");
    }

    #[test]
    fn list_items_wrap_with_hanging_indent() {
        let out = render("* aaaa bbbb cccc dddd eeee ffff", 20, false);
        assert_eq!(out, "• aaaa bbbb cccc\n  dddd eeee ffff");
    }

    #[test]
    fn table_with_header() {
        let out = plain("||request||count||\n|{{GET /a}}|0 2|\n|b|22|");
        let expected = "\
┌──────────┬───────┐
│ request  │ count │
├──────────┼───────┤
│ `GET /a` │ 0 2   │
│ b        │ 22    │
└──────────┴───────┘";
        assert_eq!(out, expected);
    }

    #[test]
    fn wide_table_wraps_cells() {
        let src = "|aaaa bbbb cccc dddd eeee ffff gggg|x|\n|{{a_very_long_identifier_name}}|y|";
        let out = render(src, 24, false);
        for line in out.lines() {
            assert_eq!(line.width(), 24, "misaligned: {line}");
        }
        assert!(out.lines().count() > 3);
    }

    #[test]
    fn pipes_inside_links_do_not_split_cells() {
        let cells = split_row("|[text|https://x.y]|{{a|b}}|");
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn code_block_keeps_text_and_strips_indent() {
        let out = plain("  {code}\n  ERROR: *not bold*\n  {code}\nafter");
        assert_eq!(out, "  ERROR: *not bold*\n\nafter");
    }

    #[test]
    fn code_on_opener_line() {
        let out = plain("{code}a  b\nc{code}");
        assert_eq!(out, "  a  b\n  c");
    }

    #[test]
    fn links_mentions_images() {
        assert_eq!(
            plain("[docs|https://x.org/a] by [~jdoe] !shot.png!"),
            "docs <https://x.org/a> by @jdoe [image: shot.png]"
        );
        assert_eq!(plain("[change|exit|never]"), "[change|exit|never]");
    }

    #[test]
    fn dashes_and_underscores_in_words_stay_literal() {
        let src = "see https://git.x/test-channel-v0-a and snake_case_name and a-b-c";
        assert_eq!(render(src, 200, false), src);
        assert_eq!(
            plain("https://g.x/-/merge\\_requests/27"),
            "https://g.x/-/merge_requests/27"
        );
    }

    #[test]
    fn emphasis_with_color() {
        let out = render("*b* _i_ -s- {{m}}", 60, true);
        assert_eq!(
            out,
            "\x1b[1mb\x1b[0m \x1b[3mi\x1b[0m \x1b[9ms\x1b[0m \x1b[36mm\x1b[0m"
        );
    }

    #[test]
    fn quote_block() {
        assert_eq!(plain("{quote}\nsaid\n{quote}"), "│ said");
        assert_eq!(plain("bq. said"), "│ said");
    }

    #[test]
    fn forced_line_break_and_escape() {
        assert_eq!(plain("a\\\\b \\*c\\*"), "a\nb *c*");
    }

    #[test]
    fn literal_text_survives() {
        assert_eq!(plain("C:\\Users\\foo and \\d+"), "C:\\Users\\foo and \\d+");
        assert_eq!(plain("-5 and 2- ok"), "-5 and 2- ok");
        assert_eq!(plain("{colorful} {anchor_x}"), "{colorful} {anchor_x}");
        assert_eq!(plain("{color:red}x{color} {anchor:a}y"), "x y");
    }

    #[test]
    fn stray_bracket_does_not_swallow_cells() {
        assert_eq!(split_row("|a[0|b|c|").len(), 3);
        assert_eq!(split_row("|{{x|y|z|").len(), 3);
    }

    #[test]
    fn macro_titles_are_kept() {
        assert_eq!(
            plain("{panel:title=Rollback plan|borderStyle=dashed}\nbody\n{panel}"),
            "│ Rollback plan\n│ body"
        );
        assert_eq!(
            plain("{code:java|title=A.java}\nx\n{code}"),
            "A.java\n\n  x"
        );
    }

    #[test]
    fn long_code_lines_and_wide_tables_fit() {
        let out = render(
            "{code}\nabcdefghijklmnopqrstuvwxyz0123456789\n{code}",
            20,
            false,
        );
        assert!(out.lines().all(|l| l.width() <= 20), "{out}");
        let row = format!("|{}", "a|".repeat(12));
        let out = render(&row, 60, false);
        assert!(out.lines().all(|l| l.width() <= 60), "{out}");
    }

    #[test]
    fn styled_gaps_keep_the_style() {
        let line = &render_lines("-a b-", 40, true)[0];
        assert_eq!(line.len(), 1);
        assert_eq!(line[0].text, "a b");
    }

    #[test]
    fn rule() {
        assert_eq!(render("----", 20, false), "─".repeat(20));
    }
}
