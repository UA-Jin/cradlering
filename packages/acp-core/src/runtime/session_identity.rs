// ACP Core module implements session identity behavior.
// 翻译自 packages/acp-core/src/runtime/session-identity.ts

use serde_json::Value;

use crate::normalize_text;
use crate::runtime::types::{AcpRuntimeHandle, AcpRuntimeStatus};
use crate::types::{SessionAcpIdentity, SessionAcpIdentitySource, SessionAcpMeta};

/// Normalize a stored identity state value from metadata.
fn normalize_identity_state(value: &Value) -> Option<String> {
    match value.as_str() {
        Some("pending") | Some("resolved") => value.as_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// Normalize where an ACP identity observation came from.
fn normalize_identity_source(value: &Value) -> Option<SessionAcpIdentitySource> {
    match value.as_str() {
        Some("ensure") | Some("status") | Some("event") => value.as_str().map(|s| s.to_string()),
        _ => None,
    }
}

#[allow(dead_code)]
fn identity_to_value(s: Option<&str>) -> Value {
    match s {
        Some(v) => Value::String(v.to_string()),
        None => Value::Null,
    }
}

/// Normalize an identity object and infer pending/resolved state from stable ids.
fn normalize_identity(identity: Option<&SessionAcpIdentity>) -> Option<SessionAcpIdentity> {
    let identity = identity?;
    let state = normalize_identity_state(&Value::String(identity.state.clone()));
    let source = normalize_identity_source(&Value::String(identity.source.clone()));
    let acpx_record_id = normalize_text::normalize_text_opt(identity.acpx_record_id.as_deref());
    let acpx_session_id = normalize_text::normalize_text_opt(identity.acpx_session_id.as_deref());
    let agent_session_id = normalize_text::normalize_text_opt(identity.agent_session_id.as_deref());
    let last_updated_at = Some(identity.last_updated_at);
    let has_any_id = acpx_record_id.is_some() || acpx_session_id.is_some() || agent_session_id.is_some();
    if state.is_none() && source.is_none() && !has_any_id && last_updated_at.is_none() {
        return None;
    }
    let resolved = acpx_session_id.is_some() || agent_session_id.is_some();
    let normalized_state = state.unwrap_or_else(|| {
        if resolved {
            "resolved".to_string()
        } else {
            "pending".to_string()
        }
    });
    Some(SessionAcpIdentity {
        state: normalized_state,
        acpx_record_id,
        acpx_session_id,
        agent_session_id,
        source: source.unwrap_or_else(|| "status".to_string()),
        last_updated_at: last_updated_at.unwrap_or_else(chrono_now_ms),
    })
}

fn chrono_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Default)]
pub struct IdentityIds {
    pub acpx_record_id: Option<String>,
    pub acpx_session_id: Option<String>,
    pub agent_session_id: Option<String>,
}

/// Read identity ids from a runtime handle shape.
fn read_identity_ids_from_handle(handle: &AcpRuntimeHandle) -> IdentityIds {
    IdentityIds {
        acpx_record_id: normalize_text::normalize_text_opt(handle.acpx_record_id.as_deref()),
        acpx_session_id: normalize_text::normalize_text_opt(handle.backend_session_id.as_deref()),
        agent_session_id: normalize_text::normalize_text_opt(handle.agent_session_id.as_deref()),
    }
}

/// Build an identity only when at least one stable id is known.
fn build_session_identity(params: BuildSessionIdentityParams) -> Option<SessionAcpIdentity> {
    let IdentityIds {
        acpx_record_id,
        acpx_session_id,
        agent_session_id,
    } = params.ids;
    if acpx_record_id.is_none() && acpx_session_id.is_none() && agent_session_id.is_none() {
        return None;
    }
    Some(SessionAcpIdentity {
        state: params.state,
        acpx_record_id,
        acpx_session_id,
        agent_session_id,
        source: params.source,
        last_updated_at: params.now,
    })
}

pub struct BuildSessionIdentityParams {
    pub ids: IdentityIds,
    pub state: String,
    pub source: String,
    pub now: i64,
}

/// Resolve normalized ACP identity from persisted session metadata.
pub fn resolve_session_identity_from_meta(meta: Option<&SessionAcpMeta>) -> Option<SessionAcpIdentity> {
    let meta = meta?;
    normalize_identity(meta.identity.as_ref())
}

/// Return true when an identity has a backend or agent session id.
pub fn identity_has_stable_session_id(identity: Option<&SessionAcpIdentity>) -> bool {
    let identity = match identity {
        Some(i) => i,
        None => return false,
    };
    identity.acpx_session_id.is_some() || identity.agent_session_id.is_some()
}

/// Resolve the runtime resume id, preferring agent session id over ACP backend id.
pub fn resolve_runtime_resume_session_id(identity: Option<&SessionAcpIdentity>) -> Option<String> {
    let identity = identity?;
    if let Some(s) = normalize_text::normalize_text_opt(identity.agent_session_id.as_deref()) {
        return Some(s);
    }
    normalize_text::normalize_text_opt(identity.acpx_session_id.as_deref())
}

/// Return true when identity is absent or still pending.
pub fn is_session_identity_pending(identity: Option<&SessionAcpIdentity>) -> bool {
    match identity {
        None => true,
        Some(i) => i.state == "pending",
    }
}

