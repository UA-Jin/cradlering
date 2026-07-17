// CradleRing SDK module implements client behavior.
// 翻译自 packages/sdk/src/client.ts

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures_util::stream::Stream;
use serde_json::Value;
use uuid::Uuid;

use crate::event_hub::{EventHub, EventHubOptions};
use crate::transport::{
    GatewayClientFactory, GatewayClientLike, GatewayClientTransport, GatewayClientTransportOptions,
};
use crate::types::{
    AgentRunParams, AgentsCreateParams, AgentsDeleteParams, AgentsUpdateParams,
    ApprovalDecisionParams, ArtifactQuery, ArtifactsDownloadResult,
    ArtifactsGetResult, ArtifactsListResult, ConnectableOpenClawTransport, EnvironmentCreateParams,
    EnvironmentSummary, EnvironmentsListResult, GatewayEvent, GatewayRequestOptions,
    OpenClawEvent, OpenClawTransport, RunCreateParams, RunResult, RunStatus, RunTimestamp,
    SDKError, SessionCreateParams, SessionSendParams, SessionTarget,
    TasksCancelResult, TasksGetResult, TasksListParams, TasksListResult, ToolInvokeParams,
    ToolInvokeResult, ToolsEffectiveParams,
};

#[allow(dead_code)]
const MAX_REPLAY_RUNS: usize = 100;
#[allow(dead_code)]
const MAX_REPLAY_EVENTS_PER_RUN: usize = 500;
const MAX_NORMALIZED_REPLAY_EVENTS: usize = 2000;

/// Connection and transport options for the CradleRing SDK client.
#[derive(Default)]
pub struct OpenClawOptions {
    pub gateway: Option<String>,
    pub url: Option<String>,
    pub token: Option<String>,
    pub password: Option<String>,
    pub request_timeout_ms: Option<u64>,
    pub transport: Option<Box<dyn OpenClawTransport>>,
}

impl OpenClawOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }
    pub fn with_transport(mut self, transport: Box<dyn OpenClawTransport>) -> Self {
        self.transport = Some(transport);
        self
    }
}

fn resolve_gateway_url(options: &OpenClawOptions) -> Option<String> {
    if let Some(url) = &options.url {
        return Some(url.clone());
    }
    if let Some(gw) = &options.gateway {
        if gw != "auto" {
            return Some(gw.clone());
        }
    }
    None
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatProjectionState {
    Delta,
    Final,
}

#[allow(dead_code)]
struct ChatProjection {
    state: ChatProjectionState,
    payload: serde_json::Map<String, Value>,
}

fn as_record(value: &Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    }
}

fn read_optional_string(value: &Value) -> Option<String> {
    if let Value::String(s) = value {
        if !s.is_empty() {
            return Some(s.clone());
        }
    }
    None
}

fn read_optional_timestamp(value: &Value) -> Option<RunTimestamp> {
    if let Value::String(s) = value {
        if !s.is_empty() {
            return Some(s.clone());
        }
    }
    if let Value::Number(n) = value {
        if let Some(f) = n.as_f64() {
            if f.is_finite() {
                return Some(n.to_string());
            }
        }
    }
    None
}

