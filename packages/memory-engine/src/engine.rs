//! 统一记忆引擎门面
//!
//! 组合 Embedding + Vector + Cache + Graph + Router + Backend，
//! 提供 store / recall / stats 三个核心 API。

use crate::backend::{MultiBackendConfig, BackendKind, BackendStatus};
use crate::cache::{CacheEngine, CacheConfig, CacheStats, CacheEntry};
use crate::embedding::{EmbeddingConfig, EmbeddingProvider, LocalHashEmbedding, ApiEmbedding};
use crate::graph::{KnowledgeGraph, GraphStats, Entity};
use crate::router::{CascadingRouter, RouterConfig, RouteDecision};
use crate::vector::{VectorStore, VectorRecord, VectorSearchHit, MetadataFilter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// 记忆引擎配置
#[derive(Clone, Debug, Deserialize)]
pub struct MemoryEngineConfig {
    /// 数据目录（默认 ~/.cradle-ring/data/memory-engine/）
    pub data_dir: String,
    /// Embedding 配置
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    /// 缓存配置
    #[serde(default)]
    pub cache: CacheConfig,
    /// 路由配置
    #[serde(default)]
    pub router: RouterConfig,
    /// 多后端配置（可选）
    #[serde(default)]
    pub backends: MultiBackendConfig,
    /// 是否启用图谱
    #[serde(default = "default_true")]
    pub enable_graph: bool,
}

fn default_true() -> bool { true }

impl Default for MemoryEngineConfig {
    fn default() -> Self {
        Self {
            data_dir: format!("{}/.cradle-ring/data/memory-engine", env!("HOME")),
            embedding: EmbeddingConfig::default(),
            cache: CacheConfig::default(),
            router: RouterConfig::default(),
            backends: MultiBackendConfig::default(),
            enable_graph: true,
        }
    }
}

impl MemoryEngineConfig {
    /// 从 JSON 配置块解析
    pub fn from_json(v: &serde_json::Value, home: &str) -> Self {
        let data_dir = v.get("dataDir").and_then(|d| d.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("{}/.cradle-ring/data/memory-engine", home));

        // 兼容旧版：memory.embedding 直接平铺
        let embedding = v.get("embedding")
            .and_then(EmbeddingConfig::from_json)
            .unwrap_or_default();

        let cache = v.get("cache")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default();

        let router = v.get("router")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or_else(|| RouterConfig {
                primary_model: v.get("primaryModel").and_then(|m| m.as_str()).unwrap_or("gpt-4o").to_string(),
                small_model: v.get("smallModel").and_then(|m| m.as_str()).map(String::from),
                easy_threshold: 0.4,
                hard_threshold: 0.7,
            });

        let backends = v.get("backends")
            .map(MultiBackendConfig::from_json)
            .unwrap_or_default();

        let enable_graph = v.get("enableGraph").and_then(|g| g.as_bool()).unwrap_or(true);

        Self { data_dir, embedding, cache, router, backends, enable_graph }
    }
}

/// 存储请求
#[derive(Clone, Debug)]
pub struct StoreRequest {
    pub body: String,
    pub kind: String,            // fact / preference / instruction / procedure / entity / note / l2_cache
    pub source: String,
    pub tags: Vec<String>,
    pub session_key: Option<String>,
    /// 关联的原始 query（如有）
    pub original_query: Option<String>,
    /// 关联的模型 ID（如有）
    pub model: Option<String>,
}

/// 召回请求
#[derive(Clone, Debug)]
pub struct RecallRequest {
    pub query: String,
    pub session_key: Option<String>,
    pub top_k: usize,
    /// 是否启用 L1 精确缓存
    pub use_l1: bool,
    /// 是否启用 L2 语义缓存
    pub use_l2: bool,
    /// 是否启用 L4 向量检索
    pub use_l4: bool,
    /// 是否启用图谱
    pub use_graph: bool,
}

impl Default for RecallRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            session_key: None,
            top_k: 5,
            use_l1: true,
            use_l2: true,
            use_l4: true,
            use_graph: true,
        }
    }
}

