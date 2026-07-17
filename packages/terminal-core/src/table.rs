// Terminal Core module implements table behavior.
// 翻译自 packages/terminal-core/src/table.ts

use crate::ansi::{split_graphemes, truncate_to_visible_width, visible_width};
use crate::display_string::display_string;

#[derive(Clone, Copy, Debug)]
pub enum Align {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone)]
pub struct TableColumn {
    pub key: String,
    pub header: String,
    pub align: Option<Align>,
    pub min_width: Option<usize>,
    pub max_width: Option<usize>,
    pub flex: bool,
}

#[derive(Debug, Clone)]
pub struct RenderTableOptions {
    pub columns: Vec<TableColumn>,
    pub rows: Vec<std::collections::BTreeMap<String, String>>,
    pub width: Option<usize>,
    pub padding: Option<usize>,
    pub border: Option<BorderKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderKind {
    Unicode,
    Ascii,
    None,
}

fn resolve_default_border(
    platform: &str,
    env: &std::collections::HashMap<String, String>,
) -> BorderKind {
    if platform != "win32" {
        return BorderKind::Unicode;
    }
    let term = env.get("TERM").cloned().unwrap_or_default();
    let term_program = env.get("TERM_PROGRAM").cloned().unwrap_or_default();
    let is_modern = env.contains_key("WT_SESSION")
        || term.contains("xterm")
        || term.contains("cygwin")
        || term.contains("msys")
        || term_program == "vscode";
    if is_modern {
        BorderKind::Unicode
    } else {
        BorderKind::Ascii
    }
}

fn repeat(ch: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    ch.repeat(n)
}

fn pad_cell(text: &str, width: usize, align: Align) -> String {
    let content = if visible_width(text) > width {
        truncate_to_visible_width(text, width)
    } else {
        text.to_string()
    };
    let w = visible_width(&content);
    if w >= width {
        return content;
    }
    let pad = width - w;
    match align {
        Align::Right => format!("{}{}", repeat(" ", pad), content),
        Align::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", repeat(" ", left), content, repeat(" ", right))
        }
        Align::Left => format!("{}{}", content, repeat(" ", pad)),
    }
}

#[derive(Clone, Debug)]
enum Token {
    Ansi(String),
    Char(String),
}

fn tokenize_for_wrap(text: &str) -> Vec<Token> {
    let esc = "\u{001B}";
    let mut tokens: Vec<Token> = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // SGR
                let mut j = i + 2;
                while j < bytes.len() {
                    let ch = bytes[j];
                    if ch == b'm' {
                        break;
                    }
                    if (ch as char).is_ascii_digit() {
                        j += 1;
                        continue;
                    }
                    if ch == b';' {
                        j += 1;
                        continue;
                    }
                    break;
                }
                if j < bytes.len() && bytes[j] == b'm' {
                    tokens.push(Token::Ansi(text[i..=j].to_string()));
                    i = j + 1;
                    continue;
                }
            }
            if i + 4 < bytes.len()
                && bytes[i + 1] == b']'
                && text.get(i + 2..i + 5) == Some("8;;")
            {
                let st_marker = format!("{}\\", esc);
                if let Some(st_offset) = text[i + 5..].find(&st_marker) {
                    let abs_st = i + 5 + st_offset;
                    tokens.push(Token::Ansi(text[i..abs_st + 2].to_string()));
                    i = abs_st + 2;
                    continue;
                }
            }
        }
        let next_esc = text[i..].find(esc).map(|off| i + off).unwrap_or(bytes.len());
        if next_esc == i {
            tokens.push(Token::Char(esc.to_string()));
            i += esc.len();
            continue;
        }
        let plain = &text[i..next_esc];
        for g in split_graphemes(plain) {
            tokens.push(Token::Char(g));
        }
        i = next_esc;
        let _ = chars;
    }
    tokens
}

