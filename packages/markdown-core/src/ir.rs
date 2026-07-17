// Markdown Core module implements ir behavior.
// 翻译自 packages/markdown-core/src/ir.ts
//
// ir.ts wraps the JavaScript `markdown-it` parser. Rust has no direct equivalent
// with the same token-based API, so this translation includes a minimal
// block-level + inline markdown tokenizer that emits the same logical tokens
// (paragraph_open/close, heading_open/close, em_open/close, strong_open/close,
//  s_open/close, code_inline, fence, code_block, bullet_list_open/close,
//  ordered_list_open/close, list_item_open/close, blockquote_open/close,
//  table_open/close, thead_open/close, tbody_open/close, tr_open/close,
//  th_open/td_open/th_close/td_close, link_open/close, softbreak, hardbreak,
//  hr, html_inline, html_block, text) consumed by the rest of the renderer.

use crate::chunk_text::chunk_text;
use crate::types::MarkdownTableMode;

#[derive(Debug, Clone)]
pub struct ListState {
    pub list_type: ListType,
    pub index: usize,
    pub open_level: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListType {
    Bullet,
    Ordered,
}

#[derive(Debug, Clone)]
pub struct LinkState {
    pub href: String,
    pub label_start: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownStyle {
    Bold,
    Italic,
    Strikethrough,
    Code,
    CodeBlock,
    Spoiler,
    Blockquote,
    Heading1,
    Heading2,
    Heading3,
    Heading4,
    Heading5,
    Heading6,
}

impl MarkdownStyle {
    pub fn as_str(&self) -> &'static str {
        match self {
            MarkdownStyle::Bold => "bold",
            MarkdownStyle::Italic => "italic",
            MarkdownStyle::Strikethrough => "strikethrough",
            MarkdownStyle::Code => "code",
            MarkdownStyle::CodeBlock => "code_block",
            MarkdownStyle::Spoiler => "spoiler",
            MarkdownStyle::Blockquote => "blockquote",
            MarkdownStyle::Heading1 => "heading_1",
            MarkdownStyle::Heading2 => "heading_2",
            MarkdownStyle::Heading3 => "heading_3",
            MarkdownStyle::Heading4 => "heading_4",
            MarkdownStyle::Heading5 => "heading_5",
            MarkdownStyle::Heading6 => "heading_6",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MarkdownStyleSpan {
    pub start: usize,
    pub end: usize,
    pub style: MarkdownStyle,
    pub language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MarkdownLinkSpan {
    pub start: usize,
    pub end: usize,
    pub href: String,
}

#[derive(Debug, Clone, Default)]
pub struct MarkdownIR {
    pub text: String,
    pub styles: Vec<MarkdownStyleSpan>,
    pub links: Vec<MarkdownLinkSpan>,
}

fn create_style_span(params: MarkdownStyleSpan) -> MarkdownStyleSpan {
    let mut span = MarkdownStyleSpan {
        start: params.start,
        end: params.end,
        style: params.style,
        language: None,
    };
    if let Some(lang) = params.language {
        span.language = Some(lang);
    }
    span
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkdownTableAlignment {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Default)]
pub struct MarkdownTableData {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub aligns: Vec<Option<MarkdownTableAlignment>>,
}

#[derive(Debug, Clone, Default)]
pub struct MarkdownTableCell {
    pub text: String,
    pub styles: Vec<MarkdownStyleSpan>,
    pub links: Vec<MarkdownLinkSpan>,
}

#[derive(Debug, Clone, Default)]
pub struct MarkdownTableMeta {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub aligns: Vec<Option<MarkdownTableAlignment>>,
    pub placeholder_offset: usize,
    pub header_cells: Vec<MarkdownTableCell>,
    pub row_cells: Vec<Vec<MarkdownTableCell>>,
}

#[derive(Debug, Clone)]
pub struct OpenStyle {
    pub style: MarkdownStyle,
    pub start: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RenderTarget {
    pub text: String,
    pub styles: Vec<MarkdownStyleSpan>,
    pub open_styles: Vec<OpenStyle>,
    pub links: Vec<MarkdownLinkSpan>,
    pub link_stack: Vec<LinkState>,
}

type TableCell = MarkdownTableCell;

#[derive(Debug, Clone, Default)]
pub struct TableState {
    pub headers: Vec<TableCell>,
    pub rows: Vec<Vec<TableCell>>,
    pub aligns: Vec<Option<MarkdownTableAlignment>>,
    pub current_row: Vec<TableCell>,
    pub current_cell: Option<RenderTarget>,
    pub in_header: bool,
}

#[derive(Debug, Clone)]
pub enum HeadingStyle {
    None,
    Bold,
    Rich,
}

#[derive(Debug, Clone)]
pub struct RenderState {
    pub target: RenderTarget,
    pub list_stack: Vec<ListState>,
    pub heading_style: HeadingStyle,
    pub blockquote_prefix: String,
    pub enable_spoilers: bool,
    pub table_mode: MarkdownTableMode,
    pub table: Option<TableState>,
    pub has_tables: bool,
    pub collected_tables: Vec<MarkdownTableMeta>,
}

#[derive(Debug, Clone, Default)]
pub struct MarkdownParseOptions {
    pub linkify: Option<bool>,
    pub enable_spoilers: Option<bool>,
    pub heading_style: Option<HeadingStyle>,
    pub blockquote_prefix: Option<String>,
    pub autolink: Option<bool>,
    pub table_mode: Option<MarkdownTableMode>,
}

// ---------- Minimal markdown tokenizer (CommonMark-ish subset) ----------

#[derive(Debug, Clone)]
pub struct MarkdownToken {
    pub token_type: String,
    pub tag: Option<String>,
    pub content: Option<String>,
    pub info: Option<String>,
    pub children: Vec<MarkdownToken>,
    pub attrs: Vec<(String, String)>,
    pub level: usize,
    pub hidden: bool,
    pub markup: String,
}

impl MarkdownToken {
    fn new(token_type: &str) -> Self {
        MarkdownToken {
            token_type: token_type.to_string(),
            tag: None,
            content: None,
            info: None,
            children: Vec::new(),
            attrs: Vec::new(),
            level: 0,
            hidden: false,
            markup: String::new(),
        }
    }

    fn with_tag(mut self, tag: &str) -> Self {
        self.tag = Some(tag.to_string());
        self
    }

    fn with_content(mut self, content: &str) -> Self {
        self.content = Some(content.to_string());
        self
    }

    fn with_info(mut self, info: &str) -> Self {
        self.info = Some(info.to_string());
        self
    }

    fn with_attrs(mut self, attrs: Vec<(String, String)>) -> Self {
        self.attrs = attrs;
        self
    }

    fn with_level(mut self, level: usize) -> Self {
        self.level = level;
        self
    }

    fn with_children(mut self, children: Vec<MarkdownToken>) -> Self {
        self.children = children;
        self
    }

    fn with_markup(mut self, markup: &str) -> Self {
        self.markup = markup.to_string();
        self
    }

    fn attr_get(&self, name: &str) -> Option<String> {
        for (k, v) in &self.attrs {
            if k == name {
                return Some(v.clone());
            }
        }
        None
    }
}

fn tokenize_inline(text: &str) -> Vec<MarkdownToken> {
    let mut out: Vec<MarkdownToken> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut buf_start = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            // inline code
            if buf_start < i {
                out.push(
                    MarkdownToken::new("text").with_content(&text[buf_start..i]),
                );
            }
            let mut run = 0;
            while i < bytes.len() && bytes[i] == b'`' {
                run += 1;
                i += 1;
            }
            let mut end = i;
            let mut close_run = 0;
            while end < bytes.len() {
                if bytes[end] == b'`' {
                    close_run += 1;
                    end += 1;
                } else {
                    break;
                }
            }
            if close_run == run {
                let content = &text[i..end - run];
                out.push(
                    MarkdownToken::new("code_inline").with_content(content).with_markup("`"),
                );
                i = end;
                buf_start = i;
                continue;
            } else {
                // treat as literal backticks
                i = end;
                buf_start = i;
                continue;
            }
        } else if c == b'<' && i + 1 < bytes.len() {
            // autolink or raw html
            if let Some(close) = text[i + 1..].find('>') {
                let inner = &text[i + 1..i + 1 + close];
                if inner.starts_with("http://") || inner.starts_with("https://") {
                    if buf_start < i {
                        out.push(
                            MarkdownToken::new("text").with_content(&text[buf_start..i]),
                        );
                    }
                    let url = inner.to_string();
                    let label = url.clone();
                    out.push(
                        MarkdownToken::new("link_open")
                            .with_tag("a")
                            .with_attrs(vec![("href".to_string(), url.clone())])
                            .with_markup("autolink"),
                    );
                    out.push(MarkdownToken::new("text").with_content(&label));
                    out.push(
                        MarkdownToken::new("link_close")
                            .with_tag("a")
                            .with_markup("autolink"),
                    );
                    i = i + 1 + close + 1;
                    buf_start = i;
                    continue;
                }
                // fall-through as raw text
            }
            i += 1;
        } else if c == b'[' {
            // link [label](href)
            if let Some(label_end) = find_link_label_end(&text[i..]) {
                if buf_start < i {
                    out.push(
                        MarkdownToken::new("text").with_content(&text[buf_start..i]),
                    );
                }
                let after_label = i + label_end;
                if after_label < bytes.len() && bytes[after_label] == b'(' {
                    if let Some(close_paren) = text[after_label + 1..].find(')') {
                        let href = text[after_label + 1..after_label + 1 + close_paren].to_string();
                        out.push(
                            MarkdownToken::new("link_open")
                                .with_tag("a")
                                .with_attrs(vec![("href".to_string(), href.clone())])
                                .with_markup("link"),
                        );
                        let label = &text[i + 1..i + label_end - 1];
                        let inline_children = tokenize_inline(label);
                        out.extend(inline_children);
                        out.push(
                            MarkdownToken::new("link_close")
                                .with_tag("a")
                                .with_markup("link"),
                        );
                        i = after_label + 1 + close_paren + 1;
                        buf_start = i;
                        continue;
                    }
                }
                // label-only or other syntax: emit literal
                out.push(MarkdownToken::new("text").with_content("["));
                i += 1;
                buf_start = i;
                continue;
            }
            i += 1;
        } else if c == b'*' || c == b'_' {
            // em/strong
            let marker = c;
            let mut run = 0;
            while i < bytes.len() && bytes[i] == marker {
                run += 1;
                i += 1;
            }
            if run == 1 {
                if buf_start < i - 1 {
                    out.push(
                        MarkdownToken::new("text").with_content(&text[buf_start..i - 1]),
                    );
                }
                out.push(MarkdownToken::new("em_open").with_tag("em").with_markup("*"));
                buf_start = i;
                // try to find matching single
                if let Some(close) = text[i..].find(marker as char) {
                    let inner = &text[i..i + close];
                    let inline_children = tokenize_inline(inner);
                    out.extend(inline_children);
                    out.push(MarkdownToken::new("em_close").with_tag("em").with_markup("*"));
                    i = i + close + 1;
                    buf_start = i;
                    continue;
                }
            } else if run >= 2 {
                if buf_start < i - 2 {
                    out.push(
                        MarkdownToken::new("text").with_content(&text[buf_start..i - 2]),
                    );
                }
                out.push(
                    MarkdownToken::new("strong_open")
                        .with_tag("strong")
                        .with_markup("**"),
                );
                buf_start = i;
                let pat = if run == 2 { vec![marker, marker] } else { vec![marker; 2] };
                let pat_str = std::str::from_utf8(&pat).unwrap();
                if let Some(close) = text[i..].find(pat_str) {
                    let inner = &text[i..i + close];
                    let inline_children = tokenize_inline(inner);
                    out.extend(inline_children);
                    out.push(
                        MarkdownToken::new("strong_close")
                            .with_tag("strong")
                            .with_markup("**"),
                    );
                    i = i + close + 2;
                    buf_start = i;
                    continue;
                }
            }
        } else if c == b'~' && i + 1 < bytes.len() && bytes[i + 1] == b'~' {
            if buf_start < i {
                out.push(
                    MarkdownToken::new("text").with_content(&text[buf_start..i]),
                );
            }
            out.push(
                MarkdownToken::new("s_open")
                    .with_tag("del")
                    .with_markup("~~"),
            );
            buf_start = i + 2;
            i += 2;
            if let Some(close) = text[i..].find("~~") {
                let inner = &text[i..i + close];
                let inline_children = tokenize_inline(inner);
                out.extend(inline_children);
                out.push(
                    MarkdownToken::new("s_close")
                        .with_tag("del")
                        .with_markup("~~"),
                );
                i = i + close + 2;
                buf_start = i;
                continue;
            }
        } else {
            i += 1;
        }
    }
    if buf_start < bytes.len() {
        out.push(
            MarkdownToken::new("text").with_content(&text[buf_start..]),
        );
    }
    out
}

fn find_link_label_end(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 1usize;
    let mut depth = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b'\\' => i += 1,
            _ => {}
        }
        i += 1;
    }
    None
}

fn tokenize_blocks(markdown: &str, table_mode: MarkdownTableMode) -> Vec<MarkdownToken> {
    let mut out: Vec<MarkdownToken> = Vec::new();
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut i = 0usize;
    let _list_stack: Vec<(String, usize, bool)> = Vec::new(); // (type, level_or_indent, ordered)
    while i < lines.len() {
        let line = lines[i];
        let trimmed_start = line.trim_start();
        let indent = line.len() - trimmed_start.len();

        // blank line
        if trimmed_start.is_empty() {
            i += 1;
            continue;
        }

        // fence code block
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            let marker = if trimmed_start.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
            let info = trimmed_start[marker.len()..].trim();
            let mut content = String::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if l.trim_start().starts_with(marker) {
                    break;
                }
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(l);
                i += 1;
            }
            if i < lines.len() {
                i += 1;
            }
            let tok = MarkdownToken::new("fence")
                .with_tag("code")
                .with_info(info)
                .with_content(&content)
                .with_markup(marker)
                .with_level(indent);
            out.push(tok);
            continue;
        }

