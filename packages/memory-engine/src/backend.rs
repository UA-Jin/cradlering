//! 多后端冗余（可选）
//!
//! 支持并行查询多个外部知识库（Obsidian / 思源 / Hindsight / Zep），
//! 用 RRF（Reciprocal Rank Fusion）融合结果。
//!
//! 设计原则：所有后端都是可选的，用户可只启用 builtin（默认），
//! 也可启用多个，由 RRF 自动融合。

use crate::embedding::EmbeddingVector;
use crate::vector::VectorSearchHit;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 后端类型
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Builtin,
    Obsidian,
    Siyuan,
    Hindsight,
    Zep,
    Custom(String),
}

impl BackendKind {
    pub fn label(&self) -> &str {
        match self {
            BackendKind::Builtin => "builtin",
            BackendKind::Obsidian => "obsidian",
            BackendKind::Siyuan => "siyuan",
            BackendKind::Hindsight => "hindsight",
            BackendKind::Zep => "zep",
            BackendKind::Custom(n) => n.as_str(),
        }
    }
}

/// 后端状态
#[derive(Clone, Debug, Serialize)]
pub struct BackendStatus {
    pub kind: BackendKind,
    pub enabled: bool,
    pub available: bool,
    pub last_check: i64,
    pub error: Option<String>,
}

/// 单个后端配置（统一格式）
#[derive(Clone, Debug, Deserialize)]
pub struct BackendConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub api_url: Option<String>,
    pub api_key: Option<String>,
    pub api_token: Option<String>,
}

fn default_true() -> bool { true }

/// 多后端配置
#[derive(Clone, Debug, Default, Deserialize)]
pub struct MultiBackendConfig {
    #[serde(default)]
    pub backends: HashMap<String, BackendConfig>,
}

impl MultiBackendConfig {
    /// 解析配置 JSON（兼容扁平 / 嵌套格式）
    pub fn from_json(v: &serde_json::Value) -> Self {
        let mut cfg = MultiBackendConfig::default();
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if let Ok(bc) = serde_json::from_value::<BackendConfig>(val.clone()) {
                    cfg.backends.insert(k.clone(), bc);
                }
            }
        }
        cfg
    }

    pub fn is_enabled(&self, kind: &BackendKind) -> bool {
        self.backends.get(kind.label()).map_or(false, |b| b.enabled)
    }
}

/// 单个后端的查询结果
pub struct BackendQueryResult {
    pub backend: BackendKind,
    pub hits: Vec<VectorSearchHit>,
    pub latency_ms: u32,
    pub error: Option<String>,
}

/// 后端 trait（async）
#[async_trait::async_trait]
pub trait MemoryBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    async fn query(&self, query_vector: &EmbeddingVector, query_text: &str, top_k: usize) -> anyhow::Result<Vec<VectorSearchHit>>;
    async fn health_check(&self) -> bool;
}

/// RRF 融合（Reciprocal Rank Fusion）
///
/// score(d) = Σ 1 / (k + rank_i(d))
pub fn rrf_fuse(results: &[BackendQueryResult], k: u32, top_k: usize) -> Vec<VectorSearchHit> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    let mut records: HashMap<String, VectorSearchHit> = HashMap::new();

    for r in results {
        if r.error.is_some() { continue; }
        for (rank, hit) in r.hits.iter().enumerate() {
            let id = hit.record.id.clone();
            let rrf_score = 1.0 / (k as f32 + rank as f32 + 1.0);
            *scores.entry(id.clone()).or_insert(0.0) += rrf_score;
            // 保留记录（后写入的覆盖前面的，但分数会重新赋值）
            records.insert(id, hit.clone());
        }
    }

    let mut fused: Vec<VectorSearchHit> = records.into_iter()
        .map(|(id, mut hit)| {
            let s = scores.get(&id).copied().unwrap_or(0.0);
            hit.score = s;
            hit
        })
        .collect();
    fused.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(top_k);
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::VectorRecord;
    use std::collections::HashMap;

    fn make_hit(id: &str, text: &str, score: f32) -> VectorSearchHit {
        VectorSearchHit {
            record: VectorRecord {
                id: id.to_string(),
                text: text.to_string(),
                vector: vec![],
                metadata: HashMap::new(),
            },
            score,
        }
    }

    #[test]
    fn test_rrf_fusion() {
        let backend_a = BackendQueryResult {
            backend: BackendKind::Builtin,
            hits: vec![make_hit("1", "doc-1", 0.9), make_hit("2", "doc-2", 0.8)],
            latency_ms: 10,
            error: None,
        };
        let backend_b = BackendQueryResult {
            backend: BackendKind::Obsidian,
            hits: vec![make_hit("2", "doc-2", 0.95), make_hit("3", "doc-3", 0.7)],
            latency_ms: 20,
            error: None,
        };
        let fused = rrf_fuse(&[backend_a, backend_b], 60, 3);
        // doc-2 在两个后端都出现，应该排第一
        assert_eq!(fused[0].record.id, "2");
        assert!(fused[0].score > fused[1].score);
    }
}