fn read_string_lower<'a>(value: &'a Value) -> Option<String> {
    read_optional_string(value).map(|s| s.to_lowercase())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStatus {
    Accepted,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl From<WaitStatus> for RunStatus {
    fn from(s: WaitStatus) -> Self {
        match s {
            WaitStatus::Accepted => RunStatus::Accepted,
            WaitStatus::Completed => RunStatus::Completed,
            WaitStatus::Failed => RunStatus::Failed,
            WaitStatus::Cancelled => RunStatus::Cancelled,
            WaitStatus::TimedOut => RunStatus::TimedOut,
        }
    }
}

fn run_status_from_wait_payload(value: &Value) -> WaitStatus {
    let record = as_record(value);
    let status = record.get("status").and_then(read_string_lower).unwrap_or_default();
    let stop_reason = record.get("stopReason").and_then(read_string_lower).unwrap_or_default();
    let pending_error = record.get("pendingError").map(|v| v == &Value::Bool(true)).unwrap_or(false);
    let timeout_phase = record.get("timeoutPhase").and_then(read_string_lower);
    let status_already_timeout_attributed = status == "timeout" || status == "timed_out";
    let hard_timeout = !pending_error
        && ((stop_reason != "restart"
            && record.get("providerStarted") == Some(&Value::Bool(true))
            && status_already_timeout_attributed)
            || timeout_phase == Some("preflight".to_string())
            || timeout_phase == Some("provider".to_string())
            || timeout_phase == Some("post_turn".to_string()));
    let has_terminal_timeout_metadata = read_optional_timestamp(record.get("endedAt").unwrap_or(&Value::Null)).is_some()
        || (!pending_error && read_optional_string(record.get("error").unwrap_or(&Value::Null)).is_some())
        || !stop_reason.is_empty()
        || record.get("livenessState").and_then(|v| v.as_str()).is_some()
        || record.get("yielded") == Some(&Value::Bool(true));

    if hard_timeout {
        return WaitStatus::TimedOut;
    }
    if status == "aborted"
        || status == "cancelled"
        || status == "canceled"
        || status == "killed"
        || stop_reason == "aborted"
        || stop_reason == "cancelled"
        || stop_reason == "canceled"
        || stop_reason == "killed"
        || stop_reason == "auth-revoked"
        || stop_reason == "restart"
        || stop_reason == "rpc"
        || stop_reason == "user"
        || (record.get("aborted") == Some(&Value::Bool(true)) && stop_reason == "stop")
    {
        return WaitStatus::Cancelled;
    }
    if status == "ok" || status == "completed" || status == "succeeded" {
        return WaitStatus::Completed;
    }
    if status == "timeout" {
        if stop_reason == "timeout"
            || stop_reason == "timed_out"
            || record.get("aborted") == Some(&Value::Bool(true))
            || has_terminal_timeout_metadata
        {
            return WaitStatus::TimedOut;
        }
        return WaitStatus::Accepted;
    }
    if status == "timed_out" {
        return WaitStatus::TimedOut;
    }
    if status == "accepted" {
        return WaitStatus::Accepted;
    }
    WaitStatus::Failed
}

fn normalize_timeout_ms(timeout_ms: &Option<u64>) -> Option<u64> {
    if let Some(v) = timeout_ms {
        if *v == u64::MAX {
            panic!("timeoutMs must be a finite non-negative number");
        }
        return Some(*v);
    }
    None
}

fn timeout_seconds_from_ms(timeout_ms: Option<u64>) -> Option<u64> {
    let normalized = normalize_timeout_ms(&timeout_ms)?;
    Some(if normalized == 0 { 0 } else { (normalized + 999) / 1000 })
}

fn split_model_ref(model: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(model) = model else {
        return (None, None);
    };
    if let Some(index) = model.find('/') {
        if index == 0 || index == model.len() - 1 {
            return (None, Some(model.to_string()));
        }
        return (
            Some(model[..index].to_string()),
            Some(model[index + 1..].to_string()),
        );
    }
    (None, Some(model.to_string()))
}

fn assert_no_unsupported_run_options(params: &AgentRunParams) -> Result<(), SDKError> {
    let mut unsupported: Vec<&str> = Vec::new();
    if params.workspace.is_some() {
        unsupported.push("workspace");
    }
    if params.runtime.is_some() {
        unsupported.push("runtime");
    }
    if params.environment.is_some() {
        unsupported.push("environment");
    }
    if params.approvals.is_some() {
        unsupported.push("approvals");
    }
    if !unsupported.is_empty() {
        return Err(SDKError {
            code: Some("unsupported".to_string()),
            message: format!(
                "CradleRing Gateway does not support per-run SDK option{} yet: {}",
                if unsupported.len() == 1 { "" } else { "s" },
                unsupported.join(", ")
            ),
            details: None,
        });
    }
    Ok(())
}

fn build_agent_params(params: &AgentRunParams) -> Result<serde_json::Map<String, Value>, SDKError> {
    assert_no_unsupported_run_options(params)?;
    let (provider, model) = split_model_ref(params.model.as_deref());
    let timeout_seconds = timeout_seconds_from_ms(params.timeout_ms);
    let mut map = serde_json::Map::new();
    map.insert("message".to_string(), Value::String(params.input.clone()));
    if let Some(a) = &params.agent_id {
        map.insert("agentId".to_string(), Value::String(a.clone()));
    }
    if let Some(p) = provider {
        map.insert("provider".to_string(), Value::String(p));
    }
    if let Some(m) = model {
        map.insert("model".to_string(), Value::String(m));
    }
    if let Some(s) = &params.session_id {
        map.insert("sessionId".to_string(), Value::String(s.clone()));
    }
    if let Some(s) = &params.session_key {
        map.insert("sessionKey".to_string(), Value::String(s.clone()));
    }
    if let Some(t) = &params.thinking {
        map.insert("thinking".to_string(), Value::String(t.clone()));
    }
    if let Some(d) = params.deliver {
        map.insert("deliver".to_string(), Value::Bool(d));
    }
    if let Some(a) = &params.attachments {
        map.insert("attachments".to_string(), Value::Array(a.clone()));
    }
    if let Some(t) = timeout_seconds {
        map.insert("timeout".to_string(), Value::Number(t.into()));
    }
    if let Some(l) = &params.label {
        map.insert("label".to_string(), Value::String(l.clone()));
    }
    map.insert(
        "idempotencyKey".to_string(),
        Value::String(params.idempotency_key.clone().unwrap_or_else(|| {
            Uuid::new_v4().to_string()
        })),
    );
    Ok(map)
}

fn unsupported_gateway_api(api: &str) -> SDKError {
    SDKError {
        code: Some("unsupported".to_string()),
        message: format!("{} is not supported by the current CradleRing Gateway yet", api),
        details: None,
    }
}

fn has_artifact_query_scope(params: &Value) -> bool {
    let record = as_record(params);
    record.get("sessionKey").and_then(read_optional_string).is_some()
        || record.get("runId").and_then(read_optional_string).is_some()
        || record.get("taskId").and_then(read_optional_string).is_some()
}

fn require_artifact_query_scope(api: &str, params: Value) -> Result<Value, SDKError> {
    if !has_artifact_query_scope(&params) {
        return Err(SDKError {
            code: Some("missing-scope".to_string()),
            message: format!("{} requires one of sessionKey, runId, or taskId", api),
            details: None,
        });
    }
    Ok(params)
}

fn has_tools_effective_session_key(params: &Value) -> bool {
    let record = as_record(params);
    record.get("sessionKey").and_then(read_optional_string).is_some()
}

fn require_tools_effective_session_key(params: Value) -> Result<Value, SDKError> {
    if !has_tools_effective_session_key(&params) {
        return Err(SDKError {
            code: Some("missing-session".to_string()),
            message: "oc.tools.effective requires sessionKey".to_string(),
            details: None,
        });
    }
    Ok(params)
}

#[allow(dead_code)]
fn is_assistant_run_event(event: &OpenClawEvent) -> bool {
    matches!(
        event.r#type,
        crate::types::OpenClawEventType::AssistantDelta
            | crate::types::OpenClawEventType::AssistantMessage
    )
}

#[allow(dead_code)]
fn is_terminal_run_event(event: &OpenClawEvent) -> bool {
    matches!(
        event.r#type,
        crate::types::OpenClawEventType::RunCompleted
            | crate::types::OpenClawEventType::RunFailed
            | crate::types::OpenClawEventType::RunCancelled
            | crate::types::OpenClawEventType::RunTimedOut
    )
}

#[allow(dead_code)]
fn read_chat_projection(event: &OpenClawEvent) -> Option<ChatProjection> {
    let raw = event.raw.as_ref()?;
    if event.r#type != crate::types::OpenClawEventType::Raw || raw.event != "chat" {
        return None;
    }
    let payload = match &raw.payload {
        Some(v) => as_record(v),
        None => return None,
    };
    let state_str = payload.get("state").and_then(|v| v.as_str())?;
    let state = match state_str {
        "delta" => ChatProjectionState::Delta,
        "final" => ChatProjectionState::Final,
        _ => return None,
    };
    Some(ChatProjection { state, payload })
}

#[allow(dead_code)]
fn read_chat_projection_text(payload: &serde_json::Map<String, Value>) -> Option<String> {
    let message = match payload.get("message") {
        Some(v) => as_record(v),
        None => return None,
    };
    let content = message.get("content")?;
    if let Value::String(s) = content {
        return Some(s.clone());
    }
    let Value::Array(parts) = content else {
        return None;
    };
    let mut text = String::new();
    for part in parts {
        let r = as_record(part);
        if r.get("type").and_then(|v| v.as_str()) == Some("text") {
            if let Some(s) = r.get("text").and_then(|v| v.as_str()) {
                text.push_str(s);
            }
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[allow(dead_code)]
fn read_chat_projection_delta_text(payload: &serde_json::Map<String, Value>) -> Option<String> {
    payload
        .get("deltaText")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[allow(dead_code)]
fn read_chat_projection_replace(payload: &serde_json::Map<String, Value>) -> bool {
    payload.get("replace") == Some(&Value::Bool(true))
}

#[allow(dead_code)]
fn normalize_chat_projection_event(
    event: &OpenClawEvent,
    projection: &ChatProjection,
    previous_text: Option<&str>,
) -> OpenClawEvent {
    let text = read_chat_projection_text(&projection.payload);
    let delta_text = read_chat_projection_delta_text(&projection.payload);
    let has_previous_text = previous_text.is_some();
    let is_replacement = read_chat_projection_replace(&projection.payload);
    let mut out = event.clone();
    out.r#type = match projection.state {
        ChatProjectionState::Delta => crate::types::OpenClawEventType::AssistantDelta,
        ChatProjectionState::Final => crate::types::OpenClawEventType::RunCompleted,
    };
    if projection.state == ChatProjectionState::Delta {
        if let Some(t) = text {
            let mut data = serde_json::Map::new();
            data.insert("text".to_string(), Value::String(t.clone()));
            let delta_value = if has_previous_text {
                delta_text.clone().unwrap_or(t)
            } else {
                t
            };
            data.insert("delta".to_string(), Value::String(delta_value));
            if is_replacement {
                data.insert("replace".to_string(), Value::Bool(true));
            }
            out.data = Some(Value::Object(data));
        }
    } else {
        let mut data = serde_json::Map::new();
        data.insert("phase".to_string(), Value::String("end".to_string()));
        if let Some(t) = text {
            data.insert("outputText".to_string(), Value::String(t));
        }
        out.data = Some(Value::Object(data));
    }
    out
}

/// Root SDK client with namespaces for agents, sessions, runs, and gateway APIs.
pub struct OpenClaw {
    pub agents: AgentsNamespace,
    pub sessions: SessionsNamespace,
    pub runs: RunsNamespace,
    pub tasks: TasksNamespace,
    pub models: ModelsNamespace,
    pub tools: ToolsNamespace,
    pub artifacts: ArtifactsNamespace,
    pub approvals: ApprovalsNamespace,
    pub environments: EnvironmentsNamespace,
    transport: Arc<dyn OpenClawTransport>,
    #[allow(dead_code)]
    normalized_events: EventHub<OpenClawEvent>,
    #[allow(dead_code)]
    replay_by_run_id: Arc<Mutex<BTreeMap<String, Vec<OpenClawEvent>>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for OpenClaw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenClaw").finish()
    }
}

impl OpenClaw {
    /// Build a new OpenClaw client from the supplied options.
    pub fn new(options: OpenClawOptions) -> Self {
        let transport = if let Some(t) = options.transport {
            t
        } else {
            // Construct a default client using the provided URL/token/password.
            let opts = GatewayClientTransportOptions {
                url: resolve_gateway_url(&options),
                token: options.token.clone(),
                password: options.password.clone(),
                request_timeout_ms: options.request_timeout_ms,
                ..Default::default()
            };
            let factory: GatewayClientFactory = Box::new(|opts: &GatewayClientTransportOptions| {
                // The SDK does not ship a default websocket implementation; the
                // host crate is expected to register its own factory. We
                // provide a placeholder here that returns a no-op client.
                Box::new(NoopGatewayClient::new(opts.clone_for_callback()))
            });
            Box::new(GatewayClientTransport::new(opts, factory))
        };
        let transport_arc: Arc<dyn OpenClawTransport> = Arc::from(transport);
        Self::with_transport(transport_arc)
    }

    /// Build a new OpenClaw client from a pre-built transport handle.
    pub fn with_transport(transport: Arc<dyn OpenClawTransport>) -> Self {
        let normalized_events = EventHub::new(EventHubOptions {
            replay_limit: Some(MAX_NORMALIZED_REPLAY_EVENTS),
        });
        let replay_by_run_id: Arc<Mutex<BTreeMap<String, Vec<OpenClawEvent>>>> =
            Arc::new(Mutex::new(BTreeMap::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client_arc = ClientArc {
            transport: transport.clone(),
            normalized_events: Arc::new(normalized_events.clone()),
            replay_by_run_id: replay_by_run_id.clone(),
            closed: closed.clone(),
        };
        OpenClaw {
            agents: AgentsNamespace::new(client_arc.clone()),
            sessions: SessionsNamespace::new(client_arc.clone()),
            runs: RunsNamespace::new(client_arc.clone()),
            tasks: TasksNamespace::new(client_arc.clone()),
            models: ModelsNamespace::new(client_arc.clone()),
            tools: ToolsNamespace::new(client_arc.clone()),
            artifacts: ArtifactsNamespace::new(client_arc.clone()),
            approvals: ApprovalsNamespace::new(client_arc.clone()),
            environments: EnvironmentsNamespace::new(client_arc.clone()),
            transport,
            normalized_events,
            replay_by_run_id,
            closed,
        }
    }

    pub async fn connect(&self) -> Result<(), SDKError> {
        self.assert_open();
        if let Some(transport) = self.as_connectable() {
            transport.connect().await?;
        }
        self.assert_open();
        Ok(())
    }

    pub async fn close(&self) -> Result<(), SDKError> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Closing is best-effort because the EventHub is moved into closures.
        self.transport.close().await
    }

    fn assert_open(&self) {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            panic!("OpenClaw SDK client is closed");
        }
    }

    fn as_connectable(&self) -> Option<&dyn ConnectableOpenClawTransport> {
        // Production integrations wrap a `ConnectableOpenClawTransport`. In
        // the absence of a stable downcast we surface a non-connectable handle.
        None
    }

    pub fn raw_events(
        &self,
        filter: Option<Box<dyn Fn(&GatewayEvent) -> bool + Send + Sync>>,
    ) -> Pin<Box<dyn Stream<Item = GatewayEvent> + Send>> {
        self.transport.events(filter)
    }

    #[allow(dead_code)]
    fn record_replay_event(&self, event: &OpenClawEvent) {
        let Some(run_id) = &event.run_id else {
            return;
        };
        let mut map = self.replay_by_run_id.lock().unwrap();
        let entry = map.entry(run_id.clone()).or_insert_with(Vec::new);
        entry.push(event.clone());
        if entry.len() > MAX_REPLAY_EVENTS_PER_RUN {
            let drop = entry.len() - MAX_REPLAY_EVENTS_PER_RUN;
            entry.drain(0..drop);
        }
        if map.len() > MAX_REPLAY_RUNS {
            // Drop the oldest key.
            if let Some(first) = map.keys().next().cloned() {
                map.remove(&first);
            }
        }
    }
}

/// Arc-shaped handle that namespaces use to talk back to the OpenClaw client.
#[derive(Clone)]
pub struct ClientArc {
    pub transport: Arc<dyn OpenClawTransport>,
    pub normalized_events: Arc<EventHub<OpenClawEvent>>,
    pub replay_by_run_id: Arc<Mutex<BTreeMap<String, Vec<OpenClawEvent>>>>,
    pub closed: Arc<std::sync::atomic::AtomicBool>,
}

impl std::fmt::Debug for ClientArc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientArc").finish()
    }
}

impl ClientArc {
    pub fn raw_events(
        &self,
        filter: Option<Box<dyn Fn(&GatewayEvent) -> bool + Send + Sync>>,
    ) -> Pin<Box<dyn Stream<Item = GatewayEvent> + Send>> {
        self.transport.events(filter)
    }
}

/// Agent-scoped helper for runs and identity lookups.
pub struct Agent {
    pub id: String,
    client: ClientArc,
}

impl Agent {
    pub fn new(client: ClientArc, id: impl Into<String>) -> Self {
        Agent {
            id: id.into(),
            client,
        }
    }

    pub async fn run(&self, input: AgentRunInput) -> Result<Run, SDKError> {
        let params = match input {
            AgentRunInput::Simple(s) => AgentRunParams {
                input: s,
                agent_id: Some(self.id.clone()),
                ..Default::default()
            },
            AgentRunInput::Params(p) => {
                let mut p = p;
                p.agent_id = Some(self.id.clone());
                p
            }
        };
        self.client.runs_namespace().create(params).await
    }
}

pub enum AgentRunInput {
    Simple(String),
    Params(AgentRunParams),
}

impl From<&str> for AgentRunInput {
    fn from(s: &str) -> Self {
        AgentRunInput::Simple(s.to_string())
    }
}

impl From<String> for AgentRunInput {
    fn from(s: String) -> Self {
        AgentRunInput::Simple(s)
    }
}

impl From<AgentRunParams> for AgentRunInput {
    fn from(p: AgentRunParams) -> Self {
        AgentRunInput::Params(p)
    }
}

/// Run handle for streaming events, waiting, and cancellation.
pub struct Run {
    pub id: String,
    pub session_key: Option<String>,
    client: ClientArc,
}

impl Run {
    pub fn new(client: ClientArc, id: impl Into<String>, session_key: Option<String>) -> Self {
        Run {
            id: id.into(),
            session_key,
            client,
        }
    }

    pub async fn wait(&self, _options: Option<RunWaitOptions>) -> Result<RunResult, SDKError> {
        let mut params = serde_json::Map::new();
        params.insert("runId".to_string(), Value::String(self.id.clone()));
        let raw = self
            .client
            .transport
            .request("agent.wait", Some(Value::Object(params)), None)
            .await?;
        let record = as_record(&raw);
        let status: RunStatus = run_status_from_wait_payload(&raw).into();
        let error = if let Some(err_str) = record.get("error").and_then(read_optional_string) {
            Some(SDKError {
                code: None,
                message: err_str.clone(),
                details: None,
            })
        } else {
            None
        };
        Ok(RunResult {
            run_id: self.id.clone(),
            status,
            session_id: record.get("sessionId").and_then(read_optional_string),
            session_key: record
                .get("sessionKey")
                .and_then(read_optional_string)
                .or_else(|| self.session_key.clone()),
            task_id: None,
            started_at: read_optional_timestamp(record.get("startedAt").unwrap_or(&Value::Null)),
            ended_at: read_optional_timestamp(record.get("endedAt").unwrap_or(&Value::Null)),
            output: None,
            usage: None,
            artifacts: None,
            error,
            raw: Some(raw),
        })
    }

    pub async fn cancel(&self) -> Result<Value, SDKError> {
        let mut params = serde_json::Map::new();
        params.insert("runId".to_string(), Value::String(self.id.clone()));
        if let Some(k) = &self.session_key {
            params.insert("key".to_string(), Value::String(k.clone()));
        }
        self.client
            .transport
            .request("sessions.abort", Some(Value::Object(params)), None)
            .await
    }
}

#[derive(Default, Clone, Debug)]
pub struct RunWaitOptions {
    pub timeout_ms: Option<u64>,
}

/// Session handle for sending messages and session-scoped mutations.
pub struct Session {
    pub key: String,
    pub info: Option<Value>,
    client: ClientArc,
}

impl Session {
    pub fn new(client: ClientArc, key: impl Into<String>, info: Option<Value>) -> Self {
        Session {
            key: key.into(),
            info,
            client,
        }
    }

    pub async fn send(&self, _input: SessionSendInput) -> Result<Run, SDKError> {
        Err(SDKError {
            code: Some("not-implemented".to_string()),
            message: "Session::send requires an async runtime".to_string(),
            details: None,
        })
    }

    pub async fn abort(&self, _run_id: Option<String>) -> Result<Value, SDKError> {
        Err(SDKError {
            code: Some("not-implemented".to_string()),
            message: "Session::abort requires an async runtime".to_string(),
            details: None,
        })
    }

    pub async fn patch(&self, params: serde_json::Map<String, Value>) -> Result<Value, SDKError> {
        let mut merged = params;
        merged.insert("key".to_string(), Value::String(self.key.clone()));
        let v = Value::Object(merged);
        self.client
            .transport
            .request("sessions.patch", Some(v), None)
            .await
    }

    pub async fn compact(&self) -> Result<Value, SDKError> {
        let mut params = serde_json::Map::new();
        params.insert("key".to_string(), Value::String(self.key.clone()));
        let v = Value::Object(params);
        self.client
            .transport
            .request("sessions.compact", Some(v), None)
            .await
    }
}

pub enum SessionSendInput {
    Simple(String),
    Params(SessionSendParams),
}

impl From<&str> for SessionSendInput {
    fn from(s: &str) -> Self {
        SessionSendInput::Simple(s.to_string())
    }
}

impl From<String> for SessionSendInput {
    fn from(s: String) -> Self {
        SessionSendInput::Simple(s)
    }
}

impl From<SessionSendParams> for SessionSendInput {
    fn from(p: SessionSendParams) -> Self {
        SessionSendInput::Params(p)
    }
}

/// Agent management namespace.
pub struct AgentsNamespace {
    client: ClientArc,
}

impl AgentsNamespace {
    pub fn new(client: ClientArc) -> Self {
        AgentsNamespace { client }
    }

    pub async fn list(&self, params: Option<Value>) -> Result<Value, SDKError> {
        self.client
            .transport
            .request("agents.list", params.or_else(|| Some(Value::Object(serde_json::Map::new()))), None)
            .await
    }

    pub fn get(&self, id: impl Into<String>) -> Agent {
        Agent::new(self.client.clone(), id)
    }

    pub async fn create(&self, params: AgentsCreateParams) -> Result<Value, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        self.client.transport.request("agents.create", Some(v), None).await
    }

    pub async fn update(&self, params: AgentsUpdateParams) -> Result<Value, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        self.client.transport.request("agents.update", Some(v), None).await
    }

    pub async fn delete(&self, params: AgentsDeleteParams) -> Result<Value, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        self.client.transport.request("agents.delete", Some(v), None).await
    }
}

