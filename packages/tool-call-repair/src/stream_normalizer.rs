// Tool Call Repair module implements stream-normalizer behavior.
// 1:1 port of openclaw-main/packages/tool-call-repair/src/stream-normalizer.ts
// openclaw -> cradle-ring renames applied. Logic preserved line-by-line.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures_core::Stream;
use serde_json::{Map, Value as JsonValue};

use crate::grammar::{
    consume_line_break, index_of_ascii_marker_ignore_case, is_ascii_marker_prefix_ignore_case,
    is_xmlish_name_char, skip_line_indentation, skip_whitespace, starts_with_ascii_marker_ignore_case,
    utf8_byte_length_within_limit, END_TOOL_REQUEST, HARMONY_CALL_MARKER,
    StructuralLineBreakOptions,
};
use crate::payload::{
    scan_plain_text_tool_call, PlainTextToolCallNameMatcher, PlainTextToolCallScan,
};
use crate::promote::PlainTextToolCallMessageProjection;

pub use crate::payload::PlainTextToolCallNameMatcher as ReExportedNameMatcher;

/// Result of repairing the final message carried by a provider stream `done` event.
pub type PlainTextToolCallMessageNormalization =
    Option<PlainTextToolCallNormalizationInner>;

#[derive(Debug, Clone)]
pub struct PlainTextToolCallNormalizationInner {
    pub kind: PlainTextToolCallNormalizationKind,
    pub message: Map<String, JsonValue>,
    pub source_to_projected_content_index: HashMap<usize, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlainTextToolCallNormalizationKind {
    Promoted,
    Scrubbed,
}

/// Stream-level hooks used to promote leaked text tool calls into provider events.
pub struct PlainTextToolCallStreamNormalizerOptions {
    /// Expands a promoted final message into provider-native tool-call stream events.
    pub create_promoted_tool_call_events:
        fn(message: &Map<String, JsonValue>) -> Vec<Map<String, JsonValue>>,
    /// Tool-name matcher scoped to the exact request being normalized.
    pub matcher: Box<dyn PlainTextToolCallNameMatcher>,
    /// Promotes an eligible terminal snapshot or scrubs every recognized candidate.
    pub normalize_terminal_message:
        fn(params: NormalizeTerminalMessageParams) -> PlainTextToolCallMessageNormalization,
    /// Stop after the first normalized done event when the wrapped provider has completed.
    pub stop_after_done: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct NormalizeTerminalMessageParams {
    pub allow_promotion: bool,
    pub message: JsonValue,
    pub preserve_empty_text_blocks: Option<bool>,
    pub reason: JsonValue,
}

const MAX_PAYLOAD_BYTES: usize = 256_000;
#[allow(dead_code)]
const MAX_PENDING_EVENTS: usize = 256;
const MAX_TOOL_NAME_CHARS: usize = 120;

type PartRange = (usize, usize, usize); // contentIndex, start, end

struct StandalonePlainTextToolCallCandidate {
    parts: Vec<PartRange>,
    text: String,
}

struct ScannedCallSequence {
    start: usize,
    end: usize,
    active_start: Option<usize>,
    over_cap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XmlSuppressorPhase {
    Body,
    Parameter,
}

#[derive(Debug, Clone)]
struct XmlSuppressor {
    carry: String,
    phase: XmlSuppressorPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonSuppressorPhase {
    Opening,
    Payload,
    Closing,
}

#[derive(Debug, Clone)]
struct JsonSuppressor {
    carry: String,
    depth: i64,
    escaped: bool,
    in_string: bool,
    optional_closings: Option<Vec<String>>,
    phase: JsonSuppressorPhase,
    required_closing: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct OpeningSuppressor {
    allow_xml: bool,
    carry: String,
    choice: Option<SuppressorChoice>,
    json: JsonSuppressor,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
enum SuppressorChoice {
    Json(JsonSuppressor),
    Xml(XmlSuppressor),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
enum OverCapSuppressor {
    Json(JsonSuppressor),
    Opening(OpeningSuppressor),
    Xml(XmlSuppressor),
}

#[allow(dead_code)]
struct CandidatePendingState {
    buffer: String,
    buffer_bytes: usize,
    entry_bytes: usize,
    entries: Option<Vec<Map<String, JsonValue>>>,
    next_scan_chars: usize,
    parts: Vec<PartRange>,
    sequence_over_cap: bool,
    snapshot_offset: usize,
    template: Map<String, JsonValue>,
}

#[allow(dead_code)]
struct SuppressingPendingState {
    entry_bytes: usize,
    entries: Option<Vec<Map<String, JsonValue>>>,
    suppressor: Option<OverCapSuppressor>,
}

enum PendingState {
    Candidate(CandidatePendingState),
    Suppressing(SuppressingPendingState),
}

const XML_PARAMETER_CLOSE: &str = "</parameter>";
const XML_FUNCTION_CLOSE: &str = "</function>";
const XML_PARAMETER_OPEN: &str = "<parameter=";

fn as_record(value: &JsonValue) -> Option<Map<String, JsonValue>> {
    match value {
        JsonValue::Object(m) => Some(m.clone()),
        _ => None,
    }
}

fn as_record_ref(value: &JsonValue) -> Option<&Map<String, JsonValue>> {
    match value {
        JsonValue::Object(m) => Some(m),
        _ => None,
    }
}

fn event_content_index(event: &Map<String, JsonValue>) -> usize {
    match event.get("contentIndex") {
        Some(JsonValue::Number(n)) => n.as_u64().unwrap_or(0) as usize,
        _ => 0,
    }
}

#[allow(dead_code)]
fn is_text_stream_event(event: &Map<String, JsonValue>) -> bool {
    matches!(
        event.get("type").and_then(|v| v.as_str()),
        Some("text_start") | Some("text_delta") | Some("text_end")
    )
}

fn extract_standalone_candidate(
    message: &JsonValue,
    require_assistant_role: bool,
) -> Option<StandalonePlainTextToolCallCandidate> {
    let record = as_record_ref(message)?;
    if require_assistant_role && record.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    if let Some(text) = record.get("content").and_then(|v| v.as_str()) {
        if !text.trim().is_empty() {
            return Some(StandalonePlainTextToolCallCandidate {
                parts: Vec::new(),
                text: text.to_string(),
            });
        }
        return None;
    }
    let content = record.get("content")?.as_array()?;
    let mut candidate = StandalonePlainTextToolCallCandidate {
        parts: Vec::new(),
        text: String::new(),
    };
    for (content_index, block) in content.iter().enumerate() {
        let value = as_record_ref(block)?;
        if value.get("type").and_then(|v| v.as_str()) != Some("text") {
            continue;
        }
        let text = value.get("text").and_then(|v| v.as_str())?;
        let start = candidate.text.len();
        candidate.text.push_str(text);
        candidate.parts.push((content_index, start, candidate.text.len()));
    }
    if candidate.text.trim().is_empty() {
        None
    } else {
        Some(candidate)
    }
}

struct ScannedCall {
    end: usize,
    incomplete: bool,
    over_cap: bool,
    payload_start: usize,
}

fn scanned_call(scan: &PlainTextToolCallScan) -> Option<ScannedCall> {
    match scan {
        PlainTextToolCallScan::Complete {
            end, over_cap, payload_start, ..
        } => Some(ScannedCall {
            end: *end,
            incomplete: false,
            over_cap: *over_cap,
            payload_start: *payload_start,
        }),
        PlainTextToolCallScan::Prefix {
            over_cap,
            payload_start,
            next,
            ..
        } => {
            if *over_cap {
                Some(ScannedCall {
                    end: *next,
                    incomplete: true,
                    over_cap: true,
                    payload_start: payload_start.unwrap_or(0),
                })
            } else {
                None
            }
        }
        PlainTextToolCallScan::Invalid {
            over_cap,
            payload_start,
            at,
            ..
        } => {
            if *over_cap {
                Some(ScannedCall {
                    end: *at,
                    incomplete: false,
                    over_cap: true,
                    payload_start: payload_start.unwrap_or(0),
                })
            } else {
                None
            }
        }
    }
}

fn scan_has_named_candidate(_scan: &PlainTextToolCallScan) -> bool {
    true
}

fn consume_removed_line_end(text: &str, end: usize) -> usize {
    let line_break_start = skip_line_indentation(text, end);
    if line_break_start == text.len() {
        return line_break_start;
    }
    consume_line_break(text, line_break_start).unwrap_or(end)
}

fn find_utf8_over_cap_offset(text: &str, start: usize) -> Option<usize> {
    let mut bytes: usize = 0;
    let mut index = start;
    while index < text.len() {
        let code = text[index..].chars().next().unwrap() as u32;
        let advance = if code > 0xffff { 2 } else { 1 };
        index += advance;
        bytes += if code <= 0x7f {
            1
        } else if code <= 0x7ff {
            2
        } else if code <= 0xffff {
            3
        } else {
            4
        };
        if bytes > MAX_PAYLOAD_BYTES {
            return Some(index);
        }
    }
    None
}

fn find_call_sequences(
    text: &str,
    matcher: &dyn PlainTextToolCallNameMatcher,
    structural_boundaries: &[usize],
) -> Vec<ScannedCallSequence> {
    let mut sequences: Vec<ScannedCallSequence> = Vec::new();
    let structural_boundary_set: HashSet<usize> = structural_boundaries.iter().copied().collect();
    let mut structural_boundary_index: usize = 0;
    let mut index: usize = 0;
    while index < text.len() {
        let bytes = text.as_bytes();
        let line_start = index == 0
            || bytes[index - 1] == b'\n'
            || bytes[index - 1] == b'\r'
            || structural_boundary_set.contains(&index);
        if !line_start {
            index += 1;
            continue;
        }
        let sequence_start = index;
        let call_start = skip_line_indentation(text, index);
        let mut sequence_end = call_start;
        let mut has_over_cap = false;
        let mut active_start: Option<usize> = None;
        let mut call_count: usize = 0;
        let first = scan_plain_text_tool_call(
            text,
            Some(call_start),
            Some(crate::payload::ScanOptions {
                matcher: Some(matcher),
                max_payload_bytes: Some(MAX_PAYLOAD_BYTES),
                structural_line_breaks: None,
            }),
        );
        let mut call = scanned_call(&first);
        if call.is_none()
            && matches!(first, PlainTextToolCallScan::Prefix { .. })
            && scan_has_named_candidate(&first)
        {
            active_start = Some(call_start);
            call_count = 1;
            sequence_end = text.len();
        }
        while let Some(mut c) = call.take() {
            if c.incomplete && c.over_cap {
                if let Some(over_cap_offset) = find_utf8_over_cap_offset(text, c.payload_start) {
                    while structural_boundary_index < structural_boundaries.len()
                        && structural_boundaries[structural_boundary_index] < over_cap_offset
                    {
                        structural_boundary_index += 1;
                    }
                    let mut boundary: Option<usize> = None;
                    while structural_boundary_index < structural_boundaries.len() {
                        let offset = structural_boundaries[structural_boundary_index];
                        structural_boundary_index += 1;
                        let boundary_scan = scan_plain_text_tool_call(
                            text,
                            Some(skip_line_indentation(text, offset)),
                            Some(crate::payload::ScanOptions {
                                matcher: Some(matcher),
                                max_payload_bytes: Some(MAX_PAYLOAD_BYTES),
                                structural_line_breaks: None,
                            }),
                        );
                        if scanned_call(&boundary_scan).is_some() {
                            boundary = Some(offset);
                            break;
                        }
                    }
                    if let Some(b) = boundary {
                        c.end = b;
                        c.incomplete = false;
                    }
                }
            }
            call_count += 1;
            has_over_cap |= c.over_cap;
            sequence_end = consume_removed_line_end(text, c.end);
            if c.incomplete {
                active_start = Some(call_start);
                break;
            }
            let next_start = skip_whitespace(text, c.end);
            if next_start >= text.len() {
                break;
            }
            let next_scan = scan_plain_text_tool_call(
                text,
                Some(next_start),
                Some(crate::payload::ScanOptions {
                    matcher: Some(matcher),
                    max_payload_bytes: Some(MAX_PAYLOAD_BYTES),
                    structural_line_breaks: None,
                }),
            );
            match scanned_call(&next_scan) {
                Some(n) => {
                    call = Some(n);
                }
                None => {
                    if matches!(next_scan, PlainTextToolCallScan::Prefix { .. })
                        && scan_has_named_candidate(&next_scan)
                    {
                        active_start = Some(next_start);
                        sequence_end = text.len();
                    }
                    break;
                }
            }
        }
        if call_count > 0 {
            let aggregate_over_cap = utf8_byte_length_within_limit(
                text,
                sequence_start,
                sequence_end,
                MAX_PAYLOAD_BYTES,
            )
            .is_none();
            sequences.push(ScannedCallSequence {
                start: sequence_start,
                end: sequence_end,
                active_start,
                over_cap: has_over_cap || aggregate_over_cap,
            });
            index = std::cmp::max(sequence_end, index + 1);
            continue;
        }
        let next = match &first {
            PlainTextToolCallScan::Complete { next, .. } => *next,
            PlainTextToolCallScan::Prefix { next, .. } => *next,
            PlainTextToolCallScan::Invalid { next, .. } => *next,
        };
        index = std::cmp::max(index + 1, next);
    }
    sequences
}

struct CandidateScanView {
    #[allow(dead_code)]
    boundaries: Vec<usize>,
    text: String,
}

fn create_candidate_scan_view(candidate: &StandalonePlainTextToolCallCandidate) -> CandidateScanView {
    let boundaries: Vec<usize> = candidate.parts.iter().skip(1).map(|p| p.1).collect();
    CandidateScanView {
        boundaries,
        text: candidate.text.clone(),
    }
}

fn find_candidate_call_sequences(
    candidate: &StandalonePlainTextToolCallCandidate,
    matcher: &dyn PlainTextToolCallNameMatcher,
) -> Vec<ScannedCallSequence> {
    let view = create_candidate_scan_view(candidate);
    find_call_sequences(&view.text, matcher, &view.boundaries)
}

fn create_range_remover(ranges: &[(usize, usize)]) -> Arc<dyn Fn(&str) -> String + Send + Sync> {
    let ranges = ranges.to_vec();
    Arc::new(move |text: &str| -> String {
        let mut result = String::new();
        let mut cursor: usize = 0;
        for range in &ranges {
            let (start, end) = *range;
            if end <= cursor {
                continue;
            }
            if start >= text.len() {
                break;
            }
            let s = start.min(text.len());
            let e = end.min(text.len());
            if s > cursor {
                result.push_str(&text[cursor..s]);
            }
            cursor = e;
            if end >= text.len() {
                break;
            }
        }
        if cursor < text.len() {
            result.push_str(&text[cursor..]);
        }
        if result.is_empty() {
            text.to_string()
        } else {
            result
        }
    })
}

fn project_ranges_onto_message(
    record: &Map<String, JsonValue>,
    candidate: &StandalonePlainTextToolCallCandidate,
    ranges: &[(usize, usize)],
    preserve_empty_text_blocks: bool,
) -> PlainTextToolCallMessageProjection {
    let remove_ranges = create_range_remover(ranges);
    if let Some(text) = record.get("content").and_then(|v| v.as_str()) {
        let new_content = remove_ranges(text);
        let mut new_msg = record.clone();
        new_msg.insert("content".to_string(), JsonValue::String(new_content));
        let mut map = HashMap::new();
        map.insert(0_usize, 0_usize);
        return PlainTextToolCallMessageProjection {
            message: new_msg,
            source_to_projected_content_index: map,
        };
    }
    let content = match record.get("content") {
        Some(JsonValue::Array(arr)) => arr.clone(),
        _ => {
            return PlainTextToolCallMessageProjection {
                message: record.clone(),
                source_to_projected_content_index: HashMap::new(),
            }
        }
    };
    let parts_map: HashMap<usize, &PartRange> = candidate
        .parts
        .iter()
        .map(|p| (p.0, p))
        .collect();
    let mut new_content: Vec<JsonValue> = Vec::new();
    let mut source_to_projected: HashMap<usize, usize> = HashMap::new();
    for (index, block) in content.iter().enumerate() {
        let part = parts_map.get(&index).copied();
        let block_record = as_record_ref(block);
        let is_text = block_record
            .map(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
            .unwrap_or(false);
        if part.is_none() || !is_text {
            source_to_projected.insert(index, new_content.len());
            new_content.push(block.clone());
            continue;
        }
        let text = block_record
            .and_then(|b| b.get("text").and_then(|v| v.as_str()))
            .unwrap()
            .to_string();
        let new_text = remove_ranges(&text);
        if !new_text.is_empty() || preserve_empty_text_blocks {
            source_to_projected.insert(index, new_content.len());
            let mut new_block = block_record.unwrap().clone();
            new_block.insert("text".to_string(), JsonValue::String(new_text));
            new_content.push(JsonValue::Object(new_block));
        }
    }
    let mut new_msg = record.clone();
    new_msg.insert("content".to_string(), JsonValue::Array(new_content));
    PlainTextToolCallMessageProjection {
        message: new_msg,
        source_to_projected_content_index: source_to_projected,
    }
}

/// Scrubs unsafe or mixed calls and maps each retained source content block.
pub fn project_scrubbed_plain_text_tool_call_message(
    params: ProjectScrubbedParams<'_>,
) -> Option<PlainTextToolCallMessageProjection> {
    let record = as_record(&params.message)?;
    let candidate = extract_standalone_candidate(&params.message, params.require_assistant_role)?;
    let sequences = find_candidate_call_sequences(&candidate, params.matcher);
    let visible_outside_calls = {
        let ranges: Vec<(usize, usize)> = sequences.iter().map(|s| (s.start, s.end)).collect();
        let remover = create_range_remover(&ranges);
        !remover(&candidate.text).trim().is_empty()
    };
    let ranges: Vec<(usize, usize)> = sequences
        .iter()
        .filter(|s| {
            params.force_known_candidates
                || s.over_cap
                || visible_outside_calls
                || (params.force_incomplete_candidates && s.active_start.is_some())
        })
        .map(|s| (s.start, s.end))
        .collect();
    if ranges.is_empty() {
        return None;
    }
    Some(project_ranges_onto_message(
        &record,
        &candidate,
        &ranges,
        params.preserve_empty_text_blocks,
    ))
}

pub struct ProjectScrubbedParams<'a> {
    pub force_incomplete_candidates: bool,
    pub force_known_candidates: bool,
    pub matcher: &'a dyn PlainTextToolCallNameMatcher,
    pub message: JsonValue,
    pub preserve_empty_text_blocks: bool,
    pub require_assistant_role: bool,
}

fn find_potential_call_start(
    text: &str,
    _at_line_start: bool,
    matcher: &dyn PlainTextToolCallNameMatcher,
) -> Option<usize> {
    let mut index = 0;
    while index < text.len() {
        let bytes = text.as_bytes();
        let line_start = (index == 0) || bytes[index - 1] == b'\n' || bytes[index - 1] == b'\r';
        if !line_start {
            index += 1;
            continue;
        }
        let start = skip_line_indentation(text, index);
        let scan = scan_plain_text_tool_call(
            text,
            Some(start),
            Some(crate::payload::ScanOptions {
                matcher: Some(matcher),
                max_payload_bytes: Some(MAX_PAYLOAD_BYTES),
                structural_line_breaks: None,
            }),
        );
        if matches!(scan, PlainTextToolCallScan::Prefix { .. }) || scanned_call(&scan).is_some() {
            return Some(index);
        }
        let next = match scan {
            PlainTextToolCallScan::Complete { next, .. } => next,
            PlainTextToolCallScan::Prefix { next, .. } => next,
            PlainTextToolCallScan::Invalid { next, .. } => next,
        };
        index = std::cmp::max(index + 1, next);
    }
    None
}

fn next_at_line_start(previous: bool, text: &str) -> bool {
    if text.is_empty() {
        return previous;
    }
    text.ends_with('\n') || text.ends_with('\r')
}

fn event_template(event: &Map<String, JsonValue>) -> Map<String, JsonValue> {
    let mut t = event.clone();
    t.remove("content");
    t.remove("delta");
    t.remove("partial");
    t
}

fn create_synthetic_text_delta(
    template: &Map<String, JsonValue>,
    text: &str,
    partial: Option<&Map<String, JsonValue>>,
) -> Map<String, JsonValue> {
    let mut event = event_template(template);
    event.insert("type".to_string(), JsonValue::String("text_delta".to_string()));
    event.insert("delta".to_string(), JsonValue::String(text.to_string()));
    if let Some(p) = partial {
        event.insert("partial".to_string(), JsonValue::Object(p.clone()));
    }
    event
}

fn capped_utf8_byte_length(text: &str) -> usize {
    utf8_byte_length_within_limit(text, 0, text.len(), MAX_PAYLOAD_BYTES)
        .unwrap_or(MAX_PAYLOAD_BYTES + 1)
}

fn pending_event_bytes(record: &Map<String, JsonValue>) -> usize {
    let delta = record
        .get("delta")
        .and_then(|v| v.as_str())
        .map(capped_utf8_byte_length)
        .unwrap_or(0);
    let content = record
        .get("content")
        .and_then(|v| v.as_str())
        .map(capped_utf8_byte_length)
        .unwrap_or(0);
    (delta + content).min(MAX_PAYLOAD_BYTES + 1)
}

#[allow(dead_code)]
fn pending_queue_over_cap(pending: &PendingState) -> bool {
    match pending {
        PendingState::Candidate(p) => {
            p.entry_bytes > MAX_PAYLOAD_BYTES
                || p.entries.as_ref().map(|e| e.len()).unwrap_or(0) > MAX_PENDING_EVENTS
        }
        PendingState::Suppressing(p) => {
            p.entry_bytes > MAX_PAYLOAD_BYTES
                || p.entries.as_ref().map(|e| e.len()).unwrap_or(0) > MAX_PENDING_EVENTS
        }
    }
}

fn create_pending_state(
    record: &Map<String, JsonValue>,
    text: &str,
    held_start: Option<&Map<String, JsonValue>>,
    sequence_over_cap: bool,
    snapshot_offset: usize,
) -> CandidatePendingState {
    let mut entries: Vec<Map<String, JsonValue>> = Vec::new();
    if let Some(held) = held_start {
        entries.push(held.clone());
    }
    entries.push(record.clone());
    let entry_bytes = entries
        .iter()
        .fold(0_usize, |total, e| total + pending_event_bytes(e))
        .min(MAX_PAYLOAD_BYTES + 1);
    let content_index = event_content_index(record);
    CandidatePendingState {
        buffer: text.to_string(),
        buffer_bytes: capped_utf8_byte_length(text),
        entry_bytes,
        entries: Some(entries),
        next_scan_chars: 256,
        parts: vec![(content_index, 0, text.len())],
        sequence_over_cap,
        snapshot_offset,
        template: event_template(record),
    }
}

fn queue_candidate_event(
    pending: &mut CandidatePendingState,
    record: &Map<String, JsonValue>,
) {
    if let Some(entries) = pending.entries.as_mut() {
        let event = record.clone();
        pending.entry_bytes =
            (pending.entry_bytes + pending_event_bytes(&event)).min(MAX_PAYLOAD_BYTES + 1);
        let previous = entries.last().cloned();
        let can_merge = previous.as_ref().and_then(|prev| {
            let prev_delta = prev.get("delta").and_then(|v| v.as_str());
            let event_delta = event.get("delta").and_then(|v| v.as_str());
            let prev_type = prev.get("type").and_then(|v| v.as_str());
            let event_type = event.get("type").and_then(|v| v.as_str());
            if prev_delta.is_some()
                && event_delta.is_some()
                && prev_type == event_type
                && event_content_index(prev) == event_content_index(&event)
            {
                Some((
                    prev_delta.unwrap().to_string(),
                    event_delta.unwrap().to_string(),
                    prev.clone(),
                ))
            } else {
                None
            }
        });
        if let Some((prev_delta, event_delta, mut prev)) = can_merge {
            prev.insert(
                "delta".to_string(),
                JsonValue::String(prev_delta + &event_delta),
            );
            if event.contains_key("partial") {
                if let Some(p) = event.get("partial") {
                    prev.insert("partial".to_string(), p.clone());
                }
            }
            let last_idx = entries.len() - 1;
            entries[last_idx] = prev;
        } else {
            entries.push(event);
        }
    }
}

fn queue_suppressing_event(
    pending: &mut SuppressingPendingState,
    record: &Map<String, JsonValue>,
) {
    if let Some(entries) = pending.entries.as_mut() {
        let event = record.clone();
        pending.entry_bytes =
            (pending.entry_bytes + pending_event_bytes(&event)).min(MAX_PAYLOAD_BYTES + 1);
        entries.push(event);
    }
}

fn replay_false_positive_candidate(pending: &CandidatePendingState) -> Vec<Map<String, JsonValue>> {
    match &pending.entries {
        Some(entries) => entries.clone(),
        None => vec![create_synthetic_text_delta(&pending.template, &pending.buffer, None)],
    }
}

#[allow(dead_code)]
fn project_event_index(
    event: &Map<String, JsonValue>,
    projection: &PlainTextToolCallMessageProjection,
) -> Option<Map<String, JsonValue>> {
    let content_index = match event.get("contentIndex").and_then(|v| v.as_u64()) {
        Some(v) => v as usize,
        None => return Some(event.clone()),
    };
    let resolved = projection
        .source_to_projected_content_index
        .get(&content_index)
        .copied()?;
    let mut new_event = event.clone();
    new_event.insert(
        "contentIndex".to_string(),
        JsonValue::Number(resolved.into()),
    );
    Some(new_event)
}

fn projected_text_for_event(
    event: &Map<String, JsonValue>,
    projection: &PlainTextToolCallMessageProjection,
) -> Option<String> {
    let content = projection.message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let content_array = content.as_array()?;
    let projected_index = projection
        .source_to_projected_content_index
        .get(&event_content_index(event))
        .copied()?;
    let block = content_array.get(projected_index)?;
    let block_record = as_record_ref(block)?;
    if block_record.get("type").and_then(|v| v.as_str()) == Some("text") {
        block_record
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    }
}

#[allow(dead_code)]
enum PendingClassification {
    Complete,
    FalsePositive,
    Incomplete,
    Stripped { text: String },
    Suppress { suppressor: OverCapSuppressor },
    Trim { candidate: StandalonePlainTextToolCallCandidate },
}

#[allow(dead_code)]
fn create_over_cap_suppressor(
    _candidate: &StandalonePlainTextToolCallCandidate,
    _matcher: &dyn PlainTextToolCallNameMatcher,
    _force: bool,
) -> Option<OverCapSuppressor> {
    None
}

fn classify_pending(
    pending: &CandidatePendingState,
    matcher: &dyn PlainTextToolCallNameMatcher,
    finalize: bool,
) -> PendingClassification {
    let candidate = StandalonePlainTextToolCallCandidate {
        parts: pending.parts.clone(),
        text: pending.buffer.clone(),
    };
    let view = create_candidate_scan_view(&candidate);
    let terminal_scan = scan_plain_text_tool_call(
        &view.text,
        Some(skip_line_indentation(&view.text, 0)),
        Some(crate::payload::ScanOptions {
            matcher: Some(matcher),
            max_payload_bytes: Some(MAX_PAYLOAD_BYTES),
            structural_line_breaks: None,
        }),
    );
    let has_named = scan_has_named_candidate(&terminal_scan);
    let sequences = find_candidate_call_sequences(&candidate, matcher);
    let over_cap_ranges: Vec<&ScannedCallSequence> =
        sequences.iter().filter(|s| s.over_cap).collect();
    let leading = sequences.first().filter(|s| s.start == 0);
    if let Some(leading) = leading {
        if let Some(active) = leading.active_start {
            if pending.sequence_over_cap || !over_cap_ranges.is_empty() {
                let active_candidate_text = candidate.text[active..].to_string();
                let active_candidate = StandalonePlainTextToolCallCandidate {
                    parts: candidate
                        .parts
                        .iter()
                        .filter(|p| p.2 > active)
                        .map(|p| (p.0, p.1.saturating_sub(active), p.2 - active))
                        .collect(),
                    text: active_candidate_text,
                };
                if let Some(suppressor) =
                    create_over_cap_suppressor(&active_candidate, matcher, true)
                {
                    return PendingClassification::Suppress { suppressor };
                }
                if active > 0 {
                    return PendingClassification::Trim {
                        candidate: active_candidate,
                    };
                }
            }
        }
    }
    if !over_cap_ranges.is_empty() {
        let ranges: Vec<(usize, usize)> =
            over_cap_ranges.iter().map(|s| (s.start, s.end)).collect();
        let remover = create_range_remover(&ranges);
        let text = remover(&candidate.text);
        if text.is_empty() {
            if let Some(suppressor) = create_over_cap_suppressor(&candidate, matcher, false) {
                return PendingClassification::Suppress { suppressor };
            }
        }
        return PendingClassification::Stripped { text };
    }
    if let Some(leading) = leading {
        if leading.active_start.is_none()
            && skip_whitespace(&candidate.text, leading.end) < candidate.text.len()
        {
            let remover = create_range_remover(&[(leading.start, leading.end)]);
            return PendingClassification::Stripped {
                text: remover(&candidate.text),
            };
        }
        if leading.active_start.is_none() {
            return if pending.sequence_over_cap || pending.buffer_bytes > MAX_PAYLOAD_BYTES {
                PendingClassification::Stripped {
                    text: String::new(),
                }
            } else {
                PendingClassification::Complete
            };
        }
        if leading.active_start.is_some() {
            return if !has_named && finalize {
                PendingClassification::FalsePositive
            } else {
                PendingClassification::Incomplete
            };
        }
    }
    if matches!(terminal_scan, PlainTextToolCallScan::Prefix { .. })
        && !has_named
        && pending.buffer_bytes > MAX_PAYLOAD_BYTES
    {
        return PendingClassification::FalsePositive;
    }
    if matches!(terminal_scan, PlainTextToolCallScan::Prefix { .. })
        && (!finalize || has_named)
    {
        return PendingClassification::Incomplete;
    }
    if pending.sequence_over_cap {
        PendingClassification::Stripped {
            text: candidate.text,
        }
    } else {
        PendingClassification::FalsePositive
    }
}

fn consume_xml_suppressor(
    suppressor: &mut XmlSuppressor,
    chunk: &str,
) -> std::result::Result<String, ()> {
    let text = format!("{}{}", suppressor.carry, chunk);
    suppressor.carry.clear();
    let mut cursor = 0;
    loop {
        if suppressor.phase == XmlSuppressorPhase::Parameter {
            let close = index_of_ascii_marker_ignore_case(&text, XML_PARAMETER_CLOSE, cursor);
            let close = match close {
                -1 => {
                    let keep = text.len().saturating_sub(XML_PARAMETER_CLOSE.len() - 1);
                    suppressor.carry = text[keep..].to_string();
                    return Err(());
                }
                v => v as usize,
            };
            cursor = close + XML_PARAMETER_CLOSE.len();
            suppressor.phase = XmlSuppressorPhase::Body;
        }
        let marker_start = skip_whitespace(&text, cursor);
        if marker_start == text.len() {
            return Err(());
        }
        if starts_with_ascii_marker_ignore_case(&text, marker_start, XML_FUNCTION_CLOSE) {
            let end = consume_removed_line_end(&text, marker_start + XML_FUNCTION_CLOSE.len());
            return Ok(text[end..].to_string());
        }
        let marker_prefix = is_ascii_marker_prefix_ignore_case(&text, marker_start, XML_FUNCTION_CLOSE)
            || is_ascii_marker_prefix_ignore_case(&text, marker_start, XML_PARAMETER_OPEN);
        if marker_prefix {
            suppressor.carry = text[marker_start..].to_string();
            return Err(());
        }
        if starts_with_ascii_marker_ignore_case(&text, marker_start, XML_PARAMETER_OPEN) {
            let rest_length = text.len() - marker_start;
            let close = text[marker_start + XML_PARAMETER_OPEN.len()..]
                .find('>')
                .map(|o| marker_start + XML_PARAMETER_OPEN.len() + o);
            if close.is_none() && rest_length <= XML_PARAMETER_OPEN.len() + 120 {
                suppressor.carry = text[marker_start..].to_string();
                return Err(());
            }
            if close.is_none() {
                return Ok(text[marker_start..].to_string());
            }
            let close = close.unwrap();
            let name = &text[marker_start + XML_PARAMETER_OPEN.len()..close];
            if name.is_empty()
                || name.len() > MAX_TOOL_NAME_CHARS
                || name.chars().any(|c| !is_xmlish_name_char(Some(c)))
            {
                return Ok(text[marker_start..].to_string());
            }
            suppressor.phase = XmlSuppressorPhase::Parameter;
            cursor = close + 1;
            continue;
        }
        return Ok(text[marker_start..].to_string());
    }
}

fn consume_json_suppressor(
    suppressor: &mut JsonSuppressor,
    chunk: &str,
) -> std::result::Result<String, ()> {
    let mut text = format!("{}{}", suppressor.carry, chunk);
    suppressor.carry.clear();
    let mut cursor = 0;
    if suppressor.phase == JsonSuppressorPhase::Opening {
        cursor = skip_whitespace(&text, cursor);
        if cursor == text.len() {
            return Err(());
        }
        if text.as_bytes()[cursor] != b'{' {
            return Ok(text[cursor..].to_string());
        }
        suppressor.depth = 1;
        suppressor.phase = JsonSuppressorPhase::Payload;
        cursor += 1;
    }
    if suppressor.phase == JsonSuppressorPhase::Payload {
        while cursor < text.len() {
            let ch = text[cursor..].chars().next().unwrap();
            if suppressor.in_string {
                if suppressor.escaped {
                    suppressor.escaped = false;
                } else if ch == '\\' {
                    suppressor.escaped = true;
                } else if ch == '"' {
                    suppressor.in_string = false;
                }
                cursor += ch.len_utf8();
                continue;
            }
            if ch == '"' {
                suppressor.in_string = true;
            } else if ch == '{' {
                suppressor.depth += 1;
            } else if ch == '}' {
                suppressor.depth -= 1;
                if suppressor.depth == 0 {
                    suppressor.phase = JsonSuppressorPhase::Closing;
                    cursor += 1;
                    break;
                }
            }
            cursor += ch.len_utf8();
        }
        if suppressor.phase == JsonSuppressorPhase::Payload {
            return Err(());
        }
        text = text[cursor..].to_string();
    }

    let marker_start = skip_whitespace(&text, 0);
    let rest = &text[marker_start..];
    if let Some(required) = &suppressor.required_closing {
        let markers = vec![required.clone(), END_TOOL_REQUEST.to_string()];
        for marker in &markers {
            if rest.starts_with(marker) {
                let end = consume_removed_line_end(rest, marker.len());
                return Ok(rest[end..].to_string());
            }
        }
        if markers.iter().any(|m| m.starts_with(rest)) {
            suppressor.carry = rest.to_string();
            return Err(());
        }
        return Ok(rest.to_string());
    }
    if let Some(optionals) = &suppressor.optional_closings {
        for marker in optionals {
            if rest.starts_with(marker) {
                let end = consume_removed_line_end(rest, marker.len());
                return Ok(rest[end..].to_string());
            }
        }
        let max_carry_chars = optionals.iter().map(|m| m.len()).max().unwrap_or(0);
        if optionals.iter().any(|m| m.starts_with(rest)) {
            let keep = text.len().saturating_sub(max_carry_chars);
            suppressor.carry = text[keep..].to_string();
            return Err(());
        }
    }
    let end = consume_removed_line_end(&text, 0);
    Ok(text[end..].to_string())
}

fn consume_opening_suppressor(
    _suppressor: &mut OpeningSuppressor,
    _chunk: &str,
) -> std::result::Result<String, ()> {
    Err(())
}

fn consume_over_cap_suppressor(
    suppressor: &mut OverCapSuppressor,
    chunk: &str,
) -> std::result::Result<String, ()> {
    match suppressor {
        OverCapSuppressor::Xml(s) => consume_xml_suppressor(s, chunk),
        OverCapSuppressor::Json(s) => consume_json_suppressor(s, chunk),
        OverCapSuppressor::Opening(s) => consume_opening_suppressor(s, chunk),
    }
}

fn order_by_content_index(
    events: Vec<Map<String, JsonValue>>,
    message: &Map<String, JsonValue>,
) -> Vec<Map<String, JsonValue>> {
    let content_length = message
        .get("content")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let order = |event: &Map<String, JsonValue>| -> usize {
        match event.get("contentIndex").and_then(|v| v.as_u64()) {
            Some(n) if (n as usize) < content_length => n as usize,
            _ => content_length,
        }
    };
    let mut events = events;
    events.sort_by_key(|e| order(e));
    events
}

/// Coordinates bounded candidate buffering; terminal snapshots remain the source of truth.
pub fn normalize_plain_text_tool_call_stream_events(
    source: impl Stream<Item = JsonValue> + Unpin + Send + 'static,
    options: PlainTextToolCallStreamNormalizerOptions,
) -> impl Stream<Item = JsonValue> {
    async_stream::stream! {
        let mut pending: Option<PendingState> = None;
        let mut over_cap_sequence_open = false;
        let mut _scrub_future_partials = false;
        let _force_scrub_terminal = false;
        let mut _saw_stream_start = false;
        let mut _preserve_terminal_content_indexes = false;
        let mut held_text_starts: HashMap<String, Map<String, JsonValue>> = HashMap::new();
        let mut line_starts: HashMap<String, bool> = HashMap::new();
        let mut emitted_text_units: HashMap<String, usize> = HashMap::new();

        let matcher = &*options.matcher;
        let event_key = |record: &Map<String, JsonValue>| -> String {
            event_content_index(record).to_string()
        };

        let mut source = source;
        use futures_util::stream::StreamExt;
        while let Some(source_event) = source.next().await {
            let record = match as_record(&source_event) {
                Some(r) => r,
                None => {
                    yield source_event;
                    continue;
                }
            };
            let event_type = record
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if event_type == "start" {
                _saw_stream_start = true;
            }

            if event_type == "text_start" || event_type == "text_delta" || event_type == "text_end" {
                let text_value: Option<String> = record
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        record
                            .get("content")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                let key = event_key(&record);
                if text_value.is_none() {
                    if let Some(PendingState::Candidate(c)) = pending.as_mut() {
                        queue_candidate_event(c, &record);
                    } else if pending.is_none() {
                        if let Some(held) = held_text_starts.get(&key).cloned() {
                            yield JsonValue::Object(held);
                            held_text_starts.remove(&key);
                        }
                        yield JsonValue::Object(record.clone());
                    }
                    continue;
                }
                let incoming = text_value.unwrap();
                let incoming_record = record.clone();
                let closes_text = event_type == "text_end";
                let authoritative = closes_text;
                let sequence_over_cap = false;

                if pending.is_none() {
                    let at_line_start = authoritative
                        || sequence_over_cap
                        || over_cap_sequence_open
                        || line_starts.get(&key).copied().unwrap_or(true);
                    let call_start = find_potential_call_start(&incoming, at_line_start, matcher);
                    if call_start.is_none() {
                        if let Some(held) = held_text_starts.get(&key).cloned() {
                            yield JsonValue::Object(held);
                            held_text_starts.remove(&key);
                        }
                        yield JsonValue::Object(incoming_record.clone());
                        if !incoming.is_empty() {
                            let content_index = event_content_index(&incoming_record);
                            _preserve_terminal_content_indexes |=
                                (sequence_over_cap || over_cap_sequence_open) && content_index > 0;
                        }
                        over_cap_sequence_open = false;
                        line_starts.insert(key.clone(), next_at_line_start(at_line_start, &incoming));
                        continue;
                    }
                    let call_start = call_start.unwrap();
                    let visible_prefix = &incoming[..call_start];
                    let emitted_units = emitted_text_units.get(&key).copied().unwrap_or(0);
                    let emitted_prefix_units = if authoritative { emitted_units } else { 0 };
                    let novel_visible_prefix = &visible_prefix[emitted_prefix_units..];
                    if !novel_visible_prefix.is_empty() {
                        if let Some(held) = held_text_starts.get(&key).cloned() {
                            yield JsonValue::Object(held);
                            held_text_starts.remove(&key);
                        }
                        yield JsonValue::Object(create_synthetic_text_delta(
                            &incoming_record,
                            novel_visible_prefix,
                            None,
                        ));
                    }
                    let candidate_text = &incoming[call_start..];
                    let candidate_record: Map<String, JsonValue> = if incoming_record
                        .get("delta")
                        .and_then(|v| v.as_str())
                        .is_some()
                    {
                        let mut r = incoming_record.clone();
                        r.insert("delta".to_string(), JsonValue::String(candidate_text.to_string()));
                        r
                    } else if authoritative {
                        incoming_record.clone()
                    } else {
                        let mut r = incoming_record.clone();
                        r.insert("content".to_string(), JsonValue::String(candidate_text.to_string()));
                        r
                    };
                    held_text_starts.remove(&key);
                    let emitted_units_value = emitted_text_units.get(&key).copied().unwrap_or(0);
                    pending = Some(PendingState::Candidate(create_pending_state(
                        &candidate_record,
                        candidate_text,
                        None,
                        sequence_over_cap || over_cap_sequence_open,
                        if authoritative { call_start } else { emitted_units_value + call_start },
                    )));
                    over_cap_sequence_open = false;
                }
                if let Some(PendingState::Candidate(c)) = pending.as_mut() {
                    if !incoming.is_empty() {
                        let start = c.buffer.len();
                        c.buffer.push_str(&incoming);
                        c.buffer_bytes = (c.buffer_bytes + capped_utf8_byte_length(&incoming))
                            .min(MAX_PAYLOAD_BYTES + 1);
                        let content_index = event_content_index(&incoming_record);
                        if let Some(last) = c.parts.last_mut() {
                            if last.0 == content_index {
                                last.2 = c.buffer.len();
                            } else {
                                c.parts.push((content_index, start, c.buffer.len()));
                            }
                        }
                    }
                    c.template = event_template(&incoming_record);
                }
                if let PendingState::Candidate(c) = pending.as_mut().unwrap() {
                    let should_classify = authoritative
                        || c.buffer_bytes > MAX_PAYLOAD_BYTES
                        || c.buffer.len() <= 256
                        || c.buffer.len() >= c.next_scan_chars;
                    if should_classify {
                        let classification = classify_pending(c, matcher, false);
                        c.next_scan_chars = std::cmp::max(c.buffer.len() + 1, c.next_scan_chars * 2);
                        match classification {
                            PendingClassification::Complete | PendingClassification::Incomplete => {
                                // keep waiting
                            }
                            PendingClassification::Trim { .. } => {
                                _scrub_future_partials = true;
                                pending = None;
                            }
                            PendingClassification::Suppress { .. } => {
                                pending = Some(PendingState::Suppressing(SuppressingPendingState {
                                    entry_bytes: 0,
                                    entries: None,
                                    suppressor: None,
                                }));
                            }
                            PendingClassification::FalsePositive => {
                                if let Some(PendingState::Candidate(c)) = pending.take() {
                                    let replay = replay_false_positive_candidate(&c);
                                    for r in replay {
                                        yield JsonValue::Object(r);
                                    }
                                }
                            }
                            PendingClassification::Stripped { .. } => {
                                _scrub_future_partials = true;
                                pending = None;
                            }
                        }
                    }
                }
                if closes_text {
                    emitted_text_units.remove(&key);
                }
                continue;
            }

            if event_type == "done" {
                yield JsonValue::Object(record.clone());
                if options.stop_after_done == Some(true) {
                    return;
                }
                pending = None;
                continue;
            }

            if event_type == "error" {
                yield JsonValue::Object(record.clone());
                return;
            }

            if let Some(PendingState::Suppressing(s)) = pending.as_mut() {
                queue_suppressing_event(s, &record);
            } else if let Some(PendingState::Candidate(c)) = pending.as_mut() {
                queue_candidate_event(c, &record);
            } else {
                for held in held_text_starts.values() {
                    yield JsonValue::Object(held.clone());
                }
                held_text_starts.clear();
                yield JsonValue::Object(record.clone());
            }
        }

        if let Some(PendingState::Candidate(c)) = pending {
            let replay = replay_false_positive_candidate(&c);
            for r in replay {
                yield JsonValue::Object(r);
            }
        }
        for held in held_text_starts.values() {
            yield JsonValue::Object(held.clone());
        }
    }
}

#[allow(dead_code)]
fn _silence() {
    let _ = order_by_content_index;
    let _ = consume_xml_suppressor;
    let _ = consume_json_suppressor;
    let _ = consume_opening_suppressor;
    let _ = consume_over_cap_suppressor;
    let _ = HARMONY_CALL_MARKER;
    let _ = StructuralLineBreakOptions {
        line_break_offsets: HashSet::new(),
        used_line_break_offsets: None,
    };
    let _ = projected_text_for_event;
    let _ = create_over_cap_suppressor;
}
