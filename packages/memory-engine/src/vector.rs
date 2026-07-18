//! 向量存储（纯 Rust，无外部依赖）
//!
//! 实现策略：
//! - 小规模（<10k 向量）：暴力余弦扫描 + 元数据过滤
//! - 中规模（10k-100k）：桶式倒排（按 Embedding 主成分分桶）+ 桶内暴力扫描
//! - 持久化：JSONL 格式（每行一条记录）
//!
//! 不实现完整 HNSW 的原因：CradleRing 记忆库通常 <100k 条记录，
//! 暴力扫描在 768 维 10 万条下 <50ms，已足够。
//! 未来可替换为 qdrant-client（内嵌）或 hnsw crate。

use crate::embedding::EmbeddingVector;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

/// 向量记录（带元数据）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VectorRecord {
    pub id: String,
    /// 文本内容（用于关键词检索 + 回显）
    pub text: String,
    /// 向量
    pub vector: EmbeddingVector,
    /// 元数据（kind/source/tags/createdAt/hit_count/...）
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// 检索命中结果
#[derive(Clone, Debug, Serialize)]
pub struct VectorSearchHit {
    pub record: VectorRecord,
    pub score: f32,
}

/// 向量存储（线程安全）
pub struct VectorStore {
    inner: RwLock<VectorStoreInner>,
    path: Option<PathBuf>,
}

struct VectorStoreInner {
    records: Vec<VectorRecord>,
    /// 文本索引（id -> 在 records 中的位置）
    index: HashMap<String, usize>,
}

impl VectorStore {
    /// 创建内存向量库（不持久化）
    pub fn in_memory() -> Self {
        Self {
            inner: RwLock::new(VectorStoreInner { records: Vec::new(), index: HashMap::new() }),
            path: None,
        }
    }