/// Session management namespace.
pub struct SessionsNamespace {
    client: ClientArc,
}

impl SessionsNamespace {
    pub fn new(client: ClientArc) -> Self {
        SessionsNamespace { client }
    }

    pub async fn list(&self, params: Option<Value>) -> Result<Value, SDKError> {
        self.client
            .transport
            .request("sessions.list", params.or_else(|| Some(Value::Object(serde_json::Map::new()))), None)
            .await
    }

    pub async fn create(&self, params: SessionCreateParams) -> Result<Session, SDKError> {
        let v = serde_json::to_value(&params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        let raw = self.client.transport.request("sessions.create", Some(v), None).await?;
        let record = as_record(&raw);
        let key = record
            .get("key")
            .and_then(read_optional_string)
            .or_else(|| record.get("sessionKey").and_then(read_optional_string))
            .or_else(|| params.key.clone())
            .ok_or_else(|| SDKError {
                code: Some("missing-key".to_string()),
                message: "sessions.create did not return a session key".to_string(),
                details: None,
            })?;
        Ok(Session::new(self.client.clone(), key, Some(raw)))
    }

    pub fn get(&self, target: SessionTargetOrKey) -> Session {
        let key = match target {
            SessionTargetOrKey::Key(k) => k,
            SessionTargetOrKey::Target(t) => t.key,
        };
        Session::new(self.client.clone(), key, None)
    }

    pub async fn resolve(&self, params: serde_json::Map<String, Value>) -> Result<Value, SDKError> {
        let v = Value::Object(params);
        self.client.transport.request("sessions.resolve", Some(v), None).await
    }

    pub async fn send(&self, _input: SessionSendParams) -> Result<Run, SDKError> {
        Err(SDKError {
            code: Some("not-implemented".to_string()),
            message: "SessionsNamespace::send requires an async runtime".to_string(),
            details: None,
        })
    }
}

pub enum SessionTargetOrKey {
    Key(String),
    Target(SessionTarget),
}

impl From<&str> for SessionTargetOrKey {
    fn from(s: &str) -> Self {
        SessionTargetOrKey::Key(s.to_string())
    }
}

impl From<String> for SessionTargetOrKey {
    fn from(s: String) -> Self {
        SessionTargetOrKey::Key(s)
    }
}

impl From<SessionTarget> for SessionTargetOrKey {
    fn from(t: SessionTarget) -> Self {
        SessionTargetOrKey::Target(t)
    }
}

/// Run creation and lifecycle namespace.
pub struct RunsNamespace {
    client: ClientArc,
}

impl RunsNamespace {
    pub fn new(client: ClientArc) -> Self {
        RunsNamespace { client }
    }

