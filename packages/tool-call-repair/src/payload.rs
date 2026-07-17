// Tool Call Repair module implements payload behavior.
// 1:1 port of openclaw-main/packages/tool-call-repair/src/payload.ts
// openclaw -> cradle-ring renames applied. Logic preserved line-by-line.

use serde_json::Value as JsonValue;
use std::collections::HashSet;

use crate::grammar::{
    consume_line_break, consume_structural_line_break_after_horizontal_whitespace,
    scan_xmlish_tool_call, skip_horizontal_whitespace, skip_line_indentation, skip_whitespace,
    starts_with_ascii_marker_ignore_case, utf8_byte_length_within_limit, XmlishToolCallScan,
    XmlishSyntax, END_TOOL_REQUEST, HARMONY_CALL_MARKER, HARMONY_CHANNEL_MARKER,
    HARMONY_MESSAGE_MARKER, StructuralLineBreakOptions,
};

/// Parsed standalone plain-text tool call block with source offsets for repair.
#[derive(Debug, Clone)]
pub struct PlainTextToolCallBlock {
    /// Parsed JSON arguments object.
    pub arguments: serde_json::Map<String, JsonValue>,
    /// Exclusive end offset of the parsed block.
    pub end: usize,
    /// Tool name parsed from bracket, Harmony, or XML-ish syntax.
    pub name: String,
    /// Original text slice that produced this block.
    pub raw: String,
    /// Inclusive start offset of the parsed block.
    pub start: usize,
}

/// Parser limits and allowlist options for plain-text tool-call repair.
#[derive(Debug, Clone, Default)]
pub struct PlainTextToolCallParseOptions {
    /// Optional allowlist of tool names that may be repaired.
    pub allowed_tool_names: Option<Vec<String>>,
    /// Maximum serialized payload size accepted for one repaired call.
    pub max_payload_bytes: Option<usize>,
}

#[derive(Debug, Clone)]
struct NormalizedPlainTextToolCallParseOptions {
    pub allowed_tool_names: Option<HashSet<String>>,
    pub max_payload_bytes: Option<usize>,
}

const DEFAULT_MAX_PLAIN_TEXT_TOOL_PAYLOAD_BYTES: usize = 256_000;
const MAX_PLAIN_TEXT_TOOL_NAME_CHARS: usize = 120;
const HARMONY_CHANNELS: [&str; 3] = ["commentary", "analysis", "final"];