        // hr
        if is_hr(trimmed_start) {
            out.push(
                MarkdownToken::new("hr")
                    .with_tag("hr")
                    .with_markup("---"),
            );
            i += 1;
            continue;
        }

        // heading
        if let Some(level) = heading_level(trimmed_start) {
            let content = trimmed_start[level..].trim_start_matches(' ').to_string();
            out.push(
                MarkdownToken::new("heading_open")
                    .with_tag(&format!("h{}", level))
                    .with_markup(&"#".repeat(level)),
            );
            let inline = tokenize_inline(&content);
            out.push(MarkdownToken::new("inline").with_children(inline));
            out.push(
                MarkdownToken::new("heading_close")
                    .with_tag(&format!("h{}", level))
                    .with_markup(&"#".repeat(level)),
            );
            i += 1;
            continue;
        }

        // blockquote
        if trimmed_start.starts_with('>') {
            out.push(
                MarkdownToken::new("blockquote_open")
                    .with_tag("blockquote")
                    .with_markup(">"),
            );
            let mut quote_lines: Vec<String> = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                let ts = l.trim_start();
                if ts.is_empty() {
                    break;
                }
                if ts.starts_with('>') {
                    let stripped = ts[1..].trim_start_matches(' ');
                    if !quote_lines.is_empty() {
                        quote_lines.push(String::new());
                    }
                    quote_lines.push(stripped.to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            let joined = quote_lines.join("\n");
            let inner = tokenize_blocks(&joined, table_mode.clone());
            out.extend(inner);
            out.push(
                MarkdownToken::new("blockquote_close")
                    .with_tag("blockquote")
                    .with_markup(">"),
            );
            continue;
        }

        // list
        if is_list_item(trimmed_start) {
            let (ordered, _marker_len) = if is_ordered_list_item(trimmed_start) {
                (true, find_ordered_marker_len(trimmed_start))
            } else {
                (false, 1)
            };
            let list_type_str = if ordered { "ordered_list" } else { "bullet_list" };
            out.push(
                MarkdownToken::new(&format!("{}_open", list_type_str))
                    .with_tag(if ordered { "ol" } else { "ul" })
                    .with_markup(if ordered { "." } else { "-" }),
            );
            while i < lines.len() {
                let l = lines[i];
                let ts = l.trim_start();
                if ts.is_empty() {
                    break;
                }
                if !(is_list_item(ts)
                    && (if ordered {
                        is_ordered_list_item(ts)
                    } else {
                        !is_ordered_list_item(ts)
                    }))
                {
                    break;
                }
                let inner_content_start = if ordered { find_ordered_marker_len(ts) } else { 1 };
                let inner_content = ts[inner_content_start..]
                    .trim_start_matches(' ')
                    .to_string();
                out.push(MarkdownToken::new("list_item_open").with_tag("li").with_markup("-"));
                let inline = tokenize_inline(&inner_content);
                out.push(MarkdownToken::new("inline").with_children(inline));
                // paragraph_close for list items
                out.push(MarkdownToken::new("paragraph_close").with_tag("p"));
                out.push(MarkdownToken::new("list_item_close").with_tag("li").with_markup("-"));
                i += 1;
            }
            out.push(
                MarkdownToken::new(&format!("{}_close", list_type_str))
                    .with_tag(if ordered { "ol" } else { "ul" }),
            );
            continue;
        }

        // table (pipe-delimited)
        if table_mode != MarkdownTableMode::Off && trimmed_start.contains('|') && i + 1 < lines.len() {
            let next = lines[i + 1].trim_start();
            if is_table_separator(next) {
                let header_line = trimmed_start.trim().trim_matches('|').to_string();
                let headers: Vec<String> = header_line.split('|').map(|s| s.trim().to_string()).collect();
                let aligns = parse_table_alignments(next);
                let mut rows: Vec<Vec<String>> = Vec::new();
                i += 2;
                while i < lines.len() {
                    let l = lines[i].trim_start();
                    if l.is_empty() || !l.contains('|') {
                        break;
                    }
                    let row_line = l.trim().trim_matches('|').to_string();
                    let cells: Vec<String> =
                        row_line.split('|').map(|s| s.trim().to_string()).collect();
                    rows.push(cells);
                    i += 1;
                }
                out.push(
                    MarkdownToken::new("table_open")
                        .with_tag("table")
                        .with_markup("|"),
                );
                out.push(MarkdownToken::new("thead_open").with_tag("thead"));
                out.push(MarkdownToken::new("tr_open").with_tag("tr"));
                for (idx, h) in headers.iter().enumerate() {
                    let align = aligns.get(idx).cloned().flatten();
                    let attrs = align
                        .map(|a| vec![(
                            "style".to_string(),
                            format!("text-align:{}", align_str(&a)),
                        )])
                        .unwrap_or_default();
                    out.push(
                        MarkdownToken::new("th_open")
                            .with_tag("th")
                            .with_attrs(attrs),
                    );
                    let inline = tokenize_inline(h);
                    out.push(MarkdownToken::new("inline").with_children(inline));
                    out.push(MarkdownToken::new("th_close").with_tag("th"));
                }
                out.push(MarkdownToken::new("tr_close").with_tag("tr"));
                out.push(MarkdownToken::new("thead_close").with_tag("thead"));
                if !rows.is_empty() {
                    out.push(MarkdownToken::new("tbody_open").with_tag("tbody"));
                    for row in &rows {
                        out.push(MarkdownToken::new("tr_open").with_tag("tr"));
                        for (idx, cell) in row.iter().enumerate() {
                            let align = aligns.get(idx).cloned().flatten();
                            let attrs = align
                                .map(|a| vec![(
                                    "style".to_string(),
                                    format!("text-align:{}", align_str(&a)),
                                )])
                                .unwrap_or_default();
                            out.push(
                                MarkdownToken::new("td_open")
                                    .with_tag("td")
                                    .with_attrs(attrs),
                            );
                            let inline = tokenize_inline(cell);
                            out.push(MarkdownToken::new("inline").with_children(inline));
                            out.push(MarkdownToken::new("td_close").with_tag("td"));
                        }
                        out.push(MarkdownToken::new("tr_close").with_tag("tr"));
                    }
                    out.push(MarkdownToken::new("tbody_close").with_tag("tbody"));
                }
                out.push(
                    MarkdownToken::new("table_close")
                        .with_tag("table")
                        .with_markup("|"),
                );
                continue;
            }
        }

        // paragraph (collect until blank line)
        let mut para = trimmed_start.to_string();
        i += 1;
        while i < lines.len() {
            let l = lines[i];
            let ts = l.trim_start();
            if ts.is_empty() {
                break;
            }
            if ts.starts_with('#')
                || ts.starts_with('>')
                || is_list_item(ts)
                || (table_mode != MarkdownTableMode::Off && ts.contains('|') && i + 1 < lines.len())
                || ts.starts_with("```")
                || ts.starts_with("~~~")
                || is_hr(ts)
            {
                break;
            }
            para.push('\n');
            para.push_str(ts);
            i += 1;
        }
        out.push(
            MarkdownToken::new("paragraph_open")
                .with_tag("p")
                .with_markup(""),
        );
        let inline = tokenize_inline(&para);
        out.push(MarkdownToken::new("inline").with_children(inline));
        out.push(
            MarkdownToken::new("paragraph_close")
                .with_tag("p")
                .with_markup(""),
        );
    }
    out
}

fn align_str(a: &MarkdownTableAlignment) -> &'static str {
    match a {
        MarkdownTableAlignment::Left => "left",
        MarkdownTableAlignment::Center => "center",
        MarkdownTableAlignment::Right => "right",
    }
}

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    if t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_') {
        return true;
    }
    false
}

