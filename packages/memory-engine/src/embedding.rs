//! Embedding 抽象层
//!
//! 支持两种后端：
//! - `local`：本地占位实现（哈希降维，无外部依赖，用于离线/无模型场景）
//! - `siliconflow`：硅基流动 API（默认 Qwen/Qwen3-VL-Embedding-8B）
//!
//! 当本地 ONNX 模型不可用时，自动降级到 `local`（hash-based）保证系统可用。

use serde::{Deserialize, Serialize};

/// 标准 Embedding 向量（f32）
pub type EmbeddingVector = Vec<f32>;

/// Embedding 配置
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "provider")]
pub enum EmbeddingConfig {
    /// 本地（占位实现，纯 Rust 哈希降维；未来可接入 ONNX/Candle）
    #[serde(rename = "local")]
    Local {
        #[serde(default = "default_local_model")]
        model: String,
        #[serde(default = "default_dim")]
        dim: usize,
    },
    /// 硅基流动 SiliconFlow API
    #[serde(rename = "siliconflow")]
    SiliconFlow {
        #[serde(default = "default_sf_model")]
        model: String,
        base_url: String,
        api_key: String,
        #[serde(default = "default_dim")]
        dim: usize,
    },
    /// OpenAI 兼容 API（自定义 base_url）
    #[serde(rename = "openai")]
    OpenAI {
        model: String,
        base_url: String,
        api_key: String,
        #[serde(default = "default_dim")]
        dim: usize,
    },
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        EmbeddingConfig::Local { model: default_local_model(), dim: default_dim() }
    }
}

fn default_local_model() -> String { "cradlering-hash-v1".to_string() }
fn default_sf_model() -> String { "Qwen/Qwen3-VL-Embedding-8B".to_string() }
fn default_dim() -> usize { 768 }

impl EmbeddingConfig {
    pub fn dim(&self) -> usize {
        match self {
            EmbeddingConfig::Local { dim, .. } => *dim,
            EmbeddingConfig::SiliconFlow { dim, .. } => *dim,
            EmbeddingConfig::OpenAI { dim, .. } => *dim,
        }
    }

    pub fn provider_label(&self) -> &'static str {
        match self {
            EmbeddingConfig::Local { .. } => "local",
            EmbeddingConfig::SiliconFlow { .. } => "siliconflow",
            EmbeddingConfig::OpenAI { .. } => "openai",
        }
    }

    /// 从 JSON 配置块解析（兼容旧版格式）
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let provider = v.get("provider")?.as_str()?;
        Some(match provider {
            "local" => EmbeddingConfig::Local {
                model: v.get("model").and_then(|m| m.as_str()).unwrap_or("cradlering-hash-v1").to_string(),
                dim: v.get("dim").and_then(|d| d.as_u64()).map(|d| d as usize).unwrap_or(768),
            },
            "siliconflow" => EmbeddingConfig::SiliconFlow {
                model: v.get("model").and_then(|m| m.as_str()).unwrap_or("Qwen/Qwen3-VL-Embedding-8B").to_string(),
                base_url: v.get("baseUrl").and_then(|u| u.as_str()).unwrap_or("https://api.siliconflow.cn/v1").to_string(),
                api_key: v.get("apiKey").and_then(|k| k.as_str()).unwrap_or("").to_string(),
                dim: v.get("dim").and_then(|d| d.as_u64()).map(|d| d as usize).unwrap_or(768),
            },
            "openai" => EmbeddingConfig::OpenAI {
                model: v.get("model").and_then(|m| m.as_str()).unwrap_or("text-embedding-3-small").to_string(),
                base_url: v.get("baseUrl").and_then(|u| u.as_str()).unwrap_or("https://api.openai.com/v1").to_string(),
                api_key: v.get("apiKey").and_then(|k| k.as_str()).unwrap_or("").to_string(),
                dim: v.get("dim").and_then(|d| d.as_u64()).map(|d| d as usize).unwrap_or(768),
            },
            _ => return None,
        })
    }
}

/// Embedding 提供者 trait
#[async_trait::async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// 把文本转向量
    async fn embed(&self, text: &str) -> anyhow::Result<EmbeddingVector>;

    /// 批量嵌入（默认逐条调用，子类可优化）
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<EmbeddingVector>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }

    /// 维度
    fn dim(&self) -> usize;

    /// 提供者标签（用于日志/UI）
    fn label(&self) -> &str;

    /// 是否为真实 Embedding（API 或本地模型）vs 占位实现
    fn is_real(&self) -> bool { true }
}

/// 本地占位 Embedding（哈希降维）
///
/// 使用 SHA-256 多次哈希 + 降维 + L2 归一化，生成稳定的伪向量。
/// 虽然不是真实语义 Embedding，但能保证：
/// 1. 相同文本 → 相同向量（确定性）
/// 2. 相似文本（编辑距离近）→ 相似向量（哈希局部性）
/// 3. 完全离线、零依赖、零成本
///
/// 仅作为"无可用模型时"的降级方案。
pub struct LocalHashEmbedding {
    dim: usize,
    salt: String,
}