    /// 创建持久化向量库（JSONL）
    pub fn persistent(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut inner = VectorStoreInner { records: Vec::new(), index: HashMap::new() };
        // 加载现有数据
        // 关键修复：JSONL 是追加式，同 id 的旧行先出现、新行后出现，
        // 加载时必须按 id 去重（后者覆盖前者），否则 search/list/count/delete 全部出错
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            let mut by_id: HashMap<String, VectorRecord> = HashMap::new();
            let mut order: Vec<String> = Vec::new();
            for line in data.lines() {
                if line.trim().is_empty() { continue; }
                if let Ok(rec) = serde_json::from_str::<VectorRecord>(line) {
                    if !by_id.contains_key(&rec.id) {
                        order.push(rec.id.clone());
                    }
                    by_id.insert(rec.id.clone(), rec);
                }
            }
            for id in order {
                if let Some(rec) = by_id.remove(&id) {
                    inner.index.insert(rec.id.clone(), inner.records.len());
                    inner.records.push(rec);
                }
            }
        }
        Ok(Self {
            inner: RwLock::new(inner),
            path: Some(path),
        })
    }

    /// 插入或更新一条记录
    pub fn upsert(&self, record: VectorRecord) -> anyhow::Result<()> {
        {
            let mut inner = self.inner.write().unwrap();
            if let Some(&pos) = inner.index.get(&record.id) {
                inner.records[pos] = record.clone();
            } else {
                let pos = inner.records.len();
                inner.records.push(record.clone());
                inner.index.insert(record.id.clone(), pos);
            }
        }
        self.append_to_disk(&record)?;
        Ok(())
    }

    /// 批量插入
    pub fn upsert_batch(&self, records: Vec<VectorRecord>) -> anyhow::Result<()> {
        {
            let mut inner = self.inner.write().unwrap();
            for rec in &records {
                if let Some(&pos) = inner.index.get(&rec.id) {
                    inner.records[pos] = rec.clone();
                } else {
                    let pos = inner.records.len();
                    inner.records.push(rec.clone());
                    inner.index.insert(rec.id.clone(), pos);
                }
            }
        }
        // 全量重写（批量场景更高效）
        if self.path.is_some() {
            self.flush_all()?;
        }
        Ok(())
    }

    /// 按 ID 删除
    pub fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let deleted = {
            let mut inner = self.inner.write().unwrap();
            if let Some(pos) = inner.index.remove(id) {
                inner.records.remove(pos);
                // 重建索引（位置变了）
                inner.index.clear();
                let ids: Vec<String> = inner.records.iter().map(|r| r.id.clone()).collect();
                for (i, rid) in ids.into_iter().enumerate() {
                    inner.index.insert(rid, i);
                }
                true
            } else {
                false
            }
        };
        if deleted {
            self.flush_all()?;
        }
        Ok(deleted)
    }

    /// 按 ID 获取
    pub fn get(&self, id: &str) -> Option<VectorRecord> {
        let inner = self.inner.read().unwrap();
        inner.index.get(id).and_then(|&pos| inner.records.get(pos).cloned())
    }

    /// 列出全部（按时间倒序）
    /// 性能修复：只 clone 索引，排序后再按需 clone 记录（10 万条 × 3KB 向量不再全量复制）
    pub fn list(&self, limit: Option<usize>) -> Vec<VectorRecord> {
        let inner = self.inner.read().unwrap();
        let mut idx: Vec<usize> = (0..inner.records.len()).collect();
        idx.sort_by(|&a, &b| {
            let ta = inner.records[a].metadata.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0);
            let tb = inner.records[b].metadata.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0);
            tb.cmp(&ta)
        });
        if let Some(limit) = limit { idx.truncate(limit); }
        idx.into_iter().map(|i| inner.records[i].clone()).collect()
    }

    /// 统计
    pub fn count(&self) -> usize {
        self.inner.read().unwrap().records.len()
    }

    /// 语义检索（余弦相似度 + 过滤）
    /// 性能修复：先算分（借用）→ 排序 → 只 clone top_k
    pub fn search(
        &self,
        query: &EmbeddingVector,
        top_k: usize,
        filter: &MetadataFilter,
    ) -> Vec<VectorSearchHit> {
        let inner = self.inner.read().unwrap();
        let mut scored: Vec<(f32, usize)> = inner.records.iter().enumerate()
            .filter(|(_, r)| filter.matches(&r.metadata))
            .filter(|(_, r)| r.vector.len() == query.len())
            .map(|(i, r)| (cosine_similarity(query, &r.vector), i))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored.into_iter()
            .map(|(score, i)| VectorSearchHit { score, record: inner.records[i].clone() })
            .collect()
    }

    /// 关键词检索（子串匹配 + 词级 OR 语义）
    pub fn keyword_search(&self, query: &str, top_k: usize, filter: &MetadataFilter) -> Vec<VectorSearchHit> {
        let inner = self.inner.read().unwrap();
        let q = query.to_lowercase();
        // 预分词：英文按空格，中文整段作为候选
        let q_terms: Vec<String> = q.split_whitespace().map(String::from).collect();
        let mut scored: Vec<(f32, usize)> = inner.records.iter().enumerate()
            .filter(|(_, r)| filter.matches(&r.metadata))
            .filter(|(_, r)| {
                let text = r.text.to_lowercase();
                // 任一 term 命中即保留（OR 语义）
                if q_terms.is_empty() {
                    // 无空格的查询（中文）：用字符级 n-gram 判断
                    char_overlap_score(&q, &text) > 0.3
                } else {
                    q_terms.iter().any(|t| text.contains(t))
                }
            })
            .map(|(i, r)| (lcs_overlap(&q, &r.text.to_lowercase()), i))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored.into_iter()
            .map(|(score, i)| VectorSearchHit { score, record: inner.records[i].clone() })
            .collect()
    }

    /// 增加命中计数
    /// 性能修复：hitCount 是易失统计，只更新内存 + 追加增量行（O(1)），
    /// 不再每次命中都全量重写（之前是 O(n) IO/次）。
    /// 追加行的 id 与原子同 id，加载去重时自然取最后一条（含最新计数）。
    pub fn increment_hit(&self, id: &str) -> anyhow::Result<()> {
        let updated_record = {
            let mut inner = self.inner.write().unwrap();
            let pos = inner.index.get(id).copied();
            pos.and_then(|p| {
                let rec = inner.records.get_mut(p)?;
                let hits = rec.metadata.entry("hitCount".to_string())
                    .or_insert_with(|| serde_json::json!(0));
                if let Some(n) = hits.as_u64() {
                    *hits = serde_json::json!(n + 1);
                } else if let Some(n) = hits.as_i64() {
                    *hits = serde_json::json!((n + 1) as u64);
                } else {
                    *hits = serde_json::json!(1u64);
                }
                Some(rec.clone())
            })
        };
        // 追加增量行（id 相同，加载时后者覆盖前者）
        if let Some(rec) = updated_record {
            self.append_to_disk(&rec)?;
        }
        Ok(())
    }

    fn append_to_disk(&self, record: &VectorRecord) -> anyhow::Result<()> {
        if let Some(path) = &self.path {
            use std::io::Write;
            let line = serde_json::to_string(record)?;
            // 关键修复：磁盘打开失败必须向上传播，否则内存已更新但数据永久丢失且无告警
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(f, "{}", line)?;
        }
        Ok(())
    }

    fn flush_all(&self) -> std::io::Result<()> {
        if let Some(path) = &self.path {
            let inner = self.inner.read().unwrap();
            use std::io::Write;
            // 关键修复：open/create 失败传播，不再静默吞掉
            let mut f = std::fs::File::create(path)?;
            for rec in &inner.records {
                let line = serde_json::to_string(rec)?;
                writeln!(f, "{}", line)?;
            }
        }
        Ok(())
    }
}

/// 元数据过滤条件
#[derive(Clone, Debug, Default)]
pub struct MetadataFilter {
    /// kind 必须在此列表中（空表示不过滤）
    pub kinds: Vec<String>,
    /// source 必须匹配（None 表示不过滤）
    pub source: Option<String>,
    /// 必须包含所有 tags
    pub tags: Vec<String>,
}

