//! ai crate
//! 翻译自 packages/ai/src/index.ts (barrel) + sibling modules.
//!
//! Reusable model API contracts, provider adapters, and streaming runtime
//! for cradle-ring. Layered on top of llm-core / agent-core /
//! model-catalog-core / gateway-protocol.

pub mod api_registry;
pub mod env_api_keys;
pub mod host;
pub mod internal;
pub mod model_utils;
pub mod providers;
pub mod providers_mod;
pub mod session_resources;
pub mod stream;
pub mod types;
pub mod utils;
pub mod validation;

pub use api_registry::*;
pub use env_api_keys::*;
pub use host::*;
#[allow(ambiguous_glob_reexports)]
pub use internal::*;
pub use providers::*;
pub use stream::*;
pub use types::*;
pub use utils::*;

// Re-export shared llm-core types so that the ai package surface keeps
// parity with `@cradle-ring/ai` (which re-exports `@cradle-ring/llm-core`).
#[allow(unused_imports)]
pub use llm_core::*;