//! Model selection, usage, and thinking-level utility helpers.
//! 翻译自 packages/ai/src/model-utils.ts

use llm_core::model_contracts::anthropic::{
    requires_claude_mandatory_adaptive_thinking, resolve_claude_native_thinking_level_map,
};
use llm_core::types::{Model, ModelThinkingLevel, Usage};

/// Calculates and stores model cost fields from token usage and per-million pricing.
pub fn calculate_cost(model: &Model, usage: &mut Usage) -> UsageCost {
    usage.cost.input = (model.cost.input / 1_000_000.0) * (usage.input as f64);
    usage.cost.output = (model.cost.output / 1_000_000.0) * (usage.output as f64);
    usage.cost.cache_read = (model.cost.cache_read / 1_000_000.0) * (usage.cache_read as f64);
    usage.cost.cache_write = (model.cost.cache_write / 1_000_000.0) * (usage.cache_write as f64);
    usage.cost.total = usage.cost.input
        + usage.cost.output
        + usage.cost.cache_read
        + usage.cost.cache_write;
    usage.cost.clone()
}

/// Replaces the catalog estimate when the provider reports an authoritative billed total.
pub fn apply_provider_reported_usage_cost(usage: &mut Usage, reported_cost: &serde_json::Value) {
    if let Some(total) = reported_cost.as_f64() {
        if total.is_finite() && total >= 0.0 {
            usage.cost.total = total;
            usage.cost.total_origin = Some("provider-billed".to_string());
        }
    }
}

/// Re-export the cost struct for convenience.
pub use llm_core::types::UsageCost;

const EXTENDED_THINKING_LEVELS: &[&str] = &[
    "off", "minimal", "low", "medium", "high", "xhigh", "max",
];

fn resolve_thinking_level_map(
    model: &Model,
) -> Option<llm_core::types::ThinkingLevelMap> {
    if model.api == "anthropic-messages" {
        let resolved = resolve_claude_native_thinking_level_map(&anthropic_ref(model));
        if resolved.is_some() {
            return resolved;
        }
    }
    model.thinking_level_map.clone()
}

fn anthropic_ref(
    model: &Model,
) -> llm_core::model_contracts::anthropic::ClaudeModelRef {
    llm_core::model_contracts::anthropic::ClaudeModelRef {
        id: Some(model.id.clone()),
        params: model.params.clone(),
        thinking_level_map: model.thinking_level_map.clone(),
    }
}

/// Returns thinking levels exposed by a reasoning-capable model.
pub fn get_supported_thinking_levels(
    model: &Model,
) -> Vec<ModelThinkingLevel> {
    let mandatory_adaptive_contract = model.api == "anthropic-messages"
        && requires_claude_mandatory_adaptive_thinking(&anthropic_ref(model));
    if !model.reasoning && !mandatory_adaptive_contract {
        return vec!["off".to_string()];
    }
    let thinking_level_map = resolve_thinking_level_map(model);

    EXTENDED_THINKING_LEVELS
        .iter()
        .filter(|level| {
            let key: String = (*level).to_string();
            let mapped = thinking_level_map
                .as_ref()
                .and_then(|m| m.get(&key));
            if mapped.is_none() {
                if **level == "xhigh" || **level == "max" {
                    return false;
                }
                return true;
            }
            // If explicit None (provider opt-out), drop the level.
            if mapped.unwrap().is_none() {
                return false;
            }
            true
        })
        .map(|s| s.to_string())
        .collect()
}

/// Clamps a requested thinking level to the closest supported level for a model.
pub fn clamp_thinking_level(
    model: &Model,
    level: ModelThinkingLevel,
) -> ModelThinkingLevel {
    let available_levels = get_supported_thinking_levels(model);
    if available_levels.contains(&level) {
        return level;
    }

    let requested_index = EXTENDED_THINKING_LEVELS
        .iter()
        .position(|l| *l == level);
    let requested_index = match requested_index {
        Some(i) => i,
        None => return available_levels.first().cloned().unwrap_or_else(|| "off".to_string()),
    };

    // Explicit provider opt-outs are hard caps. Downgrade them before considering
    // stronger levels so unsupported xhigh/max requests cannot increase cost.
    let thinking_level_map = resolve_thinking_level_map(model);
    if (level == "xhigh" || level == "max")
        && thinking_level_map
            .as_ref()
            .and_then(|m| m.get(level.as_str()))
            .map(|v| v.is_none())
            .unwrap_or(false)
    {
        let candidate_levels: Vec<&&str> = EXTENDED_THINKING_LEVELS[..requested_index].iter().collect();
        for candidate in candidate_levels.iter().rev() {
            let candidate_str: &str = *candidate;
            if available_levels.iter().any(|l| l == candidate_str) {
                return candidate_str.to_string();
            }
        }
    }

    // Prefer the next stronger available level, then walk down if the request was above the model cap.
    let next_levels: Vec<&&str> = EXTENDED_THINKING_LEVELS[requested_index..].iter().collect();
    for candidate in next_levels.iter() {
        let candidate_str: &str = *candidate;
        if available_levels.iter().any(|l| l == candidate_str) {
            return candidate_str.to_string();
        }
    }
    let prev_levels: Vec<&&str> = EXTENDED_THINKING_LEVELS[..requested_index].iter().collect();
    for candidate in prev_levels.iter().rev() {
        let candidate_str: &str = *candidate;
        if available_levels.iter().any(|l| l == candidate_str) {
            return candidate_str.to_string();
        }
    }
    available_levels
        .first()
        .cloned()
        .unwrap_or_else(|| "off".to_string())
}

/// Compares model identity by provider and id.
pub fn models_are_equal(
    a: Option<&Model>,
    b: Option<&Model>,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.id == b.id && a.provider == b.provider,
        _ => false,
    }
}