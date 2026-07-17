//! OpenAI Responses API provider adapter.
//! 翻译自 packages/ai/src/providers/openai-responses.ts

use std::future::Future;
use std::pin::Pin;

use llm_core::types::{
    AssistantMessage, AssistantMessageEventStreamContract, Context, Model, SimpleStreamOptions,
    StreamOptions,
};

/// Streams an OpenAI Responses API request.
pub fn stream_openai_responses(
    _model: Model,
    _context: Context,
    _options: Option<StreamOptions>,
) -> Box<dyn AssistantMessageEventStreamContract> {
    unimplemented!("openai-responses stream adapter: full implementation pending")
}

/// Streams a simple OpenAI Responses API request.
pub fn stream_simple_openai_responses(
    _model: Model,
    _context: Context,
    _options: Option<SimpleStreamOptions>,
) -> Box<dyn AssistantMessageEventStreamContract> {
    unimplemented!("openai-responses simple-stream adapter: full implementation pending")
}

/// Future-based complete helper.
pub fn complete_openai_responses(
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
) -> Pin<Box<dyn Future<Output = AssistantMessage> + Send>> {
    let stream = stream_openai_responses(model, context, options);
    Box::pin(async move { stream.result().await })
}

/// Future-based simple-complete helper.
pub fn complete_simple_openai_responses(
    model: Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> Pin<Box<dyn Future<Output = AssistantMessage> + Send>> {
    let stream = stream_simple_openai_responses(model, context, options);
    Box::pin(async move { stream.result().await })
}

pub use crate::providers::openai_responses_shared as shared;
pub use crate::providers::openai_responses_tools as tools;