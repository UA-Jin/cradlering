// ACP Core type module defines shared TypeScript contracts.
// 翻译自 packages/acp-core/src/runtime/types.ts

use std::collections::HashMap;

use async_stream::stream;
use futures_core::Stream;

use crate::types::AbortSignal;

pub type AcpRuntimePromptMode = String;
pub type AcpRuntimeSessionMode = String;

/// Runtime update tags emitted by ACP adapters; unknown backend tags are passed through.
pub type AcpSessionUpdateTag = String;

pub type AcpRuntimeControl = String;

/// Stable handle returned by ensureSession and passed back into all ACP runtime operations.
#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeHandle {
    pub session_key: String,
    pub backend: String,
    pub runtime_session_name: String,
    /// Effective runtime working directory for this ACP session, if exposed by adapter/runtime.
    pub cwd: Option<String>,
    /// Backend-local record identifier, if exposed by adapter/runtime (for example acpx record id).
    pub acpx_record_id: Option<String>,
    /// Backend-level ACP session identifier, if exposed by adapter/runtime.
    pub backend_session_id: Option<String>,
    /// Upstream harness session identifier, if exposed by adapter/runtime.
    pub agent_session_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeEnsureInput {
    pub session_key: String,
    pub agent: String,
    pub mode: AcpRuntimeSessionMode,
    /// Backend or agent session id to resume when reopening an existing conversation.
    pub resume_session_id: Option<String>,
    /// Optional runtime model override that must be available during session creation.
    pub model: Option<String>,
    /// Optional runtime thinking/reasoning override that must be available during session creation.
    pub thinking: Option<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeTurnAttachment {
    pub media_type: String,
    pub data: String,
}

/// Per-turn payload delivered to ACP adapters.
#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeTurnInput {
    pub handle: AcpRuntimeHandle,
    pub text: String,
    pub attachments: Option<Vec<AcpRuntimeTurnAttachment>>,
    pub mode: AcpRuntimePromptMode,
    pub request_id: String,
    pub signal: Option<AbortSignal>,
}

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeCapabilities {
    pub controls: Vec<AcpRuntimeControl>,
    /// Optional backend-advertised option keys for session/set_config_option.
    /// Empty/undefined means "backend accepts keys, but did not advertise a strict list".
    pub config_option_keys: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeStatus {
    pub summary: Option<String>,
    /// Backend-local record identifier, if exposed by adapter/runtime.
    pub acpx_record_id: Option<String>,
    /// Backend-level ACP session identifier, if known at status time.
    pub backend_session_id: Option<String>,
    /// Upstream harness session identifier, if known at status time.
    pub agent_session_id: Option<String>,
    pub details: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeDoctorReport {
    pub ok: bool,
    pub code: Option<String>,
    pub message: String,
    pub install_command: Option<String>,
    pub details: Option<Vec<String>>,
}

/// Streaming event union produced by ACP adapters while a turn is running.
#[derive(Debug, Clone)]
pub enum AcpRuntimeEvent {
    TextDelta {
        text: String,
        stream: Option<String>,
        tag: Option<AcpSessionUpdateTag>,
    },
    Status {
        text: String,
        tag: Option<AcpSessionUpdateTag>,
        used: Option<i64>,
        size: Option<i64>,
    },
    ToolCall {
        text: String,
        tag: Option<AcpSessionUpdateTag>,
        tool_call_id: Option<String>,
        status: Option<String>,
        title: Option<String>,
        kind: Option<String>,
    },
    Done {
        /// Closed result status when the manager synthesizes the terminal event.
        status: Option<String>,
        stop_reason: Option<String>,
    },
    Error {
        message: String,
        code: Option<String>,
        detail_code: Option<String>,
        retryable: Option<bool>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct AcpRuntimeTurnResultError {
    pub message: String,
    pub code: Option<String>,
    pub detail_code: Option<String>,
    pub retryable: Option<bool>,
}

/// Terminal turn result, separated from the live event stream for reliable failure handling.
#[derive(Debug, Clone)]
pub enum AcpRuntimeTurnResult {
    Completed { stop_reason: Option<String> },
    Cancelled { stop_reason: Option<String> },
    Failed { error: AcpRuntimeTurnResultError },
}

/// ACP runtime turn: live events streamed separately from the terminal result.
pub struct AcpRuntimeTurn {
    pub request_id: String,
    pub events: std::pin::Pin<Box<dyn Stream<Item = AcpRuntimeEvent> + Send>>,
    pub result: std::pin::Pin<Box<dyn std::future::Future<Output = AcpRuntimeTurnResult> + Send>>,
}

/// ACP adapter contract implemented by backend plugins and consumed by gateway/session flows.
pub trait AcpRuntime: Send + Sync {
    fn ensure_session(
        &self,
        input: AcpRuntimeEnsureInput,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AcpRuntimeHandle, String>> + Send>>;

    /// Preferred turn API. Live events are streamed separately from the terminal result.
    fn start_turn(
        &self,
        _input: AcpRuntimeTurnInput,
    ) -> Option<AcpRuntimeTurn> {
        None
    }

    fn run_turn(
        &self,
        input: AcpRuntimeTurnInput,
    ) -> std::pin::Pin<Box<dyn Stream<Item = AcpRuntimeEvent> + Send>> {
        let _ = input;
        Box::pin(stream! {
            yield AcpRuntimeEvent::Error {
                message: "AcpRuntime::run_turn not implemented".to_string(),
                code: None,
                detail_code: None,
                retryable: Some(false),
            };
        })
    }

    fn get_capabilities(
        &self,
        _input: Option<AcpRuntimeHandle>,
    ) -> Option<AcpRuntimeCapabilities> {
        None
    }

    fn get_status(
        &self,
        _input: AcpRuntimeHandle,
        _signal: Option<AbortSignal>,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<AcpRuntimeStatus, String>> + Send>>> {
        None
    }

    fn set_mode(
        &self,
        _handle: AcpRuntimeHandle,
        _mode: String,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>> {
        None
    }

    fn set_config_option(
        &self,
        _handle: AcpRuntimeHandle,
        _key: String,
        _value: String,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>> {
        None
    }

    fn doctor(
        &self,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = AcpRuntimeDoctorReport> + Send>>> {
        None
    }

    /// Prepare the next ensureSession for this session key to start fresh instead
    /// of reopening backend-owned persistent state.
    fn prepare_fresh_session(
        &self,
        _session_key: String,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>> {
        None
    }

    fn cancel(
        &self,
        handle: AcpRuntimeHandle,
        reason: Option<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

    fn close(
        &self,
        handle: AcpRuntimeHandle,
        reason: String,
        discard_persistent_state: Option<bool>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}

// Suppress unused import warnings for stream macros that may be needed downstream.
#[allow(dead_code, unused_variables)]
fn _force_imports() {
    let _: Option<fn() -> i32> = None;
    let _ = stream! { yield 1i32 };
}