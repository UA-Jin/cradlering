// Markdown Core module implements render behavior.
// 翻译自 packages/markdown-core/src/render.ts
use crate::ir::{MarkdownIR, MarkdownLinkSpan, MarkdownStyle, MarkdownStyleSpan};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum RenderStyleOpen {
    Static(String),
    Dynamic(fn(&MarkdownStyleSpan) -> String),
}

#[derive(Debug, Clone)]
pub struct RenderStyleMarker {
    pub open: RenderStyleOpen,
    pub close: String,
}

#[derive(Debug, Clone, Default)]
pub struct RenderStyleMap {
    pub bold: Option<RenderStyleMarker>,
    pub italic: Option<RenderStyleMarker>,
    pub strikethrough: Option<RenderStyleMarker>,
    pub code: Option<RenderStyleMarker>,
    pub code_block: Option<RenderStyleMarker>,
    pub spoiler: Option<RenderStyleMarker>,
    pub blockquote: Option<RenderStyleMarker>,
    pub heading_1: Option<RenderStyleMarker>,
    pub heading_2: Option<RenderStyleMarker>,
    pub heading_3: Option<RenderStyleMarker>,
    pub heading_4: Option<RenderStyleMarker>,
    pub heading_5: Option<RenderStyleMarker>,
    pub heading_6: Option<RenderStyleMarker>,
}

