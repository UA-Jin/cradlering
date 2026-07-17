//! CradleRing 记忆引擎（Memory Engine V3）
//!
//! 实现 Cache-First + 多后端冗余 + 时序知识图谱 + 级联路由 四大核心能力。
//!
//! # 模块组织
//! - [`embedding`]：Embedding 抽象（本地占位 / SiliconFlow API）
//! - [`vector`]：纯 Rust 向量库（HNSW-lite + 余弦相似度 + 元数据过滤）
//! - [`cache`]：L1 精确缓存 + L2 语义缓存
//! - [`graph`]：时序知识图谱（实体 + 关系 + 时间维度）
//! - [`router`]：级联模型路由（RouteLLM 方案）
//! - [`engine`]：统一门面，组合上述能力
//! - [`backend`]：多后端抽象（builtin / Obsidian / 思源 / Hindsight / Zep）

pub mod embedding;
pub mod vector;
pub mod cache;
pub mod graph;
pub mod router;
pub mod backend;
pub mod engine;

pub use engine::{MemoryEngine, MemoryEngineConfig, RecallResult, StoreRequest, RecallRequest};
pub use embedding::{EmbeddingProvider, EmbeddingVector, EmbeddingConfig};
pub use vector::VectorRecord;
pub use cache::{CacheEntry, CacheStats};
pub use graph::{Entity, Relation, GraphSnapshot};
pub use router::{RouteDecision, RouteTier};
pub use backend::{BackendKind, BackendStatus};
