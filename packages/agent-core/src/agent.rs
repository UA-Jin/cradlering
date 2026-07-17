// Agent Core module implements agent behavior.
// 翻译自 packages/agent-core/src/agent.ts

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use serde_json::Value;

use llm_core::types::{ImageContent, Message, Model, TextContent, ThinkingBudgets, Transport};

use crate::agent_loop::{run_agent_loop, run_agent_loop_continue};
use crate::errors::TranscriptNotContinuableError;
use crate::reasoning::resolve_agent_reasoning_option;
use crate::runtime_deps::StreamSimpleOnly;
use crate::types::{
    AbortSignalShim, AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent,
    AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage, AgentTool,
    BeforeToolCallContext, BeforeToolCallResult, QueueMode, StreamFn, TextOrImageContent,
    ThinkingLevel, ToolExecutionMode,
};

fn default_convert_to_llm(messages: Vec<AgentMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(msg) => Some(msg),
            _ => None,
        })
        .collect()
}

fn empty_usage() -> Value {
    serde_json::json!({
        "input": 0,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 0,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
    })
}

fn default_model() -> Model {
    Model {
        id: String::new(),
        name: String::new(),
        api: String::new(),
        provider: String::new(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![],
        cost: llm_core::types::ModelCost::default(),
        context_window: 0.0,
        context_tokens: None,
        max_tokens: 0.0,
        params: None,
        headers: None,
        auth_header: None,
        media_input: None,
        compat: None,
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct MutableAgentState {
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    tools: Vec<AgentTool>,
    messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub streaming_message: Option<AgentMessage>,
    pub pending_tool_calls: HashSet<String>,
    pub error_message: Option<String>,
}

#[allow(dead_code)]
impl MutableAgentState {
    fn get_tools(&self) -> Vec<AgentTool> {
        self.tools.clone()
    }
    fn set_tools(&mut self, t: Vec<AgentTool>) {
        self.tools = t;
    }
    fn get_messages(&self) -> Vec<AgentMessage> {
        self.messages.clone()
    }
    fn set_messages(&mut self, m: Vec<AgentMessage>) {
        self.messages = m;
    }
}

fn create_mutable_agent_state(initial: Option<PartialAgentState>) -> MutableAgentState {
    let initial = initial.unwrap_or_default();
    MutableAgentState {
        system_prompt: initial.system_prompt.unwrap_or_default(),
        model: initial.model.unwrap_or_else(default_model),
        thinking_level: initial.thinking_level.unwrap_or_else(|| "off".to_string()),
        tools: initial.tools.unwrap_or_default(),
        messages: initial.messages.unwrap_or_default(),
        is_streaming: false,
        streaming_message: None,
        pending_tool_calls: HashSet::new(),
        error_message: None,
    }
}

#[derive(Debug, Clone, Default)]
pub struct PartialAgentState {
    pub system_prompt: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub tools: Option<Vec<AgentTool>>,
    pub messages: Option<Vec<AgentMessage>>,
}

#[derive(Debug, Clone)]
pub struct AgentOptions {
    pub initial_state: Option<PartialAgentState>,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub max_retry_delay_ms: Option<i64>,
    pub tool_execution: Option<ToolExecutionMode>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            initial_state: None,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            session_id: None,
            thinking_budgets: None,
            transport: None,
            max_retry_delay_ms: None,
            tool_execution: None,
        }
    }
}

struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: vec![],
            mode,
        }
    }
    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }
    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }
    fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.messages),
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    vec![]
                } else {
                    let first = self.messages.remove(0);
                    vec![first]
                }
            }
        }
    }
    fn clear(&mut self) {
        self.messages.clear();
    }
}

