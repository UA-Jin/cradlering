//! Tool result text extraction and redaction.
//! 翻译自 packages/ai/src/providers/tool-result-text.ts

use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

use normalization_core::{is_record, truncate_utf16_safe};

use crate::host::get_ai_transport_host;
use crate::utils::sanitize_unicode::sanitize_unicode;

const PROVIDER_TOOL_RESULT_MAX_CHARS: usize = 8000;

const IMAGE_TOOL_RESULT_TYPES: &[&str] = &["image", "image_url", "input_image"];
const AUDIO_TOOL_RESULT_TYPES: &[&str] = &["audio", "input_audio", "output_audio"];

fn media_only_types() -> HashSet<&'static str> {
    let mut set: HashSet<&'static str> = HashSet::new();
    set.extend(IMAGE_TOOL_RESULT_TYPES.iter().copied());
    set.extend(AUDIO_TOOL_RESULT_TYPES.iter().copied());
    set
}

#[allow(dead_code)]
static INLINE_DATA_URI_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(^|[^A-Za-z0-9_])data:([a-z][a-z0-9.+-]*\/[a-z0-9.+-]+(?:;[a-z0-9.+-]+=[^,;"'\s]+|;base64)*,[^\s"'<>)]+)"#).unwrap()
});

const MIME_KEY_CANDIDATES: &[&str] = &[
    "mimeType",
    "mime_type",
    "mediaType",
    "media_type",
    "contentType",
    "content_type",
];

#[allow(dead_code)]
static TEXTUAL_MIME_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:text\/|application\/(?:json|ld\+json|x-ndjson|xml|javascript|x-www-form-urlencoded)|[^/]+\/[^+]+\+(?:json|xml)$)").unwrap()
});

#[allow(dead_code)]
static OPAQUE_OR_BINARY_FIELD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:blob|buffer|bytes|encrypted_content|encrypted_stdout)$").unwrap());

fn read_mime_type(value: &Value) -> Option<String> {
    if !is_record(value) {
        return None;
    }
    let obj = value.as_object().unwrap();
    for key in MIME_KEY_CANDIDATES {
        if let Some(v) = obj.get(*key) {
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

#[allow(dead_code)]
fn is_binary_mime_type(mime_type: &str) -> bool {
    let normalized = mime_type.split(';').next().unwrap_or("").trim().to_lowercase();
    if normalized.is_empty() {
        return false;
    }
    !TEXTUAL_MIME_PATTERN.is_match(&normalized)
}

#[allow(dead_code)]
fn describe_omitted_value(value: &Value, label: &str) -> String {
    let length = match value {
        Value::String(s) => Some(s.len()),
        _ => serde_json::to_string(value).ok().map(|s| s.len()),
    };
    if let Some(len) = length {
        format!("[{} omitted: {} chars]", label, len)
    } else {
        format!("[{} omitted]", label)
    }
}

#[allow(dead_code)]
fn redact_inline_data_uris(value: &str) -> String {
    INLINE_DATA_URI_PATTERN
        .replace_all(value, |caps: &regex::Captures<'_>| {
            let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let uri = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("{}[inline data URI: {} chars]", prefix, uri.len())
        })
        .into_owned()
}

#[allow(dead_code)]
fn redact_structured_text_value(value: &str) -> String {
    let host = get_ai_transport_host();
    let redacted = host.redact_tool_payload_text(value);
    let trimmed = redacted.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return redacted;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(parsed) => {
            let mut wrapper = Map::new();
            wrapper.insert("structuredTextValue".to_string(), parsed);
            let redacted_value = host.redact_secrets(Value::Object(wrapper));
            if let Some(v) = redacted_value
                .as_object()
                .and_then(|m| m.get("structuredTextValue"))
            {
                serde_json::to_string(v).unwrap_or(redacted)
            } else {
                redacted
            }
        }
        Err(_) => redacted,
    }
}

fn stringify_structured_block(block: &Map<String, Value>) -> Option<String> {
    let host = get_ai_transport_host();
    let mut wrapper = Map::new();
    wrapper.insert("structuredToolResult".to_string(), Value::Object(block.clone()));
    let redacted_value = host.redact_secrets(Value::Object(wrapper));
    let redacted_block = match redacted_value.as_object().and_then(|m| m.get("structuredToolResult")) {
        Some(v) => v.clone(),
        None => Value::Object(block.clone()),
    };

    let serialized = match serde_json::to_string(&redacted_block) {
        Ok(s) => s,
        Err(_) => return None,
    };
    if serialized == "{}" || serialized.is_empty() {
        return None;
    }
    Some(serialized)
}

fn truncate_provider_tool_text(text: &str) -> String {
    if text.len() <= PROVIDER_TOOL_RESULT_MAX_CHARS {
        return text.to_string();
    }
    format!(
        "{}\n…(truncated)…",
        truncate_utf16_safe(text, PROVIDER_TOOL_RESULT_MAX_CHARS as i64)
    )
}

pub fn describe_tool_result_media_placeholder(blocks: &[Value]) -> Option<String> {
    let mut has_image = false;
    let mut has_audio = false;
    for block in blocks {
        if !block.is_object() {
            continue;
        }
        let record = block.as_object().unwrap();
        let block_type = record.get("type").and_then(|v| v.as_str());
        let mime_type = read_mime_type(block);
        let mime_lower = mime_type.as_deref().map(|s| s.to_lowercase());

        if block_type.map(|t| IMAGE_TOOL_RESULT_TYPES.contains(&t)).unwrap_or(false)
            || mime_lower.as_deref().map(|m| m.starts_with("image/")).unwrap_or(false)
        {
            has_image = true;
        }
        if block_type.map(|t| AUDIO_TOOL_RESULT_TYPES.contains(&t)).unwrap_or(false)
            || mime_lower.as_deref().map(|m| m.starts_with("audio/")).unwrap_or(false)
        {
            has_audio = true;
        }
    }

    if has_image && has_audio {
        Some("(see attached media)".to_string())
    } else if has_audio {
        Some("(see attached audio)".to_string())
    } else if has_image {
        Some("(see attached image)".to_string())
    } else {
        None
    }
}

pub fn extract_tool_result_block_text(block: &Value) -> Option<String> {
    if !block.is_object() {
        return None;
    }
    let record = block.as_object().unwrap();
    if let Some(t) = record.get("type").and_then(|v| v.as_str()) {
        if media_only_types().contains(t) {
            return None;
        }
    }
    if record.get("type").and_then(|v| v.as_str()) == Some("text") {
        let text = record
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return if !text.is_empty() {
            Some(sanitize_unicode(text))
        } else {
            None
        };
    }
    stringify_structured_block(record).map(|s| sanitize_unicode(&truncate_provider_tool_text(&s)))
}

pub fn extract_tool_result_text(blocks: &[Value]) -> String {
    let mut explicit_texts: Vec<String> = Vec::new();
    let mut structured_texts: Vec<String> = Vec::new();
    for block in blocks {
        if let Some(text) = extract_tool_result_block_text(block) {
            let record = block.as_object();
            let is_text_block = record
                .and_then(|r| r.get("type"))
                .and_then(|v| v.as_str())
                == Some("text");
            if is_text_block {
                explicit_texts.push(text);
            } else {
                structured_texts.push(text);
            }
        }
    }
    if !explicit_texts.is_empty() {
        sanitize_unicode(&explicit_texts.join("\n"))
    } else {
        sanitize_unicode(&truncate_provider_tool_text(&structured_texts.join("\n")))
    }
}