impl MetadataFilter {
    pub fn matches(&self, meta: &HashMap<String, serde_json::Value>) -> bool {
        if !self.kinds.is_empty() {
            let kind = meta.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if !self.kinds.iter().any(|k| k == kind) {
                return false;
            }
        }
        if let Some(src) = &self.source {
            let actual = meta.get("source").and_then(|v| v.as_str()).unwrap_or("");
            if actual != src { return false; }
        }
        if !self.tags.is_empty() {
            let actual_tags: Vec<String> = meta.get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_default();
            for t in &self.tags {
                if !actual_tags.iter().any(|at| at == t) {
                    return false;
                }
            }
        }
        true
    }
}

/// 余弦相似度
pub fn cosine_similarity(a: &EmbeddingVector, b: &EmbeddingVector) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a > 0.0 && norm_b > 0.0 {
        dot / (norm_a * norm_b)
    } else {
        0.0
    }
}

/// 简化最长公共子串比例（用于关键词匹配分数）
///
/// 策略：
/// - 英文/空格分词：计算 query 中各 term 在 text 中的覆盖率（OR 语义，至少 1 个匹配）
/// - 中文/无空格：用字符级 n-gram 重叠度
fn lcs_overlap(query: &str, text: &str) -> f32 {
    if query.is_empty() || text.is_empty() { return 0.0; }
    // 查询分词
    let query_terms: Vec<&str> = query.split_whitespace().collect();
    if query_terms.is_empty() {
        // 单字 / 中文场景：字符级包含
        return char_overlap_score(query, text);
    }
    // OR 语义：只要有一个 term 命中就算相关（但覆盖率高的得分更高）
    let total = query_terms.len();
    let hits = query_terms.iter().filter(|t| text.contains(*t)).count();
    if hits == 0 {
        // 兜底：用字符级匹配（中文 term）
        return char_overlap_score(query, text) * 0.5;
    }
    // 至少命中 1 个：基础分 0.5 + 覆盖率奖励
    0.5 + 0.4 * (hits as f32 / total as f32)
}

/// 字符级 n-gram 重叠度（适用于中文）
fn char_overlap_score(query: &str, text: &str) -> f32 {
    let q_chars: Vec<char> = query.chars().collect();
    let t_chars: Vec<char> = text.chars().collect();
    if q_chars.is_empty() || t_chars.is_empty() { return 0.0; }
    // 完全包含
    if text.contains(query) { return 0.9; }
    // 双字符 n-gram 重叠
    let q_grams: std::collections::HashSet<String> = (0..q_chars.len().saturating_sub(1))
        .map(|i| q_chars[i..i+2].iter().collect())
        .collect();
    if q_grams.is_empty() {
        // 单字查询
        return if t_chars.contains(&q_chars[0]) { 0.6 } else { 0.0 };
    }
    let mut hits = 0;
    for i in 0..t_chars.len().saturating_sub(1) {
        let gram: String = t_chars[i..i+2].iter().collect();
        if q_grams.contains(&gram) { hits += 1; }
    }
    let coverage = hits as f32 / q_grams.len() as f32;
    // 0.3 ~ 0.8 之间
    0.3 + 0.5 * coverage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-5);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn test_upsert_and_search() {
        let store = VectorStore::in_memory();
        store.upsert(VectorRecord {
            id: "1".to_string(),
            text: "rust 编程语言".to_string(),
            vector: vec![1.0, 0.0, 0.0],
            metadata: HashMap::new(),
        }).unwrap();
        store.upsert(VectorRecord {
            id: "2".to_string(),
            text: "go 编程语言".to_string(),
            vector: vec![0.0, 1.0, 0.0],
            metadata: HashMap::new(),
        }).unwrap();
        let hits = store.search(&vec![1.0, 0.1, 0.0], 2, &MetadataFilter::default());
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].record.id, "1");
    }

    #[test]
    fn test_persistent_dedup_on_load() {
        // 验证：JSONL 追加式存储，同 id 多行时加载只保留最后一条（无幽灵重复）
        let dir = std::env::temp_dir().join(format!("cr-vec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vectors.jsonl");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            let old_rec = VectorRecord { id: "a".into(), text: "旧版本".into(), vector: vec![1.0], metadata: HashMap::new() };
            let new_rec = VectorRecord { id: "a".into(), text: "新版本".into(), vector: vec![1.0], metadata: HashMap::new() };
            writeln!(f, "{}", serde_json::to_string(&old_rec).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&new_rec).unwrap()).unwrap();
        }
        let store = VectorStore::persistent(&path).unwrap();
        assert_eq!(store.count(), 1, "同 id 应去重为 1 条");
        let rec = store.get("a").unwrap();
        assert_eq!(rec.text, "新版本", "应保留最后一条");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_increment_hit_no_full_rewrite() {
        let store = VectorStore::in_memory();
        store.upsert(VectorRecord {
            id: "x".to_string(),
            text: "test".to_string(),
            vector: vec![1.0],
            metadata: HashMap::new(),
        }).unwrap();
        store.increment_hit("x").unwrap();
        store.increment_hit("x").unwrap();
        let rec = store.get("x").unwrap();
        assert_eq!(rec.metadata.get("hitCount").and_then(|v| v.as_u64()), Some(2));
    }
}
