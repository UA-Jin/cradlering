//! 缓存层（Cache-First 策略核心）
//!
//! 三级缓存：
//! - L1：精确匹配缓存（query 文本哈希 → answer），TTL 7d，0 成本
//! - L2：语义缓存（query embedding → 相似度 >0.92 的历史回答），TTL 3d
//! - L4：向量库命中（query embedding → 相关记忆片段，注入上下文）
//!
//! L1/L2 是"直接返回"型，L4 是"上下文增强"型。

use crate::embedding::EmbeddingVector;
use crate::vector::{VectorStore, MetadataFilter, VectorSearchHit, VectorRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// 缓存条目
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: String,
    /// 原始问题（用于回显）
    pub query: String,
    /// 答案
    pub answer: String,
    /// 问题向量（用于 L2 语义匹配）
    #[serde(default)]
    pub query_vector: EmbeddingVector,
    /// 来源模型（用于级联路由统计）
    pub model: String,
    /// 创建时间（unix 秒）
    pub created_at: i64,
    /// 最后命中时间
    pub last_hit: i64,
    /// 命中次数
    pub hit_count: u32,
    /// 用户反馈（+1 满意 / -1 不满意）
    pub feedback: i32,
    /// 是否来自 L2（语义命中后被"提升"为 L1）
    #[serde(default)]
    pub promoted_from_l2: bool,
}

/// 缓存统计
#[derive(Clone, Debug, Default, Serialize)]
pub struct CacheStats {
    pub l1_total: usize,
    pub l1_hits: u64,
    pub l2_hits: u64,
    pub l4_hits: u64,
    pub misses: u64,
    /// 缓存命中率
    pub hit_rate: f32,
}

/// 缓存配置
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheConfig {
    /// L1 精确匹配 TTL（秒）
    pub l1_ttl_secs: i64,
    /// L2 语义匹配相似度阈值
    pub l2_threshold: f32,
    /// L2 语义匹配 TTL（秒）
    pub l2_ttl_secs: i64,
    /// 最大缓存条目数（LRU）
    pub max_entries: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            l1_ttl_secs: 7 * 24 * 3600, // 7 天
            l2_threshold: 0.92,
            l2_ttl_secs: 3 * 24 * 3600, // 3 天
            max_entries: 10_000,
        }
    }
}

/// Cache-First 缓存引擎
pub struct CacheEngine {
    entries: RwLock<HashMap<String, CacheEntry>>,
    config: CacheConfig,
    stats: RwLock<CacheStats>,
}