/// 召回结果
#[derive(Clone, Debug, Serialize)]
pub struct RecallResult {
    /// L1 命中（精确缓存，直接返回）
    pub cache_hit: Option<CacheEntry>,
    /// L2 命中（语义缓存）
    pub semantic_hit: Option<CacheEntry>,
    /// L4 向量检索结果
    pub vector_hits: Vec<VectorSearchHit>,
    /// 图谱相关实体（多跳）
    pub graph_entities: Vec<Entity>,
    /// 关键词检索命中（L4 fallback）
    pub keyword_hits: Vec<VectorSearchHit>,
    /// 路由决策
    pub route: RouteDecision,
    /// 处理耗时（ms）
    pub latency_ms: u32,
    /// 检索的 query 向量是否真实（非占位）
    pub embedding_real: bool,
}

impl RecallResult {
    pub fn has_cache_hit(&self) -> bool {
        self.cache_hit.is_some() || self.semantic_hit.is_some()
    }

    pub fn has_kb_support(&self) -> bool {
        !self.vector_hits.is_empty() || !self.graph_entities.is_empty()
    }
}

/// 记忆引擎（统一门面）
pub struct MemoryEngine {
    embedding: Arc<dyn EmbeddingProvider>,
    vectors: Arc<VectorStore>,
    cache: Arc<CacheEngine>,
    graph: Option<Arc<KnowledgeGraph>>,
    router: Arc<CascadingRouter>,
    config: MemoryEngineConfig,
}

impl MemoryEngine {
    /// 创建引擎（按配置初始化所有组件）
    pub fn new(config: MemoryEngineConfig) -> anyhow::Result<Self> {
        // 创建数据目录
        std::fs::create_dir_all(&config.data_dir)?;

        // 初始化 Embedding
        let embedding: Arc<dyn EmbeddingProvider> = match &config.embedding {
            EmbeddingConfig::Local { dim, .. } => Arc::new(LocalHashEmbedding::new(*dim)),
            EmbeddingConfig::SiliconFlow { api_key, model, dim, .. } if !api_key.is_empty() => {
                Arc::new(ApiEmbedding::siliconflow(api_key.clone(), model.clone(), *dim))
            }
            EmbeddingConfig::OpenAI { api_key, model, dim, base_url } if !api_key.is_empty() => {
                Arc::new(ApiEmbedding::new(base_url.clone(), api_key.clone(), model.clone(), *dim, "openai".to_string()))
            }
            // API Key 为空 → 降级到本地
            _ => {
                tracing::warn!("Embedding API Key 未配置，降级到本地哈希 Embedding");
                Arc::new(LocalHashEmbedding::new(config.embedding.dim()))
            }
        };

        // 初始化向量库
        let vectors = Arc::new(VectorStore::persistent(Path::new(&config.data_dir).join("vectors.jsonl"))?);

        // 初始化缓存
        let cache = Arc::new(CacheEngine::new(config.cache.clone()));

        // 初始化图谱（可选）
        let graph = if config.enable_graph {
            Some(Arc::new(KnowledgeGraph::persistent(Path::new(&config.data_dir).join("graph.json"))?))
        } else {
            None
        };

        // 初始化路由
        let router = Arc::new(CascadingRouter::new(config.router.clone()));

        Ok(Self { embedding, vectors, cache, graph, router, config })
    }

