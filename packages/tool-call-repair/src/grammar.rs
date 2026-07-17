// Tool Call Repair module implements grammar behavior.
// 1:1 port of openclaw-main/packages/tool-call-repair/src/grammar.ts
// openclaw -> cradle-ring renames applied. Logic preserved line-by-line.

use std::collections::HashSet;

/// Legacy marker some models emit after a serialized JSON tool request.
pub const END_TOOL_REQUEST: &str = "[END_TOOL_REQUEST]";
/// Harmony stream marker that introduces the target channel before a tool call.
pub const HARMONY_CHANNEL_MARKER: &str = "<|channel|>";
/// Harmony stream marker that may separate the header from the JSON payload.
pub const HARMONY_MESSAGE_MARKER: &str = "<|message|>";
/// Harmony stream marker that may close a serialized tool-call payload.
pub const HARMONY_CALL_MARKER: &str = "<|call|>";

/// Tool names in bracket/plain-text repairs intentionally match provider-safe ids only.
pub fn is_plain_text_tool_name_char(ch: Option<char>) -> bool {
    match ch {
        Some(c) => c.is_ascii_alphanumeric() || c == '_' || c == '-',
        None => false,
    }
}

/// XML-ish function tags allow namespace punctuation used by some model families.
pub fn is_xmlish_name_char(ch: Option<char>) -> bool {
    match ch {
        Some(c) => {
            c.is_ascii_alphanumeric()
                || c == '_'
                || c == '.'
                || c == ':'
                || c == '-'
        }
        None => false,
    }
}

/// Skips spaces and tabs only, preserving line boundaries for grammar decisions.
pub fn skip_horizontal_whitespace(text: &str, start: usize) -> usize {
    let mut index = start;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        let b = bytes[index];
        if b == b' ' || b == b'\t' {
            index += 1;
        } else {
            break;
        }
    }
    index
}

/// Skips indentation whitespace without crossing the current line boundary.
pub fn skip_line_indentation(text: &str, start: usize) -> usize {
    let mut index = start;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        let b = bytes[index];
        // JavaScript regex /[^\S\r\n]/u - whitespace that is not \r or \n
        if b == b' ' || b == b'\t' || b == 0x0b || b == 0x0c {
            index += 1;
        } else {
            break;
        }
    }
    index
}

/// Skips all JavaScript whitespace when line structure is no longer meaningful.
pub fn skip_whitespace(text: &str, start: usize) -> usize {
    let mut index = start;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        let b = bytes[index];
        // /\\s/ matches space, tab, LF, VT, FF, CR, and Unicode whitespace.
        // For our purposes (the JS code uses /\s/ on a single char), we approximate
        // with ASCII + the listed whitespace bytes.
        if b == b' '
            || b == b'\t'
            || b == b'\n'
            || b == b'\r'
            || b == 0x0b
            || b == 0x0c
            || b == 0xa0
        {
            index += 1;
        } else {
            // Continue to detect non-ASCII Unicode whitespace as best we can.
            // In Rust, char::is_whitespace works at char boundaries; we check via str slicing.
            if let Some(c) = text[index..].chars().next() {
                if c.is_whitespace() {
                    index += c.len_utf8();
                    continue;
                }
            }
            break;
        }
    }
    index
}

/// Consumes either Unix or Windows line endings and returns the first offset after them.
pub fn consume_line_break(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if start >= bytes.len() {
        return None;
    }
    if bytes[start] == b'\r' {
        return Some(if bytes.get(start + 1).copied() == Some(b'\n') {
            start + 2
        } else {
            start + 1
        });
    }
    if bytes[start] == b'\n' {
        return Some(start + 1);
    }
    None
}

#[derive(Debug, Clone)]
pub struct StructuralLineBreakOptions {
    pub line_break_offsets: HashSet<usize>,
    pub used_line_break_offsets: Option<HashSet<usize>>,
}

pub fn consume_structural_line_break_after_horizontal_whitespace(
    text: &str,
    start: usize,
    mut options: Option<&mut StructuralLineBreakOptions>,
) -> Option<usize> {
    let right = skip_horizontal_whitespace(text, start);
    let actual = consume_line_break(text, right);
    if actual.is_some() {
        return actual;
    }
    let mut offset = start;
    while offset <= right {
        let contains = options
            .as_ref()
            .map(|o| o.line_break_offsets.contains(&offset))
            .unwrap_or(false);
        if contains {
            if let Some(opts_mut) = options.as_mut() {
                if let Some(used_mut) = opts_mut.used_line_break_offsets.as_mut() {
                    used_mut.insert(offset);
                }
            }
            return Some(offset);
        }
        offset += 1;
    }
    None
}

