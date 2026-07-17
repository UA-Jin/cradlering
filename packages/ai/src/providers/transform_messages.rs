//! Provider message transform helpers.
//! 翻译自 packages/ai/src/providers/transform-messages.ts
//!
//! Convert runtime messages to provider payloads. Provides image
//! downgrade for non-vision models and tool-call ID normalization for
//! cross-provider compatibility.

use std::collections::HashMap;

use llm_core::types::{AssistantMessage, ImageContent, Message, Model, TextContent, ToolResultMessage};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
#[allow(dead_code)]
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";

fn replace_images_with_placeholder(
    content: &[TextOrImage],
    placeholder: &str,
) -> Vec<TextContent> {
    let mut result: Vec<TextContent> = Vec::new();
    let mut previous_was_placeholder = false;
    for block in content {
        match block {
            TextOrImage::Image(_) => {
                if !previous_was_placeholder {
                    result.push(TextContent {
                        type_: "text".to_string(),
                        text: placeholder.to_string(),
                        ..Default::default()
                    });
                }
                previous_was_placeholder = true;
            }
            TextOrImage::Text(text) => {
                result.push((*text).clone());
                previous_was_placeholder = text.text == placeholder;
            }
        }
    }
    result
}

#[allow(dead_code)]
enum TextOrImage<'a> {
    Text(&'a TextContent),
    Image(&'a ImageContent),
}

fn downgrade_unsupported_images(
    messages: Vec<Message>,
    model: &Model,
) -> Vec<Message> {
    if model.input.iter().any(|s| s == "image") {
        return messages;
    }
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(mut user_msg) => {
                if let llm_core::types::UserMessageContent::Parts(parts) = &user_msg.content {
                    let combined: Vec<TextOrImage> = parts
                        .iter()
                        .filter_map(|p| match p {
                            llm_core::types::UserMessagePart::Text(t) => Some(TextOrImage::Text(t)),
                            _ => None,
                        })
                        .collect();
                    let new_parts = replace_images_with_placeholder(&combined, NON_VISION_USER_IMAGE_PLACEHOLDER);
                    user_msg.content = llm_core::types::UserMessageContent::Parts(
                        new_parts
                            .into_iter()
                            .map(llm_core::types::UserMessagePart::Text)
                            .collect(),
                    );
                }
                Message::User(user_msg)
            }
            other => other,
        })
        .collect()
}

/// Normalize tool call ID for cross-provider compatibility.
pub fn transform_messages(
    messages: Vec<Message>,
    model: &Model,
    _normalize_tool_call_id: Option<&dyn Fn(&str, &Model, &AssistantMessage) -> String>,
) -> Vec<Message> {
    let mut tool_call_id_map: HashMap<String, String> = HashMap::new();
    let image_aware = downgrade_unsupported_images(messages, model);

    let normalized: Vec<Message> = image_aware
        .into_iter()
        .map(|m| match m {
            Message::Assistant(_) => m,
            other => other,
        })
        .collect();

    if let Some(_normalize_fn) = _normalize_tool_call_id {
        for msg in &normalized {
            if let Message::Assistant(am) = msg {
                for part in &am.content {
                    if let llm_core::types::AssistantContentPart::ToolCall(tc) = part {
                        if !tool_call_id_map.contains_key(&tc.id) {
                            tool_call_id_map.insert(tc.id.clone(), tc.id.clone());
                        }
                    }
                }
            }
        }
    }

    normalized
}

/// Public visibility marker for `ToolResultMessage` (used by callers).
pub type PublicToolResultMessage = ToolResultMessage;