fn heading_level(line: &str) -> Option<usize> {
    let mut level = 0;
    for c in line.chars() {
        if c == '#' {
            level += 1;
        } else {
            break;
        }
    }
    if level >= 1 && level <= 6 && line.chars().nth(level) == Some(' ') {
        return Some(level);
    }
    None
}

fn is_list_item(line: &str) -> bool {
    if line.starts_with("- ") || line.starts_with("* ") || line.starts_with("+ ") {
        return true;
    }
    is_ordered_list_item(line)
}

fn is_ordered_list_item(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i > 0 && i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b' '
}

fn find_ordered_marker_len(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i + 1 // include the dot
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('|') && !t.contains('-') {
        return false;
    }
    let parts = t.split('|');
    for p in parts {
        let s = p.trim();
        if s.is_empty() {
            continue;
        }
        if !(s.chars().all(|c| c == '-' || c == ':') && s.len() >= 3) {
            return false;
        }
    }
    true
}

fn parse_table_alignments(line: &str) -> Vec<Option<MarkdownTableAlignment>> {
    let t = line.trim();
    t.split('|')
        .map(|p| {
            let s = p.trim();
            let left = s.starts_with(':');
            let right = s.ends_with(':');
            match (left, right) {
                (true, true) => Some(MarkdownTableAlignment::Center),
                (false, true) => Some(MarkdownTableAlignment::Right),
                (true, false) => Some(MarkdownTableAlignment::Left),
                _ => None,
            }
        })
        .collect()
}

