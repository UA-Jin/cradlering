//! Azure OpenAI Responses API provider adapter.
//! 翻译自 packages/ai/src/providers/azure-openai-responses.ts

use std::future::Future;
use std::pin::Pin;

use llm_core::types::{
    AssistantMessage, AssistantMessageEventStreamContract, Context, Model, SimpleStreamOptions,
    StreamOptions,
};

/// Streams an Azure OpenAI Responses request.
pub fn stream_azure_openai_responses(
    _model: Model,
    _context: Context,
    _options: Option<StreamOptions>,
) -> Box<dyn AssistantMessageEventStreamContract> {
    unimplemented!("azure-openai-responses stream adapter: full implementation pending")
}

/// Streams a simple Azure OpenAI Responses request.
pub fn stream_simple_azure_openai_responses(
    _model: Model,
    _context: Context,
    _options: Option<SimpleStreamOptions>,
) -> Box<dyn AssistantMessageEventStreamContract> {
    unimplemented!("azure-openai-responses simple-stream adapter: full implementation pending")
}

/// Future-based complete helper.
pub fn complete_azure_openai_responses(
    model: Model,
    context: Context,
    options: Option<StreamOptions>,
) -> Pin<Box<dyn Future<Output = AssistantMessage> + Send>> {
    let stream = stream_azure_openai_responses(model, context, options);
    Box::pin(async move { stream.result().await })
}

/// Future-based simple-complete helper.
pub fn complete_simple_azure_openai_responses(
    model: Model,
    context: Context,
    options: Option<SimpleStreamOptions>,
) -> Pin<Box<dyn Future<Output = AssistantMessage> + Send>> {
    let stream = stream_simple_azure_openai_responses(model, context, options);
    Box::pin(async move { stream.result().await })
}

pub use crate::providers::azure_deployment_map as deployment_map;
pub use crate::providers::azure_openai_responses_client_compat as client_compat;