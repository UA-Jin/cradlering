// ACP Core module implements session lineage meta behavior.
// 翻译自 packages/acp-core/src/session-lineage-meta.ts

use normalization_core::string_coerce;
use serde_json::Value;

const SUBAGENT_ROLES: &[&str] = &["orchestrator", "leaf"];
const SUBAGENT_CONTROL_SCOPES: &[&str] = &["children", "none"];

#[derive(Debug, Clone, Default)]
pub struct AcpSessionLineageMeta {
    /// Stable session key emitted to ACP clients.
    pub session_key: String,
    pub kind: Option<String>,
    pub channel: Option<String>,
    /// Best available parent session id, preferring explicit parentSessionKey over legacy spawnedBy.
    pub parent_session_id: Option<String>,
    pub spawned_by: Option<String>,
    pub spawn_depth: Option<i64>,
    pub subagent_role: Option<String>,
    pub subagent_control_scope: Option<String>,
    pub spawned_workspace_dir: Option<String>,
    pub spawned_cwd: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AcpSessionLineageRow {
    /// Raw persisted session key; kept even when other optional fields are malformed.
    pub key: String,
    pub kind: Option<String>,
    pub channel: Option<String>,
    pub parent_session_key: Option<String>,
    pub spawned_by: Option<String>,
    pub spawn_depth: Option<i64>,
    pub subagent_role: Option<String>,
    pub subagent_control_scope: Option<String>,
    pub spawned_workspace_dir: Option<String>,
    pub spawned_cwd: Option<String>,
}

#[allow(dead_code)]
fn read_integer(value: &Value) -> Option<i64> {
    let n = value.as_i64()?;
    if n >= 0 {
        Some(n)
    } else {
        None
    }
}

fn read_enum(value: &Value, allowed: &[&str]) -> Option<String> {
    let normalized = string_coerce::normalize_optional_string(value)?;
    if allowed.iter().any(|a| *a == normalized) {
        Some(normalized)
    } else {
        None
    }
}

/// Converts persisted session rows into compact ACP lineage metadata for protocol responses.
pub fn to_acp_session_lineage_meta(row: &AcpSessionLineageRow) -> AcpSessionLineageMeta {
    let key_value = Value::String(row.key.clone());
    let session_key = string_coerce::normalize_optional_string(&key_value)
        .unwrap_or_else(|| row.key.clone());
    let kind = string_coerce::normalize_optional_string(&value_opt(row.kind.as_deref()));
    let channel = string_coerce::normalize_optional_string(&value_opt(row.channel.as_deref()));
    // Older rows may only carry spawnedBy; expose it as parentSessionId so ACP clients
    // can follow lineage without knowing which storage-era field populated it.
    let parent_session_id = string_coerce::normalize_optional_string(&value_opt(
        row.parent_session_key.as_deref(),
    ))
    .or_else(|| {
        string_coerce::normalize_optional_string(&value_opt(row.spawned_by.as_deref()))
    });
    let spawned_by = string_coerce::normalize_optional_string(&value_opt(row.spawned_by.as_deref()));
    let spawn_depth = row
        .spawn_depth
        .and_then(|v| if v >= 0 { Some(v) } else { None });
    let subagent_role = row
        .subagent_role
        .as_deref()
        .and_then(|s| read_enum(&Value::String(s.to_string()), SUBAGENT_ROLES));
    let subagent_control_scope = row
        .subagent_control_scope
        .as_deref()
        .and_then(|s| read_enum(&Value::String(s.to_string()), SUBAGENT_CONTROL_SCOPES));
    let spawned_workspace_dir = string_coerce::normalize_optional_string(&value_opt(
        row.spawned_workspace_dir.as_deref(),
    ));
    let spawned_cwd =
        string_coerce::normalize_optional_string(&value_opt(row.spawned_cwd.as_deref()));

    AcpSessionLineageMeta {
        session_key,
        kind,
        channel,
        parent_session_id,
        spawned_by,
        spawn_depth,
        subagent_role,
        subagent_control_scope,
        spawned_workspace_dir,
        spawned_cwd,
    }
}

fn value_opt(s: Option<&str>) -> Value {
    match s {
        Some(v) => Value::String(v.to_string()),
        None => Value::Null,
    }
}