impl RenderStyleMap {
    pub fn get(&self, style: &MarkdownStyle) -> Option<&RenderStyleMarker> {
        match style {
            MarkdownStyle::Bold => self.bold.as_ref(),
            MarkdownStyle::Italic => self.italic.as_ref(),
            MarkdownStyle::Strikethrough => self.strikethrough.as_ref(),
            MarkdownStyle::Code => self.code.as_ref(),
            MarkdownStyle::CodeBlock => self.code_block.as_ref(),
            MarkdownStyle::Spoiler => self.spoiler.as_ref(),
            MarkdownStyle::Blockquote => self.blockquote.as_ref(),
            MarkdownStyle::Heading1 => self.heading_1.as_ref(),
            MarkdownStyle::Heading2 => self.heading_2.as_ref(),
            MarkdownStyle::Heading3 => self.heading_3.as_ref(),
            MarkdownStyle::Heading4 => self.heading_4.as_ref(),
            MarkdownStyle::Heading5 => self.heading_5.as_ref(),
            MarkdownStyle::Heading6 => self.heading_6.as_ref(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderLink {
    pub start: usize,
    pub end: usize,
    pub open: String,
    pub close: String,
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub style_markers: RenderStyleMap,
    pub escape_text: fn(&str) -> String,
    pub build_link: Option<fn(&MarkdownLinkSpan, &str) -> Option<RenderLink>>,
}

const STYLE_ORDER: [&str; 13] = [
    "blockquote",
    "code_block",
    "code",
    "heading_1",
    "heading_2",
    "heading_3",
    "heading_4",
    "heading_5",
    "heading_6",
    "bold",
    "italic",
    "strikethrough",
    "spoiler",
];

fn style_rank(style: &MarkdownStyle) -> usize {
    let s = style.as_str();
    STYLE_ORDER.iter().position(|x| *x == s).unwrap_or(0)
}

fn open_marker(marker: &RenderStyleMarker, span: &MarkdownStyleSpan) -> String {
    match &marker.open {
        RenderStyleOpen::Static(s) => s.clone(),
        RenderStyleOpen::Dynamic(f) => f(span),
    }
}

pub fn render_markdown_with_markers(ir: &MarkdownIR, options: &RenderOptions) -> String {
    let text = ir.text.clone();
    if text.is_empty() {
        return String::new();
    }
    let text_len = text.len();

    let styled: Vec<&MarkdownStyleSpan> = ir
        .styles
        .iter()
        .filter(|s| options.style_markers.get(&s.style).is_some())
        .collect();

    let mut boundaries: Vec<usize> = vec![0, text_len];
    let mut starts_at: HashMap<usize, Vec<&MarkdownStyleSpan>> = HashMap::new();
    for span in &styled {
        if span.start == span.end {
            continue;
        }
        boundaries.push(span.start);
        boundaries.push(span.end);
        starts_at.entry(span.start).or_insert_with(Vec::new).push(span);
    }
    for spans in starts_at.values_mut() {
        spans.sort_by(|a, b| {
            if a.end != b.end {
                return b.end.cmp(&a.end);
            }
            style_rank(&a.style).cmp(&style_rank(&b.style))
        });
    }

    let mut link_starts: HashMap<usize, Vec<RenderLink>> = HashMap::new();
    if let Some(build_link) = options.build_link {
        for link in &ir.links {
            if link.start == link.end {
                continue;
            }
            if let Some(rendered) = build_link(link, &text) {
                boundaries.push(rendered.start);
                boundaries.push(rendered.end);
                link_starts
                    .entry(rendered.start)
                    .or_insert_with(Vec::new)
                    .push(rendered);
            }
        }
    }

    boundaries.sort();
    boundaries.dedup();
    let points = boundaries;

    #[derive(Debug)]
    #[allow(dead_code)]
    enum OpeningItem {
        Link { end: usize, open: String, close: String, index: usize },
        Style { end: usize, open: String, close: String, style: MarkdownStyle, index: usize },
    }

    struct StackItem {
        close: String,
        end: usize,
    }

    let mut stack: Vec<StackItem> = Vec::new();
    let mut out = String::new();

    for (i, &pos) in points.iter().enumerate() {
        while let Some(top) = stack.last() {
            if top.end == pos {
                let popped = stack.pop().unwrap();
                out.push_str(&popped.close);
            } else {
                break;
            }
        }

        let mut opening_items: Vec<OpeningItem> = Vec::new();

        if let Some(links) = link_starts.get(&pos) {
            for (index, link) in links.iter().enumerate() {
                opening_items.push(OpeningItem::Link {
                    end: link.end,
                    open: link.open.clone(),
                    close: link.close.clone(),
                    index,
                });
            }
        }

        if let Some(spans) = starts_at.get(&pos) {
            for (index, span) in spans.iter().enumerate() {
                let marker = options.style_markers.get(&span.style);
                let marker = match marker {
                    Some(m) => m,
                    None => continue,
                };
                opening_items.push(OpeningItem::Style {
                    end: span.end,
                    open: open_marker(marker, span),
                    close: marker.close.clone(),
                    style: span.style.clone(),
                    index,
                });
            }
        }

        if !opening_items.is_empty() {
            opening_items.sort_by(|a, b| {
                let a_end = match a {
                    OpeningItem::Link { end, .. } => *end,
                    OpeningItem::Style { end, .. } => *end,
                };
                let b_end = match b {
                    OpeningItem::Link { end, .. } => *end,
                    OpeningItem::Style { end, .. } => *end,
                };
                if a_end != b_end {
                    return b_end.cmp(&a_end);
                }
                let a_kind = matches!(a, OpeningItem::Link { .. });
                let b_kind = matches!(b, OpeningItem::Link { .. });
                if a_kind != b_kind {
                    return if a_kind { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
                }
                match (a, b) {
                    (OpeningItem::Style { style: sa, index: ia, .. }, OpeningItem::Style { style: sb, index: ib, .. }) => {
                        style_rank(sa).cmp(&style_rank(sb)).then(ia.cmp(ib))
                    }
                    _ => std::cmp::Ordering::Equal,
                }
            });

            for item in opening_items {
                let (end, open, close) = match item {
                    OpeningItem::Link { end, open, close, .. } => (end, open, close),
                    OpeningItem::Style { end, open, close, .. } => (end, open, close),
                };
                out.push_str(&open);
                stack.push(StackItem { close, end });
            }
        }

        let next = points.get(i + 1);
        if let Some(&n) = next {
            if n > pos {
                out.push_str(&(options.escape_text)(&text[pos..n]));
            }
        } else {
            break;
        }
    }

    out
}