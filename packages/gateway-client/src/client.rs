// Gateway Client module implements client behavior.
// 翻译自 packages/gateway-client/src/client.ts
//
// This module preserves the public API surface from the TypeScript source.
// WebSocket transport integration is stubbed; downstream crates supply the
// concrete transport via `GatewayClientHostDeps`.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use gateway_protocol::client_info::{GatewayClientMode, GatewayClientName};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub private_key_pem: String,
    pub public_key_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceAuthTokenRecord {
    pub token: Option<String>,
    pub scopes: Option<Vec<String>>,
}

#[derive(Default)]
pub struct GatewayClientHostDeps {
    pub load_or_create_device_identity: Option<Box<dyn Fn() -> Option<DeviceIdentity> + Send + Sync>>,
    pub sign_device_payload: Option<Box<dyn Fn(String, String) -> String + Send + Sync>>,
    pub public_key_raw_base64_url_from_pem: Option<Box<dyn Fn(String) -> String + Send + Sync>>,
    pub load_device_auth_token:
        Option<Box<dyn Fn(DeviceAuthTokenLoadParams) -> Option<DeviceAuthTokenRecord> + Send + Sync>>,
    pub store_device_auth_token:
        Option<Box<dyn Fn(DeviceAuthTokenStoreParams) + Send + Sync>>,
    pub clear_device_auth_token: Option<Box<dyn Fn(DeviceAuthTokenLoadParams) + Send + Sync>>,
    pub before_connect: Option<Box<dyn Fn() + Send + Sync>>,
    pub register_gateway_loopback_bypass:
        Option<Box<dyn Fn(String) -> Option<Box<dyn Fn() + Send + Sync>> + Send + Sync>>,
    pub log_debug: Option<Box<dyn Fn(String) + Send + Sync>>,
    pub log_error: Option<Box<dyn Fn(String) + Send + Sync>>,
    pub redact_for_log: Option<Box<dyn Fn(String) -> String + Send + Sync>>,
    pub normalize_tls_fingerprint: Option<Box<dyn Fn(Option<String>) -> String + Send + Sync>>,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceAuthTokenLoadParams {
    pub device_id: String,
    pub role: String,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceAuthTokenStoreParams {
    pub device_id: String,
    pub role: String,
    pub token: String,
    pub scopes: Vec<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayClientRequestOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_final: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_accepted: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct GatewayReconnectPausedInfo {
    pub code: i64,
    pub reason: String,
    pub detail_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GatewayClientCloseInfo {
    pub phase: String, // "pre-hello" | "post-hello"
    pub socket_opened: bool,
    pub transport_validated: bool,
    pub transient_pre_hello_clean_close: bool,
}

#[derive(Debug, Clone)]
pub struct GatewayClientConnectionMetadata {
    pub client_name: Option<GatewayClientName>,
    pub has_device_identity: bool,
    pub mode: Option<GatewayClientMode>,
    pub preauth_handshake_timeout_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct GatewayClientRequestError {
    pub gateway_code: String,
    pub details: Option<Value>,
    pub retryable: bool,
    pub retry_after_ms: Option<i64>,
    pub message: String,
}

impl std::fmt::Display for GatewayClientRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for GatewayClientRequestError {}

pub const GATEWAY_CLOSE_CODE_HINTS: &[(&str, &str)] = &[
    ("1000", "normal closure"),
    ("1006", "abnormal closure (no close frame)"),
    ("1008", "policy violation"),
    ("1012", "service restart"),
    ("1013", "try again later"),
];

pub fn describe_gateway_close_code(code: i64) -> Option<&'static str> {
    GATEWAY_CLOSE_CODE_HINTS
        .iter()
        .find(|(k, _)| *k == code.to_string().as_str())
        .map(|(_, v)| *v)
}

pub fn is_gateway_connect_assembly_error(_value: &dyn std::fmt::Debug) -> bool {
    // TS uses a Symbol-keyed marker. Rust has no equivalent symbol mechanism;
    // we conservatively return false here so callers must tag their own errors.
    false
}

#[derive(Default)]
pub struct GatewayClientOptions {
    pub url: Option<String>,
    pub origin: Option<String>,
    pub connect_challenge_timeout_ms: Option<f64>,
    pub preauth_handshake_timeout_ms: Option<i64>,
    pub tick_watch_min_interval_ms: Option<f64>,
    pub tick_watch_timeout_ms: Option<f64>,
    pub request_timeout_ms: Option<f64>,
    pub token: Option<String>,
    pub bootstrap_token: Option<String>,
    pub device_token: Option<String>,
    pub password: Option<String>,
    pub approval_runtime_token: Option<String>,
    pub agent_runtime_identity_token: Option<String>,
    pub instance_id: Option<String>,
    pub client_name: Option<GatewayClientName>,
    pub client_display_name: Option<String>,
    pub client_version: Option<String>,
    pub platform: Option<String>,
    pub device_family: Option<String>,
    pub mode: Option<GatewayClientMode>,
    pub role: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub caps: Option<Vec<String>>,
    pub commands: Option<Vec<String>>,
    pub permissions: Option<HashMap<String, bool>>,
    pub path_env: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub device_identity: Option<DeviceIdentity>,
    pub host_deps: Option<GatewayClientHostDeps>,
    pub min_protocol: Option<i64>,
    pub max_protocol: Option<i64>,
    pub tls_fingerprint: Option<String>,
    pub on_event: Option<Box<dyn Fn(Value) + Send + Sync>>,
    pub on_hello_ok: Option<Box<dyn Fn(Value) + Send + Sync>>,
    pub on_connect_error: Option<Box<dyn Fn(String) + Send + Sync>>,
    pub on_reconnect_paused: Option<Box<dyn Fn(GatewayReconnectPausedInfo) + Send + Sync>>,
    pub on_close: Option<Box<dyn Fn(i64, String, Option<GatewayClientCloseInfo>) + Send + Sync>>,
    pub on_gap: Option<Box<dyn Fn(GatewayGapInfo) + Send + Sync>>,
}

#[derive(Debug, Clone)]
pub struct GatewayGapInfo {
    pub expected: i64,
    pub received: i64,
}

/// Gateway client transport stub. Downstream crates supply the concrete WebSocket
/// transport via `host_deps`; this struct tracks connection state and exposes the
/// request API surface.
pub struct GatewayClient {
    opts: GatewayClientOptions,
    state: Mutex<GatewayClientState>,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
struct GatewayClientState {
    closed: bool,
    backoff_ms: i64,
    last_seq: Option<i64>,
    connect_nonce: Option<String>,
    connect_sent: bool,
    pending_device_token_retry: bool,
    device_token_retry_budget_used: bool,
    approval_runtime_token_compatibility_disabled: bool,
    approval_runtime_token_retry_budget_used: bool,
    pending_startup_reconnect_delay_ms: Option<i64>,
    pending_connect_error_detail_code: Option<String>,
    pending_connect_error_details: Option<Value>,
    last_tick: Option<i64>,
    tick_interval_ms: i64,
    socket_opened: bool,
    transport_validated: bool,
    hello_ok_received: bool,
    suppressed_transient_pre_hello_clean_closes: i64,
    request_timeout_ms: i64,
}

const DEFAULT_GATEWAY_CLIENT_URL: &str = "ws://127.0.0.1:18789";
#[allow(dead_code)]
const DEFAULT_CLIENT_VERSION: &str = "0.0.0";
const FORCE_STOP_TERMINATE_GRACE_MS: u64 = 250;
const STOP_AND_WAIT_TIMEOUT_MS: i64 = 1_000;
#[allow(dead_code)]
const MAX_SUPPRESSED_TRANSIENT_PRE_HELLO_CLEAN_CLOSES: i64 = 1;

impl GatewayClient {
    pub fn new(opts: GatewayClientOptions) -> Self {
        let request_timeout_ms = opts
            .request_timeout_ms
            .filter(|v| v.is_finite())
            .map(|v| crate::timeouts::resolve_safe_timeout_delay_ms(v, None))
            .unwrap_or(30_000);
        Self {
            opts,
            state: Mutex::new(GatewayClientState {
                tick_interval_ms: 30_000,
                request_timeout_ms,
                ..Default::default()
            }),
        }
    }

    pub fn get_connection_metadata(&self) -> GatewayClientConnectionMetadata {
        GatewayClientConnectionMetadata {
            client_name: self.opts.client_name.clone(),
            has_device_identity: self.opts.device_identity.is_some(),
            mode: self.opts.mode.clone(),
            preauth_handshake_timeout_ms: self.opts.preauth_handshake_timeout_ms,
        }
    }

    pub fn start(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.connect_nonce = None;
        state.connect_sent = false;
        let url = self.opts.url.clone().unwrap_or_else(|| DEFAULT_GATEWAY_CLIENT_URL.to_string());
        if self.opts.tls_fingerprint.is_some() && !url.starts_with("wss://") {
            if let Some(cb) = &self.opts.on_connect_error {
                cb("gateway tls fingerprint requires wss:// gateway url".to_string());
            }
            return;
        }
        // The actual WebSocket connect is delegated to a host-provided transport.
        // The state machine here mirrors the TS lifecycle so downstream code that
        // calls `start()` from a host event loop gets consistent transitions.
        state.socket_opened = false;
        state.transport_validated = false;
        state.hello_ok_received = false;
        let _ = url;
    }

    pub fn stop(&self) {
        let _ = FORCE_STOP_TERMINATE_GRACE_MS;
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.pending_device_token_retry = false;
        state.device_token_retry_budget_used = false;
        state.pending_startup_reconnect_delay_ms = None;
        state.pending_connect_error_detail_code = None;
        state.pending_connect_error_details = None;
    }

    pub async fn stop_and_wait(&self, _opts: Option<StopAndWaitOptions>) {
        let _ = STOP_AND_WAIT_TIMEOUT_MS;
        self.stop();
    }

    pub async fn request<T: Default>(
        &self,
        method: &str,
        _params: Option<Value>,
        _opts: Option<GatewayClientRequestOptions>,
    ) -> Result<T, String> {
        let _ = method;
        Err("GatewayClient::request requires a host-supplied transport".to_string())
    }
}

#[derive(Debug, Clone, Default)]
pub struct StopAndWaitOptions {
    pub timeout_ms: Option<f64>,
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn normalize_lowercase_string_or_empty(value: Option<&str>) -> String {
    value
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default()
}

fn is_sensitive_url_query_param_name(key: &str) -> bool {
    let re = regex::Regex::new(r"(?i)(?:token|password|secret|key|auth|credential)").unwrap();
    re.is_match(key)
}

fn normalize_fingerprint(fingerprint: Option<&str>) -> String {
    fingerprint
        .unwrap_or("")
        .replace(':', "")
        .trim()
        .to_lowercase()
}

fn raw_data_to_string(_data: Value) -> String {
    // Adapter hook: concrete transports convert their frame buffers to UTF-8 strings.
    String::new()
}

#[allow(dead_code)]
fn _force_use() {
    let _: Option<DeviceIdentity> = None;
    let _: Option<GatewayClientOptions> = None;
    let _ = normalize_optional_string(None);
    let _ = normalize_lowercase_string_or_empty(None);
    let _ = is_sensitive_url_query_param_name("token");
    let _ = normalize_fingerprint(None);
    let _ = raw_data_to_string(Value::Null);
}