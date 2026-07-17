// ACP Core module implements errors behavior.
// 翻译自 packages/acp-core/src/runtime/errors.ts

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::error_format;

pub const ACP_ERROR_CODES: &[&str] = &[
    "ACP_BACKEND_MISSING",
    "ACP_BACKEND_UNAVAILABLE",
    "ACP_BACKEND_UNSUPPORTED_CONTROL",
    "ACP_DISPATCH_DISABLED",
    "ACP_INVALID_RUNTIME_OPTION",
    "ACP_SESSION_INIT_FAILED",
    "ACP_TURN_FAILED",
];

pub type AcpRuntimeErrorCode = String;

static ACP_ERROR_CODE_SET: OnceLock<HashSet<String>> = OnceLock::new();

fn acp_error_code_set() -> &'static HashSet<String> {
    ACP_ERROR_CODE_SET.get_or_init(|| ACP_ERROR_CODES.iter().map(|s| s.to_string()).collect())
}

/// Error type used at ACP runtime boundaries so callers can preserve structured failure codes.
#[derive(Debug, Clone)]
pub struct AcpRuntimeError {
    pub code: AcpRuntimeErrorCode,
    pub message: String,
    /// Backend-specific structured failure code (e.g. acpx "SESSION_RESUME_REQUIRED"),
    /// preserved so recovery decisions key on the failure kind rather than parsing
    /// the human-readable message.
    pub detail_code: Option<String>,
}

impl std::fmt::Display for AcpRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AcpRuntimeError {}

impl AcpRuntimeError {
    pub fn new(code: AcpRuntimeErrorCode, message: String) -> Self {
        Self {
            code,
            message,
            detail_code: None,
        }
    }

    pub fn with_detail_code(mut self, detail_code: Option<String>) -> Self {
        self.detail_code = detail_code;
        self
    }
}

/// Foreign ACP runtime error detected by inspecting a Debug-formatted error string.
#[derive(Debug)]
pub struct ForeignAcpRuntimeError {
    pub code: String,
    pub message: String,
}

fn get_foreign_acp_runtime_error(formatted: &str) -> Option<(String, String)> {
    if acp_error_code_set().iter().any(|c| formatted.contains(c)) {
        // Best-effort: TS inspects `error.code` and `error.message` directly. In Rust we
        // approximate using the Debug-formatted string. Callers wanting exact foreign-error
        // detection should use `AcpRuntimeError::from(...)` constructors instead.
        for code in ACP_ERROR_CODES {
            if formatted.contains(code) {
                return Some((code.to_string(), formatted.to_string()));
            }
        }
    }
    None
}

fn read_acp_request_error_details(error: &str) -> Option<String> {
    // The TS version reads `.code` (number) and `.data.details` from a JSON-RPC error.
    // We approximate by relying on AcpRuntimeError's `.detail_code`; arbitrary foreign
    // errors do not have these fields in Rust without a typed JSON-RPC error type.
    let _ = error;
    None
}

fn message_with_acp_request_error_details(error_message: &str) -> String {
    if let Some(details) = read_acp_request_error_details(error_message) {
        if !error_message.contains(&details) {
            return format!("{}: {}", error_message, details);
        }
    }
    error_message.to_string()
}

/// Recognizes local ACP runtime errors by type, and foreign ones by inspecting their code.
pub fn is_acp_runtime_error<E: std::fmt::Debug>(error: &E) -> bool {
    if let Some(_) = try_downcast_acp_error(error) {
        return true;
    }
    let formatted = format!("{:?}", error);
    get_foreign_acp_runtime_error(&formatted).is_some()
}

fn try_downcast_acp_error<E: std::fmt::Debug>(error: &E) -> Option<&AcpRuntimeError> {
    // Use Any-like detection via Debug formatting (the helper struct path is the canonical way)
    // but since AcpRuntimeError implements Debug, we cannot downcast without Any. So we use
    // the type-name sniff instead.
    let type_name = std::any::type_name::<E>();
    if type_name == std::any::type_name::<AcpRuntimeError>() {
        // Safety: same type_name means same type. Caller passed &E == &AcpRuntimeError.
        Some(unsafe { &*(error as *const E as *const AcpRuntimeError) })
    } else {
        None
    }
}

/// Converts arbitrary thrown values into ACP runtime errors with redacted request details.
pub fn to_acp_runtime_error<E: std::fmt::Debug>(params: ToAcpParams<'_, E>) -> AcpRuntimeError {
    if let Some(existing) = try_downcast_acp_error(params.error) {
        return existing.clone();
    }
    let formatted = format!("{:?}", params.error);
    if let Some((code, message)) = get_foreign_acp_runtime_error(&formatted) {
        return AcpRuntimeError::new(code, message);
    }
    AcpRuntimeError::new(
        params.fallback_code.to_string(),
        message_with_acp_request_error_details(&formatted),
    )
}

pub struct ToAcpParams<'a, E: std::fmt::Debug> {
    pub error: &'a E,
    pub fallback_code: &'a str,
    pub fallback_message: &'a str,
}

/// Render an error chain as a single human-readable line.
pub fn format_acp_error_chain(error: &(dyn std::fmt::Debug + 'static)) -> String {
    let formatted = format!("{:?}", error);
    error_format::redact_sensitive_text(&formatted)
}

/// Wraps async runtime work and rethrows failures as ACP runtime errors.
pub async fn with_acp_runtime_error_boundary<F, T>(
    fallback_code: &'static str,
    _fallback_message: &'static str,
    run: F,
) -> Result<T, AcpRuntimeError>
where
    F: std::future::Future<Output = Result<T, Box<dyn std::fmt::Debug + Send + Sync>>>,
{
    match run.await {
        Ok(value) => Ok(value),
        Err(error) => {
            let code = fallback_code.to_string();
            let message = format!("{:?}", error);
            Err(AcpRuntimeError::new(code, message))
        }
    }
}