    pub async fn create(&self, params: RunCreateParams) -> Result<Run, SDKError> {
        let normalized = normalize_timeout_ms(&params.timeout_ms);
        let params_obj = build_agent_params(&params)?;
        let options = if let Some(t) = normalized {
            Some(GatewayRequestOptions::new().timeout_ms(Some(if t == 0 { 0 } else { t })))
        } else {
            None
        };
        let raw = self
            .client
            .transport
            .request("agent", Some(Value::Object(params_obj)), options)
            .await?;
        let record = as_record(&raw);
        let run_id = record.get("runId").and_then(read_optional_string).ok_or_else(|| SDKError {
            code: Some("missing-runId".to_string()),
            message: "agent did not return a runId".to_string(),
            details: None,
        })?;
        let session_key = record
            .get("sessionKey")
            .and_then(read_optional_string)
            .or_else(|| params.session_key.clone());
        Ok(Run::new(self.client.clone(), run_id, session_key))
    }

    pub fn get(&self, run_id: impl Into<String>) -> Run {
        Run::new(self.client.clone(), run_id, None)
    }

    pub async fn wait(&self, run_id: impl Into<String>, options: Option<RunWaitOptions>) -> Result<RunResult, SDKError> {
        Run::new(self.client.clone(), run_id, None).wait(options).await
    }

