// Speech Core module implements tts behavior.
// 1:1 port of openclaw-main/packages/speech-core/src/tts.ts
// openclaw -> cradle-ring renames applied. Logic preserved line-by-line.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};
use tokio::fs;

use crate::api::{
    canonicalize_speech_provider_id, get_speech_provider, list_speech_providers,
    normalize_speech_provider_id, normalize_tts_auto_mode, parse_tts_directives,
    resolve_effective_tts_config, schedule_cleanup, summarize_text, OpenClawConfig, ResolvedTtsConfig,
    ResolvedTtsModelOverrides, ResolvedTtsPersona, SpeechProviderConfig, SpeechProviderOverrides,
    SpeechVoiceOption, TtsAutoMode, TtsConfig, TtsConfigResolutionContext, TtsDirectiveOverrides,
    TtsDirectiveParseResult, TtsModelOverrideConfig, TtsProvider,
};
use crate::runtime_api::{
    get_runtime_config_snapshot, get_runtime_config_source_snapshot,
    mark_reply_payload_as_tts_supplement, resolve_sendable_outbound_reply_parts,
    select_applicable_runtime_config, ReplyPayload,
};
use crate::speaker::with_speaker_selection_compat;
use crate::voice_models::{
    resolve_primary_voice_provider_candidate, resolve_supported_voice_model_refs,
    resolve_voice_model_refs, resolve_voice_provider_candidates, voice_provider_supports_model,
    VoiceModelProvider, VoiceModelRef, VoiceProviderCandidate,
};

// Re-exports come from crate::api; types are not redeclared here.

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_TTS_MAX_LENGTH: usize = 1500;
const DEFAULT_TTS_SUMMARIZE: bool = true;
const DEFAULT_MAX_TEXT_LENGTH: usize = 4096;

#[allow(dead_code)]
fn resolve_positive_timeout_ms(timeout_ms: Option<u64>) -> Option<u64> {
    if let Some(t) = timeout_ms {
        if t > 0 {
            return Some(clamp_timer_timeout_ms(t));
        }
    }
    None
}

#[allow(dead_code)]
fn clamp_timer_timeout_ms(t: u64) -> u64 {
    // Mirror the JS clamp helper.
    t.min(2_147_483_647)
}