/// Returns the encoded byte length when a source span stays within its serialized limit.
pub fn utf8_byte_length_within_limit(
    text: &str,
    start: usize,
    end: usize,
    max_bytes: usize,
) -> Option<usize> {
    if end - start > max_bytes {
        return None;
    }
    let byte_length = text[start..end].as_bytes().len();
    if byte_length <= max_bytes {
        Some(byte_length)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
pub struct XmlishToolCallSpan {
    pub end: usize,
    pub start: usize,
}

#[derive(Debug, Clone)]
pub struct XmlishToolCallParameterSpan {
    pub name: XmlishToolCallSpan,
    pub value: XmlishToolCallSpan,
}

#[derive(Debug, Clone)]
pub struct XmlishToolCallCandidate {
    pub active_parameter_open_end: Option<usize>,
    pub name: XmlishToolCallSpan,
    pub name_complete: bool,
    pub parameters: Vec<XmlishToolCallParameterSpan>,
    pub payload: Option<XmlishToolCallSpan>,
    pub syntax: XmlishSyntax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlishSyntax {
    Function,
    NamedBracket,
    ToolBracket,
}

#[derive(Debug, Clone)]
pub enum XmlishToolCallScan {
    Invalid {
        at: usize,
        candidate: Option<XmlishToolCallCandidate>,
    },
    /// `complete_end` is safe for static stripping only; the prefix remains non-executable.
    Prefix {
        candidate: Option<XmlishToolCallCandidate>,
        complete_end: Option<usize>,
    },
    Complete(XmlishToolCallCandidateComplete),
}

#[derive(Debug, Clone)]
pub struct XmlishToolCallCandidateComplete {
    pub inner: XmlishToolCallCandidate,
    pub end: usize,
    pub payload: XmlishToolCallSpan,
}

const FUNCTION_OPEN: &str = "<function=";
const FUNCTION_CLOSE: &str = "</function>";
const PARAMETER_OPEN: &str = "<parameter=";
const PARAMETER_CLOSE: &str = "</parameter>";

pub fn starts_with_ascii_marker_ignore_case(text: &str, cursor: usize, marker: &str) -> bool {
    if cursor + marker.len() > text.len() {
        return false;
    }
    let text_slice = &text[cursor..cursor + marker.len()];
    text_slice.to_lowercase() == marker.to_lowercase()
}

pub fn is_ascii_marker_prefix_ignore_case(text: &str, cursor: usize, marker: &str) -> bool {
    if cursor >= text.len() {
        return marker.is_empty();
    }
    let end = std::cmp::min(cursor + marker.len(), text.len());
    let rest = &text[cursor..end].to_lowercase();
    rest.len() < marker.len() && marker.to_lowercase().starts_with(&*rest)
}

pub fn index_of_ascii_marker_ignore_case(text: &str, marker: &str, start: usize) -> i64 {
    let mut cursor: i64 = text[start..]
        .find('<')
        .map(|o| start as i64 + o as i64)
        .unwrap_or(-1);
    while cursor != -1 {
        if starts_with_ascii_marker_ignore_case(text, cursor as usize, marker) {
            return cursor;
        }
        let next_start = (cursor + 1) as usize;
        match text[next_start..].find('<') {
            Some(o) => cursor = next_start as i64 + o as i64,
            None => return -1,
        }
    }
    -1
}

fn char_at(text: &str, index: usize) -> Option<char> {
    text[index..].chars().next()
}

fn name_complete_with_candidate(
    syntax: XmlishSyntax,
    name: XmlishToolCallSpan,
    name_complete: bool,
    parameters: Vec<XmlishToolCallParameterSpan>,
) -> XmlishToolCallCandidate {
    XmlishToolCallCandidate {
        active_parameter_open_end: None,
        name,
        name_complete,
        parameters,
        payload: None,
        syntax,
    }
}

/// Uncapped structural scan shared by parsing, stripping, and stream buffering.
pub fn scan_xmlish_tool_call(
    text: &str,
    start: Option<usize>,
    structural_line_breaks: Option<&mut StructuralLineBreakOptions>,
) -> XmlishToolCallScan {
    let start = start.unwrap_or(0);
    let mut cursor = start;
    let mut syntax: XmlishSyntax;
    let mut name: XmlishToolCallSpan;

    if char_at(text, cursor) == Some('<') {
        if !starts_with_ascii_marker_ignore_case(text, cursor, FUNCTION_OPEN)
            && !is_ascii_marker_prefix_ignore_case(text, cursor, FUNCTION_OPEN)
        {
            return XmlishToolCallScan::Invalid {
                at: start,
                candidate: None,
            };
        }
        if text.len() - cursor < FUNCTION_OPEN.len() {
            return XmlishToolCallScan::Prefix {
                candidate: None,
                complete_end: None,
            };
        }
        cursor += FUNCTION_OPEN.len();
        let name_start = cursor;
        while is_xmlish_name_char(char_at(text, cursor)) && cursor - name_start < 121 {
            cursor += 1;
        }
        name = XmlishToolCallSpan {
            start: name_start,
            end: cursor,
        };
        syntax = XmlishSyntax::Function;
        if cursor - name_start > 120 {
            return XmlishToolCallScan::Invalid {
                at: cursor,
                candidate: None,
            };
        }
        if cursor == text.len() {
            return XmlishToolCallScan::Prefix {
                candidate: Some(name_complete_with_candidate(
                    syntax,
                    name,
                    false,
                    Vec::new(),
                )),
                complete_end: None,
            };
        }
        if cursor == name_start || char_at(text, cursor) != Some('>') {
            return XmlishToolCallScan::Invalid {
                at: cursor,
                candidate: None,
            };
        }
        cursor += 1;
    } else if char_at(text, cursor) == Some('[') {
        cursor += 1;
        let first_name_start = cursor;
        while is_plain_text_tool_name_char(char_at(text, cursor)) && cursor - first_name_start < 121
        {
            cursor += 1;
        }
        if cursor - first_name_start > 120 {
            return XmlishToolCallScan::Invalid {
                at: cursor,
                candidate: None,
            };
        }
        let first_name = &text[first_name_start..cursor];
        if cursor == text.len() && "tool".starts_with(first_name) {
            return XmlishToolCallScan::Prefix {
                candidate: None,
                complete_end: None,
            };
        }
        syntax = XmlishSyntax::NamedBracket;
        name = XmlishToolCallSpan {
            start: first_name_start,
            end: cursor,
        };
        if char_at(text, cursor) == Some(':') && first_name == "tool" {
            syntax = XmlishSyntax::ToolBracket;
            cursor += 1;
            let name_start = cursor;
            while is_plain_text_tool_name_char(char_at(text, cursor)) && cursor - name_start < 121
            {
                cursor += 1;
            }
            name = XmlishToolCallSpan {
                start: name_start,
                end: cursor,
            };
            if cursor - name_start > 120 {
                return XmlishToolCallScan::Invalid {
                    at: cursor,
                    candidate: None,
                };
            }
        }
        if cursor == text.len() {
            return XmlishToolCallScan::Prefix {
                candidate: Some(name_complete_with_candidate(
                    syntax,
                    name,
                    false,
                    Vec::new(),
                )),
                complete_end: None,
            };
        }
        if name.start == name.end || char_at(text, cursor) != Some(']') {
            return XmlishToolCallScan::Invalid {
                at: cursor,
                candidate: None,
            };
        }
        cursor += 1;
        if syntax == XmlishSyntax::NamedBracket {
            if cursor == text.len() {
                return XmlishToolCallScan::Prefix {
                    candidate: Some(name_complete_with_candidate(
                        syntax,
                        name,
                        true,
                        Vec::new(),
                    )),
                    complete_end: None,
                };
            }
            let after_line_break = consume_structural_line_break_after_horizontal_whitespace(
                text,
                cursor,
                structural_line_breaks,
            );
            if after_line_break.is_none() {
                return XmlishToolCallScan::Invalid {
                    at: cursor,
                    candidate: None,
                };
            }
            cursor = after_line_break.unwrap();
        }
    } else {
        return XmlishToolCallScan::Invalid {
            at: start,
            candidate: None,
        };
    }

    let body_start = cursor;
    let mut parameters: Vec<XmlishToolCallParameterSpan> = Vec::new();
    let mut last_parameter_end: Option<usize> = None;
    loop {
        let marker_start = skip_whitespace(text, cursor);
        let make_candidate_at = |payload_end: usize,
                                 active_parameter_open_end: Option<usize>|
         -> XmlishToolCallCandidate {
            XmlishToolCallCandidate {
                active_parameter_open_end,
                name,
                name_complete: true,
                parameters: parameters.clone(),
                payload: Some(XmlishToolCallSpan {
                    start: body_start,
                    end: payload_end,
                }),
                syntax,
            }
        };
        let make_complete_at = |payload_end: usize, end: Option<usize>| {
            let end = end.unwrap_or(payload_end);
            let cand = make_candidate_at(payload_end, None);
            XmlishToolCallScan::Complete(XmlishToolCallCandidateComplete {
                inner: cand,
                end,
                payload: XmlishToolCallSpan {
                    start: body_start,
                    end: payload_end,
                },
            })
        };
        let make_prefix_at = |payload_end: usize,
                              active_parameter_open_end: Option<usize>|
         -> XmlishToolCallScan {
            let cand = make_candidate_at(payload_end, active_parameter_open_end);
            let complete_end = if syntax == XmlishSyntax::ToolBracket {
                last_parameter_end
            } else {
                None
            };
            XmlishToolCallScan::Prefix {
                candidate: Some(cand),
                complete_end,
            }
        };
        if marker_start == text.len() {
            return if syntax == XmlishSyntax::ToolBracket && last_parameter_end.is_some() {
                make_complete_at(last_parameter_end.unwrap(), None)
            } else {
                make_prefix_at(text.len(), None)
            };
        }
        if starts_with_ascii_marker_ignore_case(text, marker_start, FUNCTION_CLOSE) {
            return if syntax != XmlishSyntax::Function && parameters.is_empty() {
                XmlishToolCallScan::Invalid {
                    at: marker_start,
                    candidate: Some(make_candidate_at(marker_start, None)),
                }
            } else {
                make_complete_at(marker_start, Some(marker_start + FUNCTION_CLOSE.len()))
            };
        }
        if is_ascii_marker_prefix_ignore_case(text, marker_start, FUNCTION_CLOSE) {
            return make_prefix_at(marker_start, None);
        }
        if starts_with_ascii_marker_ignore_case(text, marker_start, PARAMETER_OPEN) {
            let name_start = marker_start + PARAMETER_OPEN.len();
            let mut name_end = name_start;
            while is_xmlish_name_char(char_at(text, name_end)) && name_end - name_start < 121 {
                name_end += 1;
            }
            if name_end - name_start > 120 {
                return XmlishToolCallScan::Invalid {
                    at: marker_start,
                    candidate: Some(make_candidate_at(marker_start, None)),
                };
            }
            if name_end == text.len() {
                return make_prefix_at(marker_start, None);
            }
            if name_end == name_start || char_at(text, name_end) != Some('>') {
                return XmlishToolCallScan::Invalid {
                    at: marker_start,
                    candidate: Some(make_candidate_at(marker_start, None)),
                };
            }
            let value_start = name_end + 1;
            let close_start = index_of_ascii_marker_ignore_case(text, PARAMETER_CLOSE, value_start);
            if close_start == -1 {
                return make_prefix_at(text.len(), Some(value_start));
            }
            let end = close_start as usize + PARAMETER_CLOSE.len();
            parameters.push(XmlishToolCallParameterSpan {
                name: XmlishToolCallSpan {
                    start: name_start,
                    end: name_end,
                },
                value: XmlishToolCallSpan {
                    start: value_start,
                    end: close_start as usize,
                },
            });
            cursor = end;
            last_parameter_end = Some(end);
            continue;
        }
        if is_ascii_marker_prefix_ignore_case(text, marker_start, PARAMETER_OPEN) {
            return make_prefix_at(marker_start, None);
        }
        if syntax == XmlishSyntax::ToolBracket && last_parameter_end.is_some() {
            return make_complete_at(last_parameter_end.unwrap(), None);
        }
        return XmlishToolCallScan::Invalid {
            at: marker_start,
            candidate: Some(make_candidate_at(marker_start, None)),
        };
    }
}

// Silence unused import warnings
#[allow(dead_code)]
fn _silence_unused() {
    let _ = std::marker::PhantomData::<HashSet<usize>>;
}