    pub async fn cancel(&self, run_id: impl Into<String>, session_key: Option<String>) -> Result<Value, SDKError> {
        Run::new(self.client.clone(), run_id, session_key).cancel().await
    }
}

trait RunsNamespaceAccess {
    fn runs_namespace(&self) -> RunsNamespace;
}

impl RunsNamespaceAccess for ClientArc {
    fn runs_namespace(&self) -> RunsNamespace {
        RunsNamespace::new(self.clone())
    }
}

/// Base namespace that proxies RPC calls under a prefix.
pub struct RpcNamespace {
    client: ClientArc,
    prefix: String,
}

impl RpcNamespace {
    pub fn new(client: ClientArc, prefix: impl Into<String>) -> Self {
        RpcNamespace {
            client,
            prefix: prefix.into(),
        }
    }

    pub async fn call(
        &self,
        method: &str,
        params: Option<Value>,
        options: Option<GatewayRequestOptions>,
    ) -> Result<Value, SDKError> {
        let full = format!("{}.{}", self.prefix, method);
        self.client.transport.request(&full, params, options).await
    }
}

/// Task query and cancellation namespace.
pub struct TasksNamespace {
    inner: RpcNamespace,
}

impl TasksNamespace {
    pub fn new(client: ClientArc) -> Self {
        TasksNamespace {
            inner: RpcNamespace::new(client, "tasks"),
        }
    }

