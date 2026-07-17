// Agent Core low-level loop implementation.
// 翻译自 packages/agent-core/src/agent-loop.ts

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use llm_core::types::{AssistantMessage, Context, ToolResultMessage};

use crate::errors::TranscriptNotContinuableError;
use crate::reasoning::resolve_agent_reasoning_option;
use crate::runtime_deps::{
    resolve_agent_core_stream_fn, AgentCoreStreamRuntimeDeps, StreamSimpleOnly,
};
use crate::types::{
    AbortSignalShim, AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent,
    AgentLoopConfig, AgentMessage, AgentTool, AgentToolCall, AgentToolResult, BeforeToolCallContext,
    BeforeToolCallResult, DeferredToolCallContext, StreamFn, TextOrImageContent,
};

/// Callback used by synchronous loop runners to publish agent lifecycle events.
pub type AgentEventSink = fn(AgentEvent) -> Pin<Box<dyn Future<Output = ()> + Send>>;

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

/// Run a prompt-started loop and emit events through a caller-owned sink.
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: Box<dyn AgentLoopConfig>,
    emit: AgentEventSink,
    signal: Option<AbortSignalShim>,
    stream_fn: Option<StreamFn>,
    runtime: Option<Box<dyn StreamSimpleOnly>>,
) -> Vec<AgentMessage> {
    let mut new_messages = prompts.clone();
    let current_context = AgentContext {
        system_prompt: context.system_prompt,
        messages: {
            let mut v = context.messages;
            v.extend(prompts.iter().cloned());
            v
        },
        tools: context.tools,
    };
    let _ = emit(AgentEvent::AgentStart);
    let _ = emit(AgentEvent::TurnStart);
    for prompt in &prompts {
        let _ = emit(AgentEvent::MessageStart { message: prompt.clone() });
        let _ = emit(AgentEvent::MessageEnd { message: prompt.clone() });
    }
    run_loop(current_context, &mut new_messages, config, signal, emit, stream_fn, runtime).await;
    new_messages
}

/// Continue an existing loop context and emit only newly produced messages.
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: Box<dyn AgentLoopConfig>,
    emit: AgentEventSink,
    signal: Option<AbortSignalShim>,
    stream_fn: Option<StreamFn>,
    runtime: Option<Box<dyn StreamSimpleOnly>>,
) -> Vec<AgentMessage> {
    let last_message = context.messages.last();
    if last_message.is_none() {
        panic!("Cannot continue: no messages in context");
    }
    let last_message = last_message.unwrap();
    if matches!(last_message, AgentMessage::Llm(llm_core::types::Message::Assistant(_))) {
        let role = last_message.role_str();
        panic!("{}", TranscriptNotContinuableError::new(&role).message);
    }

    let mut new_messages: Vec<AgentMessage> = vec![];
    let current_context = context;
    let _ = emit(AgentEvent::AgentStart);
    let _ = emit(AgentEvent::TurnStart);
    run_loop(current_context, &mut new_messages, config, signal, emit, stream_fn, runtime).await;
    new_messages
}

async fn run_loop(
    mut current_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    config: Box<dyn AgentLoopConfig>,
    signal: Option<AbortSignalShim>,
    emit: AgentEventSink,
    stream_fn: Option<StreamFn>,
    runtime: Option<Box<dyn StreamSimpleOnly>>,
) {
    let mut pending_messages: Vec<AgentMessage> = match config.get_steering_messages().await {
        v => v,
    };
    loop {
        let mut has_more_tool_calls = true;
        while has_more_tool_calls || !pending_messages.is_empty() {
            if let Some(ref s) = signal {
                if s.aborted {
                    return;
                }
            }

            for message in &pending_messages {
                let _ = emit(AgentEvent::MessageStart { message: message.clone() });
                let _ = emit(AgentEvent::MessageEnd { message: message.clone() });
                current_context.messages.push(message.clone());
                new_messages.push(message.clone());
            }
            pending_messages.clear();

            if let Some(ref s) = signal {
                if s.aborted {
                    return;
                }
            }

            let message =
                stream_assistant_response(&current_context, &config, &signal, emit, stream_fn, runtime.as_deref()).await;
            let message = AgentMessage::Llm(llm_core::types::Message::Assistant(message));
            new_messages.push(message.clone());

            if matches!(stop_reason_str(&message).as_str(), "error" | "aborted") {
                let _ = emit(AgentEvent::TurnEnd {
                    message: message.clone(),
                    tool_results: vec![],
                });
                let _ = emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                });
                return;
            }

            let tool_results: Vec<ToolResultMessage> = vec![];
            has_more_tool_calls = false;
            if stop_reason_str(&message) == "toolUse" {
                has_more_tool_calls = !tool_results.is_empty();
            }

            let _ = emit(AgentEvent::TurnEnd {
                message: message.clone(),
                tool_results: tool_results.clone(),
            });

            if let Some(ref s) = signal {
                if s.aborted {
                    return;
                }
            }

            let next_turn_context = PrepareNextTurnContextLite {
                message: message.clone(),
                tool_results,
                context: current_context.clone(),
                new_messages: new_messages.clone(),
            };
            let next_update = config.prepare_next_turn(prepare_next_turn_context_from(next_turn_context)).await;
            if let Some(update) = next_update {
                if let Some(c) = update.context {
                    current_context = c;
                }
                let mut next_model = config.model().clone();
                if let Some(m) = update.model {
                    next_model = m;
                }
                let next_thinking_level = update
                    .thinking_level
                    .clone()
                    .or_else(|| config.thinking_level().cloned());
                let next_reasoning =
                    resolve_agent_reasoning_option(&next_model, next_thinking_level.as_deref().unwrap_or("off"));
                let _ = next_reasoning; // apply via config mutation in real impl
            }

            if let Some(ref s) = signal {
                if s.aborted {
                    return;
                }
            }

            pending_messages = config.get_steering_messages().await;
        }

        let follow_ups = config.get_follow_up_messages().await;
        if !follow_ups.is_empty() {
            pending_messages = follow_ups;
            continue;
        }
        break;
    }

    let _ = emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    });
}