    /// 存储一条记忆
    pub async fn store(&self, req: StoreRequest) -> anyhow::Result<String> {
        let now = chrono::Utc::now().timestamp();
        let now_ns = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(now * 1_000_000_000);
        // 用纳秒 + 随机后缀，避免同秒多条同 kind 记录覆盖
        let rand_suffix = format!("{:04x}", now_ns as u32 & 0xffff);
        let id = format!("{}-{}-{}", req.kind, now, rand_suffix);
        // 生成向量
        let vector = self.embedding.embed(&req.body).await?;

        // 写入向量库
        let mut metadata = HashMap::new();
        metadata.insert("kind".to_string(), serde_json::json!(req.kind));
        metadata.insert("source".to_string(), serde_json::json!(req.source));
        metadata.insert("tags".to_string(), serde_json::json!(req.tags));
        metadata.insert("createdAt".to_string(), serde_json::json!(now));
        metadata.insert("hitCount".to_string(), serde_json::json!(0u64));
        if let Some(m) = &req.model {
            metadata.insert("model".to_string(), serde_json::json!(m));
        }

        self.vectors.upsert(VectorRecord {
            id: id.clone(),
            text: req.body.clone(),
            vector: vector.clone(),
            metadata: metadata.clone(),
        })?;

        // 如果是 LLM 答案（有 original_query），同时写入 L2 缓存
        if let Some(query) = &req.original_query {
            if let Some(model) = &req.model {
                let _ = self.cache.l2_store(query, &req.body, vector.clone(), model, now, &self.vectors);
            }
        }

        // 写入图谱（提取实体关系）
        if let Some(g) = &self.graph {
            let triples = g.extract_from_text(&req.body, &req.source, now);
            for (subj, obj, mut rel) in triples {
                let subj_id = g.upsert_entity(subj)?;
                let obj_id = g.upsert_entity(obj)?;
                rel.from_id = subj_id;
                rel.to_id = obj_id;
                let _ = g.add_relation(rel);
            }
        }

        Ok(id)
    }

    /// 召回（Cache-First 核心流程）
    pub async fn recall(&self, req: RecallRequest) -> anyhow::Result<RecallResult> {
        let t0 = std::time::Instant::now();
        let now = chrono::Utc::now().timestamp();

        // L1 精确缓存
        if req.use_l1 {
            if let Some(hit) = self.cache.l1_lookup(&req.query, req.session_key.as_deref(), now) {
                let route = self.router.cache_hit();
                return Ok(RecallResult {
                    cache_hit: Some(hit),
                    semantic_hit: None,
                    vector_hits: vec![],
                    graph_entities: vec![],
                    keyword_hits: vec![],
                    route,
                    latency_ms: t0.elapsed().as_millis() as u32,
                    embedding_real: self.embedding.is_real(),
                });
            }
        }

        // 生成 query 向量（用于 L2 和 L4）
        let query_vector = self.embedding.embed(&req.query).await?;

        // L2 语义缓存
        if req.use_l2 {
            if let Some(hit) = self.cache.l2_lookup(&query_vector, &self.vectors, now) {
                let route = self.router.cache_hit();
                return Ok(RecallResult {
                    cache_hit: None,
                    semantic_hit: Some(hit),
                    vector_hits: vec![],
                    graph_entities: vec![],
                    keyword_hits: vec![],
                    route,
                    latency_ms: t0.elapsed().as_millis() as u32,
                    embedding_real: self.embedding.is_real(),
                });
            }
        }

        // L4 向量检索（排除 l2_cache 类型）
        let mut filter = MetadataFilter::default();
        // 过滤掉缓存类型，只查实际知识
        filter.kinds = vec!["fact", "preference", "instruction", "procedure", "entity", "note"]
            .into_iter().map(String::from).collect();
        let mut vector_hits = if req.use_l4 {
            self.vectors.search(&query_vector, req.top_k, &filter)
        } else {
            vec![]
        };

        // 关键词检索（互补，用于精确匹配场景）
        let keyword_hits = if req.use_l4 {
            self.vectors.keyword_search(&req.query, req.top_k, &filter)
        } else {
            vec![]
        };

        // 合并去重（向量 + 关键词）
        let mut seen = std::collections::HashSet::new();
        vector_hits.retain(|h| seen.insert(h.record.id.clone()));
        for h in &keyword_hits {
            if !seen.contains(&h.record.id) {
                seen.insert(h.record.id.clone());
                vector_hits.push(h.clone());
            }
        }
        vector_hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        vector_hits.truncate(req.top_k);

        // 图谱查询（找 query 中提到的实体）
        let mut graph_entities = Vec::new();
        if req.use_graph {
            if let Some(g) = &self.graph {
                // 简化：取 query 的分词作为候选实体名
                let candidates = extract_candidate_entities(&req.query);
                for name in candidates.iter().take(5) {
                    if let Some(e) = g.get_entity_by_name(name) {
                        if !graph_entities.iter().any(|x: &Entity| x.id == e.id) {
                            graph_entities.push(e);
                        }
                    }
                }
            }
        }

        // 统计 L4 命中
        if !vector_hits.is_empty() {
            self.cache.record_l4_hit();
        } else {
            self.cache.record_miss();
        }

        // 路由决策
        let stats = self.cache.stats();
        let cache_hit_rate = stats.hit_rate;
        let route = self.router.route(&req.query, !vector_hits.is_empty() || !graph_entities.is_empty(), cache_hit_rate);

        Ok(RecallResult {
            cache_hit: None,
            semantic_hit: None,
            vector_hits,
            graph_entities,
            keyword_hits: vec![],
            route,
            latency_ms: t0.elapsed().as_millis() as u32,
            embedding_real: self.embedding.is_real(),
        })
    }