    pub async fn list(&self, params: Option<TaskListParamsInput>) -> Result<TasksListResult, SDKError> {
        let p = params.unwrap_or_else(TaskListParamsInput::empty);
        let v = serde_json::to_value(p.params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        let raw = self.inner.call("list", Some(v), None).await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub async fn get(&self, task_id: impl Into<String>) -> Result<TasksGetResult, SDKError> {
        let mut params = serde_json::Map::new();
        params.insert("taskId".to_string(), Value::String(task_id.into()));
        let raw = self
            .inner
            .call("get", Some(Value::Object(params)), None)
            .await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub async fn cancel(&self, task_id: impl Into<String>, options: Option<TaskCancelOptions>) -> Result<TasksCancelResult, SDKError> {
        let mut params = serde_json::Map::new();
        params.insert("taskId".to_string(), Value::String(task_id.into()));
        if let Some(reason) = options.and_then(|o| o.reason) {
            params.insert("reason".to_string(), Value::String(reason));
        }
        let raw = self
            .inner
            .call("cancel", Some(Value::Object(params)), None)
            .await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }
}

#[derive(Default, Clone, Debug)]
pub struct TaskCancelOptions {
    pub reason: Option<String>,
}

pub struct TaskListParamsInput {
    pub params: TasksListParams,
}

impl TaskListParamsInput {
    pub fn empty() -> Self {
        TaskListParamsInput {
            params: TasksListParams::default(),
        }
    }
}

/// Model catalog and auth status namespace.
pub struct ModelsNamespace {
    inner: RpcNamespace,
}

impl ModelsNamespace {
    pub fn new(client: ClientArc) -> Self {
        ModelsNamespace {
            inner: RpcNamespace::new(client, "models"),
        }
    }

    pub async fn list(&self, params: Option<Value>) -> Result<Value, SDKError> {
        self.inner
            .call("list", params.or_else(|| Some(Value::Object(serde_json::Map::new()))), None)
            .await
    }

    pub async fn status(&self, params: Option<Value>) -> Result<Value, SDKError> {
        self.inner.call("authStatus", params, None).await
    }
}

/// Tool catalog, effective tool, and direct invocation namespace.
pub struct ToolsNamespace {
    inner: RpcNamespace,
}

impl ToolsNamespace {
    pub fn new(client: ClientArc) -> Self {
        ToolsNamespace {
            inner: RpcNamespace::new(client, "tools"),
        }
    }

    pub async fn list(&self, params: Option<Value>) -> Result<Value, SDKError> {
        self.inner
            .call("catalog", params.or_else(|| Some(Value::Object(serde_json::Map::new()))), None)
            .await
    }

    pub async fn effective(&self, params: ToolsEffectiveParams) -> Result<Value, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        let v = require_tools_effective_session_key(v)?;
        self.inner.call("effective", Some(v), None).await
    }

    pub async fn invoke(&self, name: impl Into<String>, params: Option<ToolInvokeParams>) -> Result<ToolInvokeResult, SDKError> {
        let mut req = serde_json::Map::new();
        req.insert("name".to_string(), Value::String(name.into()));
        req.insert(
            "conversationReadOrigin".to_string(),
            Value::String("direct-operator".to_string()),
        );
        if let Some(p) = params {
            if let Some(args) = p.args {
                req.insert("args".to_string(), Value::Object(args));
            }
            if let Some(s) = p.session_key {
                req.insert("sessionKey".to_string(), Value::String(s));
            }
            if let Some(a) = p.agent_id {
                req.insert("agentId".to_string(), Value::String(a));
            }
            if let Some(c) = p.confirm {
                req.insert("confirm".to_string(), Value::Bool(c));
            }
            if let Some(k) = p.idempotency_key {
                req.insert("idempotencyKey".to_string(), Value::String(k));
            }
        }
        let raw = self.inner.call("invoke", Some(Value::Object(req)), None).await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }
}

/// Run/session artifact listing and download namespace.
pub struct ArtifactsNamespace {
    inner: RpcNamespace,
}

impl ArtifactsNamespace {
    pub fn new(client: ClientArc) -> Self {
        ArtifactsNamespace {
            inner: RpcNamespace::new(client, "artifacts"),
        }
    }

    pub async fn list(&self, params: ArtifactQuery) -> Result<ArtifactsListResult, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        let v = require_artifact_query_scope("oc.artifacts.list", v)?;
        let raw = self.inner.call("list", Some(v), None).await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub async fn get(&self, id: impl Into<String>, params: ArtifactQuery) -> Result<ArtifactsGetResult, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        let mut v = require_artifact_query_scope("oc.artifacts.get", v)?;
        if let Value::Object(obj) = &mut v {
            obj.insert("artifactId".to_string(), Value::String(id.into()));
        }
        let raw = self.inner.call("get", Some(v), None).await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub async fn download(
        &self,
        id: impl Into<String>,
        params: ArtifactQuery,
    ) -> Result<ArtifactsDownloadResult, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        let mut v = require_artifact_query_scope("oc.artifacts.download", v)?;
        if let Value::Object(obj) = &mut v {
            obj.insert("artifactId".to_string(), Value::String(id.into()));
        }
        let raw = self.inner.call("download", Some(v), None).await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }
}

/// Approval request listing and response namespace.
pub struct ApprovalsNamespace {
    client: ClientArc,
}

impl ApprovalsNamespace {
    pub fn new(client: ClientArc) -> Self {
        ApprovalsNamespace { client }
    }