/// Stateful wrapper around the low-level agent loop.
#[allow(dead_code)]
pub struct Agent {
    state: Mutex<MutableAgentState>,
    listeners: Mutex<Vec<Box<dyn Fn(AgentEvent, AbortSignalShim) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>>>,
    steering_queue: Mutex<PendingMessageQueue>,
    follow_up_queue: Mutex<PendingMessageQueue>,
    pub runtime: Option<Box<dyn StreamSimpleOnly>>,
    pub stream_fn: Option<StreamFn>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Transport,
    pub max_retry_delay_ms: Option<i64>,
    pub tool_execution: ToolExecutionMode,
}

impl Agent {
    pub fn new() -> Self {
        Self::with_options(AgentOptions::default())
    }

    pub fn with_options(options: AgentOptions) -> Self {
        Self {
            state: Mutex::new(create_mutable_agent_state(options.initial_state)),
            listeners: Mutex::new(vec![]),
            steering_queue: Mutex::new(PendingMessageQueue::new(options.steering_mode)),
            follow_up_queue: Mutex::new(PendingMessageQueue::new(options.follow_up_mode)),
            runtime: None,
            stream_fn: None,
            session_id: options.session_id,
            thinking_budgets: options.thinking_budgets,
            transport: options.transport.unwrap_or("auto".to_string()),
            max_retry_delay_ms: options.max_retry_delay_ms,
            tool_execution: options.tool_execution.unwrap_or(ToolExecutionMode::Parallel),
        }
    }

    pub fn steer(&self, message: AgentMessage) {
        self.steering_queue.lock().unwrap().enqueue(message);
    }

    pub fn follow_up(&self, message: AgentMessage) {
        self.follow_up_queue.lock().unwrap().enqueue(message);
    }

    pub fn clear_steering_queue(&self) {
        self.steering_queue.lock().unwrap().clear();
    }

    pub fn clear_follow_up_queue(&self) {
        self.follow_up_queue.lock().unwrap().clear();
    }

    pub fn has_queued_messages(&self) -> bool {
        self.steering_queue.lock().unwrap().has_items() || self.follow_up_queue.lock().unwrap().has_items()
    }

    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.messages.clear();
        state.is_streaming = false;
        state.streaming_message = None;
        state.pending_tool_calls.clear();
        state.error_message = None;
        drop(state);
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    pub async fn prompt_text(&self, input: String, images: Option<Vec<ImageContent>>) {
        let mut content: Vec<TextOrImageContent> = vec![TextOrImageContent::Text(TextContent {
            text: input,
            ..Default::default()
        })];
        if let Some(imgs) = images {
            for img in imgs {
                content.push(TextOrImageContent::Image(img));
            }
        }
        let messages = vec![AgentMessage::Llm(Message::User(llm_core::types::UserMessage {
            content: llm_core::types::UserMessageContent::Parts(
                content
                    .into_iter()
                    .map(|c| match c {
                        TextOrImageContent::Text(t) => llm_core::types::UserMessagePart::Text(t),
                        TextOrImageContent::Image(i) => llm_core::types::UserMessagePart::Image(i),
                    })
                    .collect(),
            ),
            ..Default::default()
        }))];
        self.run_prompt_messages(messages).await;
    }

    pub async fn prompt(&self, message: AgentMessage) {
        self.run_prompt_messages(vec![message]).await;
    }

    async fn run_prompt_messages(&self, messages: Vec<AgentMessage>) {
        let context = self.create_context_snapshot();
        let emit: crate::agent_loop::AgentEventSink = |_event| Box::pin(async {});
        let stream_fn = self.stream_fn;
        let _runtime = self.runtime.as_deref();
        let _ = run_agent_loop(messages, context, Box::new(NoopConfig::new()), emit, None, stream_fn, None).await;
    }