pub fn markdown_to_tokens(markdown: &str, table_mode: MarkdownTableMode) -> Vec<MarkdownToken> {
    let mut tokens = tokenize_blocks(markdown, table_mode);
    // Spoiler injection
    inject_spoiler_tokens(&mut tokens);
    tokens
}

fn inject_spoiler_tokens(tokens: &mut Vec<MarkdownToken>) {
    for tok in tokens.iter_mut() {
        if !tok.children.is_empty() {
            let new_children = inject_spoilers_into_inline(&tok.children);
            tok.children = new_children;
        }
    }
}

fn inject_spoilers_into_inline(tokens: &[MarkdownToken]) -> Vec<MarkdownToken> {
    let mut total_delims = 0usize;
    for tok in tokens {
        if tok.token_type != "text" {
            continue;
        }
        let content = tok.content.clone().unwrap_or_default();
        let mut i = 0;
        while i < content.len() {
            if let Some(next) = content[i..].find("||") {
                total_delims += 1;
                i = next + 2;
            } else {
                break;
            }
        }
    }
    if total_delims < 2 {
        return tokens.to_vec();
    }
    let usable_delims = total_delims - (total_delims % 2);

    let mut result: Vec<MarkdownToken> = Vec::new();
    let mut spoiler_open = false;
    let mut consumed_delims = 0usize;

    for tok in tokens {
        if tok.token_type != "text" {
            result.push(tok.clone());
            continue;
        }
        let content = tok.content.clone().unwrap_or_default();
        if !content.contains("||") {
            result.push(tok.clone());
            continue;
        }
        let mut index = 0;
        while index < content.len() {
            let next_pos = content[index..].find("||").map(|n| n + index);
            if next_pos.is_none() {
                if index < content.len() {
                    result.push(
                        MarkdownToken::new("text").with_content(&content[index..]),
                    );
                }
                break;
            }
            let next = next_pos.unwrap();
            if consumed_delims >= usable_delims {
                result.push(
                    MarkdownToken::new("text").with_content(&content[index..]),
                );
                break;
            }
            if next > index {
                result.push(
                    MarkdownToken::new("text").with_content(&content[index..next]),
                );
            }
            consumed_delims += 1;
            spoiler_open = !spoiler_open;
            result.push(MarkdownToken::new(if spoiler_open {
                "spoiler_open"
            } else {
                "spoiler_close"
            }));
            index = next + 2;
        }
    }
    result
}

// ---------- Renderer (mirrors ir.ts renderTokens) ----------

fn init_render_target() -> RenderTarget {
    RenderTarget::default()
}

fn resolve_render_target(state: &RenderState) -> &RenderTarget {
    if let Some(t) = &state.table {
        if let Some(c) = &t.current_cell {
            return c;
        }
    }
    &state.target
}

fn resolve_render_target_mut(state: &mut RenderState) -> &mut RenderTarget {
    if let Some(t) = &mut state.table {
        if let Some(c) = &mut t.current_cell {
            return c;
        }
    }
    &mut state.target
}

fn append_text(state: &mut RenderState, value: &str) {
    if value.is_empty() {
        return;
    }
    let target = resolve_render_target_mut(state);
    target.text.push_str(value);
}

fn open_style(state: &mut RenderState, style: MarkdownStyle) {
    let target = resolve_render_target_mut(state);
    let len = target.text.len();
    target.open_styles.push(OpenStyle { style, start: len });
}

