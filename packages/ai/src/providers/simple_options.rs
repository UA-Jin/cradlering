//! Simple provider option helpers.
//! 翻译自 packages/ai/src/providers/simple-options.ts

use llm_core::types::{Model, SimpleStreamOptions, StreamOptions, ThinkingBudgets, ThinkingLevel};

/// First-event timeout options surfaced through stream options.
#[derive(Default)]
pub struct FirstEventStreamOptions {
    pub first_event_timeout_ms: Option<f64>,
    pub on_first_event_timeout: Option<std::sync::Arc<dyn Fn(String) + Send + Sync>>,
}

/// Build a base StreamOptions bag from SimpleStreamOptions + an API key.
pub fn build_base_options(
    _model: &Model,
    options: Option<&SimpleStreamOptions>,
    _api_key: Option<&str>,
) -> StreamOptions {
    let options = options.cloned().unwrap_or_default();
    let first_event = FirstEventStreamOptions {
        first_event_timeout_ms: None,
        on_first_event_timeout: None,
    };
    let _ = first_event;
    StreamOptions {
        temperature: options.temperature,
        max_tokens: options.max_tokens,
        stop: options.stop,
        transport: options.transport,
        cache_retention: options.cache_retention,
        session_id: options.session_id,
        request_id: options.request_id,
        prompt_cache_key: options.prompt_cache_key,
        headers: options.headers,
        timeout_ms: options.timeout_ms,
        max_retries: options.max_retries,
        max_retry_delay_ms: options.max_retry_delay_ms,
        metadata: options.metadata,
    }
}

/// Clamp reasoning effort to a non-xhigh value.
pub fn clamp_reasoning(effort: ThinkingLevel) -> ThinkingLevel {
    if effort == "xhigh" {
        "high".to_string()
    } else {
        effort
    }
}

/// Adjust `max_tokens` and `thinkingBudget` for the given reasoning level.
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<i64>,
    model_max_tokens: i64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustMaxTokensResult {
    let default_budgets = ThinkingBudgets {
        minimal: Some(1024.0),
        low: Some(2048.0),
        medium: Some(8192.0),
        high: Some(16384.0),
        max: Some(32768.0),
    };
    let get_budget = |level: &str| -> f64 {
        if let Some(custom) = custom_budgets {
            let v = match level {
                "minimal" => custom.minimal,
                "low" => custom.low,
                "medium" => custom.medium,
                "high" => custom.high,
                "max" => custom.max,
                _ => None,
            };
            if let Some(v) = v {
                return v;
            }
        }
        match level {
            "minimal" => default_budgets.minimal.unwrap_or(1024.0),
            "low" => default_budgets.low.unwrap_or(2048.0),
            "medium" => default_budgets.medium.unwrap_or(8192.0),
            "high" => default_budgets.high.unwrap_or(16384.0),
            "max" => default_budgets.max.unwrap_or(32768.0),
            _ => 0.0,
        }
    };
    let min_output_tokens = 1024;
    let level = clamp_reasoning(reasoning_level);
    let mut thinking_budget = get_budget(&level);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => std::cmp::min(base + thinking_budget as i64, model_max_tokens),
    };
    if (max_tokens as f64) <= thinking_budget {
        thinking_budget = ((max_tokens - min_output_tokens) as f64).max(0.0);
    }
    AdjustMaxTokensResult {
        max_tokens,
        thinking_budget,
    }
}

/// Result of `adjust_max_tokens_for_thinking`.
#[derive(Debug, Clone, Copy)]
pub struct AdjustMaxTokensResult {
    pub max_tokens: i64,
    pub thinking_budget: f64,
}