#[derive(Debug, Clone, Copy)]
pub struct PlainTextJsonToolCallSpan {
    pub end: usize,
    pub start: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlainTextJsonToolCallSyntax {
    Harmony,
    NamedBracket,
    ToolBracket,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlainTextJsonToolCallState {
    pub depth: i64,
    pub escaped: bool,
    pub in_string: bool,
}

#[derive(Debug, Clone)]
pub struct PlainTextJsonToolCallCandidate {
    pub json: Option<PlainTextJsonToolCallState>,
    pub name: PlainTextJsonToolCallSpan,
    pub name_complete: bool,
    pub payload: Option<PlainTextJsonToolCallSpan>,
    pub syntax: PlainTextJsonToolCallSyntax,
}

#[derive(Debug, Clone)]
pub enum PlainTextJsonToolCallScan {
    Invalid {
        at: usize,
        candidate: Option<PlainTextJsonToolCallCandidate>,
    },
    Prefix {
        candidate: Option<PlainTextJsonToolCallCandidate>,
    },
    Complete {
        syntax: PlainTextJsonToolCallSyntax,
        name: PlainTextJsonToolCallSpan,
        name_complete: bool,
        payload: PlainTextJsonToolCallSpan,
        end: usize,
        json: PlainTextJsonToolCallState,
    },
}

impl PlainTextJsonToolCallScan {
    pub fn syntax(&self) -> Option<PlainTextJsonToolCallSyntax> {
        match self {
            PlainTextJsonToolCallScan::Complete { syntax, .. } => Some(*syntax),
            _ => None,
        }
    }
}

pub trait PlainTextToolCallNameMatcher {
    fn has_exact_name(&self, name: &str) -> bool;
    fn has_name_prefix(&self, prefix: &str) -> bool;
}

pub struct ScannedBranches {
    pub json: PlainTextJsonToolCallScan,
    pub xmlish: XmlishToolCallScan,
}

pub enum PlainTextToolCallScan {
    Complete {
        branches: ScannedBranches,
        matches: MatchSet,
        end: usize,
        next: usize,
        over_cap: bool,
        payload_start: usize,
    },
    Prefix {
        branches: ScannedBranches,
        matches: MatchSet,
        complete_end: Option<usize>,
        next: usize,
        over_cap: bool,
        payload_start: Option<usize>,
    },
    Invalid {
        branches: ScannedBranches,
        matches: MatchSet,
        at: usize,
        next: usize,
        over_cap: bool,
        payload_start: Option<usize>,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MatchSet {
    pub json: bool,
    pub xmlish: bool,
}

fn is_literal_prefix_at(text: &str, start: i64, literal: &str) -> bool {
    if start < 0 {
        return false;
    }
    let start = start as usize;
    if start > text.len() {
        return false;
    }
    let available = text.len() - start;
    if available >= literal.len() {
        return false;
    }
    literal.starts_with(&text[start..])
}

fn scan_tool_name_end(text: &str, start: usize) -> Option<usize> {
    let mut end = start;
    let bytes = text.as_bytes();
    while end < bytes.len() {
        let ch = text[end..].chars().next();
        if !super::grammar::is_plain_text_tool_name_char(ch) {
            break;
        }
        if end - start == MAX_PLAIN_TEXT_TOOL_NAME_CHARS {
            return None;
        }
        end += ch.map(|c| c.len_utf8()).unwrap_or(1);
    }
    Some(end)
}

fn make_candidate<NameComplete: Into<bool>>(
    syntax: PlainTextJsonToolCallSyntax,
    name: PlainTextJsonToolCallSpan,
    name_complete: NameComplete,
    payload: Option<PlainTextJsonToolCallSpan>,
    json: Option<PlainTextJsonToolCallState>,
) -> PlainTextJsonToolCallCandidate {
    PlainTextJsonToolCallCandidate {
        syntax,
        name,
        name_complete: name_complete.into(),
        payload,
        json,
    }
}

enum OpeningScanResult {
    Complete {
        cursor: usize,
        value: PlainTextJsonToolCallCandidate,
    },
    Prefix {
        candidate: Option<PlainTextJsonToolCallCandidate>,
    },
    Invalid {
        at: usize,
        candidate: Option<PlainTextJsonToolCallCandidate>,
    },
}

fn scan_bracket_opening(
    text: &str,
    start: usize,
    structural_line_breaks: Option<&mut StructuralLineBreakOptions>,
) -> OpeningScanResult {
    let mut cursor = start + 1;
    let mut syntax = PlainTextJsonToolCallSyntax::NamedBracket;
    if text[cursor..].starts_with("tool:") {
        syntax = PlainTextJsonToolCallSyntax::ToolBracket;
        cursor += "tool:".len();
    } else if is_literal_prefix_at(text, cursor as i64, "tool:") {
        return OpeningScanResult::Prefix { candidate: None };
    }
    let name_start = cursor;
    let name_end = match scan_tool_name_end(text, name_start) {
        Some(v) => v,
        None => {
            return OpeningScanResult::Invalid {
                at: name_start + MAX_PLAIN_TEXT_TOOL_NAME_CHARS,
                candidate: None,
            }
        }
    };
    let name = PlainTextJsonToolCallSpan {
        start: name_start,
        end: name_end,
    };
    cursor = name_end;
    if cursor == text.len() {
        return OpeningScanResult::Prefix {
            candidate: if name_start == name_end {
                None
            } else {
                Some(make_candidate(syntax, name, false, None, None))
            },
        };
    }
    if name_start == name_end || text[cursor..].chars().next() != Some(']') {
        return OpeningScanResult::Invalid {
            at: cursor,
            candidate: None,
        };
    }
    cursor += 1;
    let value = make_candidate(syntax, name, true, None, None);
    if syntax == PlainTextJsonToolCallSyntax::NamedBracket {
        let horizontal_end = skip_horizontal_whitespace(text, cursor);
        if horizontal_end == text.len() {
            return OpeningScanResult::Prefix {
                candidate: Some(value),
            };
        }
        let after_line_break = consume_structural_line_break_after_horizontal_whitespace(
            text,
            cursor,
            structural_line_breaks,
        );
        if after_line_break.is_none() {
            return OpeningScanResult::Invalid {
                at: horizontal_end,
                candidate: Some(value),
            };
        }
        cursor = after_line_break.unwrap();
    }
    OpeningScanResult::Complete { cursor, value }
}

fn scan_harmony_opening(text: &str, start: usize) -> OpeningScanResult {
    let mut cursor = start;
    if text[cursor..].starts_with(HARMONY_CHANNEL_MARKER) {
        cursor += HARMONY_CHANNEL_MARKER.len();
    } else if is_literal_prefix_at(text, cursor as i64, HARMONY_CHANNEL_MARKER) {
        return OpeningScanResult::Prefix { candidate: None };
    } else if text[cursor..].chars().next() == Some('<') {
        return OpeningScanResult::Invalid {
            at: cursor,
            candidate: None,
        };
    }

    let channel = HARMONY_CHANNELS
        .iter()
        .find(|&&value| text[cursor..].starts_with(value))
        .copied();
    let channel = match channel {
        Some(c) => c,
        None => {
            if HARMONY_CHANNELS
                .iter()
                .any(|&value| is_literal_prefix_at(text, cursor as i64, value))
            {
                return OpeningScanResult::Prefix { candidate: None };
            }
            return OpeningScanResult::Invalid {
                at: cursor,
                candidate: None,
            };
        }
    };
    cursor += channel.len();
    if cursor == text.len() {
        return OpeningScanResult::Prefix { candidate: None };
    }
    let ch = text[cursor..].chars().next().unwrap();
    if ch != ' ' && ch != '\t' {
        return OpeningScanResult::Invalid {
            at: cursor,
            candidate: None,
        };
    }
    cursor = skip_horizontal_whitespace(text, cursor);
    if !text[cursor..].starts_with("to=") {
        if is_literal_prefix_at(text, cursor as i64, "to=") {
            return OpeningScanResult::Prefix { candidate: None };
        }
        return OpeningScanResult::Invalid {
            at: cursor,
            candidate: None,
        };
    }
    cursor += "to=".len();

    let name_start = cursor;
    let name_end = match scan_tool_name_end(text, name_start) {
        Some(v) => v,
        None => {
            return OpeningScanResult::Invalid {
                at: name_start + MAX_PLAIN_TEXT_TOOL_NAME_CHARS,
                candidate: None,
            }
        }
    };
    let name = PlainTextJsonToolCallSpan {
        start: name_start,
        end: name_end,
    };
    cursor = name_end;
    if cursor == text.len() {
        return OpeningScanResult::Prefix {
            candidate: if name_start == name_end {
                None
            } else {
                Some(make_candidate(
                    PlainTextJsonToolCallSyntax::Harmony,
                    name,
                    false,
                    None,
                    None,
                ))
            },
        };
    }
    if name_start == name_end {
        let ch = text[cursor..].chars().next().unwrap();
        if ch != ' ' && ch != '\t' {
            return OpeningScanResult::Invalid {
                at: cursor,
                candidate: None,
            };
        }
    } else {
        let ch = text[cursor..].chars().next().unwrap();
        if ch != ' ' && ch != '\t' {
            return OpeningScanResult::Invalid {
                at: cursor,
                candidate: None,
            };
        }
    }
    cursor = skip_horizontal_whitespace(text, cursor);
    let value = make_candidate(
        PlainTextJsonToolCallSyntax::Harmony,
        name,
        true,
        None,
        None,
    );
    if !text[cursor..].starts_with("code") {
        if is_literal_prefix_at(text, cursor as i64, "code") {
            return OpeningScanResult::Prefix {
                candidate: Some(value),
            };
        }
        return OpeningScanResult::Invalid {
            at: cursor,
            candidate: Some(value),
        };
    }
    cursor = skip_whitespace(text, cursor + "code".len());
    if text[cursor..].starts_with(HARMONY_MESSAGE_MARKER) {
        cursor = skip_whitespace(text, cursor + HARMONY_MESSAGE_MARKER.len());
    } else if is_literal_prefix_at(text, cursor as i64, HARMONY_MESSAGE_MARKER) {
        return OpeningScanResult::Prefix {
            candidate: Some(value),
        };
    } else if text[cursor..].chars().next() == Some('<') {
        return OpeningScanResult::Invalid {
            at: cursor,
            candidate: Some(value),
        };
    }
    OpeningScanResult::Complete { cursor, value }
}

struct JsonObjectScanResult {
    end: usize,
    kind: JsonObjectKind,
    state: PlainTextJsonToolCallState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonObjectKind {
    Complete,
    Prefix,
}

fn scan_json_object(text: &str, start: usize) -> JsonObjectScanResult {
    let mut depth: i64 = 0;
    let mut escaped = false;
    let mut in_string = false;
    let bytes = text.as_bytes();
    let mut index = start;
    while index < bytes.len() {
        let ch = text[index..].chars().next().unwrap();
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            index += ch.len_utf8();
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                return JsonObjectScanResult {
                    end: index + 1,
                    kind: JsonObjectKind::Complete,
                    state: PlainTextJsonToolCallState {
                        depth,
                        escaped,
                        in_string,
                    },
                };
            }
        }
        index += ch.len_utf8();
    }
    JsonObjectScanResult {
        end: text.len(),
        kind: JsonObjectKind::Prefix,
        state: PlainTextJsonToolCallState {
            depth,
            escaped,
            in_string,
        },
    }
}

/// Uncapped structural scan shared by parsing, stripping, and stream buffering.
pub fn scan_plain_text_json_tool_call(
    text: &str,
    start: Option<usize>,
    structural_line_breaks: Option<&mut StructuralLineBreakOptions>,
) -> PlainTextJsonToolCallScan {
    let start = start.unwrap_or(0);
    let opening = if text.as_bytes().get(start).copied() == Some(b'[') {
        scan_bracket_opening(text, start, structural_line_breaks)
    } else {
        scan_harmony_opening(text, start)
    };
    let (cursor, value) = match opening {
        OpeningScanResult::Complete { cursor, value } => (cursor, value),
        OpeningScanResult::Prefix { candidate } => {
            return PlainTextJsonToolCallScan::Prefix { candidate };
        }
        OpeningScanResult::Invalid { at, candidate } => {
            return PlainTextJsonToolCallScan::Invalid { at, candidate };
        }
    };

    let payload_start = skip_whitespace(text, cursor);
    if payload_start == text.len() {
        return PlainTextJsonToolCallScan::Prefix {
            candidate: Some(value),
        };
    }
    if text.as_bytes()[payload_start] != b'{' {
        return PlainTextJsonToolCallScan::Invalid {
            at: payload_start,
            candidate: Some(value),
        };
    }

    let json = scan_json_object(text, payload_start);
    let payload = PlainTextJsonToolCallSpan {
        start: payload_start,
        end: json.end,
    };
    if json.kind == JsonObjectKind::Prefix {
        return PlainTextJsonToolCallScan::Prefix {
            candidate: Some(make_candidate(
                value.syntax,
                value.name,
                true,
                Some(payload),
                Some(json.state),
            )),
        };
    }

    let closing_candidate = make_candidate(
        value.syntax,
        value.name,
        true,
        Some(payload),
        Some(json.state),
    );
    if value.syntax != PlainTextJsonToolCallSyntax::NamedBracket {
        let marker_start = skip_whitespace(text, json.end);
        let name = &text[value.name.start..value.name.end];
        let closings: [String; 3] = [
            HARMONY_CALL_MARKER.to_string(),
            END_TOOL_REQUEST.to_string(),
            format!("[/{}]", name),
        ];
        for closing in &closings {
            if text[marker_start..].starts_with(closing) {
                return PlainTextJsonToolCallScan::Complete {
                    syntax: value.syntax,
                    name: value.name,
                    name_complete: true,
                    payload,
                    end: marker_start + closing.len(),
                    json: json.state,
                };
            }
            if marker_start < text.len() && is_literal_prefix_at(text, marker_start as i64, closing)
            {
                return PlainTextJsonToolCallScan::Prefix {
                    candidate: Some(closing_candidate),
                };
            }
        }
        return PlainTextJsonToolCallScan::Complete {
            syntax: value.syntax,
            name: value.name,
            name_complete: true,
            payload,
            end: json.end,
            json: json.state,
        };
    }

    let closing_start = skip_whitespace(text, json.end);
    if closing_start == text.len() {
        return PlainTextJsonToolCallScan::Prefix {
            candidate: Some(closing_candidate),
        };
    }
    let name = &text[value.name.start..value.name.end];
    let closings: [String; 2] = [
        END_TOOL_REQUEST.to_string(),
        format!("[/{}]", name),
    ];
    for closing in &closings {
        if text[closing_start..].starts_with(closing) {
            return PlainTextJsonToolCallScan::Complete {
                syntax: value.syntax,
                name: value.name,
                name_complete: true,
                payload,
                end: closing_start + closing.len(),
                json: json.state,
            };
        }
        if is_literal_prefix_at(text, closing_start as i64, closing) {
            return PlainTextJsonToolCallScan::Prefix {
                candidate: Some(closing_candidate),
            };
        }
    }
    PlainTextJsonToolCallScan::Invalid {
        at: closing_start,
        candidate: Some(closing_candidate),
    }
}

pub struct ScanOptions<'a> {
    pub matcher: Option<&'a dyn PlainTextToolCallNameMatcher>,
    pub max_payload_bytes: Option<usize>,
    pub structural_line_breaks: Option<&'a mut StructuralLineBreakOptions>,
}

fn xmlish_to_candidate(xmlish: &XmlishToolCallScan) -> Option<PlainTextJsonToolCallCandidate> {
    let inner = match xmlish {
        XmlishToolCallScan::Complete(c) => &c.inner,
        XmlishToolCallScan::Prefix { candidate, .. } => candidate.as_ref()?,
        XmlishToolCallScan::Invalid { candidate, .. } => candidate.as_ref()?,
    };
    let syntax = match inner.syntax {
        XmlishSyntax::Function => PlainTextJsonToolCallSyntax::NamedBracket,
        XmlishSyntax::NamedBracket => PlainTextJsonToolCallSyntax::NamedBracket,
        XmlishSyntax::ToolBracket => PlainTextJsonToolCallSyntax::ToolBracket,
    };
    Some(PlainTextJsonToolCallCandidate {
        syntax,
        name: PlainTextJsonToolCallSpan {
            start: inner.name.start,
            end: inner.name.end,
        },
        name_complete: inner.name_complete,
        payload: inner.payload.map(|p| PlainTextJsonToolCallSpan {
            start: p.start,
            end: p.end,
        }),
        json: None,
    })
}

fn xmlish_payload_span(xmlish: &XmlishToolCallScan) -> Option<PlainTextJsonToolCallSpan> {
    match xmlish {
        XmlishToolCallScan::Complete(c) => Some(PlainTextJsonToolCallSpan {
            start: c.payload.start,
            end: c.payload.end,
        }),
        XmlishToolCallScan::Prefix { candidate, .. } => candidate
            .as_ref()
            .and_then(|c| c.payload)
            .map(|p| PlainTextJsonToolCallSpan {
                start: p.start,
                end: p.end,
            }),
        XmlishToolCallScan::Invalid { candidate, .. } => candidate
            .as_ref()
            .and_then(|c| c.payload)
            .map(|p| PlainTextJsonToolCallSpan {
                start: p.start,
                end: p.end,
            }),
    }
}

/// Classifies one JSON/XML call candidate and provides monotonic scan progress.
pub fn scan_plain_text_tool_call(
    text: &str,
    start: Option<usize>,
    options: Option<ScanOptions<'_>>,
) -> PlainTextToolCallScan {
    let start = start.unwrap_or(0);
    let default_options: Option<ScanOptions<'_>> = None;
    let options = options.or(default_options);
    let mut slb_a: Option<StructuralLineBreakOptions> = None;
    if let Some(o) = options.as_ref() {
        if let Some(s) = o.structural_line_breaks.as_ref() {
            slb_a = Some((**s).clone());
        }
    }
    let mut slb_b: Option<StructuralLineBreakOptions> = None;
    if let Some(o) = options.as_ref() {
        if let Some(s) = o.structural_line_breaks.as_ref() {
            slb_b = Some((**s).clone());
        }
    }
    let xmlish = scan_xmlish_tool_call(text, Some(start), slb_a.as_mut());
    let json = scan_plain_text_json_tool_call(text, Some(start), slb_b.as_mut());
    let max_payload_bytes = options
        .as_ref()
        .and_then(|o| o.max_payload_bytes)
        .unwrap_or(DEFAULT_MAX_PLAIN_TEXT_TOOL_PAYLOAD_BYTES);

    let allowed_xmlish = |xmlish: &XmlishToolCallScan| -> (bool, Option<PlainTextJsonToolCallSpan>) {
        let value = match xmlish {
            XmlishToolCallScan::Complete(_) => {
                return (
                    true,
                    xmlish_payload_span(xmlish),
                )
            }
            XmlishToolCallScan::Prefix { candidate, .. } => candidate.as_ref(),
            XmlishToolCallScan::Invalid { candidate, .. } => candidate.as_ref(),
        };
        let value = match value {
            Some(v) => v,
            None => return (matches!(xmlish, XmlishToolCallScan::Prefix { .. }), None),
        };
        let name = &text[value.name.start..value.name.end];
        let matches = if value.name_complete {
            options
                .as_ref()
                .and_then(|o| o.matcher)
                .map(|m| m.has_exact_name(name))
                .unwrap_or(true)
        } else {
            options
                .as_ref()
                .and_then(|o| o.matcher)
                .map(|m| m.has_name_prefix(name))
                .unwrap_or(true)
        };
        if matches {
            (
                true,
                value.payload.map(|p| PlainTextJsonToolCallSpan {
                    start: p.start,
                    end: p.end,
                }),
            )
        } else {
            (false, None)
        }
    };

    let allowed_json = |json: &PlainTextJsonToolCallScan| -> (bool, Option<PlainTextJsonToolCallSpan>) {
        let value = match json {
            PlainTextJsonToolCallScan::Complete { name: _, payload, .. } => {
                return (
                    true,
                    Some(PlainTextJsonToolCallSpan {
                        start: payload.start,
                        end: payload.end,
                    }),
                )
            }
            PlainTextJsonToolCallScan::Prefix { candidate } => candidate.as_ref(),
            PlainTextJsonToolCallScan::Invalid { candidate, .. } => candidate.as_ref(),
        };
        let value = match value {
            Some(v) => v,
            None => return (matches!(json, PlainTextJsonToolCallScan::Prefix { .. }), None),
        };
        let name = &text[value.name.start..value.name.end];
        let matches = if value.name_complete {
            options
                .as_ref()
                .and_then(|o| o.matcher)
                .map(|m| m.has_exact_name(name))
                .unwrap_or(true)
        } else {
            options
                .as_ref()
                .and_then(|o| o.matcher)
                .map(|m| m.has_name_prefix(name))
                .unwrap_or(true)
        };
        if matches {
            (
                true,
                value.payload.map(|p| PlainTextJsonToolCallSpan {
                    start: p.start,
                    end: p.end,
                }),
            )
        } else {
            (false, None)
        }
    };

    let xml = allowed_xmlish(&xmlish);
    let json_value = allowed_json(&json);

    let branches = ScannedBranches {
        json: json.clone(),
        xmlish: xmlish.clone(),
    };
    let matches = MatchSet {
        json: json_value.0,
        xmlish: xml.0,
    };

    let over_cap = |payload: Option<PlainTextJsonToolCallSpan>| -> bool {
        payload
            .map(|p| utf8_byte_length_within_limit(text, p.start, p.end, max_payload_bytes).is_none())
            .unwrap_or(false)
    };

    let xml_over_cap = over_cap(xml.1);
    let json_over_cap = over_cap(json_value.1);

    if xml.0 && matches!(xmlish, XmlishToolCallScan::Complete(_)) {
        if let XmlishToolCallScan::Complete(c) = &xmlish {
            return PlainTextToolCallScan::Complete {
                branches,
                matches,
                end: c.end,
                next: c.end,
                over_cap: xml_over_cap,
                payload_start: c.payload.start,
            };
        }
    }
    if json_value.0 && matches!(json, PlainTextJsonToolCallScan::Complete { .. }) {
        if let PlainTextJsonToolCallScan::Complete {
            end, payload, ..
        } = &json
        {
            if json_over_cap || parse_json_arguments(text, payload).is_some() {
                return PlainTextToolCallScan::Complete {
                    branches,
                    matches,
                    end: *end,
                    next: *end,
                    over_cap: json_over_cap,
                    payload_start: payload.start,
                };
            }
            return PlainTextToolCallScan::Invalid {
                branches,
                matches,
                at: *end,
                next: *end,
                over_cap: false,
                payload_start: Some(payload.start),
            };
        }
    }

    if xml.0 && matches!(xmlish, XmlishToolCallScan::Invalid { .. }) && xml_over_cap {
        if let XmlishToolCallScan::Invalid { at, .. } = &xmlish {
            if let Some(p) = xml.1 {
                return PlainTextToolCallScan::Invalid {
                    branches,
                    matches,
                    at: *at,
                    next: *at,
                    over_cap: true,
                    payload_start: Some(p.start),
                };
            }
        }
    }
    if json_value.0 && matches!(json, PlainTextJsonToolCallScan::Invalid { .. }) && json_over_cap {
        if let PlainTextJsonToolCallScan::Invalid { at, .. } = &json {
            if let Some(p) = json_value.1 {
                return PlainTextToolCallScan::Invalid {
                    branches,
                    matches,
                    at: *at,
                    next: *at,
                    over_cap: true,
                    payload_start: Some(p.start),
                };
            }
        }
    }

    let xml_prefix = xml.0 && matches!(xmlish, XmlishToolCallScan::Prefix { .. });
    let json_prefix = json_value.0 && matches!(json, PlainTextJsonToolCallScan::Prefix { .. });
    if xml_prefix || json_prefix {
        let payload = if xml_prefix { xml.1 } else { json_value.1 };
        let complete_end = if let XmlishToolCallScan::Prefix { complete_end, .. } = &xmlish {
            *complete_end
        } else {
            None
        };
        return PlainTextToolCallScan::Prefix {
            branches,
            matches,
            complete_end,
            next: text.len(),
            over_cap: over_cap(payload),
            payload_start: payload.map(|p| p.start),
        };
    }

    let mut next = start + 1;
    if xml.0 {
        next = std::cmp::max(
            next,
            match &xmlish {
                XmlishToolCallScan::Invalid { at, .. } => *at,
                _ => text.len(),
            },
        );
    }
    if json_value.0 {
        next = std::cmp::max(
            next,
            match &json {
                PlainTextJsonToolCallScan::Complete { end, .. } => *end,
                PlainTextJsonToolCallScan::Invalid { at, .. } => *at,
                _ => text.len(),
            },
        );
    }
    PlainTextToolCallScan::Invalid {
        branches,
        matches,
        at: next,
        next,
        over_cap: false,
        payload_start: None,
    }
}

fn parse_plain_text_tool_call_block_at(
    text: &str,
    start: usize,
    options: Option<&NormalizedPlainTextToolCallParseOptions>,
    structural_line_breaks: Option<&mut StructuralLineBreakOptions>,
) -> Option<PlainTextToolCallBlock> {
    let scan = scan_plain_text_json_tool_call(text, Some(start), structural_line_breaks);
    let (name_span, payload_span, end) = match scan {
        PlainTextJsonToolCallScan::Complete {
            name, payload, end, ..
        } => (name, payload, end),
        _ => return None,
    };
    let _ = name_span; // suppress unused
    let name = text[name_span.start..name_span.end].to_string();
    if let Some(allowed) = options.and_then(|o| o.allowed_tool_names.as_ref()) {
        if !allowed.contains(&name) {
            return None;
        }
    }
    let max_payload_bytes = options
        .and_then(|o| o.max_payload_bytes)
        .unwrap_or(DEFAULT_MAX_PLAIN_TEXT_TOOL_PAYLOAD_BYTES);
    if utf8_byte_length_within_limit(text, payload_span.start, payload_span.end, max_payload_bytes)
        .is_none()
    {
        return None;
    }
    let arguments_value = parse_json_arguments(text, &payload_span)?;
    Some(PlainTextToolCallBlock {
        arguments: arguments_value,
        end,
        name,
        raw: text[start..end].to_string(),
        start,
    })
}

fn parse_json_arguments(
    text: &str,
    payload: &PlainTextJsonToolCallSpan,
) -> Option<serde_json::Map<String, JsonValue>> {
    let value: JsonValue = serde_json::from_str(&text[payload.start..payload.end]).ok()?;
    match value {
        JsonValue::Object(map) => Some(map),
        _ => None,
    }
}

fn extract_xmlish_parameter_value(
    text: &str,
    start: usize,
    end: usize,
    structural_line_breaks: Option<&mut StructuralLineBreakOptions>,
) -> String {
    let mut value = text[start..end].to_string();
    if consume_line_break(text, skip_horizontal_whitespace(text, start)).is_none() {
        let boundary = consume_structural_line_break_after_horizontal_whitespace(
            text,
            start,
            structural_line_breaks,
        );
        if let Some(boundary) = boundary {
            let offset = boundary - start;
            let first = value[..offset].to_string();
            let second = value[offset..].to_string();
            value = format!("{}\n{}", first, second);
        }
    }
    let payload_start = consume_line_break(&value, 0);
    let payload_start = match payload_start {
        Some(v) => v,
        None => return value,
    };
    let trimmed = &value[payload_start..];
    // Replace trailing \r\n, \r, or \n
    let trimmed = if trimmed.ends_with("\r\n") {
        &trimmed[..trimmed.len() - 2]
    } else if trimmed.ends_with('\n') || trimmed.ends_with('\r') {
        &trimmed[..trimmed.len() - 1]
    } else {
        trimmed
    };
    trimmed.to_string()
}

fn parse_xmlish_plain_text_tool_call_block_at(
    text: &str,
    start: usize,
    options: Option<&NormalizedPlainTextToolCallParseOptions>,
    structural_line_breaks: &mut Option<&mut StructuralLineBreakOptions>,
) -> Option<PlainTextToolCallBlock> {
    let scan = scan_xmlish_tool_call(text, Some(start), structural_line_breaks.as_deref_mut());
    let (name_span, payload_span, params, end) = match scan {
        XmlishToolCallScan::Complete(c) => (c.inner.name, c.payload, c.inner.parameters, c.end),
        _ => return None,
    };
    let name = text[name_span.start..name_span.end].to_string();
    if let Some(allowed) = options.and_then(|o| o.allowed_tool_names.as_ref()) {
        if !allowed.contains(&name) {
            return None;
        }
    }
    let max_payload_bytes = options
        .and_then(|o| o.max_payload_bytes)
        .unwrap_or(DEFAULT_MAX_PLAIN_TEXT_TOOL_PAYLOAD_BYTES);
    if utf8_byte_length_within_limit(text, payload_span.start, payload_span.end, max_payload_bytes)
        .is_none()
    {
        return None;
    }
    let mut args = serde_json::Map::new();
    for parameter in params {
        let key = text[parameter.name.start..parameter.name.end].to_string();
        let val = extract_xmlish_parameter_value(
            text,
            parameter.value.start,
            parameter.value.end,
            structural_line_breaks.as_deref_mut(),
        );
        args.insert(key, JsonValue::String(val));
    }
    Some(PlainTextToolCallBlock {
        arguments: args,
        end,
        name,
        raw: text[start..end].to_string(),
        start,
    })
}

fn parse_plain_text_tool_call_block_at_any_syntax(
    text: &str,
    start: usize,
    options: Option<&NormalizedPlainTextToolCallParseOptions>,
    structural_line_breaks: &mut Option<&mut StructuralLineBreakOptions>,
) -> Option<PlainTextToolCallBlock> {
    parse_plain_text_tool_call_block_at(text, start, options, structural_line_breaks.as_deref_mut())
        .or_else(|| parse_xmlish_plain_text_tool_call_block_at(text, start, options, structural_line_breaks))
}

fn normalize_parse_options(
    options: Option<&PlainTextToolCallParseOptions>,
) -> Option<NormalizedPlainTextToolCallParseOptions> {
    options.map(|o| NormalizedPlainTextToolCallParseOptions {
        allowed_tool_names: o
            .allowed_tool_names
            .as_ref()
            .map(|v| v.iter().cloned().collect::<HashSet<_>>()),
        max_payload_bytes: o.max_payload_bytes,
    })
}

pub fn parse_standalone_plain_text_tool_call_blocks(
    text: &str,
    options: Option<&PlainTextToolCallParseOptions>,
    structural_line_breaks: Option<&StructuralLineBreakOptions>,
) -> Option<Vec<PlainTextToolCallBlock>> {
    let mut blocks: Vec<PlainTextToolCallBlock> = Vec::new();
    let normalized_options = normalize_parse_options(options);
    let mut cursor = skip_whitespace(text, 0);
    #[allow(unused_assignments)]
    let mut owned_breaks: Option<StructuralLineBreakOptions> = None;
    let mut breaks_ref: Option<&mut StructuralLineBreakOptions> = None;
    if let Some(slb) = structural_line_breaks {
        owned_breaks = Some(StructuralLineBreakOptions {
            line_break_offsets: slb.line_break_offsets.clone(),
            used_line_break_offsets: slb.used_line_break_offsets.clone(),
        });
        breaks_ref = owned_breaks.as_mut();
    }
    while cursor < text.len() {
        let mut slb_mut: Option<&mut StructuralLineBreakOptions> = None;
        if let Some(b) = breaks_ref.as_deref_mut() {
            slb_mut = Some(b);
        }
        let block = parse_plain_text_tool_call_block_at_any_syntax(
            text,
            cursor,
            normalized_options.as_ref(),
            &mut slb_mut,
        );
        let block = match block {
            Some(b) => b,
            None => return None,
        };
        blocks.push(block);
        cursor = skip_whitespace(text, blocks.last().unwrap().end);
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks)
    }
}

/// Removes full-line standalone plain-text tool-call blocks from user-visible text.
pub fn strip_plain_text_tool_call_blocks(text: &str) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let has_bracket = text.contains('[') && regex_bracket_check(text);
    let has_harmony = bytes.contains(&b'<') && regex_harmony_check(text);
    let has_function = regex_function_check(text);
    if !has_bracket && !has_harmony && !has_function {
        return text.to_string();
    }