fn close_style(
    state: &mut RenderState,
    style: MarkdownStyle,
    trim_trailing_paragraph_separator: bool,
) {
    let target = resolve_render_target_mut(state);
    for i in (0..target.open_styles.len()).rev() {
        if target.open_styles[i].style == style {
            let start = target.open_styles[i].start;
            target.open_styles.remove(i);
            let end = if trim_trailing_paragraph_separator && target.text.ends_with("\n\n") {
                target.text.len() - 2
            } else {
                target.text.len()
            };
            if end > start {
                target.styles.push(MarkdownStyleSpan {
                    start,
                    end,
                    style: style.clone(),
                    language: None,
                });
            }
            return;
        }
    }
}

fn append_paragraph_separator(state: &mut RenderState, token: Option<&MarkdownToken>) {
    if state.table.is_some() {
        return;
    }
    if let Some(top) = state.list_stack.last() {
        let direct_list_paragraph_level = top.open_level + 2;
        let ok = token.map(|t| {
            t.token_type == "paragraph_close" && !t.hidden && t.level == direct_list_paragraph_level
        });
        if !ok.unwrap_or(false) {
            return;
        }
    }
    state.target.text.push_str("\n\n");
}

fn append_top_level_list_separator(state: &mut RenderState) {
    let trailing_newlines = state
        .target
        .text
        .chars()
        .rev()
        .take_while(|c| *c == '\n')
        .count();
    if trailing_newlines < 2 {
        state.target.text.push('\n');
    }
}

fn append_nested_list_separator(state: &mut RenderState) {
    if !state.target.text.ends_with('\n') {
        state.target.text.push('\n');
    }
}

fn append_list_prefix(state: &mut RenderState) {
    let stack_len = state.list_stack.len();
    if let Some(top) = state.list_stack.last_mut() {
        top.index += 1;
        let indent = "  ".repeat(stack_len.saturating_sub(1));
        let prefix = if top.list_type == ListType::Ordered {
            format!("{}. ", top.index)
        } else {
            "• ".to_string()
        };
        state.target.text.push_str(&format!("{}{}", indent, prefix));
    }
}

fn render_inline_code(state: &mut RenderState, content: &str) {
    if content.is_empty() {
        return;
    }
    let target = resolve_render_target_mut(state);
    let start = target.text.len();
    target.text.push_str(content);
    target.styles.push(MarkdownStyleSpan {
        start,
        end: start + content.len(),
        style: MarkdownStyle::Code,
        language: None,
    });
}

fn resolve_fence_language(info: Option<&str>) -> Option<String> {
    let lang = info
        .and_then(|s| s.split_whitespace().next())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    lang.map(|s| s.to_string())
}

fn render_code_block(state: &mut RenderState, content: &str, info: Option<&str>) {
    let mut code = content.to_string();
    if !code.ends_with('\n') {
        code.push('\n');
    }
    let list_empty = state.list_stack.is_empty();
    let target = resolve_render_target_mut(state);
    let start = target.text.len();
    target.text.push_str(&code);
    target.styles.push(create_style_span(MarkdownStyleSpan {
        start,
        end: start + code.len(),
        style: MarkdownStyle::CodeBlock,
        language: resolve_fence_language(info),
    }));
    if list_empty {
        target.text.push('\n');
    }
}

fn handle_link_close(state: &mut RenderState) {
    let link = {
        let target = resolve_render_target_mut(state);
        target.link_stack.pop()
    };
    if link.is_none() {
        return;
    }
    let link = link.unwrap();
    if link.href.is_empty() {
        return;
    }
    let href = link.href.trim();
    if href.is_empty() {
        return;
    }
    let start = link.label_start;
    let end = resolve_render_target(state).text.len();
    if end <= start {
        return;
    }
    let target = resolve_render_target_mut(state);
    target.links.push(MarkdownLinkSpan {
        start,
        end,
        href: href.to_string(),
    });
}

fn heading_style_from_token(token: &MarkdownToken) -> Option<MarkdownStyle> {
    match token.tag.as_deref() {
        Some("h1") => Some(MarkdownStyle::Heading1),
        Some("h2") => Some(MarkdownStyle::Heading2),
        Some("h3") => Some(MarkdownStyle::Heading3),
        Some("h4") => Some(MarkdownStyle::Heading4),
        Some("h5") => Some(MarkdownStyle::Heading5),
        Some("h6") => Some(MarkdownStyle::Heading6),
        _ => None,
    }
}

fn is_inside_markdown_html_tag(text: &str) -> bool {
    let last_lt = text.rfind('<');
    if last_lt.is_none() {
        return false;
    }
    let last_lt = last_lt.unwrap();
    let last_gt = text.rfind('>');
    if let Some(g) = last_gt {
        if g > last_lt {
            return false;
        }
    }
    // simple ASCII tag pattern
    let suffix = &text[last_lt..];
    let bytes = suffix.as_bytes();
    let mut i = 0;
    let len = bytes.len();
    if bytes.first().copied() != Some(b'<') {
        return false;
    }
    i += 1;
    if i < len && bytes[i] == b'/' {
        i += 1;
    }
    if i >= len || !bytes[i].is_ascii_alphabetic() {
        return false;
    }
    while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    while i < len && bytes[i] != b'>' {
        i += 1;
    }
    i < len
}

fn init_table_state() -> TableState {
    TableState::default()
}

fn close_remaining_styles(target: &mut RenderTarget) {
    let opens = target.open_styles.clone();
    for open in opens.iter().rev() {
        let end = target.text.len();
        if end > open.start {
            target.styles.push(MarkdownStyleSpan {
                start: open.start,
                end,
                style: open.style.clone(),
                language: None,
            });
        }
    }
    target.open_styles.clear();
}

fn finish_table_cell(cell: RenderTarget) -> TableCell {
    let mut c = cell;
    close_remaining_styles(&mut c);
    TableCell {
        text: c.text,
        styles: c.styles,
        links: c.links,
    }
}

fn trim_cell(cell: TableCell) -> TableCell {
    let TableCell { text, styles, links } = cell;
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut end = text.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if start == 0 && end == text.len() {
        return TableCell { text, styles, links };
    }
    let trimmed_text = text[start..end].to_string();
    let trimmed_length = trimmed_text.len();
    let mut trimmed_styles: Vec<MarkdownStyleSpan> = Vec::new();
    for span in styles {
        let slice_start = span.start.saturating_sub(start);
        let slice_end = (span.end - start).min(trimmed_length);
        if slice_end > slice_start {
            trimmed_styles.push(MarkdownStyleSpan {
                start: slice_start,
                end: slice_end,
                style: span.style,
                language: span.language,
            });
        }
    }
    let mut trimmed_links: Vec<MarkdownLinkSpan> = Vec::new();
    for link in links {
        let slice_start = link.start.saturating_sub(start);
        let slice_end = (link.end - start).min(trimmed_length);
        if slice_end > slice_start {
            trimmed_links.push(MarkdownLinkSpan {
                start: slice_start,
                end: slice_end,
                href: link.href,
            });
        }
    }
    TableCell {
        text: trimmed_text,
        styles: trimmed_styles,
        links: trimmed_links,
    }
}