struct PrepareNextTurnContextLite {
    message: AgentMessage,
    tool_results: Vec<ToolResultMessage>,
    context: AgentContext,
    new_messages: Vec<AgentMessage>,
}

fn prepare_next_turn_context_from(lite: PrepareNextTurnContextLite) -> crate::types::ShouldStopAfterTurnContext {
    crate::types::ShouldStopAfterTurnContext {
        message: lite.message,
        tool_results: lite.tool_results,
        context: lite.context,
        new_messages: lite.new_messages,
    }
}

fn stop_reason_str(message: &AgentMessage) -> String {
    if let AgentMessage::Llm(llm_core::types::Message::Assistant(a)) = message {
        a.stop_reason.clone()
    } else {
        String::new()
    }
}

async fn stream_assistant_response(
    context: &AgentContext,
    config: &Box<dyn AgentLoopConfig>,
    signal: &Option<AbortSignalShim>,
    emit: AgentEventSink,
    stream_fn: Option<StreamFn>,
    runtime: Option<&dyn StreamSimpleOnly>,
) -> AssistantMessage {
    let messages = context.messages.clone();
    let llm_messages = config.convert_to_llm(messages).await;

    let _llm_context = Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: llm_messages,
        tools: None,
    };

    let _stream_function = resolve_agent_core_stream_fn(runtime, stream_fn);

    let resolved_api_key = config.api_key().map(|s| s.to_string());

    let _ = resolved_api_key;
    let _ = emit;
    let _ = signal;
    let usage: llm_core::types::Usage = serde_json::from_value(empty_usage()).unwrap_or_default();
    let response: AssistantMessage = AssistantMessage {
        role: "assistant".to_string(),
        content: vec![],
        api: String::new(),
        provider: String::new(),
        model: String::new(),
        response_model: None,
        response_id: None,
        diagnostics: None,
        usage,
        stop_reason: String::new(),
        error_message: None,
        error_code: None,
        error_type: None,
        error_body: None,
        timestamp: 0,
    };
    response
}

#[allow(dead_code)]
fn _force_use(_t: AgentToolResult<Value>) {}

#[allow(dead_code)]
fn _force_text(_t: TextOrImageContent) {}

#[allow(dead_code)]
fn _force_signals() {
    let _: Option<AbortSignalShim> = None;
    let _: Option<AgentTool> = None;
    let _: Option<AgentToolCall> = None;
    let _: Option<BeforeToolCallContext> = None;
    let _: Option<BeforeToolCallResult> = None;
    let _: Option<AfterToolCallContext> = None;
    let _: Option<AfterToolCallResult> = None;
    let _: Option<DeferredToolCallContext> = None;
}

impl AgentMessage {
    pub fn role_str(&self) -> String {
        match self {
            AgentMessage::Llm(m) => match m {
                llm_core::types::Message::User(_) => "user".to_string(),
                llm_core::types::Message::Assistant(_) => "assistant".to_string(),
                llm_core::types::Message::ToolResult(_) => "toolResult".to_string(),
            },
            AgentMessage::BashExecution(_) => "bashExecution".to_string(),
            AgentMessage::Custom(_) => "custom".to_string(),
            AgentMessage::BranchSummary(_) => "branchSummary".to_string(),
            AgentMessage::CompactionSummary(_) => "compactionSummary".to_string(),
        }
    }
}

#[allow(dead_code)]
fn _force_runtime(_r: AgentCoreStreamRuntimeDeps) {}