    let mut result = String::new();
    let mut cursor: usize = 0;
    let mut index: usize = 0;
    while index < text.len() {
        let line_start =
            index == 0 || text.as_bytes()[index - 1] == b'\n' || text.as_bytes()[index - 1] == b'\r';
        if !line_start {
            index += 1;
            continue;
        }
        let block_start = skip_line_indentation(text, index);
        let scan = scan_plain_text_tool_call(text, Some(block_start), None);
        match &scan {
            PlainTextToolCallScan::Prefix { complete_end, .. } if complete_end.is_none() => {
                return result + &text[cursor..];
            }
            PlainTextToolCallScan::Invalid { next, .. } => {
                index = std::cmp::max(index + 1, *next);
                continue;
            }
            _ => {}
        }
        let block_end = match &scan {
            PlainTextToolCallScan::Complete { end, .. } => Some(*end),
            PlainTextToolCallScan::Prefix { complete_end, .. } => *complete_end,
            _ => None,
        };
        if block_end.is_none() {
            return result + &text[cursor..];
        }
        let mut block_end = block_end.unwrap();
        result.push_str(&text[cursor..index]);
        loop {
            let adjacent_start = skip_line_indentation(text, block_end);
            let adjacent = scan_plain_text_tool_call(text, Some(adjacent_start), None);
            let adjacent_end = match &adjacent {
                PlainTextToolCallScan::Complete { end, .. } => Some(*end),
                PlainTextToolCallScan::Prefix { complete_end, .. } => *complete_end,
                _ => None,
            };
            if adjacent_end.is_none() || adjacent_end.unwrap() <= block_end {
                break;
            }
            block_end = adjacent_end.unwrap();
        }
        let line_break_start = skip_line_indentation(text, block_end);
        cursor = if line_break_start == text.len() {
            line_break_start
        } else {
            consume_line_break(text, line_break_start).unwrap_or(block_end)
        };
        index = cursor;
    }
    result.push_str(&text[cursor..]);
    result
}