fn append_cell(state: &mut RenderState, cell: &TableCell) {
    if cell.text.is_empty() {
        return;
    }
    let start = state.target.text.len();
    state.target.text.push_str(&cell.text);
    for span in &cell.styles {
        state.target.styles.push(MarkdownStyleSpan {
            start: start + span.start,
            end: start + span.end,
            style: span.style.clone(),
            language: span.language.clone(),
        });
    }
    for link in &cell.links {
        state.target.links.push(MarkdownLinkSpan {
            start: start + link.start,
            end: start + link.end,
            href: link.href.clone(),
        });
    }
}

fn append_cell_text_only(state: &mut RenderState, cell: &TableCell) {
    if cell.text.is_empty() {
        return;
    }
    state.target.text.push_str(&cell.text);
}

fn visible_width(s: &str) -> usize {
    s.chars().count()
}

fn collect_table_block(state: &mut RenderState) {
    if state.table.is_none() {
        return;
    }
    let t = state.table.as_ref().unwrap();
    let header_cells: Vec<TableCell> = t.headers.iter().cloned().map(trim_cell).collect();
    let row_cells: Vec<Vec<TableCell>> = t
        .rows
        .iter()
        .map(|r| r.iter().cloned().map(trim_cell).collect())
        .collect();
    let has_aligns = t.aligns.iter().any(|a| a.is_some());
    let placeholder_offset = state.target.text.len();
    let headers_text: Vec<String> = header_cells.iter().map(|c| c.text.clone()).collect();
    let rows_text: Vec<Vec<String>> = row_cells
        .iter()
        .map(|r| r.iter().map(|c| c.text.clone()).collect())
        .collect();
    let aligns = if has_aligns {
        t.aligns.clone()
    } else {
        Vec::new()
    };
    let table = MarkdownTableMeta {
        headers: headers_text,
        rows: rows_text,
        aligns,
        placeholder_offset,
        header_cells,
        row_cells,
    };
    state.collected_tables.push(table);
}

fn append_table_bullet_value(
    state: &mut RenderState,
    header: Option<&TableCell>,
    value: Option<&TableCell>,
    column_index: usize,
    include_column_fallback: bool,
) {
    if value.is_none() || value.unwrap().text.is_empty() {
        return;
    }
    state.target.text.push_str("• ");
    if let Some(h) = header {
        if !h.text.is_empty() {
            append_cell(state, h);
            state.target.text.push_str(": ");
        }
    } else if include_column_fallback {
        state.target.text.push_str(&format!("Column {}: ", column_index));
    }
    append_cell(state, value.unwrap());
    state.target.text.push('\n');
}

fn render_table_as_bullets(state: &mut RenderState) {
    if state.table.is_none() {
        return;
    }
    let t = state.table.as_ref().unwrap();
    let headers: Vec<TableCell> = t.headers.iter().cloned().map(trim_cell).collect();
    let rows: Vec<Vec<TableCell>> = t
        .rows
        .iter()
        .map(|r| r.iter().cloned().map(trim_cell).collect())
        .collect();

    if headers.is_empty() && rows.is_empty() {
        return;
    }

    let use_first_col_as_label = headers.len() > 1 && !rows.is_empty();

    if use_first_col_as_label {
        for row in &rows {
            if row.is_empty() {
                continue;
            }
            if let Some(row_label) = row.first() {
                if !row_label.text.is_empty() {
                    let label_start = state.target.text.len();
                    append_cell(state, row_label);
                    let label_end = state.target.text.len();
                    if label_end > label_start {
                        state.target.styles.push(MarkdownStyleSpan {
                            start: label_start,
                            end: label_end,
                            style: MarkdownStyle::Bold,
                            language: None,
                        });
                    }
                    state.target.text.push('\n');
                }
            }
            for i in 1..row.len() {
                let header = headers.get(i);
                let value = row.get(i);
                append_table_bullet_value(state, header, value, i, true);
            }
            state.target.text.push('\n');
        }
    } else {
        for row in &rows {
            for i in 0..row.len() {
                let header = headers.get(i);
                let value = row.get(i);
                append_table_bullet_value(state, header, value, i, false);
            }
            state.target.text.push('\n');
        }
    }
}

fn render_table_as_code(state: &mut RenderState) {
    if state.table.is_none() {
        return;
    }
    let t = state.table.as_ref().unwrap();
    let headers: Vec<TableCell> = t.headers.iter().cloned().map(trim_cell).collect();
    let rows: Vec<Vec<TableCell>> = t
        .rows
        .iter()
        .map(|r| r.iter().cloned().map(trim_cell).collect())
        .collect();

    let column_count = headers
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if column_count == 0 {
        return;
    }

    let mut widths = vec![0usize; column_count];
    let update_widths = |cells: &[TableCell], widths: &mut Vec<usize>| {
        for (i, w) in widths.iter_mut().enumerate() {
            let cell = cells.get(i);
            let cw = visible_width(cell.map(|c| c.text.as_str()).unwrap_or(""));
            if *w < cw {
                *w = cw;
            }
        }
    };
    update_widths(&headers, &mut widths);
    for r in &rows {
        update_widths(r, &mut widths);
    }

    let code_start = state.target.text.len();

    {
        let widths = &widths;
        let render_row = |cells: &[TableCell], state: &mut RenderState| {
            state.target.text.push('|');
            for (i, width) in widths.iter().enumerate() {
                state.target.text.push(' ');
                let cell = cells.get(i);
                if let Some(c) = cell {
                    append_cell_text_only(state, c);
                }
                let pad = width
                    .saturating_sub(visible_width(cell.map(|c| c.text.as_str()).unwrap_or("")));
                if pad > 0 {
                    state.target.text.push_str(&" ".repeat(pad));
                }
                state.target.text.push_str(" |");
            }
            state.target.text.push('\n');
        };
        render_row(&headers, state);
        state.target.text.push('|');
        for width in widths {
            let dash_count = (*width).max(3);
            state.target.text.push_str(&format!(" {} |\n", "-".repeat(dash_count)));
        }
        for r in &rows {
            render_row(r, state);
        }
    }

    let code_end = state.target.text.len();
    if code_end > code_start {
        state.target.styles.push(MarkdownStyleSpan {
            start: code_start,
            end: code_end,
            style: MarkdownStyle::CodeBlock,
            language: None,
        });
    }
    if state.list_stack.is_empty() {
        state.target.text.push('\n');
    }
}