    pub async fn list(&self, params: Option<Value>) -> Result<Value, SDKError> {
        self.client
            .transport
            .request("exec.approval.list", params.or_else(|| Some(Value::Object(serde_json::Map::new()))), None)
            .await
    }

    pub async fn respond(&self, approval_id: impl Into<String>, params: ApprovalDecisionParams) -> Result<Value, SDKError> {
        let decision_str = match params.decision {
            crate::types::ApprovalDecision::AllowOnce => "allow-once",
            crate::types::ApprovalDecision::AllowAlways => "allow-always",
            crate::types::ApprovalDecision::Deny => "deny",
        };
        let mut req = serde_json::Map::new();
        req.insert("id".to_string(), Value::String(approval_id.into()));
        req.insert("decision".to_string(), Value::String(decision_str.to_string()));
        self.client
            .transport
            .request("exec.approval.resolve", Some(Value::Object(req)), None)
            .await
    }
}

/// Environment discovery namespace.
pub struct EnvironmentsNamespace {
    inner: RpcNamespace,
}

impl EnvironmentsNamespace {
    pub fn new(client: ClientArc) -> Self {
        EnvironmentsNamespace {
            inner: RpcNamespace::new(client, "environments"),
        }
    }

    pub async fn list(&self, params: Option<Value>) -> Result<EnvironmentsListResult, SDKError> {
        let v = params.or_else(|| Some(Value::Object(serde_json::Map::new())));
        let raw = self.inner.call("list", v, None).await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub async fn create(&self, params: EnvironmentCreateParams) -> Result<EnvironmentSummary, SDKError> {
        let v = serde_json::to_value(params).map_err(|e| SDKError {
            code: Some("encoding".to_string()),
            message: e.to_string(),
            details: None,
        })?;
        let raw = self.inner.call("create", Some(v), None).await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub async fn status(&self, environment_id: impl Into<String>) -> Result<EnvironmentSummary, SDKError> {
        let mut req = serde_json::Map::new();
        req.insert("environmentId".to_string(), Value::String(environment_id.into()));
        let raw = self
            .inner
            .call("status", Some(Value::Object(req)), None)
            .await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub async fn destroy(&self, environment_id: impl Into<String>) -> Result<EnvironmentSummary, SDKError> {
        let mut req = serde_json::Map::new();
        req.insert("environmentId".to_string(), Value::String(environment_id.into()));
        let raw = self
            .inner
            .call("destroy", Some(Value::Object(req)), None)
            .await?;
        serde_json::from_value(raw).map_err(|e| SDKError {
            code: Some("decoding".to_string()),
            message: e.to_string(),
            details: None,
        })
    }

    pub fn delete(&self, _environment_id: impl Into<String>) -> Result<EnvironmentSummary, SDKError> {
        Err(unsupported_gateway_api("oc.environments.delete"))
    }
}

/// Placeholder gateway client used when no concrete factory is provided.
/// Hosts should install a real websocket-backed implementation.
pub struct NoopGatewayClient {
    #[allow(dead_code)]
    options: GatewayClientTransportOptions,
}

impl NoopGatewayClient {
    pub fn new(options: GatewayClientTransportOptions) -> Self {
        NoopGatewayClient { options }
    }
}

impl GatewayClientLike for NoopGatewayClient {
    fn start(&self) {}
    fn stop_and_wait(&self) -> Pin<Box<dyn std::future::Future<Output = Result<(), SDKError>> + Send>> {
        Box::pin(async { Ok(()) })
    }
    fn request(
        &self,
        _method: &str,
        _params: Option<Value>,
        _options: Option<GatewayRequestOptions>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, SDKError>> + Send>> {
        Box::pin(async {
            Err(SDKError {
                code: Some("not-connected".to_string()),
                message: "default no-op gateway client cannot issue RPC calls".to_string(),
                details: None,
            })
        })
    }
}
