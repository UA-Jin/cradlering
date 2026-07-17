// Agent Core module implements reasoning behavior.
// 翻译自 packages/agent-core/src/reasoning.ts

use llm_core::model_contracts::anthropic::{
    resolve_claude_fable_5_model_identity, resolve_claude_sonnet_5_model_identity,
};
use llm_core::types::{Model, SimpleStreamOptions};

const ENABLED_THINKING_LEVELS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];

fn is_enabled_thinking_level(value: &str) -> bool {
    ENABLED_THINKING_LEVELS.contains(&value)
}

pub fn resolve_agent_reasoning_option(model: &Model, thinking_level: &str) -> Option<String> {
    if thinking_level != "off" {
        return Some(thinking_level.to_string());
    }
    let off_fallback: Option<String> = model
        .thinking_level_map
        .as_ref()
        .and_then(|m| m.get("off").and_then(|v| v.clone()))
        .or_else(|| {
            if (model.api == "anthropic-messages" || model.api == "bedrock-converse-stream")
                && resolve_claude_fable_5_model_identity(&llm_core::model_contracts::anthropic::ClaudeModelRef {
                    id: Some(model.id.clone()),
                    params: model.params.clone(),
                    thinking_level_map: model.thinking_level_map.clone(),
                }).is_some()
            {
                Some("low".to_string())
            } else {
                None
            }
        });
    if let Some(ref fb) = off_fallback {
        if is_enabled_thinking_level(fb) {
            return Some(fb.clone());
        }
    }
    if model.api == "anthropic-messages" && resolve_claude_sonnet_5_model_identity(&llm_core::model_contracts::anthropic::ClaudeModelRef {
        id: Some(model.id.clone()),
        params: model.params.clone(),
        thinking_level_map: model.thinking_level_map.clone(),
    }).is_some() {
        return Some("off".to_string());
    }
    None
}

#[allow(dead_code)]
fn _force_use(_opts: &SimpleStreamOptions) {}