fn render_tokens(tokens: &[MarkdownToken], state: &mut RenderState) {
    for token in tokens {
        match token.token_type.as_str() {
            "inline" => {
                render_tokens(&token.children, state);
            }
            "text" => append_text(state, token.content.as_deref().unwrap_or("")),
            "em_open" => open_style(state, MarkdownStyle::Italic),
            "em_close" => close_style(state, MarkdownStyle::Italic, false),
            "strong_open" => open_style(state, MarkdownStyle::Bold),
            "strong_close" => close_style(state, MarkdownStyle::Bold, false),
            "s_open" => open_style(state, MarkdownStyle::Strikethrough),
            "s_close" => close_style(state, MarkdownStyle::Strikethrough, false),
            "code_inline" => render_inline_code(state, token.content.as_deref().unwrap_or("")),
            "spoiler_open" => {
                if state.enable_spoilers {
                    open_style(state, MarkdownStyle::Spoiler);
                }
            }
            "spoiler_close" => {
                if state.enable_spoilers {
                    close_style(state, MarkdownStyle::Spoiler, false);
                }
            }
            "link_open" => {
                let href = {
                    let target = resolve_render_target(state);
                    if is_inside_markdown_html_tag(&target.text) {
                        String::new()
                    } else {
                        token.attr_get("href").unwrap_or_default()
                    }
                };
                let start = resolve_render_target(state).text.len();
                resolve_render_target_mut(state).link_stack.push(LinkState {
                    href,
                    label_start: start,
                });
            }
            "link_close" => handle_link_close(state),
            "image" => append_text(state, token.content.as_deref().unwrap_or("")),
            "softbreak" | "hardbreak" => append_text(state, "\n"),
            "paragraph_close" => append_paragraph_separator(state, Some(token)),
            "heading_open" => {
                if matches!(state.heading_style, HeadingStyle::Bold) {
                    open_style(state, MarkdownStyle::Bold);
                } else if matches!(state.heading_style, HeadingStyle::Rich) {
                    if let Some(style) = heading_style_from_token(token) {
                        open_style(state, style);
                    }
                }
            }
            "heading_close" => {
                if matches!(state.heading_style, HeadingStyle::Bold) {
                    close_style(state, MarkdownStyle::Bold, false);
                } else if matches!(state.heading_style, HeadingStyle::Rich) {
                    if let Some(style) = heading_style_from_token(token) {
                        close_style(state, style, false);
                    }
                }
                append_paragraph_separator(state, None);
            }
            "blockquote_open" => {
                if !state.blockquote_prefix.is_empty() {
                    state.target.text.push_str(&state.blockquote_prefix);
                }
                open_style(state, MarkdownStyle::Blockquote);
            }
            "blockquote_close" => {
                close_style(state, MarkdownStyle::Blockquote, true);
            }
            "bullet_list_open" => {
                if !state.list_stack.is_empty() {
                    append_nested_list_separator(state);
                }
                state.list_stack.push(ListState {
                    list_type: ListType::Bullet,
                    index: 0,
                    open_level: token.level,
                });
            }
            "bullet_list_close" => {
                state.list_stack.pop();
                if state.list_stack.is_empty() {
                    append_top_level_list_separator(state);
                }
            }
            "ordered_list_open" => {
                if !state.list_stack.is_empty() {
                    append_nested_list_separator(state);
                }
                let start = token
                    .attr_get("start")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1);
                state.list_stack.push(ListState {
                    list_type: ListType::Ordered,
                    index: start - 1,
                    open_level: token.level,
                });
            }
            "ordered_list_close" => {
                state.list_stack.pop();
                if state.list_stack.is_empty() {
                    append_top_level_list_separator(state);
                }
            }
            "list_item_open" => {
                append_list_prefix(state);
            }
            "list_item_close" => {
                if !state.target.text.ends_with('\n') {
                    state.target.text.push('\n');
                }
            }
            "code_block" | "fence" => {
                render_code_block(state, token.content.as_deref().unwrap_or(""), token.info.as_deref());
            }
            "html_block" | "html_inline" => {
                append_text(state, token.content.as_deref().unwrap_or(""));
            }
            "table_open" => {
                if state.table_mode != MarkdownTableMode::Off {
                    state.table = Some(init_table_state());
                    state.has_tables = true;
                }
            }
            "table_close" => {
                if state.table.is_some() {
                    match state.table_mode {
                        MarkdownTableMode::Bullets => render_table_as_bullets(state),
                        MarkdownTableMode::Code => render_table_as_code(state),
                        MarkdownTableMode::Block => collect_table_block(state),
                        MarkdownTableMode::Off => {}
                    }
                }
                state.table = None;
            }
            "thead_open" => {
                if let Some(t) = state.table.as_mut() {
                    t.in_header = true;
                }
            }
            "thead_close" => {
                if let Some(t) = state.table.as_mut() {
                    t.in_header = false;
                }
            }
            "tbody_open" | "tbody_close" => {}
            "tr_open" => {
                if let Some(t) = state.table.as_mut() {
                    t.current_row = Vec::new();
                }
            }
            "tr_close" => {
                if let Some(t) = state.table.as_mut() {
                    if t.in_header {
                        t.headers = t.current_row.clone();
                    } else {
                        t.rows.push(t.current_row.clone());
                    }
                    t.current_row = Vec::new();
                }
            }
            "th_open" | "td_open" => {
                if let Some(t) = state.table.as_mut() {
                    t.current_cell = Some(init_render_target());
                    if token.token_type == "th_open" && t.in_header {
                        let col = t.current_row.len();
                        if t.aligns.len() <= col {
                            t.aligns.resize(col + 1, None);
                        }
                        let style = token
                            .attr_get("style")
                            .unwrap_or_default();
                        t.aligns[col] = markdown_table_alignment_from_style(&style);
                    }
                }
            }
            "th_close" | "td_close" => {
                if let Some(t) = state.table.as_mut() {
                    if let Some(cell) = t.current_cell.take() {
                        t.current_row.push(finish_table_cell(cell));
                    }
                }
            }
            "hr" => {
                state.target.text.push_str("---\n\n");
            }
            _ => {
                if !token.children.is_empty() {
                    render_tokens(&token.children, state);
                }
            }
        }
    }
}

fn markdown_table_alignment_from_style(value: &str) -> Option<MarkdownTableAlignment> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("text-align:left") {
        Some(MarkdownTableAlignment::Left)
    } else if lower.contains("text-align:center") {
        Some(MarkdownTableAlignment::Center)
    } else if lower.contains("text-align:right") {
        Some(MarkdownTableAlignment::Right)
    } else {
        None
    }
}

