//! OpenAI Completions API provider adapter.
//! 翻译自 packages/ai/src/providers/openai-completions.ts

use std::future::Future;
use std::pin::Pin;

use llm_core::types::{
    AssistantMessage, AssistantMessageEventStreamContract, Context, Model, SimpleStreamOptions,
    StreamOptions,
};

/// Streams an OpenAI Completions request.
pub fn stream_openai_completions(
    _model: Model,
    _context: Context,
    _options: Option<StreamOptions>,
) -> Box<dyn AssistantMessageEventStreamContract> {
    unimplemented!("openai-completions stream adapter: full implementation pending")
}

/// Streams a simple OpenAI Completions request.
pub fn stream_simple_openai_completions(
    _model: Model,
    _context: Context,
    _options: Option<SimpleStreamOptions>,
) -> Box<dyn AssistantMessageEventStreamContract> {
    unimplemented!("openai-completions simple-stream adapter: full implementation pending")
}

/// Future-based complete helper.
pub fn complete_openai_completions(
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
) -> Pin<Box<dyn Future<Output = AssistantMessage> + Send>> {
    let stream = stream_openai_completions(model, context, options);
    Box::pin(async move { stream.result().await })
}

/// Future-based simple-complete helper.
pub fn complete_simple_openai_completions(
    model: Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> Pin<Box<dyn Future<Output = AssistantMessage> + Send>> {
    let stream = stream_simple_openai_completions(model, context, options);
    Box::pin(async move { stream.result().await })
}

pub use crate::providers::openai_stop_reason as stop_reason;
pub use crate::providers::openai_tool_projection as tool_projection;
pub use crate::providers::openai_tool_schema as tool_schema;

// Re-export commonly used symbols so downstream consumers can import them
// from the provider module path.
pub use crate::providers::openai_completions as self_mod;