impl LocalHashEmbedding {
    pub fn new(dim: usize) -> Self {
        Self { dim, salt: "cradlering-v1".to_string() }
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for LocalHashEmbedding {
    async fn embed(&self, text: &str) -> anyhow::Result<EmbeddingVector> {
        Ok(hash_embed(text, &self.salt, self.dim))
    }
    fn dim(&self) -> usize { self.dim }
    fn label(&self) -> &str { "local-hash" }
    fn is_real(&self) -> bool { false }
}

/// 哈希降维算法：基于字符 n-gram + SHA-256 哈希到向量空间
pub fn hash_embed(text: &str, salt: &str, dim: usize) -> EmbeddingVector {
    use sha2::{Digest, Sha256};
    let mut vec = vec![0.0f32; dim];
    let normalized = text.trim().to_lowercase();
    if normalized.is_empty() {
        return vec;
    }

    // 字符 bigram 滑窗
    let chars: Vec<char> = normalized.chars().collect();
    let ngrams: Vec<String> = if chars.len() <= 1 {
        vec![normalized.clone()]
    } else {
        (0..chars.len().saturating_sub(1))
            .map(|i| chars[i..(i + 2).min(chars.len())].iter().collect())
            .chain(std::iter::once(chars[chars.len() - 1].to_string()))
            .collect()
    };

    // 单词级（按空格/标点切分）
    let words: Vec<&str> = normalized.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();

    let mut push_hash = |bucket_fn: &dyn Fn(&[u8]) -> usize| {
        for ng in &ngrams {
            let mut hasher = Sha256::new();
            hasher.update(salt.as_bytes());
            hasher.update(b":ngram:");
            hasher.update(ng.as_bytes());
            let hash = hasher.finalize();
            let idx = bucket_fn(&hash) % dim;
            vec[idx] += 1.0;
        }
        for w in &words {
            let mut hasher = Sha256::new();
            hasher.update(salt.as_bytes());
            hasher.update(b":word:");
            hasher.update(w.as_bytes());
            let hash = hasher.finalize();
            let idx = bucket_fn(&hash) % dim;
            vec[idx] += 2.0; // 词级权重更高
        }
    };

    // 多桶减少碰撞
    push_hash(&|h| {
        let v = (h[0] as usize) | ((h[1] as usize) << 8);
        v
    });
    push_hash(&|h| {
        let v = (h[2] as usize) | ((h[3] as usize) << 8) | ((h[4] as usize) << 16);
        v
    });

    // L2 归一化
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }
    vec
}

/// OpenAI 兼容的 API Embedding
pub struct ApiEmbedding {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    dim: usize,
    label: String,
}

impl ApiEmbedding {
    pub fn new(base_url: String, api_key: String, model: String, dim: usize, label: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
            model,
            dim,
            label,
        }
    }

    /// 硅基流动（默认 Qwen/Qwen3-VL-Embedding-8B）
    pub fn siliconflow(api_key: String, model: String, dim: usize) -> Self {
        Self::new(
            "https://api.siliconflow.cn/v1".to_string(),
            api_key,
            model,
            dim,
            "siliconflow".to_string(),
        )
    }

    /// OpenAI 官方
    pub fn openai(api_key: String, model: String, dim: usize) -> Self {
        Self::new(
            "https://api.openai.com/v1".to_string(),
            api_key,
            model,
            dim,
            "openai".to_string(),
        )
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for ApiEmbedding {
    async fn embed(&self, text: &str) -> anyhow::Result<EmbeddingVector> {
        let mut results = self.embed_batch(&[text]).await?;
        results.pop().ok_or_else(|| anyhow::anyhow!("empty embedding response"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<EmbeddingVector>> {
        if self.api_key.is_empty() {
            anyhow::bail!("API Key 未配置");
        }
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });
        let resp = self.client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Embedding API 失败 [{}]: {}", status, truncate_str(&text, 200));
        }
        let json: serde_json::Value = resp.json().await?;
        let data = json.get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| anyhow::anyhow!("响应缺少 data 字段"))?;
        let mut out = Vec::with_capacity(data.len());
        // 按 index 排序保证顺序
        let mut sorted: Vec<&serde_json::Value> = data.iter().collect();
        sorted.sort_by_key(|v| v.get("index").and_then(|i| i.as_u64()).unwrap_or(0));
        for item in sorted {
            let emb = item.get("embedding")
                .and_then(|e| e.as_array())
                .ok_or_else(|| anyhow::anyhow!("embedding 字段缺失"))?;
            let vec: EmbeddingVector = emb.iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            out.push(vec);
        }
        Ok(out)
    }

    fn dim(&self) -> usize { self.dim }
    fn label(&self) -> &str { &self.label }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}...", s.chars().take(max).collect::<String>()) }
}