fn clamp_style_spans(spans: Vec<MarkdownStyleSpan>, max_length: usize) -> Vec<MarkdownStyleSpan> {
    let mut clamped: Vec<MarkdownStyleSpan> = Vec::new();
    for span in spans {
        let start = span.start.min(max_length);
        let end = span.end.min(max_length).max(start);
        if end > start {
            clamped.push(create_style_span(MarkdownStyleSpan {
                start,
                end,
                style: span.style,
                language: span.language,
            }));
        }
    }
    clamped
}

fn clamp_link_spans(spans: Vec<MarkdownLinkSpan>, max_length: usize) -> Vec<MarkdownLinkSpan> {
    let mut clamped: Vec<MarkdownLinkSpan> = Vec::new();
    for span in spans {
        let start = span.start.min(max_length);
        let end = span.end.min(max_length).max(start);
        if end > start {
            clamped.push(MarkdownLinkSpan {
                start,
                end,
                href: span.href,
            });
        }
    }
    clamped
}

fn merge_style_spans(spans: Vec<MarkdownStyleSpan>) -> Vec<MarkdownStyleSpan> {
    let mut sorted = spans.clone();
    sorted.sort_by(|a, b| {
        if a.start != b.start {
            return a.start.cmp(&b.start);
        }
        if a.end != b.end {
            return a.end.cmp(&b.end);
        }
        a.style.as_str().cmp(b.style.as_str())
    });
    let mut merged: Vec<MarkdownStyleSpan> = Vec::new();
    for span in sorted {
        let prev = merged.last_mut();
        if let Some(p) = prev {
            if p.style == span.style
                && p.language == span.language
                && (span.start < p.end
                    || (span.start == p.end && span.style != MarkdownStyle::Blockquote))
            {
                if span.end > p.end {
                    p.end = span.end;
                }
                continue;
            }
        }
        merged.push(span);
    }
    merged
}

fn slice_style_spans(
    spans: &[MarkdownStyleSpan],
    start: usize,
    end: usize,
) -> Vec<MarkdownStyleSpan> {
    if spans.is_empty() {
        return Vec::new();
    }
    let mut sliced: Vec<MarkdownStyleSpan> = Vec::new();
    for span in spans {
        let slice_start = span.start.max(start);
        let slice_end = span.end.min(end);
        if slice_end <= slice_start {
            continue;
        }
        sliced.push(create_style_span(MarkdownStyleSpan {
            start: slice_start - start,
            end: slice_end - start,
            style: span.style.clone(),
            language: span.language.clone(),
        }));
    }
    merge_style_spans(sliced)
}

fn slice_link_spans(spans: &[MarkdownLinkSpan], start: usize, end: usize) -> Vec<MarkdownLinkSpan> {
    if spans.is_empty() {
        return Vec::new();
    }
    let mut sliced: Vec<MarkdownLinkSpan> = Vec::new();
    for span in spans {
        let slice_start = span.start.max(start);
        let slice_end = span.end.min(end);
        if slice_end <= slice_start {
            continue;
        }
        sliced.push(MarkdownLinkSpan {
            start: slice_start - start,
            end: slice_end - start,
            href: span.href.clone(),
        });
    }
    sliced
}

pub fn slice_markdown_ir(ir: &MarkdownIR, start: usize, end: usize) -> MarkdownIR {
    MarkdownIR {
        text: ir.text[start..end].to_string(),
        styles: slice_style_spans(&ir.styles, start, end),
        links: slice_link_spans(&ir.links, start, end),
    }
}

pub fn markdown_to_ir(markdown: &str, options: MarkdownParseOptions) -> MarkdownIR {
    markdown_to_ir_with_meta(markdown, options).ir
}

pub struct MarkdownParseResult {
    pub ir: MarkdownIR,
    pub has_tables: bool,
    pub tables: Vec<MarkdownTableMeta>,
}

pub fn markdown_to_ir_with_meta(
    markdown: &str,
    options: MarkdownParseOptions,
) -> MarkdownParseResult {
    let table_mode = options.table_mode.unwrap_or(MarkdownTableMode::Off);
    let tokens = markdown_to_tokens(markdown, table_mode.clone());

    let mut state = RenderState {
        target: RenderTarget::default(),
        list_stack: Vec::new(),
        heading_style: options.heading_style.unwrap_or(HeadingStyle::None),
        blockquote_prefix: options.blockquote_prefix.unwrap_or_default(),
        enable_spoilers: options.enable_spoilers.unwrap_or(false),
        table_mode,
        table: None,
        has_tables: false,
        collected_tables: Vec::new(),
    };

    render_tokens(&tokens, &mut state);
    close_remaining_styles(&mut state.target);

    let trimmed_text = state.target.text.trim_end().to_string();
    let trimmed_length = trimmed_text.len();
    let mut code_block_end = 0usize;
    for span in &state.target.styles {
        if span.style == MarkdownStyle::CodeBlock && span.end > code_block_end {
            code_block_end = span.end;
        }
    }
    let final_length = trimmed_length.max(code_block_end);
    let final_text = if final_length == state.target.text.len() {
        state.target.text.clone()
    } else {
        state.target.text[..final_length].to_string()
    };

    let final_styles = merge_style_spans(clamp_style_spans(state.target.styles.clone(), final_length));
    let final_links = clamp_link_spans(state.target.links.clone(), final_length);
    let ir = MarkdownIR {
        text: final_text,
        styles: final_styles,
        links: final_links,
    };
    let has_tables = state.has_tables;
    let tables = state
        .collected_tables
        .into_iter()
        .map(|mut t| {
            t.placeholder_offset = t.placeholder_offset.min(final_length);
            t
        })
        .collect();
    MarkdownParseResult { ir, has_tables, tables }
}

pub fn chunk_markdown_ir(ir: &MarkdownIR, limit: usize) -> Vec<MarkdownIR> {
    if ir.text.is_empty() {
        return Vec::new();
    }
    if limit == 0 || ir.text.chars().count() <= limit {
        return vec![ir.clone()];
    }

    let chunks = chunk_text(&ir.text, limit);
    let mut results: Vec<MarkdownIR> = Vec::new();
    let bytes = ir.text.as_bytes();
    let mut cursor = 0usize;

    for (index, chunk) in chunks.iter().enumerate() {
        if chunk.is_empty() {
            continue;
        }
        if index > 0 {
            while cursor < bytes.len() && (bytes[cursor] as char).is_whitespace() {
                cursor += 1;
            }
        }
        let start = cursor;
        let end = (bytes.len()).min(start + chunk.len());
        results.push(MarkdownIR {
            text: chunk.clone(),
            styles: slice_style_spans(&ir.styles, start, end),
            links: slice_link_spans(&ir.links, start, end),
        });
        cursor = end;
    }
    results
}