/// Compare identities ignoring lastUpdatedAt timestamp churn.
pub fn identity_equals(
    left: Option<&SessionAcpIdentity>,
    right: Option<&SessionAcpIdentity>,
) -> bool {
    let a = normalize_identity(left);
    let b = normalize_identity(right);
    match (a, b) {
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
        (Some(a), Some(b)) => {
            a.state == b.state
                && a.acpx_record_id == b.acpx_record_id
                && a.acpx_session_id == b.acpx_session_id
                && a.agent_session_id == b.agent_session_id
                && a.source == b.source
        }
    }
}

/// Merge current and incoming identity observations without downgrading resolved ids.
pub fn merge_session_identity(params: MergeSessionIdentityParams) -> Option<SessionAcpIdentity> {
    let current = normalize_identity(params.current);
    let incoming = normalize_identity(params.incoming);
    let current = match current {
        Some(c) => c,
        None => {
            return match incoming {
                Some(mut i) => {
                    i.last_updated_at = params.now;
                    Some(i)
                }
                None => None,
            };
        }
    };
    let incoming = match incoming {
        Some(i) => i,
        None => return Some(current),
    };

    let current_resolved = current.state == "resolved";
    let incoming_resolved = incoming.state == "resolved";
    let allow_incoming_value = !current_resolved || incoming_resolved;
    let next_record_id = if allow_incoming_value && incoming.acpx_record_id.is_some() {
        incoming.acpx_record_id
    } else {
        current.acpx_record_id
    };
    let next_acpx_session_id = if allow_incoming_value && incoming.acpx_session_id.is_some() {
        incoming.acpx_session_id
    } else {
        current.acpx_session_id
    };
    let next_agent_session_id = if allow_incoming_value && incoming.agent_session_id.is_some() {
        incoming.agent_session_id
    } else {
        current.agent_session_id
    };

    let next_resolved = next_acpx_session_id.is_some() || next_agent_session_id.is_some();
    let next_state = if next_resolved {
        "resolved".to_string()
    } else if current_resolved {
        "resolved".to_string()
    } else {
        incoming.state
    };
    let next_source = if allow_incoming_value {
        incoming.source
    } else {
        current.source
    };
    Some(SessionAcpIdentity {
        state: next_state,
        acpx_record_id: next_record_id,
        acpx_session_id: next_acpx_session_id,
        agent_session_id: next_agent_session_id,
        source: next_source,
        last_updated_at: params.now,
    })
}

pub struct MergeSessionIdentityParams<'a> {
    pub current: Option<&'a SessionAcpIdentity>,
    pub incoming: Option<&'a SessionAcpIdentity>,
    pub now: i64,
}

/// Create a pending identity from an ensure-session handle.
pub fn create_identity_from_ensure(handle: &AcpRuntimeHandle, now: i64) -> Option<SessionAcpIdentity> {
    build_session_identity(BuildSessionIdentityParams {
        ids: read_identity_ids_from_handle(handle),
        state: "pending".to_string(),
        source: "ensure".to_string(),
        now,
    })
}

/// Create an identity from a runtime event handle.
pub fn create_identity_from_handle_event(handle: &AcpRuntimeHandle, now: i64) -> Option<SessionAcpIdentity> {
    let ids = read_identity_ids_from_handle(handle);
    build_session_identity(BuildSessionIdentityParams {
        state: if ids.agent_session_id.is_some() {
            "resolved".to_string()
        } else {
            "pending".to_string()
        },
        source: "event".to_string(),
        now,
        ids,
    })
}

/// Create an identity from runtime status output.
pub fn create_identity_from_status(status: Option<&AcpRuntimeStatus>, now: i64) -> Option<SessionAcpIdentity> {
    let status = status?;
    let acpx_record_id = normalize_text::normalize_text_opt(status.acpx_record_id.as_deref())
        .or_else(|| {
            status
                .details
                .as_ref()
                .and_then(|d| d.get("acpxRecordId"))
                .and_then(|v| normalize_text::normalize_text(v))
        });
    let acpx_session_id = normalize_text::normalize_text_opt(status.backend_session_id.as_deref())
        .or_else(|| {
            status
                .details
                .as_ref()
                .and_then(|d| d.get("backendSessionId"))
                .and_then(|v| normalize_text::normalize_text(v))
        })
        .or_else(|| {
            status
                .details
                .as_ref()
                .and_then(|d| d.get("acpxSessionId"))
                .and_then(|v| normalize_text::normalize_text(v))
        });
    let agent_session_id = normalize_text::normalize_text_opt(status.agent_session_id.as_deref())
        .or_else(|| {
            status
                .details
                .as_ref()
                .and_then(|d| d.get("agentSessionId"))
                .and_then(|v| normalize_text::normalize_text(v))
        });
    if acpx_record_id.is_none() && acpx_session_id.is_none() && agent_session_id.is_none() {
        return None;
    }
    let resolved = acpx_session_id.is_some() || agent_session_id.is_some();
    Some(SessionAcpIdentity {
        state: if resolved { "resolved".to_string() } else { "pending".to_string() },
        acpx_record_id,
        acpx_session_id,
        agent_session_id,
        source: "status".to_string(),
        last_updated_at: now,
    })
}

/// Convert ACP identity ids into runtime handle resume identifiers.
pub fn resolve_runtime_handle_identifiers_from_identity(
    identity: Option<&SessionAcpIdentity>,
) -> RuntimeHandleIdentifiers {
    let identity = match identity {
        Some(i) => i,
        None => return RuntimeHandleIdentifiers::default(),
    };
    RuntimeHandleIdentifiers {
        backend_session_id: identity.acpx_session_id.clone(),
        agent_session_id: identity.agent_session_id.clone(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeHandleIdentifiers {
    pub backend_session_id: Option<String>,
    pub agent_session_id: Option<String>,
}