    /// 记录一次 LLM 回答到 L1 + L2 缓存
    pub async fn cache_answer(
        &self,
        query: &str,
        answer: &str,
        session_key: Option<&str>,
        model: &str,
    ) -> anyhow::Result<()> {
        let now = chrono::Utc::now().timestamp();
        let query_vector = self.embedding.embed(query).await?;
        let entry = self.cache.l1_store(query, session_key, answer, query_vector.clone(), model, now);
        // L2 也会写一份，便于跨会话语义匹配
        let _ = entry;
        self.cache.l2_store(query, answer, query_vector, model, now, &self.vectors)?;
        Ok(())
    }

    /// 列出全部记忆（带分页）
    pub fn list_memories(&self, limit: Option<usize>) -> Vec<VectorRecord> {
        self.vectors.list(limit)
    }

    /// 按 ID 获取
    pub fn get_memory(&self, id: &str) -> Option<VectorRecord> {
        self.vectors.get(id)
    }

    /// 删除
    pub fn delete_memory(&self, id: &str) -> anyhow::Result<bool> {
        self.vectors.delete(id)
    }

    /// 增加命中计数
    pub fn increment_hit(&self, id: &str) -> anyhow::Result<()> {
        self.vectors.increment_hit(id)
    }

    /// 用户反馈
    pub fn feedback(&self, cache_key: &str, positive: bool) -> bool {
        self.cache.feedback(cache_key, positive)
    }

    /// 缓存统计
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// 图谱统计
    pub fn graph_stats(&self) -> Option<GraphStats> {
        self.graph.as_ref().map(|g| g.stats())
    }

    /// 图谱快照
    pub fn graph_snapshot(&self) -> Option<crate::graph::GraphSnapshot> {
        self.graph.as_ref().map(|g| g.snapshot())
    }

    /// 直接访问图谱（用于高级查询）
    pub fn graph(&self) -> Option<Arc<KnowledgeGraph>> {
        self.graph.clone()
    }

    /// 向量库引用
    pub fn vectors(&self) -> Arc<VectorStore> {
        self.vectors.clone()
    }

    /// Embedding 提供者
    pub fn embedding(&self) -> &dyn EmbeddingProvider {
        self.embedding.as_ref()
    }

    /// 路由器引用
    pub fn router(&self) -> &CascadingRouter {
        &self.router
    }

    /// 后端状态（简化：根据配置返回）
    pub fn backend_statuses(&self) -> Vec<BackendStatus> {
        let now = chrono::Utc::now().timestamp();
        let mut statuses = vec![
            BackendStatus {
                kind: BackendKind::Builtin,
                enabled: true,
                available: true,
                last_check: now,
                error: None,
            },
        ];
        for kind in [BackendKind::Obsidian, BackendKind::Siyuan, BackendKind::Hindsight, BackendKind::Zep] {
            let enabled = self.config.backends.is_enabled(&kind);
            statuses.push(BackendStatus {
                kind,
                enabled,
                available: false,
                last_check: now,
                error: if enabled { Some("not implemented".to_string()) } else { None },
            });
        }
        statuses
    }

    /// 配置引用
    pub fn config(&self) -> &MemoryEngineConfig {
        &self.config
    }
}

/// 从 query 中提取候选实体名（简化分词）
fn extract_candidate_entities(query: &str) -> Vec<String> {
    // 按非字母数字字符切分，过滤掉太短的
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.chars().count() >= 2)
        .map(String::from)
        .collect()
}