impl CacheEngine {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            config,
            stats: RwLock::new(CacheStats::default()),
        }
    }

    /// 计算 query 的缓存 key（SHA-256 哈希，归一化）
    pub fn query_key(query: &str, session_key: Option<&str>) -> String {
        let normalized = normalize_query(query);
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        if let Some(sk) = session_key {
            hasher.update(b"|session:");
            hasher.update(sk.as_bytes());
        }
        let result = hasher.finalize();
        // 取前 16 字节 hex
        hex_encode(&result[..16])
    }

    /// L1 精确匹配
    pub fn l1_lookup(&self, query: &str, session_key: Option<&str>, now: i64) -> Option<CacheEntry> {
        let key = Self::query_key(query, session_key);
        // 修复：先快速读锁判断，命中后在写锁内直接 +1（避免 lost-update 竞态），
        // 且不再持 entries 读锁的同时申请 stats 写锁（统一锁序 entries→stats，消除 ABBA 死锁窗口）
        let hit_entry = {
            let mut entries = self.entries.write().unwrap();
            match entries.get_mut(&key) {
                Some(entry) if now - entry.created_at <= self.config.l1_ttl_secs => {
                    entry.hit_count += 1;
                    entry.last_hit = now;
                    Some(entry.clone())
                }
                _ => None,
            }
        };
        if hit_entry.is_some() {
            // entries 锁已释放，安全申请 stats 锁
            let mut stats = self.stats.write().unwrap();
            stats.l1_hits += 1;
            let total = stats.l1_hits + stats.l2_hits + stats.l4_hits + stats.misses;
            stats.hit_rate = if total > 0 { (stats.l1_hits + stats.l2_hits) as f32 / total as f32 } else { 0.0 };
        }
        hit_entry
    }

    /// L2 语义匹配（在向量库中查找相似 query）
    pub fn l2_lookup(
        &self,
        query_vector: &EmbeddingVector,
        vector_store: &VectorStore,
        now: i64,
    ) -> Option<CacheEntry> {
        // 在向量库中查找 namespace=l2_cache 的记录
        let mut filter = MetadataFilter::default();
        filter.kinds = vec!["l2_cache".to_string()];
        let hits = vector_store.search(query_vector, 5, &filter);
        for hit in hits {
            if hit.score >= self.config.l2_threshold {
                // 检查 TTL（从 metadata.createdAt）
                let created_at = hit.record.metadata.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0);
                if now - created_at <= self.config.l2_ttl_secs {
                    let mut stats = self.stats.write().unwrap();
                    stats.l2_hits += 1;
                    let total = stats.l1_hits + stats.l2_hits + stats.l4_hits + stats.misses;
                    stats.hit_rate = if total > 0 { (stats.l1_hits + stats.l2_hits) as f32 / total as f32 } else { 0.0 };
                    return Some(CacheEntry {
                        key: hit.record.id.clone(),
                        query: hit.record.metadata.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        answer: hit.record.text.clone(),
                        query_vector: hit.record.vector.clone(),
                        model: hit.record.metadata.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        created_at,
                        last_hit: now,
                        hit_count: 1,
                        feedback: 0,
                        promoted_from_l2: true,
                    });
                }
            }
        }
        None
    }

    /// 记录一次未命中
    pub fn record_miss(&self) {
        let mut stats = self.stats.write().unwrap();
        stats.misses += 1;
        let total = stats.l1_hits + stats.l2_hits + stats.l4_hits + stats.misses;
        stats.hit_rate = if total > 0 { (stats.l1_hits + stats.l2_hits) as f32 / total as f32 } else { 0.0 };
    }

    /// L4 命中统计（向量库命中作为上下文增强）
    pub fn record_l4_hit(&self) {
        let mut stats = self.stats.write().unwrap();
        stats.l4_hits += 1;
        // 修复：L4 也要重算 hit_rate（分母含 l4_hits，之前不刷新导致路由拿到过期命中率）
        let total = stats.l1_hits + stats.l2_hits + stats.l4_hits + stats.misses;
        stats.hit_rate = if total > 0 { (stats.l1_hits + stats.l2_hits) as f32 / total as f32 } else { 0.0 };
    }

    /// 存入 L1 缓存（精确）
    pub fn l1_store(
        &self,
        query: &str,
        session_key: Option<&str>,
        answer: &str,
        query_vector: EmbeddingVector,
        model: &str,
        now: i64,
    ) -> CacheEntry {
        let key = Self::query_key(query, session_key);
        let entry = CacheEntry {
            key: key.clone(),
            query: query.to_string(),
            answer: answer.to_string(),
            query_vector,
            model: model.to_string(),
            created_at: now,
            last_hit: now,
            hit_count: 0,
            feedback: 0,
            promoted_from_l2: false,
        };
        let mut entries = self.entries.write().unwrap();
        entries.insert(key.clone(), entry.clone());
        // LRU 截断
        if entries.len() > self.config.max_entries {
            // 移除最旧的
            if let Some((oldest_key, _)) = entries.iter().min_by_key(|(_, e)| e.last_hit).map(|(k, v)| (k.clone(), v.clone())) {
                entries.remove(&oldest_key);
            }
        }
        entry
    }

    /// 同时写入 L2（向量库，便于跨会话语义匹配）
    pub fn l2_store(
        &self,
        query: &str,
        answer: &str,
        query_vector: EmbeddingVector,
        model: &str,
        now: i64,
        vector_store: &VectorStore,
    ) -> anyhow::Result<()> {
        let id = format!("l2-{}", Self::query_key(query, None));
        let mut metadata = HashMap::new();
        metadata.insert("kind".to_string(), serde_json::json!("l2_cache"));
        metadata.insert("query".to_string(), serde_json::json!(query));
        metadata.insert("model".to_string(), serde_json::json!(model));
        metadata.insert("createdAt".to_string(), serde_json::json!(now));
        vector_store.upsert(VectorRecord {
            id,
            text: answer.to_string(),
            vector: query_vector,
            metadata,
        })?;
        Ok(())
    }

    /// 用户反馈
    pub fn feedback(&self, key: &str, positive: bool) -> bool {
        let mut entries = self.entries.write().unwrap();
        if let Some(entry) = entries.get_mut(key) {
            entry.feedback += if positive { 1 } else { -1 };
            return true;
        }
        false
    }

    /// 获取统计
    pub fn stats(&self) -> CacheStats {
        let stats = self.stats.read().unwrap();
        let mut s = stats.clone();
        s.l1_total = self.entries.read().unwrap().len();
        s
    }

    /// 清空缓存
    pub fn clear(&self) {
        self.entries.write().unwrap().clear();
    }
}

/// 归一化 query（去标点 / 转小写 / 去多余空白）
/// 归一化 query（去标点 / 转小写 / 去多余空白）
/// 修复：纯标点/emoji 查询归一化为空串时，回退为原文 trim（否则所有符号类 query 共享同一缓存 key 互相串答案）
fn normalize_query(q: &str) -> String {
    let normalized = q.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        // 回退：保留原文（小写 trim），保证不同符号 query 有不同的 key
        q.trim().to_lowercase()
    } else {
        normalized
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l1_cache_hit() {
        let cache = CacheEngine::new(CacheConfig::default());
        let now = 1000i64;
        let _entry = cache.l1_store("你好", None, "你好！", vec![1.0, 0.0], "test-model", now);
        let hit = cache.l1_lookup("你好", None, now + 10);
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().answer, "你好！");
    }

    #[test]
    fn test_normalize_query() {
        assert_eq!(normalize_query("Hello, World!"), "hello world");
        assert_eq!(normalize_query("  multiple   spaces  "), "multiple spaces");
    }
}

// 抑制未使用导入警告（保留以备后用）
#[allow(dead_code)]
fn _silence_duration() -> Duration { Duration::from_secs(0) }
#[allow(dead_code)]
fn _silence_instant() -> Instant { Instant::now() }
#[allow(dead_code)]
fn _silence_hits() -> Vec<VectorSearchHit> { vec![] }