fn parse_sgr_params(value: &str) -> Option<Vec<i32>> {
    let esc = "\u{001B}";
    if !value.starts_with(&format!("{}[", esc)) || !value.ends_with('m') {
        return None;
    }
    let raw = &value[2..value.len() - 1];
    if raw.is_empty() {
        return Some(vec![0]);
    }
    let params: Vec<i32> = raw
        .split(';')
        .map(|part| if part.is_empty() { 0 } else { part.parse::<i32>().unwrap_or(0) })
        .collect();
    Some(params)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SgrCategory {
    Foreground,
    Background,
    Intensity,
    Italic,
    Underline,
    Blink,
    Inverse,
    Conceal,
    Strike,
}

fn reset_categories_for(params: &[i32]) -> std::collections::HashSet<SgrCategory> {
    let mut cats = std::collections::HashSet::new();
    for param in params {
        match param {
            22 => {
                cats.insert(SgrCategory::Intensity);
            }
            23 => {
                cats.insert(SgrCategory::Italic);
            }
            24 => {
                cats.insert(SgrCategory::Underline);
            }
            25 => {
                cats.insert(SgrCategory::Blink);
            }
            27 => {
                cats.insert(SgrCategory::Inverse);
            }
            28 => {
                cats.insert(SgrCategory::Conceal);
            }
            29 => {
                cats.insert(SgrCategory::Strike);
            }
            39 => {
                cats.insert(SgrCategory::Foreground);
            }
            49 => {
                cats.insert(SgrCategory::Background);
            }
            _ => {}
        }
    }
    cats
}

fn active_categories_for(params: &[i32]) -> (std::collections::HashSet<SgrCategory>, usize) {
    let mut cats = std::collections::HashSet::new();
    let mut consumed = 0;
    let mut i = 0;
    while i < params.len() {
        let param = params[i];
        match param {
            1 | 2 => {
                cats.insert(SgrCategory::Intensity);
            }
            3 => {
                cats.insert(SgrCategory::Italic);
            }
            4 => {
                cats.insert(SgrCategory::Underline);
            }
            5 | 6 => {
                cats.insert(SgrCategory::Blink);
            }
            7 => {
                cats.insert(SgrCategory::Inverse);
            }
            8 => {
                cats.insert(SgrCategory::Conceal);
            }
            9 => {
                cats.insert(SgrCategory::Strike);
            }
            30..=37 | 90..=97 => {
                cats.insert(SgrCategory::Foreground);
            }
            38 => {
                cats.insert(SgrCategory::Foreground);
                if i + 1 < params.len() && params[i + 1] == 2 && i + 4 < params.len() {
                    i += 4;
                    consumed += 4;
                } else if i + 1 < params.len() && params[i + 1] == 5 && i + 2 < params.len() {
                    i += 2;
                    consumed += 2;
                }
            }
            40..=47 | 100..=107 => {
                cats.insert(SgrCategory::Background);
            }
            48 => {
                cats.insert(SgrCategory::Background);
                if i + 1 < params.len() && params[i + 1] == 2 && i + 4 < params.len() {
                    i += 4;
                    consumed += 4;
                } else if i + 1 < params.len() && params[i + 1] == 5 && i + 2 < params.len() {
                    i += 2;
                    consumed += 2;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let _ = consumed;
    (cats, 0)
}

fn intersects(left: &std::collections::HashSet<SgrCategory>, right: &std::collections::HashSet<SgrCategory>) -> bool {
    for v in left {
        if right.contains(v) {
            return true;
        }
    }
    false
}

fn active_sgr_after(tokens: &[Token]) -> String {
    #[derive(Clone)]
    struct Entry {
        value: String,
        categories: std::collections::HashSet<SgrCategory>,
    }
    let mut active: Vec<Entry> = Vec::new();
    for token in tokens {
        if let Token::Ansi(value) = token {
            let parsed = match parse_sgr_params(value) {
                Some(p) => p,
                None => continue,
            };
            if parsed.contains(&0) {
                active.clear();
            }
            let reset_categories = reset_categories_for(&parsed);
            if !reset_categories.is_empty() {
                let mut i = active.len();
                while i > 0 {
                    i -= 1;
                    let entry = &active[i];
                    if intersects(&entry.categories, &reset_categories) {
                        active.remove(i);
                    }
                }
            }
            let (active_categories, _) = active_categories_for(&parsed);
            if !active_categories.is_empty() {
                let mut i = active.len();
                while i > 0 {
                    i -= 1;
                    let entry = &active[i];
                    if intersects(&entry.categories, &active_categories) {
                        active.remove(i);
                    }
                }
                active.push(Entry {
                    value: value.clone(),
                    categories: active_categories,
                });
            }
        }
    }
    let mut out = String::new();
    for e in active {
        out.push_str(&e.value);
    }
    out
}

fn trim_trailing_ws(s: &str) -> &str {
    let trimmed = s.trim_end();
    trimmed
}

fn push_line(lines: &mut Vec<String>, value: String) {
    let cleaned = trim_trailing_ws(&value);
    if visible_width(cleaned) == 0 {
        return;
    }
    lines.push(cleaned.to_string());
}

fn is_break_char(ch: &str) -> bool {
    matches!(ch, " " | "\t" | "/" | "-" | "_" | ".")
}

fn is_space_char(ch: &str) -> bool {
    matches!(ch, " " | "\t")
}

fn buf_to_string(buf: &[Token]) -> String {
    buf.iter()
        .map(|t| match t {
            Token::Ansi(s) => s.clone(),
            Token::Char(s) => s.clone(),
        })
        .collect()
}

fn buf_visible_width(buf: &[Token]) -> usize {
    buf.iter()
        .map(|t| match t {
            Token::Char(s) => visible_width(s),
            Token::Ansi(_) => 0,
        })
        .sum()
}

fn trim_leading_spaces(buf: &mut Vec<Token>) {
    loop {
        let first_char_index = buf.iter().position(|t| matches!(t, Token::Char(_)));
        let Some(pos) = first_char_index else {
            return;
        };
        let is_space = matches!(&buf[pos], Token::Char(s) if is_space_char(s));
        if !is_space {
            return;
        }
        buf.remove(pos);
    }
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let tokens = tokenize_for_wrap(text);
    let first_char_index = tokens.iter().position(|t| matches!(t, Token::Char(_)));
    let Some(first_char_index) = first_char_index else {
        return vec![text.to_string()];
    };
    let mut last_char_index = tokens.len();
    for (i, t) in tokens.iter().enumerate().rev() {
        if matches!(t, Token::Char(_)) {
            last_char_index = i + 1;
            break;
        }
    }
    let prefix_ansi: String = tokens[..first_char_index]
        .iter()
        .filter_map(|t| match t {
            Token::Ansi(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let suffix_ansi: String = tokens[last_char_index..]
        .iter()
        .filter_map(|t| match t {
            Token::Ansi(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let core_tokens = &tokens[first_char_index..last_char_index];

    let mut lines: Vec<String> = Vec::new();
    let mut skip_next_lf = false;
    let mut buf: Vec<Token> = Vec::new();
    let mut buf_visible: usize = 0;
    let mut last_break_index: Option<usize> = None;

    let flush_at = |break_at: Option<usize>, buf: &mut Vec<Token>, buf_visible: &mut usize, last_break_index: &mut Option<usize>, lines: &mut Vec<String>| {
        if buf.is_empty() {
            return;
        }
        match break_at {
            None | Some(0) => {
                let active = active_sgr_after(buf);
                let body = buf_to_string(buf);
                if !active.is_empty() {
                    push_line(lines, format!("{}{}\u{001B}[0m", body, active));
                } else {
                    push_line(lines, body);
                }
                buf.clear();
                if !active.is_empty() {
                    buf.push(Token::Ansi(active));
                }
                *buf_visible = 0;
                *last_break_index = None;
            }
            Some(break_at) => {
                let left: Vec<Token> = buf[..break_at].to_vec();
                let mut rest: Vec<Token> = buf[break_at..].to_vec();
                let active = active_sgr_after(&left);
                let body = buf_to_string(&left);
                if !active.is_empty() {
                    push_line(lines, format!("{}{}\u{001B}[0m", body, active));
                } else {
                    push_line(lines, body);
                }
                trim_leading_spaces(&mut rest);
                if !active.is_empty() {
                    rest.insert(0, Token::Ansi(active));
                }
                buf.clear();
                buf.extend(rest);
                *buf_visible = buf_visible_width(buf);
                *last_break_index = None;
            }
        }
    };

    for token in core_tokens {
        if let Token::Ansi(s) = token {
            buf.push(Token::Ansi(s.clone()));
            continue;
        }
        let ch_owned = match token {
            Token::Char(s) => s.clone(),
            _ => continue,
        };
        let ch_str = ch_owned.clone();
        if skip_next_lf {
            skip_next_lf = false;
            if ch_str == "\n" {
                continue;
            }
        }
        if ch_str == "\n" || ch_str == "\r" {
            flush_at(Some(buf.len()), &mut buf, &mut buf_visible, &mut last_break_index, &mut lines);
            if ch_str == "\r" {
                skip_next_lf = true;
            }
            continue;
        }
        let char_width = visible_width(&ch_str);
        if buf_visible + char_width > width && buf_visible > 0 {
            flush_at(last_break_index, &mut buf, &mut buf_visible, &mut last_break_index, &mut lines);
        }
        if buf_visible == 0 && is_space_char(&ch_str) {
            continue;
        }
        buf.push(Token::Char(ch_str));
        buf_visible += char_width;
        if is_break_char(&ch_owned) {
            last_break_index = Some(buf.len());
        }
    }

    flush_at(Some(buf.len()), &mut buf, &mut buf_visible, &mut last_break_index, &mut lines);
    if lines.is_empty() {
        return vec![String::new()];
    }
    if prefix_ansi.is_empty() && suffix_ansi.is_empty() {
        return lines;
    }
    lines
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                line
            } else {
                format!("{}{}{}", prefix_ansi, line, suffix_ansi)
            }
        })
        .collect()
}

fn normalize_width(n: Option<usize>) -> Option<usize> {
    n?;
    let v = n.unwrap();
    if v == 0 {
        None
    } else {
        Some(v)
    }
}

pub fn get_terminal_table_width(min_width: usize, fallback_width: usize) -> usize {
    use std::io::IsTerminal;
    let cols = if std::io::stdout().is_terminal() {
        // Best-effort terminal columns: ask the OS if we can; else fall back.
        // `terminal_size` crate would do better; we keep this self-contained.
        None
    } else {
        None
    };
    let cols = cols.unwrap_or(fallback_width);
    std::cmp::max(min_width, cols)
}

#[derive(Debug, Clone, Copy)]
struct BoxChars {
    tl: &'static str,
    tr: &'static str,
    bl: &'static str,
    br: &'static str,
    h: &'static str,
    v: &'static str,
    t: &'static str,
    ml: &'static str,
    m: &'static str,
    mr: &'static str,
    b: &'static str,
}

fn box_chars(border: BorderKind) -> BoxChars {
    match border {
        BorderKind::Ascii => BoxChars {
            tl: "+",
            tr: "+",
            bl: "+",
            br: "+",
            h: "-",
            v: "|",
            t: "+",
            ml: "+",
            m: "+",
            mr: "+",
            b: "+",
        },
        BorderKind::Unicode | BorderKind::None => BoxChars {
            tl: "┌",
            tr: "┐",
            bl: "└",
            br: "┘",
            h: "─",
            v: "│",
            t: "┬",
            ml: "├",
            m: "┼",
            mr: "┤",
            b: "┴",
        },
    }
}

pub fn render_table(opts: RenderTableOptions) -> String {
    let rows: Vec<std::collections::BTreeMap<String, String>> = opts
        .rows
        .into_iter()
        .map(|row| {
            let mut next = std::collections::BTreeMap::new();
            for (k, v) in row {
                next.insert(k, display_string(&v));
            }
            next
        })
        .collect();

    let border = opts
        .border
        .unwrap_or_else(|| resolve_default_border(std::env::consts::OS, &std::env::vars().collect()));

    if border == BorderKind::None {
        let mut lines: Vec<String> = Vec::new();
        let headers: Vec<String> = opts.columns.iter().map(|c| c.header.clone()).collect();
        lines.push(headers.join(" | "));
        for row in &rows {
            let mut parts: Vec<String> = Vec::new();
            for c in &opts.columns {
                parts.push(row.get(&c.key).cloned().unwrap_or_default());
            }
            lines.push(parts.join(" | "));
        }
        return format!("{}\n", lines.join("\n"));
    }

    let padding = opts.padding.unwrap_or(1);
    let columns = opts.columns;

    let metrics: Vec<(usize, usize)> = columns
        .iter()
        .map(|c| {
            let header_w = visible_width(&c.header);
            let cell_w = rows
                .iter()
                .map(|r| visible_width(r.get(&c.key).map(|s| s.as_str()).unwrap_or("")))
                .max()
                .unwrap_or(0);
            (header_w, cell_w)
        })
        .collect();

    let mut widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (header_w, cell_w) = metrics[i];
            let base = std::cmp::max(header_w, cell_w) + padding * 2;
            let capped = c.max_width.map(|m| std::cmp::min(base, m)).unwrap_or(base);
            std::cmp::max(c.min_width.unwrap_or(3), capped)
        })
        .collect();

    let max_width = normalize_width(opts.width);
    let sep_count = columns.len() + 1;
    let total: usize = widths.iter().sum::<usize>() + sep_count;

    let preferred_min_widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (header_w, _) = metrics[i];
            std::cmp::max(
                c.min_width.unwrap_or(3),
                std::cmp::max(header_w + padding * 2, 3),
            )
        })
        .collect();
    let absolute_min_widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, _c)| {
            let (header_w, _) = metrics[i];
            std::cmp::max(header_w + padding * 2, 3)
        })
        .collect();

    if let Some(max_width) = max_width {
        if total > max_width {
            let mut over = total - max_width;
            let flex_order: Vec<usize> = {
                let mut v: Vec<(usize, usize)> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, _c)| (i, widths[i]))
                    .filter(|(i, _)| columns[*i].flex)
                    .collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                v.into_iter().map(|(i, _)| i).collect()
            };
            let non_flex_order: Vec<usize> = {
                let mut v: Vec<(usize, usize)> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, _c)| (i, widths[i]))
                    .filter(|(i, _)| !columns[*i].flex)
                    .collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                v.into_iter().map(|(i, _)| i).collect()
            };

            let shrink = |order: &[usize], min_widths: &[usize], widths: &mut Vec<usize>, over: &mut usize| {
                while *over > 0 {
                    let mut progressed = false;
                    for &i in order {
                        if widths[i] <= min_widths[i] {
                            continue;
                        }
                        widths[i] -= 1;
                        *over -= 1;
                        progressed = true;
                        if *over <= 0 {
                            break;
                        }
                    }
                    if !progressed {
                        break;
                    }
                }
            };

            shrink(&flex_order, &preferred_min_widths, &mut widths, &mut over);
            shrink(&flex_order, &absolute_min_widths, &mut widths, &mut over);
            shrink(&non_flex_order, &preferred_min_widths, &mut widths, &mut over);
            shrink(&non_flex_order, &absolute_min_widths, &mut widths, &mut over);
        }
    }

    if let Some(max_width) = max_width {
        let sep_count_local = columns.len() + 1;
        let current_total: usize = widths.iter().sum::<usize>() + sep_count_local;
        let mut extra = max_width as isize - current_total as isize;
        if extra > 0 {
            let flex_cols: Vec<usize> = columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.flex)
                .map(|(i, _)| i)
                .collect();
            if !flex_cols.is_empty() {
                let caps: Vec<usize> = columns
                    .iter()
                    .map(|c| {
                        if let Some(m) = c.max_width {
                            if m > 0 {
                                return m;
                            }
                        }
                        usize::MAX
                    })
                    .collect();
                while extra > 0 {
                    let mut progressed = false;
                    for &i in &flex_cols {
                        if widths[i] >= caps[i] {
                            continue;
                        }
                        widths[i] += 1;
                        extra -= 1;
                        progressed = true;
                        if extra <= 0 {
                            break;
                        }
                    }
                    if !progressed {
                        break;
                    }
                }
            }
        }
    }

    let box_ch = box_chars(border);
    let h_line = |left: &str, mid: &str, right: &str| -> String {
        let mut s = String::new();
        s.push_str(left);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&repeat(box_ch.h, *w));
            if i + 1 < widths.len() {
                s.push_str(mid);
            }
        }
        s.push_str(right);
        s
    };

    let content_width_for = |i: usize| -> usize {
        let width = widths[i];
        std::cmp::max(1, width - padding * 2)
    };
    let pad_str = repeat(" ", padding);

    let render_row = |record: &std::collections::BTreeMap<String, String>, is_header: bool| -> Vec<String> {
        let cells: Vec<String> = columns
            .iter()
            .map(|c| {
                if is_header {
                    c.header.clone()
                } else {
                    record.get(&c.key).cloned().unwrap_or_default()
                }
            })
            .collect();
        let wrapped: Vec<Vec<String>> = cells
            .iter()
            .enumerate()
            .map(|(i, cell)| wrap_line(cell, content_width_for(i)))
            .collect();
        let height = wrapped.iter().map(|w| w.len()).max().unwrap_or(0);
        let mut out: Vec<String> = Vec::new();
        for li in 0..height {
            let mut parts: Vec<String> = Vec::new();
            for (i, lines) in wrapped.iter().enumerate() {
                let raw = lines.get(li).cloned().unwrap_or_default();
                let align = columns.get(i).and_then(|c| c.align).unwrap_or(Align::Left);
                let aligned = pad_cell(&raw, content_width_for(i), align);
                parts.push(format!("{}{}{}", pad_str, aligned, pad_str));
            }
            out.push(format!("{}{}{}", box_ch.v, parts.join(box_ch.v), box_ch.v));
        }
        out
    };

    let empty_record = std::collections::BTreeMap::new();
    let mut lines: Vec<String> = Vec::new();
    lines.push(h_line(box_ch.tl, box_ch.t, box_ch.tr));
    lines.extend(render_row(&empty_record, true));
    lines.push(h_line(box_ch.ml, box_ch.m, box_ch.mr));
    for row in &rows {
        lines.extend(render_row(row, false));
    }
    lines.push(h_line(box_ch.bl, box_ch.b, box_ch.br));
    let _ = rows;
    format!("{}\n", lines.join("\n"))
}