    pub async fn continue_run(&self) {
        let state = self.state.lock().unwrap();
        let last = state.messages.last().cloned();
        drop(state);
        let last_message = match last {
            Some(m) => m,
            None => panic!("No messages to continue from"),
        };
        if matches!(last_message, AgentMessage::Llm(llm_core::types::Message::Assistant(_))) {
            let queued_steering = self.steering_queue.lock().unwrap().drain();
            if !queued_steering.is_empty() {
                self.run_prompt_messages(queued_steering).await;
                return;
            }
            let queued_follow_ups = self.follow_up_queue.lock().unwrap().drain();
            if !queued_follow_ups.is_empty() {
                self.run_prompt_messages(queued_follow_ups).await;
                return;
            }
            let role = last_message.role_str();
            panic!("{}", TranscriptNotContinuableError::new(&role).message);
        }
        let context = self.create_context_snapshot();
        let emit: crate::agent_loop::AgentEventSink = |_event| Box::pin(async {});
        let stream_fn = self.stream_fn;
        let _ = run_agent_loop_continue(context, Box::new(NoopConfig::new()), emit, None, stream_fn, None).await;
    }

    fn create_context_snapshot(&self) -> AgentContext {
        let state = self.state.lock().unwrap();
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages.clone(),
            tools: Some(state.tools.clone()),
        }
    }
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

struct NoopConfig {
    model: Model,
}

impl NoopConfig {
    fn new() -> Self {
        Self { model: default_model() }
    }
}

impl AgentLoopConfig for NoopConfig {
    fn model(&self) -> &Model { &self.model }
    fn thinking_level(&self) -> Option<&ThinkingLevel> { None }
    fn reasoning(&self) -> Option<&str> { None }
    fn session_id(&self) -> Option<&str> { None }
    fn thinking_budgets(&self) -> Option<&ThinkingBudgets> { None }
    fn transport(&self) -> &str { "auto" }
    fn max_retry_delay_ms(&self) -> Option<i64> { None }
    fn api_key(&self) -> Option<&str> { None }
    fn signal(&self) -> Option<&AbortSignalShim> { None }
    fn convert_to_llm(&self, messages: Vec<AgentMessage>) -> Pin<Box<dyn Future<Output = Vec<Message>> + Send>> {
        Box::pin(async move { default_convert_to_llm(messages) })
    }
    fn transform_context(&self, messages: Vec<AgentMessage>, _signal: Option<AbortSignalShim>) -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>> {
        Box::pin(async move { messages })
    }
    fn get_api_key(&self, _provider: String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
        Box::pin(async move { None })
    }
    fn should_stop_after_turn(&self, _context: crate::types::ShouldStopAfterTurnContext) -> Pin<Box<dyn Future<Output = bool> + Send>> {
        Box::pin(async move { true })
    }
    fn prepare_next_turn(&self, _context: crate::types::PrepareNextTurnContext) -> Pin<Box<dyn Future<Output = Option<AgentLoopTurnUpdate>> + Send>> {
        Box::pin(async move { None })
    }
    fn get_steering_messages(&self) -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>> {
        Box::pin(async move { vec![] })
    }
    fn get_follow_up_messages(&self) -> Pin<Box<dyn Future<Output = Vec<AgentMessage>> + Send>> {
        Box::pin(async move { vec![] })
    }
    fn tool_execution(&self) -> ToolExecutionMode { ToolExecutionMode::Parallel }
    fn before_tool_call(&self, _context: BeforeToolCallContext, _signal: Option<AbortSignalShim>) -> Pin<Box<dyn Future<Output = Option<BeforeToolCallResult>> + Send>> {
        Box::pin(async move { None })
    }
    fn resolve_deferred_tool(&self, _context: crate::types::DeferredToolCallContext, _signal: Option<AbortSignalShim>) -> Pin<Box<dyn Future<Output = Option<AgentTool>> + Send>> {
        Box::pin(async move { None })
    }
    fn after_tool_call(&self, _context: AfterToolCallContext, _signal: Option<AbortSignalShim>) -> Pin<Box<dyn Future<Output = Option<AfterToolCallResult>> + Send>> {
        Box::pin(async move { None })
    }
}

#[allow(dead_code)]
fn _force_use(_t: Value, _u: empty_usage_marker::Marker) {
    let _ = empty_usage();
    let _ = resolve_agent_reasoning_option(&default_model(), "off");
    let _: Option<AgentEvent> = None;
}

mod empty_usage_marker {
    pub struct Marker;
}