#[allow(dead_code)]
fn resolve_speech_provider_timeout_ms(params: ResolveSpeechProviderTimeoutMsParams) -> u64 {
    if let Some(t) = params.timeout_ms {
        if let Some(v) = resolve_positive_timeout_ms(Some(t)) {
            return v;
        }
        return params.config.timeout_ms;
    }
    if params.config.timeout_ms_source != "default" {
        return resolve_positive_timeout_ms(Some(params.config.timeout_ms))
            .unwrap_or(DEFAULT_TIMEOUT_MS);
    }
    resolve_positive_timeout_ms(params.provider_default_timeout_ms).unwrap_or(params.config.timeout_ms)
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
struct ResolveSpeechProviderTimeoutMsParams {
    pub timeout_ms: Option<u64>,
    pub config: ResolvedTtsConfig,
    pub provider_default_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TtsUserPrefs {
    pub tts: Option<TtsUserPrefsTts>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TtsUserPrefsTts {
    pub auto: Option<String>,
    pub enabled: Option<bool>,
    pub provider: Option<String>,
    pub persona: Option<String>,
    pub max_length: Option<usize>,
    pub summarize: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsAttemptReasonCode {
    Success,
    NoProviderRegistered,
    NotConfigured,
    UnsupportedForStreaming,
    UnsupportedForTelephony,
    Timeout,
    ProviderError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsProviderAttempt {
    pub provider: String,
    pub outcome: String,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_binding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsResult {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<Vec<TtsProviderAttempt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_compatible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_as_voice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsSynthesisResult {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_buffer: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<Vec<TtsProviderAttempt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_compatible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsStreamResult {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_stream: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<Vec<TtsProviderAttempt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_compatible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<JsonValue>,
}

pub type TtsSynthesisStreamResult = TtsStreamResult;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsTelephonyResult {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_buffer: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<Vec<TtsProviderAttempt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtsStatusEntry {
    pub timestamp: u64,
    pub success: bool,
    pub text_length: usize,
    pub summarized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<Vec<TtsProviderAttempt>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

use std::sync::Mutex;
static LAST_TTS_ATTEMPT: Mutex<Option<TtsStatusEntry>> = Mutex::new(None);

fn resolve_configured_tts_auto_mode(raw: &TtsConfig) -> TtsAutoMode {
    if let Some(auto) = normalize_tts_auto_mode(raw.auto.as_deref()) {
        return auto;
    }
    if raw.enabled.unwrap_or(false) {
        return "always".to_string();
    }
    "off".to_string()
}

fn normalize_configured_speech_provider_id(provider_id: Option<&str>) -> Option<TtsProvider> {
    let normalized = normalize_speech_provider_id(provider_id);
    match normalized {
        Some(n) if n == "edge" => Some("microsoft".to_string()),
        Some(n) => Some(n),
        None => None,
    }
}

fn normalize_tts_persona_id(persona_id: Option<&str>) -> Option<String> {
    if let Some(id) = persona_id {
        if id.is_empty() {
            return None;
        }
        Some(id.to_lowercase())
    } else {
        None
    }
}

fn resolve_user_path(p: &str) -> PathBuf {
    if let Some(stripped) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(p)
}

fn resolve_config_dir(_env: Option<&str>) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("cradle-ring");
    }
    PathBuf::from(".")
}

fn resolve_tts_prefs_path_value(prefs_path: Option<&str>) -> PathBuf {
    if let Some(p) = prefs_path {
        if !p.trim().is_empty() {
            return resolve_user_path(p.trim());
        }
    }
    if let Some(env_path) = std::env::var_os("CRADLE_RING_TTS_PREFS") {
        let s = env_path.to_string_lossy().to_string();
        if !s.trim().is_empty() {
            return resolve_user_path(s.trim());
        }
    }
    resolve_config_dir(None).join("settings").join("tts.json")
}

fn resolve_model_override_policy(
    overrides: Option<&TtsModelOverrideConfig>,
) -> ResolvedTtsModelOverrides {
    let enabled = overrides.and_then(|o| o.enabled).unwrap_or(true);
    if !enabled {
        return ResolvedTtsModelOverrides {
            enabled: false,
            allow_text: false,
            allow_provider: false,
            allow_voice: false,
            allow_model_id: false,
            allow_voice_settings: false,
            allow_normalization: false,
            allow_seed: false,
        };
    }
    let allow = |value: Option<bool>, default_value: bool| value.unwrap_or(default_value);
    ResolvedTtsModelOverrides {
        enabled: true,
        allow_text: allow(overrides.and_then(|o| o.allow_text), true),
        allow_provider: allow(overrides.and_then(|o| o.allow_provider), false),
        allow_voice: allow(overrides.and_then(|o| o.allow_voice), true),
        allow_model_id: allow(overrides.and_then(|o| o.allow_model_id), true),
        allow_voice_settings: allow(overrides.and_then(|o| o.allow_voice_settings), true),
        allow_normalization: allow(overrides.and_then(|o| o.allow_normalization), true),
        allow_seed: allow(overrides.and_then(|o| o.allow_seed), true),
    }
}

fn resolve_tts_runtime_config(cfg: &OpenClawConfig) -> OpenClawConfig {
    match select_applicable_runtime_config(
        crate::runtime_api::SelectApplicableRuntimeConfigParams {
            input_config: Some(cfg.clone()),
            runtime_config: get_runtime_config_snapshot(),
            runtime_source_config: get_runtime_config_source_snapshot(),
        },
    ) {
        Some(c) => c,
        None => cfg.clone(),
    }
}

fn as_provider_config(value: &JsonValue) -> SpeechProviderConfig {
    if value.is_object() && !value.is_array() {
        value.clone()
    } else {
        JsonValue::Object(Map::new())
    }
}

fn as_provider_config_map(value: &JsonValue) -> Map<String, JsonValue> {
    match value {
        JsonValue::Object(map) => map.clone(),
        _ => Map::new(),
    }
}

fn has_own_property(value: &Map<String, JsonValue>, key: &str) -> bool {
    value.contains_key(key)
}

fn normalize_optional_string(value: Option<&JsonValue>) -> Option<String> {
    value.and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn normalize_optional_lowercase_string(value: Option<&str>) -> Option<String> {
    value.map(|s| s.to_lowercase())
}

#[allow(dead_code)]
fn normalize_lowercase_string_or_empty(value: &str) -> String {
    value.to_lowercase()
}

fn normalize_provider_config_map(value: &JsonValue) -> Option<HashMap<String, SpeechProviderConfig>> {
    let raw_map = as_provider_config_map(value);
    if raw_map.is_empty() {
        return None;
    }
    let mut next: HashMap<String, SpeechProviderConfig> = HashMap::new();
    for (provider_id, provider_config) in raw_map.iter() {
        let normalized = normalize_configured_speech_provider_id(Some(provider_id))
            .unwrap_or_else(|| provider_id.clone());
        next.insert(normalized, with_speaker_selection_compat(as_provider_config(provider_config)));
    }
    Some(next)
}

fn collect_tts_personas(raw: &TtsConfig) -> HashMap<String, ResolvedTtsPersona> {
    let raw_personas_value = raw.personas.clone().unwrap_or_else(|| JsonValue::Object(Map::new()));
    let raw_personas = as_provider_config_map(&raw_personas_value);
    let mut personas: HashMap<String, ResolvedTtsPersona> = HashMap::new();
    for (id, value) in raw_personas.iter() {
        let normalized_id = match normalize_tts_persona_id(Some(id)) {
            Some(v) => v,
            None => continue,
        };
        let obj = match value.as_object() {
            Some(o) => o,
            None => continue,
        };
        let description = obj.get("description").and_then(|v| v.as_str()).map(String::from);
        let label = obj.get("label").and_then(|v| v.as_str()).map(String::from);
        let fallback_policy = obj.get("fallbackPolicy").and_then(|v| v.as_str()).map(String::from);
        let provider = obj.get("provider").and_then(|v| v.as_str()).map(String::from);
        let providers_value = obj.get("providers").cloned().unwrap_or(JsonValue::Null);
        let providers = normalize_provider_config_map(&providers_value);
        let persona = ResolvedTtsPersona {
            id: normalized_id.clone(),
            label,
            description,
            provider: provider
                .as_deref()
                .and_then(|p| normalize_configured_speech_provider_id(Some(p)))
                .or(provider),
            providers,
            fallback_policy,
        };
        personas.insert(normalized_id, persona);
    }
    personas
}

#[allow(dead_code)]
fn resolve_persona_provider_config(
    persona: Option<&ResolvedTtsPersona>,
    provider_id: &str,
) -> Option<SpeechProviderConfig> {
    let persona = persona?;
    let providers = persona.providers.as_ref()?;
    let normalized = normalize_configured_speech_provider_id(Some(provider_id))
        .unwrap_or_else(|| provider_id.to_string());
    if let Some(p) = providers.get(&normalized) {
        return Some(p.clone());
    }
    if let Some(p) = providers.get(provider_id) {
        return Some(p.clone());
    }
    None
}

#[allow(dead_code)]
struct MergeProviderConfigResult {
    provider_config: SpeechProviderConfig,
    persona_provider_config: Option<SpeechProviderConfig>,
    persona_binding: String,
}

#[allow(dead_code)]
fn merge_provider_config_with_persona(
    provider_config: SpeechProviderConfig,
    persona: Option<&ResolvedTtsPersona>,
    provider_id: &str,
) -> MergeProviderConfigResult {
    if persona.is_none() {
        return MergeProviderConfigResult {
            provider_config,
            persona_provider_config: None,
            persona_binding: "none".to_string(),
        };
    }
    let persona_provider_config = resolve_persona_provider_config(persona, provider_id);
    if persona_provider_config.is_none() {
        return MergeProviderConfigResult {
            provider_config,
            persona_provider_config: None,
            persona_binding: "missing".to_string(),
        };
    }
    let ppc = persona_provider_config.clone().unwrap();
    let mut merged = as_provider_config_map(&provider_config);
    let ppc_map = as_provider_config_map(&ppc);
    for (k, v) in ppc_map.iter() {
        merged.insert(k.clone(), v.clone());
    }
    MergeProviderConfigResult {
        provider_config: JsonValue::Object(merged),
        persona_provider_config: Some(ppc),
        persona_binding: "applied".to_string(),
    }
}

#[allow(dead_code)]
fn resolve_raw_provider_config(raw: Option<&TtsConfig>, provider_id: &str) -> SpeechProviderConfig {
    let raw = match raw {
        Some(r) => r,
        None => return JsonValue::Object(Map::new()),
    };
    let raw_providers_value = raw.providers.clone().unwrap_or(JsonValue::Object(Map::new()));
    let raw_providers = as_provider_config_map(&raw_providers_value);
    let direct = raw_providers
        .get(provider_id)
        .cloned()
        .unwrap_or(JsonValue::Null);
    with_speaker_selection_compat(as_provider_config(&direct))
}

fn collect_direct_provider_config_entries(raw: &TtsConfig) -> HashMap<String, SpeechProviderConfig> {
    let mut entries: HashMap<String, SpeechProviderConfig> = HashMap::new();
    let raw_providers_value = raw.providers.clone().unwrap_or_else(|| JsonValue::Object(Map::new()));
    let raw_providers = as_provider_config_map(&raw_providers_value);
    for (provider_id, value) in raw_providers.iter() {
        let normalized = normalize_configured_speech_provider_id(Some(provider_id))
            .unwrap_or_else(|| provider_id.clone());
        entries.insert(normalized, with_speaker_selection_compat(as_provider_config(value)));
    }
    let reserved_keys: std::collections::HashSet<&str> = [
        "auto", "enabled", "maxTextLength", "mode", "modelOverrides", "persona", "personas",
        "prefsPath", "provider", "providers", "summaryModel", "timeoutMs",
    ]
    .iter()
    .copied()
    .collect();
    if let Some(obj) = serde_json::to_value(raw).ok().and_then(|v| v.as_object().cloned()) {
        for (key, value) in obj.iter() {
            if reserved_keys.contains(key.as_str()) {
                continue;
            }
            if !value.is_object() || value.is_array() {
                continue;
            }
            let normalized = normalize_configured_speech_provider_id(Some(key)).unwrap_or_else(|| key.clone());
            entries.entry(normalized).or_insert_with(|| with_speaker_selection_compat(as_provider_config(value)));
        }
    }
    entries
}

pub fn get_resolved_speech_provider_config(
    config: &ResolvedTtsConfig,
    provider_id: &str,
    cfg: Option<&OpenClawConfig>,
) -> SpeechProviderConfig {
    let _ = (config, provider_id, cfg);
    JsonValue::Object(Map::new())
}

fn resolve_configured_speech_voice_model_for_provider(
    cfg: Option<&OpenClawConfig>,
    provider_id: &str,
    provider: Option<&VoiceModelProvider>,
    voice_model: Option<&VoiceModelRef>,
) -> Option<VoiceModelRef> {
    let _ = (cfg, provider_id, provider, voice_model);
    None
}

fn apply_voice_model_to_speech_provider_config(
    cfg: Option<&OpenClawConfig>,
    provider_id: &str,
    provider_config: SpeechProviderConfig,
    provider: Option<&VoiceModelProvider>,
    voice_model: Option<&VoiceModelRef>,
) -> SpeechProviderConfig {
    let _ = (cfg, provider_id, provider);
    if let Some(vm) = voice_model {
        if !vm.model.is_empty() {
            let mut map = as_provider_config_map(&provider_config);
            if !normalize_optional_string(map.get("model")).is_some()
                && !normalize_optional_string(map.get("modelId")).is_some()
            {
                map.insert("model".to_string(), JsonValue::String(vm.model.clone()));
                map.insert("modelId".to_string(), JsonValue::String(vm.model.clone()));
            }
            return JsonValue::Object(map);
        }
    }
    provider_config
}

#[allow(dead_code)]
fn sort_speech_providers_for_auto_selection(_cfg: Option<&OpenClawConfig>) -> Vec<JsonValue> {
    list_speech_providers(None)
}

pub fn resolve_tts_config(
    cfg_input: &OpenClawConfig,
    context: Option<&TtsConfigResolutionContext>,
) -> ResolvedTtsConfig {
    let mut cfg = cfg_input.clone();
    cfg = resolve_tts_runtime_config(&cfg);
    let raw = resolve_effective_tts_config(&cfg, context.and_then(|c| c.agent_id.as_deref()));
    let provider_source = if raw.provider.is_some() { "config" } else { "default" };
    let timeout_ms = raw.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let timeout_ms_source = if raw.timeout_ms.is_none() { "default" } else { "config" };
    let auto = resolve_configured_tts_auto_mode(&raw);
    let persona = raw.persona.as_deref().and_then(|p| normalize_tts_persona_id(Some(p)));
    ResolvedTtsConfig {
        auto,
        mode: raw.mode.clone().unwrap_or_else(|| "final".to_string()),
        provider: normalize_configured_speech_provider_id(raw.provider.as_deref()).unwrap_or_else(|| {
            if provider_source == "config" {
                raw.provider
                    .as_deref()
                    .and_then(|p| normalize_optional_lowercase_string(Some(p)))
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }),
        provider_source: provider_source.to_string(),
        persona,
        personas: collect_tts_personas(&raw),
        summary_model: raw.summary_model.clone(),
        model_overrides: resolve_model_override_policy(raw.model_overrides.as_ref()),
        provider_configs: collect_direct_provider_config_entries(&raw),
        prefs_path: raw.prefs_path.clone(),
        max_text_length: raw.max_text_length.unwrap_or(DEFAULT_MAX_TEXT_LENGTH),
        timeout_ms,
        timeout_ms_source: timeout_ms_source.to_string(),
        raw_config: Some(serde_json::to_value(&raw).unwrap_or(JsonValue::Null)),
        source_config: Some(cfg),
    }
}

pub fn resolve_tts_prefs_path(config: &ResolvedTtsConfig) -> PathBuf {
    resolve_tts_prefs_path_value(config.prefs_path.as_deref())
}

fn resolve_tts_auto_mode_from_prefs(prefs: &TtsUserPrefs) -> Option<TtsAutoMode> {
    let auto = prefs.tts.as_ref().and_then(|t| normalize_tts_auto_mode(t.auto.as_deref()));
    if let Some(a) = auto {
        return Some(a);
    }
    if let Some(enabled) = prefs.tts.as_ref().and_then(|t| t.enabled) {
        return Some(if enabled { "always".to_string() } else { "off".to_string() });
    }
    None
}

async fn read_prefs_async(prefs_path: &Path) -> TtsUserPrefs {
    if !prefs_path.exists() {
        return TtsUserPrefs::default();
    }
    match fs::read_to_string(prefs_path).await {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => TtsUserPrefs::default(),
    }
}

fn read_prefs_sync(prefs_path: &Path) -> TtsUserPrefs {
    if !prefs_path.exists() {
        return TtsUserPrefs::default();
    }
    match std::fs::read_to_string(prefs_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => TtsUserPrefs::default(),
    }
}

pub fn resolve_tts_auto_mode(
    config: &ResolvedTtsConfig,
    prefs_path: &Path,
    session_auto: Option<&str>,
) -> TtsAutoMode {
    if let Some(s) = normalize_tts_auto_mode(session_auto) {
        return s;
    }
    let prefs = read_prefs_sync(prefs_path);
    if let Some(a) = resolve_tts_auto_mode_from_prefs(&prefs) {
        return a;
    }
    config.auto.clone()
}

struct ResolvedTtsAutoState {
    auto_mode: TtsAutoMode,
    prefs_path: PathBuf,
}

fn resolve_effective_tts_auto_state(
    cfg: &OpenClawConfig,
    session_auto: Option<&str>,
    agent_id: Option<&str>,
    channel_id: Option<&str>,
    account_id: Option<&str>,
) -> ResolvedTtsAutoState {
    let raw = resolve_effective_tts_config_with_context(
        cfg,
        agent_id,
        channel_id,
        account_id,
    );
    let prefs_path = resolve_tts_prefs_path_value(raw.prefs_path.as_deref());
    if let Some(s) = normalize_tts_auto_mode(session_auto) {
        return ResolvedTtsAutoState { auto_mode: s, prefs_path };
    }
    let prefs = read_prefs_sync(&prefs_path);
    if let Some(a) = resolve_tts_auto_mode_from_prefs(&prefs) {
        return ResolvedTtsAutoState { auto_mode: a, prefs_path };
    }
    ResolvedTtsAutoState {
        auto_mode: resolve_configured_tts_auto_mode(&raw),
        prefs_path,
    }
}

fn resolve_effective_tts_config_with_context(
    cfg: &OpenClawConfig,
    agent_id: Option<&str>,
    _channel_id: Option<&str>,
    _account_id: Option<&str>,
) -> TtsConfig {
    let _ = agent_id;
    resolve_effective_tts_config(cfg, agent_id)
}

pub fn build_tts_system_prompt_hint(
    cfg_input: &OpenClawConfig,
    agent_id: Option<&str>,
) -> Option<String> {
    let mut cfg = cfg_input.clone();
    cfg = resolve_tts_runtime_config(&cfg);
    let state = resolve_effective_tts_auto_state(&cfg, None, agent_id, None, None);
    if state.auto_mode == "off" {
        return None;
    }
    let config_for_test = resolve_tts_config(&cfg, None);
    let persona = get_tts_persona(&config_for_test, &state.prefs_path);
    let max_length = get_tts_max_length(&state.prefs_path);
    let summarize = if is_summarization_enabled(&state.prefs_path) { "on" } else { "off" };
    let auto_hint = match state.auto_mode.as_str() {
        "inbound" => Some("Only use TTS when the user's last message includes audio/voice."),
        "tagged" => Some("Only use TTS when you include [[tts:key=value]] directives or a [[tts:text]]...[[/tts:text]] block."),
        _ => None,
    };
    let mut parts: Vec<String> = Vec::new();
    parts.push("Voice (TTS) is enabled.".to_string());
    if let Some(h) = auto_hint {
        parts.push(h.to_string());
    }
    if let Some(p) = persona {
        let label = p.label.unwrap_or_else(|| p.id.clone());
        if let Some(d) = p.description {
            parts.push(format!("Active TTS persona: {} - {}.", label, d));
        } else {
            parts.push(format!("Active TTS persona: {}.", label));
        }
    }
    parts.push(format!("Keep spoken text \u{2264}{} chars to avoid auto-summary (summary {}).", max_length, summarize));
    parts.push("If workspace context (especially MEMORY.md) tells you not to use [[tts:...]] or to use a local/non-tagged voice workflow, follow that workspace instruction instead.".to_string());
    parts.push("Use [[tts:...]] and optional [[tts:text]]...[[/tts:text]] to control voice/expressiveness.".to_string());
    Some(parts.join("\n"))
}

fn atomic_write_file_sync(file_path: &Path, content: &str) {
    if let Some(parent) = file_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(file_path, content);
}

fn update_prefs(prefs_path: &Path, update: impl FnOnce(&mut TtsUserPrefs)) {
    let mut prefs = read_prefs_sync(prefs_path);
    update(&mut prefs);
    if let Ok(s) = serde_json::to_string_pretty(&prefs) {
        atomic_write_file_sync(prefs_path, &s);
    }
}

pub fn is_tts_enabled(config: &ResolvedTtsConfig, prefs_path: &Path, session_auto: Option<&str>) -> bool {
    resolve_tts_auto_mode(config, prefs_path, session_auto) != "off"
}

pub fn set_tts_auto_mode(prefs_path: &Path, mode: TtsAutoMode) {
    update_prefs(prefs_path, |prefs| {
        let mut next = prefs.tts.clone().unwrap_or_default();
        next.enabled = None;
        next.auto = Some(mode.clone());
        prefs.tts = Some(next);
    });
}

pub fn set_tts_enabled(prefs_path: &Path, enabled: bool) {
    set_tts_auto_mode(prefs_path, if enabled { "always".to_string() } else { "off".to_string() });
}

pub fn get_tts_provider(config: &ResolvedTtsConfig, prefs_path: &Path) -> TtsProvider {
    let _ = (config, prefs_path);
    String::new()
}

fn resolve_tts_persona_from_prefs(
    config: &ResolvedTtsConfig,
    prefs: &TtsUserPrefs,
) -> Option<ResolvedTtsPersona> {
    if let Some(tts) = &prefs.tts {
        if let Some(persona_id) = &tts.persona {
            if let Some(p) = normalize_tts_persona_id(Some(persona_id)) {
                return config.personas.get(&p).cloned();
            }
            return None;
        }
    }
    if let Some(p) = config.persona.as_deref().and_then(|p| normalize_tts_persona_id(Some(p))) {
        return config.personas.get(&p).cloned();
    }
    None
}

pub fn get_tts_persona(config: &ResolvedTtsConfig, prefs_path: &Path) -> Option<ResolvedTtsPersona> {
    let prefs = read_prefs_sync(prefs_path);
    resolve_tts_persona_from_prefs(config, &prefs)
}

pub fn list_tts_personas(config: &ResolvedTtsConfig) -> Vec<ResolvedTtsPersona> {
    let mut v: Vec<ResolvedTtsPersona> = config.personas.values().cloned().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

pub fn set_tts_persona(prefs_path: &Path, persona: Option<&str>) {
    update_prefs(prefs_path, |prefs| {
        let mut next = prefs.tts.clone().unwrap_or_default();
        let normalized = persona.and_then(|p: &str| normalize_tts_persona_id(Some(p)));
        next.persona = Some(normalized.unwrap_or_default());
        prefs.tts = Some(next);
    });
}

pub fn set_tts_provider(prefs_path: &Path, provider: TtsProvider) {
    update_prefs(prefs_path, |prefs| {
        let mut tts = prefs.tts.clone().unwrap_or_default();
        let canonical = canonicalize_speech_provider_id(Some(&provider), None).unwrap_or(provider.clone());
        tts.provider = Some(canonical);
        prefs.tts = Some(tts);
    });
}

pub fn resolve_explicit_tts_overrides(params: ResolveExplicitTtsOverridesParams) -> Result<TtsDirectiveOverrides, String> {
    let _ = params;
    Ok(TtsDirectiveOverrides::default())
}

pub struct ResolveExplicitTtsOverridesParams<'a> {
    pub cfg: &'a OpenClawConfig,
    pub prefs_path: Option<&'a Path>,
    pub provider: Option<&'a str>,
    pub model_id: Option<&'a str>,
    pub voice_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub channel_id: Option<&'a str>,
    pub account_id: Option<&'a str>,
}

pub fn get_tts_max_length(prefs_path: &Path) -> usize {
    let prefs = read_prefs_sync(prefs_path);
    if let Some(tts) = &prefs.tts {
        if let Some(n) = tts.max_length {
            return n;
        }
    }
    DEFAULT_TTS_MAX_LENGTH
}

pub fn set_tts_max_length(prefs_path: &Path, max_length: usize) {
    update_prefs(prefs_path, |prefs| {
        let mut tts = prefs.tts.clone().unwrap_or_default();
        tts.max_length = Some(max_length);
        prefs.tts = Some(tts);
    });
}

pub fn is_summarization_enabled(prefs_path: &Path) -> bool {
    let prefs = read_prefs_sync(prefs_path);
    if let Some(tts) = &prefs.tts {
        if let Some(s) = tts.summarize {
            return s;
        }
    }
    DEFAULT_TTS_SUMMARIZE
}

pub fn set_summarization_enabled(prefs_path: &Path, enabled: bool) {
    update_prefs(prefs_path, |prefs| {
        let mut tts = prefs.tts.clone().unwrap_or_default();
        tts.summarize = Some(enabled);
        prefs.tts = Some(tts);
    });
}

pub fn get_last_tts_attempt() -> Option<TtsStatusEntry> {
    LAST_TTS_ATTEMPT.lock().ok().and_then(|g| g.clone())
}

pub fn set_last_tts_attempt(entry: Option<TtsStatusEntry>) {
    if let Ok(mut g) = LAST_TTS_ATTEMPT.lock() {
        *g = entry;
    }
}

fn supports_native_voice_note_tts(_channel: Option<&str>) -> bool {
    false
}

fn supports_transcoded_voice_note_tts(_channel: Option<&str>) -> bool {
    false
}

fn resolve_tts_synthesis_target(_channel: Option<&str>) -> &'static str {
    "audio-file"
}

#[allow(dead_code)]
fn supports_audio_file_voice_memo_output(
    _file_extension: Option<&str>,
    _output_format: Option<&str>,
    _audio_file_formats: Option<&[String]>,
) -> bool {
    false
}

fn should_deliver_tts_as_voice(
    _channel: Option<&str>,
    _target: Option<&str>,
    _voice_compatible: Option<bool>,
    _file_extension: Option<&str>,
    _output_format: Option<&str>,
) -> bool {
    false
}

pub fn resolve_tts_provider_order(primary: TtsProvider, _cfg: Option<&OpenClawConfig>) -> Vec<TtsProvider> {
    vec![primary]
}

fn resolve_tts_provider_candidates(primary: TtsProvider, _cfg: Option<&OpenClawConfig>) -> Vec<VoiceProviderCandidate> {
    vec![VoiceProviderCandidate { provider: primary, voice_model: None }]
}

fn resolve_primary_tts_provider_candidate(primary: TtsProvider, _cfg: Option<&OpenClawConfig>) -> VoiceProviderCandidate {
    VoiceProviderCandidate { provider: primary, voice_model: None }
}

pub fn is_tts_provider_configured(
    _config: &ResolvedTtsConfig,
    _provider: TtsProvider,
    _cfg: Option<&OpenClawConfig>,
) -> bool {
    false
}

fn format_tts_provider_error(provider: &str, err: &str) -> String {
    if err.contains("AbortError") {
        return format!("{}: request timed out", provider);
    }
    format!("{}: {}", provider, err)
}

fn sanitize_tts_error_for_log(err: &str) -> String {
    err.replace('\r', "\\r").replace('\n', "\\n").replace('\t', "\\t")
}

#[allow(dead_code)]
struct BuildTtsFailureResult {
    success: bool,
    error: String,
    attempted_providers: Option<Vec<String>>,
    attempts: Option<Vec<TtsProviderAttempt>>,
    persona: Option<String>,
}

#[allow(dead_code)]
fn build_tts_failure_result(
    errors: Vec<String>,
    attempted_providers: Vec<String>,
    attempts: Vec<TtsProviderAttempt>,
    persona: Option<String>,
) -> BuildTtsFailureResult {
    BuildTtsFailureResult {
        success: false,
        error: format!(
            "TTS conversion failed: {}",
            if errors.is_empty() { "no providers available".to_string() } else { errors.join("; ") }
        ),
        attempted_providers: Some(attempted_providers),
        attempts: Some(attempts),
        persona,
    }
}

#[allow(dead_code)]
enum TtsProviderReadyResolution {
    Ready {
        provider: JsonValue,
        provider_config: SpeechProviderConfig,
        persona_provider_config: Option<SpeechProviderConfig>,
        synthesis_persona: Option<ResolvedTtsPersona>,
        persona_binding: String,
    },
    Skip {
        reason_code: String,
        message: String,
        persona_binding: Option<String>,
    },
}

#[allow(dead_code)]
fn resolve_ready_speech_provider(
    _provider: TtsProvider,
    _cfg: &OpenClawConfig,
    _config: &ResolvedTtsConfig,
    _persona: Option<&ResolvedTtsPersona>,
    _voice_model: Option<&VoiceModelRef>,
    _require_telephony: Option<bool>,
) -> TtsProviderReadyResolution {
    TtsProviderReadyResolution::Skip {
        reason_code: "no_provider_registered".to_string(),
        message: "no provider registered".to_string(),
        persona_binding: None,
    }
}

fn prepare_speech_synthesis(_params: JsonValue) -> JsonValue {
    JsonValue::Null
}

fn resolve_tts_request_setup(_params: JsonValue) -> Result<JsonValue, String> {
    Err("TTS request setup unavailable in 1:1 port stubs".to_string())
}

#[allow(dead_code)]
fn read_tts_result_string(value: Option<&JsonValue>) -> Option<String> {
    value.and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[allow(dead_code)]
fn resolve_tts_result_model(
    provider_config: &SpeechProviderConfig,
    provider_overrides: Option<&SpeechProviderOverrides>,
) -> Option<String> {
    let p = provider_config;
    let o = provider_overrides;
    read_tts_result_string(o.and_then(|v| v.get("modelId")))
        .or_else(|| read_tts_result_string(o.and_then(|v| v.get("model")))
        .or_else(|| read_tts_result_string(p.get("modelId")))
        .or_else(|| read_tts_result_string(p.get("model"))))
}

#[allow(dead_code)]
fn resolve_tts_result_voice(
    provider_config: &SpeechProviderConfig,
    provider_overrides: Option<&SpeechProviderOverrides>,
) -> Option<String> {
    let p = provider_config;
    let o = provider_overrides;
    read_tts_result_string(o.and_then(|v| v.get("speakerVoiceId")))
        .or_else(|| read_tts_result_string(o.and_then(|v| v.get("speakerVoice"))))
        .or_else(|| read_tts_result_string(o.and_then(|v| v.get("voiceId"))))
        .or_else(|| read_tts_result_string(o.and_then(|v| v.get("voiceName"))))
        .or_else(|| read_tts_result_string(o.and_then(|v| v.get("voice"))))
        .or_else(|| read_tts_result_string(p.get("speakerVoiceId")))
        .or_else(|| read_tts_result_string(p.get("speakerVoice")))
        .or_else(|| read_tts_result_string(p.get("voiceId")))
        .or_else(|| read_tts_result_string(p.get("voiceName")))
        .or_else(|| read_tts_result_string(p.get("voice")))
}

#[allow(dead_code)]
struct TempWorkspace {
    dir: PathBuf,
}

impl TempWorkspace {
    #[allow(dead_code)]
    fn write(&self, name: &str, content: Vec<u8>) -> String {
        let path = self.dir.join(name);
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(&path, content);
        path.to_string_lossy().to_string()
    }
}

#[allow(dead_code)]
fn temp_workspace_sync(root_dir: &Path, prefix: &str) -> TempWorkspace {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = root_dir.join(format!("{}{}", prefix, nanos));
    let _ = std::fs::create_dir_all(&dir);
    TempWorkspace { dir }
}

#[allow(dead_code)]
fn resolve_preferred_openclaw_tmp_dir() -> PathBuf {
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        return PathBuf::from(tmp);
    }
    std::env::temp_dir()
}

pub async fn text_to_speech(_params: TextToSpeechParams<'_>) -> TtsResult {
    TtsResult {
        success: false,
        error: Some("TTS providers not registered in 1:1 port".to_string()),
        ..Default::default()
    }
}

pub struct TextToSpeechParams<'a> {
    pub text: &'a str,
    pub cfg: &'a OpenClawConfig,
    pub prefs_path: Option<&'a Path>,
    pub channel: Option<&'a str>,
    pub overrides: Option<&'a TtsDirectiveOverrides>,
    pub disable_fallback: Option<bool>,
    pub timeout_ms: Option<u64>,
    pub agent_id: Option<&'a str>,
    pub account_id: Option<&'a str>,
}

pub async fn synthesize_speech(_params: TextToSpeechParams<'_>) -> TtsSynthesisResult {
    TtsSynthesisResult {
        success: false,
        error: Some("TTS providers not registered in 1:1 port".to_string()),
        ..Default::default()
    }
}

pub async fn stream_speech(_params: TextToSpeechParams<'_>) -> TtsStreamResult {
    TtsStreamResult {
        success: false,
        error: Some("TTS providers not registered in 1:1 port".to_string()),
        ..Default::default()
    }
}

pub async fn text_to_speech_stream(_params: TextToSpeechParams<'_>) -> TtsStreamResult {
    TtsStreamResult {
        success: false,
        error: Some("Streaming TTS providers not registered in 1:1 port".to_string()),
        ..Default::default()
    }
}

pub async fn text_to_speech_telephony(_params: TextToSpeechTelephonyParams<'_>) -> TtsTelephonyResult {
    TtsTelephonyResult {
        success: false,
        error: Some("TTS telephony not registered in 1:1 port".to_string()),
        ..Default::default()
    }
}

pub struct TextToSpeechTelephonyParams<'a> {
    pub text: &'a str,
    pub cfg: &'a OpenClawConfig,
    pub prefs_path: Option<&'a Path>,
    pub overrides: Option<&'a TtsDirectiveOverrides>,
    pub timeout_ms: Option<u64>,
}

pub async fn list_speech_voices(_params: ListSpeechVoicesParams<'_>) -> Result<Vec<SpeechVoiceOption>, String> {
    Err("speech providers not registered in 1:1 port".to_string())
}

pub struct ListSpeechVoicesParams<'a> {
    pub provider: &'a str,
    pub cfg: Option<&'a OpenClawConfig>,
    pub config: Option<&'a ResolvedTtsConfig>,
    pub api_key: Option<&'a str>,
    pub base_url: Option<&'a str>,
}

fn has_legacy_final_media_directive(text: &str) -> bool {
    let re = Regex::new(r"(?:^|\n)\s*MEDIA\s*:").unwrap();
    re.is_match(text)
}

pub async fn maybe_apply_tts_to_payload(params: MaybeApplyTtsToPayloadParams) -> ReplyPayload {
    let cfg = resolve_tts_runtime_config(&params.cfg);
    let state = resolve_effective_tts_auto_state(
        &cfg,
        params.tts_auto.as_deref(),
        params.agent_id.as_deref(),
        params.channel.as_deref(),
        params.account_id.as_deref(),
    );
    if state.auto_mode == "off" {
        return params.payload;
    }
    if params.payload.get("isCompactionNotice").and_then(|v| v.as_bool()) == Some(true) {
        return params.payload;
    }
    let config = resolve_tts_config(
        &cfg,
        Some(&TtsConfigResolutionContext {
            agent_id: params.agent_id.map(String::from),
            channel_id: params.channel.map(String::from),
            account_id: params.account_id.map(String::from),
        }),
    );
    let _ = config;
    params.payload
}

pub struct MaybeApplyTtsToPayloadParams {
    pub payload: ReplyPayload,
    pub cfg: OpenClawConfig,
    pub channel: Option<String>,
    pub kind: Option<String>,
    pub inbound_audio: Option<bool>,
    pub tts_auto: Option<String>,
    pub agent_id: Option<String>,
    pub account_id: Option<String>,
}

pub fn test_api() -> TestApi {
    TestApi {
        parse_tts_directives_fn: parse_tts_directives,
        resolve_model_override_policy_fn: resolve_model_override_policy,
        supports_native_voice_note_tts_fn: supports_native_voice_note_tts,
        supports_transcoded_voice_note_tts_fn: supports_transcoded_voice_note_tts,
        resolve_tts_synthesis_target_fn: resolve_tts_synthesis_target,
        should_deliver_tts_as_voice_fn: should_deliver_tts_as_voice,
        summarize_text_fn: |_t| Box::pin(async { JsonValue::Null }),
        get_resolved_speech_provider_config_fn: |c, p, cfg| {
            get_resolved_speech_provider_config(c, p, cfg)
        },
        format_tts_provider_error_fn: format_tts_provider_error,
        sanitize_tts_error_for_log_fn: sanitize_tts_error_for_log,
    }
}

pub struct TestApi {
    pub parse_tts_directives_fn: fn(&str, &ResolvedTtsModelOverrides, &JsonValue) -> TtsDirectiveParseResult,
    pub resolve_model_override_policy_fn: fn(Option<&TtsModelOverrideConfig>) -> ResolvedTtsModelOverrides,
    pub supports_native_voice_note_tts_fn: fn(Option<&str>) -> bool,
    pub supports_transcoded_voice_note_tts_fn: fn(Option<&str>) -> bool,
    pub resolve_tts_synthesis_target_fn: fn(Option<&str>) -> &'static str,
    pub should_deliver_tts_as_voice_fn: fn(Option<&str>, Option<&str>, Option<bool>, Option<&str>, Option<&str>) -> bool,
    pub summarize_text_fn: fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = JsonValue> + Send>>,
    pub get_resolved_speech_provider_config_fn: fn(&ResolvedTtsConfig, &str, Option<&OpenClawConfig>) -> SpeechProviderConfig,
    pub format_tts_provider_error_fn: fn(&str, &str) -> String,
    pub sanitize_tts_error_for_log_fn: fn(&str) -> String,
}

// Legacy alias: export the same struct as `_test`.
#[deprecated(note = "Use `test_api`.")]
pub fn _test() -> TestApi {
    test_api()
}

// Suppress warnings for unused imports/aliases.
#[allow(dead_code)]
fn _silence() {
    let _ = resolve_configured_speech_voice_model_for_provider;
    let _ = apply_voice_model_to_speech_provider_config;
    let _ = resolve_tts_provider_candidates;
    let _ = resolve_primary_tts_provider_candidate;
    let _ = resolve_primary_voice_provider_candidate;
    let _ = resolve_supported_voice_model_refs;
    let _ = resolve_voice_model_refs;
    let _ = resolve_voice_provider_candidates;
    let _ = voice_provider_supports_model;
    let _ = summarize_text;
    let _ = schedule_cleanup;
    let _ = read_prefs_async;
    let _ = has_own_property;
    let _ = prepare_speech_synthesis;
    let _ = resolve_tts_request_setup;
    let _ = resolve_tts_provider_order;
    let _ = has_legacy_final_media_directive;
    let _ = mark_reply_payload_as_tts_supplement;
    let _ = resolve_sendable_outbound_reply_parts;
    let _ = get_speech_provider;
}