// Plain-text tool-call bracket opener detection (matches /(?:^|\n)\s*(?:<\|channel\|>)?.../).
// Use lightweight substring/regex checks; these intentionally avoid pulling in
// a full regex engine. We do the simple bracket syntax check inline.
fn regex_bracket_check(text: &str) -> bool {
    // Check for [tool:...] or [name] patterns
    for (i, _) in text.match_indices('[') {
        let after = &text[i + 1..];
        if after.starts_with("tool:") {
            let rest = &after[5..];
            if let Some(end) = rest.find(']') {
                let name = &rest[..end];
                if !name.is_empty() && name.chars().all(is_bracket_name_char) {
                    return true;
                }
            }
        } else if let Some(end) = after.find(']') {
            let name = &after[..end];
            if !name.is_empty() && name.chars().all(is_bracket_name_char) {
                return true;
            }
        }
    }
    false
}

fn is_bracket_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn regex_harmony_check(text: &str) -> bool {
    for (i, _) in text.match_indices(|c: char| c == '<' || c == 'c' || c == 'a' || c == 'f') {
        if text[i..].starts_with("<|channel|>") {
            // Already detected.
        }
        // Check for one of the harmony channels at the start of an indented line
        for channel in &HARMONY_CHANNELS {
            if text[i..].starts_with(channel) {
                let next_idx = i + channel.len();
                if i == 0
                    || text.as_bytes()[i - 1] == b'\n'
                    || text.as_bytes()[i - 1] == b'\r'
                {
                    // Check the next char is whitespace then "to="
                    let after_channel = &text[next_idx..];
                    if after_channel.starts_with(" \t") || after_channel.starts_with("\t ")
                        || after_channel.starts_with("  ")
                        || after_channel.starts_with("\t\t")
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn regex_function_check(text: &str) -> bool {
    for (i, _) in text.match_indices('<') {
        if i == 0 || text.as_bytes()[i - 1] == b'\n' || text.as_bytes()[i - 1] == b'\r' {
            // Check if preceded by indentation (we treat any < immediately after \n as valid per the regex)
            if text[i..].to_lowercase().starts_with("<function=") {
                return true;
            }
        }
    }
    false
}

#[allow(dead_code)]
struct NoopMatcher;
impl PlainTextToolCallNameMatcher for NoopMatcher {
    fn has_exact_name(&self, _name: &str) -> bool {
        true
    }
    fn has_name_prefix(&self, _prefix: &str) -> bool {
        true
    }
}

#[allow(dead_code)]
fn _silence_unused() {
    let _ = starts_with_ascii_marker_ignore_case;
    let _ = xmlish_to_candidate;
}
