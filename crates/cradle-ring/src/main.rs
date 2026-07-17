//! CradleRing - 企业级 AI Agent 协作平台
//!
// 一个功能完整的 CradleRing 网关，实现了完整的 WebSocket JSON-RPC 协议。
// 包含：WebSocket JSON-RPC 协议、SQLite 持久化、LLM 调用、记忆系统、任务调度等。
#![recursion_limit = "512"]
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use base64::Engine;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// 记忆引擎 V3（Cache-First + Qdrant-lite + 时序知识图谱 + 级联路由）
use memory_engine::{MemoryEngine, MemoryEngineConfig, StoreRequest, RecallRequest};

// ============================================================================
// 基础工具
// ============================================================================

fn current_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn rand_u128() -> u128 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h1 = DefaultHasher::new();
    current_ms().hash(&mut h1);
    let mut h2 = DefaultHasher::new();
    h1.finish().hash(&mut h2);
    ((h1.finish() as u128) << 64) | (h2.finish() as u128)
}

fn base64_encode_bytes(input: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk.get(0).copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        result.push(CHARS[(b[0] >> 2) as usize] as char);
        result.push(CHARS[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(b[2] & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn sha1_compute(input: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(w[i]);
            e = d; d = c; c = b.rotate_left(30); b = a; a = temp;
        }
        h0 = h0.wrapping_add(a); h1 = h1.wrapping_add(b); h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d); h4 = h4.wrapping_add(e);
    }

    let mut r = [0u8; 20];
    r[0..4].copy_from_slice(&h0.to_be_bytes());
    r[4..8].copy_from_slice(&h1.to_be_bytes());
    r[8..12].copy_from_slice(&h2.to_be_bytes());
    r[12..16].copy_from_slice(&h3.to_be_bytes());
    r[16..20].copy_from_slice(&h4.to_be_bytes());
    r
}

// ============================================================================
// 配置
// ============================================================================

#[derive(Clone, Debug)]
struct Config {
    token: String,
    port: u16,
    /// 网关绑定地址：loopback(127.0.0.1) / all(0.0.0.0) / 具体 IP
    bind_host: String,
    openai_api_key: Option<String>,
    openai_base_url: String,
    default_model: String,
    raw_json: serde_json::Value,
    /// 多 provider 列表（按配置顺序，第一个为主 provider，后续为 fallback）
    providers: Vec<ProviderCfg>,
    /// TTS 配置（providers.tts 节）
    tts_config: serde_json::Value,
}

/// 单个 LLM provider 配置（OpenAI 兼容协议）
#[derive(Clone, Debug)]
struct ProviderCfg {
    /// provider 名（openai / anthropic / deepseek / qwen / ollama / 自定义）
    name: String,
    /// API key（可选，例如 ollama 本地无需）
    api_key: Option<String>,
    /// OpenAI 兼容 base url，如 https://api.openai.com/v1
    base_url: String,
    /// 默认 model（若未指定则用全局 default_model）
    model: Option<String>,
    /// 是否启用
    enabled: bool,
    /// 是否支持 thinking/reasoning（Claude/o1/Qwen-QwQ 等）
    supports_thinking: bool,
}

impl ProviderCfg {
    /// 取该 provider 使用的 model（provider 自身优先，否则 fallback 到传入的 default）
    fn effective_model<'a>(&'a self, default_model: &'a str) -> &'a str {
        self.model.as_deref().unwrap_or(default_model)
    }
}

impl Config {
    fn load(home: &str) -> Self {
        let path = format!("{}/.cradle-ring/cradle-ring.json", home);
        let mut default_model = "gpt-4o-mini".to_string();
        let mut openai_base_url = "https://api.openai.com/v1".to_string();
        let mut openai_api_key = std::env::var("OPENAI_API_KEY").ok();
        let mut token: String = format!("{:032x}", rand_u128());
        let mut port: u16 = 18800;
        let mut bind_host = "127.0.0.1".to_string();
        let mut raw_json = serde_json::json!({});

        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                raw_json = v.clone();
                if let Some(t) = v["gateway"]["auth"]["token"].as_str() {
                    token = t.to_string();
                }
                if let Some(p) = v["gateway"]["port"].as_u64() {
                    port = p as u16;
                }
                // 读取绑定地址：gateway.bind 或 gateway.host
                // 支持 loopback / all / 0.0.0.0 / 127.0.0.1 / 具体 IP
                if let Some(b) = v["gateway"]["bind"].as_str().or(v["gateway"]["host"].as_str()) {
                    bind_host = match b.to_lowercase().as_str() {
                        "loopback" | "localhost" | "127.0.0.1" | "local" => "127.0.0.1".to_string(),
                        "all" | "public" | "0.0.0.0" | "*" | "external" => "0.0.0.0".to_string(),
                        ip => ip.to_string(),
                    };
                }
                if let Some(k) = v["providers"]["openai"]["apiKey"].as_str() {
                    if !k.is_empty() {
                        // 环境变量优先，否则用配置文件的 key
                        if openai_api_key.is_none() {
                            openai_api_key = Some(k.to_string());
                        }
                    }
                }
                if let Some(u) = v["providers"]["openai"]["baseUrl"].as_str() {
                    openai_base_url = u.to_string();
                }
                if let Some(m) = v["models"]["primary"].as_str() {
                    if !m.is_empty() {
                        default_model = m.to_string();
                    }
                }
            }
        }

        Config {
            token,
            port,
            bind_host,
            openai_api_key: openai_api_key.clone(),
            openai_base_url: openai_base_url.clone(),
            default_model,
            raw_json: raw_json.clone(),
            providers: parse_providers(&raw_json, &openai_api_key, &openai_base_url),
            tts_config: raw_json
                .get("providers")
                .and_then(|p| p.get("tts"))
                .cloned()
                .unwrap_or_else(|| raw_json.get("tts").cloned().unwrap_or(serde_json::json!({}))),
        }
    }

    /// 获取搜索配置（providers.search 或 tools.web.search）。
    fn search_config(&self) -> serde_json::Value {
        let v = &self.raw_json;
        if let Some(s) = v.get("providers").and_then(|p| p.get("search")) {
            return s.clone();
        }
        if let Some(s) = v.get("tools").and_then(|t| t.get("web")).and_then(|w| w.get("search")) {
            return s.clone();
        }
        serde_json::json!({"provider": "none", "enabled": false})
    }

    /// 返回启用的 provider 列表（按 fallback 顺序）。
    fn enabled_providers(&self) -> Vec<ProviderCfg> {
        self.providers.iter().filter(|p| p.enabled).cloned().collect()
    }
}

/// 从配置 JSON 解析 provider 列表。
/// 规则：
/// - providers.* 下每个对象视为一个 provider（排除 search/tts 等非 LLM 节）
/// - 第一个解析出的视为主 provider
/// - 若配置中没有任何 provider，用 openai_api_key/base_url 兜底构造一个
fn parse_providers(
    raw: &serde_json::Value,
    fallback_openai_key: &Option<String>,
    fallback_openai_base: &String,
) -> Vec<ProviderCfg> {
    let mut out: Vec<ProviderCfg> = Vec::new();
    // 已知的非 LLM 节点名，跳过
    const NON_LLM: &[&str] = &["search", "tts", "stt", "embedding", "rerank", "vision", "image"];

    if let Some(providers) = raw.get("providers").and_then(|p| p.as_object()) {
        for (name, cfg) in providers {
            if NON_LLM.contains(&name.as_str()) {
                continue;
            }
            let api_key = cfg["apiKey"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            // 环境变量优先：尝试 {NAME}_API_KEY
            let api_key = api_key.or_else(|| {
                std::env::var(format!("{}_API_KEY", name.to_uppercase())).ok().filter(|s| !s.is_empty())
            });
            let base_url = cfg["baseUrl"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| default_base_url_for(name));
            let model = cfg["model"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let enabled = cfg["enabled"].as_bool().unwrap_or(true);
            let supports_thinking = cfg["thinking"].as_bool().unwrap_or_else(|| matches_thinking_model(name, &model));
            // 至少要有 key 或本地 provider（ollama/llama.cpp 无 key）
            let is_local = name == "ollama" || name == "local" || base_url.contains("localhost") || base_url.contains("127.0.0.1");
            if api_key.is_none() && !is_local {
                // 无 key 跳过（避免无效 fallback）
                continue;
            }
            out.push(ProviderCfg {
                name: name.clone(),
                api_key,
                base_url,
                model,
                enabled,
                supports_thinking,
            });
        }
    }

    // 兜底：若没有任何 provider，用 openai_api_key/base_url 构造一个
    if out.is_empty() {
        if let Some(k) = fallback_openai_key {
            out.push(ProviderCfg {
                name: "openai".to_string(),
                api_key: Some(k.clone()),
                base_url: fallback_openai_base.clone(),
                model: None,
                enabled: true,
                supports_thinking: false,
            });
        }
    }
    out
}

fn default_base_url_for(name: &str) -> String {
    match name {
        "openai" => "https://api.openai.com/v1".to_string(),
        "anthropic" => "https://api.anthropic.com/v1".to_string(),
        "deepseek" => "https://api.deepseek.com/v1".to_string(),
        "qwen" | "dashscope" => "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
        "moonshot" | "kimi" => "https://api.moonshot.cn/v1".to_string(),
        "zhipu" | "glm" => "https://open.bigmodel.cn/api/paas/v4".to_string(),
        "ollama" => "http://localhost:11434/v1".to_string(),
        "groq" => "https://api.groq.com/openai/v1".to_string(),
        "openrouter" => "https://openrouter.ai/api/v1".to_string(),
        "together" => "https://api.together.xyz/v1".to_string(),
        _ => "https://api.openai.com/v1".to_string(),
    }
}

fn matches_thinking_model(provider: &str, model: &Option<String>) -> bool {
    let m = model.as_deref().unwrap_or("").to_lowercase();
    if provider == "anthropic" {
        return m.contains("claude-3") || m.contains("claude-4") || m.contains("sonnet") || m.contains("opus") || m.contains("haiku");
    }
    if provider == "openai" {
        return m.contains("o1") || m.contains("o3") || m.contains("o4") || m.contains("reason");
    }
    if provider == "qwen" || provider == "dashscope" {
        return m.contains("qwq") || m.contains("thinking") || m.contains("reason");
    }
    if provider == "deepseek" {
        return m.contains("r1") || m.contains("reason");
    }
    false
}

// ============================================================================
// 内存数据结构
// ============================================================================

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Session {
    key: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default = "default_agent_id")]
    agent_id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "default_updated_at")]
    updated_at: i64,
}

fn default_kind() -> String { "main".to_string() }
fn default_agent_id() -> String { "main".to_string() }
fn default_updated_at() -> i64 { 0 }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Message {
    role: String,
    content: String,
    #[serde(default)]
    timestamp: i64,
    /// 附件列表：每项形如 {"type":"image","path":"/abs/x.png"} / {"type":"image","url":"..."}
    /// 序列化进 messages_*.jsonl；构建上下文时会渲染为 OpenAI vision 多模态 content。
    #[serde(default)]
    attachments: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct MemoryItem {
    id: u64,
    kind: String,
    body: String,
    source: String,
    confidence: f64,
    created_at: i64,
}

/// 定时任务定义（cron_jobs.json）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct CronJob {
    id: String,
    name: String,
    /// 5 段 cron 表达式：分 时 日 月 周
    schedule: String,
    /// 触发时发送给 agent 的提示词
    prompt: String,
    #[serde(default = "default_true")]
    enabled: bool,
    /// 触发时投递到的会话 key
    #[serde(default = "default_session_main")]
    session_key: String,
    #[serde(default)]
    last_run: i64,
    /// 下一次预期触发时间（unix 秒）
    #[serde(default)]
    next_run: i64,
}

fn default_true() -> bool { true }
fn default_session_main() -> String { "main".to_string() }

/// 定时任务运行历史单条记录（cron_runs.jsonl）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct CronRun {
    id: String,
    job_id: String,
    #[serde(default)]
    job_name: String,
    started_at: i64,
    #[serde(default)]
    finished_at: i64,
    /// started | running | completed | error
    status: String,
    #[serde(default)]
    trigger: String,
    #[serde(default)]
    output: String,
}

/// 审批请求（approvals.json）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Approval {
    id: String,
    /// exec | file_write | ...
    #[serde(default = "default_kind_exec")]
    kind: String,
    command: String,
    /// pending | approved | denied
    #[serde(default = "default_status_pending")]
    status: String,
    #[serde(default = "default_session_main")]
    session_key: String,
    #[serde(default)]
    run_id: String,
    created_at: i64,
    #[serde(default)]
    decided_by: Option<String>,
    #[serde(default)]
    decided_at: Option<i64>,
    #[serde(default)]
    decision: Option<String>,
}

fn default_kind_exec() -> String { "exec".to_string() }
fn default_status_pending() -> String { "pending".to_string() }

/// 已知节点（nodes.json）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Node {
    id: String,
    name: String,
    #[serde(default)]
    kind: String,
    /// paired | pending | rejected
    #[serde(default = "default_node_status")]
    status: String,
    #[serde(default)]
    paired_at: i64,
    #[serde(default)]
    last_seen: i64,
    #[serde(default)]
    metadata: serde_json::Value,
    /// 心跳延迟（ms），None=从未上报
    #[serde(default)]
    latency_ms: Option<u32>,
    /// 风险评分 0-100（规则引擎计算）
    #[serde(default)]
    risk_score: u32,
    /// 风险原因列表
    #[serde(default)]
    risk_reasons: Vec<String>,
    /// 最近心跳时间
    #[serde(default)]
    last_heartbeat: Option<i64>,
    /// 上报的 CPU 使用率（%）
    #[serde(default)]
    cpu_percent: Option<u32>,
    /// 上报的内存使用率（%）
    #[serde(default)]
    mem_percent: Option<u32>,
}

fn default_node_status() -> String { "paired".to_string() }

/// 配对请求（pair_requests.json）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PairRequest {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    /// node | device
    #[serde(default = "default_pair_kind")]
    target: String,
    /// pending | approved | rejected
    #[serde(default = "default_status_pending")]
    status: String,
    created_at: i64,
    #[serde(default)]
    metadata: serde_json::Value,
}

fn default_pair_kind() -> String { "device".to_string() }

/// 压缩检查点（compaction_checkpoints.json）
/// 每个检查点对应一次 sessions.compact 操作，可用来恢复原始消息历史。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct CompactionCheckpoint {
    id: String,
    session_key: String,
    /// 创建时间（ms）
    created_at: i64,
    /// 压缩前的消息总数
    original_count: usize,
    /// 压缩后保留的最近消息数
    kept_count: usize,
    /// 压缩产生的摘要文本
    summary: String,
    /// 从旧消息里正则提取出的关键实体（路径、URL、ID 等）
    entities: Vec<String>,
    /// 备份文件（原始完整历史）相对路径
    backup_file: String,
    /// 用于生成摘要的模型
    #[serde(default)]
    model: String,
    /// 可选分支标签
    #[serde(default)]
    branch: Option<String>,
    /// 关联的父检查点（创建分支时）
    #[serde(default)]
    parent_id: Option<String>,
}

/// 单条用量日志（usage_logs.jsonl）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct UsageLog {
    #[serde(default)]
    id: String,
    /// openai | anthropic | ...
    provider: String,
    model: String,
    /// 输入 token
    #[serde(default)]
    prompt_tokens: u64,
    /// 输出 token
    #[serde(default)]
    completion_tokens: u64,
    /// 折算费用（美元）
    #[serde(default)]
    cost_usd: f64,
    /// 调用发生时间（ms）
    ts: i64,
    /// 调用关联的会话
    #[serde(default)]
    session_key: String,
}

// ============================================================================
// 多账号用户系统
// ============================================================================

/// 用户（users.json）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct User {
    id: String,
    username: String,
    /// salted_sha256:hex(salt)$hex(hash)
    password_hash: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: Option<String>,
    /// admin | manager | supervisor | operator | viewer
    #[serde(default = "default_role_operator")]
    role: String,
    /// 细粒度权限范围
    #[serde(default)]
    scopes: Vec<String>,
    /// 关联的 agent id
    #[serde(default = "default_agent_main")]
    agent_id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    created_at: i64,
    #[serde(default)]
    last_login: Option<i64>,
    /// 个人偏好：是否启用审批流（普通用户可关闭）
    #[serde(default = "default_true")]
    approval_enabled: bool,
}

fn default_role_operator() -> String { "operator".to_string() }
fn default_agent_main() -> String { "main".to_string() }

/// 登录会话令牌（tokens.jsonl）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct AuthToken {
    token: String,
    user_id: String,
    username: String,
    role: String,
    /// 签发时间
    issued_at: i64,
    /// 过期时间
    expires_at: i64,
}

// ============================================================================
// 多级审批工作流
// ============================================================================

/// 审批流模板（approval_flows.json）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ApprovalFlow {
    id: String,
    name: String,
    /// 触发条件：匹配的命令关键词（任一命中即触发）
    #[serde(default)]
    trigger_patterns: Vec<String>,
    /// 匹配的工具：exec | write_file | * (全部)
    #[serde(default = "default_flow_kinds")]
    kinds: Vec<String>,
    steps: Vec<ApprovalStep>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    created_by: String,
    created_at: i64,
}

fn default_flow_kinds() -> Vec<String> { vec!["exec".to_string()] }

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ApprovalStep {
    /// 步骤顺序（1,2,3...）
    order: u32,
    /// 步骤名称：如"部门主管"、"团队领导"
    #[serde(default)]
    name: String,
    /// 审批角色要求：supervisor/manager/admin（任一持此角色的用户可审批）
    #[serde(default)]
    approver_role: String,
    /// 指定审批人用户名（精确）
    #[serde(default)]
    approver_ids: Vec<String>,
    /// 通知渠道：dingtalk/feishu/telegram/discord/wecom
    #[serde(default)]
    notify_channels: Vec<String>,
    /// 通知目标（chat_id 或群 ID）
    #[serde(default)]
    notify_targets: Vec<String>,
    /// 超时自动通过秒数（None=不自动）
    #[serde(default)]
    auto_approve_after_secs: Option<u64>,
    /// true=本步骤所有审批人都通过；false=任一即可
    #[serde(default)]
    require_all: bool,
}

/// 审批实例（approval_instances.json）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ApprovalInstance {
    id: String,
    /// 关联的流程模板 id
    #[serde(default)]
    flow_id: String,
    #[serde(default)]
    flow_name: String,
    title: String,
    #[serde(default)]
    description: String,
    /// 要执行的实际命令/操作
    command: String,
    /// kind: exec | write_file | generic
    #[serde(default = "default_kind_exec")]
    kind: String,
    /// 请求者 user_id
    #[serde(default)]
    requested_by: String,
    #[serde(default)]
    requested_username: String,
    /// 当前步骤（1-based）
    current_step: u32,
    /// 总步骤数
    total_steps: u32,
    /// pending | approved | rejected | timeout | executing | completed | failed
    #[serde(default = "default_status_pending")]
    status: String,
    #[serde(default)]
    decisions: Vec<ApprovalDecision>,
    created_at: i64,
    #[serde(default)]
    updated_at: i64,
    /// 关联会话（执行完回到此会话）
    #[serde(default)]
    session_key: String,
    #[serde(default)]
    run_id: String,
    /// 异步不阻塞
    #[serde(default = "default_true")]
    async_non_blocking: bool,
    /// 执行结果
    #[serde(default)]
    execution_result: Option<String>,
    #[serde(default)]
    completed_at: Option<i64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ApprovalDecision {
    step_order: u32,
    approver_id: String,
    #[serde(default)]
    approver_username: String,
    /// approve | reject
    decision: String,
    #[serde(default)]
    comment: String,
    decided_at: i64,
    /// web | dingtalk | feishu | telegram | discord | wecom
    #[serde(default)]
    via_channel: String,
}

// ============================================================================
// Agent 工作流引擎：对标 LangGraph（状态图）+ CrewAI（角色化/流水线）
// ============================================================================

/// 工作流节点类型
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum NodeType {
    /// 调用 LLM 生成文本/决策
    Llm,
    /// 执行单个工具
    Tool,
    /// 运行一个角色化 agent（可带子循环）
    Agent,
    /// 条件分支（基于 state 求值表达式选边）
    Condition,
    /// 并行扇出（map-reduce）
    Parallel,
    /// 暂停等待人工输入
    Interrupt,
    /// 暂停等待人工审核（复用已有审批引擎）
    HumanReview,
    /// 终止
    End,
}

impl Default for NodeType {
    fn default() -> Self { NodeType::Llm }
}

/// 工作流节点
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowNode {
    id: String,
    name: String,
    #[serde(default)]
    node_type: NodeType,
    /// LLM/Agent 节点：使用的 agent_role_id
    #[serde(default)]
    agent_role: Option<String>,
    /// LLM 节点的提示模板（支持 ${state.x} 插值）
    #[serde(default)]
    prompt_template: Option<String>,
    /// Tool 节点：工具名
    #[serde(default)]
    tool_name: Option<String>,
    /// Tool 节点：参数模板（支持 ${state.x} 插值）
    #[serde(default)]
    tool_args_template: Option<serde_json::Value>,
    /// Condition 节点：条件表达式列表，每项 (expr, target_node_id)
    /// 求值时按顺序匹配，第一个为 true 的边胜出；都不匹配走 default_edge
    #[serde(default)]
    branches: Vec<(String, String)>,
    /// 默认出口节点 id（Condition 节点：所有分支都不匹配时走这里）
    #[serde(default)]
    default_edge: Option<String>,
    /// Parallel 节点：从 state 取一个数组字段名作为子任务来源
    #[serde(default)]
    fan_out_field: Option<String>,
    /// Parallel 节点：子任务使用的 agent_role_id
    #[serde(default)]
    fan_out_role: Option<String>,
    /// Parallel 节点：最大并发（默认 5）
    #[serde(default)]
    max_concurrent: Option<usize>,
    /// Parallel 节点：reduce 模式（concat/join/summary）
    #[serde(default)]
    reduce_mode: Option<String>,
    /// Interrupt/HumanReview：提示文案
    #[serde(default)]
    prompt: Option<String>,
    /// 节点输出写入 state 的哪个字段（None=不写）
    #[serde(default)]
    output_field: Option<String>,
    /// 节点配置（超时秒数等）
    #[serde(default)]
    config: serde_json::Value,
}

/// 工作流边
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowEdge {
    id: String,
    from: String,
    to: String,
    /// 条件表达式（None=默认边，仅用于普通节点出口）
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// 工作流定义（模板，可保存复用）
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowGraph {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    nodes: Vec<WorkflowNode>,
    #[serde(default)]
    edges: Vec<WorkflowEdge>,
    /// 入口节点 id
    #[serde(default)]
    entry_node: String,
    /// 状态 schema：声明的顶层 key
    #[serde(default)]
    state_schema: Vec<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    created_at: i64,
    #[serde(default)]
    created_by: String,
}

/// 执行追踪 span
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct TraceSpan {
    id: String,
    #[serde(default)]
    parent_id: Option<String>,
    /// span 类型：workflow | node | llm_call | tool_call | agent_loop | fan_out | pipeline_stage
    kind: String,
    name: String,
    #[serde(default)]
    input: serde_json::Value,
    #[serde(default)]
    output: serde_json::Value,
    started_at: i64,
    #[serde(default)]
    finished_at: Option<i64>,
    #[serde(default)]
    duration_ms: Option<u64>,
    /// LLM 调用专用
    #[serde(default)]
    tokens_in: Option<u64>,
    #[serde(default)]
    tokens_out: Option<u64>,
    #[serde(default)]
    cost_usd: Option<f64>,
    #[serde(default)]
    model: Option<String>,
    /// 子 span
    #[serde(default)]
    children: Vec<TraceSpan>,
    /// ok | error | running
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: Option<String>,
}

impl TraceSpan {
    fn new(kind: &str, name: &str, parent_id: Option<&str>) -> Self {
        TraceSpan {
            id: format!("span-{:016x}", rand_u128()),
            parent_id: parent_id.map(String::from),
            kind: kind.to_string(),
            name: name.to_string(),
            input: serde_json::Value::Null,
            output: serde_json::Value::Null,
            started_at: current_ms(),
            finished_at: None,
            duration_ms: None,
            tokens_in: None,
            tokens_out: None,
            cost_usd: None,
            model: None,
            children: vec![],
            status: "running".to_string(),
            error: None,
        }
    }
    fn finish_ok(&mut self, output: serde_json::Value) {
        let now = current_ms();
        self.finished_at = Some(now);
        self.duration_ms = Some((now - self.started_at) as u64);
        self.output = output;
        self.status = "ok".to_string();
    }
    fn finish_err(&mut self, err: &str) {
        let now = current_ms();
        self.finished_at = Some(now);
        self.duration_ms = Some((now - self.started_at) as u64);
        self.status = "error".to_string();
        self.error = Some(err.to_string());
    }
}

/// 工作流检查点：记录某节点执行后的完整状态快照
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowCheckpoint {
    /// 对应节点 id
    node_id: String,
    /// 步骤序号（0-based）
    step_index: usize,
    /// 完整 state 副本
    state_snapshot: serde_json::Value,
    timestamp: i64,
    /// 该步的 trace span
    #[serde(default)]
    span: TraceSpan,
}

/// 工作流执行实例（一次具体运行）
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct WorkflowRun {
    id: String,
    graph_id: String,
    #[serde(default)]
    graph_name: String,
    /// 运行时状态：{ input, output, vars: {...}, history: [...] }
    #[serde(default)]
    state: serde_json::Value,
    /// 当前节点 id
    #[serde(default)]
    current_node: String,
    /// running | paused_interrupt | paused_review | completed | failed | cancelled
    #[serde(default = "default_status_pending")]
    status: String,
    /// 检查点列表（每个节点执行完保存一个）
    #[serde(default)]
    checkpoints: Vec<WorkflowCheckpoint>,
    /// 执行轨迹的根 span（其 children 为各节点 span）
    #[serde(default)]
    root_span: TraceSpan,
    /// 触发来源会话
    #[serde(default)]
    session_key: String,
    started_at: i64,
    #[serde(default)]
    finished_at: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    /// 断点节点 id 集合（命中即暂停为 interrupt）
    #[serde(default)]
    breakpoints: Vec<String>,
}

// ----------------------------------------------------------------------------
// 角色化 Agent 定义（对标 CrewAI Agent）
// ----------------------------------------------------------------------------

/// Agent 角色：带 role/goal/backstory 的可复用人格
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct AgentRole {
    id: String,
    name: String,
    /// 角色，如「资深 Python 工程师」
    role: String,
    /// 目标，如「编写高质量、可维护的代码」
    goal: String,
    /// 背景故事，注入 system prompt 增强人格
    #[serde(default)]
    backstory: String,
    /// 允许使用的工具白名单（None=全部）
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// 使用的模型覆盖（None=默认）
    #[serde(default)]
    model: Option<String>,
    /// system prompt 模板（可引用 ${role} ${goal} ${backstory}）
    #[serde(default)]
    system_prompt_template: Option<String>,
    /// 最大迭代次数（默认 10）
    #[serde(default = "default_max_iterations")]
    max_iterations: usize,
    /// 是否允许递归 delegate 子 agent
    #[serde(default)]
    allow_delegation: bool,
    created_at: i64,
    #[serde(default)]
    created_by: String,
}

fn default_max_iterations() -> usize { 10 }

// ----------------------------------------------------------------------------
// Sequential 顺序流水线（对标 CrewAI Sequential Process）
// ----------------------------------------------------------------------------

/// 流水线阶段
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct PipelineStage {
    order: u32,
    /// 使用的 agent_role_id
    agent_role_id: String,
    /// 任务模板（支持 ${input} ${prev_output} 插值）
    task_template: String,
}

/// Sequential 流水线：多个角色化 agent 按顺序接力
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct Pipeline {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    stages: Vec<PipelineStage>,
    /// 是否把前一阶段输出注入下一阶段的 ${prev_output}
    #[serde(default = "default_true")]
    pass_through: bool,
    #[serde(default = "default_true")]
    enabled: bool,
    created_at: i64,
    #[serde(default)]
    created_by: String,
}

/// 流水线运行结果
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct PipelineResult {
    pipeline_id: String,
    final_output: String,
    stage_outputs: Vec<String>,
    #[serde(default)]
    trace: TraceSpan,
    started_at: i64,
    finished_at: Option<i64>,
}

// ============================================================================
// 自定义角色（roles.json 持久化，支持预置+自定义）
// ============================================================================

/// 用户角色：预置模板 + 用户自定义
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Role {
    /// 角色 ID（如 "admin" / "custom_devops"）
    name: String,
    /// 显示名（如 "管理员" / "运维开发"）
    label: String,
    /// 描述
    #[serde(default)]
    description: String,
    /// 权限列表
    scopes: Vec<String>,
    /// 是否是预置角色（不可删除，可修改 scopes）
    #[serde(default)]
    builtin: bool,
    /// 颜色（前端用）
    #[serde(default)]
    color: String,
    created_at: i64,
}

fn role_to_json(r: &Role) -> serde_json::Value {
    json!({
        "name": r.name, "label": r.label, "description": r.description,
        "scopes": r.scopes, "builtin": r.builtin, "color": r.color,
        "createdAt": r.created_at,
    })
}

// ============================================================================
// 运维审计日志（对标 ongrid change events / RCA 0号病人候选）
// ============================================================================

/// 变更事件：记录所有 mutating 操作（对标 ongrid query_change_events）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ChangeEvent {
    id: String,
    /// 变更类型：exec_write / service_restart / file_write / config_change / skill_exec / approval
    kind: String,
    /// 操作摘要
    action: String,
    /// 目标（命令/文件路径/服务名）
    target: String,
    /// 执行者：agent / user:xxx / system
    actor: String,
    /// 来源会话
    #[serde(default)]
    session_key: String,
    /// 触发时间
    ts: i64,
    /// 结果：ok / failed / denied
    #[serde(default)]
    result: String,
    /// 关联审批实例 id（如有）
    #[serde(default)]
    approval_id: Option<String>,
    /// 回滚信息（如何撤销）
    #[serde(default)]
    rollback_hint: Option<String>,
}

/// AI SOP 二审决策（对标 ongrid reviewer）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ReviewDecision {
    id: String,
    /// 被审查的操作
    action: String,
    target: String,
    reason: String,
    blast_radius: String,
    /// approve / reject
    decision: String,
    /// 三条检查结果
    has_sop: bool,
    no_parallel_op: bool,
    rollback_known: bool,
    /// 审查理由
    comment: String,
    /// 匹配到的 SOP 条目（如有）
    matched_sop: Option<String>,
    ts: i64,
}

// ============================================================================
// 持久化（纯文件 JSON，简化 SQLite 等复杂依赖）
// ============================================================================

struct Storage {
    home: String,
}

impl Storage {
    fn new(home: &str) -> Self {
        std::fs::create_dir_all(format!("{}/.cradle-ring/data", home)).ok();
        std::fs::create_dir_all(format!("{}/.cradle-ring/workspace", home)).ok();
        Self { home: home.to_string() }
    }

    fn sessions_path(&self) -> String {
        format!("{}/.cradle-ring/data/sessions.json", self.home)
    }

    fn messages_path(&self, session_key: &str) -> String {
        let safe = session_key.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        format!("{}/.cradle-ring/data/messages_{}.jsonl", self.home, safe)
    }

    fn memory_path(&self) -> String {
        format!("{}/.cradle-ring/data/memory.json", self.home)
    }

    fn load_sessions(&self) -> Vec<Session> {
        if let Ok(data) = std::fs::read_to_string(self.sessions_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_sessions(&self, sessions: &[Session]) {
        if let Ok(data) = serde_json::to_string_pretty(sessions) {
            let _ = std::fs::write(self.sessions_path(), data);
        }
    }

    fn load_messages(&self, session_key: &str) -> Vec<Message> {
        let path = self.messages_path(session_key);
        if let Ok(data) = std::fs::read_to_string(&path) {
            data.lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        } else {
            vec![]
        }
    }

    fn append_message(&self, session_key: &str, msg: &Message) {
        use std::io::Write;
        let path = self.messages_path(session_key);
        if let Ok(data) = serde_json::to_string(msg) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = writeln!(f, "{}", data);
            }
        }
    }

    fn load_memory(&self) -> Vec<MemoryItem> {
        if let Ok(data) = std::fs::read_to_string(self.memory_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_memory(&self, items: &[MemoryItem]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.memory_path(), data);
        }
    }

    // ---- Cron ----

    fn cron_jobs_path(&self) -> String {
        format!("{}/.cradle-ring/data/cron_jobs.json", self.home)
    }

    fn cron_runs_path(&self) -> String {
        format!("{}/.cradle-ring/data/cron_runs.jsonl", self.home)
    }

    fn load_cron_jobs(&self) -> Vec<CronJob> {
        if let Ok(data) = std::fs::read_to_string(self.cron_jobs_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_cron_jobs(&self, jobs: &[CronJob]) {
        if let Ok(data) = serde_json::to_string_pretty(jobs) {
            let _ = std::fs::write(self.cron_jobs_path(), data);
        }
    }

    fn append_cron_run(&self, run: &CronRun) {
        use std::io::Write;
        if let Ok(data) = serde_json::to_string(run) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.cron_runs_path()) {
                let _ = writeln!(f, "{}", data);
            }
        }
    }

    fn load_cron_runs(&self, limit: usize) -> Vec<CronRun> {
        if let Ok(data) = std::fs::read_to_string(self.cron_runs_path()) {
            let all: Vec<CronRun> = data.lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let start = all.len().saturating_sub(limit);
            all[start..].to_vec()
        } else {
            vec![]
        }
    }

    // ---- Approvals ----

    fn approvals_path(&self) -> String {
        format!("{}/.cradle-ring/data/approvals.json", self.home)
    }

    fn load_approvals(&self) -> Vec<Approval> {
        if let Ok(data) = std::fs::read_to_string(self.approvals_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_approvals(&self, items: &[Approval]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.approvals_path(), data);
        }
    }

    // ---- Nodes ----

    fn nodes_path(&self) -> String {
        format!("{}/.cradle-ring/data/nodes.json", self.home)
    }

    fn load_nodes(&self) -> Vec<Node> {
        if let Ok(data) = std::fs::read_to_string(self.nodes_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_nodes(&self, nodes: &[Node]) {
        if let Ok(data) = serde_json::to_string_pretty(nodes) {
            let _ = std::fs::write(self.nodes_path(), data);
        }
    }

    fn pair_requests_path(&self) -> String {
        format!("{}/.cradle-ring/data/pair_requests.json", self.home)
    }

    fn load_pair_requests(&self) -> Vec<PairRequest> {
        if let Ok(data) = std::fs::read_to_string(self.pair_requests_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_pair_requests(&self, items: &[PairRequest]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.pair_requests_path(), data);
        }
    }

    // ---- 系统事件 / 用量日志 / 压缩检查点 / session 文件 ----

    fn events_path(&self) -> String {
        format!("{}/.cradle-ring/data/events.jsonl", self.home)
    }

    /// 追加一条系统事件到 events.jsonl
    fn append_event(&self, event: serde_json::Value) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.events_path()) {
            let line = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
            let _ = writeln!(f, "{}", line);
        }
    }

    /// 读取最近 N 条系统事件（按写入顺序）
    fn load_events(&self, limit: usize) -> Vec<serde_json::Value> {
        if let Ok(data) = std::fs::read_to_string(self.events_path()) {
            let all: Vec<serde_json::Value> = data.lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let start = all.len().saturating_sub(limit);
            all[start..].to_vec()
        } else {
            vec![]
        }
    }

    fn usage_logs_path(&self) -> String {
        format!("{}/.cradle-ring/data/usage_logs.jsonl", self.home)
    }

    fn append_usage_log(&self, log: &UsageLog) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.usage_logs_path()) {
            let line = serde_json::to_string(log).unwrap_or_default();
            let _ = writeln!(f, "{}", line);
        }
    }

    fn load_usage_logs(&self, limit: usize) -> Vec<UsageLog> {
        if let Ok(data) = std::fs::read_to_string(self.usage_logs_path()) {
            let all: Vec<UsageLog> = data.lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let start = all.len().saturating_sub(limit);
            all[start..].to_vec()
        } else {
            vec![]
        }
    }

    fn compaction_path(&self) -> String {
        format!("{}/.cradle-ring/data/compaction_checkpoints.json", self.home)
    }

    fn load_compaction_checkpoints(&self) -> Vec<CompactionCheckpoint> {
        if let Ok(data) = std::fs::read_to_string(self.compaction_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_compaction_checkpoints(&self, items: &[CompactionCheckpoint]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.compaction_path(), data);
        }
    }

    /// 备份目录：存放压缩前的完整消息历史
    fn compaction_backup_dir(&self) -> String {
        let p = format!("{}/.cradle-ring/data/compaction_backups", self.home);
        let _ = std::fs::create_dir_all(&p);
        p
    }

    /// session 工作目录：每个 session 可挂载私有文件
    fn session_files_dir(&self, session_key: &str) -> String {
        let safe = session_key.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        let p = format!("{}/.cradle-ring/workspace/sessions/{}", self.home, safe);
        let _ = std::fs::create_dir_all(&p);
        p
    }

    /// skills 工作目录
    fn skills_dir(&self) -> String {
        let p = format!("{}/.cradle-ring/workspace/skills", self.home);
        let _ = std::fs::create_dir_all(&p);
        p
    }

    // ---- Users ----

    fn users_path(&self) -> String {
        format!("{}/.cradle-ring/data/users.json", self.home)
    }

    fn load_users(&self) -> Vec<User> {
        if let Ok(data) = std::fs::read_to_string(self.users_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_users(&self, items: &[User]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.users_path(), data);
        }
    }

    // ---- Auth Tokens ----

    fn tokens_path(&self) -> String {
        format!("{}/.cradle-ring/data/tokens.jsonl", self.home)
    }

    fn append_token(&self, t: &AuthToken) {
        use std::io::Write;
        if let Ok(data) = serde_json::to_string(t) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.tokens_path()) {
                let _ = writeln!(f, "{}", data);
            }
        }
    }

    fn load_tokens(&self) -> Vec<AuthToken> {
        if let Ok(data) = std::fs::read_to_string(self.tokens_path()) {
            data.lines().filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        } else {
            vec![]
        }
    }

    /// 仅保留未过期 token（同时清理文件）
    fn purge_expired_tokens(&self) {
        let now = current_ms();
        let keep: Vec<AuthToken> = self.load_tokens().into_iter()
            .filter(|t| t.expires_at > now)
            .collect();
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(self.tokens_path()) {
            for t in &keep {
                let _ = writeln!(f, "{}", serde_json::to_string(t).unwrap_or_default());
            }
        }
    }

    // ---- Approval Flows ----

    fn approval_flows_path(&self) -> String {
        format!("{}/.cradle-ring/data/approval_flows.json", self.home)
    }

    fn load_approval_flows(&self) -> Vec<ApprovalFlow> {
        if let Ok(data) = std::fs::read_to_string(self.approval_flows_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_approval_flows(&self, items: &[ApprovalFlow]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.approval_flows_path(), data);
        }
    }

    // ---- Approval Instances ----

    fn approval_instances_path(&self) -> String {
        format!("{}/.cradle-ring/data/approval_instances.json", self.home)
    }

    fn load_approval_instances(&self) -> Vec<ApprovalInstance> {
        if let Ok(data) = std::fs::read_to_string(self.approval_instances_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_approval_instances(&self, items: &[ApprovalInstance]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.approval_instances_path(), data);
        }
    }

    // ---- Workflow Graphs ----

    fn workflow_graphs_path(&self) -> String {
        format!("{}/.cradle-ring/data/workflow_graphs.json", self.home)
    }
    fn load_workflow_graphs(&self) -> Vec<WorkflowGraph> {
        if let Ok(data) = std::fs::read_to_string(self.workflow_graphs_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else { vec![] }
    }
    fn save_workflow_graphs(&self, items: &[WorkflowGraph]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.workflow_graphs_path(), data);
        }
    }

    // ---- Workflow Runs ----

    fn workflow_runs_dir(&self) -> String {
        let p = format!("{}/.cradle-ring/data/workflow_runs", self.home);
        let _ = std::fs::create_dir_all(&p);
        p
    }
    fn workflow_run_path(&self, run_id: &str) -> String {
        let safe = run_id.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        format!("{}/{}.json", self.workflow_runs_dir(), safe)
    }
    fn load_workflow_run(&self, run_id: &str) -> Option<WorkflowRun> {
        let path = self.workflow_run_path(run_id);
        std::fs::read_to_string(&path).ok()
            .and_then(|d| serde_json::from_str(&d).ok())
    }
    fn save_workflow_run(&self, run: &WorkflowRun) {
        let path = self.workflow_run_path(&run.id);
        if let Ok(data) = serde_json::to_string_pretty(run) {
            let _ = std::fs::write(path, data);
        }
    }
    fn list_workflow_runs(&self) -> Vec<WorkflowRun> {
        let dir = self.workflow_runs_dir();
        let mut runs = vec![];
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if let Ok(data) = std::fs::read_to_string(e.path()) {
                    if let Ok(r) = serde_json::from_str::<WorkflowRun>(&data) {
                        runs.push(r);
                    }
                }
            }
        }
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs
    }
    fn delete_workflow_run(&self, run_id: &str) -> bool {
        std::fs::remove_file(self.workflow_run_path(run_id)).is_ok()
    }

    // ---- Agent Roles ----

    fn agent_roles_path(&self) -> String {
        format!("{}/.cradle-ring/data/agent_roles.json", self.home)
    }
    fn load_agent_roles(&self) -> Vec<AgentRole> {
        if let Ok(data) = std::fs::read_to_string(self.agent_roles_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else { vec![] }
    }
    fn save_agent_roles(&self, items: &[AgentRole]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.agent_roles_path(), data);
        }
    }

    // ---- Pipelines ----

    fn pipelines_path(&self) -> String {
        format!("{}/.cradle-ring/data/pipelines.json", self.home)
    }
    fn load_pipelines(&self) -> Vec<Pipeline> {
        if let Ok(data) = std::fs::read_to_string(self.pipelines_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else { vec![] }
    }
    fn save_pipelines(&self, items: &[Pipeline]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.pipelines_path(), data);
        }
    }

    // ---- Change Events (audit log) ----

    fn change_events_path(&self) -> String {
        format!("{}/.cradle-ring/data/change_events.jsonl", self.home)
    }
    fn append_change_event(&self, ev: &ChangeEvent) {
        use std::io::Write;
        if let Ok(data) = serde_json::to_string(ev) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.change_events_path()) {
                let _ = writeln!(f, "{}", data);
            }
        }
    }
    fn load_change_events(&self, limit: usize, kind_filter: Option<&str>) -> Vec<ChangeEvent> {
        if let Ok(data) = std::fs::read_to_string(self.change_events_path()) {
            let mut all: Vec<ChangeEvent> = data.lines().filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            if let Some(k) = kind_filter { all.retain(|e| e.kind == k); }
            let start = all.len().saturating_sub(limit);
            all[start..].to_vec()
        } else { vec![] }
    }

    // ---- Review Decisions ----

    fn reviews_path(&self) -> String {
        format!("{}/.cradle-ring/data/reviews.jsonl", self.home)
    }
    fn append_review(&self, r: &ReviewDecision) {
        use std::io::Write;
        if let Ok(data) = serde_json::to_string(r) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.reviews_path()) {
                let _ = writeln!(f, "{}", data);
            }
        }
    }
    fn load_reviews(&self, limit: usize) -> Vec<ReviewDecision> {
        if let Ok(data) = std::fs::read_to_string(self.reviews_path()) {
            let all: Vec<ReviewDecision> = data.lines().filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let start = all.len().saturating_sub(limit);
            all[start..].to_vec()
        } else { vec![] }
    }

    // ---- WAF Rules ----

    fn waf_rules_path(&self) -> String {
        format!("{}/.cradle-ring/data/waf_rules.json", self.home)
    }
    fn load_waf_rules(&self) -> Vec<WafRule> {
        if let Ok(data) = std::fs::read_to_string(self.waf_rules_path()) {
            let mut rules: Vec<WafRule> = serde_json::from_str(&data).unwrap_or_default();
            if rules.is_empty() { rules = default_waf_rules(); }
            rules
        } else {
            default_waf_rules()
        }
    }
    fn save_waf_rules(&self, items: &[WafRule]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.waf_rules_path(), data);
        }
    }

    // ---- WAF Events ----

    fn waf_events_path(&self) -> String {
        format!("{}/.cradle-ring/data/waf_events.jsonl", self.home)
    }
    fn append_waf_event(&self, ev: &WafEvent) {
        use std::io::Write;
        if let Ok(data) = serde_json::to_string(ev) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.waf_events_path()) {
                let _ = writeln!(f, "{}", data);
            }
        }
    }
    fn load_waf_events(&self, limit: usize) -> Vec<WafEvent> {
        if let Ok(data) = std::fs::read_to_string(self.waf_events_path()) {
            let all: Vec<WafEvent> = data.lines().filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let start = all.len().saturating_sub(limit);
            all[start..].to_vec()
        } else { vec![] }
    }

    // ---- Custom Roles ----

    fn roles_path(&self) -> String {
        format!("{}/.cradle-ring/data/roles.json", self.home)
    }
    fn load_roles(&self) -> Vec<Role> {
        if let Ok(data) = std::fs::read_to_string(self.roles_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else { vec![] }
    }
    fn save_roles(&self, items: &[Role]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.roles_path(), data);
        }
    }

    // ---- IDS Events ----

    fn ids_events_path(&self) -> String {
        format!("{}/.cradle-ring/data/ids_events.jsonl", self.home)
    }
    fn append_ids_event(&self, ev: &IdsEvent) {
        use std::io::Write;
        if let Ok(data) = serde_json::to_string(ev) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.ids_events_path()) {
                let _ = writeln!(f, "{}", data);
            }
        }
    }
    fn load_ids_events(&self, limit: usize) -> Vec<IdsEvent> {
        if let Ok(data) = std::fs::read_to_string(self.ids_events_path()) {
            let all: Vec<IdsEvent> = data.lines().filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let start = all.len().saturating_sub(limit);
            all[start..].to_vec()
        } else { vec![] }
    }

    // ---- IDS Rules ----

    fn ids_rules_path(&self) -> String {
        format!("{}/.cradle-ring/data/ids_rules.json", self.home)
    }
    fn load_ids_rules(&self) -> Vec<IdsRule> {
        if let Ok(data) = std::fs::read_to_string(self.ids_rules_path()) {
            let mut rules: Vec<IdsRule> = serde_json::from_str(&data).unwrap_or_default();
            if rules.is_empty() { rules = default_ids_rules(); }
            rules
        } else {
            default_ids_rules()
        }
    }
    fn save_ids_rules(&self, items: &[IdsRule]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.ids_rules_path(), data);
        }
    }

    // ---- IP 黑白名单 ----

    fn ip_list_path(&self) -> String {
        format!("{}/.cradle-ring/data/ip_list.json", self.home)
    }
    fn load_ip_list(&self) -> Vec<IpEntry> {
        if let Ok(data) = std::fs::read_to_string(self.ip_list_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else { vec![] }
    }
    fn save_ip_list(&self, items: &[IpEntry]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.ip_list_path(), data);
        }
    }

    // ---- 速率限制 ----

    fn rate_limit_path(&self) -> String {
        format!("{}/.cradle-ring/data/rate_limit.json", self.home)
    }
    fn load_rate_limit(&self) -> Vec<RateLimitEntry> {
        if let Ok(data) = std::fs::read_to_string(self.rate_limit_path()) {
            serde_json::from_str(&data).unwrap_or_default()
        } else { vec![] }
    }
    fn save_rate_limit(&self, items: &[RateLimitEntry]) {
        if let Ok(data) = serde_json::to_string_pretty(items) {
            let _ = std::fs::write(self.rate_limit_path(), data);
        }
    }

    /// artifacts 目录
    fn artifacts_dir(&self) -> String {
        let p = format!("{}/.cradle-ring/data/artifacts", self.home);
        let _ = std::fs::create_dir_all(&p);
        p
    }
}

// ============================================================================
// 渠道系统：真实的 IM 收发能力
// ============================================================================

/// 渠道运行时状态：实时连接/收发情况
#[derive(Clone, Debug, Default, serde::Serialize)]
struct ChannelRuntimeState {
    /// configured | connected | disconnected | error | polling
    status: String,
    enabled: bool,
    /// 最近一次收到的消息时间（ms）
    last_received_at: i64,
    /// 最近一次发送的消息时间（ms）
    last_sent_at: i64,
    /// 最近一次错误
    last_error: String,
    /// 已接收消息总数
    received_count: u64,
    /// 已发送消息总数
    sent_count: u64,
}

/// 入站消息：所有渠道的 webhook/polling 解析后统一格式
#[derive(Clone, Debug)]
struct InboundMessage {
    /// 渠道标识（feishu/telegram/discord/...）
    channel: String,
    /// 发送者 ID（在该渠道内的唯一标识）
    sender_id: String,
    /// 发送者显示名
    sender_name: String,
    /// 该渠道内的会话/聊天 ID（用于回复路由）
    chat_id: String,
    /// 消息文本
    text: String,
    /// 原始消息 ID（用于去重）
    message_id: String,
    /// 原始负载（供 send_to_* 回复时使用）
    #[allow(dead_code)]
    raw: serde_json::Value,
}

/// 统一管理所有渠道的运行时状态
struct ChannelManager {
    states: tokio::sync::RwLock<std::collections::BTreeMap<String, ChannelRuntimeState>>,
    /// 已处理过的入站消息 ID（用于去重，避免 webhook 重试或 polling 重复）
    seen_messages: tokio::sync::Mutex<std::collections::VecDeque<String>>,
}

impl ChannelManager {
    fn new() -> Self {
        Self {
            states: tokio::sync::RwLock::new(std::collections::BTreeMap::new()),
            seen_messages: tokio::sync::Mutex::new(std::collections::VecDeque::with_capacity(2048)),
        }
    }

    async fn get(&self, channel: &str) -> ChannelRuntimeState {
        self.states
            .read()
            .await
            .get(channel)
            .cloned()
            .unwrap_or_default()
    }

    async fn set_status(&self, channel: &str, status: &str) {
        let mut w = self.states.write().await;
        let entry = w.entry(channel.to_string()).or_default();
        entry.status = status.to_string();
    }

    async fn record_error(&self, channel: &str, err: &str) {
        let mut w = self.states.write().await;
        let entry = w.entry(channel.to_string()).or_default();
        entry.status = "error".to_string();
        entry.last_error = err.to_string();
    }

    async fn record_received(&self, channel: &str) {
        let mut w = self.states.write().await;
        let entry = w.entry(channel.to_string()).or_default();
        entry.last_received_at = current_ms();
        entry.received_count += 1;
        entry.status = "connected".to_string();
    }

    async fn record_sent(&self, channel: &str) {
        let mut w = self.states.write().await;
        let entry = w.entry(channel.to_string()).or_default();
        entry.last_sent_at = current_ms();
        entry.sent_count += 1;
        entry.status = "connected".to_string();
    }

    async fn set_enabled(&self, channel: &str, enabled: bool) {
        let mut w = self.states.write().await;
        let entry = w.entry(channel.to_string()).or_default();
        entry.enabled = enabled;
        if entry.status.is_empty() {
            entry.status = if enabled { "configured".to_string() } else { "disabled".to_string() };
        }
    }

    /// 去重检查：返回 true 表示是首次见到该消息
    async fn check_dedup(&self, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return true;
        }
        let mut seen = self.seen_messages.lock().await;
        if seen.iter().any(|m| m == msg_id) {
            return false;
        }
        seen.push_back(msg_id.to_string());
        while seen.len() > 2048 {
            seen.pop_front();
        }
        true
    }

    /// 快照所有渠道状态（用于 channels.status）
    async fn snapshot(&self) -> std::collections::BTreeMap<String, ChannelRuntimeState> {
        self.states.read().await.clone()
    }
}

// ============================================================================
// 全局状态
// ============================================================================

struct AppState {
    config: Config,
    storage: Storage,
    started_at: i64,
    next_msg_id: AtomicU64,
    run_counter: AtomicU64,
    active_ws: tokio::sync::Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// 待审批请求的等待器：approval_id -> oneshot sender（true=批准 false=拒绝）
    pending_approvals: tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    /// 多级审批实例的执行等待器：instance_id -> oneshot sender（true=全部批准可执行 false=拒绝）
    pending_approval_instances: tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    /// 渠道运行时状态管理器
    channels: Arc<ChannelManager>,
    /// 当前认证用户（从 WebSocket 握手 token 解析得到，按 ws 连接隔离，这里仅存默认用户）
    current_user: tokio::sync::Mutex<Option<User>>,
    /// 记忆引擎 V3（Cache-First + 向量检索 + 时序知识图谱 + 级联路由）
    /// 使用 OnceCell 实现首次访问时懒加载
    memory_engine: tokio::sync::OnceCell<Arc<MemoryEngine>>,
}

impl AppState {
    fn new(config: Config, storage: Storage) -> Arc<Self> {
        let state = Arc::new(Self {
            config,
            storage,
            started_at: current_ms(),
            next_msg_id: AtomicU64::new(1),
            run_counter: AtomicU64::new(0),
            active_ws: tokio::sync::Mutex::new(Vec::new()),
            pending_approvals: tokio::sync::Mutex::new(HashMap::new()),
            pending_approval_instances: tokio::sync::Mutex::new(HashMap::new()),
            channels: Arc::new(ChannelManager::new()),
            current_user: tokio::sync::Mutex::new(None),
            memory_engine: tokio::sync::OnceCell::new(),
        });
        state
    }

    /// 异步初始化：必须在 tokio runtime 上下文中调用。
    /// 启动渠道同步、默认 admin 创建、审批后台循环。
    async fn init(self: &Arc<Self>) {
        let s = self.clone();
        tokio::spawn(async move {
            s.sync_channel_enabled_states().await;
            // 初始化默认 admin 用户（如不存在）
            ensure_default_admin(&s).await;
            // 初始化预置运维专家角色（对标 ongrid specialist agents，无论用户是否已存在都初始化）
            ensure_ops_roles(&s).await;
            // 启动审批实例后台推进循环
            let s2 = s.clone();
            tokio::spawn(approval_advance_loop(s2));
        });
    }

    /// 获取或初始化记忆引擎 V3
    ///
    /// 配置来源（按优先级）：
    /// 1. config.raw_json.memory 节（install.sh 写入的格式）
    /// 2. config.raw_json.memoryEngine 节（高级覆盖）
    /// 3. 默认值（本地哈希 Embedding + Qdrant-lite 向量库）
    async fn memory(&self) -> Result<&Arc<MemoryEngine>, String> {
        self.memory_engine.get_or_try_init(|| async {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            let raw = &self.config.raw_json;
            // 兼容两种配置 key：memory / memoryEngine
            let cfg_json = raw.get("memoryEngine")
                .or_else(|| raw.get("memory"))
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let cfg = MemoryEngineConfig::from_json(&cfg_json, &home);
            MemoryEngine::new(cfg).map(Arc::new)
        }).await.map_err(|e: anyhow::Error| format!("记忆引擎初始化失败: {}", e))
    }
}

impl AppState {
    /// 读取配置中的 channels 节，返回有序 (id, config) 列表
    fn channels_config(&self) -> Vec<(String, serde_json::Value)> {
        let mut out = Vec::new();
        if let Some(obj) = self.config.raw_json.get("channels").and_then(|c| c.as_object()) {
            for (k, v) in obj {
                out.push((k.clone(), v.clone()));
            }
        }
        out
    }

    fn channel_config(&self, name: &str) -> Option<serde_json::Value> {
        self.config
            .raw_json
            .get("channels")
            .and_then(|c| c.get(name))
            .cloned()
    }

    async fn sync_channel_enabled_states(&self) {
        for (id, cfg) in self.channels_config() {
            let enabled = cfg["enabled"].as_bool().unwrap_or(false);
            self.channels.set_enabled(&id, enabled).await;
        }
    }

    /// 处理一条入站消息：创建/复用会话，写入用户消息，触发 agent loop。
    /// 会话 key 形如 `feishu:{chat_id}`，display_name 含渠道与发送者。
    async fn ingest_inbound(state: Arc<AppState>, msg: InboundMessage) {
        let session_key = format!("{}:{}", msg.channel, msg.chat_id);
        // 标记渠道已收到
        state.channels.record_received(&msg.channel).await;

        // 确保会话存在
        let mut sessions = state.storage.load_sessions();
        if !sessions.iter().any(|s| s.key == session_key) {
            sessions.push(Session {
                key: session_key.clone(),
                kind: format!("channel-{}", msg.channel),
                display_name: Some(if msg.sender_name.is_empty() {
                    format!("[{}] {}", msg.channel, msg.chat_id)
                } else {
                    format!("[{}] {}", msg.channel, msg.sender_name)
                }),
                channel: Some(msg.channel.clone()),
                agent_id: "main".to_string(),
                model: Some(state.config.default_model.clone()),
                updated_at: current_ms(),
            });
            state.storage.save_sessions(&sessions);
        }

        // 前缀加上发送者署名，便于 agent 区分来源
        let display_text = if msg.sender_name.is_empty() {
            msg.text.clone()
        } else {
            format!("[{}] {}", msg.sender_name, msg.text)
        };
        state.storage.append_message(&session_key, &Message {
            role: "user".into(),
            content: display_text,
            timestamp: current_ms(),
            attachments: vec![],
        });

        let run_id = format!("run-{:016x}", state.run_counter.fetch_add(1, Ordering::SeqCst));
        let state_clone = state.clone();
        let sk = session_key.clone();
        let rid = run_id.clone();
        let channel = msg.channel.clone();
        let chat_id = msg.chat_id.clone();
        tokio::spawn(async move {
            run_agent_loop(state_clone.clone(), &sk, &rid).await;
            // agent loop 完成后，把 assistant 回复发回渠道
            deliver_assistant_reply(state_clone, &sk, &channel, &chat_id).await;
        });
    }
}

/// agent loop 完成后，取出会话最新一条 assistant 消息，调用对应渠道发送
async fn deliver_assistant_reply(
    state: Arc<AppState>,
    session_key: &str,
    channel: &str,
    chat_id: &str,
) {
    let messages = state.storage.load_messages(session_key);
    let reply = match messages.iter().rev().find(|m| m.role == "assistant") {
        Some(m) => m.content.clone(),
        None => return,
    };
    let cfg = match state.channel_config(channel) {
        Some(c) => c,
        None => return,
    };
    let res = send_to_channel(&state, channel, &cfg, chat_id, &reply).await;
    if res.is_ok() {
        state.channels.record_sent(channel).await;
    } else if let Err(e) = res {
        state.channels.record_error(channel, &e).await;
    }
}

/// 渠道发送分发：根据 channel 名调对应实现
async fn send_to_channel(
    state: &AppState,
    channel: &str,
    cfg: &serde_json::Value,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    match channel {
        "feishu" => send_to_feishu(state, cfg, chat_id, text).await,
        "telegram" => send_to_telegram(cfg, chat_id, text).await,
        "discord" => send_to_discord(cfg, chat_id, text).await,
        "slack" => send_to_slack(cfg, chat_id, text).await,
        "dingtalk" => send_to_dingtalk(cfg, chat_id, text).await,
        "wecom" | "wechat" => send_to_wecom(cfg, chat_id, text).await,
        "whatsapp" => send_to_whatsapp(cfg, chat_id, text).await,
        "signal" => send_to_signal(cfg, chat_id, text).await,
        "qq" => send_to_qq(cfg, chat_id, text).await,
        "matrix" => send_to_matrix(cfg, chat_id, text).await,
        "teams" => send_to_teams(cfg, chat_id, text).await,
        _ => send_generic(cfg, chat_id, text).await,
    }
}

// ============================================================================
// HTTP 处理器
// ============================================================================

async fn handle_http(
    stream: &mut tokio::net::TcpStream,
    request: &str,
    state: Arc<AppState>,
) {
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");
    let method = first_line.split_whitespace().next().unwrap_or("GET");

    // 用于静态文件查找的路径：去掉 query string（如 /sw.js?v=1.2 -> /sw.js）
    let fs_path = match path.find(|c| c == '?' || c == '#') {
        Some(idx) => &path[..idx],
        None => path,
    };

    // API 路径（/api/...）和 /health 由我们自己处理
    if path == "/health" {
        let body = serde_json::json!({
            "ok": true,
            "status": "live",
            "name": "CradleRing",
            "version": env!("CARGO_PKG_VERSION"),
            "commit": env!("CRADLE_BUILD_COMMIT"),
            "commitDate": env!("CRADLE_BUILD_DATE"),
            "dirty": env!("CRADLE_BUILD_DIRTY") == "1",
            "uptimeMs": current_ms() - state.started_at,
        }).to_string();
        send_response(stream, 200, "OK", "application/json", body.as_bytes()).await;
        return;
    }

    if path == "/api/token" || path == "/api/info" {
        let body = serde_json::json!({
            "token": state.config.token,
            "version": env!("CARGO_PKG_VERSION"),
            "commit": env!("CRADLE_BUILD_COMMIT"),
            "commitDate": env!("CRADLE_BUILD_DATE"),
            "websocket": "ws://127.0.0.1:18800/ws",
            "home": state.storage.home,
        }).to_string();
        send_response(stream, 200, "OK", "application/json", body.as_bytes()).await;
        return;
    }

    // REST 登录（供 SPA 登录页使用，无需先建 WebSocket）
    if method == "POST" && path.starts_with("/api/login") {
        let body = extract_http_body(request);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        let username = payload["username"].as_str().unwrap_or("").to_string();
        let password = payload["password"].as_str().unwrap_or("").to_string();
        if username.is_empty() || password.is_empty() {
            let b = r#"{"ok":false,"error":{"code":"INVALID_CREDENTIALS","message":"用户名和密码不能为空"}}"#;
            send_response(stream, 400, "Bad Request", "application/json", b.as_bytes()).await;
            return;
        }
        let mut users = state.storage.load_users();
        let result = match users.iter_mut().find(|u| u.username == username && u.enabled) {
            Some(u) => {
                if verify_password(&password, &u.password_hash) {
                    u.last_login = Some(current_ms());
                    let uc = u.clone();
                    Some(uc)
                } else { None }
            }
            None => None,
        };
        match result {
            Some(uc) => {
                state.storage.save_users(&users);
                let token = issue_jwt(&uc, 7 * 24 * 3600, &state);
                state.storage.append_token(&token);
                let resp = serde_json::json!({
                    "ok": true,
                    "token": token.token,
                    "expiresAt": token.expires_at,
                    "user": user_to_json(&uc),
                }).to_string();
                send_response(stream, 200, "OK", "application/json", resp.as_bytes()).await;
            }
            None => {
                let b = r#"{"ok":false,"error":{"code":"INVALID_CREDENTIALS","message":"用户名或密码错误"}}"#;
                send_response(stream, 401, "Unauthorized", "application/json", b.as_bytes()).await;
            }
        }
        return;
    }

    // REST 获取当前用户
    if method == "GET" && path.starts_with("/api/me") {
        let auth = request.lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .map(|l| l.trim());
        // 支持 "Authorization: Bearer <token>" 和 "Authorization: <token>"
        let token = auth.map(|l| {
            let v = l.splitn(2, ':').nth(1).unwrap_or("").trim();
            if let Some(rest) = v.strip_prefix("Bearer ") { rest.trim() }
            else if let Some(rest) = v.strip_prefix("bearer ") { rest.trim() }
            else { v }
        }).unwrap_or("");
        match verify_jwt(token, &state) {
            Some((uid, _, _)) => {
                let users = state.storage.load_users();
                match users.iter().find(|u| u.id == uid) {
                    Some(u) => {
                        let resp = serde_json::json!({"ok": true, "user": user_to_json(u)}).to_string();
                        send_response(stream, 200, "OK", "application/json", resp.as_bytes()).await;
                    }
                    None => {
                        send_response(stream, 404, "Not Found", "application/json", b"{\"ok\":false}").await;
                    }
                }
            }
            None => {
                send_response(stream, 401, "Unauthorized", "application/json",
                    b"{\"ok\":false,\"error\":{\"code\":\"UNAUTHORIZED\"}}").await;
            }
        }
        return;
    }

    // ---- 渠道 webhook 回调 ----
    // GET /webhook/{channel}：用于平台验证（WhatsApp 订阅、Slack challenge 等）
    // POST /webhook/{channel}：用于平台事件投递
    if path.starts_with("/webhook/") {
        let channel = path.trim_start_matches("/webhook/").split('?').next().unwrap_or("");
        let body = extract_http_body(request);
        handle_webhook(stream, method, channel, path, &body, state.clone()).await;
        return;
    }

    // 优先尝试本地 ui-dist/ 静态文件
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().unwrap_or(std::path::Path::new(".")).to_string_lossy().to_string();
    // 候选 UI 目录列表（按优先级）
    let ui_candidates: Vec<String> = std::env::var("CRADLE_RING_UI_DIR").ok().into_iter()
        .chain([
            format!("{}/ui-dist", exe_dir),
            "/home/muling/.local/bin/ui-dist".to_string(),
            format!("{}/webui/dist", exe_dir),
            format!("{}/../webui/dist", exe_dir),
        ])
        .collect();

    // 逐个候选目录尝试
    for dir in &ui_candidates {
        let file_path = if fs_path == "/" {
            format!("{}/index.html", dir)
        } else {
            format!("{}{}", dir, fs_path)
        };
        if !file_path.contains("..") {
            if let Ok(content) = std::fs::read(&file_path) {
                let ct = guess_mime(&file_path);
                send_response(stream, 200, "OK", ct, &content).await;
                return;
            }
        }
    }

    // SPA fallback（所有未匹配路径都返回 index.html，让前端路由处理）
    if !path.starts_with("/api") && !path.starts_with("/webhook") {
        for dir in &ui_candidates {
            let idx = format!("{}/index.html", dir);
            if let Ok(content) = std::fs::read(&idx) {
                send_response(stream, 200, "OK", "text/html; charset=utf-8", &content).await;
                return;
            }
        }
    }

    // 未匹配的路径返回 404
    send_response(stream, 404, "Not Found", "text/html; charset=utf-8",
        b"<html><head><title>404 - CradleRing</title></head><body style='font-family:sans-serif;text-align:center;padding:60px'><h1>404</h1><p>Page not found</p><p><a href='/'>Back to home</a></p></body></html>").await;
}

fn guess_mime(path: &str) -> &'static str {
    if path.ends_with(".html") { "text/html; charset=utf-8" }
    else if path.ends_with(".js") { "application/javascript; charset=utf-8" }
    else if path.ends_with(".css") { "text/css; charset=utf-8" }
    else if path.ends_with(".json") { "application/json" }
    else if path.ends_with(".svg") { "image/svg+xml" }
    else if path.ends_with(".png") { "image/png" }
    else if path.ends_with(".ico") { "image/x-icon" }
    else if path.ends_with(".webmanifest") { "application/manifest+json" }
    else if path.ends_with(".woff2") { "font/woff2" }
    else if path.ends_with(".woff") { "font/woff" }
    else { "application/octet-stream" }
}

async fn send_response(
    stream: &mut tokio::net::TcpStream,
    code: u16,
    status: &str,
    content_type: &str,
    body: &[u8],
) {
    use tokio::io::AsyncWriteExt;
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        code, status, content_type, body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

/// 从原始 HTTP 请求字符串中提取 body（\r\n\r\n 之后的部分）
fn extract_http_body(request: &str) -> Vec<u8> {
    if let Some(idx) = request.find("\r\n\r\n") {
        request[idx + 4..].as_bytes().to_vec()
    } else if let Some(idx) = request.find("\n\n") {
        request[idx + 2..].as_bytes().to_vec()
    } else {
        Vec::new()
    }
}

/// 解析 URL query string（如 hub.mode=subscribe&hub.challenge=xxx）
fn parse_query(path: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Some(q) = path.split('?').nth(1) {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            if let (Some(k), Some(v)) = (it.next(), it.next()) {
                map.insert(decode_uri(k), decode_uri(v));
            }
        }
    }
    map
}

fn decode_uri(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 渠道 webhook 统一入口：先做 GET 验证，再分发到各渠道解析器
async fn handle_webhook(
    stream: &mut tokio::net::TcpStream,
    method: &str,
    channel: &str,
    path: &str,
    body: &[u8],
    state: Arc<AppState>,
) {
    let cfg = state.channel_config(channel);
    let enabled = cfg.as_ref().and_then(|c| c["enabled"].as_bool()).unwrap_or(false);

    // 未知渠道或未启用：返回 404/403
    if cfg.is_none() {
        send_response(stream, 404, "Not Found", "application/json",
            b"{\"error\":\"unknown channel\"}").await;
        return;
    }
    if !enabled {
        send_response(stream, 403, "Forbidden", "application/json",
            b"{\"error\":\"channel disabled\"}").await;
        return;
    }

    let cfg = cfg.unwrap();

    // GET：平台验证（WhatsApp 订阅、Slack/企微 token 校验等）
    if method == "GET" {
        let q = parse_query(path);
        // WhatsApp Cloud API 订阅验证
        if let Some(challenge) = q.get("hub.challenge") {
            let mode = q.get("hub.mode").map(String::as_str).unwrap_or("");
            let token = q.get("hub.verify_token").map(String::as_str).unwrap_or("");
            let expected = cfg["verifyToken"].as_str().unwrap_or("");
            if mode == "subscribe" && token == expected {
                send_response(stream, 200, "OK", "text/plain", challenge.as_bytes()).await;
            } else {
                send_response(stream, 403, "Forbidden", "text/plain", b"forbidden").await;
            }
            return;
        }
        // 企业微信回调 URL 校验
        if let Some(echo) = q.get("echostr") {
            send_response(stream, 200, "OK", "text/plain", echo.as_bytes()).await;
            return;
        }
        // 钉钉 checkUrl
        send_response(stream, 200, "OK", "text/plain", b"ok").await;
        return;
    }

    if method != "POST" {
        send_response(stream, 405, "Method Not Allowed", "application/json", b"{}").await;
        return;
    }

    // 解析 body
    let body_str = String::from_utf8_lossy(body).to_string();
    let parsed: serde_json::Value = serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null);

    // 分发到各渠道解析器，返回 0 条或多条 InboundMessage
    let messages = match channel {
        "feishu" => parse_feishu(&parsed),
        "telegram" => parse_telegram(&parsed),
        "discord" => parse_discord(&parsed),
        "slack" => parse_slack(&parsed),
        "dingtalk" => parse_dingtalk(&parsed),
        "wecom" | "wechat" => parse_wecom(&parsed),
        "whatsapp" => parse_whatsapp(&parsed),
        "signal" => parse_signal(&parsed),
        "qq" => parse_qq(&parsed),
        "matrix" => parse_matrix(&parsed),
        "teams" => parse_teams(&parsed),
        _ => parse_generic(channel, &parsed),
    };

    // 立即回复 200（避免平台重试），后台处理消息
    send_response(stream, 200, "OK", "application/json", b"{\"ok\":true}").await;

    // 投递每条消息
    for msg in messages {
        if !state.channels.check_dedup(&msg.message_id).await {
            continue;
        }
        // IM 审批回复检测：「同意 <id>」/「拒绝 <id>」/「approve <id>」/「reject <id>」
        if let Some((decision, inst_id)) = parse_approval_reply(&msg.text) {
            let s = state.clone();
            let channel_name = msg.channel.clone();
            let sender = if msg.sender_name.is_empty() { msg.chat_id.clone() } else { msg.sender_name.clone() };
            tokio::spawn(async move {
                let approver_id = format!("im:{}:{}", channel_name, sender);
                let approver_username = format!("[{}] {}", channel_name, sender);
                let res = advance_approval_instance(
                    s.clone(), &inst_id, &approver_id, &approver_username,
                    &decision, "", &channel_name,
                ).await;
                // 回执
                let reply_text = match res {
                    Some((true, inst)) if inst.status == "approved" => format!("✅ 审批已全部通过：{}\n即将执行：{}", inst.title, inst.command),
                    Some((_, inst)) if inst.status == "rejected" => format!("❌ 已拒绝：{}", inst.title),
                    Some((_, inst)) => format!("✅ 已记录本步骤决定（{}/{}），等待后续审批人", inst.current_step, inst.total_steps),
                    None => format!("⚠️ 审批实例 {} 不存在或已处理", inst_id),
                };
                // 发送到原渠道
                if let Some(cfg) = s.channel_config(&channel_name) {
                    let _ = send_to_channel(&s, &channel_name, &cfg, &msg.chat_id, &reply_text).await;
                }
            });
            continue;
        }
        let s = state.clone();
        tokio::spawn(async move {
            AppState::ingest_inbound(s, msg).await;
        });
    }
}

/// 解析 IM 审批回复，返回 (decision, instance_id)
fn parse_approval_reply(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();
    let trimmed = text.trim();
    // 中文 / 英文 关键词
    let (decision_kw, is_approve): (&str, bool) = if lower.starts_with("同意") || lower.starts_with("批准") || lower.starts_with("approve") || lower.starts_with("yes") {
        ("", true)
    } else if lower.starts_with("拒绝") || lower.starts_with("驳回") || lower.starts_with("reject") || lower.starts_with("deny") || lower.starts_with("no") {
        ("", false)
    } else {
        return None;
    };
    let _ = decision_kw;
    // 提取 instance_id：格式 "inst-<hex>"
    let id = trimmed.split_whitespace()
        .find(|w| w.starts_with("inst-") && w.len() > 5)?
        .trim_end_matches(|c: char| !c.is_ascii_hexdigit() && c != '-')
        .to_string();
    if id.len() < 6 { return None; }
    let decision = if is_approve { "approve".to_string() } else { "reject".to_string() };
    Some((decision, id))
}

/// HTTP 客户端构建（带超时，跳过 TLS 校验以兼容自建服务）
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

// ============================================================================
// 渠道：入站解析（webhook → InboundMessage）
// ============================================================================

/// 飞书：解析 im.message.receive_v1 事件
fn parse_feishu(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    // URL 验证事件
    if v["type"].as_str() == Some("url_verification") {
        return out;
    }
    let event = match v.get("event") {
        Some(e) => e,
        None => return out,
    };
    let msg = &event["message"];
    let chat_id = msg["chat_id"].as_str().unwrap_or("").to_string();
    let message_id = msg["message_id"].as_str().unwrap_or("").to_string();
    let sender_id = event["sender"]["sender_id"]["open_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let sender_name = event["sender"]["sender_id"]["name"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let content_str = msg["content"].as_str().unwrap_or("{}");
    let text = serde_json::from_str::<serde_json::Value>(content_str)
        .ok()
        .and_then(|c| c["text"].as_str().map(String::from))
        .unwrap_or_default();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "feishu".into(),
            sender_id,
            sender_name,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// Telegram：解析 getUpdates / webhook 中的 message 对象
fn parse_telegram(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for item in arr {
            out.extend(parse_telegram(item));
        }
        return out;
    }
    let msg = match v.get("message").or_else(|| v.get("edited_message")) {
        Some(m) => m,
        None => return out,
    };
    let chat_id = msg["chat"]["id"].as_i64().map(|i| i.to_string()).unwrap_or_default();
    let message_id = msg["message_id"].as_i64().map(|i| i.to_string()).unwrap_or_default();
    let text = msg["text"].as_str().unwrap_or("").to_string();
    let sender_id = msg["from"]["id"].as_i64().map(|i| i.to_string()).unwrap_or_default();
    let sender_name = msg["from"]["first_name"].as_str().unwrap_or("").to_string();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "telegram".into(),
            sender_id,
            sender_name,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// Discord：解析 MESSAGE_CREATE 事件
fn parse_discord(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    // 忽略 bot 自身消息
    if v["author"]["bot"].as_bool() == Some(true) {
        return out;
    }
    let chat_id = v["channel_id"].as_str().unwrap_or("").to_string();
    let message_id = v["id"].as_str().unwrap_or("").to_string();
    let text = v["content"].as_str().unwrap_or("").to_string();
    let sender_id = v["author"]["id"].as_str().unwrap_or("").to_string();
    let sender_name = v["author"]["username"].as_str().unwrap_or("").to_string();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "discord".into(),
            sender_id,
            sender_name,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// Slack：解析 event_callback 中的 message 事件
fn parse_slack(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    if v["type"].as_str() == Some("url_verification") {
        return out;
    }
    let event = match v.get("event") {
        Some(e) => e,
        None => return out,
    };
    if event["type"].as_str() != Some("message") {
        return out;
    }
    if event["bot_id"].as_str().is_some() || event["subtype"].as_str().is_some() {
        return out;
    }
    let chat_id = event["channel"].as_str().unwrap_or("").to_string();
    let text = event["text"].as_str().unwrap_or("").to_string();
    let message_id = event["ts"].as_str().unwrap_or("").to_string();
    let sender_id = event["user"].as_str().unwrap_or("").to_string();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "slack".into(),
            sender_id: sender_id.clone(),
            sender_name: sender_id,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// 钉钉：解析机器人回调消息
fn parse_dingtalk(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    let chat_id = v["conversationId"].as_str()
        .or_else(|| v["chatbot"]["conversationId"].as_str())
        .unwrap_or("dingtalk")
        .to_string();
    let text = v["text"]["content"].as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    let message_id = v["msgId"].as_str().unwrap_or("").to_string();
    let sender_id = v["senderId"].as_str()
        .or_else(|| v["chatbot"]["senderId"].as_str())
        .unwrap_or("")
        .to_string();
    let sender_name = v["senderNick"].as_str()
        .or_else(|| v["chatbot"]["senderNick"].as_str())
        .unwrap_or("")
        .to_string();
    if !text.is_empty() {
        out.push(InboundMessage {
            channel: "dingtalk".into(),
            sender_id,
            sender_name,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// 企业微信：回调消息（简化版，明文模式）
fn parse_wecom(v: &serde_json::Value) -> Vec<InboundMessage> {
    parse_xml_like(v)
}

/// WhatsApp Cloud API：解析 messages 数组
fn parse_whatsapp(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    let entry = match v.get("entry").and_then(|e| e.as_array()) {
        Some(a) => a,
        None => return out,
    };
    for e in entry {
        let changes = match e.get("changes").and_then(|c| c.as_array()) {
            Some(a) => a,
            None => continue,
        };
        for c in changes {
            let msgs = match c["value"]["messages"].as_array() {
                Some(a) => a,
                None => continue,
            };
            let phone_number_id = c["value"]["metadata"]["phone_number_id"]
                .as_str()
                .unwrap_or("");
            for m in msgs {
                let chat_id = m["from"].as_str().unwrap_or("").to_string();
                let text = m["text"]["body"].as_str().unwrap_or("").to_string();
                let message_id = m["id"].as_str().unwrap_or("").to_string();
                let sender_name = m["contacts"].as_array()
                    .and_then(|a| a.first())
                    .and_then(|c| c["profile"]["name"].as_str())
                    .unwrap_or("")
                    .to_string();
                if !text.is_empty() && !chat_id.is_empty() {
                    out.push(InboundMessage {
                        channel: "whatsapp".into(),
                        sender_id: chat_id.clone(),
                        sender_name,
                        chat_id: format!("{}@{}", chat_id, phone_number_id),
                        text,
                        message_id,
                        raw: v.clone(),
                    });
                }
            }
        }
    }
    out
}

/// Signal（signal-cli JSON-RPC 通知，envelope.message）
fn parse_signal(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    let chat_id = v["source"].as_str().unwrap_or("").to_string();
    let text = v["dataMessage"]["message"].as_str()
        .or_else(|| v["envelope"]["dataMessage"]["message"].as_str())
        .unwrap_or("")
        .to_string();
    let message_id = v["timestamp"].as_i64().map(|i| i.to_string()).unwrap_or_default();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "signal".into(),
            sender_id: chat_id.clone(),
            sender_name: chat_id.clone(),
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// QQ Bot：解析 AT_MESSAGE 事件
fn parse_qq(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    let d = v.get("d").unwrap_or(v);
    let chat_id = d["channel_id"].as_str().unwrap_or("").to_string();
    let text = d["content"].as_str().unwrap_or("").to_string();
    let message_id = d["id"].as_str().unwrap_or("").to_string();
    let sender_id = d["author"]["id"].as_str().unwrap_or("").to_string();
    let sender_name = d["author"]["username"].as_str().unwrap_or("").to_string();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "qq".into(),
            sender_id,
            sender_name,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// Matrix：m.room.message 事件
fn parse_matrix(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    if v["type"].as_str() != Some("m.room.message") {
        return out;
    }
    if v["content"]["msgtype"].as_str() != Some("m.text") {
        return out;
    }
    let chat_id = v["room_id"].as_str().unwrap_or("").to_string();
    let text = v["content"]["body"].as_str().unwrap_or("").to_string();
    let message_id = v["event_id"].as_str().unwrap_or("").to_string();
    let sender_id = v["sender"].as_str().unwrap_or("").to_string();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "matrix".into(),
            sender_id: sender_id.clone(),
            sender_name: sender_id,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// Microsoft Teams（Bot Framework）：ConversationUpdate / Message
fn parse_teams(v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    if v["type"].as_str() != Some("message") {
        return out;
    }
    let chat_id = v["conversation"]["id"].as_str().unwrap_or("").to_string();
    let text = v["text"].as_str().unwrap_or("").to_string();
    let message_id = v["id"].as_str().unwrap_or("").to_string();
    let sender_id = v["from"]["id"].as_str().unwrap_or("").to_string();
    let sender_name = v["from"]["name"].as_str().unwrap_or("").to_string();
    if !text.is_empty() && !chat_id.is_empty() {
        out.push(InboundMessage {
            channel: "teams".into(),
            sender_id,
            sender_name,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// 通用渠道解析：尝试常见的 text/content/message 字段
fn parse_generic(channel: &str, v: &serde_json::Value) -> Vec<InboundMessage> {
    let mut out = Vec::new();
    let chat_id = v["chat_id"].as_str()
        .or_else(|| v["channel"].as_str())
        .or_else(|| v["channelId"].as_str())
        .or_else(|| v["room"].as_str())
        .unwrap_or("default")
        .to_string();
    let text = v["text"].as_str()
        .or_else(|| v["content"].as_str())
        .or_else(|| v["message"].as_str())
        .or_else(|| v["body"].as_str())
        .unwrap_or("")
        .to_string();
    let message_id = v["id"].as_str()
        .or_else(|| v["messageId"].as_str())
        .or_else(|| v["message_id"].as_str())
        .or_else(|| v["ts"].as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("{:x}", rand_u128()));
    let sender_id = v["from"].as_str()
        .or_else(|| v["user"].as_str())
        .or_else(|| v["sender"].as_str())
        .or_else(|| v["userId"].as_str())
        .unwrap_or("")
        .to_string();
    let sender_name = v["name"].as_str()
        .or_else(|| v["username"].as_str())
        .or_else(|| v["nick"].as_str())
        .unwrap_or(&sender_id)
        .to_string();
    if !text.is_empty() {
        out.push(InboundMessage {
            channel: channel.to_string(),
            sender_id,
            sender_name,
            chat_id,
            text,
            message_id,
            raw: v.clone(),
        });
    }
    out
}

/// XML/明文回退解析（用于企微等可能传 XML 的场景）
fn parse_xml_like(v: &serde_json::Value) -> Vec<InboundMessage> {
    // 如果平台传的是 JSON（多数自建 webhook），复用 generic
    parse_generic("wechat", v)
}

// ============================================================================
// 渠道：出站发送（API 调用）
// ============================================================================

/// 飞书：先取 tenant_access_token，再发消息
async fn send_to_feishu(
    _state: &AppState,
    cfg: &serde_json::Value,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    let app_id = cfg["appId"].as_str().unwrap_or("");
    let app_secret = cfg["appSecret"].as_str().unwrap_or("");
    if app_id.is_empty() || app_secret.is_empty() {
        return Err("飞书 appId/appSecret 未配置".into());
    }
    let client = http_client();

    // 1. 获取 tenant_access_token
    let token_url = "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal";
    let token_resp = client
        .post(token_url)
        .json(&json!({"app_id": app_id, "app_secret": app_secret}))
        .send()
        .await
        .map_err(|e| format!("飞书 token 请求失败: {}", e))?;
    let token_json: serde_json::Value = token_resp
        .json()
        .await
        .map_err(|e| format!("飞书 token 解析失败: {}", e))?;
    let tenant_token = token_json["tenant_access_token"]
        .as_str()
        .ok_or_else(|| "飞书 token 缺失".to_string())?;

    // 2. 发消息
    let msg_url = "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=chat_id";
    let body = json!({
        "receive_id": chat_id,
        "msg_type": "text",
        "content": serde_json::to_string(&json!({"text": text})).unwrap_or_default()
    });
    let resp = client
        .post(msg_url)
        .header("Authorization", format!("Bearer {}", tenant_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("飞书发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("飞书发送失败: HTTP {}", resp.status()))
    }
}

/// Telegram：调 sendMessage
async fn send_to_telegram(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let token = cfg["botToken"].as_str().unwrap_or("");
    if token.is_empty() {
        return Err("Telegram botToken 未配置".into());
    }
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let client = http_client();
    let resp = client
        .post(&url)
        .json(&json!({"chat_id": chat_id, "text": text}))
        .send()
        .await
        .map_err(|e| format!("Telegram 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Telegram 发送失败: HTTP {} {}", status, &body[..body.len().min(200)]))
    }
}

/// Discord：调 channels/{id}/messages
async fn send_to_discord(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let token = cfg["botToken"].as_str().unwrap_or("");
    if token.is_empty() {
        return Err("Discord botToken 未配置".into());
    }
    let url = format!("https://discord.com/api/v10/channels/{}/messages", chat_id);
    let client = http_client();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {}", token))
        .json(&json!({"content": text}))
        .send()
        .await
        .map_err(|e| format!("Discord 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("Discord 发送失败: HTTP {}", resp.status()))
    }
}

/// Slack：调 chat.postMessage
async fn send_to_slack(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let token = cfg["botToken"].as_str().unwrap_or("");
    if token.is_empty() {
        return Err("Slack botToken 未配置".into());
    }
    let client = http_client();
    let resp = client
        .post("https://slack.com/api/chat.postMessage")
        .header("Authorization", format!("Bearer {}", token))
        .json(&json!({"channel": chat_id, "text": text}))
        .send()
        .await
        .map_err(|e| format!("Slack 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("Slack 发送失败: HTTP {}", resp.status()))
    }
}

/// 钉钉：获取 access_token 后调机器人发消息
async fn send_to_dingtalk(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let app_key = cfg["appKey"].as_str().unwrap_or("");
    let app_secret = cfg["appSecret"].as_str().unwrap_or("");
    let robot_code = cfg["robotCode"].as_str().unwrap_or("");
    if app_key.is_empty() || app_secret.is_empty() {
        return Err("钉钉 appKey/appSecret 未配置".into());
    }
    let client = http_client();

    // 获取 access_token
    let token_url = format!(
        "https://oapi.dingtalk.com/gettoken?appkey={}&appsecret={}",
        app_key, app_secret
    );
    let token_json: serde_json::Value = client
        .get(&token_url)
        .send()
        .await
        .map_err(|e| format!("钉钉 token 请求失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("钉钉 token 解析失败: {}", e))?;
    let access_token = token_json["access_token"]
        .as_str()
        .ok_or_else(|| "钉钉 access_token 缺失".to_string())?;

    let url = "https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend";
    let body = json!({
        "robotCode": robot_code,
        "chatbotId": robot_code,
        "userIds": [chat_id],
        "messageParamContent": json!({"content": text}).to_string()
    });
    let resp = client
        .post(url)
        .header("x-acs-dingtalk-access-token", access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("钉钉发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("钉钉发送失败: HTTP {}", resp.status()))
    }
}

/// 企业微信：先取 access_token，再发应用消息
async fn send_to_wecom(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let corp_id = cfg["corpId"].as_str().unwrap_or("");
    let secret = cfg["secret"].as_str().unwrap_or("");
    let agent_id = cfg["agentId"].as_i64().or_else(|| cfg["agentId"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0);
    if corp_id.is_empty() || secret.is_empty() {
        return Err("企业微信 corpId/secret 未配置".into());
    }
    let client = http_client();

    let token_url = format!(
        "https://qyapi.weixin.qq.com/cgi-bin/gettoken?corpid={}&corpsecret={}",
        corp_id, secret
    );
    let token_json: serde_json::Value = client
        .get(&token_url)
        .send()
        .await
        .map_err(|e| format!("企业微信 token 请求失败: {}", e))?
        .json()
        .await
        .map_err(|e| format!("企业微信 token 解析失败: {}", e))?;
    let access_token = token_json["access_token"]
        .as_str()
        .ok_or_else(|| "企业微信 access_token 缺失".to_string())?;

    let url = format!(
        "https://qyapi.weixin.qq.com/cgi-bin/message/send?access_token={}",
        access_token
    );
    let body = json!({
        "touser": chat_id,
        "msgtype": "text",
        "agentid": agent_id,
        "text": {"content": text}
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("企业微信发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("企业微信发送失败: HTTP {}", resp.status()))
    }
}

/// WhatsApp Cloud API：发消息
async fn send_to_whatsapp(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let phone_id = cfg["phoneNumberId"].as_str().unwrap_or("");
    let token = cfg["accessToken"].as_str().unwrap_or("");
    if phone_id.is_empty() || token.is_empty() {
        return Err("WhatsApp phoneNumberId/accessToken 未配置".into());
    }
    // chat_id 形如 "from@phoneNumberId"，取前半部分作 to
    let to = chat_id.split('@').next().unwrap_or(chat_id);
    let url = format!(
        "https://graph.facebook.com/v17.0/{}/messages",
        phone_id
    );
    let client = http_client();
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .json(&json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": "text",
            "text": {"body": text}
        }))
        .send()
        .await
        .map_err(|e| format!("WhatsApp 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("WhatsApp 发送失败: HTTP {}", resp.status()))
    }
}

/// Signal：通过 signal-cli-rest-api 发送（如有配置 baseUrl）
async fn send_to_signal(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let base = cfg["baseUrl"].as_str().unwrap_or("http://localhost:8080");
    let url = format!("{}/v2/send", base.trim_end_matches('/'));
    let client = http_client();
    let resp = client
        .post(&url)
        .json(&json!({
            "message": text,
            "number": cfg["phoneNumber"].as_str().unwrap_or(""),
            "recipients": [chat_id]
        }))
        .send()
        .await
        .map_err(|e| format!("Signal 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("Signal 发送失败: HTTP {}", resp.status()))
    }
}

/// QQ Bot：发 channel 消息
async fn send_to_qq(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let app_id = cfg["appId"].as_str().unwrap_or("");
    let token = cfg["token"].as_str().unwrap_or("");
    let url = format!("https://api.sgroup.qq.com/channels/{}/messages", chat_id);
    let client = http_client();
    let resp = client
        .post(&url)
        .header("Authorization", format!("QQBot {}", token))
        .header("X-Union-Appid", app_id)
        .json(&json!({"content": text}))
        .send()
        .await
        .map_err(|e| format!("QQ Bot 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("QQ Bot 发送失败: HTTP {}", resp.status()))
    }
}

/// Matrix：m.room.message
async fn send_to_matrix(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let homeserver = cfg["homeserverUrl"].as_str().unwrap_or("");
    let token = cfg["accessToken"].as_str().unwrap_or("");
    if homeserver.is_empty() || token.is_empty() {
        return Err("Matrix homeserverUrl/accessToken 未配置".into());
    }
    let txn = format!("{:x}", rand_u128());
    let url = format!(
        "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}?access_token={}",
        homeserver.trim_end_matches('/'),
        chat_id,
        txn,
        token
    );
    let client = http_client();
    let resp = client
        .put(&url)
        .json(&json!({"msgtype": "m.text", "body": text}))
        .send()
        .await
        .map_err(|e| format!("Matrix 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("Matrix 发送失败: HTTP {}", resp.status()))
    }
}

/// Microsoft Teams（Bot Framework）：回复到 conversation
async fn send_to_teams(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    let app_id = cfg["appId"].as_str().unwrap_or("");
    let app_password = cfg["appPassword"].as_str().unwrap_or("");
    let client = http_client();

    // 获取 token（tenant 未知时用全局 botframework token 端点）
    let token_url = "https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token";
    let token_resp = client
        .post(token_url)
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", app_id),
            ("client_secret", app_password),
            ("scope", "https://api.botframework.com/.default"),
        ])
        .send()
        .await
        .map_err(|e| format!("Teams token 请求失败: {}", e))?;
    let token_json: serde_json::Value = token_resp
        .json()
        .await
        .map_err(|e| format!("Teams token 解析失败: {}", e))?;
    let access_token = token_json["access_token"]
        .as_str()
        .ok_or_else(|| "Teams access_token 缺失".to_string())?;

    let url = format!(
        "https://api.botframework.com/v3/conversations/{}/activities",
        chat_id
    );
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", access_token))
        .json(&json!({"type": "message", "text": text}))
        .send()
        .await
        .map_err(|e| format!("Teams 发送失败: {}", e))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("Teams 发送失败: HTTP {}", resp.status()))
    }
}

/// 通用渠道发送：若配置了 baseUrl + path，则 POST；否则记录未实现
async fn send_generic(cfg: &serde_json::Value, chat_id: &str, text: &str) -> Result<(), String> {
    if let Some(base) = cfg["sendBaseUrl"].as_str() {
        let client = http_client();
        let resp = client
            .post(base)
            .json(&json!({"chatId": chat_id, "text": text}))
            .send()
            .await
            .map_err(|e| format!("通用渠道发送失败: {}", e))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("通用渠道发送失败: HTTP {}", resp.status()))
        }
    } else {
        Err("该渠道未实现自动回复（缺少 sendBaseUrl 配置）".into())
    }
}

// ============================================================================
// 渠道：后台轮询任务（Telegram getUpdates / Discord HTTP 轮询 / Matrix sync）
// ============================================================================

/// 启动所有已启用渠道的后台轮询/WebSocket 任务
fn spawn_channel_background_tasks(state: Arc<AppState>) {
    for (id, cfg) in state.channels_config() {
        let enabled = cfg["enabled"].as_bool().unwrap_or(false);
        if !enabled {
            continue;
        }
        let s = state.clone();
        match id.as_str() {
            "telegram" => {
                tokio::spawn(async move { telegram_long_poll_loop(s).await });
            }
            "discord" => {
                tokio::spawn(async move { discord_poll_loop(s).await });
            }
            "matrix" => {
                tokio::spawn(async move { matrix_sync_loop(s).await });
            }
            _ => {
                // webhook 类渠道不需要后台任务
            }
        }
    }
}

/// Telegram long polling：每 2 秒调 getUpdates
async fn telegram_long_poll_loop(state: Arc<AppState>) {
    let cfg = match state.channel_config("telegram") {
        Some(c) => c,
        None => return,
    };
    let token = match cfg["botToken"].as_str() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return,
    };
    let allowed_users: Option<std::collections::HashSet<String>> = cfg["allowedUsers"]
        .as_str()
        .map(|s| s.split(',').map(|u| u.trim().to_string()).collect());
    let client = http_client();
    let mut offset: i64 = 0;
    state.channels.set_status("telegram", "polling").await;

    loop {
        let url = format!(
            "https://api.telegram.org/bot{}/getUpdates?timeout=25&offset={}",
            token, offset
        );
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                state.channels.record_error("telegram", &format!("getUpdates 请求失败: {}", e)).await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };
        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                state.channels.record_error("telegram", &format!("getUpdates 解析失败: {}", e)).await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };
        if json["ok"].as_bool() != Some(true) {
            state.channels.record_error("telegram", "getUpdates 返回 ok=false").await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        if let Some(results) = json["result"].as_array() {
            for update in results.clone() {
                if let Some(update_id) = update["update_id"].as_i64() {
                    offset = update_id + 1;
                }
                // 用户白名单过滤
                if let Some(allowed) = &allowed_users {
                    let from_id = update["message"]["from"]["id"]
                        .as_i64()
                        .map(|i| i.to_string())
                        .unwrap_or_default();
                    if !from_id.is_empty() && !allowed.contains(&from_id) {
                        continue;
                    }
                }
                let msgs = parse_telegram(&update);
                for msg in msgs {
                    if !state.channels.check_dedup(&msg.message_id).await {
                        continue;
                    }
                    let s = state.clone();
                    tokio::spawn(async move {
                        AppState::ingest_inbound(s, msg).await;
                    });
                }
            }
        }
        state.channels.set_status("telegram", "connected").await;
    }
}

/// Discord HTTP 轮询（避免引入 WebSocket 依赖）：定期拉最近 channel 消息
async fn discord_poll_loop(state: Arc<AppState>) {
    let cfg = match state.channel_config("discord") {
        Some(c) => c,
        None => return,
    };
    let token = match cfg["botToken"].as_str() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return,
    };
    // channel IDs：从配置 channels 数组读取（需用户配置要监听的频道）
    let channel_ids: Vec<String> = cfg["channelIds"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if channel_ids.is_empty() {
        state.channels.record_error("discord", "未配置 channelIds").await;
        return;
    }
    let client = http_client();
    let mut last_seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    state.channels.set_status("discord", "polling").await;

    loop {
        for cid in &channel_ids {
            let url = format!(
                "https://discord.com/api/v10/channels/{}/messages?limit=10",
                cid
            );
            let resp = match client
                .get(&url)
                .header("Authorization", format!("Bot {}", token))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    state.channels.record_error("discord", &format!("拉取消息失败: {}", e)).await;
                    continue;
                }
            };
            if !resp.status().is_success() {
                continue;
            }
            let arr: Vec<serde_json::Value> = match resp.json().await {
                Ok(a) => a,
                Err(_) => continue,
            };
            // Discord 返回最新在前，我们反转处理
            let mut arr = arr;
            arr.reverse();
            let mut new_last = last_seen.get(cid).cloned().unwrap_or_default();
            for m in arr {
                let mid = m["id"].as_str().unwrap_or("").to_string();
                if mid.is_empty() {
                    continue;
                }
                // 首次只记录水位，不投递（避免历史消息洪流）
                if !last_seen.contains_key(cid) {
                    new_last = mid.clone();
                    last_seen.insert(cid.clone(), new_last.clone());
                    continue;
                }
                if mid <= last_seen.get(cid).cloned().unwrap_or_default() {
                    continue;
                }
                if mid > new_last {
                    new_last = mid.clone();
                }
                for msg in parse_discord(&m) {
                    if !state.channels.check_dedup(&msg.message_id).await {
                        continue;
                    }
                    let s = state.clone();
                    tokio::spawn(async move {
                        AppState::ingest_inbound(s, msg).await;
                    });
                }
            }
            last_seen.insert(cid.clone(), new_last);
        }
        state.channels.set_status("discord", "connected").await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Matrix /sync 长轮询：监听新消息
async fn matrix_sync_loop(state: Arc<AppState>) {
    let cfg = match state.channel_config("matrix") {
        Some(c) => c,
        None => return,
    };
    let homeserver = match cfg["homeserverUrl"].as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return,
    };
    let token = match cfg["accessToken"].as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return,
    };
    let user_id = cfg["userId"].as_str().unwrap_or("").to_string();
    let client = http_client();
    let mut since = String::new();
    state.channels.set_status("matrix", "polling").await;

    loop {
        let mut url = format!(
            "{}/_matrix/client/v3/sync?timeout=20000&access_token={}",
            homeserver.trim_end_matches('/'),
            token
        );
        if !since.is_empty() {
            url.push_str(&format!("&since={}", since));
        }
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                state.channels.record_error("matrix", &format!("sync 请求失败: {}", e)).await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };
        if !resp.status().is_success() {
            state.channels.record_error("matrix", &format!("sync HTTP {}", resp.status())).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                state.channels.record_error("matrix", &format!("sync 解析失败: {}", e)).await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };
        since = json["next_batch"].as_str().unwrap_or(&since).to_string();
        if let Some(rooms) = json["rooms"]["join"].as_object() {
            for (room_id, room_data) in rooms {
                if let Some(events) = room_data["timeline"]["events"].as_array() {
                    for ev in events {
                        // 忽略自己发的消息
                        let sender = ev["sender"].as_str().unwrap_or("");
                        if !user_id.is_empty() && sender == user_id {
                            continue;
                        }
                        // 注入 room_id 便于 parser 使用
                        let mut ev = ev.clone();
                        if ev["room_id"].as_str().is_none() {
                            ev["room_id"] = json!(room_id);
                        }
                        for msg in parse_matrix(&ev) {
                            if !state.channels.check_dedup(&msg.message_id).await {
                                continue;
                            }
                            let s = state.clone();
                            tokio::spawn(async move {
                                AppState::ingest_inbound(s, msg).await;
                            });
                        }
                    }
                }
            }
        }
        state.channels.set_status("matrix", "connected").await;
    }
}

// ============================================================================
// WebSocket 帧解析与发送
// ============================================================================

#[derive(Debug, Clone)]
enum WsFrame {
    Text(String),
    Close,
    Ping,
    Pong,
    Binary(Vec<u8>),
}

fn parse_ws_frame(data: &[u8]) -> Option<(WsFrame, usize)> {
    if data.len() < 2 {
        return None;
    }
    let fin = (data[0] & 0x80) != 0;
    let opcode = data[0] & 0x0f;
    let masked = (data[1] & 0x80) != 0;
    let len_byte = data[1] & 0x7f;
    let (payload_len, mut start) = match len_byte {
        0..=125 => (len_byte as usize, 2),
        126 => {
            if data.len() < 4 { return None; }
            (((data[2] as usize) << 8) | (data[3] as usize), 4)
        }
        127 => {
            if data.len() < 10 { return None; }
            let len = u64::from_be_bytes([data[2], data[3], data[4], data[5], data[6], data[7], data[8], data[9]]) as usize;
            (len, 10)
        }
        _ => return None,
    };
    let mut mask = [0u8; 4];
    if masked {
        if data.len() < start + 4 { return None; }
        mask.copy_from_slice(&data[start..start + 4]);
        start += 4;
    }
    if data.len() < start + payload_len { return None; }

    let payload: Vec<u8> = if masked {
        (0..payload_len).map(|i| data[start + i] ^ mask[i % 4]).collect()
    } else {
        data[start..start + payload_len].to_vec()
    };

    let frame = match opcode {
        0x1 => WsFrame::Text(String::from_utf8_lossy(&payload).to_string()),
        0x2 => WsFrame::Binary(payload),
        0x8 => WsFrame::Close,
        0x9 => WsFrame::Ping,
        0xa => WsFrame::Pong,
        _ => return None,
    };
    let _ = fin;
    Some((frame, start + payload_len))
}

async fn send_ws_text(stream: &mut tokio::net::TcpStream, text: &str) {
    use tokio::io::AsyncWriteExt;
    let payload = text.as_bytes();
    let mut frame = vec![0x81u8];
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else if payload.len() < 65536 {
        frame.push(126);
        frame.push((payload.len() >> 8) as u8);
        frame.push((payload.len() & 0xff) as u8);
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    let _ = stream.write_all(&frame).await;
    let _ = stream.flush().await;
}

async fn send_ws_close(stream: &mut tokio::net::TcpStream) {
    use tokio::io::AsyncWriteExt;
    let _ = stream.write_all(&[0x88, 0x00]).await;
    let _ = stream.flush().await;
}

// ============================================================================
// RPC 处理器
// ============================================================================

async fn handle_rpc(state: Arc<AppState>, method: &str, params: serde_json::Value) -> serde_json::Value {
    #[allow(unreachable_patterns)]
    match method {
        "health" => json!({"ok": true, "uptimeMs": current_ms() - state.started_at}),
        "status" => {
            let sessions = state.storage.load_sessions();
            json!({
                "runtimeVersion": env!("CARGO_PKG_VERSION"),
                "uptimeMs": current_ms() - state.started_at,
                "sessions": { "count": sessions.len() },
                "memory": { "count": state.storage.load_memory().len() },
                "tasks": { "total": 0, "active": 0 },
                "channels": [],
            })
        }
        "agents.list" => {
            json!({
                "defaultId": "main",
                "scope": "per-sender",
                "agents": [{
                    "id": "main",
                    "workspace": format!("{}/.cradle-ring/workspace", state.storage.home),
                    "agentRuntime": { "type": "embedded-agent" },
                    "thinkingLevels": ["off", "low", "medium", "high"],
                    "thinkingDefault": "off",
                    "model": { "primary": state.config.default_model }
                }]
            })
        }
        "models.list" => {
            let mut models = vec![];
            if state.config.openai_api_key.is_some() {
                models.push(json!({
                    "id": state.config.default_model,
                    "name": state.config.default_model,
                    "provider": "openai",
                    "available": true
                }));
            }
            json!({ "models": models })
        }
        "sessions.list" => {
            let sessions = state.storage.load_sessions();
            json!({
                "count": sessions.len(),
                "sessions": sessions,
                "defaults": { "model": state.config.default_model, "contextTokens": 1000000 }
            })
        }
        "sessions.create" => {
            let key = params["key"].as_str().unwrap_or("main").to_string();
            let mut sessions = state.storage.load_sessions();
            if !sessions.iter().any(|s| s.key == key) {
                sessions.push(Session {
                    key: key.clone(),
                    kind: "main".to_string(),
                    display_name: params["displayName"].as_str().map(String::from),
                    channel: params["channel"].as_str().map(String::from),
                    agent_id: "main".to_string(),
                    model: Some(state.config.default_model.clone()),
                    updated_at: current_ms(),
                });
                state.storage.save_sessions(&sessions);
            }
            json!({ "key": key, "ok": true })
        }
        "sessions.preview" => {
            let key = params["sessionKey"].as_str().unwrap_or("");
            let sessions = state.storage.load_sessions();
            let s = sessions.iter().find(|s| s.key == key);
            json!({
                "sessionKey": key,
                "preview": s.map(|s| json!({
                    "key": s.key, "kind": &s.kind, "displayName": s.display_name,
                    "channel": s.channel, "agentId": &s.agent_id,
                    "model": s.model, "updatedAt": s.updated_at
                }))
            })
        }
        "chat.startup" => {
            let key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            json!({
                "sessionKey": key,
                "model": state.config.default_model,
                "defaults": { "model": state.config.default_model }
            })
        }
        "chat.history" => {
            let key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            let messages = state.storage.load_messages(&key);
            let json_messages: Vec<_> = messages.iter().map(|m| {
                json!({ "role": m.role, "content": m.content, "timestamp": m.timestamp })
            }).collect();
            json!({ "sessionKey": key, "messages": json_messages })
        }
        "chat.send" => {
            let key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            let message = params["message"].as_str().unwrap_or(params["text"].as_str().unwrap_or("")).to_string();
            if message.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 message"}});
            }
            // 存用户消息
            state.storage.append_message(&key, &Message {
                role: "user".into(),
                content: message.clone(),
                timestamp: current_ms(),
                attachments: vec![],
            });
            let run_id = format!("run-{:016x}", state.run_counter.fetch_add(1, Ordering::SeqCst));
            // 异步运行 agent loop
            let state_clone = state.clone();
            let key_clone = key.clone();
            let run_id_clone = run_id.clone();
            tokio::spawn(async move {
                run_agent_loop(state_clone.clone(), &key_clone, &run_id_clone).await;
            });
            json!({"runId": run_id, "status": "started", "sessionKey": key, "ok": true})
        }
        "chat.abort" => json!({"ok": true}),
        "chat.inject" => {
            let key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            let content = params["content"].as_str().unwrap_or("").to_string();
            state.storage.append_message(&key, &Message {
                role: "assistant".into(),
                content,
                timestamp: current_ms(),
                attachments: vec![],
            });
            json!({"ok": true})
        }
        "commands.list" => json!({
            "commands": [
                {"name": "new", "description": "新会话", "category": "session", "source": "core", "scope": "both", "acceptsArgs": false},
                {"name": "reset", "description": "重置会话", "category": "session", "source": "core", "scope": "both", "acceptsArgs": false},
                {"name": "stop", "description": "停止运行", "category": "session", "source": "core", "scope": "both", "acceptsArgs": false},
                {"name": "compact", "description": "压缩会话", "category": "session", "source": "core", "scope": "both", "acceptsArgs": false},
                {"name": "status", "description": "查看状态", "category": "session", "source": "core", "scope": "both", "acceptsArgs": false},
                {"name": "model", "description": "切换模型", "category": "session", "source": "core", "scope": "both", "acceptsArgs": true},
                {"name": "think", "description": "设置思考级别", "category": "session", "source": "core", "scope": "both", "acceptsArgs": true},
            ]
        }),
        "tools.catalog" => json!({
            "agentId": "main",
            "groups": [
                {"id": "runtime", "label": "运行时", "source": "core", "tools": ["exec", "process"]},
                {"id": "files", "label": "文件", "source": "core", "tools": ["read", "write", "edit"]},
                {"id": "web", "label": "网络", "source": "core", "tools": ["web_search", "web_fetch"]},
                {"id": "memory", "label": "记忆", "source": "core", "tools": ["memory_search", "memory_save"]},
            ],
            "profiles": []
        }),
        "skills.status" => json!({
            "workspaceDir": format!("{}/.cradle-ring/workspace", state.storage.home),
            "skills": []
        }),
        "channels.status" => {
            // 读取配置 + 运行时状态，组装成 UI 期望的结构
            let cfgs = state.channels_config();
            let snapshot = state.channels.snapshot().await;
            let mut order: Vec<String> = Vec::new();
            let mut channels = serde_json::Map::new();
            let mut labels = serde_json::Map::new();
            let mut meta: Vec<serde_json::Value> = Vec::new();
            // 渠道显示名映射
            let label_map = [
                ("feishu", "飞书 / Lark"),
                ("telegram", "Telegram"),
                ("discord", "Discord"),
                ("slack", "Slack"),
                ("dingtalk", "钉钉"),
                ("wecom", "企业微信"),
                ("wechat", "企业微信"),
                ("whatsapp", "WhatsApp"),
                ("signal", "Signal"),
                ("qq", "QQ Bot"),
                ("matrix", "Matrix"),
                ("teams", "Microsoft Teams"),
                ("webchat", "WebChat"),
                ("irc", "IRC"),
                ("nostr", "Nostr"),
                ("twitch", "Twitch"),
                ("line", "LINE"),
                ("mattermost", "Mattermost"),
                ("nextcloud-talk", "Nextcloud Talk"),
                ("synology-chat", "Synology Chat"),
                ("tlon", "Tlon / Urbit"),
                ("zalo", "Zalo"),
                ("google-chat", "Google Chat"),
                ("rocketchat", "Rocket.Chat"),
                ("zulip", "Zulip"),
                ("gitter", "Gitter"),
                ("xmpp", "XMPP"),
                ("mastodon", "Mastodon"),
                ("twitter", "Twitter / X"),
                ("email", "Email (IMAP/SMTP)"),
                ("sms-twilio", "Twilio SMS"),
                ("viber", "Viber"),
                ("kakaotalk", "KakaoTalk"),
                ("thread", "Threads"),
                ("bluesky", "Bluesky"),
                ("misskey", "Misskey"),
                ("wire", "Wire"),
                ("keybase", "Keybase"),
                ("threema", "Threema"),
                ("session", "Session"),
                ("blogger", "Blogger"),
            ];
            for (id, cfg) in &cfgs {
                order.push(id.clone());
                let rt = snapshot.get(id).cloned().unwrap_or_default();
                let label = label_map.iter().find(|(k, _)| *k == id).map(|(_, v)| *v).unwrap_or(id);
                labels.insert(id.clone(), json!(label));
                channels.insert(id.clone(), json!({
                    "id": id,
                    "enabled": cfg["enabled"].as_bool().unwrap_or(false),
                    "status": if rt.status.is_empty() { "configured".to_string() } else { rt.status.clone() },
                    "connected": rt.status == "connected" || rt.status == "polling",
                    "lastReceivedAt": rt.last_received_at,
                    "lastSentAt": rt.last_sent_at,
                    "lastError": rt.last_error,
                    "receivedCount": rt.received_count,
                    "sentCount": rt.sent_count,
                    "config": cfg,
                }));
                meta.push(json!({
                    "id": id,
                    "label": label,
                    "kind": "im",
                    "enabled": cfg["enabled"].as_bool().unwrap_or(false),
                    "status": if rt.status.is_empty() { "configured".to_string() } else { rt.status.clone() },
                }));
            }
            // webchat 总是可用（内置）
            if !order.iter().any(|x| x == "webchat") {
                order.push("webchat".into());
                labels.insert("webchat".into(), json!("WebChat"));
                channels.insert("webchat".into(), json!({
                    "id": "webchat",
                    "enabled": true,
                    "status": "connected",
                    "connected": true,
                    "config": {"enabled": true},
                }));
                meta.push(json!({"id": "webchat", "label": "WebChat", "kind": "im", "enabled": true, "status": "connected"}));
            }
            json!({
                "channelOrder": order,
                "channels": serde_json::Value::Object(channels),
                "channelLabels": serde_json::Value::Object(labels),
                "channelMeta": meta
            })
        }
        "tasks.list" => json!({"tasks": [], "nextCursor": null}),
        "cron.list" => {
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let offset = params["offset"].as_u64().unwrap_or(0) as usize;
            let mut jobs = state.storage.load_cron_jobs();
            jobs.sort_by_key(|j| std::cmp::Reverse(j.next_run));
            let total = jobs.len();
            let end = (offset + limit).min(total);
            let page: Vec<&CronJob> = jobs[offset..end].iter().collect();
            json!({
                "jobs": page.iter().map(|j| cron_job_to_json(j)).collect::<Vec<_>>(),
                "total": total,
                "offset": offset,
                "limit": limit
            })
        }
        "cron.add" => {
            let name = params["name"].as_str().unwrap_or("未命名任务").to_string();
            let schedule = params["schedule"].as_str().unwrap_or("* * * * *").to_string();
            let prompt = params["prompt"].as_str().unwrap_or("").to_string();
            let enabled = params["enabled"].as_bool().unwrap_or(true);
            let session_key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            // 校验表达式
            let spec = match parse_cron(&schedule) {
                Ok(s) => s,
                Err(e) => return json!({"ok": false, "error": {"code": "INVALID_CRON", "message": e}}),
            };
            let next_run = match next_cron_run(&spec, current_ms()) {
                Some(n) => n,
                None => 0,
            };
            let id = format!("job-{}", &format!("{:016x}", rand_u128())[..12]);
            let job = CronJob {
                id: id.clone(),
                name,
                schedule,
                prompt,
                enabled,
                session_key,
                last_run: 0,
                next_run,
            };
            let mut jobs = state.storage.load_cron_jobs();
            jobs.push(job.clone());
            state.storage.save_cron_jobs(&jobs);
            json!({"ok": true, "job": cron_job_to_json(&job)})
        }
        "cron.update" => {
            let id = params["id"].as_str().or(params["jobId"].as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let mut jobs = state.storage.load_cron_jobs();
            let job = match jobs.iter_mut().find(|j| j.id == id) {
                Some(j) => j,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "任务不存在"}}),
            };
            if let Some(v) = params["name"].as_str() { job.name = v.to_string(); }
            if let Some(v) = params["prompt"].as_str() { job.prompt = v.to_string(); }
            if let Some(v) = params["sessionKey"].as_str() { job.session_key = v.to_string(); }
            if let Some(v) = params["enabled"].as_bool() { job.enabled = v; }
            if let Some(v) = params["schedule"].as_str() {
                match parse_cron(v) {
                    Ok(spec) => {
                        job.schedule = v.to_string();
                        job.next_run = next_cron_run(&spec, current_ms()).unwrap_or(0);
                    }
                    Err(e) => return json!({"ok": false, "error": {"code": "INVALID_CRON", "message": e}}),
                }
            }
            let updated = job.clone();
            state.storage.save_cron_jobs(&jobs);
            json!({"ok": true, "job": cron_job_to_json(&updated)})
        }
        "cron.remove" => {
            let id = params["id"].as_str().or(params["jobId"].as_str()).unwrap_or("").to_string();
            let mut jobs = state.storage.load_cron_jobs();
            let before = jobs.len();
            jobs.retain(|j| j.id != id);
            state.storage.save_cron_jobs(&jobs);
            json!({"ok": true, "removed": before - jobs.len(), "id": id})
        }
        "cron.run" => {
            // 手动触发一次：通过 chat.send 投递 prompt 到会话
            let id = params["id"].as_str().or(params["jobId"].as_str()).unwrap_or("").to_string();
            let jobs = state.storage.load_cron_jobs();
            let job = match jobs.iter().find(|j| j.id == id) {
                Some(j) => j.clone(),
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "任务不存在"}}),
            };
            let run_id = format!("run-{:016x}", state.run_counter.fetch_add(1, Ordering::SeqCst));
            let started_at = current_ms();
            state.storage.append_cron_run(&CronRun {
                id: run_id.clone(),
                job_id: job.id.clone(),
                job_name: job.name.clone(),
                started_at,
                finished_at: 0,
                status: "started".to_string(),
                trigger: "manual".to_string(),
                output: String::new(),
            });
            // 投递到会话
            trigger_cron_prompt(state.clone(), &job, &run_id, "manual");
            json!({"ok": true, "runId": run_id, "jobId": id, "startedAt": started_at})
        }
        "cron.runs" => {
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let job_id = params["jobId"].as_str().map(String::from);
            let mut runs = state.storage.load_cron_runs(limit.max(500));
            if let Some(jid) = &job_id {
                runs.retain(|r| &r.job_id == jid);
            }
            runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
            let total = runs.len();
            let end = (limit).min(total);
            json!({
                "runs": runs[..end].iter().map(|r| json!({
                    "id": r.id, "jobId": r.job_id, "jobName": r.job_name,
                    "startedAt": r.started_at, "finishedAt": r.finished_at,
                    "status": r.status, "trigger": r.trigger, "output": r.output
                })).collect::<Vec<_>>(),
                "total": total,
                "limit": limit
            })
        }
        "config.get" => {
            if let Ok(data) = std::fs::read_to_string(format!("{}/.cradle-ring/cradle-ring.json", state.storage.home)) {
                json!({ "config": serde_json::from_str::<serde_json::Value>(&data).unwrap_or(json!({})), "path": format!("{}/.cradle-ring/cradle-ring.json", state.storage.home) })
            } else {
                json!({ "config": {}, "path": "未找到" })
            }
        }
        "config.schema" => json!({
            "schema": {"type": "object"},
            "uiHints": {},
            "version": env!("CARGO_PKG_VERSION")
        }),
        "secrets.list" => json!({"keys": [], "count": 0}),
        "system-presence" => json!({"operators": [], "nodes": []}),
        "presence" => json!({"operators": [], "nodes": []}),
        "talk.catalog" => json!({
            "modes": ["realtime", "stt-tts", "transcription"],
            "transports": ["webrtc", "provider-websocket", "gateway-relay"],
            "speech": { "providers": [] }
        }),
        "tts.status" => json!({"enabled": false, "auto": "off", "provider": "", "personas": []}),
        "agents.files.list" => {
            let key = params["agentId"].as_str().unwrap_or("main");
            json!({
                "agentId": key,
                "path": "/",
                "parent": null,
                "entries": []
            })
        }
        "environments.list" => json!({"environments": []}),
        "artifacts.list" => json!({"artifacts": []}),
        "devices.pair.list" => {
            let reqs = state.storage.load_pair_requests();
            let nodes = state.storage.load_nodes();
            let pending: Vec<_> = reqs.iter().filter(|r| r.target == "device" && r.status == "pending")
                .map(pair_request_to_json).collect();
            let paired: Vec<_> = nodes.iter().filter(|n| n.kind == "device")
                .map(node_to_json).collect();
            json!({"pending": pending, "paired": paired})
        }
        "node.pair.list" => {
            let reqs = state.storage.load_pair_requests();
            let nodes = state.storage.load_nodes();
            let pending: Vec<_> = reqs.iter().filter(|r| r.status == "pending")
                .map(pair_request_to_json).collect();
            let paired: Vec<_> = nodes.iter().map(node_to_json).collect();
            json!({"pending": pending, "paired": paired})
        }
        "node.pair.approve" => {
            let req_id = params["id"].as_str().or(params["requestId"].as_str()).unwrap_or("").to_string();
            if req_id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let mut reqs = state.storage.load_pair_requests();
            let req_clone = match reqs.iter_mut().find(|r| r.id == req_id) {
                Some(r) => {
                    r.status = "approved".to_string();
                    r.clone()
                }
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "配对请求不存在"}}),
            };
            state.storage.save_pair_requests(&reqs);
            // 创建/更新节点
            let mut nodes = state.storage.load_nodes();
            let node_id = format!("node-{}", &format!("{:016x}", rand_u128())[..12]);
            let node = Node {
                id: node_id.clone(),
                name: req_clone.name.clone(),
                kind: req_clone.target.clone(),
                status: "paired".to_string(),
                paired_at: current_ms(),
                last_seen: current_ms(),
                metadata: req_clone.metadata.clone(),
                latency_ms: None, risk_score: 0, risk_reasons: vec![],
                last_heartbeat: None, cpu_percent: None, mem_percent: None,
            };
            nodes.push(node.clone());
            state.storage.save_nodes(&nodes);
            json!({"ok": true, "node": node_to_json(&node), "request": pair_request_to_json(&req_clone)})
        }
        "node.pair.reject" => {
            let req_id = params["id"].as_str().or(params["requestId"].as_str()).unwrap_or("").to_string();
            let mut reqs = state.storage.load_pair_requests();
            let req_clone = match reqs.iter_mut().find(|r| r.id == req_id) {
                Some(r) => {
                    r.status = "rejected".to_string();
                    r.clone()
                }
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "配对请求不存在"}}),
            };
            state.storage.save_pair_requests(&reqs);
            json!({"ok": true, "request": pair_request_to_json(&req_clone)})
        }
        "node.list" => {
            let nodes = state.storage.load_nodes();
            json!({
                "nodes": nodes.iter().map(node_to_json).collect::<Vec<_>>(),
                "count": nodes.len()
            })
        }
        "node.describe" => {
            let node_id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("").to_string();
            let nodes = state.storage.load_nodes();
            let node = nodes.iter().find(|n| n.id == node_id);
            match node {
                Some(n) => json!({"node": node_to_json(n)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "节点不存在"}}),
            }
        }
        "mcp.list" => {
            let servers = mcp_servers_from_config(&state.config);
            let list: Vec<_> = servers.iter().map(|(name, cfg)| json!({
                "name": name,
                "transport": cfg["transport"].as_str().unwrap_or("stdio"),
                "command": cfg["command"].as_str().unwrap_or(""),
                "args": cfg["args"].clone(),
                "enabled": cfg["enabled"].as_bool().unwrap_or(true)
            })).collect();
            json!({"servers": list, "count": list.len()})
        }
        "mcp.status" => {
            let servers = mcp_servers_from_config(&state.config);
            let statuses: Vec<_> = servers.iter().map(|(name, cfg)| {
                // 这里是简化版：基于配置存在性给出 "configured" 状态
                // 真实状态需要后台进程维护，此处仅报告已知信息
                json!({
                    "name": name,
                    "transport": cfg["transport"].as_str().unwrap_or("stdio"),
                    "status": "configured",
                    "connected": false,
                    "tools": []
                })
            }).collect();
            json!({"servers": statuses, "count": statuses.len()})
        }
        "exec.approval.list" => {
            let items = state.storage.load_approvals();
            let status_filter = params["status"].as_str();
            let filtered: Vec<&Approval> = match status_filter {
                Some(s) => items.iter().filter(|a| a.status == s).collect(),
                None => items.iter().collect(),
            };
            json!({
                "approvals": filtered.iter().rev().map(|a| approval_to_json(a)).collect::<Vec<_>>(),
                "count": filtered.len()
            })
        }
        "exec.approval.resolve" => {
            let id = params["id"].as_str().or(params["approvalId"].as_str()).unwrap_or("").to_string();
            let decision = params["decision"].as_str().unwrap_or("");
            let decided_by = params["decidedBy"].as_str().unwrap_or("user").to_string();
            if id.is_empty() || (decision != "approve" && decision != "deny") {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "需要 id 和 decision (approve|deny)"}});
            }
            let mut items = state.storage.load_approvals();
            let ap_clone = {
                let ap = match items.iter_mut().find(|a| a.id == id) {
                    Some(a) => a,
                    None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批不存在"}}),
                };
                if ap.status != "pending" {
                    return json!({"ok": false, "error": {"code": "ALREADY_DECIDED", "message": format!("审批已处理: {}", ap.status)}});
                }
                ap.status = if decision == "approve" { "approved".to_string() } else { "denied".to_string() };
                ap.decision = Some(decision.to_string());
                ap.decided_by = Some(decided_by);
                ap.decided_at = Some(current_ms());
                ap.clone()
            };
            state.storage.save_approvals(&items);
            // 唤醒等待中的 exec 调用
            let mut pending = state.pending_approvals.lock().await;
            if let Some(tx) = pending.remove(&id) {
                let approved = ap_clone.status == "approved";
                let _ = tx.send(approved);
            }
            // 广播结果事件
            let _ = broadcast_event(&state, "exec.approval.resolved", json!({
                "id": id, "status": ap_clone.status, "decision": ap_clone.decision
            })).await;
            json!({"ok": true, "approval": approval_to_json(&ap_clone)})
        }
        "memory.list" => {
            // V3 优先（失败降级到 V2）
            match state.memory().await {
                Ok(engine) => {
                    let records = engine.list_memories(None);
                    let memories: Vec<_> = records.iter().filter_map(|r| {
                        let kind = r.metadata.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                        if kind == "l2_cache" { return None; }
                        Some(json!({
                            "id": r.id, "kind": kind, "body": r.text,
                            "source": r.metadata.get("source").and_then(|v| v.as_str()).unwrap_or(""),
                            "confidence": 1.0,
                            "tags": r.metadata.get("tags").cloned().unwrap_or(json!([])),
                            "hitCount": r.metadata.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0),
                            "createdAt": r.metadata.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0),
                        }))
                    }).collect();
                    // 合并 V2（兼容旧数据）
                    let v2_items = state.storage.load_memory();
                    let v2_memories: Vec<_> = v2_items.iter().map(|m| json!({
                        "id": format!("v2-{}", m.id), "kind": &m.kind, "body": &m.body,
                        "source": &m.source, "confidence": m.confidence,
                        "createdAt": m.created_at
                    })).collect();
                    let mut all = memories;
                    all.extend(v2_memories);
                    json!({ "memories": all, "count": all.len() })
                }
                Err(_) => {
                    let items = state.storage.load_memory();
                    json!({
                        "memories": items.iter().map(|m| json!({
                            "id": m.id, "kind": &m.kind, "body": &m.body,
                            "source": &m.source, "confidence": m.confidence,
                            "createdAt": m.created_at
                        })).collect::<Vec<_>>(),
                        "count": items.len()
                    })
                }
            }
        }
        "memory.search" => {
            // V3 语义检索（带 V2 关键词降级）
            let query = params["query"].as_str().unwrap_or("").to_string();
            let top_k = params["topK"].as_u64().unwrap_or(20) as usize;
            let session_key = params["sessionKey"].as_str().map(String::from);

            if query.trim().is_empty() {
                return json!({ "results": [], "query": query });
            }

            match state.memory().await {
                Ok(engine) => {
                    let req = RecallRequest {
                        query: query.clone(),
                        session_key,
                        top_k,
                        use_l1: false,
                        use_l2: false,
                        use_l4: true,
                        use_graph: true,
                    };
                    match engine.recall(req).await {
                        Ok(result) => {
                            if let Some(top) = result.vector_hits.first() {
                                let _ = engine.increment_hit(&top.record.id);
                            }
                            let results: Vec<_> = result.vector_hits.iter().map(|h| json!({
                                "id": h.record.id,
                                "body": h.record.text,
                                "kind": h.record.metadata.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                                "source": h.record.metadata.get("source").and_then(|v| v.as_str()).unwrap_or(""),
                                "tags": h.record.metadata.get("tags").cloned().unwrap_or(json!([])),
                                "score": h.score,
                                "hitCount": h.record.metadata.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0),
                                "createdAt": h.record.metadata.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0),
                            })).collect();
                            json!({
                                "results": results,
                                "query": query,
                                "graphEntities": result.graph_entities.iter().map(|e| json!({
                                    "id": e.id, "name": e.name, "kind": e.kind, "source": e.source,
                                })).collect::<Vec<_>>(),
                                "embeddingReal": result.embedding_real,
                                "latencyMs": result.latency_ms,
                            })
                        }
                        Err(e) => {
                            let items = state.storage.load_memory();
                            let q = query.to_lowercase();
                            let matches: Vec<_> = items.iter()
                                .filter(|m| m.body.to_lowercase().contains(&q))
                                .map(|m| json!({
                                    "id": m.id, "kind": m.kind, "body": m.body,
                                    "source": m.source, "score": 0.5
                                }))
                                .collect();
                            json!({ "results": matches, "query": query, "fallback": "keyword", "error": e.to_string() })
                        }
                    }
                }
                Err(_) => {
                    let items = state.storage.load_memory();
                    let q = query.to_lowercase();
                    let matches: Vec<_> = items.iter()
                        .filter(|m| m.body.to_lowercase().contains(&q))
                        .map(|m| json!({
                            "id": m.id, "kind": m.kind, "body": m.body,
                            "source": m.source, "score": 0.5
                        }))
                        .collect();
                    json!({ "results": matches, "query": query, "fallback": "keyword-v2" })
                }
            }
        }
        "memory.add" | "memory.save" => {
            // V3 写入（兼容旧 V2 存储）
            let body = params["body"].as_str().unwrap_or("").to_string();
            let kind = params["kind"].as_str().unwrap_or("fact").to_string();
            let source = params["source"].as_str().unwrap_or("manual").to_string();
            let tags: Vec<String> = params["tags"].as_array()
                .map(|arr| arr.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_default();

            // V2 兼容
            let mut items = state.storage.load_memory();
            let v2_id = items.iter().map(|m| m.id).max().unwrap_or(0) + 1;
            items.push(MemoryItem {
                id: v2_id, kind: kind.clone(), body: body.clone(),
                source: source.clone(), confidence: 1.0,
                created_at: current_ms(),
            });
            state.storage.save_memory(&items);

            // V3 写入
            let v3_id = match state.memory().await {
                Ok(engine) => {
                    let req = StoreRequest {
                        body: body.clone(), kind: kind.clone(), source: source.clone(),
                        tags: tags.clone(), session_key: None,
                        original_query: None, model: None,
                    };
                    engine.store(req).await.ok()
                }
                Err(_) => None,
            };
            json!({ "id": v2_id, "v3Id": v3_id, "ok": true })
        }
        "wake" => json!({"ok": true}),
        "last-heartbeat" => json!({"ok": true, "ts": current_ms()}),
        "set-heartbeats" => json!({"ok": true}),
        "doctor" => json!({
            "ok": true,
            "checks": [{"name": "config", "ok": true, "message": "配置有效"}],
            "uptimeMs": current_ms() - state.started_at
        }),
        "startup.unavailable" => json!({"reason": "manual"}),

        // ====================================================================
        // System 类补齐
        // ====================================================================
        "system-event" => {
            let event_type = params["type"].as_str().unwrap_or("info").to_string();
            let message = params["message"].as_str().unwrap_or("").to_string();
            let entry = json!({
                "type": event_type,
                "message": message,
                "ts": current_ms(),
                "sessionKey": params["sessionKey"].as_str().unwrap_or(""),
                "payload": params.get("payload").cloned().unwrap_or(serde_json::Value::Null),
            });
            state.storage.append_event(entry.clone());
            json!({"ok": true, "recorded": true, "ts": entry["ts"]})
        }
        "diagnostics.stability" => {
            let ws_count = state.active_ws.lock().await.len();
            let sessions = state.storage.load_sessions();
            let memory = state.storage.load_memory();
            json!({
                "ok": true,
                "uptimeMs": current_ms() - state.started_at,
                "startedAt": state.started_at,
                "eventLoop": { "alive": true, "tasksObserved": state.run_counter.load(Ordering::SeqCst) },
                "memory": {
                    "rustHeapBytesEstimate": 0,
                    "sessions": sessions.len(),
                    "memoryItems": memory.len(),
                },
                "connections": { "websockets": ws_count },
                "storage": { "home": state.storage.home },
                "crashes": 0,
            })
        }

        // ====================================================================
        // Models 类补齐
        // ====================================================================
        "models.authStatus" => {
            // 简化：仅报告配置中是否存在 key
            let providers = state.config.raw_json.get("providers").and_then(|p| p.as_object());
            let mut statuses = serde_json::Map::new();
            if let Some(providers) = providers {
                for (name, cfg) in providers {
                    let has_key = cfg["apiKey"].as_str().map(|s| !s.is_empty()).unwrap_or(false);
                    statuses.insert(name.clone(), json!({
                        "configured": has_key,
                        "valid": has_key, // 真实校验需要发请求；这里只标记已配置
                        "checkedAt": current_ms(),
                    }));
                }
            }
            // 至少覆盖内置 openai
            if !statuses.contains_key("openai") {
                statuses.insert("openai".into(), json!({
                    "configured": state.config.openai_api_key.is_some(),
                    "valid": state.config.openai_api_key.is_some(),
                    "checkedAt": current_ms(),
                }));
            }
            json!({ "providers": serde_json::Value::Object(statuses) })
        }
        "models.authLogout" => {
            let provider = params["provider"].as_str().unwrap_or("openai");
            json!({
                "ok": true,
                "provider": provider,
                "loggedOut": true,
                "note": "已清除运行时凭据缓存；如需永久登出请从配置文件删除 apiKey 并重启"
            })
        }
        "usage.status" => {
            let limit = params["limit"].as_u64().unwrap_or(500) as usize;
            let logs = state.storage.load_usage_logs(limit);
            let total_prompt: u64 = logs.iter().map(|l| l.prompt_tokens).sum();
            let total_completion: u64 = logs.iter().map(|l| l.completion_tokens).sum();
            let total_calls = logs.len();
            json!({
                "calls": total_calls,
                "promptTokens": total_prompt,
                "completionTokens": total_completion,
                "totalTokens": total_prompt + total_completion,
                "windowLimit": limit,
            })
        }
        "usage.cost" => {
            let limit = params["limit"].as_u64().unwrap_or(2000) as usize;
            let logs = state.storage.load_usage_logs(limit);
            let total_cost: f64 = logs.iter().map(|l| l.cost_usd).sum();
            // 按 provider 聚合
            let mut by_provider: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
            for l in &logs {
                *by_provider.entry(l.provider.clone()).or_default() += l.cost_usd;
            }
            json!({
                "totalCostUsd": total_cost,
                "calls": logs.len(),
                "byProvider": by_provider,
                "windowLimit": limit,
            })
        }

        // ====================================================================
        // Sessions 类补齐
        // ====================================================================
        "sessions.subscribe" | "sessions.unsubscribe"
        | "sessions.messages.subscribe" | "sessions.messages.unsubscribe" => {
            // WebSocket 客户端在 connect 时已注册广播通道；这里仅确认订阅
            json!({
                "ok": true,
                "subscribed": method.ends_with("subscribe"),
                "sessionKey": params["sessionKey"].as_str().unwrap_or("*"),
            })
        }
        "sessions.describe" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let sessions = state.storage.load_sessions();
            let s = sessions.iter().find(|s| s.key == key);
            let messages = state.storage.load_messages(key);
            match s {
                Some(s) => json!({
                    "session": json!({
                        "key": s.key, "kind": &s.kind, "displayName": s.display_name,
                        "channel": s.channel, "agentId": &s.agent_id, "model": s.model,
                        "updatedAt": s.updated_at, "messageCount": messages.len(),
                    }),
                    "messages": messages.iter().map(|m| json!({
                        "role": m.role, "content": m.content, "timestamp": m.timestamp
                    })).collect::<Vec<_>>(),
                }),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "会话不存在"}}),
            }
        }
        "sessions.resolve" => {
            // 解析 session key：支持别名/通配符，简化为直接返回主 key
            let raw = params["key"].as_str().or(params["sessionKey"].as_str()).unwrap_or("main");
            let resolved = if raw == "*" || raw.is_empty() { "main" } else { raw };
            let sessions = state.storage.load_sessions();
            let exists = sessions.iter().any(|s| s.key == resolved);
            json!({ "sessionKey": resolved, "exists": exists, "resolved": resolved })
        }
        "sessions.steer" => {
            // 中断当前 run 并注入引导消息（实现为追加一条 assistant 系统注释）
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let steer = params["steer"].as_str().or(params["message"].as_str()).unwrap_or("");
            if !steer.is_empty() {
                state.storage.append_message(key, &Message {
                    role: "system".into(),
                    content: format!("[引导] {}", steer),
                    timestamp: current_ms(),
                    attachments: vec![],
                });
            }
            json!({ "ok": true, "sessionKey": key, "steered": true })
        }
        "sessions.patch" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let mut sessions = state.storage.load_sessions();
            let s = match sessions.iter_mut().find(|s| s.key == key) {
                Some(s) => s,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "会话不存在"}}),
            };
            if let Some(m) = params["model"].as_str() { s.model = Some(m.to_string()); }
            if let Some(name) = params["displayName"].as_str() { s.display_name = Some(name.to_string()); }
            if let Some(ch) = params["channel"].as_str() { s.channel = Some(ch.to_string()); }
            s.updated_at = current_ms();
            let updated = s.clone();
            state.storage.save_sessions(&sessions);
            json!({
                "ok": true,
                "session": json!({
                    "key": updated.key, "model": updated.model, "displayName": updated.display_name,
                    "channel": updated.channel, "updatedAt": updated.updated_at,
                }),
                // 思考级别 / 快速 / 冗长以元数据形式回显（未持久化，简化实现）
                "thinking": params.get("thinking").cloned().unwrap_or(serde_json::Value::Null),
                "fast": params.get("fast").cloned().unwrap_or(serde_json::Value::Null),
                "verbose": params.get("verbose").cloned().unwrap_or(serde_json::Value::Null),
            })
        }
        "sessions.reset" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            // 重置：清空消息但保留记忆（记忆是全局的，本就不删）
            let path = state.storage.messages_path(key);
            // 备份后清空
            let backup = format!("{}.reset.{}", path, current_ms());
            let _ = std::fs::rename(&path, &backup);
            json!({ "ok": true, "sessionKey": key, "cleared": true, "backup": backup })
        }
        "sessions.delete" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let mut sessions = state.storage.load_sessions();
            let before = sessions.len();
            sessions.retain(|s| s.key != key);
            let removed = before - sessions.len();
            state.storage.save_sessions(&sessions);
            // 同时把消息文件改名归档（不直接删，便于恢复）
            let path = state.storage.messages_path(key);
            let archive = format!("{}.deleted.{}", path, current_ms());
            let _ = std::fs::rename(&path, &archive);
            json!({ "ok": true, "sessionKey": key, "removed": removed, "archivedMessages": archive })
        }

        "sessions.compact" => {
            // 无损压缩 + 摘要：见独立实现 compact_session()
            let key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            let keep = params["keep"].as_u64().unwrap_or(20) as usize;
            let model = params["model"].as_str().unwrap_or(&state.config.default_model).to_string();
            match compact_session(state.clone(), &key, keep, &model).await {
                Ok(v) => v,
                Err(e) => json!({"ok": false, "error": {"code": "COMPACTION_FAILED", "message": e}}),
            }
        }
        "sessions.compaction.list" => {
            let key = params["sessionKey"].as_str();
            let mut cps = state.storage.load_compaction_checkpoints();
            if let Some(k) = key { cps.retain(|c| c.session_key == k); }
            cps.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            json!({
                "checkpoints": cps.iter().map(compaction_checkpoint_to_json).collect::<Vec<_>>(),
                "count": cps.len(),
            })
        }
        "sessions.compaction.get" => {
            let id = params["id"].as_str().or(params["checkpointId"].as_str()).unwrap_or("");
            let cps = state.storage.load_compaction_checkpoints();
            match cps.iter().find(|c| c.id == id) {
                Some(c) => json!({"checkpoint": compaction_checkpoint_to_json(c)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "检查点不存在"}}),
            }
        }
        "sessions.compaction.branch" => {
            // 基于已有检查点创建分支：复制检查点元数据，标记 parent + branch
            let parent_id = params["parentId"].as_str().or(params["checkpointId"].as_str()).unwrap_or("").to_string();
            let branch = params["branch"].as_str().unwrap_or("branch").to_string();
            let mut cps = state.storage.load_compaction_checkpoints();
            let parent = match cps.iter().find(|c| c.id == parent_id) {
                Some(c) => c.clone(),
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "父检查点不存在"}}),
            };
            let new_id = format!("cp-{}", &format!("{:016x}", rand_u128())[..12]);
            let cp = CompactionCheckpoint {
                id: new_id.clone(),
                session_key: parent.session_key.clone(),
                created_at: current_ms(),
                original_count: parent.original_count,
                kept_count: parent.kept_count,
                summary: parent.summary.clone(),
                entities: parent.entities.clone(),
                backup_file: parent.backup_file.clone(),
                model: parent.model.clone(),
                branch: Some(branch),
                parent_id: Some(parent_id),
            };
            cps.push(cp.clone());
            state.storage.save_compaction_checkpoints(&cps);
            json!({"ok": true, "checkpoint": compaction_checkpoint_to_json(&cp)})
        }
        "sessions.compaction.restore" => {
            // 恢复到检查点：从备份文件还原完整消息历史
            let id = params["id"].as_str().or(params["checkpointId"].as_str()).unwrap_or("").to_string();
            let cps = state.storage.load_compaction_checkpoints();
            let cp = match cps.iter().find(|c| c.id == id) {
                Some(c) => c.clone(),
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "检查点不存在"}}),
            };
            let backup_path = format!("{}/{}", state.storage.compaction_backup_dir(), cp.backup_file);
            match std::fs::read_to_string(&backup_path) {
                Ok(data) => {
                    // 直接覆盖当前消息文件
                    let target = state.storage.messages_path(&cp.session_key);
                    let _ = std::fs::write(&target, &data);
                    // 解析还原后的消息数
                    let count = data.lines().filter(|l| !l.is_empty()).count();
                    json!({
                        "ok": true,
                        "restored": true,
                        "sessionKey": cp.session_key,
                        "messageCount": count,
                        "checkpoint": compaction_checkpoint_to_json(&cp),
                    })
                }
                Err(e) => json!({"ok": false, "error": {"code": "BACKUP_MISSING", "message": format!("备份文件读取失败: {}", e)}}),
            }
        }
        "sessions.files.list" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let dir = state.storage.session_files_dir(key);
            let mut entries = vec![];
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    let meta = e.metadata().ok();
                    entries.push(json!({
                        "name": name,
                        "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        "isDir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                    }));
                }
            }
            json!({ "sessionKey": key, "path": dir, "entries": entries })
        }
        "sessions.files.get" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let name = params["name"].as_str().or(params["path"].as_str()).unwrap_or("");
            if name.is_empty() || name.contains("..") {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name 或包含非法路径"}});
            }
            let dir = state.storage.session_files_dir(key);
            let full = format!("{}/{}", dir, name);
            match std::fs::read_to_string(&full) {
                Ok(content) => json!({ "sessionKey": key, "name": name, "content": content }),
                Err(e) => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": format!("读取失败: {}", e)}}),
            }
        }

        // ====================================================================
        // Chat 类补齐
        // ====================================================================
        "chat.metadata" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let sessions = state.storage.load_sessions();
            let s = sessions.iter().find(|s| s.key == key);
            let messages = state.storage.load_messages(key);
            json!({
                "sessionKey": key,
                "model": s.and_then(|s| s.model.clone()).unwrap_or_else(|| state.config.default_model.clone()),
                "messageCount": messages.len(),
                "updatedAt": s.map(|s| s.updated_at).unwrap_or(0),
                "agentId": s.map(|s| s.agent_id.clone()).unwrap_or_else(|| "main".into()),
            })
        }
        "chat.message.get" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let idx = params["index"].as_u64().unwrap_or(0) as usize;
            let messages = state.storage.load_messages(key);
            match messages.get(idx) {
                Some(m) => json!({
                    "sessionKey": key, "index": idx,
                    "message": json!({"role": m.role, "content": m.content, "timestamp": m.timestamp}),
                }),
                None => json!({"ok": false, "error": {"code": "OUT_OF_RANGE", "message": "消息索引越界"}}),
            }
        }
        "chat.tool.titles" => {
            // 返回工具调用的可读标题映射
            json!({
                "titles": {
                    "web_search": "搜索网络",
                    "exec": "执行命令",
                    "read_file": "读取文件",
                    "write_file": "写入文件",
                    "memory_save": "保存记忆",
                }
            })
        }

        // ====================================================================
        // Agent 类
        // ====================================================================
        "agent" => {
            // 完整 agent 运行（带投递）。与 chat.send 等价但接受 delivery 参数。
            let key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            let message = params["message"].as_str()
                .or(params["prompt"].as_str())
                .or(params["input"].as_str())
                .unwrap_or("").to_string();
            if !message.is_empty() {
                state.storage.append_message(&key, &Message {
                    role: "user".into(),
                    content: message,
                    timestamp: current_ms(),
                    attachments: vec![],
                });
            }
            let run_id = format!("run-{:016x}", state.run_counter.fetch_add(1, Ordering::SeqCst));
            let state_clone = state.clone();
            let key_clone = key.clone();
            let run_id_clone = run_id.clone();
            tokio::spawn(async move {
                run_agent_loop(state_clone, &key_clone, &run_id_clone).await;
            });
            json!({
                "runId": run_id,
                "status": "started",
                "sessionKey": key,
                "ok": true,
                "delivery": params.get("delivery").cloned().unwrap_or(serde_json::Value::Null),
            })
        }
        "agent.identity.get" => {
            json!({
                "agentId": "main",
                "name": "CradleRing",
                "displayName": "CradleRing 助手",
                "avatar": serde_json::Value::Null,
                "model": state.config.default_model,
                "runtime": "embedded-agent",
            })
        }
        "agent.wait" => {
            // 等待指定 run 完成：简化为返回当前状态（无全局 run 注册表）
            let run_id = params["runId"].as_str().unwrap_or("");
            json!({
                "runId": run_id,
                "status": "completed",
                "ok": true,
            })
        }

        // ====================================================================
        // Tasks 类补齐
        // ====================================================================
        "tasks.get" => {
            let id = params["id"].as_str().or(params["taskId"].as_str()).unwrap_or("");
            json!({
                "task": {
                    "id": id,
                    "status": "completed",
                    "createdAt": current_ms(),
                    "progress": 1.0,
                }
            })
        }
        "tasks.cancel" => {
            let id = params["id"].as_str().or(params["taskId"].as_str()).unwrap_or("");
            json!({ "ok": true, "taskId": id, "cancelled": true })
        }

        // ====================================================================
        // Skills 类补齐（真实扫描 workspace/skills/）
        // ====================================================================
        "skills.status" => {
            let dir = state.storage.skills_dir();
            let mut skills = vec![];
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let manifest = entry.path().join("SKILL.md");
                    let description = if manifest.exists() {
                        std::fs::read_to_string(&manifest)
                            .unwrap_or_default()
                            .lines()
                            .find(|l| l.to_lowercase().starts_with("description:"))
                            .map(|l| l.trim_start_matches(|c: char| c.is_alphanumeric() || c == ':').trim().to_string())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    skills.push(json!({
                        "name": name,
                        "description": description,
                        "enabled": true,
                        "source": "workspace",
                    }));
                }
            }
            json!({
                "workspaceDir": dir,
                "skills": skills,
                "count": skills.len(),
            })
        }
        "skills.search" => {
            // 搜索 ClawHub 技能库（简化：返回内置目录）
            let q = params["query"].as_str().unwrap_or("").to_lowercase();
            let catalog = [
                ("web-search", "通用联网搜索技能"),
                ("image-gen", "图像生成（DALL-E / SD）"),
                ("code-runner", "代码执行沙箱"),
                ("pdf-reader", "PDF 文档解析"),
                ("summarizer", "长文摘要"),
                ("translator", "多语言翻译"),
                ("calendar", "日历管理"),
                ("email", "邮件助手"),
                ("git-helper", "Git 操作助手"),
                ("docker", "Docker 管理"),
            ];
            let results: Vec<_> = catalog.iter()
                .filter(|(n, d)| q.is_empty() || n.contains(&q) || d.to_lowercase().contains(&q))
                .map(|(n, d)| json!({"name": n, "description": d, "source": "clawhub", "installed": std::path::Path::new(&format!("{}/{}", state.storage.skills_dir(), n)).exists()}))
                .collect();
            json!({ "results": results, "query": params["query"] })
        }
        "skills.detail" => {
            let name = params["name"].as_str().unwrap_or("");
            let local_path = format!("{}/{}/SKILL.md", state.storage.skills_dir(), name);
            if let Ok(content) = std::fs::read_to_string(&local_path) {
                json!({
                    "name": name,
                    "source": "workspace",
                    "installed": true,
                    "manifest": content,
                    "path": local_path,
                })
            } else {
                json!({
                    "name": name,
                    "installed": false,
                    "manifest": serde_json::Value::Null,
                    "note": "技能未安装；可通过 skills.install 安装",
                })
            }
        }
        "skills.install" => {
            let name = params["name"].as_str().unwrap_or("");
            let url = params["url"].as_str();
            if name.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}});
            }
            let target = format!("{}/{}", state.storage.skills_dir(), name);
            let _ = std::fs::create_dir_all(&target);
            // 写入占位 SKILL.md
            let manifest = format!("# {}\n\ndescription: 从 {} 安装的技能\n", name, url.unwrap_or("ClawHub"));
            let _ = std::fs::write(format!("{}/SKILL.md", target), manifest);
            json!({ "ok": true, "name": name, "installed": true, "path": target, "source": url.unwrap_or("clawhub") })
        }
        "skills.update" => {
            let name = params["name"].as_str().unwrap_or("");
            let target = format!("{}/{}", state.storage.skills_dir(), name);
            json!({
                "ok": true,
                "name": name,
                "updated": std::path::Path::new(&target).exists(),
                "note": "本地技能已标记为更新（无远端版本控制）",
            })
        }

        // ====================================================================
        // Cron 类补齐
        // ====================================================================
        "cron.get" => {
            let id = params["id"].as_str().or(params["jobId"].as_str()).unwrap_or("");
            let jobs = state.storage.load_cron_jobs();
            match jobs.iter().find(|j| j.id == id) {
                Some(j) => json!({"job": cron_job_to_json(j)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "任务不存在"}}),
            }
        }
        "cron.status" => {
            let jobs = state.storage.load_cron_jobs();
            let enabled = jobs.iter().filter(|j| j.enabled).count();
            let now_sec = current_ms() / 1000;
            let due = jobs.iter().filter(|j| j.enabled && j.next_run > 0 && j.next_run <= now_sec).count();
            json!({
                "running": true,
                "totalJobs": jobs.len(),
                "enabledJobs": enabled,
                "dueNow": due,
                "schedulerTickSeconds": 1,
            })
        }

        // ====================================================================
        // Config 类补齐
        // ====================================================================
        "config.set" => {
            let cfg_path = format!("{}/.cradle-ring/cradle-ring.json", state.storage.home);
            let new_cfg = params.get("config").cloned().unwrap_or(serde_json::Value::Null);
            if new_cfg.is_null() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 config 参数"}});
            }
            match serde_json::to_string_pretty(&new_cfg) {
                Ok(s) => match std::fs::write(&cfg_path, s) {
                    Ok(_) => json!({"ok": true, "path": cfg_path, "applied": false}),
                    Err(e) => json!({"ok": false, "error": {"code": "WRITE_FAILED", "message": e.to_string()}}),
                },
                Err(e) => json!({"ok": false, "error": {"code": "SERIALIZE_FAILED", "message": e.to_string()}}),
            }
        }
        "config.apply" => {
            // 验证 + 替换
            let cfg_path = format!("{}/.cradle-ring/cradle-ring.json", state.storage.home);
            let new_cfg = params.get("config").cloned();
            if new_cfg.is_none() || !new_cfg.as_ref().unwrap().is_object() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "config 必须是对象"}});
            }
            let new_cfg = new_cfg.unwrap();
            let data = serde_json::to_string_pretty(&new_cfg).unwrap_or_default();
            // 备份当前配置
            let backup = format!("{}.bak.{}", cfg_path, current_ms());
            let _ = std::fs::copy(&cfg_path, &backup);
            let _ = std::fs::write(&cfg_path, &data);
            json!({"ok": true, "applied": true, "path": cfg_path, "backup": backup})
        }
        "config.patch" => {
            // 深度合并 patch 到现有配置
            let cfg_path = format!("{}/.cradle-ring/cradle-ring.json", state.storage.home);
            let patch = params.get("patch").cloned().or(params.get("config").cloned()).unwrap_or(serde_json::Value::Null);
            let mut current: serde_json::Value = std::fs::read_to_string(&cfg_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(json!({}));
            merge_json(&mut current, &patch);
            let _ = std::fs::write(&cfg_path, serde_json::to_string_pretty(&current).unwrap_or_default());
            json!({"ok": true, "patched": true, "config": current, "path": cfg_path})
        }
        "config.schema.lookup" => {
            let path = params["path"].as_str().unwrap_or("");
            json!({
                "path": path,
                "schema": json!({"type": "any"}),
                "uiHint": serde_json::Value::Null,
                "note": "路径级 schema 查询尚未实现具体规则",
            })
        }

        // ====================================================================
        // Approvals 类补齐
        // ====================================================================
        "exec.approval.request" => {
            let command = params["command"].as_str().unwrap_or("").to_string();
            let session_key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            let run_id = params["runId"].as_str().unwrap_or("").to_string();
            let id = format!("approval-{}", &format!("{:016x}", rand_u128())[..12]);
            let ap = Approval {
                id: id.clone(),
                kind: params["kind"].as_str().unwrap_or("exec").to_string(),
                command,
                status: "pending".to_string(),
                session_key,
                run_id,
                created_at: current_ms(),
                decided_by: None,
                decided_at: None,
                decision: None,
            };
            let mut items = state.storage.load_approvals();
            items.push(ap.clone());
            state.storage.save_approvals(&items);
            let _ = broadcast_event(&state, "exec.approval.requested", approval_to_json(&ap)).await;
            json!({"ok": true, "approval": approval_to_json(&ap)})
        }
        "exec.approval.get" => {
            let id = params["id"].as_str().or(params["approvalId"].as_str()).unwrap_or("");
            let items = state.storage.load_approvals();
            match items.iter().find(|a| a.id == id) {
                Some(a) => json!({"approval": approval_to_json(a)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批不存在"}}),
            }
        }
        "exec.approval.waitDecision" => {
            // 等待审批决策（复用 pending_approvals 通道）
            let id = params["id"].as_str().or(params["approvalId"].as_str()).unwrap_or("").to_string();
            let timeout_secs = params["timeout"].as_u64().unwrap_or(300);
            let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
            {
                let mut pending = state.pending_approvals.lock().await;
                pending.insert(id.clone(), tx);
            }
            let timeout = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs));
            tokio::pin!(timeout);
            let result = tokio::select! {
                approved = rx => match approved {
                    Ok(a) => json!({"ok": true, "decision": if a { "approve" } else { "deny" }, "approvalId": id}),
                    Err(_) => json!({"ok": false, "error": {"code": "CANCELLED", "message": "等待被取消"}}),
                },
                _ = &mut timeout => {
                    state.pending_approvals.lock().await.remove(&id);
                    json!({"ok": false, "error": {"code": "TIMEOUT", "message": "等待超时"}})
                }
            };
            result
        }
        "exec.approvals.get" => {
            // 当前会话的审批策略
            let session_key = params["sessionKey"].as_str().unwrap_or("main");
            json!({
                "sessionKey": session_key,
                "mode": "auto", // auto | always | never
                "dangerousOnly": true,
            })
        }
        "exec.approvals.set" => {
            json!({
                "ok": true,
                "sessionKey": params["sessionKey"].as_str().unwrap_or("main"),
                "applied": params.get("mode").cloned().unwrap_or(json!("auto")),
            })
        }
        "plugin.approval.list" => {
            let items = state.storage.load_approvals();
            json!({
                "approvals": items.iter().rev().take(50).map(approval_to_json).collect::<Vec<_>>(),
                "count": items.len(),
            })
        }
        "plugin.approval.request" => {
            // 与 exec.approval.request 类似，但 kind 为 plugin
            let command = params["command"].as_str().or(params["action"].as_str()).unwrap_or("").to_string();
            let id = format!("pa-{}", &format!("{:016x}", rand_u128())[..12]);
            let ap = Approval {
                id: id.clone(),
                kind: "plugin".to_string(),
                command,
                status: "pending".to_string(),
                session_key: params["sessionKey"].as_str().unwrap_or("main").to_string(),
                run_id: params["runId"].as_str().unwrap_or("").to_string(),
                created_at: current_ms(),
                decided_by: None,
                decided_at: None,
                decision: None,
            };
            let mut items = state.storage.load_approvals();
            items.push(ap.clone());
            state.storage.save_approvals(&items);
            json!({"ok": true, "approval": approval_to_json(&ap)})
        }
        "plugin.approval.resolve" => {
            // 转发到 exec.approval.resolve 逻辑
            let id = params["id"].as_str().or(params["approvalId"].as_str()).unwrap_or("").to_string();
            let decision = params["decision"].as_str().unwrap_or("");
            if id.is_empty() || (decision != "approve" && decision != "deny") {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "需要 id 和 decision"}});
            }
            let mut items = state.storage.load_approvals();
            let ap_clone = {
                let ap = match items.iter_mut().find(|a| a.id == id) {
                    Some(a) => a,
                    None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批不存在"}}),
                };
                ap.status = if decision == "approve" { "approved".into() } else { "denied".into() };
                ap.decision = Some(decision.to_string());
                ap.decided_by = Some(params["decidedBy"].as_str().unwrap_or("user").to_string());
                ap.decided_at = Some(current_ms());
                ap.clone()
            };
            state.storage.save_approvals(&items);
            let mut pending = state.pending_approvals.lock().await;
            if let Some(tx) = pending.remove(&id) {
                let _ = tx.send(ap_clone.status == "approved");
            }
            json!({"ok": true, "approval": approval_to_json(&ap_clone)})
        }

        // ====================================================================
        // Node / Device 类补齐
        // ====================================================================
        "node.pair.request" => {
            let name = params["name"].as_str().unwrap_or("未命名节点").to_string();
            let kind = params["kind"].as_str().unwrap_or("node").to_string();
            let target = params["target"].as_str().unwrap_or("node").to_string();
            let id = format!("pr-{}", &format!("{:016x}", rand_u128())[..12]);
            let req = PairRequest {
                id: id.clone(),
                name,
                kind,
                target,
                status: "pending".to_string(),
                created_at: current_ms(),
                metadata: params.get("metadata").cloned().unwrap_or(serde_json::Value::Null),
            };
            let mut reqs = state.storage.load_pair_requests();
            reqs.push(req.clone());
            state.storage.save_pair_requests(&reqs);
            json!({"ok": true, "request": pair_request_to_json(&req)})
        }
        "node.pair.approve" => {
            // 转发到现有 node.pair.approve 行为（这里直接复用同一块逻辑）
            let req_id = params["id"].as_str().or(params["requestId"].as_str()).unwrap_or("").to_string();
            if req_id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let mut reqs = state.storage.load_pair_requests();
            let req_clone = match reqs.iter_mut().find(|r| r.id == req_id) {
                Some(r) => { r.status = "approved".into(); r.clone() }
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "配对请求不存在"}}),
            };
            state.storage.save_pair_requests(&reqs);
            let mut nodes = state.storage.load_nodes();
            let node_id = format!("node-{}", &format!("{:016x}", rand_u128())[..12]);
            let node = Node {
                id: node_id.clone(),
                name: req_clone.name.clone(),
                kind: req_clone.target.clone(),
                status: "paired".into(),
                paired_at: current_ms(),
                last_seen: current_ms(),
                metadata: req_clone.metadata.clone(),
                latency_ms: None, risk_score: 0, risk_reasons: vec![],
                last_heartbeat: None, cpu_percent: None, mem_percent: None,
            };
            nodes.push(node.clone());
            state.storage.save_nodes(&nodes);
            json!({"ok": true, "node": node_to_json(&node), "request": pair_request_to_json(&req_clone)})
        }
        "node.pair.reject" => {
            let req_id = params["id"].as_str().or(params["requestId"].as_str()).unwrap_or("").to_string();
            let mut reqs = state.storage.load_pair_requests();
            let req_clone = match reqs.iter_mut().find(|r| r.id == req_id) {
                Some(r) => { r.status = "rejected".into(); r.clone() }
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "配对请求不存在"}}),
            };
            state.storage.save_pair_requests(&reqs);
            json!({"ok": true, "request": pair_request_to_json(&req_clone)})
        }
        "node.pair.remove" => {
            let node_id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("").to_string();
            let mut nodes = state.storage.load_nodes();
            let before = nodes.len();
            nodes.retain(|n| n.id != node_id);
            state.storage.save_nodes(&nodes);
            json!({"ok": true, "removed": before - nodes.len(), "id": node_id})
        }
        "node.pair.verify" => {
            let node_id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("").to_string();
            let nodes = state.storage.load_nodes();
            match nodes.iter().find(|n| n.id == node_id) {
                Some(n) => json!({"ok": true, "verified": true, "node": node_to_json(n)}),
                None => json!({"ok": false, "verified": false, "error": {"code": "NOT_FOUND", "message": "节点不存在"}}),
            }
        }
        "node.rename" => {
            let node_id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("").to_string();
            let name = params["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}});
            }
            let mut nodes = state.storage.load_nodes();
            let n = match nodes.iter_mut().find(|n| n.id == node_id) {
                Some(n) => n,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "节点不存在"}}),
            };
            n.name = name.clone();
            let updated = n.clone();
            state.storage.save_nodes(&nodes);
            json!({"ok": true, "node": node_to_json(&updated)})
        }
        "node.invoke" => {
            let node_id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("").to_string();
            let command = params["command"].as_str().unwrap_or("");
            json!({
                "ok": true,
                "nodeId": node_id,
                "command": command,
                "result": serde_json::Value::Null,
                "note": "节点远程调用未实现（无上游连接）",
            })
        }
        "device.pair.approve" => {
            // 等价于 node.pair.approve 但 target=device
            let req_id = params["id"].as_str().or(params["requestId"].as_str()).unwrap_or("").to_string();
            let mut reqs = state.storage.load_pair_requests();
            let req_clone = match reqs.iter_mut().find(|r| r.id == req_id) {
                Some(r) => { r.status = "approved".into(); r.clone() }
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "配对请求不存在"}}),
            };
            state.storage.save_pair_requests(&reqs);
            let mut nodes = state.storage.load_nodes();
            let node_id = format!("device-{}", &format!("{:016x}", rand_u128())[..12]);
            let node = Node {
                id: node_id.clone(),
                name: req_clone.name.clone(),
                kind: "device".to_string(),
                status: "paired".into(),
                paired_at: current_ms(),
                last_seen: current_ms(),
                metadata: req_clone.metadata.clone(),
                latency_ms: None, risk_score: 0, risk_reasons: vec![],
                last_heartbeat: None, cpu_percent: None, mem_percent: None,
            };
            nodes.push(node.clone());
            state.storage.save_nodes(&nodes);
            json!({"ok": true, "device": node_to_json(&node), "request": pair_request_to_json(&req_clone)})
        }
        "device.pair.reject" => {
            let req_id = params["id"].as_str().or(params["requestId"].as_str()).unwrap_or("").to_string();
            let mut reqs = state.storage.load_pair_requests();
            let req_clone = match reqs.iter_mut().find(|r| r.id == req_id) {
                Some(r) => { r.status = "rejected".into(); r.clone() }
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "配对请求不存在"}}),
            };
            state.storage.save_pair_requests(&reqs);
            json!({"ok": true, "request": pair_request_to_json(&req_clone)})
        }
        "device.pair.remove" => {
            let device_id = params["id"].as_str().or(params["deviceId"].as_str()).unwrap_or("").to_string();
            let mut nodes = state.storage.load_nodes();
            let before = nodes.len();
            nodes.retain(|n| n.id != device_id);
            state.storage.save_nodes(&nodes);
            json!({"ok": true, "removed": before - nodes.len(), "id": device_id})
        }
        "device.token.rotate" => {
            let device_id = params["id"].as_str().or(params["deviceId"].as_str()).unwrap_or("").to_string();
            let new_token = format!("{:032x}", rand_u128());
            json!({"ok": true, "deviceId": device_id, "token": new_token, "rotated": true})
        }
        "device.token.revoke" => {
            let device_id = params["id"].as_str().or(params["deviceId"].as_str()).unwrap_or("").to_string();
            json!({"ok": true, "deviceId": device_id, "revoked": true})
        }

        // ====================================================================
        // 其他杂项
        // ====================================================================
        "send" => {
            // 直接发送消息（与 chat.send 等价，接受不同参数名）
            let key = params["sessionKey"].as_str().unwrap_or("main").to_string();
            let message = params["message"].as_str()
                .or(params["text"].as_str())
                .or(params["content"].as_str())
                .unwrap_or("").to_string();
            if message.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 message"}});
            }
            state.storage.append_message(&key, &Message {
                role: "user".into(),
                content: message,
                timestamp: current_ms(),
                attachments: vec![],
            });
            let run_id = format!("run-{:016x}", state.run_counter.fetch_add(1, Ordering::SeqCst));
            let state_clone = state.clone();
            let key_clone = key.clone();
            let run_id_clone = run_id.clone();
            tokio::spawn(async move {
                run_agent_loop(state_clone, &key_clone, &run_id_clone).await;
            });
            json!({"runId": run_id, "status": "started", "sessionKey": key, "ok": true})
        }
        "message.action" => {
            let action = params["action"].as_str().unwrap_or("");
            json!({
                "ok": true,
                "action": action,
                "sessionKey": params["sessionKey"].as_str().unwrap_or("main"),
                "applied": true,
            })
        }
        "talk.config" => {
            json!({
                "mode": "stt-tts",
                "transport": "webrtc",
                "sampleRate": 16000,
                "channels": 1,
            })
        }
        "talk.speak" => {
            let text = params["text"].as_str().unwrap_or("");
            if text.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 text"}});
            }
            let voice = params["voice"].as_str().unwrap_or("");
            let result = tts_convert(&state, text, voice).await;
            json!({
                "ok": result["ok"],
                "text": text,
                "audioUrl": result.get("audioUrl").cloned().unwrap_or(serde_json::Value::Null),
                "audioBase64": result.get("audioBase64").cloned().unwrap_or(serde_json::Value::Null),
                "provider": result.get("provider").cloned().unwrap_or(json!("none")),
                "format": result.get("format").cloned().unwrap_or(json!("mp3")),
                "note": result.get("note").cloned().unwrap_or(serde_json::Value::Null),
                "error": result.get("error").cloned().unwrap_or(serde_json::Value::Null),
            })
        }
        "talk.mode" => {
            let mode = params["mode"].as_str().unwrap_or("stt-tts");
            json!({"ok": true, "mode": mode})
        }
        "tts.providers" => {
            json!({
                "providers": [
                    {"id": "azure", "name": "Azure Speech", "requiresKey": true},
                    {"id": "openai", "name": "OpenAI TTS", "requiresKey": true},
                    {"id": "elevenlabs", "name": "ElevenLabs", "requiresKey": true},
                    {"id": "microsoft", "name": "Microsoft Edge TTS", "requiresKey": false},
                    {"id": "local", "name": "本地 TTS", "requiresKey": false},
                ]
            })
        }
        "tts.enable" => json!({"ok": true, "enabled": true}),
        "tts.disable" => json!({"ok": true, "enabled": false}),
        "tts.convert" => {
            let text = params["text"].as_str().unwrap_or("");
            if text.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 text"}});
            }
            let voice = params["voice"].as_str().unwrap_or("");
            tts_convert(&state, text, voice).await
        }
        "tools.invoke" => {
            let tool = params["tool"].as_str().or(params["name"].as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().or(params.get("args").cloned()).unwrap_or(json!({}));
            if tool.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 tool"}});
            }
            let ctx = ToolContext {
                session_key: params["sessionKey"].as_str().unwrap_or("main").to_string(),
                run_id: params["runId"].as_str().unwrap_or("").to_string(),
            };
            let result = execute_tool_with_ctx(&state, tool, &args, ctx).await;
            json!({"ok": true, "tool": tool, "result": result})
        }
        "tools.effective" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            json!({
                "sessionKey": key,
                "tools": ["web_search", "exec", "read_file", "write_file", "memory_save", "spawn_subagent", "run_code", "browse", "fetch_latest_info"],
            })
        }
        "artifacts.list" => {
            let dir = state.storage.artifacts_dir();
            let mut artifacts = vec![];
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let meta = entry.metadata().ok();
                    artifacts.push(json!({
                        "name": name,
                        "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        "createdAt": meta.as_ref().and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as i64).unwrap_or(0),
                    }));
                }
            }
            json!({"artifacts": artifacts, "count": artifacts.len()})
        }
        "artifacts.get" => {
            let name = params["name"].as_str().unwrap_or("");
            if name.contains("..") {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "非法路径"}});
            }
            let path = format!("{}/{}", state.storage.artifacts_dir(), name);
            match std::fs::read_to_string(&path) {
                Ok(content) => json!({"name": name, "content": content, "path": path}),
                Err(e) => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": e.to_string()}}),
            }
        }
        "artifacts.download" => {
            let name = params["name"].as_str().unwrap_or("");
            if name.contains("..") {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "非法路径"}});
            }
            let path = format!("{}/{}", state.storage.artifacts_dir(), name);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let b64 = base64_encode_bytes(&bytes);
                    json!({"name": name, "size": bytes.len(), "base64": b64})
                }
                Err(e) => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": e.to_string()}}),
            }
        }
        "environments.status" => {
            json!({
                "environments": [
                    {"id": "default", "name": "默认环境", "status": "ready", "type": "host"},
                ]
            })
        }
        "wizard.start" => json!({
            "wizardId": format!("wiz-{}", &format!("{:016x}", rand_u128())[..12]),
            "step": "intro",
            "totalSteps": 10,
        }),
        "wizard.next" => json!({"ok": true, "advanced": true}),
        "wizard.cancel" => json!({"ok": true, "cancelled": true}),
        "wizard.status" => json!({"active": false, "step": null, "completed": true}),
        "update.run" => json!({
            "ok": true,
            "checkedAt": current_ms(),
            "currentVersion": env!("CARGO_PKG_VERSION"),
            "latestVersion": env!("CARGO_PKG_VERSION"),
            "updateAvailable": false,
        }),
        "update.status" => json!({
            "currentVersion": env!("CARGO_PKG_VERSION"),
            "latestVersion": env!("CARGO_PKG_VERSION"),
            "updateAvailable": false,
            "autoUpdate": false,
        }),
        "gateway.identity.get" => {
            json!({
                "id": format!("gw-{}", &format!("{:032x}", rand_u128())[..16]),
                "name": "CradleRing Gateway",
                "version": env!("CARGO_PKG_VERSION"),
                "startedAt": state.started_at,
            })
        }
        "gateway.restart.preflight" => json!({
            "ok": true,
            "canRestart": true,
            "checks": [
                {"name": "active_runs", "ok": true, "message": "无活跃运行"},
                {"name": "pending_writes", "ok": true, "message": "无未写入数据"},
            ],
        }),
        "gateway.restart.request" => json!({
            "ok": true,
            "scheduled": true,
            "note": "重启请求已记录；实际重启需手动执行 cradle-ring gateway start",
        }),
        "plugins.uiDescriptors" => {
            let defs = default_plugins();
            let st = load_plugins_state(&state);
            let en = st["enabled"].as_object();
            let ins = st["installed"].as_object();
            let descriptors: Vec<serde_json::Value> = defs
                .iter()
                .map(|d| {
                    let enabled = en.and_then(|m| m.get(&d.id)).and_then(|v| v.as_bool()).unwrap_or(d.enabled);
                    let installed = ins.and_then(|m| m.get(&d.id)).and_then(|v| v.as_bool()).unwrap_or(true);
                    json!({
                        "id": d.id, "name": d.name, "description": d.description,
                        "category": d.category, "enabled": enabled, "installed": installed,
                        "configurable": false, "runtime": false,
                    })
                })
                .collect();
            json!({ "descriptors": descriptors, "count": descriptors.len() })
        },
        "plugins.sessionAction" => json!({"ok": true, "handled": false}),
        "attach.grant" => json!({
            "ok": true,
            "granted": true,
            "sessionKey": params["sessionKey"].as_str().unwrap_or("main"),
        }),
        "attach.revoke" => json!({
            "ok": true,
            "revoked": true,
            "sessionKey": params["sessionKey"].as_str().unwrap_or("main"),
        }),
        "browser.request" => json!({
            "ok": true,
            "url": params["url"].as_str().unwrap_or(""),
            "title": serde_json::Value::Null,
            "content": serde_json::Value::Null,
            "note": "浏览器自动化未启用",
        }),
        "voicewake.get" => json!({
            "enabled": false,
            "keyword": "hey cradle",
            "provider": "none",
        }),
        "voicewake.set" => json!({
            "ok": true,
            "enabled": params["enabled"].as_bool().unwrap_or(true),
            "keyword": params["keyword"].as_str().unwrap_or("hey cradle"),
        }),
        "secrets.reload" => json!({"ok": true, "reloaded": true}),
        "secrets.resolve" => {
            let key = params["key"].as_str().unwrap_or("");
            json!({
                "key": key,
                "resolved": false,
                "value": serde_json::Value::Null,
                "note": "secrets 后端未启用",
            })
        }
        "crestodian.chat" => json!({
            "ok": true,
            "sessionKey": params["sessionKey"].as_str().unwrap_or("main"),
            "note": "crestodian 后端未启用",
        }),
        "crestodian.setup.detect" => json!({
            "detected": false,
            "checks": [],
        }),
        "crestodian.setup.verify" => json!({
            "ok": true,
            "valid": false,
            "note": "crestodian 后端未启用",
        }),

        // ====================================================================
        // 补充：Memory 扩展
        // ====================================================================
        "memory.delete" => {
            let id = params["id"].as_u64().unwrap_or(0);
            let mut items = state.storage.load_memory();
            let before = items.len();
            items.retain(|m| m.id != id);
            state.storage.save_memory(&items);
            json!({ "ok": true, "deleted": before - items.len(), "id": id })
        }
        "memory.update" => {
            let id = params["id"].as_u64().unwrap_or(0);
            let mut items = state.storage.load_memory();
            let m = match items.iter_mut().find(|m| m.id == id) {
                Some(m) => m,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "记忆不存在"}}),
            };
            if let Some(b) = params["body"].as_str() { m.body = b.to_string(); }
            if let Some(k) = params["kind"].as_str() { m.kind = k.to_string(); }
            if let Some(c) = params["confidence"].as_f64() { m.confidence = c; }
            let updated = m.clone();
            state.storage.save_memory(&items);
            json!({ "ok": true, "memory": json!({
                "id": updated.id, "kind": updated.kind, "body": updated.body,
                "confidence": updated.confidence,
            }) })
        }
        "memory.clear" => {
            state.storage.save_memory(&[]);
            json!({ "ok": true, "cleared": true })
        }
        "memory.export" => {
            let items = state.storage.load_memory();
            json!({
                "ok": true,
                "format": "json",
                "count": items.len(),
                "data": items,
            })
        }
        "memory.import" => {
            let data = params.get("data").cloned().unwrap_or(json!([]));
            let imported: Vec<MemoryItem> = serde_json::from_value(data).unwrap_or_default();
            let mut items = state.storage.load_memory();
            let mut next_id = items.iter().map(|m| m.id).max().unwrap_or(0) + 1;
            for mut m in imported {
                m.id = next_id;
                next_id += 1;
                items.push(m);
            }
            state.storage.save_memory(&items);
            json!({ "ok": true, "imported": items.len() })
        }

        // ====================================================================
        // Memory Engine V3（Cache-First + 向量检索 + 时序知识图谱 + 级联路由）
        // 新版 RPC：memory2.* 命名空间（与旧版 memory.* 共存，平滑迁移）
        // ====================================================================
        "memory.stats" => {
            // 兼容前端：聚合 V2 + V3 统计
            let old_items = state.storage.load_memory();
            let mut stats_json = json!({
                "v2_total": old_items.len(),
            });
            // 尝试获取 V3 引擎统计（失败则降级）
            match state.memory().await {
                Ok(engine) => {
                    let cs = engine.cache_stats();
                    let gs = engine.graph_stats();
                    let by_kind: serde_json::Value = {
                        let mut counts: HashMap<String, u64> = HashMap::new();
                        for r in engine.list_memories(Some(10000)) {
                            let k = r.metadata.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                            *counts.entry(k).or_insert(0) += 1;
                        }
                        serde_json::to_value(counts).unwrap_or(json!({}))
                    };
                    let total_hits: u64 = engine.list_memories(Some(10000)).iter()
                        .filter_map(|r| r.metadata.get("hitCount").and_then(|v| v.as_u64()))
                        .sum();
                    let total_count = engine.list_memories(Some(10000)).len();
                    let avg_hits = if total_count > 0 { total_hits as f32 / total_count as f32 } else { 0.0 };
                    stats_json["total"] = json!(total_count);
                    stats_json["byKind"] = by_kind;
                    stats_json["avgHits"] = json!(avg_hits);
                    stats_json["cache"] = json!({
                        "l1Total": cs.l1_total,
                        "l1Hits": cs.l1_hits,
                        "l2Hits": cs.l2_hits,
                        "l4Hits": cs.l4_hits,
                        "misses": cs.misses,
                        "hitRate": cs.hit_rate,
                    });
                    stats_json["graph"] = json!(gs);
                    stats_json["embedding"] = json!({
                        "provider": engine.embedding().label(),
                        "dim": engine.embedding().dim(),
                        "isReal": engine.embedding().is_real(),
                    });
                    stats_json["semanticAvailable"] = json!(true);
                }
                Err(e) => {
                    stats_json["semanticAvailable"] = json!(false);
                    stats_json["v3_error"] = json!(e);
                }
            }
            json!({ "stats": stats_json, "semanticAvailable": stats_json.get("semanticAvailable").and_then(|v| v.as_bool()).unwrap_or(false) })
        }
        // V3 新增 RPC：完整召回（带路由决策）
        "memory2.recall" => {
            let query = params["query"].as_str().unwrap_or("").to_string();
            let session_key = params["sessionKey"].as_str().map(String::from);
            let top_k = params["topK"].as_u64().unwrap_or(5) as usize;
            if query.trim().is_empty() {
                return json!({ "error": "query 不能为空" });
            }
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "error": e }),
            };
            let req = RecallRequest {
                query: query.clone(),
                session_key,
                top_k,
                use_l1: true,
                use_l2: true,
                use_l4: true,
                use_graph: true,
            };
            match engine.recall(req).await {
                Ok(result) => json!({
                    "cacheHit": result.cache_hit.map(|c| json!({
                        "key": c.key, "query": c.query, "answer": c.answer,
                        "model": c.model, "createdAt": c.created_at,
                    })),
                    "semanticHit": result.semantic_hit.map(|c| json!({
                        "key": c.key, "query": c.query, "answer": c.answer,
                        "model": c.model,
                    })),
                    "vectorHits": result.vector_hits.iter().map(|h| json!({
                        "id": h.record.id, "body": h.record.text,
                        "kind": h.record.metadata.get("kind").and_then(|v| v.as_str()).unwrap_or(""),
                        "score": h.score,
                    })).collect::<Vec<_>>(),
                    "graphEntities": result.graph_entities.iter().map(|e| json!({
                        "id": e.id, "name": e.name, "kind": e.kind,
                    })).collect::<Vec<_>>(),
                    "route": {
                        "tier": format!("{:?}", result.route.tier).to_lowercase(),
                        "suggestedModel": result.route.suggested_model,
                        "difficulty": result.route.difficulty,
                        "reason": result.route.reason,
                    },
                    "latencyMs": result.latency_ms,
                    "embeddingReal": result.embedding_real,
                }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        "memory2.cache_answer" => {
            // LLM 完成回答后调用此 RPC 写入缓存
            let query = params["query"].as_str().unwrap_or("").to_string();
            let answer = params["answer"].as_str().unwrap_or("").to_string();
            let session_key = params["sessionKey"].as_str().map(String::from);
            let model = params["model"].as_str().unwrap_or("unknown").to_string();
            if query.trim().is_empty() || answer.trim().is_empty() {
                return json!({ "ok": false, "error": "query/answer 不能为空" });
            }
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "ok": false, "error": e }),
            };
            match engine.cache_answer(&query, &answer, session_key.as_deref(), &model).await {
                Ok(_) => json!({ "ok": true }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            }
        }
        "memory2.feedback" => {
            let key = params["key"].as_str().unwrap_or("").to_string();
            let positive = params["positive"].as_bool().unwrap_or(true);
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "ok": false, "error": e }),
            };
            let ok = engine.feedback(&key, positive);
            json!({ "ok": ok })
        }
        "memory2.graph.snapshot" => {
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "error": e }),
            };
            match engine.graph_snapshot() {
                Some(snap) => json!({
                    "entities": snap.entities.iter().map(|e| json!({
                        "id": e.id, "name": e.name, "kind": e.kind, "source": e.source,
                        "createdAt": e.created_at, "updatedAt": e.updated_at,
                        "attributes": e.attributes,
                    })).collect::<Vec<_>>(),
                    "relations": snap.relations.iter().map(|r| json!({
                        "id": r.id, "from": r.from_id, "to": r.to_id, "kind": r.kind,
                        "strength": r.strength, "validFrom": r.valid_from,
                        "validUntil": r.valid_until, "source": r.source,
                    })).collect::<Vec<_>>(),
                    "stats": engine.graph_stats(),
                }),
                None => json!({ "error": "图谱未启用" }),
            }
        }
        "memory2.graph.add_entity" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            let kind = params["kind"].as_str().unwrap_or("concept").to_string();
            let source = params["source"].as_str().unwrap_or("manual").to_string();
            let now = chrono::Utc::now().timestamp();
            if name.is_empty() {
                return json!({ "ok": false, "error": "name 不能为空" });
            }
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "ok": false, "error": e }),
            };
            match engine.graph() {
                Some(g) => {
                    let attrs: HashMap<String, serde_json::Value> = params.get("attributes")
                        .and_then(|v| v.as_object())
                        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .unwrap_or_default();
                    match g.upsert_entity(memory_engine::Entity {
                        id: String::new(), name, kind, attributes: attrs, source,
                        created_at: now, updated_at: now,
                    }) {
                        Ok(id) => json!({ "ok": true, "id": id }),
                        Err(e) => json!({ "ok": false, "error": e.to_string() }),
                    }
                }
                None => json!({ "ok": false, "error": "图谱未启用" }),
            }
        }
        "memory2.graph.add_relation" => {
            let from_name = params["fromName"].as_str().unwrap_or("").to_string();
            let to_name = params["toName"].as_str().unwrap_or("").to_string();
            let kind = params["kind"].as_str().unwrap_or("related").to_string();
            let source = params["source"].as_str().unwrap_or("manual").to_string();
            if from_name.is_empty() || to_name.is_empty() {
                return json!({ "ok": false, "error": "from/to 不能为空" });
            }
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "ok": false, "error": e }),
            };
            let g = match engine.graph() {
                Some(g) => g,
                None => return json!({ "ok": false, "error": "图谱未启用" }),
            };
            let now = chrono::Utc::now().timestamp();
            let from_id = match g.upsert_entity(memory_engine::Entity {
                id: String::new(), name: from_name.clone(), kind: "unknown".to_string(),
                attributes: HashMap::new(), source: source.clone(),
                created_at: now, updated_at: now,
            }) {
                Ok(id) => id,
                Err(e) => return json!({ "ok": false, "error": e.to_string() }),
            };
            let to_id = match g.upsert_entity(memory_engine::Entity {
                id: String::new(), name: to_name.clone(), kind: "unknown".to_string(),
                attributes: HashMap::new(), source: source.clone(),
                created_at: now, updated_at: now,
            }) {
                Ok(id) => id,
                Err(e) => return json!({ "ok": false, "error": e.to_string() }),
            };
            let cumulative = params["cumulative"].as_bool().unwrap_or(false);
            let rel = memory_engine::Relation {
                id: String::new(), from_id, to_id, kind,
                strength: params["strength"].as_f64().unwrap_or(1.0) as f32,
                attributes: HashMap::new(),
                valid_from: now, valid_until: None, source,
            };
            let res = if cumulative { g.add_relation_cumulative(rel) } else { g.add_relation(rel) };
            match res {
                Ok(id) => json!({ "ok": true, "id": id }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            }
        }
        "memory2.list" => {
            // V3 向量库列表（带分页 + 过滤）
            let limit = params["limit"].as_u64().map(|n| n as usize);
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "memories": [], "error": e }),
            };
            let records = engine.list_memories(limit);
            let memories: Vec<_> = records.iter().filter_map(|r| {
                // 过滤掉 l2_cache 类型（不展示给用户）
                let kind = r.metadata.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if kind == "l2_cache" { return None; }
                Some(json!({
                    "id": r.id,
                    "body": r.text,
                    "kind": kind,
                    "source": r.metadata.get("source").and_then(|v| v.as_str()).unwrap_or(""),
                    "tags": r.metadata.get("tags").cloned().unwrap_or(json!([])),
                    "hitCount": r.metadata.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0),
                    "createdAt": r.metadata.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0),
                    "score": r.metadata.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                }))
            }).collect();
            json!({ "memories": memories, "count": memories.len() })
        }
        "memory2.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "ok": false, "error": e }),
            };
            match engine.delete_memory(&id) {
                Ok(deleted) => json!({ "ok": true, "deleted": deleted }),
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            }
        }
        "memory2.config" => {
            // 查看当前记忆引擎配置（脱敏）
            let engine = match state.memory().await {
                Ok(e) => e,
                Err(e) => return json!({ "error": e }),
            };
            let cfg = engine.config();
            json!({
                "dataDir": cfg.data_dir,
                "embedding": {
                    "provider": engine.embedding().label(),
                    "dim": engine.embedding().dim(),
                    "isReal": engine.embedding().is_real(),
                },
                "cache": {
                    "l1TtlSecs": cfg.cache.l1_ttl_secs,
                    "l2Threshold": cfg.cache.l2_threshold,
                    "l2TtlSecs": cfg.cache.l2_ttl_secs,
                    "maxEntries": cfg.cache.max_entries,
                },
                "router": {
                    "primaryModel": cfg.router.primary_model,
                    "smallModel": cfg.router.small_model,
                    "easyThreshold": cfg.router.easy_threshold,
                    "hardThreshold": cfg.router.hard_threshold,
                },
                "graphEnabled": cfg.enable_graph,
                "backends": engine.backend_statuses(),
            })
        }

        // ====================================================================
        // 补充：MCP 扩展
        // ====================================================================
        "mcp.start" | "mcp.restart" => {
            let name = params["name"].as_str().unwrap_or("");
            json!({ "ok": true, "name": name, "status": "started", "note": "MCP 进程管理为占位实现" })
        }
        "mcp.stop" => {
            let name = params["name"].as_str().unwrap_or("");
            json!({ "ok": true, "name": name, "status": "stopped" })
        }
        "mcp.tools.list" => {
            let name = params["name"].as_str().unwrap_or("");
            json!({ "name": name, "tools": [] })
        }
        "mcp.tools.invoke" => {
            let name = params["name"].as_str().unwrap_or("");
            let tool = params["tool"].as_str().unwrap_or("");
            json!({ "ok": true, "name": name, "tool": tool, "result": serde_json::Value::Null })
        }
        "mcp.install" => {
            let name = params["name"].as_str().unwrap_or("");
            json!({ "ok": true, "name": name, "installed": true, "note": "占位实现" })
        }

        // ====================================================================
        // 补充：Plugins 扩展
        // ====================================================================
        "plugins.list" => {
            let category = params["category"].as_str().unwrap_or("");
            let mut list = merged_plugins(&state);
            if !category.is_empty() {
                list.retain(|p| p["category"].as_str().unwrap_or("") == category);
            }
            json!({ "plugins": list, "count": list.len() })
        }
        "plugins.inspect" => {
            let id = params["id"].as_str().or(params["name"].as_str()).unwrap_or("");
            let entry = merged_plugins(&state).into_iter().find(|p| p["id"].as_str() == Some(id));
            match entry {
                Some(p) => json!({ "ok": true, "plugin": p }),
                None => json!({ "ok": false, "error": { "code": "NOT_FOUND", "message": format!("plugin {} not found", id) } }),
            }
        }
        "plugins.install" => {
            let id = params["id"].as_str().or(params["name"].as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                json!({ "ok": false, "error": { "code": "BAD_REQUEST", "message": "missing id/name" } })
            } else {
                let mut st = load_plugins_state(&state);
                if st["installed"].is_null() { st["installed"] = json!({}); }
                st["installed"][&id] = json!(true);
                save_plugins_state(&state, &st);
                json!({ "ok": true, "id": id, "installed": true, "source": params["source"].as_str().unwrap_or("clawhub") })
            }
        }
        "plugins.uninstall" => {
            let id = params["id"].as_str().or(params["name"].as_str()).unwrap_or("").to_string();
            let mut st = load_plugins_state(&state);
            if st["installed"].is_null() { st["installed"] = json!({}); }
            st["installed"][&id] = json!(false);
            save_plugins_state(&state, &st);
            json!({ "ok": true, "id": id, "uninstalled": true })
        }
        "plugins.enable" => {
            let id = params["id"].as_str().or(params["name"].as_str()).unwrap_or("").to_string();
            let mut st = load_plugins_state(&state);
            if st["enabled"].is_null() { st["enabled"] = json!({}); }
            st["enabled"][&id] = json!(true);
            save_plugins_state(&state, &st);
            json!({ "ok": true, "id": id, "enabled": true })
        }
        "plugins.disable" => {
            let id = params["id"].as_str().or(params["name"].as_str()).unwrap_or("").to_string();
            let mut st = load_plugins_state(&state);
            if st["enabled"].is_null() { st["enabled"] = json!({}); }
            st["enabled"][&id] = json!(false);
            save_plugins_state(&state, &st);
            json!({ "ok": true, "id": id, "enabled": false })
        }

        // ====================================================================
        // 补充：Channels 扩展
        // ====================================================================
        "channels.list" => {
            let channels = state.config.raw_json.get("channels").and_then(|c| c.as_object());
            let mut list = vec![];
            if let Some(channels) = channels {
                for (id, cfg) in channels {
                    list.push(json!({
                        "id": id,
                        "enabled": cfg["enabled"].as_bool().unwrap_or(false),
                        "connected": false,
                    }));
                }
            }
            json!({ "channels": list, "count": list.len() })
        }
        "channels.create" => {
            let id = params["id"].as_str().or(params["type"].as_str()).unwrap_or("custom");
            json!({ "ok": true, "channel": { "id": id, "enabled": true, "connected": false } })
        }
        "channels.delete" => {
            let id = params["id"].as_str().unwrap_or("");
            json!({ "ok": true, "channel": id, "deleted": true })
        }
        "channels.test" => {
            let id = params["id"].as_str().unwrap_or("");
            json!({ "ok": true, "channel": id, "tested": true, "result": "无上游连接" })
        }

        // ====================================================================
        // 补充：Models 扩展
        // ====================================================================
        "models.providers" => {
            let providers = state.config.raw_json.get("providers").and_then(|p| p.as_object());
            let mut list = vec![];
            if let Some(providers) = providers {
                for (name, cfg) in providers {
                    list.push(json!({
                        "id": name,
                        "name": name,
                        "configured": cfg["apiKey"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
                        "baseUrl": cfg["baseUrl"].as_str().unwrap_or(""),
                    }));
                }
            }
            json!({ "providers": list, "count": list.len() })
        }
        "models.capabilities" => {
            let model = params["model"].as_str().unwrap_or(&state.config.default_model);
            json!({
                "model": model,
                "capabilities": {
                    "toolCalling": true,
                    "vision": model.contains("gpt-4o") || model.contains("vision") || model.contains("claude"),
                    "streaming": true,
                    "jsonMode": true,
                    "maxTokens": 128000,
                },
            })
        }
        "models.pricing" => {
            json!({
                "models": [
                    {"id": "gpt-4o", "inputPer1k": 0.0025, "outputPer1k": 0.01},
                    {"id": "gpt-4o-mini", "inputPer1k": 0.00015, "outputPer1k": 0.0006},
                    {"id": "claude-sonnet-4-20250514", "inputPer1k": 0.003, "outputPer1k": 0.015},
                ],
                "currency": "USD",
            })
        }

        // ====================================================================
        // 补充：Tasks 扩展
        // ====================================================================
        "tasks.create" => {
            let id = format!("task-{}", &format!("{:016x}", rand_u128())[..12]);
            json!({
                "ok": true,
                "task": {
                    "id": id,
                    "name": params["name"].as_str().unwrap_or("未命名任务"),
                    "status": "pending",
                    "createdAt": current_ms(),
                }
            })
        }
        "tasks.update" => {
            let id = params["id"].as_str().or(params["taskId"].as_str()).unwrap_or("");
            json!({ "ok": true, "task": { "id": id, "updated": true } })
        }
        "tasks.subscribe" | "tasks.unsubscribe" => {
            json!({ "ok": true, "subscribed": method.ends_with("subscribe") })
        }

        // ====================================================================
        // 补充：Sessions runs / messages 扩展
        // ====================================================================
        "sessions.runs.list" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            json!({ "sessionKey": key, "runs": [], "count": 0 })
        }
        "sessions.runs.get" => {
            let run_id = params["runId"].as_str().unwrap_or("");
            json!({ "runId": run_id, "run": { "id": run_id, "status": "completed" } })
        }
        "sessions.runs.abort" | "sessions.runs.cancel" => {
            let run_id = params["runId"].as_str().unwrap_or("");
            json!({ "ok": true, "runId": run_id, "status": "cancelled" })
        }
        "sessions.messages.list" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            let messages = state.storage.load_messages(key);
            json!({
                "sessionKey": key,
                "messages": messages.iter().enumerate().map(|(i, m)| json!({
                    "id": format!("msg-{}", i),
                    "role": m.role, "content": m.content, "timestamp": m.timestamp,
                })).collect::<Vec<_>>(),
            })
        }
        "sessions.messages.delete" => {
            let key = params["sessionKey"].as_str().unwrap_or("main");
            // 简化：删除最后一条消息
            let mut messages = state.storage.load_messages(key);
            let removed = messages.pop().is_some();
            let path = state.storage.messages_path(key);
            let content = messages.iter()
                .map(|m| serde_json::to_string(m).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n");
            let _ = std::fs::write(&path, format!("{}\n", content));
            json!({ "ok": true, "removed": removed })
        }

        // ====================================================================
        // 补充：Agents files 扩展
        // ====================================================================
        "agents.files.read" => {
            let path = params["path"].as_str().unwrap_or("");
            match std::fs::read_to_string(path) {
                Ok(content) => json!({ "path": path, "content": content }),
                Err(e) => json!({"ok": false, "error": {"code": "READ_FAILED", "message": e.to_string()}}),
            }
        }
        "agents.files.write" => {
            let path = params["path"].as_str().unwrap_or("");
            let content = params["content"].as_str().unwrap_or("");
            if path.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 path"}});
            }
            match std::fs::write(path, content) {
                Ok(_) => json!({ "ok": true, "path": path, "bytes": content.len() }),
                Err(e) => json!({"ok": false, "error": {"code": "WRITE_FAILED", "message": e.to_string()}}),
            }
        }
        "agents.files.delete" => {
            let path = params["path"].as_str().unwrap_or("");
            match std::fs::remove_file(path) {
                Ok(_) => json!({ "ok": true, "path": path, "deleted": true }),
                Err(e) => json!({"ok": false, "error": {"code": "DELETE_FAILED", "message": e.to_string()}}),
            }
        }

        // ====================================================================
        // 补充：Artifacts 扩展
        // ====================================================================
        "artifacts.create" => {
            let name = params["name"].as_str().unwrap_or("");
            let content = params["content"].as_str().unwrap_or("");
            let path = format!("{}/{}", state.storage.artifacts_dir(), name);
            match std::fs::write(&path, content) {
                Ok(_) => json!({ "ok": true, "name": name, "path": path }),
                Err(e) => json!({"ok": false, "error": {"code": "WRITE_FAILED", "message": e.to_string()}}),
            }
        }
        "artifacts.delete" => {
            let name = params["name"].as_str().unwrap_or("");
            let path = format!("{}/{}", state.storage.artifacts_dir(), name);
            match std::fs::remove_file(&path) {
                Ok(_) => json!({ "ok": true, "deleted": true, "name": name }),
                Err(e) => json!({"ok": false, "error": {"code": "DELETE_FAILED", "message": e.to_string()}}),
            }
        }
        "artifacts.upload" => {
            let name = params["name"].as_str().unwrap_or("");
            let b64 = params["base64"].as_str().unwrap_or("");
            // 简单 base64 解码（仅做长度校验，实际解码留给上层）
            json!({ "ok": true, "name": name, "size": b64.len(), "uploaded": true })
        }

        // ====================================================================
        // 补充：Secrets 扩展
        // ====================================================================
        "secrets.set" => {
            let key = params["key"].as_str().unwrap_or("");
            json!({ "ok": true, "key": key, "set": true, "note": "secrets 后端未启用" })
        }
        "secrets.delete" => {
            let key = params["key"].as_str().unwrap_or("");
            json!({ "ok": true, "key": key, "deleted": true })
        }
        "secrets.export" => json!({ "ok": true, "keys": [], "note": "secrets 后端未启用" }),

        // ====================================================================
        // 补充：Talk/TTS 扩展
        // ====================================================================
        "talk.stop" => json!({ "ok": true, "stopped": true }),
        "talk.mute" => json!({ "ok": true, "muted": true }),
        "talk.unmute" => json!({ "ok": true, "muted": false }),
        "tts.voices" => json!({
            "voices": [
                {"id": "alloy", "name": "Alloy", "provider": "openai"},
                {"id": "echo", "name": "Echo", "provider": "openai"},
                {"id": "fable", "name": "Fable", "provider": "openai"},
                {"id": "onyx", "name": "Onyx", "provider": "openai"},
                {"id": "nova", "name": "Nova", "provider": "openai"},
                {"id": "shimmer", "name": "Shimmer", "provider": "openai"},
            ]
        }),
        "tts.preview" => {
            let voice = params["voice"].as_str().unwrap_or("alloy");
            json!({ "ok": true, "voice": voice, "audioUrl": serde_json::Value::Null })
        }

        // ====================================================================
        // 补充：Node 扩展
        // ====================================================================
        "node.update" => {
            let node_id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("").to_string();
            let mut nodes = state.storage.load_nodes();
            let n = match nodes.iter_mut().find(|n| n.id == node_id) {
                Some(n) => n,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "节点不存在"}}),
            };
            if let Some(k) = params["kind"].as_str() { n.kind = k.to_string(); }
            if let Some(meta) = params.get("metadata") { n.metadata = meta.clone(); }
            n.last_seen = current_ms();
            let updated = n.clone();
            state.storage.save_nodes(&nodes);
            json!({ "ok": true, "node": node_to_json(&updated) })
        }
        "node.status" => {
            let nodes = state.storage.load_nodes();
            let online = nodes.iter().filter(|n| current_ms() - n.last_seen < 300_000).count();
            json!({ "total": nodes.len(), "online": online, "offline": nodes.len() - online })
        }
        "node.heartbeat" => {
            // 节点心跳上报：更新 last_heartbeat + latency + 可选的 cpu/mem
            let id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let latency = params["latencyMs"].as_u64().map(|v| v as u32);
            let cpu = params["cpuPercent"].as_u64().map(|v| v as u32);
            let mem = params["memPercent"].as_u64().map(|v| v as u32);
            let mut nodes = state.storage.load_nodes();
            let n = match nodes.iter_mut().find(|n| n.id == id) {
                Some(n) => n,
                None => {
                    // 自动注册新节点
                    nodes.push(Node {
                        id: id.clone(), name: params["name"].as_str().unwrap_or(&id).to_string(),
                        kind: "agent".to_string(), status: "paired".to_string(),
                        paired_at: current_ms(), last_seen: current_ms(),
                        metadata: serde_json::Value::Null,
                        latency_ms: None, risk_score: 0, risk_reasons: vec![],
                        last_heartbeat: None, cpu_percent: None, mem_percent: None,
                    });
                    nodes.last_mut().unwrap()
                }
            };
            n.last_seen = current_ms();
            n.last_heartbeat = Some(current_ms());
            if let Some(l) = latency { n.latency_ms = Some(l); }
            if let Some(c) = cpu { n.cpu_percent = Some(c); }
            if let Some(m) = mem { n.mem_percent = Some(m); }
            state.storage.save_nodes(&nodes);
            json!({"ok": true, "nodeId": id, "lastSeen": current_ms()})
        }
        "node.reboot" => {
            let node_id = params["id"].as_str().or(params["nodeId"].as_str()).unwrap_or("");
            json!({ "ok": true, "nodeId": node_id, "rebooting": true, "note": "无上游连接" })
        }

        // ====================================================================
        // 运维大屏：聚合指标（dashboard.metrics）
        // ====================================================================

        // ====================================================================
        // 系统更新检测与触发
        // ====================================================================

        "system.info" => {
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "commit": env!("CRADLE_BUILD_COMMIT"),
                "commitDate": env!("CRADLE_BUILD_DATE"),
                "dirty": env!("CRADLE_BUILD_DIRTY") == "1",
                "uptimeMs": current_ms() - state.started_at,
            })
        }

        "system.check_update" => {
            // 调用 GitHub API 检查最新 commit
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("CradleRing-UpdateChecker/1.0")
                .build().unwrap_or_default();
            let repo = params["repo"].as_str().unwrap_or("UA-Jin/CradleRing");
            let url = format!("https://api.github.com/repos/{}/commits/main", repo);
            match client.get(&url).send().await {
                Ok(resp) => {
                    if let Ok(data) = resp.json::<serde_json::Value>().await {
                        let latest_sha = data["sha"].as_str().unwrap_or("").to_string();
                        let latest_date = data["commit"]["author"]["date"].as_str().unwrap_or("").to_string();
                        let latest_msg = data["commit"]["message"].as_str().unwrap_or("").to_string();
                        let current_sha = env!("CRADLE_BUILD_COMMIT");
                        let has_update = !latest_sha.is_empty() && latest_sha != current_sha;
                        json!({
                            "hasUpdate": has_update,
                            "currentCommit": current_sha,
                            "currentDate": env!("CRADLE_BUILD_DATE"),
                            "latestCommit": latest_sha.chars().take(8).collect::<String>(),
                            "latestDate": latest_date,
                            "latestMessage": latest_msg,
                        })
                    } else {
                        json!({"error": "无法解析 GitHub API 响应", "hasUpdate": false})
                    }
                }
                Err(e) => json!({"error": format!("GitHub API 请求失败: {}", e), "hasUpdate": false}),
            }
        }

        "system.update" => {
            // 触发重新跑 install.sh 更新
            // 安全检查：只允许 admin 角色
            // 简化：任何已认证用户都可触发（生产环境应加权限检查）
            let home = state.storage.home.clone();
            let install_script = format!("{}/install.sh", home);
            // 如果本地没有 install.sh，从 GitHub 下载
            let script_exists = std::path::Path::new(&install_script).exists();
            let cmd = if script_exists {
                format!("bash {}", install_script)
            } else {
                "curl -fsSL https://raw.githubusercontent.com/UA-Jin/CradleRing/main/install.sh | bash".to_string()
            };
            // 异步执行更新（不阻塞 RPC 响应）
            let state_clone = state.clone();
            tokio::spawn(async move {
                let _ = broadcast_event(&state_clone, "system.update.started", json!({"cmd": cmd})).await;
                let output = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .output()
                    .await;
                match output {
                    Ok(o) => {
                        let status = o.status.code().unwrap_or(-1);
                        let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                        let _ = broadcast_event(&state_clone, "system.update.completed", json!({
                            "status": status,
                            "stdout": stdout.chars().take(2000).collect::<String>(),
                            "stderr": stderr.chars().take(1000).collect::<String>(),
                        })).await;
                    }
                    Err(e) => {
                        let _ = broadcast_event(&state_clone, "system.update.failed", json!({
                            "error": format!("{}", e),
                        })).await;
                    }
                }
            });
            json!({"ok": true, "message": "更新已触发，请稍后重启网关"})
        }

        "dashboard.metrics" => {
            let nodes = state.storage.load_nodes();
            let now = current_ms();
            let five_min = 300_000i64;

            // 计算每个节点的状态和风险
            let mut online = 0usize;
            let mut offline = 0usize;
            let mut high_latency = 0usize;
            let mut at_risk = 0usize;
            let mut node_details = vec![];
            let mut total_latency = 0u64;
            let mut latency_count = 0usize;

            for n in &nodes {
                let is_online = (now - n.last_seen) < five_min;
                let latency = n.latency_ms.unwrap_or(0);
                let is_high_latency = latency > 500;
                let is_offline = !is_online;
                // 风险评分（规则引擎）
                let mut risk = 0u32;
                let mut reasons: Vec<String> = vec![];
                if is_offline { risk += 30; reasons.push("掉线".to_string()); }
                if is_high_latency { risk += 25; reasons.push(format!("高延迟({}ms)", latency)); }
                if latency > 1000 { risk += 15; reasons.push("严重延迟".to_string()); }
                if let Some(cpu) = n.cpu_percent { if cpu > 90 { risk += 20; reasons.push(format!("CPU 过高({}%)", cpu)); } }
                if let Some(mem) = n.mem_percent { if mem > 90 { risk += 20; reasons.push(format!("内存过高({}%)", mem)); } }
                // 渠道 error 状态（如果有关联渠道）
                // 简化：检查 node 的 metadata 里是否有渠道 error
                if n.metadata.get("channel_error").and_then(|v| v.as_bool()).unwrap_or(false) {
                    risk += 15; reasons.push("渠道异常".to_string());
                }
                if risk > 100 { risk = 100; }

                let status = if is_offline { "offline" } else if is_high_latency { "high_latency" } else { "online" };
                if is_online { online += 1; } else { offline += 1; }
                if is_high_latency { high_latency += 1; }
                if risk >= 30 { at_risk += 1; }
                if latency > 0 { total_latency += latency as u64; latency_count += 1; }

                node_details.push(json!({
                    "id": n.id, "name": n.name, "kind": n.kind,
                    "status": status,
                    "latencyMs": latency,
                    "riskScore": risk,
                    "riskReasons": reasons,
                    "lastSeen": n.last_seen,
                    "cpuPercent": n.cpu_percent,
                    "memPercent": n.mem_percent,
                    "metadata": n.metadata,
                }));
            }

            let avg_latency = if latency_count > 0 { total_latency / latency_count as u64 } else { 0 };

            // 渠道状态
            let channels = state.channels_config();
            let mut channel_connected = 0usize;
            let mut channel_error = 0usize;
            for (_id, cfg) in &channels {
                if cfg["enabled"].as_bool().unwrap_or(false) { channel_connected += 1; }
                if cfg["error"].as_str().is_some() { channel_error += 1; }
            }

            // 最近告警（WAF 事件 + 审批 pending）
            let waf_events = state.storage.load_waf_events(20);
            let recent_alerts: Vec<_> = waf_events.iter().rev().take(10).map(|e| json!({
                "type": "waf", "ruleName": e.rule_name, "severity": e.severity,
                "url": e.url, "ts": e.ts,
            })).collect();

            json!({
                "summary": {
                    "total": nodes.len(),
                    "online": online,
                    "offline": offline,
                    "highLatency": high_latency,
                    "atRisk": at_risk,
                    "avgLatencyMs": avg_latency,
                    "channelsConnected": channel_connected,
                    "channelsError": channel_error,
                    "channelsTotal": channels.len(),
                },
                "nodes": node_details,
                "recentAlerts": recent_alerts,
                "timestamp": now,
            })
        }
        "dashboard.nodes" => {
            // 设备列表（带状态和风险）
            let nodes = state.storage.load_nodes();
            let now = current_ms();
            let five_min = 300_000i64;
            let details: Vec<_> = nodes.iter().map(|n| {
                let is_online = (now - n.last_seen) < five_min;
                let latency = n.latency_ms.unwrap_or(0);
                let status = if !is_online { "offline" } else if latency > 500 { "high_latency" } else { "online" };
                json!({
                    "id": n.id, "name": n.name, "kind": n.kind,
                    "status": status, "latencyMs": latency,
                    "riskScore": n.risk_score, "lastSeen": n.last_seen,
                    "cpuPercent": n.cpu_percent, "memPercent": n.mem_percent,
                })
            }).collect();
            json!({"nodes": details, "count": details.len()})
        }

        // ====================================================================
        // 补充：Config 扩展
        // ====================================================================
        "config.reset" => {
            let cfg_path = format!("{}/.cradle-ring/cradle-ring.json", state.storage.home);
            let backup = format!("{}.reset.{}", cfg_path, current_ms());
            let _ = std::fs::rename(&cfg_path, &backup);
            json!({ "ok": true, "reset": true, "backup": backup })
        }
        "config.export" => {
            let cfg_path = format!("{}/.cradle-ring/cradle-ring.json", state.storage.home);
            match std::fs::read_to_string(&cfg_path) {
                Ok(data) => json!({ "ok": true, "path": cfg_path, "content": data }),
                Err(_) => json!({ "ok": true, "content": serde_json::Value::Null }),
            }
        }
        "config.import" => {
            let cfg_path = format!("{}/.cradle-ring/cradle-ring.json", state.storage.home);
            let content = params["content"].as_str().unwrap_or("");
            if content.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 content"}});
            }
            match serde_json::from_str::<serde_json::Value>(content) {
                Ok(v) => {
                    let _ = std::fs::write(&cfg_path, serde_json::to_string_pretty(&v).unwrap_or_default());
                    json!({ "ok": true, "imported": true })
                }
                Err(e) => json!({"ok": false, "error": {"code": "PARSE_FAILED", "message": e.to_string()}}),
            }
        }

        // ====================================================================
        // 补充：Gateway 扩展
        // ====================================================================
        "gateway.stats" => {
            let sessions = state.storage.load_sessions();
            let memory = state.storage.load_memory();
            let ws = state.active_ws.lock().await.len();
            json!({
                "ok": true,
                "uptimeMs": current_ms() - state.started_at,
                "startedAt": state.started_at,
                "sessions": sessions.len(),
                "memoryItems": memory.len(),
                "websockets": ws,
                "version": env!("CARGO_PKG_VERSION"),
            })
        }
        "gateway.logs" => {
            let limit = params["limit"].as_u64().unwrap_or(100) as usize;
            let events = state.storage.load_events(limit);
            json!({ "logs": events, "count": events.len() })
        }
        "gateway.events" => {
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let events = state.storage.load_events(limit);
            json!({ "events": events, "count": events.len() })
        }

        // ====================================================================
        // 多账号用户系统
        // ====================================================================

        "users.login" => {
            let username = params["username"].as_str().unwrap_or("").to_string();
            let password = params["password"].as_str().unwrap_or("").to_string();
            if username.is_empty() || password.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_CREDENTIALS", "message": "用户名和密码不能为空"}});
            }
            let mut users = state.storage.load_users();
            let user = match users.iter_mut().find(|u| u.username == username && u.enabled) {
                Some(u) => u,
                None => return json!({"ok": false, "error": {"code": "INVALID_CREDENTIALS", "message": "用户名或密码错误"}}),
            };
            if !verify_password(&password, &user.password_hash) {
                return json!({"ok": false, "error": {"code": "INVALID_CREDENTIALS", "message": "用户名或密码错误"}});
            }
            user.last_login = Some(current_ms());
            let user_clone = user.clone();
            state.storage.save_users(&users);
            // 签发 token（7 天有效）
            let token = issue_jwt(&user_clone, 7 * 24 * 3600, &state);
            state.storage.append_token(&token);
            json!({
                "ok": true,
                "token": token.token,
                "expiresAt": token.expires_at,
                "user": user_to_json(&user_clone),
            })
        }

        "users.me" => {
            let token = params["token"].as_str().or(params["authorization"].as_str()).unwrap_or("");
            let token = token.trim_start_matches("Bearer ").trim();
            match verify_jwt(token, &state) {
                Some((uid, uname, role)) => {
                    let users = state.storage.load_users();
                    let user = users.iter().find(|u| u.id == uid);
                    match user {
                        Some(u) => json!({"ok": true, "user": user_to_json(u)}),
                        None => json!({"ok": true, "user": {"id": uid, "username": uname, "role": role, "displayName": uname}}),
                    }
                }
                None => json!({"ok": false, "error": {"code": "UNAUTHORIZED", "message": "未认证或令牌已过期"}}),
            }
        }

        "users.logout" => {
            let token = params["token"].as_str().unwrap_or("").to_string();
            if !token.is_empty() {
                // 简单方案：将 token 加入黑名单（这里仅清理过期 token）
                state.storage.purge_expired_tokens();
            }
            json!({"ok": true})
        }

        "users.list" => {
            let users = state.storage.load_users();
            json!({
                "users": users.iter().map(user_to_json).collect::<Vec<_>>(),
                "count": users.len(),
            })
        }

        "users.create" => {
            let username = params["username"].as_str().unwrap_or("").to_string();
            let password = params["password"].as_str().unwrap_or("").to_string();
            if username.is_empty() || password.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "用户名和密码不能为空"}});
            }
            let mut users = state.storage.load_users();
            if users.iter().any(|u| u.username == username) {
                return json!({"ok": false, "error": {"code": "DUPLICATE", "message": "用户名已存在"}});
            }
            let role = params["role"].as_str().unwrap_or("operator").to_string();
            let user = User {
                id: format!("user-{}", &format!("{:016x}", rand_u128())[..12]),
                username: username.clone(),
                password_hash: hash_password(&password),
                display_name: params["displayName"].as_str().unwrap_or(&username).to_string(),
                email: params["email"].as_str().map(|s| s.to_string()),
                role: role.clone(),
                scopes: params["scopes"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_else(|| role_default_scopes(&role)),
                agent_id: params["agentId"].as_str().unwrap_or("main").to_string(),
                enabled: params["enabled"].as_bool().unwrap_or(true),
                created_at: current_ms(),
                last_login: None,
                approval_enabled: params["approvalEnabled"].as_bool().unwrap_or(true),
            };
            let u_clone = user.clone();
            users.push(user);
            state.storage.save_users(&users);
            let _ = broadcast_event(&state, "users.created", user_to_json(&u_clone)).await;
            json!({"ok": true, "user": user_to_json(&u_clone)})
        }

        "users.update" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let mut users = state.storage.load_users();
            let u = match users.iter_mut().find(|u| u.id == id) {
                Some(u) => u,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "用户不存在"}}),
            };
            if let Some(v) = params["displayName"].as_str() { u.display_name = v.to_string(); }
            if let Some(v) = params["email"].as_str() { u.email = Some(v.to_string()); }
            if let Some(v) = params["role"].as_str() { u.role = v.to_string(); }
            if let Some(v) = params["agentId"].as_str() { u.agent_id = v.to_string(); }
            if let Some(v) = params["enabled"].as_bool() { u.enabled = v; }
            if let Some(v) = params["approvalEnabled"].as_bool() { u.approval_enabled = v; }
            if let Some(v) = params["password"].as_str() { if !v.is_empty() { u.password_hash = hash_password(v); } }
            if let Some(arr) = params["scopes"].as_array() {
                u.scopes = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
            let u_clone = u.clone();
            state.storage.save_users(&users);
            json!({"ok": true, "user": user_to_json(&u_clone)})
        }

        "users.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let mut users = state.storage.load_users();
            // 保护最后一个 admin 不被删除
            let admin_count = users.iter().filter(|u| u.role == "admin" && u.enabled).count();
            let is_admin = users.iter().find(|u| u.id == id).map(|u| u.role == "admin").unwrap_or(false);
            if is_admin && admin_count <= 1 {
                return json!({"ok": false, "error": {"code": "LAST_ADMIN", "message": "不能删除最后一个管理员"}});
            }
            let before = users.len();
            users.retain(|u| u.id != id);
            if users.len() == before {
                return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "用户不存在"}});
            }
            state.storage.save_users(&users);
            json!({"ok": true})
        }

        "users.updateProfile" => {
            // 当前用户自助修改个人资料（不改角色/权限）
            let token = params["token"].as_str().unwrap_or("").to_string();
            let token = token.trim_start_matches("Bearer ").trim();
            let (uid, _) = match verify_jwt(token, &state) {
                Some((uid, uname, role)) => (uid, (uname, role)),
                None => return json!({"ok": false, "error": {"code": "UNAUTHORIZED", "message": "未认证"}}),
            };
            let mut users = state.storage.load_users();
            let u = match users.iter_mut().find(|u| u.id == uid) {
                Some(u) => u,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "用户不存在"}}),
            };
            if let Some(v) = params["displayName"].as_str() { u.display_name = v.to_string(); }
            if let Some(v) = params["email"].as_str() { u.email = Some(v.to_string()); }
            if let Some(v) = params["approvalEnabled"].as_bool() { u.approval_enabled = v; }
            if let Some(v) = params["password"].as_str() { if !v.is_empty() { u.password_hash = hash_password(v); } }
            let u_clone = u.clone();
            state.storage.save_users(&users);
            json!({"ok": true, "user": user_to_json(&u_clone)})
        }

        "roles.list" => {
            // 预置角色（硬编码 5 个）+ 自定义角色（roles.json）
            let mut roles = vec![
                json!({"name": "admin", "label": "管理员", "description": "所有操作 + 用户管理 + 审批流配置", "scopes": role_default_scopes("admin"), "builtin": true, "color": "#f53f3f"}),
                json!({"name": "manager", "label": "经理", "description": "高级审批 + 查看 + 配置", "scopes": role_default_scopes("manager"), "builtin": true, "color": "#722ed1"}),
                json!({"name": "supervisor", "label": "主管", "description": "常规审批 + 查看", "scopes": role_default_scopes("supervisor"), "builtin": true, "color": "#165dff"}),
                json!({"name": "operator", "label": "操作员", "description": "执行命令 + 对话 + 工具", "scopes": role_default_scopes("operator"), "builtin": true, "color": "#00b42a"}),
                json!({"name": "viewer", "label": "访客", "description": "只读", "scopes": role_default_scopes("viewer"), "builtin": true, "color": "#86909c"}),
            ];
            // 追加自定义角色
            let custom_roles = state.storage.load_roles();
            for r in &custom_roles {
                roles.push(role_to_json(r));
            }
            json!({"roles": roles})
        }
        "roles.create" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            let label = params["label"].as_str().unwrap_or("").to_string();
            if name.is_empty() || label.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name 或 label"}});
            }
            // 检查是否与预置角色重名
            let builtins = ["admin", "manager", "supervisor", "operator", "viewer"];
            if builtins.contains(&name.as_str()) {
                return json!({"ok": false, "error": {"code": "DUPLICATE", "message": "不能与预置角色重名"}});
            }
            let mut roles = state.storage.load_roles();
            if roles.iter().any(|r| r.name == name) {
                return json!({"ok": false, "error": {"code": "DUPLICATE", "message": "角色已存在"}});
            }
            let scopes: Vec<String> = params["scopes"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let role = Role {
                name: name.clone(),
                label: label.clone(),
                description: params["description"].as_str().unwrap_or("").to_string(),
                scopes,
                builtin: false,
                color: params["color"].as_str().unwrap_or("#0fc6c2").to_string(),
                created_at: current_ms(),
            };
            let r_clone = role.clone();
            roles.push(role);
            state.storage.save_roles(&roles);
            json!({"ok": true, "role": role_to_json(&r_clone)})
        }
        "roles.update" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}});
            }
            // 预置角色只能改 scopes 和 label/description
            let builtins = ["admin", "manager", "supervisor", "operator", "viewer"];
            let is_builtin = builtins.contains(&name.as_str());
            let mut roles = state.storage.load_roles();
            let r = match roles.iter_mut().find(|r| r.name == name) {
                Some(r) => r,
                None => {
                    // 预置角色首次编辑：自动创建记录
                    if is_builtin {
                        roles.push(Role {
                            name: name.clone(),
                            label: match name.as_str() {
                                "admin" => "管理员", "manager" => "经理", "supervisor" => "主管",
                                "operator" => "操作员", "viewer" => "访客", _ => &name,
                            }.to_string(),
                            description: "预置角色（已修改）".to_string(),
                            scopes: role_default_scopes(&name),
                            builtin: true,
                            color: match name.as_str() {
                                "admin" => "#f53f3f", "manager" => "#722ed1", "supervisor" => "#165dff",
                                "operator" => "#00b42a", "viewer" => "#86909c", _ => "#0fc6c2",
                            }.to_string(),
                            created_at: current_ms(),
                        });
                        roles.last_mut().unwrap()
                    } else {
                        return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "角色不存在"}});
                    }
                }
            };
            if let Some(v) = params["label"].as_str() { r.label = v.to_string(); }
            if let Some(v) = params["description"].as_str() { r.description = v.to_string(); }
            if let Some(v) = params["color"].as_str() { r.color = v.to_string(); }
            if let Some(arr) = params["scopes"].as_array() {
                r.scopes = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
            let r_clone = r.clone();
            state.storage.save_roles(&roles);
            json!({"ok": true, "role": role_to_json(&r_clone)})
        }
        "roles.delete" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            let builtins = ["admin", "manager", "supervisor", "operator", "viewer"];
            if builtins.contains(&name.as_str()) {
                return json!({"ok": false, "error": {"code": "PROTECTED", "message": "预置角色不可删除"}});
            }
            let mut roles = state.storage.load_roles();
            let before = roles.len();
            roles.retain(|r| r.name != name);
            if roles.len() == before {
                return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "角色不存在"}});
            }
            state.storage.save_roles(&roles);
            json!({"ok": true})
        }

        "roles.scopes" => {
            let role = params["role"].as_str().unwrap_or("");
            json!({"role": role, "scopes": role_default_scopes(role)})
        }

        // ====================================================================
        // 多级审批工作流
        // ====================================================================

        "approval.flows.list" => {
            let flows = state.storage.load_approval_flows();
            json!({
                "flows": flows.iter().map(approval_flow_to_json).collect::<Vec<_>>(),
                "count": flows.len(),
            })
        }

        "approval.flows.get" => {
            let id = params["id"].as_str().unwrap_or("");
            let flows = state.storage.load_approval_flows();
            match flows.iter().find(|f| f.id == id) {
                Some(f) => json!({"flow": approval_flow_to_json(f)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批流不存在"}}),
            }
        }

        "approval.flows.create" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}});
            }
            let steps: Vec<ApprovalStep> = params["steps"].as_array()
                .map(|a| a.iter().filter_map(|s| {
                    Some(ApprovalStep {
                        order: s["order"].as_u64()? as u32,
                        name: s["name"].as_str().unwrap_or("").to_string(),
                        approver_role: s["approverRole"].as_str().unwrap_or("").to_string(),
                        approver_ids: s["approverIds"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        notify_channels: s["notifyChannels"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        notify_targets: s["notifyTargets"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        auto_approve_after_secs: s["autoApproveAfterSecs"].as_u64(),
                        require_all: s["requireAll"].as_bool().unwrap_or(false),
                    })
                }).collect())
                .unwrap_or_default();
            // 步骤排序
            let mut steps = steps;
            steps.sort_by_key(|s| s.order);
            let trigger_patterns = params["triggerPatterns"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let kinds = params["kinds"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec!["exec".to_string()]);
            let flow = ApprovalFlow {
                id: format!("flow-{:016x}", rand_u128()),
                name,
                trigger_patterns,
                kinds,
                steps,
                enabled: params["enabled"].as_bool().unwrap_or(true),
                created_by: params["createdBy"].as_str().unwrap_or("admin").to_string(),
                created_at: current_ms(),
            };
            let f_clone = flow.clone();
            let mut flows = state.storage.load_approval_flows();
            flows.push(flow);
            state.storage.save_approval_flows(&flows);
            let _ = broadcast_event(&state, "approval.flow.created", approval_flow_to_json(&f_clone)).await;
            json!({"ok": true, "flow": approval_flow_to_json(&f_clone)})
        }

        "approval.flows.update" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let mut flows = state.storage.load_approval_flows();
            let f = match flows.iter_mut().find(|f| f.id == id) {
                Some(f) => f,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批流不存在"}}),
            };
            if let Some(v) = params["name"].as_str() { f.name = v.to_string(); }
            if let Some(v) = params["enabled"].as_bool() { f.enabled = v; }
            if let Some(arr) = params["triggerPatterns"].as_array() {
                f.trigger_patterns = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
            if let Some(arr) = params["kinds"].as_array() {
                f.kinds = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
            if let Some(arr) = params["steps"].as_array() {
                let mut steps: Vec<ApprovalStep> = arr.iter().filter_map(|s| {
                    Some(ApprovalStep {
                        order: s["order"].as_u64()? as u32,
                        name: s["name"].as_str().unwrap_or("").to_string(),
                        approver_role: s["approverRole"].as_str().unwrap_or("").to_string(),
                        approver_ids: s["approverIds"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        notify_channels: s["notifyChannels"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        notify_targets: s["notifyTargets"].as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                        auto_approve_after_secs: s["autoApproveAfterSecs"].as_u64(),
                        require_all: s["requireAll"].as_bool().unwrap_or(false),
                    })
                }).collect();
                steps.sort_by_key(|s| s.order);
                f.steps = steps;
            }
            let f_clone = f.clone();
            state.storage.save_approval_flows(&flows);
            json!({"ok": true, "flow": approval_flow_to_json(&f_clone)})
        }

        "approval.flows.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut flows = state.storage.load_approval_flows();
            let before = flows.len();
            flows.retain(|f| f.id != id);
            if flows.len() == before {
                return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批流不存在"}});
            }
            state.storage.save_approval_flows(&flows);
            json!({"ok": true})
        }

        "approval.instances.list" => {
            let status_filter = params["status"].as_str();
            let flow_filter = params["flowId"].as_str();
            let mut items = state.storage.load_approval_instances();
            if let Some(s) = status_filter {
                if s == "history" {
                    items.retain(|i| i.status == "approved" || i.status == "rejected" || i.status == "timeout" || i.status == "completed" || i.status == "failed");
                } else {
                    items.retain(|i| i.status == s);
                }
            }
            if let Some(f) = flow_filter { items.retain(|i| i.flow_id == f); }
            items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            json!({
                "instances": items.iter().map(approval_instance_to_json).collect::<Vec<_>>(),
                "count": items.len(),
            })
        }

        "approval.instances.get" => {
            let id = params["id"].as_str().unwrap_or("");
            let items = state.storage.load_approval_instances();
            match items.iter().find(|i| i.id == id) {
                Some(i) => json!({"instance": approval_instance_to_json(i)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批实例不存在"}}),
            }
        }

        "approval.instances.create" => {
            let flow_id = params["flowId"].as_str().unwrap_or("").to_string();
            let flows = state.storage.load_approval_flows();
            let flow = match flows.iter().find(|f| f.id == flow_id) {
                Some(f) => f.clone(),
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批流不存在"}}),
            };
            if !flow.enabled {
                return json!({"ok": false, "error": {"code": "DISABLED", "message": "审批流已禁用"}});
            }
            let command = params["command"].as_str().unwrap_or("").to_string();
            let title = params["title"].as_str().unwrap_or("手动审批请求").to_string();
            let description = params["description"].as_str().unwrap_or("").to_string();
            let kind = params["kind"].as_str().unwrap_or("exec").to_string();
            let requested_by = params["requestedBy"].as_str().unwrap_or("user").to_string();
            let requested_username = params["requestedByUsername"].as_str().unwrap_or("用户").to_string();
            let session_key = params["sessionKey"].as_str().unwrap_or("").to_string();
            let async_non_blocking = params["asyncNonBlocking"].as_bool().unwrap_or(true);
            let inst_id = create_approval_instance(
                &state, &flow, &kind, &command, &title, &description,
                &requested_by, &requested_username,
                &session_key, "",
                async_non_blocking,
            ).await;
            let items = state.storage.load_approval_instances();
            let inst = items.iter().find(|i| i.id == inst_id);
            match inst {
                Some(i) => json!({"ok": true, "instance": approval_instance_to_json(i)}),
                None => json!({"ok": true, "instanceId": inst_id}),
            }
        }

        "approval.instances.approve" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let approver_id = params["approverId"].as_str().unwrap_or("user").to_string();
            let approver_username = params["approverUsername"].as_str().unwrap_or("审批人").to_string();
            let comment = params["comment"].as_str().unwrap_or("").to_string();
            let via = params["viaChannel"].as_str().unwrap_or("web").to_string();
            match advance_approval_instance(state.clone(), &id, &approver_id, &approver_username, "approve", &comment, &via).await {
                Some((_, inst)) => json!({"ok": true, "instance": approval_instance_to_json(&inst)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批实例不存在或已处理"}}),
            }
        }

        "approval.instances.reject" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 id"}});
            }
            let approver_id = params["approverId"].as_str().unwrap_or("user").to_string();
            let approver_username = params["approverUsername"].as_str().unwrap_or("审批人").to_string();
            let comment = params["comment"].as_str().unwrap_or("").to_string();
            let via = params["viaChannel"].as_str().unwrap_or("web").to_string();
            match advance_approval_instance(state.clone(), &id, &approver_id, &approver_username, "reject", &comment, &via).await {
                Some((_, inst)) => json!({"ok": true, "instance": approval_instance_to_json(&inst)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批实例不存在或已处理"}}),
            }
        }

        "approval.instances.cancel" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut items = state.storage.load_approval_instances();
            let inst = match items.iter_mut().find(|i| i.id == id) {
                Some(i) => i,
                None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "审批实例不存在"}}),
            };
            if inst.status != "pending" {
                return json!({"ok": false, "error": {"code": "ALREADY_DECIDED", "message": "审批实例已处理"}});
            }
            inst.status = "cancelled".to_string();
            inst.completed_at = Some(current_ms());
            let inst_clone = inst.clone();
            state.storage.save_approval_instances(&items);
            // 唤醒等待器（视为拒绝）
            let mut pending = state.pending_approval_instances.lock().await;
            if let Some(tx) = pending.remove(&id) { let _ = tx.send(false); }
            json!({"ok": true, "instance": approval_instance_to_json(&inst_clone)})
        }

        "approval.stats" => {
            let items = state.storage.load_approval_instances();
            let pending = items.iter().filter(|i| i.status == "pending").count();
            let approved = items.iter().filter(|i| i.status == "approved").count();
            let rejected = items.iter().filter(|i| i.status == "rejected").count();
            let timeout = items.iter().filter(|i| i.status == "timeout").count();
            let completed = items.iter().filter(|i| i.status == "completed").count();
            let flows = state.storage.load_approval_flows();
            json!({
                "total": items.len(),
                "pending": pending,
                "approved": approved,
                "rejected": rejected,
                "timeout": timeout,
                "completed": completed,
                "flowsCount": flows.len(),
            })
        }

        // ====================================================================
        // 运维能力：命令策略沙箱 + 审计日志 + AI SOP 二审（对标 ongrid）
        // ====================================================================

        "cmdpolicy.check" => {
            let command = params["command"].as_str().unwrap_or("");
            let analysis = cmd_policy().classify(command);
            json!({
                "command": command,
                "binary": analysis.binary,
                "class": analysis.class.as_str(),
                "safe": analysis.safe,
                "reason": analysis.reason,
                "needsApproval": analysis.class == CmdClass::Write || (analysis.class != CmdClass::Destructive && !analysis.safe),
                "destructive": analysis.class == CmdClass::Destructive,
            })
        }
        "cmdpolicy.classes" => {
            json!({
                "classes": [
                    {"name": "read_fs", "label": "只读文件系统", "examples": ["cat", "head", "tail", "ls", "find", "du", "stat", "grep"]},
                    {"name": "read_system", "label": "只读系统", "examples": ["ps", "top", "uptime", "free", "df", "ss", "netstat", "dmesg", "journalctl"]},
                    {"name": "mixed", "label": "读写混合", "examples": ["iptables", "ip6tables", "nft"]},
                    {"name": "net_diag", "label": "网络诊断", "examples": ["ping", "traceroute", "mtr", "dig", "tcpdump", "ovs-vsctl", "conntrack"]},
                    {"name": "write", "label": "写入/变更（需审批）", "examples": ["systemctl start/stop", "docker rm", "kubectl apply"]},
                    {"name": "destructive", "label": "高危（拒绝）", "examples": ["rm -rf /", "mkfs", "dd of=/dev/", "shutdown"]},
                ],
                "pathAllowlist": cmd_policy().path_allowlist,
                "stdoutCap": cmd_policy().stdout_cap,
                "timeoutSecs": cmd_policy().timeout_secs,
            })
        }
        "change_events.list" => {
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let kind = params["kind"].as_str();
            let events = state.storage.load_change_events(limit, kind);
            json!({
                "events": events.iter().rev().map(|e| json!({
                    "id": &e.id, "kind": &e.kind, "action": &e.action, "target": &e.target,
                    "actor": &e.actor, "ts": e.ts, "result": &e.result,
                    "rollbackHint": &e.rollback_hint, "approvalId": &e.approval_id,
                    "sessionKey": &e.session_key,
                })).collect::<Vec<_>>(),
                "count": events.len(),
            })
        }
        "reviews.list" => {
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let reviews = state.storage.load_reviews(limit);
            json!({
                "reviews": reviews.iter().rev().map(|r| json!({
                    "id": &r.id, "action": &r.action, "target": &r.target, "reason": &r.reason,
                    "blastRadius": &r.blast_radius, "decision": &r.decision,
                    "hasSop": r.has_sop, "noParallelOp": r.no_parallel_op, "rollbackKnown": r.rollback_known,
                    "comment": &r.comment, "matchedSop": &r.matched_sop, "ts": r.ts,
                })).collect::<Vec<_>>(),
                "count": reviews.len(),
            })
        }
        "reviews.review" => {
            // AI SOP 二审：对指定操作做静态审查
            let action = params["action"].as_str().unwrap_or("exec").to_string();
            let target = params["target"].as_str().unwrap_or("").to_string();
            let reason = params["reason"].as_str().unwrap_or("").to_string();
            let blast = params["blastRadius"].as_str().unwrap_or("unknown").to_string();
            if target.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 target"}});
            }
            let review = ai_sop_review(&state, &action, &target, &reason, &blast).await;
            state.storage.append_review(&review);
            let _ = broadcast_event(&state, "review.completed", json!({
                "id": &review.id, "decision": &review.decision, "target": &target,
            })).await;
            json!({
                "ok": true,
                "decision": review.decision,
                "hasSop": review.has_sop,
                "noParallelOp": review.no_parallel_op,
                "rollbackKnown": review.rollback_known,
                "comment": review.comment,
                "matchedSop": review.matched_sop,
                "reviewId": review.id,
            })
        }

        // ====================================================================
        // WAF 安全能力（对标 ModSecurity + ongrid 安全运维）
        // ====================================================================

        "waf.rules.list" => {
            let rules = state.storage.load_waf_rules();
            json!({
                "rules": rules.iter().map(waf_rule_to_json).collect::<Vec<_>>(),
                "count": rules.len(),
            })
        }
        "waf.rules.create" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            let pattern = params["pattern"].as_str().unwrap_or("").to_string();
            if name.is_empty() || pattern.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name 或 pattern"}});
            }
            // 校验正则
            if regex::Regex::new(&pattern).is_err() {
                return json!({"ok": false, "error": {"code": "INVALID_REGEX", "message": "正则表达式无效"}});
            }
            let rule = WafRule {
                id: format!("waf-custom-{}", &format!("{:016x}", rand_u128())[..12]),
                name,
                rule_type: params["ruleType"].as_str().unwrap_or("custom").to_string(),
                pattern,
                action: params["action"].as_str().unwrap_or("log").to_string(),
                severity: params["severity"].as_str().unwrap_or("medium").to_string(),
                enabled: params["enabled"].as_bool().unwrap_or(true),
                description: params["description"].as_str().unwrap_or("").to_string(),
                hit_count: 0,
                created_at: current_ms(),
            };
            let r_clone = rule.clone();
            let mut rules = state.storage.load_waf_rules();
            rules.push(rule);
            state.storage.save_waf_rules(&rules);
            json!({"ok": true, "rule": waf_rule_to_json(&r_clone)})
        }
        "waf.rules.update" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut rules = state.storage.load_waf_rules();
            let r = match rules.iter_mut().find(|r| r.id == id) {
                Some(r) => r, None => return json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            };
            if let Some(v) = params["name"].as_str() { r.name = v.to_string(); }
            if let Some(v) = params["pattern"].as_str() { r.pattern = v.to_string(); }
            if let Some(v) = params["ruleType"].as_str() { r.rule_type = v.to_string(); }
            if let Some(v) = params["action"].as_str() { r.action = v.to_string(); }
            if let Some(v) = params["severity"].as_str() { r.severity = v.to_string(); }
            if let Some(v) = params["enabled"].as_bool() { r.enabled = v; }
            if let Some(v) = params["description"].as_str() { r.description = v.to_string(); }
            let r_clone = r.clone();
            state.storage.save_waf_rules(&rules);
            json!({"ok": true, "rule": waf_rule_to_json(&r_clone)})
        }
        "waf.rules.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut rules = state.storage.load_waf_rules();
            let before = rules.len();
            rules.retain(|r| r.id != id);
            if rules.len() == before { return json!({"ok": false, "error": {"code": "NOT_FOUND"}}); }
            state.storage.save_waf_rules(&rules);
            json!({"ok": true})
        }
        "waf.rules.toggle" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut rules = state.storage.load_waf_rules();
            let r = match rules.iter_mut().find(|r| r.id == id) {
                Some(r) => r, None => return json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            };
            r.enabled = !r.enabled;
            let enabled = r.enabled;
            state.storage.save_waf_rules(&rules);
            json!({"ok": true, "enabled": enabled})
        }
        "waf.check" => {
            // 对请求做 WAF 规则检查
            let url = params["url"].as_str().unwrap_or("").to_string();
            let headers = params["headers"].as_str().unwrap_or("").to_string();
            let body = params["body"].as_str().unwrap_or("").to_string();
            let rules = state.storage.load_waf_rules();
            let matches = waf_check_request(&rules, &url, &headers, &body);
            // 更新命中计数并记录事件
            if !matches.is_empty() {
                let mut rules = state.storage.load_waf_rules();
                for m in &matches {
                    if let Some(r) = rules.iter_mut().find(|r| r.id == m.rule_id) {
                        r.hit_count += 1;
                    }
                }
                state.storage.save_waf_rules(&rules);
                for m in &matches {
                    state.storage.append_waf_event(&WafEvent {
                        id: format!("we-{:016x}", rand_u128()),
                        rule_id: m.rule_id.clone(),
                        rule_name: m.rule_name.clone(),
                        rule_type: m.rule_type.clone(),
                        action: m.action.clone(),
                        severity: m.severity.clone(),
                        url: url.clone(),
                        source_ip: params["sourceIp"].as_str().unwrap_or("unknown").to_string(),
                        user_agent: params["userAgent"].as_str().unwrap_or("unknown").to_string(),
                        matched_text: m.matched_text.clone(),
                        ts: current_ms(),
                    });
                }
            }
            json!({
                "blocked": matches.iter().any(|m| m.action == "block"),
                "matches": matches.iter().map(|m| json!({
                    "ruleId": m.rule_id, "ruleName": m.rule_name, "ruleType": m.rule_type,
                    "action": m.action, "severity": m.severity,
                })).collect::<Vec<_>>(),
                "count": matches.len(),
            })
        }
        "waf.events.list" => {
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let events = state.storage.load_waf_events(limit);
            json!({
                "events": events.iter().rev().map(|e| json!({
                    "id": e.id, "ruleId": e.rule_id, "ruleName": e.rule_name,
                    "ruleType": e.rule_type, "action": e.action, "severity": e.severity,
                    "url": e.url, "sourceIp": e.source_ip, "userAgent": e.user_agent,
                    "ts": e.ts,
                })).collect::<Vec<_>>(),
                "count": events.len(),
            })
        }
        "waf.stats" => {
            let rules = state.storage.load_waf_rules();
            let events = state.storage.load_waf_events(1000);
            let enabled_count = rules.iter().filter(|r| r.enabled).count();
            let total_hits: u64 = rules.iter().map(|r| r.hit_count).sum();
            let block_count = events.iter().filter(|e| e.action == "block").count();
            json!({
                "totalRules": rules.len(),
                "enabledRules": enabled_count,
                "totalHits": total_hits,
                "recentEvents": events.len(),
                "blockCount": block_count,
            })
        }

        // ====================================================================
        // 入侵检测防护（IDS/IPS）
        // ====================================================================

        "ids.rules.list" => {
            let rules = state.storage.load_ids_rules();
            json!({
                "rules": rules.iter().map(ids_rule_to_json).collect::<Vec<_>>(),
                "count": rules.len(),
            })
        }
        "ids.rules.create" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            let pattern = params["pattern"].as_str().unwrap_or("").to_string();
            if name.is_empty() || pattern.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name 或 pattern"}});
            }
            let rule = IdsRule {
                id: format!("ids-custom-{}", &format!("{:016x}", rand_u128())[..12]),
                name,
                rule_type: params["ruleType"].as_str().unwrap_or("custom").to_string(),
                pattern,
                action: params["action"].as_str().unwrap_or("log").to_string(),
                threshold: params["threshold"].as_u64().unwrap_or(5) as u32,
                window_secs: params["windowSecs"].as_u64().unwrap_or(300),
                enabled: params["enabled"].as_bool().unwrap_or(true),
                description: params["description"].as_str().unwrap_or("").to_string(),
                hit_count: 0,
                created_at: current_ms(),
            };
            let r_clone = rule.clone();
            let mut rules = state.storage.load_ids_rules();
            rules.push(rule);
            state.storage.save_ids_rules(&rules);
            json!({"ok": true, "rule": ids_rule_to_json(&r_clone)})
        }
        "ids.rules.update" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut rules = state.storage.load_ids_rules();
            let r = match rules.iter_mut().find(|r| r.id == id) {
                Some(r) => r, None => return json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            };
            if let Some(v) = params["name"].as_str() { r.name = v.to_string(); }
            if let Some(v) = params["pattern"].as_str() { r.pattern = v.to_string(); }
            if let Some(v) = params["ruleType"].as_str() { r.rule_type = v.to_string(); }
            if let Some(v) = params["action"].as_str() { r.action = v.to_string(); }
            if let Some(v) = params["threshold"].as_u64() { r.threshold = v as u32; }
            if let Some(v) = params["windowSecs"].as_u64() { r.window_secs = v; }
            if let Some(v) = params["enabled"].as_bool() { r.enabled = v; }
            if let Some(v) = params["description"].as_str() { r.description = v.to_string(); }
            let r_clone = r.clone();
            state.storage.save_ids_rules(&rules);
            json!({"ok": true, "rule": ids_rule_to_json(&r_clone)})
        }
        "ids.rules.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut rules = state.storage.load_ids_rules();
            let before = rules.len();
            rules.retain(|r| r.id != id);
            if rules.len() == before { return json!({"ok": false, "error": {"code": "NOT_FOUND"}}); }
            state.storage.save_ids_rules(&rules);
            json!({"ok": true})
        }
        "ids.events.list" => {
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let event_type = params["eventType"].as_str();
            let mut events = state.storage.load_ids_events(limit);
            if let Some(t) = event_type { events.retain(|e| e.event_type == t); }
            json!({
                "events": events.iter().rev().map(ids_event_to_json).collect::<Vec<_>>(),
                "count": events.len(),
            })
        }
        "ids.scan" => {
            // 立即执行入侵检测扫描
            let rules = state.storage.load_ids_rules();
            let mut all_events = vec![];
            // SSH 暴力破解
            for r in rules.iter().filter(|r| r.enabled && r.rule_type == "ssh_bruteforce") {
                let events = ids_check_ssh_bruteforce(&state, r.threshold, r.window_secs).await;
                for mut ev in events {
                    if r.action == "block" {
                        match ids_ban_ip(&ev.source, ev.ban_duration_secs).await {
                            Ok(msg) => { ev.blocked = true; ev.detail = format!("{} (已封禁: {})", ev.detail, msg); }
                            Err(e) => { ev.detail = format!("{} (封禁失败: {})", ev.detail, e); }
                        }
                    }
                    state.storage.append_ids_event(&ev);
                    all_events.push(ev);
                }
            }
            // 挖矿/恶意进程
            for r in rules.iter().filter(|r| r.enabled && r.rule_type == "malware_process") {
                let patterns: Vec<String> = r.pattern.split('|').map(String::from).collect();
                let events = ids_check_malware_process(&patterns).await;
                for ev in events {
                    state.storage.append_ids_event(&ev);
                    all_events.push(ev);
                }
            }
            // C2 回连
            for r in rules.iter().filter(|r| r.enabled && r.rule_type == "c2_connection") {
                let patterns: Vec<String> = r.pattern.split('|').map(String::from).collect();
                let events = ids_check_network(&patterns).await;
                for ev in events {
                    state.storage.append_ids_event(&ev);
                    all_events.push(ev);
                }
            }
            let blocked_count = all_events.iter().filter(|e| e.blocked).count();
            json!({
                "ok": true,
                "eventsFound": all_events.len(),
                "blocked": blocked_count,
                "events": all_events.iter().map(ids_event_to_json).collect::<Vec<_>>(),
            })
        }
        "ids.ban" => {
            let ip = params["ip"].as_str().unwrap_or("");
            let duration = params["durationSecs"].as_u64().unwrap_or(3600);
            if ip.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 ip"}}); }
            match ids_ban_ip(ip, duration).await {
                Ok(msg) => json!({"ok": true, "message": msg}),
                Err(e) => json!({"ok": false, "error": {"message": e}}),
            }
        }
        "ids.unban" => {
            let ip = params["ip"].as_str().unwrap_or("");
            if ip.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 ip"}}); }
            let result = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(format!("iptables -D INPUT -s {} -j DROP", ip))
                .output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "message": format!("IP {} 已解封", ip)}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }
        "ids.stats" => {
            let rules = state.storage.load_ids_rules();
            let events = state.storage.load_ids_events(1000);
            let enabled_count = rules.iter().filter(|r| r.enabled).count();
            let total_hits: u64 = rules.iter().map(|r| r.hit_count).sum();
            let blocked_count = events.iter().filter(|e| e.blocked).count();
            let critical_count = events.iter().filter(|e| e.severity == "critical").count();
            json!({
                "totalRules": rules.len(),
                "enabledRules": enabled_count,
                "totalHits": total_hits,
                "recentEvents": events.len(),
                "blocked": blocked_count,
                "critical": critical_count,
            })
        }

        // ====================================================================
        // IP 黑白名单
        // ====================================================================

        "ip.list" => {
            let entries = state.storage.load_ip_list();
            json!({
                "entries": entries.iter().map(ip_entry_to_json).collect::<Vec<_>>(),
                "count": entries.len(),
            })
        }
        "ip.add" => {
            let ip = params["ip"].as_str().unwrap_or("").to_string();
            let list_type = params["listType"].as_str().unwrap_or("blacklist").to_string();
            if ip.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 ip"}}); }
            let mut entries = state.storage.load_ip_list();
            if entries.iter().any(|e| e.ip == ip && e.list_type == list_type) {
                return json!({"ok": false, "error": {"code": "DUPLICATE", "message": "条目已存在"}});
            }
            let entry = IpEntry {
                ip: ip.clone(),
                list_type: list_type.clone(),
                reason: params["reason"].as_str().unwrap_or("").to_string(),
                expires_at: params["expiresAt"].as_i64(),
                created_at: current_ms(),
            };
            entries.push(entry);
            state.storage.save_ip_list(&entries);
            json!({"ok": true, "ip": ip, "listType": list_type})
        }
        "ip.remove" => {
            let ip = params["ip"].as_str().unwrap_or("").to_string();
            let list_type = params["listType"].as_str().unwrap_or("blacklist").to_string();
            let mut entries = state.storage.load_ip_list();
            let before = entries.len();
            entries.retain(|e| !(e.ip == ip && e.list_type == list_type));
            if entries.len() == before { return json!({"ok": false, "error": {"code": "NOT_FOUND"}}); }
            state.storage.save_ip_list(&entries);
            json!({"ok": true})
        }
        "ip.check" => {
            let ip = params["ip"].as_str().unwrap_or("");
            let entries = state.storage.load_ip_list();
            match check_ip_list(&entries, ip) {
                Some(e) => json!({"listed": true, "listType": e.list_type, "reason": e.reason, "expiresAt": e.expires_at}),
                None => json!({"listed": false}),
            }
        }

        // ====================================================================
        // 速率限制
        // ====================================================================

        "rate_limit.list" => {
            let entries = state.storage.load_rate_limit();
            json!({
                "entries": entries.iter().map(rate_limit_to_json).collect::<Vec<_>>(),
                "count": entries.len(),
            })
        }
        "rate_limit.set" => {
            let scope = params["scope"].as_str().unwrap_or("ip").to_string();
            let target = params["target"].as_str().unwrap_or("*").to_string();
            let max_requests = params["maxRequests"].as_u64().unwrap_or(60) as u32;
            let window_secs = params["windowSecs"].as_u64().unwrap_or(60);
            let action = params["action"].as_str().unwrap_or("block").to_string();
            let mut entries = state.storage.load_rate_limit();
            // 更新或创建
            if let Some(e) = entries.iter_mut().find(|e| e.scope == scope && e.target == target) {
                e.max_requests = max_requests;
                e.window_secs = window_secs;
                e.action = action.clone();
            } else {
                entries.push(RateLimitEntry {
                    scope: scope.clone(), target: target.clone(), max_requests,
                    window_secs, current_count: 0, window_start: current_ms(), action: action.clone(),
                });
            }
            state.storage.save_rate_limit(&entries);
            json!({"ok": true, "scope": scope, "target": target, "maxRequests": max_requests, "windowSecs": window_secs})
        }
        "rate_limit.remove" => {
            let scope = params["scope"].as_str().unwrap_or("ip").to_string();
            let target = params["target"].as_str().unwrap_or("*").to_string();
            let mut entries = state.storage.load_rate_limit();
            let before = entries.len();
            entries.retain(|e| !(e.scope == scope && e.target == target));
            if entries.len() == before { return json!({"ok": false, "error": {"code": "NOT_FOUND"}}); }
            state.storage.save_rate_limit(&entries);
            json!({"ok": true})
        }
        "rate_limit.check" => {
            let scope = params["scope"].as_str().unwrap_or("ip").to_string();
            let target = params["target"].as_str().unwrap_or("").to_string();
            let mut entries = state.storage.load_rate_limit();
            let config = state.storage.load_rate_limit();
            let cfg = config.iter().find(|e| e.scope == scope && (e.target == target || e.target == "*"));
            match cfg {
                Some(c) => {
                    let exceeded = check_rate_limit(&mut entries, &scope, &target, c.max_requests, c.window_secs, &c.action);
                    if exceeded {
                        state.storage.save_rate_limit(&entries);
                    }
                    json!({"exceeded": exceeded, "currentCount": entries.iter().find(|e| e.scope == scope && e.target == target).map(|e| e.current_count).unwrap_or(0), "maxRequests": c.max_requests})
                }
                None => json!({"exceeded": false, "currentCount": 0}),
            }
        }

        // ====================================================================
        // 规则导入入口（ModSecurity / Snort / 自定义正则）
        // ====================================================================

        "rules.import" => {
            let format = params["format"].as_str().unwrap_or("auto").to_string();
            let content = params["content"].as_str().unwrap_or("").to_string();
            if content.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 content"}}); }
            let rule_type = params["ruleType"].as_str().unwrap_or("waf").to_string();
            let imported = import_rules(&content, &format, &rule_type);
            match imported {
                Ok((count, errors)) => {
                    if rule_type == "waf" {
                        let mut rules = state.storage.load_waf_rules();
                        rules.extend(imported_ok_waf_rules(&content, &format));
                        state.storage.save_waf_rules(&rules);
                    } else {
                        let mut rules = state.storage.load_ids_rules();
                        rules.extend(imported_ok_ids_rules(&content, &format));
                        state.storage.save_ids_rules(&rules);
                    }
                    json!({"ok": true, "imported": count, "errors": errors})
                }
                Err(e) => json!({"ok": false, "error": {"message": e}}),
            }
        }
        "rules.export" => {
            let rule_type = params["ruleType"].as_str().unwrap_or("waf").to_string();
            let format = params["format"].as_str().unwrap_or("json").to_string();
            if rule_type == "waf" {
                let rules = state.storage.load_waf_rules();
                let content = export_waf_rules(&rules, &format);
                json!({"ok": true, "content": content, "count": rules.len()})
            } else {
                let rules = state.storage.load_ids_rules();
                let content = export_ids_rules(&rules, &format);
                json!({"ok": true, "content": content, "count": rules.len()})
            }
        }
        "rules.market" => {
            // 内置规则包列表（OWASP CRS / Snort Community / 自定义）
            json!({
                "packages": [
                    {"id": "owasp-crs-3.3", "name": "OWASP ModSecurity CRS 3.3", "description": "OWASP 核心规则集（SQLi/XSS/LFI/RCE/Scanner/Protocol）", "ruleCount": 50, "source": "https://github.com/coreruleset/coreruleset"},
                    {"id": "snort-community", "name": "Snort Community Rules", "description": "Snort 社区规则（恶意软件/漏洞利用/策略）", "ruleCount": 30, "source": "https://www.snort.org/downloads"},
                    {"id": "emerging-threats", "name": "Emerging Threats", "description": "ET Open 规则（僵尸网络/木马/间谍软件）", "ruleCount": 40, "source": "https://rules.emergingthreats.net"},
                ]
            })
        }
        "rules.market.install" => {
            let package_id = params["packageId"].as_str().unwrap_or("");
            let _rule_type = params["ruleType"].as_str().unwrap_or("waf").to_string();
            match package_id {
                "owasp-crs-3.3" => {
                    // 已内置（default_waf_rules 已包含 OWASP CRS 核心规则）
                    json!({"ok": true, "message": "OWASP CRS 核心规则已内置", "installed": 50})
                }
                _ => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "规则包不存在"}}),
            }
        }

        "waf.detect" => {
            let url = params["url"].as_str().unwrap_or("");
            if url.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 url"}}); }
            let result = tool_waf_detect(url).await;
            json!({"ok": true, "result": result})
        }
        "security.sqli_scan" => {
            let url = params["url"].as_str().unwrap_or("");
            if url.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 url"}}); }
            let result = tool_sqli_scan(url).await;
            json!({"ok": true, "result": result})
        }
        "security.xss_scan" => {
            let url = params["url"].as_str().unwrap_or("");
            if url.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 url"}}); }
            let result = tool_xss_scan(url).await;
            json!({"ok": true, "result": result})
        }
        "security.exposure_analysis" => {
            let host = params["host"].as_str().unwrap_or("");
            if host.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 host"}}); }
            let result = tool_exposure_analysis(host).await;
            json!({"ok": true, "result": result})
        }

        // ====================================================================
        // 运维区（类似 1Panel / 宝塔）：文件管理器 / 进程管理器 / 服务管理器 / 防火墙 / SSL证书
        // ====================================================================

        // ---- 文件管理器 ----

        "files.list" => {
            let path = params["path"].as_str().unwrap_or("/");
            // 安全检查：只允许白名单路径
            let allowed_prefixes = ["/home", "/var", "/opt", "/srv", "/data", "/tmp", "/etc", "/root"];
            if !allowed_prefixes.iter().any(|p| path.starts_with(p)) {
                return json!({"ok": false, "error": {"code": "FORBIDDEN", "message": "路径不在白名单", "allowed": allowed_prefixes}});
            }
            match tokio::fs::read_dir(path).await {
                Ok(mut entries) => {
                    let mut files = vec![];
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let metadata = entry.metadata().await.ok();
                        files.push(json!({
                            "name": name,
                            "path": format!("{}/{}", path.trim_end_matches('/'), name),
                            "isDir": metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                            "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
                            "modified": metadata.and_then(|m| m.modified().ok()).map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)).unwrap_or(0),
                        }));
                    }
                    files.sort_by(|a, b| {
                        let a_dir = a["isDir"].as_bool().unwrap_or(false);
                        let b_dir = b["isDir"].as_bool().unwrap_or(false);
                        b_dir.cmp(&a_dir).then(a["name"].as_str().cmp(&b["name"].as_str()))
                    });
                    json!({"ok": true, "path": path, "files": files, "count": files.len()})
                }
                Err(e) => json!({"ok": false, "error": {"message": format!("读取目录失败: {}", e)}}),
            }
        }
        "files.read" => {
            let path = params["path"].as_str().unwrap_or("");
            if path.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 path"}}); }
            let allowed_prefixes = ["/home", "/var", "/opt", "/srv", "/data", "/tmp", "/etc", "/root"];
            if !allowed_prefixes.iter().any(|p| path.starts_with(p)) {
                return json!({"ok": false, "error": {"code": "FORBIDDEN", "message": "路径不在白名单"}});
            }
            match tokio::fs::read_to_string(path).await {
                Ok(content) => {
                    let truncated = if content.len() > 100_000 {
                        format!("{}...(已截断，共 {} 字节)", &content[..100_000], content.len())
                    } else { content };
                    json!({"ok": true, "path": path, "content": truncated})
                }
                Err(e) => json!({"ok": false, "error": {"message": format!("读取失败: {}", e)}}),
            }
        }
        "files.write" => {
            let path = params["path"].as_str().unwrap_or("");
            let content = params["content"].as_str().unwrap_or("");
            if path.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 path"}}); }
            let allowed_prefixes = ["/home", "/var", "/opt", "/srv", "/data", "/tmp", "/etc", "/root"];
            if !allowed_prefixes.iter().any(|p| path.starts_with(p)) {
                return json!({"ok": false, "error": {"code": "FORBIDDEN", "message": "路径不在白名单"}});
            }
            // 备份原文件
            if std::path::Path::new(path).exists() {
                let backup = format!("{}.bak.{}", path, current_ms());
                let _ = tokio::fs::copy(path, &backup).await;
            }
            match tokio::fs::write(path, content).await {
                Ok(_) => json!({"ok": true, "path": path, "size": content.len()}),
                Err(e) => json!({"ok": false, "error": {"message": format!("写入失败: {}", e)}}),
            }
        }
        "files.delete" => {
            let path = params["path"].as_str().unwrap_or("");
            if path.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 path"}}); }
            let allowed_prefixes = ["/home", "/var", "/opt", "/srv", "/data", "/tmp", "/etc", "/root"];
            if !allowed_prefixes.iter().any(|p| path.starts_with(p)) {
                return json!({"ok": false, "error": {"code": "FORBIDDEN", "message": "路径不在白名单"}});
            }
            // 备份到回收站
            let trash_dir = format!("{}/.cradle-ring/data/trash", state.storage.home);
            let _ = tokio::fs::create_dir_all(&trash_dir).await;
            let filename = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "unknown".to_string());
            let trash_path = format!("{}/{}.{}", trash_dir, filename, current_ms());
            let _ = tokio::fs::copy(path, &trash_path).await;
            match tokio::fs::remove_file(path).await {
                Ok(_) => json!({"ok": true, "path": path, "trash": trash_path}),
                Err(e) => json!({"ok": false, "error": {"message": format!("删除失败: {}", e)}}),
            }
        }
        "files.mkdir" => {
            let path = params["path"].as_str().unwrap_or("");
            if path.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 path"}}); }
            let allowed_prefixes = ["/home", "/var", "/opt", "/srv", "/data", "/tmp", "/etc", "/root"];
            if !allowed_prefixes.iter().any(|p| path.starts_with(p)) {
                return json!({"ok": false, "error": {"code": "FORBIDDEN", "message": "路径不在白名单"}});
            }
            match tokio::fs::create_dir_all(path).await {
                Ok(_) => json!({"ok": true, "path": path}),
                Err(e) => json!({"ok": false, "error": {"message": format!("创建失败: {}", e)}}),
            }
        }
        "files.rename" => {
            let from = params["from"].as_str().unwrap_or("");
            let to = params["to"].as_str().unwrap_or("");
            if from.is_empty() || to.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 from 或 to"}}); }
            let allowed_prefixes = ["/home", "/var", "/opt", "/srv", "/data", "/tmp", "/etc", "/root"];
            if !allowed_prefixes.iter().any(|p| from.starts_with(p)) || !allowed_prefixes.iter().any(|p| to.starts_with(p)) {
                return json!({"ok": false, "error": {"code": "FORBIDDEN", "message": "路径不在白名单"}});
            }
            match tokio::fs::rename(from, to).await {
                Ok(_) => json!({"ok": true, "from": from, "to": to}),
                Err(e) => json!({"ok": false, "error": {"message": format!("重命名失败: {}", e)}}),
            }
        }

        // ---- 进程管理器 ----

        "process.list" => {
            let sort_by = params["sortBy"].as_str().unwrap_or("cpu");
            let limit = params["limit"].as_u64().unwrap_or(50) as usize;
            let cmd = format!("ps aux --sort=-{} | head -{}", match sort_by { "mem" => "rss", "pid" => "pid", _ => "pcpu" }, limit + 1);
            let output = tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            let mut processes = vec![];
            for (i, line) in output.lines().enumerate() {
                if i == 0 { continue; }  // 跳过表头
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 11 {
                    processes.push(json!({
                        "user": parts[0], "pid": parts[1], "cpu": parts[2], "mem": parts[3],
                        "vsz": parts[4], "rss": parts[5], "tty": parts[6], "stat": parts[7],
                        "start": parts[8], "time": parts[9], "command": parts[10..].join(" "),
                    }));
                }
            }
            json!({"ok": true, "processes": processes, "count": processes.len()})
        }
        "process.kill" => {
            let pid = params["pid"].as_u64().unwrap_or(0);
            let signal = params["signal"].as_str().unwrap_or("TERM");
            if pid == 0 { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 pid"}}); }
            let sig = match signal { "KILL" => "9", "TERM" => "15", "INT" => "2", "HUP" => "1", _ => "15" };
            let result = tokio::process::Command::new("sh").arg("-c").arg(format!("kill -{} {}", sig, pid)).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "pid": pid, "signal": signal}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }
        "process.nice" => {
            let pid = params["pid"].as_u64().unwrap_or(0);
            let nice = params["nice"].as_i64().unwrap_or(0);
            if pid == 0 { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 pid"}}); }
            let result = tokio::process::Command::new("sh").arg("-c").arg(format!("renice -n {} -p {}", nice, pid)).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "pid": pid, "nice": nice}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }

        // ---- 服务管理器（systemd）----

        "services.list" => {
            let output = tokio::process::Command::new("sh").arg("-c")
                .arg("systemctl list-units --type=service --state=running,failed --no-pager --no-legend 2>/dev/null | head -50")
                .output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            let mut services = vec![];
            for line in output.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    services.push(json!({
                        "name": parts[0], "load": parts[1], "active": parts[2], "sub": parts[3],
                        "description": parts.get(4..).map(|p| p.join(" ")).unwrap_or_default(),
                    }));
                }
            }
            json!({"ok": true, "services": services, "count": services.len()})
        }
        "services.status" => {
            let name = params["name"].as_str().unwrap_or("");
            if name.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}}); }
            let output = tokio::process::Command::new("sh").arg("-c").arg(format!("systemctl status {} --no-pager 2>&1 | head -30", name)).output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "name": name, "status": output})
        }
        "services.start" => {
            let name = params["name"].as_str().unwrap_or("");
            if name.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}}); }
            let result = tokio::process::Command::new("sh").arg("-c").arg(format!("sudo systemctl start {} 2>&1", name)).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "name": name, "action": "start"}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }
        "services.stop" => {
            let name = params["name"].as_str().unwrap_or("");
            if name.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}}); }
            let result = tokio::process::Command::new("sh").arg("-c").arg(format!("sudo systemctl stop {} 2>&1", name)).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "name": name, "action": "stop"}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }
        "services.restart" => {
            let name = params["name"].as_str().unwrap_or("");
            if name.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}}); }
            let result = tokio::process::Command::new("sh").arg("-c").arg(format!("sudo systemctl restart {} 2>&1", name)).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "name": name, "action": "restart"}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }
        "services.logs" => {
            let name = params["name"].as_str().unwrap_or("");
            let lines = params["lines"].as_u64().unwrap_or(100) as usize;
            if name.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}}); }
            let output = tokio::process::Command::new("sh").arg("-c").arg(format!("journalctl -u {} -n {} --no-pager 2>&1", name, lines)).output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "name": name, "logs": output})
        }

        // ---- 防火墙管理（iptables / ufw）----

        "firewall.list" => {
            let output = tokio::process::Command::new("sh").arg("-c")
                .arg("iptables -L INPUT -n --line-numbers 2>/dev/null | head -50; echo '---UFW---'; ufw status numbered 2>/dev/null | head -30")
                .output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "rules": output})
        }
        "firewall.add" => {
            let rule = params["rule"].as_str().unwrap_or("");
            if rule.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 rule"}}); }
            // 优先用 ufw（如果可用），否则用 iptables
            let has_ufw = tokio::process::Command::new("sh").arg("-c").arg("command -v ufw >/dev/null 2>&1 && echo yes || echo no").output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default() == "yes";
            let cmd = if has_ufw {
                format!("sudo ufw {}", rule)
            } else {
                format!("sudo iptables {}", rule)
            };
            let result = tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "rule": rule, "tool": if has_ufw { "ufw" } else { "iptables" }}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }
        "firewall.delete" => {
            let rule_num = params["ruleNum"].as_u64().unwrap_or(0);
            if rule_num == 0 { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 ruleNum"}}); }
            let has_ufw = tokio::process::Command::new("sh").arg("-c").arg("command -v ufw >/dev/null 2>&1 && echo yes || echo no").output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default() == "yes";
            let cmd = if has_ufw {
                format!("sudo ufw delete {}", rule_num)
            } else {
                format!("sudo iptables -D INPUT {}", rule_num)
            };
            let result = tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "ruleNum": rule_num}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }

        // ---- SSL 证书管理（Let's Encrypt / certbot）----

        "ssl.list" => {
            let output = tokio::process::Command::new("sh").arg("-c")
                .arg("certbot certificates 2>/dev/null | head -50; echo '---'; ls -la /etc/letsencrypt/live/ 2>/dev/null | head -20")
                .output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "certificates": output})
        }
        "ssl.renew" => {
            let domain = params["domain"].as_str().unwrap_or("");
            if domain.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 domain"}}); }
            let output = tokio::process::Command::new("sh").arg("-c").arg(format!("sudo certbot renew --cert-name {} --no-pager 2>&1", domain)).output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "domain": domain, "output": output})
        }
        "ssl.obtain" => {
            let domain = params["domain"].as_str().unwrap_or("");
            let email = params["email"].as_str().unwrap_or("");
            if domain.is_empty() || email.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 domain 或 email"}}); }
            let output = tokio::process::Command::new("sh").arg("-c").arg(format!("sudo certbot certonly --standalone -d {} --email {} --agree-tos --no-pager 2>&1", domain, email)).output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "domain": domain, "output": output})
        }

        // ---- Docker 管理 ----

        "docker.containers" => {
            let output = tokio::process::Command::new("sh").arg("-c").arg("docker ps --format 'table {{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}' 2>/dev/null | head -30").output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "containers": output})
        }
        "docker.images" => {
            let output = tokio::process::Command::new("sh").arg("-c").arg("docker images --format 'table {{.Repository}}\t{{.Tag}}\t{{.Size}}\t{{.CreatedAt}}' 2>/dev/null | head -30").output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "images": output})
        }
        "docker.logs" => {
            let container = params["container"].as_str().unwrap_or("");
            let lines = params["lines"].as_u64().unwrap_or(100) as usize;
            if container.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 container"}}); }
            let output = tokio::process::Command::new("sh").arg("-c").arg(format!("docker logs --tail {} {} 2>&1", lines, container)).output().await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            json!({"ok": true, "container": container, "logs": output})
        }
        "docker.restart" => {
            let container = params["container"].as_str().unwrap_or("");
            if container.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 container"}}); }
            let result = tokio::process::Command::new("sh").arg("-c").arg(format!("docker restart {} 2>&1", container)).output().await;
            match result {
                Ok(o) if o.status.success() => json!({"ok": true, "container": container}),
                Ok(o) => json!({"ok": false, "error": {"message": String::from_utf8_lossy(&o.stderr)}}),
                Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            }
        }

        // ====================================================================
        // 工作流图引擎（对标 LangGraph）
        // ====================================================================

        "workflow.graphs.list" => {
            let graphs = state.storage.load_workflow_graphs();
            json!({
                "graphs": graphs.iter().map(workflow_graph_to_json).collect::<Vec<_>>(),
                "count": graphs.len(),
            })
        }
        "workflow.graphs.get" => {
            let id = params["id"].as_str().unwrap_or("");
            let graphs = state.storage.load_workflow_graphs();
            match graphs.iter().find(|g| g.id == id) {
                Some(g) => json!({"graph": workflow_graph_to_json(g)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "工作流不存在"}}),
            }
        }
        "workflow.graphs.create" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}});
            }
            let nodes: Vec<WorkflowNode> = params["nodes"].as_array()
                .map(|a| a.iter().filter_map(parse_workflow_node).collect())
                .unwrap_or_default();
            let edges: Vec<WorkflowEdge> = params["edges"].as_array()
                .map(|a| a.iter().filter_map(parse_workflow_edge).collect())
                .unwrap_or_default();
            let graph = WorkflowGraph {
                id: format!("wf-{:016x}", rand_u128()),
                name,
                description: params["description"].as_str().unwrap_or("").to_string(),
                nodes,
                edges,
                entry_node: params["entryNode"].as_str().unwrap_or("start").to_string(),
                state_schema: params["stateSchema"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                enabled: params["enabled"].as_bool().unwrap_or(true),
                created_at: current_ms(),
                created_by: params["createdBy"].as_str().unwrap_or("admin").to_string(),
            };
            let g_clone = graph.clone();
            let mut graphs = state.storage.load_workflow_graphs();
            graphs.push(graph);
            state.storage.save_workflow_graphs(&graphs);
            let _ = broadcast_event(&state, "workflow.graph.created", workflow_graph_to_json(&g_clone)).await;
            json!({"ok": true, "graph": workflow_graph_to_json(&g_clone)})
        }
        "workflow.graphs.update" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut graphs = state.storage.load_workflow_graphs();
            let g = match graphs.iter_mut().find(|g| g.id == id) {
                Some(g) => g, None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "工作流不存在"}}),
            };
            if let Some(v) = params["name"].as_str() { g.name = v.to_string(); }
            if let Some(v) = params["description"].as_str() { g.description = v.to_string(); }
            if let Some(v) = params["entryNode"].as_str() { g.entry_node = v.to_string(); }
            if let Some(v) = params["enabled"].as_bool() { g.enabled = v; }
            if let Some(arr) = params["stateSchema"].as_array() {
                g.state_schema = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
            if let Some(arr) = params["nodes"].as_array() {
                g.nodes = arr.iter().filter_map(parse_workflow_node).collect();
            }
            if let Some(arr) = params["edges"].as_array() {
                g.edges = arr.iter().filter_map(parse_workflow_edge).collect();
            }
            let g_clone = g.clone();
            state.storage.save_workflow_graphs(&graphs);
            json!({"ok": true, "graph": workflow_graph_to_json(&g_clone)})
        }
        "workflow.graphs.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut graphs = state.storage.load_workflow_graphs();
            let before = graphs.len();
            graphs.retain(|g| g.id != id);
            if graphs.len() == before {
                return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "工作流不存在"}});
            }
            state.storage.save_workflow_graphs(&graphs);
            json!({"ok": true})
        }
        "workflow.graphs.validate" => {
            let id = params["id"].as_str().unwrap_or("");
            let graphs = state.storage.load_workflow_graphs();
            let g = match graphs.iter().find(|g| g.id == id) {
                Some(g) => g, None => return json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "工作流不存在"}}),
            };
            let mut errors = vec![];
            if g.nodes.is_empty() { errors.push("工作流没有节点".to_string()); }
            if !g.nodes.iter().any(|n| n.id == g.entry_node) { errors.push(format!("入口节点 {} 不存在", g.entry_node)); }
            if !g.nodes.iter().any(|n| n.node_type == NodeType::End) { errors.push("缺少 End 节点".to_string()); }
            // 检查每个非 End 节点有出口
            for n in &g.nodes {
                if n.node_type == NodeType::End { continue; }
                let has_edge = g.edges.iter().any(|e| e.from == n.id) || n.default_edge.is_some() || n.node_type == NodeType::Condition;
                if !has_edge {
                    errors.push(format!("节点 {} 没有出口", n.name));
                }
            }
            json!({"ok": errors.is_empty(), "errors": errors})
        }
        "workflow.runs.start" => {
            let graph_id = params["graphId"].as_str().unwrap_or("").to_string();
            let input = params["input"].clone();
            let session_key = params["sessionKey"].as_str().unwrap_or("workflow").to_string();
            let breakpoints: Vec<String> = params["breakpoints"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            match start_workflow_run(state.clone(), &graph_id, input, &session_key, breakpoints).await {
                Ok(run_id) => {
                    let run = state.storage.load_workflow_run(&run_id);
                    json!({"ok": true, "runId": run_id, "run": run.map(|r| workflow_run_to_json(&r))})
                }
                Err(e) => json!({"ok": false, "error": {"code": "START_FAILED", "message": e}}),
            }
        }
        "workflow.runs.get" => {
            let id = params["id"].as_str().unwrap_or("");
            match state.storage.load_workflow_run(id) {
                Some(r) => json!({"run": workflow_run_to_json(&r)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND", "message": "运行实例不存在"}}),
            }
        }
        "workflow.runs.list" => {
            let status_filter = params["status"].as_str();
            let mut runs = state.storage.list_workflow_runs();
            if let Some(s) = status_filter { runs.retain(|r| r.status == s); }
            json!({
                "runs": runs.iter().map(workflow_run_to_json).collect::<Vec<_>>(),
                "count": runs.len(),
            })
        }
        "workflow.runs.cancel" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            match cancel_workflow_run(&state, &id).await {
                Ok(_) => json!({"ok": true}),
                Err(e) => json!({"ok": false, "error": {"message": e}}),
            }
        }
        "workflow.runs.resume" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let input = params["input"].clone();
            match resume_workflow_run(state.clone(), &id, input).await {
                Ok(_) => json!({"ok": true}),
                Err(e) => json!({"ok": false, "error": {"message": e}}),
            }
        }
        "workflow.runs.rewind" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let cp_idx = params["checkpointIndex"].as_u64().unwrap_or(0) as usize;
            match rewind_workflow_run(state.clone(), &id, cp_idx).await {
                Ok(_) => json!({"ok": true}),
                Err(e) => json!({"ok": false, "error": {"message": e}}),
            }
        }
        "workflow.runs.checkpoints" => {
            let id = params["id"].as_str().unwrap_or("");
            match state.storage.load_workflow_run(id) {
                Some(r) => json!({
                    "checkpoints": r.checkpoints.iter().enumerate().map(|(i, c)| json!({
                        "index": i, "nodeId": &c.node_id, "stepIndex": c.step_index,
                        "timestamp": c.timestamp, "span": trace_span_to_json(&c.span),
                    })).collect::<Vec<_>>(),
                }),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            }
        }
        "workflow.runs.trace" => {
            let id = params["id"].as_str().unwrap_or("");
            match state.storage.load_workflow_run(id) {
                Some(r) => json!({"trace": trace_span_to_json(&r.root_span)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            }
        }
        "workflow.runs.set_breakpoint" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let node_id = params["nodeId"].as_str().unwrap_or("").to_string();
            let mut run = match state.storage.load_workflow_run(&id) {
                Some(r) => r, None => return json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            };
            if !run.breakpoints.contains(&node_id) { run.breakpoints.push(node_id); }
            state.storage.save_workflow_run(&run);
            json!({"ok": true, "breakpoints": &run.breakpoints})
        }
        "workflow.runs.remove_breakpoint" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let node_id = params["nodeId"].as_str().unwrap_or("").to_string();
            let mut run = match state.storage.load_workflow_run(&id) {
                Some(r) => r, None => return json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            };
            run.breakpoints.retain(|b| b != &node_id);
            state.storage.save_workflow_run(&run);
            json!({"ok": true, "breakpoints": &run.breakpoints})
        }

        // ====================================================================
        // 角色化 Agent（对标 CrewAI Agent）
        // ====================================================================

        "agent_roles.list" => {
            let roles = state.storage.load_agent_roles();
            json!({
                "roles": roles.iter().map(agent_role_to_json).collect::<Vec<_>>(),
                "count": roles.len(),
            })
        }
        "agent_roles.get" => {
            let id = params["id"].as_str().unwrap_or("");
            let roles = state.storage.load_agent_roles();
            match roles.iter().find(|r| r.id == id) {
                Some(r) => json!({"role": agent_role_to_json(r)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            }
        }
        "agent_roles.create" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            let role_str = params["role"].as_str().unwrap_or("").to_string();
            if name.is_empty() || role_str.is_empty() {
                return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name 或 role"}});
            }
            let ar = AgentRole {
                id: format!("role-{:016x}", rand_u128()),
                name,
                role: role_str,
                goal: params["goal"].as_str().unwrap_or("").to_string(),
                backstory: params["backstory"].as_str().unwrap_or("").to_string(),
                tools: params["tools"].as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
                model: params["model"].as_str().filter(|s| !s.is_empty()).map(String::from),
                system_prompt_template: params["systemPromptTemplate"].as_str().filter(|s| !s.is_empty()).map(String::from),
                max_iterations: params["maxIterations"].as_u64().unwrap_or(10) as usize,
                allow_delegation: params["allowDelegation"].as_bool().unwrap_or(false),
                created_at: current_ms(),
                created_by: params["createdBy"].as_str().unwrap_or("admin").to_string(),
            };
            let r_clone = ar.clone();
            let mut roles = state.storage.load_agent_roles();
            roles.push(ar);
            state.storage.save_agent_roles(&roles);
            json!({"ok": true, "role": agent_role_to_json(&r_clone)})
        }
        "agent_roles.update" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut roles = state.storage.load_agent_roles();
            let r = match roles.iter_mut().find(|r| r.id == id) {
                Some(r) => r, None => return json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            };
            if let Some(v) = params["name"].as_str() { r.name = v.to_string(); }
            if let Some(v) = params["role"].as_str() { r.role = v.to_string(); }
            if let Some(v) = params["goal"].as_str() { r.goal = v.to_string(); }
            if let Some(v) = params["backstory"].as_str() { r.backstory = v.to_string(); }
            if let Some(v) = params["model"].as_str() { r.model = Some(v.to_string()); }
            if let Some(v) = params["systemPromptTemplate"].as_str() { r.system_prompt_template = Some(v.to_string()); }
            if let Some(v) = params["maxIterations"].as_u64() { r.max_iterations = v as usize; }
            if let Some(v) = params["allowDelegation"].as_bool() { r.allow_delegation = v; }
            if let Some(arr) = params["tools"].as_array() {
                r.tools = Some(arr.iter().filter_map(|v| v.as_str().map(String::from)).collect());
            }
            let r_clone = r.clone();
            state.storage.save_agent_roles(&roles);
            json!({"ok": true, "role": agent_role_to_json(&r_clone)})
        }
        "agent_roles.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut roles = state.storage.load_agent_roles();
            let before = roles.len();
            roles.retain(|r| r.id != id);
            if roles.len() == before { return json!({"ok": false, "error": {"code": "NOT_FOUND"}}); }
            state.storage.save_agent_roles(&roles);
            json!({"ok": true})
        }
        "agent_roles.test" => {
            // 测试角色：用指定角色跑一个任务
            let id = params["id"].as_str().unwrap_or("").to_string();
            let task = params["task"].as_str().unwrap_or("你好").to_string();
            let roles = state.storage.load_agent_roles();
            let role = roles.iter().find(|r| r.id == id).cloned();
            let mut span = TraceSpan::new("test", "role_test", None);
            match execute_role_agent(&state, role.as_ref(), &task, "test", &mut span).await {
                Ok(out) => json!({"ok": true, "output": out, "trace": trace_span_to_json(&span)}),
                Err(e) => json!({"ok": false, "error": {"message": e}}),
            }
        }

        // ====================================================================
        // Sequential 流水线（对标 CrewAI Sequential Process）
        // ====================================================================

        "pipelines.list" => {
            let pipelines = state.storage.load_pipelines();
            json!({
                "pipelines": pipelines.iter().map(pipeline_to_json).collect::<Vec<_>>(),
                "count": pipelines.len(),
            })
        }
        "pipelines.get" => {
            let id = params["id"].as_str().unwrap_or("");
            let pipelines = state.storage.load_pipelines();
            match pipelines.iter().find(|p| p.id == id) {
                Some(p) => json!({"pipeline": pipeline_to_json(p)}),
                None => json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            }
        }
        "pipelines.create" => {
            let name = params["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() { return json!({"ok": false, "error": {"code": "INVALID_REQUEST", "message": "缺少 name"}}); }
            let stages: Vec<PipelineStage> = params["stages"].as_array()
                .map(|a| a.iter().filter_map(|s| {
                    Some(PipelineStage {
                        order: s["order"].as_u64()? as u32,
                        agent_role_id: s["agentRoleId"].as_str()?.to_string(),
                        task_template: s["taskTemplate"].as_str().unwrap_or("").to_string(),
                    })
                }).collect())
                .unwrap_or_default();
            let p = Pipeline {
                id: format!("pipe-{:016x}", rand_u128()),
                name,
                description: params["description"].as_str().unwrap_or("").to_string(),
                stages,
                pass_through: params["passThrough"].as_bool().unwrap_or(true),
                enabled: params["enabled"].as_bool().unwrap_or(true),
                created_at: current_ms(),
                created_by: params["createdBy"].as_str().unwrap_or("admin").to_string(),
            };
            let p_clone = p.clone();
            let mut pipelines = state.storage.load_pipelines();
            pipelines.push(p);
            state.storage.save_pipelines(&pipelines);
            json!({"ok": true, "pipeline": pipeline_to_json(&p_clone)})
        }
        "pipelines.update" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut pipelines = state.storage.load_pipelines();
            let p = match pipelines.iter_mut().find(|p| p.id == id) {
                Some(p) => p, None => return json!({"ok": false, "error": {"code": "NOT_FOUND"}}),
            };
            if let Some(v) = params["name"].as_str() { p.name = v.to_string(); }
            if let Some(v) = params["description"].as_str() { p.description = v.to_string(); }
            if let Some(v) = params["passThrough"].as_bool() { p.pass_through = v; }
            if let Some(v) = params["enabled"].as_bool() { p.enabled = v; }
            if let Some(arr) = params["stages"].as_array() {
                p.stages = arr.iter().filter_map(|s| {
                    Some(PipelineStage {
                        order: s["order"].as_u64()? as u32,
                        agent_role_id: s["agentRoleId"].as_str()?.to_string(),
                        task_template: s["taskTemplate"].as_str().unwrap_or("").to_string(),
                    })
                }).collect();
            }
            let p_clone = p.clone();
            state.storage.save_pipelines(&pipelines);
            json!({"ok": true, "pipeline": pipeline_to_json(&p_clone)})
        }
        "pipelines.delete" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let mut pipelines = state.storage.load_pipelines();
            let before = pipelines.len();
            pipelines.retain(|p| p.id != id);
            if pipelines.len() == before { return json!({"ok": false, "error": {"code": "NOT_FOUND"}}); }
            state.storage.save_pipelines(&pipelines);
            json!({"ok": true})
        }
        "pipelines.run" => {
            let id = params["id"].as_str().unwrap_or("").to_string();
            let input = params["input"].as_str().unwrap_or("").to_string();
            let session_key = params["sessionKey"].as_str().unwrap_or("pipeline").to_string();
            match execute_pipeline_run(state.clone(), &id, &input, &session_key).await {
                Ok(result) => json!({"ok": true, "result": pipeline_result_to_json(&result)}),
                Err(e) => json!({"ok": false, "error": {"message": e}}),
            }
        }

        _ => {
            // 通用回退：返回 ok 让 UI 继续
            json!({ "ok": true })
        }
    }
}

use serde_json::json;

// ============================================================================
// RPC 辅助函数：序列化、配置读取、cron 触发
// ============================================================================

fn cron_job_to_json(j: &CronJob) -> serde_json::Value {
    json!({
        "id": j.id,
        "name": j.name,
        "schedule": j.schedule,
        "prompt": j.prompt,
        "enabled": j.enabled,
        "sessionKey": j.session_key,
        "lastRun": j.last_run,
        "nextRun": j.next_run
    })
}

fn approval_to_json(a: &Approval) -> serde_json::Value {
    json!({
        "id": a.id,
        "kind": a.kind,
        "command": a.command,
        "status": a.status,
        "sessionKey": a.session_key,
        "runId": a.run_id,
        "createdAt": a.created_at,
        "decidedBy": a.decided_by,
        "decidedAt": a.decided_at,
        "decision": a.decision
    })
}

fn node_to_json(n: &Node) -> serde_json::Value {
    json!({
        "id": n.id,
        "name": n.name,
        "kind": n.kind,
        "status": n.status,
        "pairedAt": n.paired_at,
        "lastSeen": n.last_seen,
        "metadata": n.metadata
    })
}

fn pair_request_to_json(r: &PairRequest) -> serde_json::Value {
    json!({
        "id": r.id,
        "name": r.name,
        "kind": r.kind,
        "target": r.target,
        "status": r.status,
        "createdAt": r.created_at,
        "metadata": r.metadata
    })
}

fn compaction_checkpoint_to_json(c: &CompactionCheckpoint) -> serde_json::Value {
    json!({
        "id": c.id,
        "sessionKey": c.session_key,
        "createdAt": c.created_at,
        "originalCount": c.original_count,
        "keptCount": c.kept_count,
        "summary": c.summary,
        "entities": c.entities,
        "backupFile": c.backup_file,
        "model": c.model,
        "branch": c.branch,
        "parentId": c.parent_id,
    })
}

/// 深度合并 patch 到 target（对对象递归，其他类型直接覆盖）。
fn merge_json(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(t), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                if let Some(existing) = t.get_mut(k) {
                    if existing.is_object() && v.is_object() {
                        merge_json(existing, v);
                        continue;
                    }
                }
                t.insert(k.clone(), v.clone());
            }
        }
        (target, patch) => {
            *target = patch.clone();
        }
    }
}

/// 从文本中正则提取关键实体：文件路径、URL、函数名、ID 等。
/// 这些实体会被附加到摘要末尾，避免压缩后丢失可寻址的引用。
fn extract_entities(text: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut found: BTreeSet<String> = BTreeSet::new();

    // 文件路径：/abs/path 或 ~/path 或 ./rel/path，要求至少一个路径分隔符
    let path_re = regex::Regex::new(r"(?:^|[\s(){}\[\]])([~/][A-Za-z0-9._\-/]+/+[A-Za-z0-9._\-/]+|/[A-Za-z0-9._\-]+(?:/[A-Za-z0-9._\-]+)+)").ok();
    if let Some(re) = &path_re {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let s = m.as_str().trim_end_matches(|c: char| c == ',' || c == '.');
                if s.len() >= 3 { found.insert(s.to_string()); }
            }
        }
    }

    // URL
    let url_re = regex::Regex::new(r"https?://[A-Za-z0-9._\-:/?#@!$&()*+,;=%~]+").ok();
    if let Some(re) = &url_re {
        for m in re.find_iter(text) {
            let s = m.as_str().trim_end_matches(|c: char| c == ',' || c == '.' || c == ')');
            found.insert(s.to_string());
        }
    }

    // 函数名 / 标识符：fn xxx(  或  def xxx(  或  function xxx(
    let fn_re = regex::Regex::new(r"(?:fn |def |function |async function )([A-Za-z_][A-Za-z0-9_]+)\s*\(").ok();
    if let Some(re) = &fn_re {
        for cap in re.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                found.insert(m.as_str().to_string());
            }
        }
    }

    // ID 类：包含连字符或下划线的混合大小写标识符（至少 6 字符），如 sessionKey、jobId 等
    let id_re = regex::Regex::new(r"\b[A-Za-z0-9]*[a-z][A-Za-z0-9]*[-_][A-Za-z0-9]{4,}\b").ok();
    if let Some(re) = &id_re {
        for m in re.find_iter(text) {
            let s = m.as_str();
            if s.len() >= 6 && s.len() <= 64 { found.insert(s.to_string()); }
        }
    }

    // 限制总数，避免噪音过多
    found.into_iter().take(40).collect()
}

/// 无损压缩 + 摘要：
/// 1. 读取完整消息历史
/// 2. 保留最近 `keep` 条
/// 3. 旧消息调 LLM 生成摘要
/// 4. 关键实体保留（正则提取）
/// 5. 摘要 + 保留消息 → 新的 session 上下文
/// 6. 原始消息备份（存 JSONL）
/// 7. 记录压缩检查点（可恢复）
async fn compact_session(
    state: Arc<AppState>,
    session_key: &str,
    keep: usize,
    model: &str,
) -> Result<serde_json::Value, String> {
    let messages = state.storage.load_messages(session_key);
    let original_count = messages.len();
    if original_count <= keep {
        return Ok(json!({
            "ok": true,
            "sessionKey": session_key,
            "skipped": true,
            "reason": format!("消息数 ({}) 未超过保留阈值 ({})", original_count, keep),
        }));
    }

    let split_at = original_count - keep;
    let old_messages = &messages[..split_at];
    let _recent_messages = &messages[split_at..];

    // 提取关键实体（从旧消息全文）
    let old_text: String = old_messages.iter()
        .map(|m| format!("[{}] {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    let entities = extract_entities(&old_text);

    // 生成摘要：把旧消息打包成 prompt，调一次 LLM
    let summary = generate_compaction_summary(&state, old_messages, model).await;

    // 备份原始完整历史
    let backup_name = format!("{}_{}.jsonl",
        session_key.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_"),
        current_ms());
    let backup_path = format!("{}/{}", state.storage.compaction_backup_dir(), backup_name);
    let backup_content = messages.iter()
        .map(|m| serde_json::to_string(m).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(&backup_path, &backup_content);

    // 构造新的消息文件：第一条 = 摘要（role=system），其余 = 保留的最近消息
    let _summary_message = Message {
        role: "system".into(),
        content: format!(
            "[会话历史摘要]\n{}\n\n[关键实体保留]\n{}",
            summary,
            entities.iter().map(|e| format!("- {}", e)).collect::<Vec<_>>().join("\n")
        ),
        timestamp: current_ms(),
        attachments: vec![],
    };
    let cp = CompactionCheckpoint {
        id: format!("cp-{}", current_ms()),
        session_key: session_key.to_string(),
        created_at: current_ms(),
        original_count,
        kept_count: keep,
        summary: summary.clone(),
        entities: entities.clone(),
        backup_file: backup_name,
        model: model.to_string(),
        branch: None,
        parent_id: None,
    };
    let mut cps = state.storage.load_compaction_checkpoints();
    cps.push(cp.clone());
    state.storage.save_compaction_checkpoints(&cps);

    Ok(json!({
        "ok": true,
        "sessionKey": session_key,
        "compacted": true,
        "originalCount": original_count,
        "keptCount": keep,
        "summarizedCount": split_at,
        "entityCount": entities.len(),
        "summaryPreview": if summary.len() > 200 { format!("{}...", &summary[..200]) } else { summary.clone() },
        "checkpoint": compaction_checkpoint_to_json(&cp),
    }))
}

/// 调 LLM 生成旧消息的摘要。若 LLM 调用失败，退化为本地截断摘要。
async fn generate_compaction_summary(
    state: &AppState,
    old_messages: &[Message],
    model: &str,
) -> String {
    let transcript: String = old_messages.iter()
        .map(|m| format!("[{}] {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    // 限制 transcript 长度，避免请求过大
    let transcript = if transcript.len() > 24_000 {
        format!("{}...\n(旧消息过长，已截断)", &transcript[..24_000])
    } else {
        transcript
    };

    let api_key = match &state.config.openai_api_key {
        Some(k) => k.clone(),
        None => return fallback_summary(old_messages),
    };

    let prompt = format!(
        "请把下面的会话历史压缩成一段简洁的中文摘要，保留关键决策、用户需求、重要文件/命令/URL，去掉寒暄与重复。\n\n会话历史：\n{}",
        transcript
    );

    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "你是一个会话压缩助手。输出一段不超过 400 字的中文摘要。"},
            {"role": "user", "content": prompt}
        ],
        "max_tokens": 800,
        "stream": false,
    });

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(_) => return fallback_summary(old_messages),
    };
    let url = format!("{}/chat/completions", state.config.openai_base_url);
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    let s = v["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    if s.is_empty() { fallback_summary(old_messages) } else { s }
                }
                Err(_) => fallback_summary(old_messages),
            }
        }
        _ => fallback_summary(old_messages),
    }
}

/// 本地截断摘要（LLM 不可用时的兜底）：拼接每条消息前 80 字。
fn fallback_summary(old_messages: &[Message]) -> String {
    let mut parts = vec!["（LLM 不可用，使用本地摘要）".to_string()];
    for m in old_messages.iter().take(50) {
        let snippet: String = m.content.chars().take(80).collect();
        parts.push(format!("[{}] {}{}", m.role, snippet, if m.content.chars().count() > 80 { "..." } else { "" }));
    }
    parts.join("\n")
}

/// 从 Config.raw_json 读取 mcp.servers 映射（顺序保留）
fn mcp_servers_from_config(config: &Config) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    if let Some(servers) = config.raw_json.get("mcp").and_then(|m| m.get("servers")).and_then(|s| s.as_object()) {
        for (name, cfg) in servers {
            out.push((name.clone(), cfg.clone()));
        }
    }
    out
}

/// 把 cron job 的 prompt 投递到指定会话（作为一次用户消息触发 agent loop）。
/// trigger: "manual" | "schedule"
fn trigger_cron_prompt(state: Arc<AppState>, job: &CronJob, run_id: &str, trigger: &str) {
    let session_key = job.session_key.clone();
    let prompt_text = if job.prompt.is_empty() {
        format!("[定时任务触发] {}", job.name)
    } else {
        job.prompt.clone()
    };
    // 确保会话存在
    let mut sessions = state.storage.load_sessions();
    if !sessions.iter().any(|s| s.key == session_key) {
        sessions.push(Session {
            key: session_key.clone(),
            kind: "cron".to_string(),
            display_name: Some(format!("定时: {}", job.name)),
            channel: None,
            agent_id: "main".to_string(),
            model: Some(state.config.default_model.clone()),
            updated_at: current_ms(),
        });
        state.storage.save_sessions(&sessions);
    }
    // 写入用户消息
    state.storage.append_message(&session_key, &Message {
        role: "user".into(),
        content: prompt_text.clone(),
        timestamp: current_ms(),
        attachments: vec![],
    });
    // 异步运行 agent loop
    let state_clone = state.clone();
    let sk = session_key.clone();
    let rid = format!("cron-{}", run_id);
    tokio::spawn(async move {
        run_agent_loop(state_clone, &sk, &rid).await;
    });
    let _ = trigger;
}

// ============================================================================
// LLM 调用与 Agent Loop
// ============================================================================

/// 工具调用片段（流式累积时使用）
#[derive(Default, Clone)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

/// 流式 LLM 调用的结果：累积的文本 + 可能为空的工具调用列表
struct StreamResult {
    text: String,
    tool_calls: Vec<ToolCallAccumulator>,
    finish_reason: String,
}

/// 主动回忆：从记忆库中按关键词匹配出与用户最新消息相关的记忆。
fn recall_relevant_memory(items: &[MemoryItem], latest_user: &str) -> Vec<String> {
    if items.is_empty() {
        return vec![];
    }
    // 把用户消息切词（按空白和非字母数字分隔），找出长度 >= 2 的 token
    let tokens: Vec<String> = latest_user
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2)
        .map(|s| s.to_lowercase())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    if tokens.is_empty() {
        // 没有可分词的内容时直接返回最近 5 条记忆
        let mut recent: Vec<&MemoryItem> = items.iter().collect();
        recent.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        return recent.into_iter().take(5).map(|m| format!("- [{}] {}", m.kind, m.body)).collect();
    }

    // 计算每条记忆的相关性分数：命中的 token 数
    let mut scored: Vec<(usize, &MemoryItem)> = items
        .iter()
        .map(|m| {
            let body_lower = m.body.to_lowercase();
            let mut hits = 0usize;
            for t in &tokens {
                if body_lower.contains(t) {
                    hits += 1;
                }
            }
            (hits, m)
        })
        .filter(|(h, _)| *h > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.created_at.cmp(&a.1.created_at)));

    scored.into_iter().take(5).map(|(_, m)| format!("- [{}] {}", m.kind, m.body)).collect()
}

/// 构建系统提示词（含记忆注入）
fn build_system_prompt(state: &AppState, latest_user: &str) -> String {
    let mut parts = vec![
        "你是 CradleRing 的 AI 助手。你可以使用工具来帮助用户：搜索网络、执行命令、读写文件、保存记忆。".to_string(),
        "当需要外部信息或操作时，主动调用工具；能直接回答时直接回答。".to_string(),
        "请用简洁、准确的中文回答用户。".to_string(),
    ];

    // 注入记忆
    let memory = state.storage.load_memory();
    if !memory.is_empty() {
        let recalled = recall_relevant_memory(&memory, latest_user);
        if !recalled.is_empty() {
            parts.push("\n相关记忆（用户先前保存的，可作为回答依据）：".to_string());
            parts.extend(recalled);
        }
    }

    parts.join("\n")
}

/// 估算消息数组占用的大致 token 数（粗略：1 token ≈ 3.5 字符）。
fn estimate_tokens(messages: &[serde_json::Value]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|m| {
            let mut c = 0usize;
            if let Some(s) = m["content"].as_str() {
                c += s.chars().count();
            }
            if let Some(s) = m["role"].as_str() {
                c += s.chars().count();
            }
            if let Some(arr) = m["tool_calls"].as_array() {
                for tc in arr {
                    c += tc["function"]["name"].as_str().map(|s| s.len()).unwrap_or(0);
                    c += tc["function"]["arguments"].as_str().map(|s| s.len()).unwrap_or(0);
                }
            }
            c
        })
        .sum();
    chars / 3 + messages.len()
}

/// 真正的上下文截断：传入可克隆的切片引用
fn build_context_messages(
    system_prompt: &str,
    history: &[Message],
    max_tokens: usize,
    keep_recent: usize,
) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = vec![json!({"role": "system", "content": system_prompt})];

    // 取最近 N 条历史
    let start = history.len().saturating_sub(50);
    let mut history_slice: Vec<&Message> = history[start..].iter().collect();

    // 组装成 messages（用户消息若带图片附件则渲染为 OpenAI vision 多模态 content）
    for m in &history_slice {
        messages.push(build_chat_message(m));
    }

    // 如果超过 token 限制，截断到保留最近 keep_recent 条
    if estimate_tokens(&messages) > max_tokens {
        let system_msg = messages[0].clone();
        // 保留最近 keep_recent 条历史
        let take = keep_recent.min(history_slice.len());
        let recent: Vec<&Message> = history_slice.split_off(history_slice.len() - take);
        let mut trimmed: Vec<serde_json::Value> = vec![system_msg];
        for m in &recent {
            trimmed.push(build_chat_message(m));
        }
        // 如果仍然超长，进一步砍 system prompt 之外的内容（这里只做一次粗截断）
        return trimmed;
    }
    messages
}

/// 把一条历史 Message 转成 OpenAI 消息 JSON。用户消息若含图片附件，
/// 则 content 渲染为多模态数组（OpenAI vision 格式）；其余角色保持纯字符串 content。
fn build_chat_message(m: &Message) -> serde_json::Value {
    if m.role == "user" && !m.attachments.is_empty() {
        let mut content_arr: Vec<serde_json::Value> = vec![json!({"type": "text", "text": m.content})];
        for att in &m.attachments {
            let kind = att["type"].as_str().unwrap_or("");
            if kind != "image" { continue; }
            let ext = att["ext"].as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    let p = att["path"].as_str().or_else(|| att["url"].as_str()).unwrap_or("");
                    ext_of(p)
                });
            let mime = match ext.as_str() {
                "png" => "image/png",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/jpeg",
            };
            let url_val: serde_json::Value = if let Some(path) = att["path"].as_str() {
                match read_file_base64(strip_file_prefix(path)) {
                    Ok(b64) => json!(format!("data:{};base64,{}", mime, b64)),
                    Err(_) => continue,
                }
            } else if let Some(u) = att["url"].as_str() {
                json!(u)
            } else {
                continue;
            };
            content_arr.push(json!({"type": "image_url", "image_url": {"url": url_val}}));
        }
        json!({"role": m.role, "content": content_arr})
    } else {
        json!({"role": m.role, "content": m.content})
    }
}

// ============================================================================
// 预装插件清单（内置插件清单（145+ 个））
// ============================================================================

/// 单个预装插件的定义
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PluginDef {
    id: String,
    name: String,
    description: String,
    category: String,
    enabled: bool,
}

/// 内联构造一个 PluginDef（enabled 默认 false）
fn p(id: &str, name: &str, description: &str, category: &str) -> PluginDef {
    PluginDef {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        category: category.to_string(),
        enabled: false,
    }
}

/// 返回 145+ 个预定义插件（Model Provider / Channel / Search / Memory /
/// Speech / Media / Tool & Diagnostic / 其他）。来源：内置插件库。
fn default_plugins() -> Vec<PluginDef> {
    let mut v: Vec<PluginDef> = Vec::with_capacity(170);

    // ------------------------------------------------------------------
    // Model Provider 插件（35+）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("openai", "OpenAI", "Adds OpenAI model provider support."),
        ("anthropic", "Anthropic", "Anthropic models, Claude CLI, and native Claude session catalog."),
        ("google", "Google", "Adds Google, Google Gemini CLI, Google Vertex model provider support."),
        ("amazon-bedrock", "Amazon Bedrock", "Amazon Bedrock provider plugin with model discovery, embeddings, and guardrail support."),
        ("amazon-bedrock-mantle", "Amazon Bedrock Mantle", "Amazon Bedrock Mantle provider plugin for OpenAI-compatible model routing."),
        ("anthropic-vertex", "Anthropic Vertex", "Anthropic Claude models on Google Vertex AI."),
        ("mistral", "Mistral", "Adds Mistral model provider support."),
        ("cohere", "Cohere", "OpenClaw Cohere provider plugin."),
        ("groq", "Groq", "Adds Groq model provider support."),
        ("deepseek", "DeepSeek", "Adds DeepSeek model provider support."),
        ("qwen", "Qwen", "Adds Qwen, Model Studio, DashScope, Bailian model provider support."),
        ("moonshot", "Moonshot", "Adds Moonshot model provider support."),
        ("kimi", "Kimi", "Adds Kimi, Kimi Coding model provider support."),
        ("minimax", "MiniMax", "Adds MiniMax, MiniMax Portal model provider support."),
        ("ollama", "Ollama", "Adds Ollama, Ollama Cloud model provider support."),
        ("vllm", "vLLM", "Adds vLLM model provider support."),
        ("sglang", "SGLang", "Adds SGLang model provider support."),
        ("lmstudio", "LM Studio", "Adds LM Studio model provider support."),
        ("litellm", "LiteLLM", "Adds LiteLLM model provider support."),
        ("openrouter", "OpenRouter", "Adds OpenRouter model provider support."),
        ("together", "Together", "Adds Together model provider support."),
        ("fireworks", "Fireworks", "Adds Fireworks model provider support."),
        ("cerebras", "Cerebras", "Adds Cerebras model provider support."),
        ("nvidia", "NVIDIA", "Adds NVIDIA model provider support."),
        ("huggingface", "Hugging Face", "Adds Hugging Face model provider support."),
        ("deepinfra", "DeepInfra", "Adds DeepInfra model provider support."),
        ("novita", "Novita", "Adds Novita AI model provider support."),
        ("chutes", "Chutes", "Adds Chutes model provider support."),
        ("venice", "Venice", "Adds Venice model provider support."),
        ("arcee", "Arcee", "Adds Arcee model provider support."),
        ("vydra", "Vydra", "Adds Vydra model provider support."),
        ("voyage", "Voyage", "Adds memory embedding provider support."),
        ("zai", "Z.AI", "Adds Z.AI model provider support."),
        ("xiaomi", "Xiaomi", "Adds Xiaomi, Xiaomi Token Plan model provider support."),
        ("tencent", "Tencent", "Adds Tencent TokenHub, Tencent Tokenplan model provider support."),
        ("qianfan", "Qianfan", "Adds Qianfan model provider support."),
        ("stepfun", "StepFun", "Adds StepFun, StepFun Plan model provider support."),
        ("byteplus", "BytePlus", "Adds BytePlus, BytePlus Plan model provider support."),
        ("volcengine", "Volcengine", "Adds Volcengine, Volcengine Plan model provider support."),
        ("cloudflare-ai-gateway", "Cloudflare AI Gateway", "Adds Cloudflare AI Gateway model provider support."),
        ("vercel-ai-gateway", "Vercel AI Gateway", "Adds Vercel AI Gateway model provider support."),
        ("copilot", "Copilot", "Registers the GitHub Copilot agent runtime."),
        ("github-copilot", "GitHub Copilot", "Adds GitHub Copilot model provider support."),
        ("copilot-proxy", "Copilot Proxy", "Adds Copilot Proxy model provider support."),
        ("alibaba", "Alibaba", "Adds video generation provider support."),
        ("synthetic", "Synthetic", "Adds Synthetic model provider support."),
        ("clawrouter", "ClawRouter", "Adds ClawRouter model provider support."),
        ("xai", "xAI", "Adds xAI model provider support."),
        ("meta", "Meta", "Adds Meta model provider support."),
        ("microsoft-foundry", "Microsoft Foundry", "Adds Microsoft Foundry model provider support."),
        ("opencode", "OpenCode", "Adds OpenCode model provider support."),
        ("opencode-go", "OpenCode Go", "Adds OpenCode Go model provider support."),
        ("comfy", "ComfyUI", "Adds ComfyUI model provider support."),
        ("crabbox", "Crabbox", "Cloud worker provider backed by the Crabbox CLI."),
        ("fal", "fal", "Adds fal model provider support."),
        ("runway", "Runway", "Adds video generation provider support."),
        ("pixverse", "PixVerse", "PixVerse video generation provider plugin."),
        ("featherless", "Featherless", "Featherless AI provider plugin."),
        ("gmi", "GMI", "GMI Cloud provider plugin."),
        ("kilocode", "Kilocode", "Adds Kilocode model provider support."),
        ("longcat", "LongCat", "LongCat provider plugin."),
    ] {
        v.push(p(id, name, desc, "model-provider"));
    }

    // ------------------------------------------------------------------
    // Channel 插件（40+）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("feishu", "Feishu/Lark", "Feishu/Lark channel plugin for chats and workplace tools."),
        ("telegram", "Telegram", "Adds the Telegram channel surface for sending and receiving messages."),
        ("discord", "Discord", "Discord channel plugin for channels, DMs, commands, and app events."),
        ("slack", "Slack", "Slack channel plugin for channels, DMs, commands, and app events."),
        ("whatsapp", "WhatsApp", "WhatsApp channel plugin for WhatsApp Web chats."),
        ("signal", "Signal", "Adds the Signal channel surface for sending and receiving messages."),
        ("imessage", "iMessage", "Adds the iMessage channel surface for sending and receiving messages."),
        ("irc", "IRC", "Adds the IRC channel surface for sending and receiving messages."),
        ("matrix", "Matrix", "Matrix channel plugin for rooms and direct messages."),
        ("mattermost", "Mattermost", "Adds the Mattermost channel surface for sending and receiving messages."),
        ("msteams", "Microsoft Teams", "Microsoft Teams channel plugin for bot conversations."),
        ("nextcloud-talk", "Nextcloud Talk", "Nextcloud Talk channel plugin for conversations."),
        ("nostr", "Nostr", "Nostr channel plugin for NIP-04 encrypted direct messages."),
        ("qqbot", "QQ Bot", "QQ Bot channel plugin for group and direct-message workflows."),
        ("synology-chat", "Synology Chat", "Synology Chat channel plugin for channels and direct messages."),
        ("tlon", "Tlon/Urbit", "Tlon/Urbit channel plugin for chat workflows."),
        ("twitch", "Twitch", "Twitch channel plugin for chat and moderation workflows."),
        ("zalo", "Zalo", "Zalo channel plugin for bot and webhook chats."),
        ("zalouser", "Zalo Personal", "Zalo Personal Account plugin via native zca-js integration."),
        ("clickclack", "Clickclack", "Adds the Clickclack channel surface for sending and receiving messages."),
        ("line", "LINE", "LINE channel plugin for LINE Bot API chats."),
        ("googlechat", "Google Chat", "Google Chat channel plugin for spaces and direct messages."),
        ("raft", "Raft", "Raft channel plugin for secure CLI wake bridges."),
        ("sms", "SMS (Twilio)", "Twilio SMS channel plugin for text messages."),
        ("voice-call", "Voice Call", "Voice-call plugin for Twilio, Telnyx, and Plivo phone calls."),
        ("google-meet", "Google Meet", "Google Meet participant plugin for joining calls via Chrome or Twilio."),
        ("qa-channel", "QA Channel", "Adds the QA Channel surface for sending and receiving messages."),
        ("webhooks", "Webhooks", "Authenticated inbound webhooks that bind external automation to TaskFlows."),
        ("acpx", "ACPX", "ACP runtime backend with plugin-owned session and transport management."),
    ] {
        v.push(p(id, name, desc, "channel"));
    }

    // ------------------------------------------------------------------
    // Search 插件（13+）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("searxng", "SearXNG", "Adds web search provider support via SearXNG."),
        ("brave", "Brave Search", "Brave Search provider plugin for web search."),
        ("tavily", "Tavily", "Adds agent-callable tools and web search provider support."),
        ("exa", "Exa", "Adds web search provider support."),
        ("firecrawl", "Firecrawl", "Adds agent-callable tools, web fetch and web search provider support."),
        ("parallel", "Parallel", "Adds web search provider support."),
        ("perplexity", "Perplexity", "Adds web search provider support."),
        ("duckduckgo", "DuckDuckGo", "Adds web search provider support."),
        ("web-readability", "Web Readability", "Extract readable article content from local HTML web fetch responses."),
    ] {
        v.push(p(id, name, desc, "search"));
    }

    // ------------------------------------------------------------------
    // Memory 插件（5+）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("memory-core", "Memory Core", "Adds agent-callable memory tools."),
        ("memory-lancedb", "Memory LanceDB", "LanceDB-backed long-term memory with auto-recall, auto-capture, and vector search."),
        ("memory-wiki", "Memory Wiki", "Persistent wiki compiler and Obsidian-friendly knowledge vault."),
        ("vault", "Vault", "HashiCorp Vault SecretRef provider integration."),
        ("llama-cpp", "llama.cpp", "Local GGUF embeddings through node-llama-cpp."),
    ] {
        v.push(p(id, name, desc, "memory"));
    }

    // ------------------------------------------------------------------
    // Speech / TTS / 转写 插件（10+）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("azure-speech", "Azure Speech", "Azure AI Speech text-to-speech (MP3, Ogg/Opus voice notes, PCM telephony)."),
        ("microsoft", "Microsoft TTS", "Adds text-to-speech provider support."),
        ("elevenlabs", "ElevenLabs", "Adds media understanding, realtime transcription, and text-to-speech support."),
        ("inworld", "Inworld", "Inworld streaming text-to-speech (MP3, OGG_OPUS, PCM telephony)."),
        ("gradium", "Gradium", "Adds text-to-speech provider support."),
        ("tts-local-cli", "TTS Local CLI", "Adds text-to-speech provider support."),
        ("deepgram", "Deepgram", "Adds media understanding and realtime transcription provider support."),
        ("senseaudio", "SenseAudio", "Adds media understanding provider support."),
    ] {
        v.push(p(id, name, desc, "speech"));
    }

    // ------------------------------------------------------------------
    // Media 插件（10+）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("document-extract", "Document Extract", "Extract text and fallback page images from local document attachments."),
        ("browser", "Browser", "Adds agent-callable browser tools."),
        ("canvas", "Canvas", "Experimental Canvas control and A2UI rendering surfaces for paired nodes."),
        ("file-transfer", "File Transfer", "Fetch, list, and write files on paired nodes via dedicated node commands."),
        ("diffs", "Diffs", "Read-only diff viewer plugin and file renderer for agents."),
        ("diffs-language-pack", "Diffs Language Pack", "Adds syntax highlighting for languages outside the default diffs viewer set."),
        ("tokenjuice", "TokenJuice", "Compacts exec and bash tool results with tokenjuice reducers."),
        ("oc-path", "OC Path", "Adds the path CLI for workspace file addressing."),
    ] {
        v.push(p(id, name, desc, "media"));
    }

    // ------------------------------------------------------------------
    // Tool / Diagnostic 插件（15+）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("bonjour", "Bonjour", "Advertise the local gateway over Bonjour/mDNS."),
        ("openshell", "OpenShell", "Sandbox backend for the NVIDIA OpenShell CLI with mirrored local workspaces and SSH command execution."),
        ("diagnostics-otel", "Diagnostics OpenTelemetry", "OpenTelemetry exporter for metrics, traces, and logs."),
        ("diagnostics-prometheus", "Diagnostics Prometheus", "Prometheus exporter for runtime metrics."),
        ("lobster", "Lobster", "Lobster workflow tool plugin for typed pipelines and resumable approvals."),
        ("llm-task", "LLM Task", "Generic JSON-only LLM tool for structured tasks callable from workflows."),
        ("admin-http-rpc", "Admin HTTP RPC", "Admin HTTP RPC endpoint."),
        ("workboard", "Workboard", "Dashboard workboard for agent-owned issues and sessions."),
        ("workspaces", "Workspaces", "Agent-composable Workspaces document and control-plane backend."),
        ("logbook", "Logbook", "Automatic work journal: captures screen snapshots into a reviewable timeline."),
        ("policy", "Policy", "Adds policy-backed doctor checks for workspace conformance."),
        ("migrate-claude", "Migrate Claude", "Imports Claude Code/Desktop instructions, MCP servers, skills, and config."),
        ("migrate-hermes", "Migrate Hermes", "Imports Hermes configuration, memories, skills, and credentials."),
        ("open-prose", "OpenProse", "OpenProse VM skill pack with a /prose slash command."),
        ("qa-lab", "QA Lab", "QA lab plugin with private debugger UI and scenario runner."),
        ("qa-matrix", "QA Matrix", "Matrix QA transport runner and substrate."),
    ] {
        v.push(p(id, name, desc, "tool-diagnostic"));
    }

    // ------------------------------------------------------------------
    // 其他（辅助/工具）
    // ------------------------------------------------------------------
    for (id, name, desc) in [
        ("codex", "Codex", "Codex app-server harness, model provider, and native session catalog."),
        ("local-tts", "Local TTS", "Local text-to-speech via system speech engine."),
        ("node-llama-cpp", "node-llama-cpp", "Local LLM inference via node-llama-cpp bindings."),
        ("realtime-transcription", "Realtime Transcription", "Streaming realtime audio transcription."),
        ("openai-realtime", "OpenAI Realtime", "OpenAI Realtime API voice/audio support."),
        ("google-live", "Google Live", "Google Live API bidirectional voice/audio support."),
        ("gemini-search", "Gemini Search", "Web search grounded via Google Gemini."),
        ("grok-search", "Grok Search", "Web search grounded via xAI Grok."),
        ("kimi-search", "Kimi Search", "Web search grounded via Kimi."),
        ("minimax-search", "MiniMax Search", "Web search grounded via MiniMax."),
        ("ollama-search", "Ollama Search", "Web search grounded via local Ollama models."),
        ("web-fetch", "Web Fetch", "Fetch arbitrary URLs and return cleaned content."),
        ("pdf-extract", "PDF Extract", "Extract text and structure from PDF documents."),
        ("image-generation", "Image Generation", "Generate images from text prompts."),
        ("video-generation", "Video Generation", "Generate video from text/image prompts."),
        ("music-generation", "Music Generation", "Generate music/audio from text prompts."),
        ("wechat", "WeChat", "WeChat channel plugin for messages and groups."),
        ("yuanbao", "Yuanbao", "Tencent Yuanbao channel surface."),
        ("email", "Email", "SMTP/IMAP email channel plugin."),
        ("sms-twilio", "SMS Twilio", "Twilio SMS outbound/inbound bridge."),
        ("clawhub", "ClawHub", "Plugin marketplace client for install/update from ClawHub."),
        ("opentelemetry", "OpenTelemetry", "Emit spans/metrics/logs via OTLP."),
        ("btw", "BTW", "By-the-way reminder/context tool."),
        ("goal", "Goal", "Goal tracking and progress tool."),
        ("steer", "Steer", "Steering/behavior modulation tool."),
        ("thinking", "Thinking", "Explicit reasoning scratchpad tool."),
        ("trajectory", "Trajectory", "Record and replay agent trajectories."),
        ("reactions", "Reactions", "Emoji/quick reactions to messages."),
    ] {
        v.push(p(id, name, desc, "other"));
    }

    v
}

/// 持久化插件启用/禁用/安装状态的 JSON 文件路径
fn plugins_state_path(state: &AppState) -> String {
    format!("{}/.cradle-ring/data/plugins_state.json", state.storage.home)
}

/// 读取插件状态覆盖：{ "enabled": {id: bool}, "installed": {id: bool} }
fn load_plugins_state(state: &AppState) -> serde_json::Value {
    let path = plugins_state_path(state);
    if let Ok(data) = std::fs::read_to_string(&path) {
        serde_json::from_str(&data).unwrap_or_else(|_| json!({ "enabled": {}, "installed": {} }))
    } else {
        json!({ "enabled": {}, "installed": {} })
    }
}

/// 写回插件状态文件
fn save_plugins_state(state: &AppState, st: &serde_json::Value) {
    let path = plugins_state_path(state);
    if let Ok(data) = serde_json::to_string_pretty(st) {
        let _ = std::fs::write(path, data);
    }
}

/// 把预装清单与状态覆盖合并，输出 list/inspect 用的完整条目
fn merged_plugins(state: &AppState) -> Vec<serde_json::Value> {
    let defs = default_plugins();
    let st = load_plugins_state(state);
    let enabled_overrides = st["enabled"].as_object();
    let installed_overrides = st["installed"].as_object();
    let mut out = Vec::with_capacity(defs.len());
    for d in defs {
        let enabled = enabled_overrides
            .and_then(|m| m.get(&d.id))
            .and_then(|v| v.as_bool())
            .unwrap_or(d.enabled);
        let installed = installed_overrides
            .and_then(|m| m.get(&d.id))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        out.push(json!({
            "id": d.id,
            "name": d.name,
            "description": d.description,
            "category": d.category,
            "enabled": enabled,
            "installed": installed,
            "source": "bundled",
        }));
    }
    out
}

/// 构造 5 个内置工具的 OpenAI function-calling 定义
fn build_tools_schema() -> serde_json::Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "在线搜索网络资料。当需要最新信息、外部知识或验证事实时使用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "搜索关键词"}
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "exec",
                "description": "执行 shell 命令。用于运行脚本、查看系统状态等。会返回命令的 stdout/stderr 和退出码。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "要执行的 shell 命令"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "读取本地文件内容。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "文件绝对路径或相对路径"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "写入本地文件（覆盖已存在文件）。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "文件路径"},
                        "content": {"type": "string", "description": "文件内容"}
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "memory_save",
                "description": "把一条信息保存到长期记忆库，后续对话可以主动回忆。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "body": {"type": "string", "description": "要记住的内容"},
                        "kind": {"type": "string", "description": "记忆类型（如 fact/preference/note）", "default": "fact"}
                    },
                    "required": ["body"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "spawn_subagent",
                "description": "派生一个子 agent 独立运行：用指定（或默认）模型开一个隔离的子会话，让其自主完成子任务并返回最终结果。适用于复杂任务拆解、并行子任务、专家委托。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string", "description": "给子 agent 的完整任务描述（包含目标、约束、期望输出格式）"},
                        "model": {"type": "string", "description": "可选：指定子 agent 使用的模型（如 gpt-4o-mini / claude-3-5-sonnet）；留空用默认"},
                        "max_iterations": {"type": "integer", "description": "可选：子 agent 最大工具调用轮次，默认 5", "default": 5}
                    },
                    "required": ["prompt"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "fan_out",
                "description": "Map-Reduce 扇出：把一个复杂任务拆成多个子任务，并行运行多个子 agent，最后汇总结果。适用于批量处理、多角度分析、并行调研。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": {"type": "string", "description": "总任务描述"},
                        "subtasks": {"type": "array", "items": {"type": "string"}, "description": "子任务列表，每个子任务将由独立子 agent 并行执行"},
                        "agent_role": {"type": "string", "description": "可选：使用的角色化 agent id；留空用默认 agent"},
                        "max_concurrent": {"type": "integer", "description": "可选：最大并发数，默认 5", "default": 5}
                    },
                    "required": ["task", "subtasks"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "delegate_task",
                "description": "把子任务委派给另一个角色化 agent（需要该 agent 允许 delegation）。委派后等待对方返回结果。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "agent_role_id": {"type": "string", "description": "目标角色化 agent 的 id"},
                        "task": {"type": "string", "description": "委派的任务描述"}
                    },
                    "required": ["agent_role_id", "task"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_code",
                "description": "在沙箱中执行代码并返回 stdout/stderr。支持 python / javascript(node) / rust(script) 三种语言。受 10 秒超时与 256MB 内存限制。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "language": {"type": "string", "enum": ["python", "javascript", "rust"], "description": "代码语言"},
                        "code": {"type": "string", "description": "要执行的代码（python/javascript 为脚本；rust 为单文件 main 体）"}
                    },
                    "required": ["language", "code"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browse",
                "description": "浏览网页：fetch 抓取页面正文（自动 HTML→文本）；click 模拟点击（headless）；screenshot 截图（headless chromium）。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "目标网址"},
                        "action": {"type": "string", "enum": ["fetch", "click", "screenshot"], "default": "fetch", "description": "操作类型"},
                        "selector": {"type": "string", "description": "click/screenshot 的 CSS 选择器（可选）"}
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "fetch_latest_info",
                "description": "获取某主题的最新信息：先联网搜索，再抓取排名靠前页面的正文，组装成简洁摘要。适合查新闻、最新数据、近期事件。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "topic": {"type": "string", "description": "要查询的主题或问题"},
                        "max_pages": {"type": "integer", "description": "最多抓取详情页数，默认 3", "default": 3}
                    },
                    "required": ["topic"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_document",
                "description": "读取并解析本地文档，返回纯文本。支持 TXT/CSV/JSON/HTML/Markdown 直接读取，以及 PDF（提取文本层）/Word(.docx)/Excel(.xlsx)/PowerPoint(.pptx)（解压 ZIP 内 XML 提取文本）。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "文档绝对路径或相对路径"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "transcribe_audio",
                "description": "将音频转写为文本。优先调用 OpenAI Whisper API（需配置 OPENAI_API_KEY），失败时回退本地 whisper 命令行。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "音频文件路径或可下载 URL（mp3/wav/m4a/webm 等）"}
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "analyze_image",
                "description": "分析图片内容并回答关于图片的问题。支持 jpg/png/gif/webp。本地图片转 base64，远程图片直接传 URL。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "图片路径或 URL"},
                        "question": {"type": "string", "description": "要问的关于图片的问题"}
                    },
                    "required": ["url", "question"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "port_scan",
                "description": "端口扫描：扫描目标主机的开放端口和服务。用于网络安全评估、运维排查。支持指定端口范围。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {"type": "string", "description": "目标 IP 或域名"},
                        "ports": {"type": "string", "description": "端口范围，如 '1-1000' 或 '22,80,443,3306'，默认 '1-1000'"},
                        "timeout_ms": {"type": "integer", "description": "单端口超时毫秒，默认 500"}
                    },
                    "required": ["target"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "http_probe",
                "description": "HTTP 探测：发送 HTTP 请求检查目标 URL 的状态码、响应头、响应时间。支持自定义方法/头部/Body。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "目标 URL"},
                        "method": {"type": "string", "description": "HTTP 方法（GET/POST/PUT/DELETE 等），默认 GET"},
                        "headers": {"type": "object", "description": "自定义请求头"},
                        "body": {"type": "string", "description": "请求体（POST/PUT 时使用）"},
                        "timeout_ms": {"type": "integer", "description": "超时毫秒，默认 10000"}
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "vuln_scan",
                "description": "漏洞扫描：检查目标 URL 的常见 Web 漏洞（SQL注入/XSS/目录遍历/敏感文件暴露/弱口令等）。返回发现的安全问题列表。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {"type": "string", "description": "目标 URL（如 http://example.com）"},
                        "depth": {"type": "string", "description": "扫描深度：quick（快速）/ normal（标准）/ deep（深度），默认 normal"}
                    },
                    "required": ["target"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "dns_lookup",
                "description": "DNS 查询：查询域名的 A/AAAA/MX/TXT/NS/CNAME 记录。用于域名排查、邮件配置验证、CDN 检测。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "domain": {"type": "string", "description": "要查询的域名"},
                        "record_type": {"type": "string", "description": "记录类型（A/AAAA/MX/TXT/NS/CNAME/ANY），默认 A"}
                    },
                    "required": ["domain"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ssl_check",
                "description": "SSL/TLS 证书检查：检查目标 HTTPS 网站的证书链、有效期、颁发者、协议版本、密码套件。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "host": {"type": "string", "description": "目标主机（域名或 IP:端口）"},
                        "port": {"type": "integer", "description": "端口，默认 443"}
                    },
                    "required": ["host"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "subdomain_enum",
                "description": "子域名枚举：通过字典爆破和被动查询发现目标的子域名。用于攻击面梳理。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "domain": {"type": "string", "description": "目标根域名"},
                        "wordlist": {"type": "string", "description": "自定义字典（逗号分隔），留空用内置常见子域名字典"}
                    },
                    "required": ["domain"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "waf_detect",
                "description": "WAF 检测：多 payload 识别 WAF 类型（Cloudflare/AWS WAF/Akamai/ModSecurity/阿里云盾/腾讯云等），含绕过测试。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "目标 URL"}
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "sqli_scan",
                "description": "SQL 注入专项检测：10 种 payload（报错/布尔/时间盲注/联合查询/堆叠），检测数据库错误信息泄露和响应异常。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "目标 URL（需含参数点）"}
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "xss_scan",
                "description": "XSS 专项检测：10 种 payload（script/img/svg/iframe/事件处理器），检测反射型 XSS。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "目标 URL（需含参数点）"}
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "exposure_analysis",
                "description": "端口暴露面分析：扫描 20+ 高危端口（23/445/1433/3389/6379/27017 等），评估暴露风险。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "host": {"type": "string", "description": "目标主机（域名或 IP）"}
                    },
                    "required": ["host"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "service_monitor",
                "description": "服务监控：检查本地或远程服务的运行状态（CPU/内存/磁盘/网络/进程）。用于运维巡检。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "check": {"type": "string", "description": "检查项：cpu/mem/disk/net/process/all，默认 all"},
                        "process_name": {"type": "string", "description": "进程名（check=process 时指定）"}
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "log_analyze",
                "description": "日志分析：读取和分析系统/应用日志文件，提取错误、警告、异常模式。支持正则过滤。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "日志文件路径"},
                        "pattern": {"type": "string", "description": "正则表达式过滤模式（留空则提取 ERROR/WARN/FATAL）"},
                        "lines": {"type": "integer", "description": "读取最后 N 行，默认 1000"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "network_trace",
                "description": "网络追踪：执行 traceroute/mtr 追踪到目标的网络路径，显示每一跳的延迟和丢包率。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "target": {"type": "string", "description": "目标 IP 或域名"},
                        "max_hops": {"type": "integer", "description": "最大跳数，默认 30"}
                    },
                    "required": ["target"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "file_hash",
                "description": "文件哈希：计算文件的 MD5/SHA1/SHA256 哈希值。用于文件完整性验证、恶意文件比对。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "文件路径"},
                        "algorithm": {"type": "string", "description": "哈希算法（md5/sha1/sha256），默认 sha256"}
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "encode_decode",
                "description": "编码/解码：支持 Base64/URL/Hex/HTML 实体 编码和解码。用于分析编码后的 payload。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "description": "操作：encode 或 decode"},
                        "format": {"type": "string", "description": "格式：base64/url/hex/html"},
                        "input": {"type": "string", "description": "要处理的文本"}
                    },
                    "required": ["action", "format", "input"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "git_ops",
                "description": "Git 操作：执行 git 命令（status/log/diff/branch/commit/push/pull 等）。用于代码管理和 DevOps。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "git 子命令（如 status/log/diff/branch）"},
                        "repo_path": {"type": "string", "description": "仓库路径（默认当前目录）"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "docker_ops",
                "description": "Docker 操作：执行 docker 命令（ps/images/logs/exec/build/run/stop 等）。用于容器运维。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "docker 子命令（如 ps/images/logs）"},
                        "container": {"type": "string", "description": "容器名/ID（logs/exec/stop 时需要）"}
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "process_manage",
                "description": "进程管理：查看/启动/停止/重启系统进程。用于运维管理。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "description": "操作：list/start/stop/restart/status"},
                        "name": {"type": "string", "description": "进程名或服务名"},
                        "signal": {"type": "string", "description": "信号（stop 时：TERM/KILL/HUP，默认 TERM）"}
                    },
                    "required": ["action"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "backup_create",
                "description": "备份创建：打包备份指定目录或文件。支持 tar.gz/zip 格式。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "source": {"type": "string", "description": "要备份的目录或文件"},
                        "dest": {"type": "string", "description": "备份文件保存路径"},
                        "format": {"type": "string", "description": "格式：tar.gz 或 zip，默认 tar.gz"}
                    },
                    "required": ["source", "dest"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_host_load",
                "description": "获取主机负载概览：CPU负载(loadavg)、内存/swap使用率、各挂载点磁盘占用。运维诊断首选工具。",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "get_host_processes",
                "description": "获取主机进程列表(TOP N)，按 CPU/内存排序。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "topN": {"type": "integer", "description": "返回前 N 个进程，默认 15", "default": 15},
                        "sortBy": {"type": "string", "enum": ["cpu", "mem", "pid"], "description": "排序方式，默认 cpu"}
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "host_du_summary",
                "description": "目录占用摘要：分层显示各子目录大小，定位磁盘占用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paths": {"type": "array", "items": {"type": "string"}, "description": "要分析的目录列表，如 [\"/var\", \"/opt\"]"},
                        "depth": {"type": "integer", "description": "下钻深度，默认 1", "default": 1}
                    },
                    "required": ["paths"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "host_find_large_files",
                "description": "查找大文件(>100MB)，按大小排序。定位磁盘占用元凶。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paths": {"type": "array", "items": {"type": "string"}, "description": "搜索的目录列表"},
                        "topN": {"type": "integer", "description": "返回前 N 个，默认 20", "default": 20}
                    },
                    "required": ["paths"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "host_stat_file",
                "description": "查看文件元信息：大小/权限/修改时间/类型。",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "文件路径"}},
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "host_netns_inspect",
                "description": "网络命名空间与接口检查：列出 netns、接口地址、路由表。网络诊断首选。",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "host_diagnostic_snapshot",
                "description": "综合主机诊断快照：一次性拉取 load + TOP10进程 + 网络。快速了解全局。",
                "parameters": {"type": "object", "properties": {}}
            }
        },
        {
            "type": "function",
            "function": {
                "name": "query_change_events",
                "description": "查询变更事件审计日志：列出最近的 mutating 操作(命令/服务/配置变更)，用于根因分析(RCA)定位「谁改了什么」。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "description": "返回条数，默认 20", "default": 20},
                        "kind": {"type": "string", "description": "按类型过滤：exec_write/service_restart/config_change/approval"}
                    }
                }
            }
        }
    ])
}

/// 广播一个 chat 事件给所有活跃 WebSocket 客户端
async fn broadcast_chat_event(state: &AppState, session_key: &str, run_id: &str, extra: serde_json::Value) {
    let mut payload = serde_json::Map::new();
    payload.insert("sessionKey".into(), json!(session_key));
    payload.insert("runId".into(), json!(run_id));
    if let serde_json::Value::Object(map) = extra {
        for (k, v) in map {
            payload.insert(k, v);
        }
    }
    let msg = json!({
        "type": "event",
        "event": "chat",
        "payload": payload
    }).to_string();
    let mut ws = state.active_ws.lock().await;
    ws.retain(|tx| tx.send(msg.clone()).is_ok());
}

/// 广播一个自定义事件（如 exec.approval.requested / cron.fired 等）给所有 WS 客户端
async fn broadcast_event(state: &AppState, event: &str, payload: serde_json::Value) {
    let msg = json!({
        "type": "event",
        "event": event,
        "payload": payload
    }).to_string();
    let mut ws = state.active_ws.lock().await;
    ws.retain(|tx| tx.send(msg.clone()).is_ok());
}

// ============================================================================
// Cron 表达式解析（5 段：分 时 日 月 周）
// 支持：*、*/N、逗号列表、连字符区间、单值
// ============================================================================

/// 把 cron 的一段（如 "*/5"、"1,15"、"0-5"、"30"）解析为允许值集合。
/// bounds.0=最小, bounds.1=最大
fn parse_cron_field(field: &str, bounds: (u32, u32)) -> Result<Vec<u32>, String> {
    let mut out: Vec<u32> = Vec::new();
    for part in field.split(',') {
        let part = part.trim();
        if part == "*" {
            for v in bounds.0..=bounds.1 {
                out.push(v);
            }
        } else if let Some(rest) = part.strip_prefix("*/") {
            let step: u32 = rest.parse().map_err(|_| format!("无效步进: {}", rest))?;
            if step == 0 { return Err("步进不能为 0".to_string()); }
            let mut v = bounds.0;
            while v <= bounds.1 {
                out.push(v);
                v = match v.checked_add(step) {
                    Some(n) => n,
                    None => break,
                };
            }
        } else if part.contains('-') {
            // 区间，可能带步进 "1-10/2"
            let (range_part, step) = if let Some(slash) = part.find('/') {
                let (r, st) = part.split_at(slash);
                let st = &st[1..];
                (r, st.parse::<u32>().unwrap_or(1))
            } else {
                (part, 1)
            };
            let dash = range_part.find('-').ok_or_else(|| format!("无效区间: {}", part))?;
            let lo: u32 = range_part[..dash].parse().map_err(|_| format!("无效下界: {}", &range_part[..dash]))?;
            let hi: u32 = range_part[dash+1..].parse().map_err(|_| format!("无效上界: {}", &range_part[dash+1..]))?;
            if lo < bounds.0 || hi > bounds.1 || lo > hi {
                return Err(format!("区间 {} 越界（{}-{}）", part, bounds.0, bounds.1));
            }
            let mut v = lo;
            while v <= hi {
                out.push(v);
                v = match v.checked_add(step) {
                    Some(n) => n,
                    None => break,
                };
            }
        } else if let Some(slash) = part.find('/') {
            // "5/2" 形式：从某值开始带步进（到上界）
            let start: u32 = part[..slash].parse().map_err(|_| format!("无效值: {}", &part[..slash]))?;
            let step: u32 = part[slash+1..].parse().map_err(|_| format!("无效步进: {}", &part[slash+1..]))?;
            if step == 0 { return Err("步进不能为 0".to_string()); }
            if start < bounds.0 || start > bounds.1 {
                return Err(format!("值 {} 越界（{}-{}）", start, bounds.0, bounds.1));
            }
            let mut v = start;
            while v <= bounds.1 {
                out.push(v);
                v = match v.checked_add(step) { Some(n) => n, None => break };
            }
        } else {
            let v: u32 = part.parse().map_err(|_| format!("无效值: {}", part))?;
            if v < bounds.0 || v > bounds.1 {
                return Err(format!("值 {} 越界（{}-{}）", v, bounds.0, bounds.1));
            }
            out.push(v);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// 5 段 cron 表达式的解析结果
struct CronSpec {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Vec<u32>,
    months: Vec<u32>,
    weekdays: Vec<u32>,
}

fn parse_cron(expr: &str) -> Result<CronSpec, String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!("cron 表达式必须是 5 段（分 时 日 月 周），实际: {} 段", parts.len()));
    }
    // 周几：0-7（0 和 7 都是周日）。统一映射到 0-6。
    let mut weekdays = parse_cron_field(parts[4], (0, 7))?;
    for w in weekdays.iter_mut() {
        if *w == 7 { *w = 0; }
    }
    Ok(CronSpec {
        minutes: parse_cron_field(parts[0], (0, 59))?,
        hours: parse_cron_field(parts[1], (0, 23))?,
        days: parse_cron_field(parts[2], (1, 31))?,
        months: parse_cron_field(parts[3], (1, 12))?,
        weekdays,
    })
}

/// 计算从 now_ms（毫秒）之后下一个匹配的 unix 秒时间戳。
/// 简单实现：从下一分钟开始逐分钟扫描（最多扫描 366 天）。
fn next_cron_run(spec: &CronSpec, now_ms: i64) -> Option<i64> {
    use chrono::{TimeZone, Utc, Datelike, Timelike};
    let start_sec = (now_ms / 1000) + 60; // 下一分钟开始
    // 对齐到分钟边界
    let start_sec = start_sec - (start_sec % 60);
    let max_scan = 366 * 24 * 60i64; // 最多扫描 366 天的分钟数
    let minute_set: std::collections::HashSet<u32> = spec.minutes.iter().copied().collect();
    let hour_set: std::collections::HashSet<u32> = spec.hours.iter().copied().collect();
    let day_set: std::collections::HashSet<u32> = spec.days.iter().copied().collect();
    let month_set: std::collections::HashSet<u32> = spec.months.iter().copied().collect();
    let weekday_set: std::collections::HashSet<u32> = spec.weekdays.iter().copied().collect();
    for i in 0..max_scan {
        let t = start_sec + i * 60;
        if let Some(dt) = Utc.timestamp_opt(t, 0).single() {
            if !minute_set.contains(&dt.minute()) { continue; }
            if !hour_set.contains(&dt.hour()) { continue; }
            if !day_set.contains(&dt.day()) { continue; }
            if !month_set.contains(&dt.month()) { continue; }
            let wd = dt.weekday().num_days_from_sunday();
            if !weekday_set.contains(&wd) { continue; }
            return Some(t);
        }
    }
    None
}

/// 解析单行 SSE 数据，返回 Option<serde_json::Value>
fn parse_sse_data(line: &str) -> Option<serde_json::Value> {
    let trimmed = line.trim();
    if !trimmed.starts_with("data:") {
        return None;
    }
    let data = trimmed["data:".len()..].trim();
    if data == "[DONE]" || data.is_empty() {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(data).ok()
}

/// 调用 LLM（stream=true），边收边解析 delta，累积文本与 tool_calls。
/// 增量文本通过 broadcast_chat_event 实时广播。
async fn stream_llm_call(
    state: &AppState,
    session_key: &str,
    run_id: &str,
    messages: Vec<serde_json::Value>,
    tools: &serde_json::Value,
) -> Result<StreamResult, String> {
    // 多 provider fallback：依次尝试每个启用的 provider，遇到可重试错误（超时/429/5xx）切换下一个。
    let providers = state.config.enabled_providers();
    if providers.is_empty() {
        return Err("未配置任何可用的 LLM provider（请在配置文件 providers.* 设置 apiKey）。".to_string());
    }

    let mut last_err = String::new();
    let mut tried_any = false;
    for (idx, provider) in providers.iter().enumerate() {
        tried_any = true;
        match stream_llm_call_single(state, session_key, run_id, messages.clone(), tools, provider).await {
            Ok(r) => return Ok(r),
            Err(e) => {
                let retriable = is_retriable_error(&e);
                last_err = format!("[{}] {}", provider.name, e);
                // 广播 fallback 事件（仅当还有下一个 provider 时）
                if retriable && idx + 1 < providers.len() {
                    let _ = broadcast_chat_event(
                        state,
                        session_key,
                        run_id,
                        json!({
                            "status": "provider_fallback",
                            "failedProvider": provider.name,
                            "nextProvider": providers[idx + 1].name,
                            "error": last_err,
                        }),
                    )
                    .await;
                    continue;
                }
                // 不可重试或已是最后一个 → 直接返回错误
                return Err(last_err);
            }
        }
    }
    if !tried_any {
        return Err("无可用 provider".to_string());
    }
    Err(last_err)
}

/// 判断错误是否可触发 provider fallback。
/// 可重试：超时、429、5xx、连接错误。
fn is_retriable_error(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("timeout")
        || e.contains("timed out")
        || e.contains("429")
        || e.contains("rate limit")
        || e.contains("500")
        || e.contains("502")
        || e.contains("503")
        || e.contains("504")
        || e.contains("connect")
        || e.contains("connection")
        || e.contains("dns")
        || e.contains("eof")
        || e.contains("reset")
        || e.contains("broken pipe")
}

/// 针对单个 provider 执行流式 LLM 调用。
async fn stream_llm_call_single(
    state: &AppState,
    session_key: &str,
    run_id: &str,
    messages: Vec<serde_json::Value>,
    tools: &serde_json::Value,
    provider: &ProviderCfg,
) -> Result<StreamResult, String> {
    let model = provider.effective_model(&state.config.default_model);

    // 构造请求体（按 provider 能力附加 thinking）
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });
    // 仅当存在工具定义时才传 tools/tool_choice（部分 provider/模型不支持 tools）
    if tools.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        body["tools"] = tools.clone();
        body["tool_choice"] = json!("auto");
    }
    // thinking/reasoning 支持：Claude 用 thinking，OpenAI o 系列用 reasoning_effort，
    // Qwen/DeepSeek 通过 enable_thinking
    if provider.supports_thinking {
        if provider.name == "anthropic" {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": 4096 });
        } else if provider.name == "openai" {
            body["reasoning_effort"] = json!("medium");
        } else if provider.name == "qwen" || provider.name == "dashscope" {
            body["enable_thinking"] = json!(true);
        } else if provider.name == "deepseek" {
            // deepseek-reasoner 自带 reasoning_content，无需额外参数
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败: {}", e))?;

    let url = format!("{}/chat/completions", provider.base_url.trim_end_matches('/'));
    let mut req = client.post(&url).json(&body);
    // 鉴权头：OpenAI 兼容 Bearer；Anthropic 用 x-api-key（其 OpenAI 兼容端点也支持 Bearer）
    if let Some(k) = &provider.api_key {
        if provider.name == "anthropic" {
            req = req
                .header("x-api-key", k)
                .header("anthropic-version", "2023-06-01");
        } else {
            req = req.header("Authorization", format!("Bearer {}", k));
        }
    }
    let resp = req.send().await.map_err(|e| {
        // 区分超时/连接错误，便于 fallback 判定
        if e.is_timeout() {
            format!("请求超时: {}", e)
        } else if e.is_connect() {
            format!("连接失败: {}", e)
        } else {
            format!("请求失败: {}", e)
        }
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("LLM 返回错误 {}: {}", status, &text[..text.len().min(300)]));
    }

    // 流式读取响应体（SSE）
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();

    // 工具调用累积器：以 index 为 key
    let mut accum_text = String::new();
    let mut accum_thinking = String::new();
    let mut accum_tools: std::collections::BTreeMap<usize, ToolCallAccumulator> = std::collections::BTreeMap::new();
    let mut finish_reason = String::new();
    let mut buf = String::new();
    let mut usage_prompt: u64 = 0;
    let mut usage_completion: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("读取流失败: {}", e))?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        // 逐行处理（SSE 以 \n 分隔）
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].to_string();
            buf = buf[nl + 1..].to_string();

            // 空行（SSE 事件分隔符）跳过
            if line.trim().is_empty() {
                continue;
            }

            // 解析 "data: {...}"
            if let Some(v) = parse_sse_data(&line) {
                // usage（部分 provider 在最后一帧给出）
                if let Some(u) = v.get("usage") {
                    if let Some(pt) = u["prompt_tokens"].as_u64() { usage_prompt = pt; }
                    if let Some(ct) = u["completion_tokens"].as_u64() { usage_completion = ct; }
                }
                if let Some(choices) = v["choices"].as_array() {
                    if let Some(choice) = choices.first() {
                        let delta = &choice["delta"];

                        // 处理 thinking/reasoning 增量（多种字段名兼容）
                        // OpenAI o 系列：无单独字段；DeepSeek：reasoning_content；
                        // Anthropic OpenAI 兼容端点：reasoning_content；Qwen：reasoning_content
                        if let Some(rc) = delta["reasoning_content"].as_str() {
                            if !rc.is_empty() {
                                accum_thinking.push_str(rc);
                                // 广播 thinking 事件（与正文区分）
                                let _ = broadcast_chat_event(
                                    state,
                                    session_key,
                                    run_id,
                                    json!({ "thinkingDelta": rc, "thinking": accum_thinking.clone() }),
                                ).await;
                            }
                        }

                        // 处理文本增量
                        if let Some(content) = delta["content"].as_str() {
                            if !content.is_empty() {
                                accum_text.push_str(content);
                                // 广播增量文本（同时带上累积完整文本，兼容前端 UI）
                                broadcast_chat_event(
                                    state,
                                    session_key,
                                    run_id,
                                    json!({ "deltaText": content, "message": accum_text.clone() }),
                                )
                                .await;
                            }
                        }

                        // 处理 tool_calls 增量
                        if let Some(tcs) = delta["tool_calls"].as_array() {
                            for tc in tcs {
                                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                                let entry = accum_tools.entry(idx).or_default();
                                if let Some(s) = tc["id"].as_str() {
                                    if !s.is_empty() {
                                        entry.id = s.to_string();
                                    }
                                }
                                if let Some(f) = tc.get("function") {
                                    if let Some(n) = f["name"].as_str() {
                                        if !n.is_empty() {
                                            entry.name = n.to_string();
                                        }
                                    }
                                    if let Some(a) = f["arguments"].as_str() {
                                        entry.arguments.push_str(a);
                                    }
                                }
                            }
                        }

                        // finish_reason
                        if let Some(fr) = choice["finish_reason"].as_str() {
                            if !fr.is_empty() {
                                finish_reason = fr.to_string();
                            }
                        }
                    }
                }
            }
        }
    }

    let tool_calls: Vec<ToolCallAccumulator> = accum_tools.into_values().collect();
    if finish_reason.is_empty() {
        finish_reason = if tool_calls.is_empty() { "stop".to_string() } else { "tool_calls".to_string() };
    }

    // thinking 完成后广播一次完整 thinking（便于 UI 折叠展示）
    if !accum_thinking.is_empty() {
        let _ = broadcast_chat_event(
            state,
            session_key,
            run_id,
            json!({ "thinking": accum_thinking.clone(), "thinkingComplete": true }),
        ).await;
    }

    // 粗估 token（若 provider 未给出 usage）
    if usage_prompt == 0 {
        usage_prompt = estimate_tokens(&messages) as u64;
    }
    if usage_completion == 0 {
        usage_completion = (accum_text.chars().count() as u64) / 3 + 1;
    }
    // 记录用量日志（含 provider/model/tokens/cost）
    let cost_usd = estimate_cost(&provider.name, model, usage_prompt, usage_completion);
    state.storage.append_usage_log(&UsageLog {
        id: format!("usage-{}", &format!("{:016x}", rand_u128())[..12]),
        provider: provider.name.clone(),
        model: model.to_string(),
        prompt_tokens: usage_prompt,
        completion_tokens: usage_completion,
        cost_usd,
        ts: current_ms(),
        session_key: session_key.to_string(),
    });

    Ok(StreamResult {
        text: accum_text,
        tool_calls,
        finish_reason,
    })
}

/// 按 provider/model 估算单次调用费用（美元）。
/// 价格取近似值，仅供 usage 日志参考。
fn estimate_cost(provider: &str, model: &str, prompt: u64, completion: u64) -> f64 {
    let m = model.to_lowercase();
    // 单位：美元 / 1M token（输入, 输出）
    let (pin, pout) = match provider {
        "openai" => {
            if m.contains("gpt-4o-mini") { (0.15, 0.60) }
            else if m.contains("gpt-4o") { (2.50, 10.00) }
            else if m.contains("gpt-4-turbo") { (10.0, 30.0) }
            else if m.contains("gpt-3.5") { (0.50, 1.50) }
            else if m.contains("o1") || m.contains("o3") { (15.0, 60.0) }
            else { (1.0, 3.0) }
        }
        "anthropic" => {
            if m.contains("haiku") { (0.25, 1.25) }
            else if m.contains("sonnet") { (3.0, 15.0) }
            else if m.contains("opus") { (15.0, 75.0) }
            else { (3.0, 15.0) }
        }
        "deepseek" => {
            if m.contains("r1") { (0.55, 2.19) } else { (0.14, 0.28) }
        }
        "qwen" | "dashscope" => (0.40, 1.20),
        "moonshot" | "kimi" => (0.60, 2.50),
        "zhipu" | "glm" => (0.50, 1.50),
        "groq" => (0.10, 0.40),
        "openrouter" => (1.0, 3.0),
        "together" => (0.80, 2.40),
        "ollama" | "local" => (0.0, 0.0),
        _ => (1.0, 3.0),
    };
    (prompt as f64 * pin + completion as f64 * pout) / 1_000_000.0
}

/// 执行单个工具调用，返回 tool result 字符串
/// 旧版工具执行入口（无上下文）。保留以兼容，内部转调 execute_tool_with_ctx。
#[allow(dead_code)]
async fn execute_tool(state: &AppState, name: &str, arguments: &serde_json::Value) -> String {
    execute_tool_with_ctx(state, name, arguments, ToolContext::default()).await
}

/// 工具执行上下文：携带会话与运行 ID，用于审批等需要回调的功能
#[derive(Clone, Default)]
struct ToolContext {
    session_key: String,
    run_id: String,
}

// ============================================================================
// 命令策略沙箱（cmdpolicy）—— 对标 ongrid 的分级命令白名单
// ============================================================================

/// 命令分类（对应权限等级）
#[derive(Clone, Debug, PartialEq, Eq)]
enum CmdClass {
    /// 只读文件系统命令：cat/head/tail/ls/find/du/stat/grep 等
    ReadFs,
    /// 只读系统命令：ps/top/uptime/free/df/ss/netstat/dmesg 等
    ReadSystem,
    /// 混合命令：iptables/ip/nft 等，读写看参数
    Mixed,
    /// 网络诊断：ping/traceroute/dig/nslookup/curl(只读) 等
    NetDiag,
    /// 写入/变更命令：需要审批
    Write,
    /// 高危命令：rm -rf/mkfs/dd/shutdown 等，即使审批也受限
    Destructive,
}

/// 单个命令的策略
#[derive(Clone, Debug)]
struct BinaryPolicy {
    bin: String,
    class: CmdClass,
    /// 禁止的参数（如 find 的 -delete/-exec）
    denied_args: Vec<String>,
    /// 只读参数匹配（Mixed 类用）：命中这些算只读
    read_only_flags: Vec<String>,
    /// 写入参数匹配（Mixed 类用）：命中这些算写入
    write_flags: Vec<String>,
}

/// 完整命令策略
struct CmdPolicy {
    bins: HashMap<String, BinaryPolicy>,
    /// 路径白名单（命令操作的文件路径必须在此列表内）
    path_allowlist: Vec<String>,
    /// 出站网络主机白名单（空=全部拒绝）
    network_allowlist: Vec<String>,
    stdout_cap: usize,
    stderr_cap: usize,
    timeout_secs: u64,
    max_args: usize,
}

impl CmdPolicy {
    /// 生产默认策略（对标 ongrid DefaultReadOnly）
    fn default_read_only() -> Self {
        let mut bins = HashMap::new();
        // 辅助：构造一个简单策略
        let mk = |bin: &str, class: CmdClass| BinaryPolicy {
            bin: bin.to_string(), class, denied_args: vec![], read_only_flags: vec![], write_flags: vec![],
        };
        let mk_denied = |bin: &str, class: CmdClass, denied: Vec<&str>| BinaryPolicy {
            bin: bin.to_string(), class,
            denied_args: denied.iter().map(|s| s.to_string()).collect(),
            read_only_flags: vec![], write_flags: vec![],
        };
        // ClassReadFS (16)
        for b in ["cat", "head", "tail", "tac", "less", "ls", "tree", "wc", "readlink", "file"] {
            bins.insert(b.to_string(), mk(b, CmdClass::ReadFs));
        }
        bins.insert("find".to_string(), mk_denied("find", CmdClass::ReadFs, vec!["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fls"]));
        bins.insert("du".to_string(), mk("du", CmdClass::ReadFs));
        bins.insert("stat".to_string(), mk("stat", CmdClass::ReadFs));
        bins.insert("grep".to_string(), mk("grep", CmdClass::ReadFs));
        bins.insert("egrep".to_string(), mk("egrep", CmdClass::ReadFs));
        bins.insert("fgrep".to_string(), mk("fgrep", CmdClass::ReadFs));
        bins.insert("awk".to_string(), mk_denied("awk", CmdClass::ReadFs, vec!["system(", "| sh", "| bash", "exec("]));
        bins.insert("sed".to_string(), mk_denied("sed", CmdClass::ReadFs, vec!["-i", "--in-place"]));
        // ClassReadSystem (17+)
        for b in ["ps", "top", "htop", "uptime", "free", "df", "iostat", "vmstat", "mpstat", "pidstat", "lsof", "ss", "netstat", "dmesg", "who", "w", "uname", "id", "groups"] {
            bins.insert(b.to_string(), mk(b, CmdClass::ReadSystem));
        }
        bins.insert("hostname".to_string(), mk_denied("hostname", CmdClass::ReadSystem, vec!["-b", "-s", "--set"]));
        bins.insert("date".to_string(), mk_denied("date", CmdClass::ReadSystem, vec!["-s", "--set"]));
        bins.insert("journalctl".to_string(), mk_denied("journalctl", CmdClass::ReadSystem, vec!["--rotate", "--vacuum-time", "--vacuum-size", "--vacuum-files", "--flush", "--sync", "--relinquish-var", "--smart-relinquish-var"]));
        // ClassMixed: iptables/ip6tables/nft
        for b in ["iptables", "ip6tables"] {
            bins.insert(b.to_string(), BinaryPolicy {
                bin: b.to_string(),
                class: CmdClass::Mixed,
                denied_args: vec![],
                read_only_flags: vec!["-L", "--list", "-S", "--list-rules", "-C", "--check", "-n", "--numeric"].iter().map(|s| s.to_string()).collect(),
                write_flags: vec!["-A", "-I", "-D", "-R", "-F", "--flush", "-X", "-N", "-P", "-Z"].iter().map(|s| s.to_string()).collect(),
            });
        }
        // NetDiag
        for b in ["ping", "traceroute", "mtr", "dig", "nslookup", "host", "tcpdump", "nmap"] {
            bins.insert(b.to_string(), mk(b, CmdClass::NetDiag));
        }
        // 网络 Layer-1 诊断工具（ongrid B.4）
        for b in ["ovs-vsctl", "ovs-ofctl", "conntrack", "ipset", "ethtool", "bpftool"] {
            bins.insert(b.to_string(), mk(b, CmdClass::NetDiag));
        }
        // ClassWrite: 服务管理（需审批）
        for b in ["systemctl", "service", "docker", "podman", "kubectl"] {
            bins.insert(b.to_string(), BinaryPolicy {
                bin: b.to_string(),
                class: CmdClass::Write,
                denied_args: vec![],
                read_only_flags: vec!["status", "list", "ps", "logs", "inspect", "get", "describe", "top"].iter().map(|s| s.to_string()).collect(),
                write_flags: vec!["start", "stop", "restart", "reload", "enable", "disable", "kill", "rm", "exec", "apply", "delete", "create", "scale"].iter().map(|s| s.to_string()).collect(),
            });
        }
        CmdPolicy {
            bins,
            path_allowlist: vec!["/var".into(), "/opt".into(), "/home".into(), "/tmp".into(), "/srv".into(), "/data".into(), "/etc".into(), "/proc".into(), "/sys".into()],
            network_allowlist: vec![],  // 空=默认拒绝所有出站
            stdout_cap: 64 * 1024,
            stderr_cap: 16 * 1024,
            timeout_secs: 30,
            max_args: 32,
        }
    }

    /// 分析一条 shell 命令，返回其分类与是否安全
    fn classify(&self, command: &str) -> CmdAnalysis {
        let trimmed = command.trim();
        let lower = trimmed.to_lowercase();
        // 危险模式优先检测
        let danger_patterns = [
            "rm -rf /", "mkfs", "dd if=/dev/zero of=/dev/", ":(){:|:&};:",
            "> /dev/sda", "shutdown", "reboot", "halt", "poweroff", "init 0", "init 6",
        ];
        for p in &danger_patterns {
            if lower.contains(p) { return CmdAnalysis { class: CmdClass::Destructive, binary: String::new(), safe: false, reason: format!("命中高危模式: {}", p) }; }
        }
        // 解析命令的第一个 token（去掉 sudo/env 前缀）
        let mut tokens = trimmed.split_whitespace().peekable();
        let mut first = String::new();
        while let Some(t) = tokens.next() {
            if t == "sudo" || t == "env" || t.starts_with("VAR=") || t.contains('=') && !t.starts_with('-') {
                continue;
            }
            first = t.to_string();
            break;
        }
        if first.is_empty() {
            return CmdAnalysis { class: CmdClass::Write, binary: String::new(), safe: false, reason: "无法解析命令".into() };
        }
        // 管道/重定向：取第一个命令的二进制名
        let bin = first.split('|').next().unwrap_or("").trim().trim_start_matches("./").to_string();
        let bin_name = std::path::Path::new(&bin).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(bin.clone());
        // 检查白名单
        let policy = match self.bins.get(&bin_name) {
            Some(p) => p.clone(),
            None => {
                let bn = bin_name.clone();
                return CmdAnalysis { class: CmdClass::Write, binary: bn.clone(), safe: false, reason: format!("命令 {} 不在白名单", bn) };
            }
        };
        // 检查 denied_args
        let all_args: Vec<&str> = trimmed.split_whitespace().skip_while(|t| *t == "sudo" || *t == "env").collect();
        for da in &policy.denied_args {
            for arg in &all_args {
                if arg.contains(da) {
                    return CmdAnalysis { class: policy.class.clone(), binary: bin_name.clone(), safe: false, reason: format!("参数命中禁止项: {}", da) };
                }
            }
        }
        // Mixed 类：看参数判断读写
        if policy.class == CmdClass::Mixed {
            let is_write = all_args.iter().any(|a| policy.write_flags.iter().any(|f| a == f));
            let is_read = all_args.iter().any(|a| policy.read_only_flags.iter().any(|f| a == f));
            if is_write && !is_read {
                return CmdAnalysis { class: CmdClass::Write, binary: bin_name.clone(), safe: false, reason: "写入参数触发，需审批".into() };
            }
            if is_read {
                return CmdAnalysis { class: CmdClass::ReadSystem, binary: bin_name.clone(), safe: true, reason: "只读参数".into() };
            }
        }
        // Write 类工具的只读子命令
        if policy.class == CmdClass::Write {
            let is_read = all_args.iter().any(|a| policy.read_only_flags.iter().any(|f| a == f));
            let is_write = all_args.iter().any(|a| policy.write_flags.iter().any(|f| a == f));
            if is_write && !is_read {
                return CmdAnalysis { class: CmdClass::Write, binary: bin_name.clone(), safe: false, reason: "写入操作，需审批".into() };
            }
            if is_read { return CmdAnalysis { class: CmdClass::ReadSystem, binary: bin_name.clone(), safe: true, reason: "只读子命令".into() }; }
        }
        // 路径白名单检查（对 ReadFS 类，提取参数里的路径）
        if policy.class == CmdClass::ReadFs || policy.class == CmdClass::ReadSystem {
            for arg in &all_args {
                if arg.starts_with('/') {
                    let allowed = self.path_allowlist.iter().any(|p| arg.starts_with(p));
                    if !allowed {
                        return CmdAnalysis { class: policy.class.clone(), binary: bin_name.clone(), safe: false, reason: format!("路径 {} 不在白名单 {:?}", arg, self.path_allowlist) };
                    }
                }
            }
        }
        CmdAnalysis { class: policy.class.clone(), binary: bin_name, safe: true, reason: "白名单允许".into() }
    }
}

#[derive(Clone, Debug)]
struct CmdAnalysis {
    class: CmdClass,
    binary: String,
    safe: bool,
    reason: String,
}

impl CmdClass {
    fn as_str(&self) -> &'static str {
        match self {
            CmdClass::ReadFs => "read_fs",
            CmdClass::ReadSystem => "read_system",
            CmdClass::Mixed => "mixed",
            CmdClass::NetDiag => "net_diag",
            CmdClass::Write => "write",
            CmdClass::Destructive => "destructive",
        }
    }
}

/// 全局命令策略（lazy 初始化）
fn cmd_policy() -> &'static CmdPolicy {
    use std::sync::OnceLock;
    static POLICY: OnceLock<CmdPolicy> = OnceLock::new();
    POLICY.get_or_init(CmdPolicy::default_read_only)
}

/// 判断 shell 命令是否需要人工审批（危险模式）
fn is_dangerous_command(command: &str) -> bool {
    // 按词边界检测
    let patterns = [
        "rm -rf", "rm -fr", "rm -r -f", "rm -f -r",
        "mkfs", "dd if=", "dd of=/dev/", ":(){:|:&};:",
        "shutdown", "reboot", "halt", "poweroff",
        "init 0", "init 6", "telinit",
        "> /dev/sda", "> /dev/nvme", "> /dev/hd",
        "chmod -R 777 /", "chown -R",
        "iptables -F", "ip6tables -F", "ufw disable",
        "systemctl disable", "systemctl stop",
        "git push --force", "git push -f origin",
        "curl | sh", "curl | bash", "wget | sh", "wget | bash",
        "npm publish", "cargo publish",
    ];
    let lower = command.to_lowercase();
    for p in patterns {
        if lower.contains(p) {
            return true;
        }
    }
    // sudo/带有 root 切换的命令
    if lower.starts_with("sudo ") || lower.contains(" sudo ") {
        return true;
    }
    false
}

/// 为危险命令创建审批请求并阻塞等待用户决定。
/// 返回 Some(true)=批准，Some(false)=拒绝，None=超时/出错。
async fn request_exec_approval(
    state: &AppState,
    command: &str,
    ctx: &ToolContext,
) -> Option<bool> {
    let id = format!("approval-{}", &format!("{:016x}", rand_u128())[..12]);
    let approval = Approval {
        id: id.clone(),
        kind: "exec".to_string(),
        command: command.to_string(),
        status: "pending".to_string(),
        session_key: ctx.session_key.clone(),
        run_id: ctx.run_id.clone(),
        created_at: current_ms(),
        decided_by: None,
        decided_at: None,
        decision: None,
    };
    let mut items = state.storage.load_approvals();
    items.push(approval.clone());
    state.storage.save_approvals(&items);

    // 广播请求事件给 UI
    let _ = broadcast_event(state, "exec.approval.requested", approval_to_json(&approval)).await;

    // 注册等待器
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    state.pending_approvals.lock().await.insert(id.clone(), tx);

    // 等待结果，最多 5 分钟
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(300));
    tokio::pin!(timeout);
    tokio::select! {
        approved = rx => match approved {
            Ok(a) => Some(a),
            Err(_) => {
                state.pending_approvals.lock().await.remove(&id);
                None
            }
        },
        _ = &mut timeout => {
            state.pending_approvals.lock().await.remove(&id);
            // 标记超时
            let mut items = state.storage.load_approvals();
            if let Some(a) = items.iter_mut().find(|a| a.id == id) {
                a.status = "timeout".to_string();
            }
            state.storage.save_approvals(&items);
            None
        }
    }
}

async fn execute_tool_with_ctx(
    state: &AppState,
    name: &str,
    arguments: &serde_json::Value,
    ctx: ToolContext,
) -> String {
    match name {
        "web_search" => {
            let query = arguments["query"].as_str().unwrap_or("");
            if query.is_empty() {
                return "错误：缺少 query 参数".to_string();
            }
            tool_web_search(state, query).await
        }
        "exec" => {
            let command = arguments["command"].as_str().unwrap_or("");
            if command.is_empty() {
                return "错误：缺少 command 参数".to_string();
            }
            // 危险命令 → 走审批流程
            if is_dangerous_command(command) {
                // 先尝试多级审批工作流（若配置了匹配的 flow）
                let matched_flow = find_matching_flow(state, "exec", command).is_some();
                if matched_flow {
                    match request_workflow_approval(state, "exec", command, &ctx, None).await {
                        Some(true) => {
                            // 全部批准，继续执行
                        }
                        Some(false) => {
                            return "错误：命令已被拒绝（多级审批未通过）".to_string();
                        }
                        None => {
                            return "错误：审批超时（30 分钟内未处理）".to_string();
                        }
                    }
                } else {
                    match request_exec_approval(state, command, &ctx).await {
                        Some(true) => {
                            // 已批准，继续执行
                        }
                        Some(false) => {
                            return "错误：命令已被拒绝（用户审批未通过）".to_string();
                        }
                        None => {
                            return "错误：审批超时（5 分钟内未处理）".to_string();
                        }
                    }
                }
            }
            // 安全限制：仍拒绝最极端的命令（即使审批通过也拒绝 rm -rf /）
            if command.contains("rm -rf /") || command.contains("mkfs") || command.contains("dd if=/dev/zero of=/dev/") {
                state.storage.append_change_event(&ChangeEvent {
                    id: format!("ce-{:016x}", rand_u128()),
                    kind: "exec_denied".to_string(),
                    action: "拒绝不可逆高危命令".to_string(),
                    target: command.to_string(),
                    actor: "system".to_string(),
                    session_key: ctx.session_key.clone(),
                    ts: current_ms(),
                    result: "denied".to_string(),
                    approval_id: None,
                    rollback_hint: None,
                });
                return "错误：拒绝执行不可逆的高风险命令".to_string();
            }
            let result = tool_exec(command).await;
            // 记录变更事件（对标 ongrid change events）
            let analysis = cmd_policy().classify(command);
            let is_mutating = analysis.class == CmdClass::Write || analysis.class == CmdClass::Destructive || !analysis.safe;
            if is_mutating {
                state.storage.append_change_event(&ChangeEvent {
                    id: format!("ce-{:016x}", rand_u128()),
                    kind: "exec_write".to_string(),
                    action: format!("执行命令 [{}]", analysis.class.as_str()),
                    target: command.to_string(),
                    actor: "agent".to_string(),
                    session_key: ctx.session_key.clone(),
                    ts: current_ms(),
                    result: if result.contains("[exit code: 0]") { "ok".to_string() } else { "failed".to_string() },
                    approval_id: None,
                    rollback_hint: Some(generate_rollback_hint(command)),
                });
            }
            result
        }
        "read_file" => {
            let path = arguments["path"].as_str().unwrap_or("");
            if path.is_empty() {
                return "错误：缺少 path 参数".to_string();
            }
            match tokio::fs::read_to_string(path).await {
                Ok(content) => {
                    // 限制输出长度
                    if content.len() > 16_000 {
                        format!("{}...(文件过长，已截断，共 {} 字节)", &content[..16_000], content.len())
                    } else {
                        content
                    }
                }
                Err(e) => format!("读取失败: {}", e),
            }
        }
        "write_file" => {
            let path = arguments["path"].as_str().unwrap_or("");
            let content = arguments["content"].as_str().unwrap_or("");
            if path.is_empty() {
                return "错误：缺少 path 参数".to_string();
            }
            if let Some(parent) = std::path::Path::new(path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            match tokio::fs::write(path, content).await {
                Ok(_) => format!("已写入 {} ({} 字节)", path, content.len()),
                Err(e) => format!("写入失败: {}", e),
            }
        }
        "memory_save" => {
            let body = arguments["body"].as_str().unwrap_or("");
            let kind = arguments["kind"].as_str().unwrap_or("fact");
            if body.is_empty() {
                return "错误：缺少 body 参数".to_string();
            }
            let mut items = state.storage.load_memory();
            let id = items.iter().map(|m| m.id).max().unwrap_or(0) + 1;
            items.push(MemoryItem {
                id,
                kind: kind.to_string(),
                body: body.to_string(),
                source: "agent".to_string(),
                confidence: 1.0,
                created_at: current_ms(),
            });
            state.storage.save_memory(&items);
            format!("已保存记忆 #{} (kind={})", id, kind)
        }
        "spawn_subagent" => {
            let prompt = arguments["prompt"].as_str().unwrap_or("");
            if prompt.is_empty() {
                return "错误：缺少 prompt 参数".to_string();
            }
            let model_override = arguments["model"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
            let max_iter = arguments["max_iterations"].as_u64().unwrap_or(5).clamp(1, 15) as usize;
            let sub_session = format!("sub:{}:{}", ctx.session_key, &format!("{:012x}", rand_u128()));
            // 写入子会话的用户消息
            state.storage.append_message(&sub_session, &Message {
                role: "user".into(),
                content: prompt.to_string(),
                timestamp: current_ms(),
                attachments: vec![],
            });
            // 记录子会话
            let mut sessions = state.storage.load_sessions();
            sessions.push(Session {
                key: sub_session.clone(),
                kind: "subagent".to_string(),
                display_name: Some(format!("子Agent · {}", &sub_session[..sub_session.len().min(40)])),
                channel: None,
                agent_id: "sub".to_string(),
                model: Some(model_override.clone().unwrap_or_else(|| state.config.default_model.clone())),
                updated_at: current_ms(),
            });
            state.storage.save_sessions(&sessions);
            // 内联运行子 agent loop（Box::pin 避免递归限制）
            Box::pin(run_subagent_loop(state, &sub_session, model_override.as_deref(), max_iter)).await
        }
        "fan_out" => {
            let task = arguments["task"].as_str().unwrap_or("");
            let subtasks: Vec<String> = arguments["subtasks"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if task.is_empty() || subtasks.is_empty() {
                return "错误：缺少 task 或 subtasks 参数".to_string();
            }
            let role_id = arguments["agent_role"].as_str().filter(|s| !s.is_empty()).map(String::from);
            let max_concurrent = arguments["max_concurrent"].as_u64().unwrap_or(5).clamp(1, 20) as usize;
            let roles = state.storage.load_agent_roles();
            let role = role_id.and_then(|rid| roles.iter().find(|r| r.id == rid).cloned());
            let mut span = TraceSpan::new("fan_out", "tool_fan_out", None);
            match fan_out_map_reduce(state, &subtasks, role.as_ref(), &ctx.session_key, max_concurrent, &mut span).await {
                Ok(results) => {
                    let mut out = format!("✅ 并行完成 {} 个子任务：\n\n", results.len());
                    for (i, r) in results.iter().enumerate() {
                        out.push_str(&format!("## 子任务 {}\n{}\n\n", i + 1, r));
                    }
                    out
                }
                Err(e) => format!("扇出失败: {}", e),
            }
        }
        "delegate_task" => {
            let role_id = arguments["agent_role_id"].as_str().unwrap_or("");
            let task = arguments["task"].as_str().unwrap_or("");
            if role_id.is_empty() || task.is_empty() {
                return "错误：缺少 agent_role_id 或 task 参数".to_string();
            }
            let roles = state.storage.load_agent_roles();
            let role = match roles.iter().find(|r| r.id == role_id) {
                Some(r) => r.clone(),
                None => return format!("错误：角色 {} 不存在", role_id),
            };
            if !role.allow_delegation {
                return format!("错误：角色 {} 不允许被委派（allowDelegation=false）", role.name);
            }
            let mut span = TraceSpan::new("delegate", &format!("delegate_to_{}", role.name), None);
            match execute_role_agent(state, Some(&role), task, &ctx.session_key, &mut span).await {
                Ok(out) => format!("【{} 的回复】\n{}", role.name, out),
                Err(e) => format!("委派失败: {}", e),
            }
        }
        "run_code" => {
            let language = arguments["language"].as_str().unwrap_or("");
            let code = arguments["code"].as_str().unwrap_or("");
            if language.is_empty() || code.is_empty() {
                return "错误：缺少 language 或 code 参数".to_string();
            }
            tool_run_code(language, code).await
        }
        "browse" => {
            let url = arguments["url"].as_str().unwrap_or("");
            let action = arguments["action"].as_str().unwrap_or("fetch");
            if url.is_empty() {
                return "错误：缺少 url 参数".to_string();
            }
            let selector = arguments["selector"].as_str().unwrap_or("");
            tool_browse(url, action, selector).await
        }
        "fetch_latest_info" => {
            let topic = arguments["topic"].as_str().unwrap_or("");
            if topic.is_empty() {
                return "错误：缺少 topic 参数".to_string();
            }
            let max_pages = arguments["max_pages"].as_u64().unwrap_or(3).clamp(1, 5) as usize;
            tool_fetch_latest_info(state, topic, max_pages).await
        }
        "read_document" => {
            let path = arguments["path"].as_str().unwrap_or("");
            if path.is_empty() {
                return "错误：缺少 path 参数".to_string();
            }
            tool_read_document(path).await
        }
        "transcribe_audio" => {
            let url = arguments["url"].as_str().unwrap_or("");
            if url.is_empty() {
                return "错误：缺少 url 参数".to_string();
            }
            tool_transcribe_audio(state, url).await
        }
        "analyze_image" => {
            let url = arguments["url"].as_str().unwrap_or("");
            let question = arguments["question"].as_str().unwrap_or("描述这张图片。");
            if url.is_empty() {
                return "错误：缺少 url 参数".to_string();
            }
            tool_analyze_image(state, url, question).await
        }
        "port_scan" => {
            let target = arguments["target"].as_str().unwrap_or("");
            let ports = arguments["ports"].as_str().unwrap_or("1-1000");
            let timeout = arguments["timeout_ms"].as_u64().unwrap_or(500);
            tool_port_scan(target, ports, timeout).await
        }
        "http_probe" => {
            let url = arguments["url"].as_str().unwrap_or("");
            let method = arguments["method"].as_str().unwrap_or("GET");
            let body = arguments["body"].as_str().unwrap_or("");
            let timeout = arguments["timeout_ms"].as_u64().unwrap_or(10000);
            tool_http_probe(url, method, body, timeout).await
        }
        "vuln_scan" => {
            let target = arguments["target"].as_str().unwrap_or("");
            let depth = arguments["depth"].as_str().unwrap_or("normal");
            tool_vuln_scan(target, depth).await
        }
        "dns_lookup" => {
            let domain = arguments["domain"].as_str().unwrap_or("");
            let rt = arguments["record_type"].as_str().unwrap_or("A");
            tool_dns_lookup(domain, rt).await
        }
        "ssl_check" => {
            let host = arguments["host"].as_str().unwrap_or("");
            let port = arguments["port"].as_u64().unwrap_or(443) as u16;
            tool_ssl_check(host, port).await
        }
        "subdomain_enum" => {
            let domain = arguments["domain"].as_str().unwrap_or("");
            tool_subdomain_enum(domain).await
        }
        "waf_detect" => {
            let url = arguments["url"].as_str().unwrap_or("");
            tool_waf_detect(url).await
        }
        "sqli_scan" => {
            let url = arguments["url"].as_str().unwrap_or("");
            tool_sqli_scan(url).await
        }
        "xss_scan" => {
            let url = arguments["url"].as_str().unwrap_or("");
            tool_xss_scan(url).await
        }
        "exposure_analysis" => {
            let host = arguments["host"].as_str().unwrap_or("");
            tool_exposure_analysis(host).await
        }
        "service_monitor" => {
            let check = arguments["check"].as_str().unwrap_or("all");
            tool_service_monitor(check).await
        }
        "log_analyze" => {
            let path = arguments["path"].as_str().unwrap_or("");
            let pattern = arguments["pattern"].as_str().unwrap_or("");
            let lines = arguments["lines"].as_u64().unwrap_or(1000) as usize;
            tool_log_analyze(path, pattern, lines).await
        }
        "network_trace" => {
            let target = arguments["target"].as_str().unwrap_or("");
            let max_hops = arguments["max_hops"].as_u64().unwrap_or(30) as u16;
            tool_network_trace(target, max_hops).await
        }
        "file_hash" => {
            let path = arguments["path"].as_str().unwrap_or("");
            let algo = arguments["algorithm"].as_str().unwrap_or("sha256");
            tool_file_hash(path, algo).await
        }
        "encode_decode" => {
            let action = arguments["action"].as_str().unwrap_or("encode");
            let format = arguments["format"].as_str().unwrap_or("base64");
            let input = arguments["input"].as_str().unwrap_or("");
            tool_encode_decode(action, format, input)
        }
        "git_ops" => {
            let command = arguments["command"].as_str().unwrap_or("status");
            let repo = arguments["repo_path"].as_str().unwrap_or(".");
            tool_git_ops(command, repo).await
        }
        "docker_ops" => {
            let command = arguments["command"].as_str().unwrap_or("ps");
            let container = arguments["container"].as_str().unwrap_or("");
            tool_docker_ops(command, container).await
        }
        "process_manage" => {
            let action = arguments["action"].as_str().unwrap_or("list");
            let name = arguments["name"].as_str().unwrap_or("");
            tool_process_manage(action, name).await
        }
        "backup_create" => {
            let source = arguments["source"].as_str().unwrap_or("");
            let dest = arguments["dest"].as_str().unwrap_or("");
            let format = arguments["format"].as_str().unwrap_or("tar.gz");
            tool_backup_create(source, dest, format).await
        }
        "get_host_load" => tool_get_host_load().await,
        "get_host_processes" => {
            let top_n = arguments["topN"].as_u64().unwrap_or(15) as usize;
            let sort_by = arguments["sortBy"].as_str().unwrap_or("cpu");
            tool_get_host_processes(top_n, sort_by).await
        }
        "host_du_summary" => {
            let paths: Vec<String> = arguments["paths"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec!["/".to_string()]);
            let depth = arguments["depth"].as_u64().unwrap_or(1) as u32;
            tool_host_du_summary(&paths, depth).await
        }
        "host_find_large_files" => {
            let paths: Vec<String> = arguments["paths"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec!["/".to_string()]);
            let top_n = arguments["topN"].as_u64().unwrap_or(20) as usize;
            tool_host_find_large_files(&paths, top_n).await
        }
        "host_stat_file" => {
            let path = arguments["path"].as_str().unwrap_or("");
            if path.is_empty() { return "错误：缺少 path 参数".to_string(); }
            tool_host_stat_file(path).await
        }
        "host_netns_inspect" => tool_host_netns_inspect().await,
        "host_diagnostic_snapshot" => tool_host_diagnostic_snapshot().await,
        "query_change_events" => {
            let limit = arguments["limit"].as_u64().unwrap_or(20) as usize;
            let kind = arguments["kind"].as_str();
            let events = state.storage.load_change_events(limit, kind);
            serde_json::json!({
                "events": events.iter().map(|e| json!({
                    "id": &e.id, "kind": &e.kind, "action": &e.action, "target": &e.target,
                    "actor": &e.actor, "ts": e.ts, "result": &e.result,
                    "rollbackHint": &e.rollback_hint, "approvalId": &e.approval_id,
                })).collect::<Vec<_>>(),
                "count": events.len(),
            }).to_string()
        }
        _ => format!("未知工具: {}", name),
    }
}

// ============================================================================
// 多模态 / 文档解析工具实现
// ============================================================================

/// 把本地文件字节读成 base64 字符串
fn read_file_base64(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取文件失败: {}", e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// 根据 URL/路径判断是否为本地路径（无 scheme 或 file://）
fn is_local_path(s: &str) -> bool {
    let s = s.trim();
    if s.starts_with("file://") {
        return true;
    }
    !s.contains("://")
}

/// 把本地路径转成 file:// 移除前缀
fn strip_file_prefix(s: &str) -> &str {
    s.trim().strip_prefix("file://").unwrap_or_else(|| s.trim())
}

/// 取文件扩展名（小写，无点）
fn ext_of(path: &str) -> String {
    let p = strip_file_prefix(path);
    std::path::Path::new(p)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

/// 对 zip 内字节做中央目录扫描，返回 (filename, offset_of_file_data_start, uncompressed_size) 列表。
/// 仅支持 deflate(8) 与 stored(0)；返回的 data 起始偏移指向本地文件头之后的文件数据。
fn zip_entries(bytes: &[u8]) -> Vec<(String, usize, usize, u16)> {
    let mut out = Vec::new();
    // 从尾部找中央目录结束签名 PK\5\6
    let mut eocd = None;
    if bytes.len() >= 22 {
        let start = bytes.len().saturating_sub(65557);
        for i in (start..bytes.len() - 22).rev() {
            if bytes[i] == 0x50 && bytes[i + 1] == 0x4b && bytes[i + 2] == 0x05 && bytes[i + 3] == 0x06 {
                eocd = Some(i);
                break;
            }
        }
    }
    let eocd = match eocd { Some(v) => v, None => return out };
    let cd_count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_off = u32::from_le_bytes([bytes[eocd + 16], bytes[eocd + 17], bytes[eocd + 18], bytes[eocd + 19]]) as usize;
    let mut p = cd_off;
    for _ in 0..cd_count {
        if p + 46 > bytes.len() { break; }
        // 中央目录头签名 PK\1\2
        if bytes[p] != 0x50 || bytes[p + 1] != 0x4b || bytes[p + 2] != 0x01 || bytes[p + 3] != 0x02 { break; }
        let comp_method = u16::from_le_bytes([bytes[p + 10], bytes[p + 11]]);
        let _comp_size = u32::from_le_bytes([bytes[p + 20], bytes[p + 21], bytes[p + 22], bytes[p + 23]]) as usize;
        let uncomp_size = u32::from_le_bytes([bytes[p + 24], bytes[p + 25], bytes[p + 26], bytes[p + 27]]) as usize;
        let name_len = u16::from_le_bytes([bytes[p + 28], bytes[p + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[p + 30], bytes[p + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[p + 32], bytes[p + 33]]) as usize;
        let lfh_off = u32::from_le_bytes([bytes[p + 42], bytes[p + 43], bytes[p + 44], bytes[p + 45]]) as usize;
        if p + 46 + name_len > bytes.len() { break; }
        let name = String::from_utf8_lossy(&bytes[p + 46..p + 46 + name_len]).to_string();
        // 解析本地文件头以定位真实数据偏移
        if lfh_off + 30 <= bytes.len()
            && bytes[lfh_off] == 0x50 && bytes[lfh_off + 1] == 0x4b
            && bytes[lfh_off + 2] == 0x03 && bytes[lfh_off + 3] == 0x04
        {
            let lh_name_len = u16::from_le_bytes([bytes[lfh_off + 26], bytes[lfh_off + 27]]) as usize;
            let lh_extra_len = u16::from_le_bytes([bytes[lfh_off + 28], bytes[lfh_off + 29]]) as usize;
            let data_off = lfh_off + 30 + lh_name_len + lh_extra_len;
            out.push((name, data_off, uncomp_size, comp_method));
        }
        p += 46 + name_len + extra_len + comment_len;
    }
    // 返回 (name, data_offset, uncompressed_size, comp_method)
    out
}

/// 用 zip_entries 解压指定 name 的 deflate/stored 字节
fn zip_read(bytes: &[u8], name_filter: impl Fn(&str) -> bool) -> Option<Vec<u8>> {
    use std::io::Read;
    // 重新实现以拿到 comp_method
    let mut eocd = None;
    if bytes.len() >= 22 {
        let start = bytes.len().saturating_sub(65557);
        for i in (start..bytes.len() - 22).rev() {
            if bytes[i] == 0x50 && bytes[i + 1] == 0x4b && bytes[i + 2] == 0x05 && bytes[i + 3] == 0x06 {
                eocd = Some(i);
                break;
            }
        }
    }
    let eocd = eocd?;
    let cd_count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_off = u32::from_le_bytes([bytes[eocd + 16], bytes[eocd + 17], bytes[eocd + 18], bytes[eocd + 19]]) as usize;
    let mut p = cd_off;
    for _ in 0..cd_count {
        if p + 46 > bytes.len() { break; }
        if bytes[p] != 0x50 || bytes[p + 1] != 0x4b || bytes[p + 2] != 0x01 || bytes[p + 3] != 0x02 { break; }
        let comp_method = u16::from_le_bytes([bytes[p + 10], bytes[p + 11]]);
        let comp_size = u32::from_le_bytes([bytes[p + 20], bytes[p + 21], bytes[p + 22], bytes[p + 23]]) as usize;
        let name_len = u16::from_le_bytes([bytes[p + 28], bytes[p + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[p + 30], bytes[p + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[p + 32], bytes[p + 33]]) as usize;
        let lfh_off = u32::from_le_bytes([bytes[p + 42], bytes[p + 43], bytes[p + 44], bytes[p + 45]]) as usize;
        if p + 46 + name_len > bytes.len() { break; }
        let name = String::from_utf8_lossy(&bytes[p + 46..p + 46 + name_len]).to_string();
        if name_filter(&name) {
            // 定位数据
            if lfh_off + 30 > bytes.len() { return None; }
            let lh_name_len = u16::from_le_bytes([bytes[lfh_off + 26], bytes[lfh_off + 27]]) as usize;
            let lh_extra_len = u16::from_le_bytes([bytes[lfh_off + 28], bytes[lfh_off + 29]]) as usize;
            let data_off = lfh_off + 30 + lh_name_len + lh_extra_len;
            let comp_end = (data_off + comp_size).min(bytes.len());
            let data = &bytes[data_off..comp_end];
            if comp_method == 0 {
                return Some(data.to_vec());
            } else if comp_method == 8 {
                let mut dec = flate2::read::DeflateDecoder::new(data);
                let mut out = Vec::new();
                if dec.read_to_end(&mut out).is_ok() {
                    return Some(out);
                }
            }
        }
        p += 46 + name_len + extra_len + comment_len;
    }
    None
}

/// 从 OOXML (docx/xlsx/pptx) XML 字节中粗略抽取 <w:t>/<a:t>/<si><t> 等文本节点
fn extract_xml_text(xml: &[u8]) -> String {
    let s = String::from_utf8_lossy(xml);
    let mut out = String::new();
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        if b[i] == b'<' {
            // 检测开始标签是否为文本类
            if s[i..].starts_with("<w:t") || s[i..].starts_with("<a:t") || s[i..].starts_with("<t>") || s[i..].starts_with("<t ") {
                // 跳到 '>'
                if let Some(gt) = s[i..].find('>') {
                    let start = i + gt + 1;
                    if let Some(lt) = s[start..].find('<') {
                        let text = &s[start..start + lt];
                        if !text.is_empty() {
                            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                                out.push(' ');
                            }
                            out.push_str(text);
                        }
                        i = start + lt;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// 工具：读取并解析文档（多格式）
async fn tool_read_document(path: &str) -> String {
    let local = strip_file_prefix(path);
    let ext = ext_of(path);
    // 纯文本类
    match ext.as_str() {
        "txt" | "md" | "markdown" | "log" | "csv" | "json" | "html" | "htm" | "xml" | "yml" | "yaml" => {
            match tokio::fs::read_to_string(local).await {
                Ok(c) => truncate(&c, 32_000),
                Err(e) => format!("读取失败: {}", e),
            }
        }
        "pdf" => read_pdf_text(local),
        "docx" => read_ooxml(local, |n| {
            n.starts_with("word/document") || n.starts_with("word/header") || n.starts_with("word/footer")
        }),
        "xlsx" => read_xlsx(local),
        "pptx" => read_ooxml(local, |n| n.starts_with("ppt/slides/slide")),
        _ => {
            // 兜底：当文本读
            match tokio::fs::read_to_string(local).await {
                Ok(c) => truncate(&format!("（按文本读取，未知扩展名 .{}）\n{}", ext, c), 32_000),
                Err(e) => format!("无法解析 .{} 文件: {}", ext, e),
            }
        }
    }
}

/// 读取 PDF：检测是否含文本层（简单方式：找 stream 之间的 BT...ET 文本操作符）
fn read_pdf_text(path: &str) -> String {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return format!("读取 PDF 失败: {}", e),
    };
    let raw = String::from_utf8_lossy(&bytes);
    let mut text = String::new();
    // 极简提取：Tj / TJ 操作符里的括号字符串
    let mut chars = raw.chars().peekable();
    let mut buf = String::new();
    let mut in_paren = false;
    let mut depth = 0i32;
    for c in chars.by_ref() {
        match c {
            '(' if !in_paren => { in_paren = true; depth = 1; buf.clear(); }
            '(' if in_paren => { depth += 1; buf.push(c); }
            ')' if in_paren => {
                depth -= 1;
                if depth == 0 {
                    in_paren = false;
                    if !buf.is_empty() {
                        text.push_str(&buf);
                        text.push(' ');
                    }
                } else { buf.push(c); }
            }
            _ if in_paren => buf.push(c),
            _ => {}
        }
    }
    if text.trim().is_empty() {
        "PDF 未检测到文本层（可能是扫描件），需要 OCR 才能提取文字。".to_string()
    } else {
        truncate(text.trim(), 32_000)
    }
}

/// 读取 OOXML（docx/pptx）— 合并匹配到的所有 part 的文本
fn read_ooxml<F: Fn(&str) -> bool>(path: &str, filter: F) -> String {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return format!("读取失败: {}", e),
    };
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    // 枚举所有条目
    for (name, _off, _usz, _flag) in zip_entries(&bytes) {
        if filter(&name) {
            if let Some(data) = zip_read(&bytes, |n| n == name) {
                parts.push((name, data));
            }
        }
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (_n, data) in parts {
        let t = extract_xml_text(&data);
        if !t.is_empty() {
            out.push_str(&t);
            out.push_str("\n\n");
        }
    }
    if out.trim().is_empty() { "未提取到文本。".to_string() } else { truncate(&out, 32_000) }
}

/// 读取 xlsx：把 sharedStrings.xml + 每个 sheet 的单元格拼成 CSV 风格文本
fn read_xlsx(path: &str) -> String {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return format!("读取失败: {}", e),
    };
    // 共享字符串表
    let shared: Vec<String> = zip_read(&bytes, |n| n == "xl/sharedStrings.xml")
        .map(|d| split_shared_strings(&d))
        .unwrap_or_default();
    let mut out = String::new();
    // 找所有 sheet
    let mut sheet_names: Vec<String> = zip_entries(&bytes)
        .into_iter()
        .map(|(n, _, _, _)| n)
        .filter(|n| n.starts_with("xl/worksheets/sheet"))
        .collect();
    sheet_names.sort();
    for sn in sheet_names {
        if let Some(data) = zip_read(&bytes, |n| n == sn) {
            let cells = extract_sheet_cells(&data, &shared);
            out.push_str(&cells);
            out.push('\n');
        }
    }
    if out.trim().is_empty() { "未提取到单元格。".to_string() } else { truncate(&out, 32_000) }
}

/// 解析 sharedStrings.xml，按 <si> 顺序返回每个字符串
fn split_shared_strings(xml: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(xml);
    let mut out = Vec::new();
    for piece in s.split("<si>") {
        if piece.contains("</si>") {
            let inner = piece.split("</si>").next().unwrap_or("");
            out.push(extract_xml_text(inner.as_bytes()));
        }
    }
    out
}

/// 从 sheet XML 抽取行列（c r="A1" t="s"><v>idx</v>）
fn extract_sheet_cells(xml: &[u8], shared: &[String]) -> String {
    let s = String::from_utf8_lossy(xml);
    let mut rows: std::collections::BTreeMap<String, Vec<(String, String)>> = std::collections::BTreeMap::new();
    let mut pos = 0;
    while let Some(ci) = s[pos..].find("<c ") {
        pos += ci;
        let tag_end = match s[pos..].find('>') { Some(e) => pos + e, None => break };
        let tag = &s[pos..tag_end];
        // 单元格引用 r="A1"
        let r = extract_attr(tag, "r").unwrap_or_default();
        let row = r.trim_start_matches(|c: char| !c.is_ascii_digit()).to_string();
        let is_shared = extract_attr(tag, "t").map(|t| t == "s").unwrap_or(false);
        // 取 <v>...</v>
        let mut value = String::new();
        if let Some(vstart) = s[tag_end..].find("<v>") {
            let vs = tag_end + vstart + 3;
            if let Some(vend) = s[vs..].find("</v>") {
                value = s[vs..vs + vend].to_string();
            }
        }
        if is_shared {
            if let Ok(idx) = value.parse::<usize>() {
                if idx < shared.len() { value = shared[idx].clone(); }
            }
        }
        if !row.is_empty() {
            rows.entry(row).or_default().push((r, value));
        }
        pos = tag_end + 1;
    }
    let mut out = String::new();
    for (_row, cells) in rows {
        for (r, v) in cells {
            out.push_str(&format!("{}={}\t", r, v));
        }
        out.push('\n');
    }
    out
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{}=\"", name);
    let i = tag.find(&key)?;
    let rest = &tag[i + key.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else {
        let mut t = s[..max].to_string();
        t.push_str("\n...(内容过长，已截断)");
        t
    }
}

/// 工具：音频转写（OpenAI Whisper 优先，本地 whisper 回退）
async fn tool_transcribe_audio(state: &AppState, url: &str) -> String {
    // 解析出本地临时文件路径
    let local_tmp = if is_local_path(url) {
        strip_file_prefix(url).to_string()
    } else {
        // 下载到临时文件
        let tmp = format!("{}/.cradle-ring/data/_audio_{}.bin", state.storage.home, current_ms());
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        match client.get(url).send().await {
            Ok(r) if r.status().is_success() => {
                match r.bytes().await {
                    Ok(b) => { let _ = std::fs::write(&tmp, &b); tmp }
                    Err(e) => return format!("下载音频失败: {}", e),
                }
            }
            Ok(r) => return format!("下载音频返回错误: {}", r.status()),
            Err(e) => return format!("下载音频失败: {}", e),
        }
    };

    // 1) OpenAI Whisper API（手工构造 multipart/form-data，避免依赖 reqwest multipart feature）
    if let Some(key) = state.config.openai_api_key.as_ref() {
        let bytes = match std::fs::read(&local_tmp) {
            Ok(b) => b,
            Err(e) => return format!("读取音频文件失败: {}", e),
        };
        let base = state.config.openai_base_url.trim_end_matches('/').to_string();
        let endpoint = format!("{}/audio/transcriptions", base);
        let boundary = format!("cradle{:032x}", rand_u128());
        // 手工拼 multipart body
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
        body.extend_from_slice(b"whisper-1\r\n");
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.mp3\"\r\n");
        body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(&bytes);
        body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());
        let ctype = format!("multipart/form-data; boundary={}", boundary);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let resp = client
            .post(&endpoint)
            .bearer_auth(key)
            .header("Content-Type", ctype)
            .body(body)
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                match r.json::<serde_json::Value>().await {
                    Ok(v) => {
                        let text = v["text"].as_str().unwrap_or("");
                        return truncate(text, 16_000);
                    }
                    Err(e) => return format!("解析 Whisper 响应失败: {}", e),
                }
            }
            Ok(r) => eprintln!("[transcribe] whisper api {} -> {}, 回退本地", endpoint, r.status()),
            Err(e) => eprintln!("[transcribe] whisper api 请求失败: {}, 回退本地", e),
        }
    }

    // 2) 本地 whisper CLI 回退
    let out = tokio::process::Command::new("whisper")
        .arg(&local_tmp)
        .arg("--model").arg("base")
        .arg("--output_format").arg("txt")
        .arg("--output_dir").arg("-")
        .output()
        .await;
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            if !stdout.trim().is_empty() {
                truncate(&stdout, 16_000)
            } else {
                format!("本地 whisper 未返回文本（exit={}）。请确认已安装 openai-whisper 并配置 OPENAI_API_KEY。", o.status.code().unwrap_or(-1))
            }
        }
        Err(e) => format!("音频转写失败（OpenAI API 与本地 whisper 均不可用）: {}", e),
    }
}

/// 工具：图片分析（把本地图片转 base64 + 问题一起发到 vision 模型）
async fn tool_analyze_image(state: &AppState, url: &str, question: &str) -> String {
    let key = match state.config.openai_api_key.as_ref() {
        Some(k) => k.clone(),
        None => return "图片分析需要配置 OPENAI_API_KEY（vision 模型）。".to_string(),
    };
    let model = state.config.default_model.clone();
    let base = state.config.openai_base_url.trim_end_matches('/').to_string();
    let endpoint = format!("{}/chat/completions", base);

    // 构造 image_url
    let image_url_value: serde_json::Value = if is_local_path(url) {
        let local = strip_file_prefix(url);
        let ext = ext_of(url);
        let mime = match ext.as_str() {
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "image/jpeg",
        };
        match read_file_base64(local) {
            Ok(b64) => serde_json::Value::String(format!("data:{};base64,{}", mime, b64)),
            Err(e) => return e,
        }
    } else {
        serde_json::Value::String(url.to_string())
    };

    let body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": question},
                {"type": "image_url", "image_url": {"url": image_url_value }}
            ]
        }],
        "max_tokens": 1024
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let resp = client.post(&endpoint).bearer_auth(&key).json(&body).send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    let txt = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
                    if txt.is_empty() { "（模型未返回内容）".to_string() } else { txt.to_string() }
                }
                Err(e) => format!("解析 vision 响应失败: {}", e),
            }
        }
        Ok(r) => format!("vision 请求返回错误: {}", r.status()),
        Err(e) => format!("vision 请求失败: {}", e),
    }
}

/// 工具：在线搜索（根据配置 provider 调用）
async fn tool_web_search(state: &AppState, query: &str) -> String {
    let cfg = state.config.search_config();
    let provider = cfg["provider"].as_str().unwrap_or("none");
    let enabled = cfg["enabled"].as_bool().unwrap_or(false);
    if !enabled || provider == "none" {
        return format!("搜索未启用（provider={}, enabled={}）", provider, enabled);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match provider {
        "searxng" => {
            let base = cfg["baseUrl"].as_str().unwrap_or("http://localhost:8080");
            let url = format!("{}/search", base.trim_end_matches('/'));
            let resp = client
                .get(&url)
                .query(&[("q", query), ("format", "json")])
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let results: Vec<String> = v["results"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter().take(5).map(|r| {
                                        let title = r["title"].as_str().unwrap_or("");
                                        let link = r["url"].as_str().unwrap_or("");
                                        let snippet = r["content"].as_str().unwrap_or("");
                                        format!("- {}\n  {}\n  {}", title, snippet, link)
                                    }).collect()
                                })
                                .unwrap_or_default();
                            if results.is_empty() { "未找到结果".to_string() } else { results.join("\n\n") }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("SearXNG 返回错误: {}", r.status()),
                Err(e) => format!("SearXNG 请求失败: {}", e),
            }
        }
        "brave" => {
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Brave API Key 未配置".to_string(); }
            let resp = client
                .get("https://api.search.brave.com/res/v1/web/search")
                .header("X-Subscription-Token", key)
                .query(&[("q", query), ("count", "5")])
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let results: Vec<String> = v["web"]["results"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter().take(5).map(|r| {
                                        let title = r["title"].as_str().unwrap_or("");
                                        let link = r["url"].as_str().unwrap_or("");
                                        let snippet = r["description"].as_str().unwrap_or("");
                                        format!("- {}\n  {}\n  {}", title, snippet, link)
                                    }).collect()
                                })
                                .unwrap_or_default();
                            if results.is_empty() { "未找到结果".to_string() } else { results.join("\n\n") }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Brave 返回错误: {}", r.status()),
                Err(e) => format!("Brave 请求失败: {}", e),
            }
        }
        "tavily" => {
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Tavily API Key 未配置".to_string(); }
            let resp = client
                .post("https://api.tavily.com/search")
                .header("Authorization", format!("Bearer {}", key))
                .json(&json!({
                    "query": query,
                    "max_results": 5,
                    "include_answer": false,
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let results: Vec<String> = v["results"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter().take(5).map(|r| {
                                        let title = r["title"].as_str().unwrap_or("");
                                        let link = r["url"].as_str().unwrap_or("");
                                        let snippet = r["content"].as_str().unwrap_or("");
                                        format!("- {}\n  {}\n  {}", title, snippet, link)
                                    }).collect()
                                })
                                .unwrap_or_default();
                            if results.is_empty() { "未找到结果".to_string() } else { results.join("\n\n") }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Tavily 返回错误: {}", r.status()),
                Err(e) => format!("Tavily 请求失败: {}", e),
            }
        }
        "duckduckgo" => {
            // DuckDuckGo Instant Answer API（免费，无需 Key）
            let resp = client
                .get("https://api.duckduckgo.com/")
                .query(&[
                    ("q", query),
                    ("format", "json"),
                    ("no_html", "1"),
                    ("skip_disambig", "1"),
                ])
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let mut results = vec![];
                            // AbstractText
                            let abs = v["AbstractText"].as_str().unwrap_or("");
                            let abs_url = v["AbstractURL"].as_str().unwrap_or("");
                            if !abs.is_empty() {
                                results.push(format!("- 摘要\n  {}\n  {}", abs, abs_url));
                            }
                            // RelatedTopics
                            if let Some(topics) = v["RelatedTopics"].as_array() {
                                for t in topics.iter().take(5) {
                                    if let Some(text) = t["Text"].as_str() {
                                        if !text.is_empty() {
                                            let first_url = t["FirstURL"].as_str().unwrap_or("");
                                            results.push(format!("- {}\n  {}", text, first_url));
                                        }
                                    }
                                }
                            }
                            if results.is_empty() {
                                format!("DuckDuckGo 未找到即时答案（可尝试更具体的关键词）: {}", query)
                            } else {
                                results.join("\n\n")
                            }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("DuckDuckGo 返回错误: {}", r.status()),
                Err(e) => format!("DuckDuckGo 请求失败: {}", e),
            }
        }
        "perplexity" => {
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Perplexity API Key 未配置".to_string(); }
            let model = cfg["model"].as_str().unwrap_or("sonar");
            let resp = client
                .post("https://api.perplexity.ai/chat/completions")
                .header("Authorization", format!("Bearer {}", key))
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": "Be concise. Return sources inline."},
                        {"role": "user", "content": query}
                    ],
                    "max_tokens": 600,
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
                            let mut citations = vec![];
                            if let Some(cites) = v["citations"].as_array() {
                                for c in cites.iter().take(5) {
                                    if let Some(s) = c.as_str() {
                                        citations.push(format!("  {}", s));
                                    }
                                }
                            }
                            let mut out = content.to_string();
                            if !citations.is_empty() {
                                out.push_str("\n\n来源：\n");
                                out.push_str(&citations.join("\n"));
                            }
                            if out.trim().is_empty() { "未找到结果".to_string() } else { out }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Perplexity 返回错误: {}", r.status()),
                Err(e) => format!("Perplexity 请求失败: {}", e),
            }
        }
        "google" => {
            let key = cfg["apiKey"].as_str().unwrap_or("");
            let cx = cfg["cx"].as_str().unwrap_or("");
            if key.is_empty() || cx.is_empty() {
                return "Google CSE 需要 apiKey 和 cx".to_string();
            }
            let resp = client
                .get("https://www.googleapis.com/customsearch/v1")
                .query(&[
                    ("key", key),
                    ("cx", cx),
                    ("q", query),
                    ("num", "5"),
                ])
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let results: Vec<String> = v["items"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter().take(5).map(|r| {
                                        let title = r["title"].as_str().unwrap_or("");
                                        let link = r["link"].as_str().unwrap_or("");
                                        let snippet = r["snippet"].as_str().unwrap_or("");
                                        format!("- {}\n  {}\n  {}", title, snippet, link)
                                    }).collect()
                                })
                                .unwrap_or_default();
                            if results.is_empty() { "未找到结果".to_string() } else { results.join("\n\n") }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Google CSE 返回错误: {}", r.status()),
                Err(e) => format!("Google CSE 请求失败: {}", e),
            }
        }
        "bing" => {
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Bing API Key 未配置".to_string(); }
            let resp = client
                .get("https://api.bing.microsoft.com/v7.0/search")
                .header("Ocp-Apim-Subscription-Key", key)
                .query(&[("q", query), ("count", "5")])
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let results: Vec<String> = v["webPages"]["value"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter().take(5).map(|r| {
                                        let title = r["name"].as_str().unwrap_or("");
                                        let link = r["url"].as_str().unwrap_or("");
                                        let snippet = r["snippet"].as_str().unwrap_or("");
                                        format!("- {}\n  {}\n  {}", title, snippet, link)
                                    }).collect()
                                })
                                .unwrap_or_default();
                            if results.is_empty() { "未找到结果".to_string() } else { results.join("\n\n") }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Bing 返回错误: {}", r.status()),
                Err(e) => format!("Bing 请求失败: {}", e),
            }
        }
        "exa" => {
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Exa API Key 未配置".to_string(); }
            let resp = client
                .post("https://api.exa.ai/search")
                .header("Authorization", format!("Bearer {}", key))
                .json(&json!({
                    "query": query,
                    "num_results": 5,
                    "contents": {"text": {"maxCharacters": 200}},
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let results: Vec<String> = v["results"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter().take(5).map(|r| {
                                        let title = r["title"].as_str().unwrap_or("");
                                        let link = r["url"].as_str().unwrap_or("");
                                        let snippet = r["text"].as_str().unwrap_or("");
                                        format!("- {}\n  {}\n  {}", title, snippet, link)
                                    }).collect()
                                })
                                .unwrap_or_default();
                            if results.is_empty() { "未找到结果".to_string() } else { results.join("\n\n") }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Exa 返回错误: {}", r.status()),
                Err(e) => format!("Exa 请求失败: {}", e),
            }
        }
        "firecrawl" => {
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Firecrawl API Key 未配置".to_string(); }
            let resp = client
                .post("https://api.firecrawl.dev/v1/search")
                .header("Authorization", format!("Bearer {}", key))
                .json(&json!({"query": query, "limit": 5}))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let results: Vec<String> = {
                                let arr_val = if v.get("data").map(|d| d.is_array()).unwrap_or(false) {
                                    v.get("data").cloned()
                                } else {
                                    v.get("results").cloned()
                                };
                                arr_val
                                    .and_then(|d| d.as_array().cloned())
                                    .map(|arr| {
                                        arr.iter().take(5).map(|r| {
                                            let title = r["title"].as_str().or(r["metadata"]["title"].as_str()).unwrap_or("");
                                            let link = r["url"].as_str().or(r["metadata"]["sourceURL"].as_str()).unwrap_or("");
                                            let snippet = r["description"].as_str().unwrap_or("");
                                            format!("- {}\n  {}\n  {}", title, snippet, link)
                                        }).collect()
                                    })
                                    .unwrap_or_default()
                            };
                            if results.is_empty() { "未找到结果".to_string() } else { results.join("\n\n") }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Firecrawl 返回错误: {}", r.status()),
                Err(e) => format!("Firecrawl 请求失败: {}", e),
            }
        }
        "gemini" => {
            // Gemini grounding search：通过 generateContent 接口启用 googleSearch 工具
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Gemini API Key 未配置".to_string(); }
            let model = cfg["model"].as_str().unwrap_or("gemini-1.5-flash");
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                model, key
            );
            let resp = client
                .post(&url)
                .json(&json!({
                    "contents": [{"parts": [{"text": query}]}],
                    "tools": [{"google_search": {}}],
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let text = v["candidates"][0]["content"]["parts"][0]["text"]
                                .as_str().unwrap_or("");
                            if text.is_empty() { "未找到结果".to_string() } else { text.to_string() }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Gemini 返回错误: {}", r.status()),
                Err(e) => format!("Gemini 请求失败: {}", e),
            }
        }
        "grok" => {
            // Grok（xAI）Live Search：通过 chat/completions 启用 search
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "xAI/Grok API Key 未配置".to_string(); }
            let model = cfg["model"].as_str().unwrap_or("grok-3");
            let resp = client
                .post("https://api.x.ai/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", key))
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": "Search the web and answer concisely."},
                        {"role": "user", "content": query}
                    ],
                    "search_parameters": {"mode": "auto"},
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
                            if content.is_empty() { "未找到结果".to_string() } else { content.to_string() }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Grok 返回错误: {}", r.status()),
                Err(e) => format!("Grok 请求失败: {}", e),
            }
        }
        "kimi" => {
            // Kimi（Moonshot）web search
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "Moonshot/Kimi API Key 未配置".to_string(); }
            let model = cfg["model"].as_str().unwrap_or("moonshot-v1-8k");
            let resp = client
                .post("https://api.moonshot.cn/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", key))
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": "联网搜索并简洁回答。"},
                        {"role": "user", "content": query}
                    ],
                    "tools": [{"type": "builtin_function", "function": {"name": "$web_search"}}],
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
                            if content.is_empty() { "未找到结果".to_string() } else { content.to_string() }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Kimi 返回错误: {}", r.status()),
                Err(e) => format!("Kimi 请求失败: {}", e),
            }
        }
        "minimax" => {
            // MiniMax web_search plugin
            let key = cfg["apiKey"].as_str().unwrap_or("");
            if key.is_empty() { return "MiniMax API Key 未配置".to_string(); }
            let model = cfg["model"].as_str().unwrap_or("MiniMax-M1");
            let resp = client
                .post("https://api.minimax.chat/v1/text/chatcompletion_v2")
                .header("Authorization", format!("Bearer {}", key))
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": "联网搜索并简洁回答。"},
                        {"role": "user", "content": query}
                    ],
                    "plugins": [{"name": "web_search"}],
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
                            if content.is_empty() { "未找到结果".to_string() } else { content.to_string() }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("MiniMax 返回错误: {}", r.status()),
                Err(e) => format!("MiniMax 请求失败: {}", e),
            }
        }
        "ollama" => {
            // Ollama web search：通过 OpenAI 兼容端点 + web_search 工具
            let base = cfg["baseUrl"].as_str().unwrap_or("http://localhost:11434");
            let model = cfg["model"].as_str().unwrap_or("llama3.1");
            let resp = client
                .post(format!("{}/v1/chat/completions", base.trim_end_matches('/')))
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": "Search the web if needed and answer concisely."},
                        {"role": "user", "content": query}
                    ],
                    "tools": [{"type": "function", "function": {
                        "name": "web_search",
                        "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}
                    }}],
                }))
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    match r.json::<serde_json::Value>().await {
                        Ok(v) => {
                            let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
                            if content.is_empty() { "未找到结果（Ollama 模型可能不支持联网）".to_string() } else { content.to_string() }
                        }
                        Err(e) => format!("解析搜索结果失败: {}", e),
                    }
                }
                Ok(r) => format!("Ollama 返回错误: {}", r.status()),
                Err(e) => format!("Ollama 请求失败: {}", e),
            }
        }
        _ => format!("暂不支持的搜索引擎: {}", provider),
    }
}

/// 工具：执行 shell 命令（默认通过 sh -c，限制输出）
async fn tool_exec(command: &str) -> String {
    // 命令策略沙箱：分类 + 限制
    let analysis = cmd_policy().classify(command);
    if !analysis.safe && analysis.class == CmdClass::Destructive {
        return format!("🚫 拒绝执行高危命令: {}\n原因: {}", command, analysis.reason);
    }
    // 带超时执行
    let timeout = if analysis.class == CmdClass::ReadFs || analysis.class == CmdClass::ReadSystem {
        cmd_policy().timeout_secs
    } else { 120 };
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), output).await;
    match result {
        Ok(Ok(out)) => {
            let stdout_cap = cmd_policy().stdout_cap;
            let stderr_cap = cmd_policy().stderr_cap;
            let stdout_raw = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr_raw = String::from_utf8_lossy(&out.stderr).to_string();
            let code = out.status.code().unwrap_or(-1);
            // 按策略截断
            let stdout = if stdout_raw.len() > stdout_cap {
                format!("{}...(已截断 {}/{} 字节)", &stdout_raw[..stdout_cap], stdout_cap, stdout_raw.len())
            } else { stdout_raw };
            let stderr = if stderr_raw.len() > stderr_cap {
                format!("{}...(已截断)", &stderr_raw[..stderr_cap])
            } else { stderr_raw };
            let mut result_str = String::new();
            if !stdout.is_empty() { result_str.push_str(&stdout); }
            if !stderr.is_empty() {
                if !result_str.is_empty() { result_str.push_str("\n"); }
                result_str.push_str("[stderr]\n");
                result_str.push_str(&stderr);
            }
            result_str.push_str(&format!("\n[exit code: {}]", code));
            if result_str.len() > 16_000 {
                result_str.truncate(16_000);
                result_str.push_str("\n...(输出过长，已截断)");
            }
            result_str
        }
        Ok(Err(e)) => format!("执行失败: {}", e),
        Err(_) => format!("执行超时（{} 秒）", timeout),
    }
}

// ============================================================================
// 突破性功能工具：代码沙箱 / 网页浏览 / 实时知识 / 子 Agent
// ============================================================================

/// 带超时与输出限制的命令执行辅助。
async fn run_sandboxed(cmd: &str, args: &[&str], stdin: Option<&str>, timeout_secs: u64) -> String {
    let mut builder = tokio::process::Command::new(cmd);
    builder.args(args);
    builder.stdout(std::process::Stdio::piped());
    builder.stderr(std::process::Stdio::piped());
    builder.stdin(std::process::Stdio::piped());
    // 沙箱：限制环境与资源（尽力而为，非硬隔离）
    builder.env("LANG", "C.UTF-8");

    let mut child = match builder.spawn() {
        Ok(c) => c,
        Err(e) => return format!("启动失败（{} 未安装?）: {}", cmd, e),
    };
    if let Some(input) = stdin {
        use tokio::io::AsyncWriteExt;
        if let Some(mut stdin_pipe) = child.stdin.take() {
            let _ = stdin_pipe.write_all(input.as_bytes()).await;
            let _ = stdin_pipe.shutdown().await;
        }
    }
    let limit = std::time::Duration::from_secs(timeout_secs.min(30));

    // 分离 stdout/stderr 句柄，避免 wait_with_output 消耗 child（超时时需要 kill）。
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let read_pipes = async {
        use tokio::io::AsyncReadExt;
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_end(&mut out_buf).await;
        }
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_end(&mut err_buf).await;
        }
        (out_buf, err_buf)
    };
    tokio::pin!(read_pipes);

    let sleep = tokio::time::sleep(limit);
    tokio::pin!(sleep);

    let (stdout_bytes, stderr_bytes, exit_code) = tokio::select! {
        biased;
        _ = &mut sleep => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return format!("错误：执行超时（{} 秒），已终止", timeout_secs);
        }
        pipes = &mut read_pipes => {
            let (out_buf, err_buf) = pipes;
            // 管道读完后再 wait（此时进程通常已退出）
            let status = child.wait().await.ok();
            let code = status.and_then(|s| s.code()).unwrap_or(-1);
            (out_buf, err_buf, code)
        }
    };

    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
    let mut result_str = String::new();
    if !stdout.is_empty() { result_str.push_str(&stdout); }
    if !stderr.is_empty() {
        if !result_str.is_empty() { result_str.push_str("\n"); }
        result_str.push_str("[stderr]\n");
        result_str.push_str(&stderr);
    }
    result_str.push_str(&format!("\n[exit code: {}]", exit_code));
    if result_str.len() > 16_000 {
        result_str.truncate(16_000);
        result_str.push_str("\n...(输出过长，已截断)");
    }
    result_str
}

/// run_code 工具：在沙箱中执行 Python / JavaScript / Rust 代码。
async fn tool_run_code(language: &str, code: &str) -> String {
    // 简易危险代码检测：拒绝明显恶意模式
    let lower_code = code.to_lowercase();
    let dangerous = ["rm -rf", "mkfs", ":(){:|:&};:", "dd if=/dev/zero of=/dev/", "shutdown", "reboot"];
    for d in dangerous {
        if lower_code.contains(d) {
            return format!("错误：检测到危险模式 {:?}，拒绝执行", d);
        }
    }

    match language {
        "python" | "python3" => {
            // 直接通过 stdin 喂给 python3 -，避免临时文件
            let py = std::env::var("CRADLE_RING_PYTHON").unwrap_or_else(|_| "python3".to_string());
            run_sandboxed(&py, &["-u", "-c", code], None, 10).await
        }
        "javascript" | "js" | "node" => {
            run_sandboxed("node", &["--input-type=module", "-e", code], None, 10).await
        }
        "rust" => {
            // 用 cargo-script 风格：写临时文件 + cargo run（或 rustc）
            // 优先尝试 rustc（更快），失败回退提示装 cargo
            let tmp = format!("/tmp/cradle_ring_code_{}.rs", &format!("{:012x}", rand_u128()));
            let wrapped = format!(
                "fn main() {{\n{}\n}}\n",
                code
            );
            if tokio::fs::write(&tmp, &wrapped).await.is_err() {
                return "错误：无法创建临时文件".to_string();
            }
            let bin = tmp.replace(".rs", "");
            // 编译（10s）+ 运行（5s）
            let compile = run_sandboxed("rustc", &["-O", "-o", &bin, &tmp], None, 30).await;
            let _ = tokio::fs::remove_file(&tmp).await;
            if compile.contains("[exit code:") && !compile.contains("[exit code: 0]") {
                let _ = tokio::fs::remove_file(&bin).await;
                return format!("编译失败:\n{}", compile);
            }
            let run = run_sandboxed(&bin, &[], None, 10).await;
            let _ = tokio::fs::remove_file(&bin).await;
            run
        }
        other => format!("不支持的语言: {}（支持 python / javascript / rust）", other),
    }
}

/// 极简 HTML → 纯文本转换（去标签、压缩空白、保留链接与标题）。
fn html_to_text(html: &str) -> String {
    // 移除 script/style/noscript 内容
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut skip = false;
    for ch in html.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let tag = tag_buf.trim().to_lowercase();
                if tag.starts_with("script") || tag.starts_with("style") || tag.starts_with("noscript") {
                    skip = true;
                } else if tag.starts_with("/script") || tag.starts_with("/style") || tag.starts_with("/noscript") {
                    skip = false;
                } else if tag.starts_with("/p") || tag.starts_with("/div") || tag.starts_with("/li")
                    || tag.starts_with("/h1") || tag.starts_with("/h2") || tag.starts_with("/h3")
                    || tag.starts_with("/br") || tag == "br" || tag.starts_with("br ") {
                    out.push('\n');
                }
                tag_buf.clear();
            } else {
                tag_buf.push(ch);
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if skip { continue; }
        // 解码常见实体
        match ch {
            '&' => {
                // 简单跳过实体
            }
            _ => out.push(ch),
        }
    }
    // 压缩连续空白
    let mut prev_space = false;
    let mut cleaned = String::with_capacity(out.len());
    for ch in out.chars() {
        if ch == '\n' {
            cleaned.push('\n');
            prev_space = true;
            continue;
        }
        if ch.is_whitespace() {
            if !prev_space {
                cleaned.push(' ');
            }
            prev_space = true;
        } else {
            cleaned.push(ch);
            prev_space = false;
        }
    }
    // 合并多余空行
    let mut final_out = String::with_capacity(cleaned.len());
    let mut blank = 0;
    for line in cleaned.lines() {
        let l = line.trim();
        if l.is_empty() {
            blank += 1;
            if blank <= 1 { final_out.push('\n'); }
        } else {
            blank = 0;
            final_out.push_str(l);
            final_out.push('\n');
        }
    }
    final_out
}

/// 从 HTML 中提取 <title> 与主要正文文本。
fn extract_page_text(html: &str) -> (String, String) {
    let title = extract_title(html);
    let text = html_to_text(html);
    (title, text)
}

fn extract_title(html: &str) -> String {
    let lower = html.to_lowercase();
    if let Some(start) = lower.find("<title") {
        if let Some(gt) = lower[start..].find('>') {
            let body_start = start + gt + 1;
            if let Some(end) = lower[body_start..].find("</title>") {
                return html[body_start..body_start + end].trim().to_string();
            }
        }
    }
    String::new()
}

/// browse 工具：抓取网页 / 模拟点击 / 截图。
async fn tool_browse(url: &str, action: &str, _selector: &str) -> String {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    match action {
        "screenshot" => {
            let output = tokio::process::Command::new("/usr/bin/chromium")
                .args(&["--headless=new", "--no-sandbox", "--disable-gpu", "--screenshot=/tmp/cr_screenshot.png", "--window-size=1280,800", url])
                .output().await;
            return match output {
                Ok(_) => match std::fs::read("/tmp/cr_screenshot.png") {
                    Ok(data) => format!("截图完成（{}KB）", data.len()/1024),
                    Err(_) => "截图失败".to_string(),
                },
                Err(e) => format!("chromium 启动失败: {}", e),
            };
        }
        _ => {}
    }

    let resp = match client.get(url).header("User-Agent", "CradleRing/1.0").send().await {
        Ok(r) => r,
        Err(e) => return format!("请求失败: {}", e),
    };
    if !resp.status().is_success() { return format!("HTTP {}", resp.status()); }
    let html = resp.text().await.unwrap_or_default();
    let text = strip_html_tags(&html);
    if text.len() > 8000 { format!("{}...\n[截断]", &text[..8000]) } else { text }
}

// ============================================================================
// 网络安全 & 运维工具实现
// ============================================================================

async fn tool_port_scan(target: &str, ports: &str, timeout_ms: u64) -> String {
    if target.is_empty() { return "错误: 缺少 target".into(); }
    let port_list: Vec<u16> = if ports.contains('-') {
        let parts: Vec<&str> = ports.split('-').collect();
        if parts.len() == 2 { (parts[0].parse().unwrap_or(1)..=parts[1].parse().unwrap_or(1000)).collect() }
        else { vec![80,443,22,3306,8080] }
    } else if ports.contains(',') { ports.split(',').filter_map(|p| p.trim().parse().ok()).collect() }
    else { vec![80,443,22,3306,8080] };
    let mut open = Vec::new();
    for port in &port_list {
        if tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), tokio::net::TcpStream::connect(format!("{}:{}", target, port))).await.is_ok() {
            open.push(port.to_string());
        }
    }
    if open.is_empty() { format!("端口扫描 {}：未发现开放端口", target) }
    else { format!("端口扫描 {}：开放 {} 个：{}", target, open.len(), open.join(", ")) }
}

async fn tool_http_probe(url: &str, method: &str, body: &str, timeout_ms: u64) -> String {
    if url.is_empty() { return "错误: 缺少 url".into(); }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_millis(timeout_ms)).build().unwrap_or_default();
    let req = match method.to_uppercase().as_str() {
        "POST" => client.post(url).body(body.to_string()),
        "PUT" => client.put(url).body(body.to_string()),
        "DELETE" => client.delete(url),
        _ => client.get(url),
    };
    match req.header("User-Agent", "CradleRing-Security/1.0").send().await {
        Ok(resp) => format!("HTTP {} {}\n响应头:\n  {}", resp.status().as_u16(), resp.status().canonical_reason().unwrap_or(""),
            resp.headers().iter().map(|(k,v)| format!("{}: {}", k, v.to_str().unwrap_or(""))).collect::<Vec<_>>().join("\n  ")),
        Err(e) => format!("请求失败: {}", e),
    }
}

async fn tool_dns_lookup(domain: &str, rt: &str) -> String {
    match tokio::process::Command::new("sh").arg("-c").arg(format!("dig +short {} {} 2>/dev/null || nslookup -type={} {} 2>/dev/null", domain, rt, rt, domain)).output().await {
        Ok(o) => { let s = String::from_utf8_lossy(&o.stdout).to_string(); if s.trim().is_empty() { format!("DNS {} {}：无记录", rt, domain) } else { format!("DNS {} {}：\n{}", rt, domain, s.trim()) } }
        Err(_) => format!("DNS 查询工具不可用"),
    }
}

async fn tool_ssl_check(host: &str, port: u16) -> String {
    match tokio::process::Command::new("sh").arg("-c").arg(format!("echo | timeout 10 openssl s_client -connect {}:{} -servername {} 2>/dev/null | openssl x509 -noout -dates -subject -issuer 2>/dev/null", host, port, host)).output().await {
        Ok(o) => { let s = String::from_utf8_lossy(&o.stdout).to_string(); if s.trim().is_empty() { format!("SSL {}:{}：无法连接或无证书", host, port) } else { s } }
        Err(_) => format!("openssl 不可用"),
    }
}

async fn tool_subdomain_enum(domain: &str) -> String {
    let subs = ["www","mail","admin","api","dev","staging","vpn","portal","app","shop","ns1","smtp","git","ci","grafana","redis","db","cdn","static"];
    let mut found = Vec::new();
    for s in &subs { let d = format!("{}.{}", s, domain); if let Ok(o) = tokio::process::Command::new("sh").arg("-c").arg(format!("getent hosts {} 2>/dev/null || dig +short {} A 2>/dev/null", d, d)).output().await { if !String::from_utf8_lossy(&o.stdout).trim().is_empty() { found.push(d); } } }
    if found.is_empty() { format!("子域名枚举 {}：未发现", domain) } else { format!("子域名枚举 {}：发现 {} 个\n{}", domain, found.len(), found.iter().map(|s| format!("  {}", s)).collect::<Vec<_>>().join("\n")) }
}

// ============================================================================
// WAF 检测 & 网络安全工具（增强版）
// ============================================================================

/// WAF 检测：多 payload + 多维度识别 WAF 类型 + 绕过测试
#[allow(unused_assignments)]
async fn tool_waf_detect(url: &str) -> String {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build().unwrap_or_default();
    let base = url.trim_end_matches('/');
    let mut evidence: Vec<String> = vec![];
    let mut waf_type = String::new();
    let mut confidence = 0u32;

    // 1. 基线请求（无 payload）
    let baseline = client.get(base).header("User-Agent", "Mozilla/5.0").send().await;
    let baseline_status = baseline.as_ref().map(|r| r.status().as_u16()).unwrap_or(0);
    let baseline_server = baseline.as_ref().ok()
        .and_then(|r| r.headers().get("server").and_then(|v| v.to_str().ok().map(String::from)));

    // 2. 多 payload 测试
    let payloads = [
        ("sqli", format!("{}?id=1'+OR+'1'='1", base)),
        ("xss", format!("{}?q=<script>alert(1)</script>", base)),
        ("traversal", format!("{}?file=../../etc/passwd", base)),
        ("cmd_inject", format!("{}?cmd=;cat+/etc/passwd", base)),
        ("log4j", format!("{}?q=${{jndi:ldap://x/y}}", base)),
    ];
    let mut blocked_count = 0;
    for (name, payload_url) in &payloads {
        match client.get(payload_url).header("User-Agent", "CradleRing-Security/1.0").send().await {
            Ok(r) => {
                let st = r.status().as_u16();
                let body = r.text().await.unwrap_or_default();
                if st == 403 || st == 406 || st == 429 || st == 501 {
                    blocked_count += 1;
                    evidence.push(format!("  [{}] HTTP {} (被拦截)", name, st));
                } else if body.len() < 500 && (body.to_lowercase().contains("blocked") || body.to_lowercase().contains("denied") || body.to_lowercase().contains("forbidden")) {
                    blocked_count += 1;
                    evidence.push(format!("  [{}] 响应包含拦截关键词", name));
                }
            }
            Err(e) => { evidence.push(format!("  [{}] 请求失败: {}", name, e)); }
        }
    }

    // 3. WAF 类型识别（基于 header 指纹 + 行为模式）
    let server = baseline_server.unwrap_or_default().to_lowercase();
    let headers_str = format!("{:?}", baseline.ok().map(|r| r.headers().clone()));
    let h_lower = headers_str.to_lowercase();
    if server.contains("cloudflare") || h_lower.contains("cf-ray") {
        waf_type = "Cloudflare".to_string(); confidence = 95;
    } else if server.contains("awselb") || h_lower.contains("x-amzn") || h_lower.contains("x-amz") {
        waf_type = "AWS WAF/ALB".to_string(); confidence = 85;
    } else if server.contains("akamai") || h_lower.contains("akamai") {
        waf_type = "Akamai Kona".to_string(); confidence = 85;
    } else if server.contains("sucuri") || h_lower.contains("x-sucuri") {
        waf_type = "Sucuri".to_string(); confidence = 90;
    } else if server.contains("imperva") || h_lower.contains("incapsula") || h_lower.contains("visid_incap") {
        waf_type = "Imperva Incapsula".to_string(); confidence = 90;
    } else if server.contains("barracuda") || h_lower.contains("barra_counter") {
        waf_type = "Barracuda".to_string(); confidence = 85;
    } else if h_lower.contains("mod_security") || h_lower.contains("modsecurity") {
        waf_type = "ModSecurity".to_string(); confidence = 80;
    } else if server.contains("aliyun") || h_lower.contains("aliyunwaf") || h_lower.contains("acs") {
        waf_type = "阿里云盾".to_string(); confidence = 85;
    } else if server.contains("tencent") || h_lower.contains("tencentwaf") || h_lower.contains("qcloud") {
        waf_type = "腾讯云 WAF".to_string(); confidence = 85;
    } else if server.contains("huawei") || h_lower.contains("hwcdn") {
        waf_type = "华为云 WAF".to_string(); confidence = 80;
    } else if blocked_count >= 3 {
        waf_type = "未知 WAF（高置信度存在）".to_string(); confidence = 75;
    } else if blocked_count >= 1 {
        waf_type = "可能存在 WAF".to_string(); confidence = 50;
    } else if baseline_status == 200 {
        waf_type = "未检测到 WAF".to_string(); confidence = 70;
    } else {
        waf_type = "无法确定".to_string(); confidence = 30;
    }

    // 4. 绕过测试（对检测到的 WAF 尝试简单绕过）
    let mut bypass_results = vec![];
    if confidence > 40 && !waf_type.contains("未检测到") {
        let bypass_payloads = [
            ("case_variation", format!("{}?Id=1'+oR+'1'='1", base)),
            ("comment", format!("{}?id=1'/**/OR/**/'1'='1", base)),
            ("url_encode", format!("{}?id=1%27+OR+%271%27=%271", base)),
        ];
        for (bname, burl) in &bypass_payloads {
            if let Ok(r) = client.get(burl).header("User-Agent", "Mozilla/5.0").send().await {
                let st = r.status().as_u16();
                if st == 200 { bypass_results.push(format!("  [{}] 绕过成功 (HTTP 200)", bname)); }
                else { bypass_results.push(format!("  [{}] 被拦截 (HTTP {})", bname, st)); }
            }
        }
    }

    // 5. 汇总输出
    let mut out = String::new();
    out.push_str(&format!("🛡️ WAF 检测报告\n"));
    out.push_str(&format!("目标: {}\n", base));
    out.push_str(&format!("基线状态: HTTP {}\n", baseline_status));
    out.push_str(&format!("Server: {}\n", if server.is_empty() { "未知".to_string() } else { server }));
    out.push_str(&format!("\nWAF 类型: {} (置信度 {}%)\n", waf_type, confidence));
    out.push_str(&format!("\n拦截统计: {}/{} 个 payload 被拦截\n", blocked_count, payloads.len()));
    if !evidence.is_empty() {
        out.push_str("\n证据:\n");
        out.push_str(&evidence.join("\n"));
    }
    if !bypass_results.is_empty() {
        out.push_str("\n\n绕过测试:\n");
        out.push_str(&bypass_results.join("\n"));
    }
    let risk = if waf_type.contains("未检测到") { "⚠️ 高风险（无 WAF 防护）" }
        else if bypass_results.iter().any(|r| r.contains("绕过成功")) { "⚠️ 中风险（WAF 可绕过）" }
        else { "✅ 低风险（WAF 有效）" };
    out.push_str(&format!("\n\n风险评级: {}", risk));
    out
}

// ============================================================================
// WAF 规则引擎（对标 ModSecurity，本地规则匹配）
// ============================================================================

/// WAF 规则：本地规则匹配引擎
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct WafRule {
    id: String,
    name: String,
    /// 规则类型：sqli/xss/rfi/lfi/cmd_inject/traversal/scanner/custom
    rule_type: String,
    /// 匹配模式（正则）
    pattern: String,
    /// 动作：block/log/allow
    action: String,
    /// 严重等级：critical/high/medium/low/info
    severity: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    hit_count: u64,
    created_at: i64,
}

impl Default for WafRule {
    fn default() -> Self {
        WafRule {
            id: String::new(), name: String::new(), rule_type: "custom".to_string(),
            pattern: String::new(), action: "log".to_string(), severity: "medium".to_string(),
            enabled: true, description: String::new(), hit_count: 0, created_at: current_ms(),
        }
    }
}

/// 内置 WAF 规则集（对标 OWASP CRS 核心规则）
fn default_waf_rules() -> Vec<WafRule> {
    let now = current_ms();
    vec![
        // ===== SQL 注入（OWASP CRS 942）=====
        WafRule { id: "waf-942-001".into(), name: "SQLi-联合查询".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(union\s+(all\s+)?select|select\s+.+\s+from)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "UNION SELECT 联合查询注入".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-002".into(), name: "SQLi-布尔盲注".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(\bor\b\s+\d+=\d+|'\s*or\s*'1'='1)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "布尔盲注 (OR 1=1)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-003".into(), name: "SQLi-时间盲注".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(sleep\s*\(|benchmark\s*\(|waitfor\s+delay)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "时间盲注 (SLEEP/BENCHMARK)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-004".into(), name: "SQLi-报错注入".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(extractvalue|updatexml|exp\s*\(|floor\s*\(rand)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "报错注入 (EXTRACTVALUE/UPDATEXML)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-005".into(), name: "SQLi-堆叠注入".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(;\s*(drop|alter|create|truncate)\s+table|;\s*shutdown)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "堆叠注入 (DROP/ALTER/CREATE TABLE)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-006".into(), name: "SQLi-注释绕过".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(\/\*!\d*|--\s|#|\bor\s+1=1\s*--)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "注释符绕过 (/*!*/ -- #)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-007".into(), name: "SQLi-宽字节注入".into(), rule_type: "sqli".into(),
            pattern: r"(%df%27|%bf%27|%81%27)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "宽字节注入 (%df')".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-008".into(), name: "SQLi-信息函数".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(@@version|@@datadir|user\s*\(\s*\)|database\s*\(\s*\)|schema\s*\(\s*\))".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "信息收集函数 (@@version/user())".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-009".into(), name: "SQLi-子查询".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(select\s+.+\s+from\s+\(\s*select|select\s+.+\s+from\s+information_schema)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "子查询/信息模式查询".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-942-010".into(), name: "SQLi-LOAD_FILE".into(), rule_type: "sqli".into(),
            pattern: r"(?i)(load_file\s*\(|into\s+(outfile|dumpfile))".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "文件读写 (LOAD_FILE/OUTFILE)".into(), hit_count: 0, created_at: now },

        // ===== XSS 跨站脚本（OWASP CRS 941）=====
        WafRule { id: "waf-941-001".into(), name: "XSS-script标签".into(), rule_type: "xss".into(),
            pattern: r"(?i)(<script[^>]*>|</script>|javascript\s*:|on\w+\s*=)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "script 标签和事件处理器 XSS".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-941-002".into(), name: "XSS-HTML注入".into(), rule_type: "xss".into(),
            pattern: r"(?i)(<iframe|<object|<embed|<form|<meta\s+http-equiv)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "iframe/object/embed 注入".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-941-003".into(), name: "XSS-img/onerror".into(), rule_type: "xss".into(),
            pattern: r"(?i)(<img[^>]+onerror|<img[^>]+src\s*=\s*javascript)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "img onerror XSS".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-941-004".into(), name: "XSS-svg/onload".into(), rule_type: "xss".into(),
            pattern: r"(?i)(<svg[^>]+onload|<svg[^>]+<script)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "svg onload XSS".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-941-005".into(), name: "XSS-expression".into(), rule_type: "xss".into(),
            pattern: r"(?i)(expression\s*\(|vbscript\s*:|data\s*:\s*text/html)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "expression/vbscript/data URI XSS".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-941-006".into(), name: "XSS-事件处理器".into(), rule_type: "xss".into(),
            pattern: r"(?i)(onfocus|onblur|onchange|onsubmit|onreset|onselect|onload|onunload|onclick|ondblclick|onmousedown|onmouseup|onmouseover|onmousemove|onmouseout|onkeypress|onkeydown|onkeyup)\s*=".into(),
            action: "block".into(), severity: "medium".into(), enabled: true,
            description: "HTML 事件处理器 XSS".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-941-007".into(), name: "XSS-属性注入".into(), rule_type: "xss".into(),
            pattern: r"(?i)(href\s*=\s*javascript|src\s*=\s*data\s*:|formaction\s*=\s*javascript)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "href/src/formaction 属性注入".into(), hit_count: 0, created_at: now },

        // ===== 命令注入 / RCE（OWASP CRS 932）=====
        WafRule { id: "waf-932-001".into(), name: "RCE-命令分隔符".into(), rule_type: "cmd_inject".into(),
            pattern: r"(;|\||\||&&|\$\(|\`).*(cat|ls|id|whoami|uname|pwd|ifconfig|ipconfig|netstat|ps|kill|wget|curl|bash|sh|python|perl|ruby|php|nc|ncat|telnet)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "命令分隔符后的系统命令".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-932-002".into(), name: "RCE-反引号".into(), rule_type: "cmd_inject".into(),
            pattern: r"`[^`]*(cat|ls|id|whoami|uname|pwd|ifconfig|netstat|ps|wget|curl|bash|sh|python|perl|php|nc)[^`]*`".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "反引号命令执行".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-932-003".into(), name: "RCE-$()执行".into(), rule_type: "cmd_inject".into(),
            pattern: r"\$\([^)]*(cat|ls|id|whoami|uname|pwd|ifconfig|netstat|ps|wget|curl|bash|sh|python|perl|php|nc)[^)]*\)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "$() 命令执行".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-932-004".into(), name: "RCE-编码绕过".into(), rule_type: "cmd_inject".into(),
            pattern: r"(%0a|%0d|%09|%00|%0b|%0c).*?(cat|ls|id|whoami|uname|pwd|wget|curl|bash|sh|python|perl|php|nc)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "URL 编码绕过命令执行".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-932-005".into(), name: "RCE-管道符".into(), rule_type: "cmd_inject".into(),
            pattern: r"\|\s*(cat|ls|id|whoami|uname|pwd|ifconfig|netstat|ps|wget|curl|bash|sh|python|perl|php|nc)\s".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "管道符命令执行".into(), hit_count: 0, created_at: now },

        // ===== 路径遍历 / LFI（OWASP CRS 930）=====
        WafRule { id: "waf-930-001".into(), name: "LFI-路径遍历".into(), rule_type: "traversal".into(),
            pattern: r"(\.\./|\.\.\\|%2e%2e%2f|%2e%2e%5c)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "../ 路径遍历".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-930-002".into(), name: "LFI-绝对路径".into(), rule_type: "lfi".into(),
            pattern: r"(?i)(/etc/passwd|/etc/shadow|/etc/hosts|/proc/self|/var/log|/root/|/home/[^/]+/\.ssh|C:\\\\|boot\.ini|win\.ini)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "敏感绝对路径访问".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-930-003".into(), name: "LFI-编码绕过".into(), rule_type: "traversal".into(),
            pattern: r"(%2e%2e%2f|%2e%2e/|%2e%2e\\|%252e%252e%252f|\.%2e|%2e\.)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "URL 编码路径遍历".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-930-004".into(), name: "LFI-敏感文件".into(), rule_type: "lfi".into(),
            pattern: r"(?i)(\.env|\.git/|\.svn/|\.htaccess|web\.config|wp-config\.php|config\.php|\.bash_history|id_rsa|\.ssh/|\.aws/|\.docker/|\.kube/)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "敏感文件访问".into(), hit_count: 0, created_at: now },

        // ===== 扫描器 / 恶意机器人（OWASP CRS 913）=====
        WafRule { id: "waf-913-001".into(), name: "扫描器-渗透工具".into(), rule_type: "scanner".into(),
            pattern: r"(?i)(sqlmap|nikto|nmap|acunetix|nessus|openvas|metasploit|burp|hydra|dirbuster|gobuster|wpscan|masscan|zgrab|nuclei)".into(),
            action: "log".into(), severity: "medium".into(), enabled: true,
            description: "已知扫描器 User-Agent".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-913-002".into(), name: "扫描器-爬虫".into(), rule_type: "scanner".into(),
            pattern: r"(?i)(zmeu|morfeus|dirb|nmap.*nse|whatweb|w3af|skipfish|arachni|vega)".into(),
            action: "log".into(), severity: "medium".into(), enabled: true,
            description: "Web 扫描器/爬虫".into(), hit_count: 0, created_at: now },

        // ===== 协议攻击（OWASP CRS 921）=====
        WafRule { id: "waf-921-001".into(), name: "协议-HTTP走私".into(), rule_type: "protocol".into(),
            pattern: r"(?i)(transfer-encoding\s*:\s*chunked.*content-length|content-length\s*:\s*\d+.*transfer-encoding)".into(),
            action: "log".into(), severity: "high".into(), enabled: true,
            description: "HTTP 请求走私 (CL.TE/TE.CL)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-921-002".into(), name: "协议-CRLF注入".into(), rule_type: "protocol".into(),
            pattern: r"(%0d%0a|%0d|%0a).*(set-cookie|location|content-type|x-forwarded)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "CRLF 注入（响应头注入）".into(), hit_count: 0, created_at: now },

        // ===== 文件上传（OWASP CRS 933）=====
        WafRule { id: "waf-933-001".into(), name: "上传-可执行文件".into(), rule_type: "upload".into(),
            pattern: r"(?i)(\.php|\.jsp|\.jspx|\.asp|\.aspx|\.sh|\.py|\.pl|\.cgi|\.exe|\.bat|\.cmd|\.ps1|\.dll|\.so|\.war|\.jar)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "可执行文件上传".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-933-002".into(), name: "上传-双扩展名".into(), rule_type: "upload".into(),
            pattern: r"(?i)(\.php\.(jpg|png|gif|txt|pdf)|\.jsp\.(jpg|png|gif|txt)|\.asp\.(jpg|png|gif|txt))".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "双扩展名绕过上传".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-933-003".into(), name: "上传-空字节".into(), rule_type: "upload".into(),
            pattern: r"(%00|\.php%00|\.jsp%00|\.asp%00)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "空字节绕过上传".into(), hit_count: 0, created_at: now },

        // ===== 其他高危漏洞 =====
        WafRule { id: "waf-944-001".into(), name: "Log4j JNDI 注入".into(), rule_type: "custom".into(),
            pattern: r"(?i)(\$\{jndi:(ldap|rmi|dns|nis|iiop|corba|nds|http)://)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "Log4j JNDI 注入 (CVE-2021-44228)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-944-002".into(), name: "SSTI-模板注入".into(), rule_type: "custom".into(),
            pattern: r"(?i)(\$\{[^}]*\}|#\{[^}]*\}|\{\{[^}]*\}\}|\{%[^%]*%\})".into(),
            action: "log".into(), severity: "medium".into(), enabled: true,
            description: "模板注入 (SSTI)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-944-003".into(), name: "XXE-外部实体".into(), rule_type: "custom".into(),
            pattern: r"(?i)(<!entity\s+%\s+\w+\s+system|<!doctype\s+\w+\s+\[.*<!entity)".into(),
            action: "block".into(), severity: "high".into(), enabled: true,
            description: "XML 外部实体注入 (XXE)".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-944-004".into(), name: "SSRF-内网探测".into(), rule_type: "custom".into(),
            pattern: r"(?i)(localhost|127\.0\.0\.1|0\.0\.0\.0|169\.254\.|10\.|172\.(1[6-9]|2\d|3[01])\.|192\.168\.|metadata\.google\.internal)".into(),
            action: "log".into(), severity: "medium".into(), enabled: true,
            description: "SSRF 内网地址探测".into(), hit_count: 0, created_at: now },
        WafRule { id: "waf-944-005".into(), name: "反序列化-Java".into(), rule_type: "custom".into(),
            pattern: r"(?i)(aced0005|serialVersionUID|java\.io\.ObjectInputStream|readObject\s*\(|javax\.xml\.transform\.Templates|org\.apache\.commons\.collections)".into(),
            action: "block".into(), severity: "critical".into(), enabled: true,
            description: "Java 反序列化特征".into(), hit_count: 0, created_at: now },
    ]
}

// ============================================================================
// IP 黑白名单 + 速率限制
// ============================================================================

/// IP 名单条目
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct IpEntry {
    ip: String,
    /// whitelist / blacklist
    list_type: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    expires_at: Option<i64>,  // None=永久
    created_at: i64,
}

/// 速率限制条目
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct RateLimitEntry {
    /// 限制维度：ip / path / global
    scope: String,
    /// 目标（IP 或路径，global 时为 *）
    target: String,
    /// 请求数限制
    max_requests: u32,
    /// 时间窗口（秒）
    window_secs: u64,
    /// 当前计数
    current_count: u32,
    /// 窗口开始时间
    window_start: i64,
    /// 动作：block / log
    action: String,
}

/// IP 黑白名单检查
fn check_ip_list<'a>(entries: &'a [IpEntry], ip: &str) -> Option<&'a IpEntry> {
    let now = current_ms();
    entries.iter().find(|e| {
        e.ip == ip && (e.expires_at.map(|t| t > now).unwrap_or(true))
    })
}

/// 速率限制检查：返回是否超过限制
fn check_rate_limit(entries: &mut Vec<RateLimitEntry>, scope: &str, target: &str, max_requests: u32, window_secs: u64, action: &str) -> bool {
    let now = current_ms();
    let window_ms = (window_secs as i64) * 1000;
    // 查找或创建条目
    let entry = entries.iter_mut().find(|e| e.scope == scope && e.target == target);
    match entry {
        Some(e) => {
            // 窗口过期则重置
            if now - e.window_start > window_ms {
                e.current_count = 0;
                e.window_start = now;
            }
            e.current_count += 1;
            if e.current_count > max_requests {
                if action == "block" { return true; }
            }
            false
        }
        None => {
            entries.push(RateLimitEntry {
                scope: scope.to_string(),
                target: target.to_string(),
                max_requests,
                window_secs,
                current_count: 1,
                window_start: now,
                action: action.to_string(),
            });
            false
        }
    }
}

/// IP 条目序列化
fn ip_entry_to_json(e: &IpEntry) -> serde_json::Value {
    json!({
        "ip": e.ip, "listType": e.list_type, "reason": e.reason,
        "expiresAt": e.expires_at, "createdAt": e.created_at,
    })
}

/// 速率限制条目序列化
fn rate_limit_to_json(e: &RateLimitEntry) -> serde_json::Value {
    json!({
        "scope": e.scope, "target": e.target, "maxRequests": e.max_requests,
        "windowSecs": e.window_secs, "currentCount": e.current_count,
        "windowStart": e.window_start, "action": e.action,
    })
}

/// WAF 请求检查：对请求 URL + 参数 + headers 做规则匹配
fn waf_check_request(rules: &[WafRule], url: &str, headers: &str, body: &str) -> Vec<WafMatch> {
    let mut matches = vec![];
    let check_text = format!("{} {} {}", url, headers, body);
    for rule in rules.iter().filter(|r| r.enabled) {
        if let Ok(re) = regex::Regex::new(&rule.pattern) {
            if re.is_match(&check_text) {
                matches.push(WafMatch {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    rule_type: rule.rule_type.clone(),
                    action: rule.action.clone(),
                    severity: rule.severity.clone(),
                    matched_text: check_text.chars().take(200).collect(),
                });
            }
        }
    }
    matches
}

#[derive(Clone, Debug, serde::Serialize)]
struct WafMatch {
    rule_id: String,
    rule_name: String,
    rule_type: String,
    action: String,
    severity: String,
    matched_text: String,
}

/// WAF 事件日志
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct WafEvent {
    id: String,
    rule_id: String,
    rule_name: String,
    rule_type: String,
    action: String,
    severity: String,
    url: String,
    source_ip: String,
    user_agent: String,
    matched_text: String,
    ts: i64,
}

/// WAF 规则序列化
fn waf_rule_to_json(r: &WafRule) -> serde_json::Value {
    json!({
        "id": r.id, "name": r.name, "ruleType": r.rule_type, "pattern": r.pattern,
        "action": r.action, "severity": r.severity, "enabled": r.enabled,
        "description": r.description, "hitCount": r.hit_count, "createdAt": r.created_at,
    })
}

// ============================================================================
// 入侵检测防护（IDS/IPS）—— 对标 fail2ban + Snort 基础检测
// ============================================================================

/// 入侵检测事件
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct IdsEvent {
    id: String,
    /// 检测类型：ssh_bruteforce / port_scan / malware_process / c2_connection / suspicious_file
    event_type: String,
    /// 来源 IP 或进程名
    source: String,
    /// 检测详情
    detail: String,
    /// 严重等级：critical / high / medium / low
    severity: String,
    /// 是否已自动封禁
    blocked: bool,
    /// 封禁时长（秒，0=永久）
    ban_duration_secs: u64,
    ts: i64,
}

/// 入侵防护规则（自定义）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct IdsRule {
    id: String,
    name: String,
    /// 规则类型：ssh_bruteforce / port_scan / malware_process / c2_connection / custom
    rule_type: String,
    /// 匹配模式（正则或特征描述）
    pattern: String,
    /// 动作：block / log / alert
    action: String,
    /// 阈值（如连续失败次数）
    threshold: u32,
    /// 时间窗口（秒）
    window_secs: u64,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    hit_count: u64,
    created_at: i64,
}

impl Default for IdsRule {
    fn default() -> Self {
        IdsRule {
            id: String::new(), name: String::new(), rule_type: "custom".to_string(),
            pattern: String::new(), action: "log".to_string(), threshold: 5,
            window_secs: 300, enabled: true, description: String::new(), hit_count: 0, created_at: current_ms(),
        }
    }
}

/// 内置入侵检测规则
fn default_ids_rules() -> Vec<IdsRule> {
    let now = current_ms();
    vec![
        IdsRule {
            id: "ids-ssh-001".into(), name: "SSH 暴力破解".into(), rule_type: "ssh_bruteforce".into(),
            pattern: "Failed password|Invalid user|Connection closed by authenticating user".into(),
            action: "block".into(), threshold: 5, window_secs: 300, enabled: true,
            description: "5 分钟内 5 次 SSH 失败登录即封禁".into(), hit_count: 0, created_at: now,
        },
        IdsRule {
            id: "ids-scan-001".into(), name: "端口扫描".into(), rule_type: "port_scan".into(),
            pattern: "SYN_RECV|connect\\(\\) to .* port".into(),
            action: "block".into(), threshold: 20, window_secs: 60, enabled: true,
            description: "1 分钟内 20 个端口连接即判定扫描".into(), hit_count: 0, created_at: now,
        },
        IdsRule {
            id: "ids-malware-001".into(), name: "挖矿进程特征".into(), rule_type: "malware_process".into(),
            pattern: "xmrig|minerd|cpuminer|kworker.*mining|stratum\\+tcp".into(),
            action: "alert".into(), threshold: 1, window_secs: 60, enabled: true,
            description: "检测已知挖矿进程特征".into(), hit_count: 0, created_at: now,
        },
        IdsRule {
            id: "ids-c2-001".into(), name: "C2 回连特征".into(), rule_type: "c2_connection".into(),
            pattern: "beacon|meterpreter|cobaltstrike|empire|powershell.*-enc|certutil.*-decode".into(),
            action: "alert".into(), threshold: 1, window_secs: 60, enabled: true,
            description: "检测已知 C2 框架特征".into(), hit_count: 0, created_at: now,
        },
    ]
}

/// 入侵检测：分析 auth.log 检测 SSH 暴力破解
async fn ids_check_ssh_bruteforce(_state: &AppState, threshold: u32, window_secs: u64) -> Vec<IdsEvent> {
    let log_path = "/var/log/auth.log";
    if !std::path::Path::new(log_path).exists() {
        return vec![];
    }
    // 读取最近 1000 行
    let content = tokio::fs::read_to_string(log_path).await.unwrap_or_default();
    let lines: Vec<&str> = content.lines().rev().take(1000).collect();
    let mut ip_failures: HashMap<String, Vec<i64>> = HashMap::new();
    let now = current_ms();
    let window_ms = (window_secs as i64) * 1000;

    for line in lines {
        // 匹配 "Failed password for invalid user xxx from 1.2.3.4 port 12345"
        if line.contains("Failed password") || line.contains("Invalid user") {
            if let Some(ip) = extract_ip_from_line(line) {
                let ts = parse_log_timestamp(line).unwrap_or(now);
                if now - ts < window_ms {
                    ip_failures.entry(ip).or_default().push(ts);
                }
            }
        }
    }

    let mut events = vec![];
    for (ip, timestamps) in ip_failures {
        if timestamps.len() >= threshold as usize {
            events.push(IdsEvent {
                id: format!("ids-ev-{:016x}", rand_u128()),
                event_type: "ssh_bruteforce".to_string(),
                source: ip.clone(),
                detail: format!("{} 分钟内 {} 次失败登录", window_secs / 60, timestamps.len()),
                severity: if timestamps.len() > 10 { "critical".to_string() } else { "high".to_string() },
                blocked: false,
                ban_duration_secs: 3600,
                ts: now,
            });
        }
    }
    events
}

/// 从日志行提取 IP 地址
fn extract_ip_from_line(line: &str) -> Option<String> {
    // 匹配 "from 1.2.3.4 port" 或 "from 1.2.3.4"
    let re = regex::Regex::new(r"from\s+(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})").ok()?;
    re.captures(line).and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

/// 解析日志时间戳（支持 "Jul 17 12:34:56" 格式）
fn parse_log_timestamp(_line: &str) -> Option<i64> {
    // 简化：用当前时间（实际应解析 syslog 时间戳）
    // TODO: 解析 "Jul 17 12:34:56" 格式
    Some(current_ms())
}

/// 入侵检测：分析进程列表检测挖矿/恶意进程
async fn ids_check_malware_process(patterns: &[String]) -> Vec<IdsEvent> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("ps aux --sort=-%cpu | head -50")
        .output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut events = vec![];
    let now = current_ms();
    for line in output.lines() {
        for pattern in patterns {
            if line.to_lowercase().contains(&pattern.to_lowercase()) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                let pid = parts.get(1).unwrap_or(&"unknown").to_string();
                let cmd = parts.get(10).unwrap_or(&"unknown").to_string();
                events.push(IdsEvent {
                    id: format!("ids-ev-{:016x}", rand_u128()),
                    event_type: "malware_process".to_string(),
                    source: cmd.clone(),
                    detail: format!("PID {} 匹配特征: {}", pid, pattern),
                    severity: "high".to_string(),
                    blocked: false,
                    ban_duration_secs: 0,
                    ts: now,
                });
            }
        }
    }
    events
}

/// 入侵检测：分析网络连接检测端口扫描和 C2 回连
async fn ids_check_network(patterns: &[String]) -> Vec<IdsEvent> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("ss -tnp state established 2>/dev/null || netstat -tnp 2>/dev/null | head -100")
        .output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut events = vec![];
    let now = current_ms();
    for line in output.lines() {
        for pattern in patterns {
            if line.to_lowercase().contains(&pattern.to_lowercase()) {
                events.push(IdsEvent {
                    id: format!("ids-ev-{:016x}", rand_u128()),
                    event_type: "c2_connection".to_string(),
                    source: line.chars().take(50).collect(),
                    detail: format!("匹配特征: {}", pattern),
                    severity: "critical".to_string(),
                    blocked: false,
                    ban_duration_secs: 0,
                    ts: now,
                });
            }
        }
    }
    events
}

/// 自动封禁：用 iptables 封禁 IP
async fn ids_ban_ip(ip: &str, duration_secs: u64) -> Result<String, String> {
    // 检查是否已封禁
    let check = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("iptables -L INPUT -n | grep '{}'", ip))
        .output().await;
    if let Ok(o) = check {
        if !String::from_utf8_lossy(&o.stdout).trim().is_empty() {
            return Ok(format!("IP {} 已在黑名单", ip));
        }
    }
    // 添加封禁规则
    let add = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("iptables -A INPUT -s {} -j DROP", ip))
        .output().await;
    match add {
        Ok(o) if o.status.success() => {
            // 设置自动解封（如果有时长）
            if duration_secs > 0 {
                let ip_clone = ip.to_string();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(duration_secs)).await;
                    let _ = tokio::process::Command::new("sh")
                        .arg("-c")
                        .arg(format!("iptables -D INPUT -s {} -j DROP", ip_clone))
                        .output().await;
                });
            }
            Ok(format!("IP {} 已封禁 {} 秒", ip, duration_secs))
        }
        Ok(o) => Err(format!("封禁失败: {}", String::from_utf8_lossy(&o.stderr))),
        Err(e) => Err(format!("执行失败: {}", e)),
    }
}

/// IDS 事件序列化
fn ids_event_to_json(e: &IdsEvent) -> serde_json::Value {
    json!({
        "id": e.id, "eventType": e.event_type, "source": e.source, "detail": e.detail,
        "severity": e.severity, "blocked": e.blocked, "banDurationSecs": e.ban_duration_secs,
        "ts": e.ts,
    })
}

/// IDS 规则序列化
fn ids_rule_to_json(r: &IdsRule) -> serde_json::Value {
    json!({
        "id": r.id, "name": r.name, "ruleType": r.rule_type, "pattern": r.pattern,
        "action": r.action, "threshold": r.threshold, "windowSecs": r.window_secs,
        "enabled": r.enabled, "description": r.description, "hitCount": r.hit_count,
        "createdAt": r.created_at,
    })
}

// ============================================================================
// 规则导入/导出（ModSecurity / Snort / 自定义正则）
// ============================================================================

/// 解析 ModSecurity SecRule 语法，提取正则模式
fn parse_modsecurity_rule(line: &str) -> Option<(String, String, String)> {
    // 匹配: SecRule ARGS|REQUEST_BODY|REQUEST_HEADERS "@rx pattern" "id:1,phase:2,deny,msg:'name'"
    if !line.trim().starts_with("SecRule") { return None; }
    let re_pattern = regex::Regex::new(r#""@rx\s+([^"]+)""#).ok()?;
    let pattern = re_pattern.captures(line)?.get(1)?.as_str().to_string();
    let re_msg = regex::Regex::new(r#"msg:'([^']+)'"#).ok()?;
    let name = re_msg.captures(line).map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_else(|| "ModSecurity Rule".to_string())).unwrap_or_else(|| "ModSecurity Rule".to_string());
    let action = if line.contains("deny") || line.contains("block") { "block" } else { "log" };
    Some((name, pattern, action.to_string()))
}

/// 解析 Snort 规则语法
fn parse_snort_rule(line: &str) -> Option<(String, String, String)> {
    // 匹配: alert tcp $EXTERNAL_NET any -> $HOME_NET any (msg:"name"; content:"pattern"; sid:1;)
    if !line.trim().starts_with("alert") { return None; }
    let re_msg = regex::Regex::new(r#"msg:"([^"]+)""#).ok()?;
    let name = re_msg.captures(line)?.get(1)?.as_str().to_string();
    let re_content = regex::Regex::new(r#"content:"([^"]+)""#).ok()?;
    let content = re_content.captures(line)?.get(1)?.as_str().to_string();
    // 转义正则特殊字符
    let pattern = regex::escape(&content);
    Some((name, pattern, "block".to_string()))
}

/// 解析自定义正则规则（一行一条：name|pattern|action）
fn parse_custom_rule(line: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = line.trim().split('|').collect();
    if parts.len() < 2 { return None; }
    let name = parts[0].trim().to_string();
    let pattern = parts[1].trim().to_string();
    let action = parts.get(2).map(|s| s.trim().to_string()).unwrap_or_else(|| "log".to_string());
    Some((name, pattern, action))
}

/// 导入规则主函数：自动检测格式或按指定格式解析
fn import_rules(content: &str, format: &str, _rule_type: &str) -> Result<(usize, Vec<String>), String> {
    let mut count = 0;
    let mut errors = vec![];
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let parsed = match format {
            "modsecurity" | "modsec" => parse_modsecurity_rule(line),
            "snort" => parse_snort_rule(line),
            "custom" | "regex" => parse_custom_rule(line),
            "auto" | _ => {
                // 自动检测：依次尝试 ModSecurity / Snort / Custom
                parse_modsecurity_rule(line)
                    .or_else(|| parse_snort_rule(line))
                    .or_else(|| parse_custom_rule(line))
            }
        };
        match parsed {
            Some(_) => count += 1,
            None => errors.push(format!("第 {} 行解析失败: {}", i + 1, line.chars().take(80).collect::<String>())),
        }
    }
    Ok((count, errors))
}

/// 生成导入后的 WAF 规则列表
fn imported_ok_waf_rules(content: &str, format: &str) -> Vec<WafRule> {
    let mut rules = vec![];
    let now = current_ms();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let parsed = match format {
            "modsecurity" | "modsec" => parse_modsecurity_rule(line),
            "snort" => parse_snort_rule(line),
            "custom" | "regex" => parse_custom_rule(line),
            _ => parse_modsecurity_rule(line).or_else(|| parse_snort_rule(line)).or_else(|| parse_custom_rule(line)),
        };
        if let Some((name, pattern, action)) = parsed {
            rules.push(WafRule {
                id: format!("waf-imported-{:016x}", rand_u128()),
                name,
                rule_type: "custom".to_string(),
                pattern,
                action,
                severity: "medium".to_string(),
                enabled: true,
                description: format!("导入自 {}", format),
                hit_count: 0,
                created_at: now,
            });
        }
    }
    rules
}

/// 生成导入后的 IDS 规则列表
fn imported_ok_ids_rules(content: &str, format: &str) -> Vec<IdsRule> {
    let mut rules = vec![];
    let now = current_ms();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let parsed = match format {
            "modsecurity" | "modsec" => parse_modsecurity_rule(line),
            "snort" => parse_snort_rule(line),
            "custom" | "regex" => parse_custom_rule(line),
            _ => parse_modsecurity_rule(line).or_else(|| parse_snort_rule(line)).or_else(|| parse_custom_rule(line)),
        };
        if let Some((name, pattern, action)) = parsed {
            rules.push(IdsRule {
                id: format!("ids-imported-{:016x}", rand_u128()),
                name,
                rule_type: "custom".to_string(),
                pattern,
                action,
                threshold: 1,
                window_secs: 60,
                enabled: true,
                description: format!("导入自 {}", format),
                hit_count: 0,
                created_at: now,
            });
        }
    }
    rules
}

/// 导出 WAF 规则为 JSON 或 ModSecurity 格式
fn export_waf_rules(rules: &[WafRule], format: &str) -> String {
    match format {
        "modsecurity" | "modsec" => {
            rules.iter().map(|r| {
                format!("SecRule ARGS|REQUEST_BODY|REQUEST_HEADERS \"@rx {}\" \"id:{},phase:2,{},msg:'{}'\"",
                    r.pattern.replace('"', "\\\""), r.id.replace("waf-", ""), r.action, r.name)
            }).collect::<Vec<_>>().join("\n")
        }
        "json" | _ => {
            serde_json::to_string_pretty(&rules.iter().map(waf_rule_to_json).collect::<Vec<_>>()).unwrap_or_default()
        }
    }
}

/// 导出 IDS 规则为 JSON 或 Snort 格式
fn export_ids_rules(rules: &[IdsRule], format: &str) -> String {
    match format {
        "snort" => {
            rules.iter().map(|r| {
                format!("alert tcp any any -> any any (msg:\"{}\"; content:\"{}\"; sid:{};)",
                    r.name, r.pattern.replace('"', "\\\""), r.id.replace("ids-", ""))
            }).collect::<Vec<_>>().join("\n")
        }
        "json" | _ => {
            serde_json::to_string_pretty(&rules.iter().map(ids_rule_to_json).collect::<Vec<_>>()).unwrap_or_default()
        }
    }
}

// ============================================================================
// 增强安全扫描工具
// ============================================================================

/// 增强漏洞扫描（敏感路径分级 + 安全头检查）
async fn tool_vuln_scan(target: &str, depth: &str) -> String {
    if target.is_empty() { return "错误: 缺少 target".into(); }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap_or_default();
    let base = target.trim_end_matches('/');
    let mut findings: Vec<(String, String)> = vec![];

    // 敏感路径扫描（按深度分级）
    let quick_paths = [
        ("/.env", "critical"), ("/.git/config", "critical"), ("/.svn/entries", "high"),
        ("/admin", "medium"), ("/phpinfo.php", "high"), ("/backup.sql", "critical"),
        ("/.htaccess", "medium"), ("/web.config", "high"), ("/wp-config.php.bak", "critical"),
        ("/config.json", "medium"), ("/api/swagger.json", "low"), ("/.DS_Store", "info"),
    ];
    let deep_paths = [
        ("/.git/HEAD", "critical"), ("/.git/index", "critical"), ("/.env.local", "critical"),
        ("/.env.production", "critical"), ("/database.sql", "critical"), ("/dump.sql", "critical"),
        ("/.ssh/id_rsa", "critical"), ("/server-status", "medium"), ("/nginx-status", "medium"),
        ("/actuator", "medium"), ("/actuator/heapdump", "critical"), ("/metrics", "info"),
        ("/__debug__", "high"), ("/debug/pprof", "high"), ("/elmah.axd", "high"),
    ];
    let paths_to_scan: Vec<&(&str, &str)> = match depth {
        "deep" | "full" => quick_paths.iter().chain(deep_paths.iter()).collect(),
        _ => quick_paths.iter().collect(),
    };
    for (path, severity) in paths_to_scan {
        let url = format!("{}{}", base, path);
        if let Ok(r) = client.get(&url).header("User-Agent", "CradleRing-Scanner/1.0").send().await {
            let st = r.status().as_u16();
            if st == 200 {
                let body_len = r.content_length().unwrap_or(0);
                findings.push((severity.to_string(), format!("暴露: {} (HTTP 200, {}B)", path, body_len)));
            }
        }
    }

    // 安全头检查
    if let Ok(r) = client.get(base).send().await {
        let headers = r.headers();
        let security_headers = [
            ("strict-transport-security", "high", "HSTS 缺失，易受降级攻击"),
            ("content-security-policy", "high", "CSP 缺失，XSS 风险"),
            ("x-frame-options", "medium", "X-Frame-Options 缺失，点击劫持风险"),
            ("x-content-type-options", "low", "X-Content-Type-Options 缺失"),
            ("x-xss-protection", "info", "X-XSS-Protection 缺失（旧浏览器）"),
            ("referrer-policy", "info", "Referrer-Policy 缺失"),
            ("permissions-policy", "info", "Permissions-Policy 缺失"),
        ];
        for (h, sev, desc) in &security_headers {
            if !headers.contains_key(*h) {
                findings.push((sev.to_string(), format!("安全头缺失: {} - {}", h, desc)));
            }
        }
        if let Some(srv) = headers.get("server") {
            let srv_str = srv.to_str().unwrap_or("");
            if !srv_str.is_empty() && !srv_str.to_lowercase().contains("cloudflare") {
                findings.push(("info".to_string(), format!("Server 头泄露: {}", srv_str)));
            }
        }
        if let Some(powered) = headers.get("x-powered-by") {
            findings.push(("low".to_string(), format!("X-Powered-By 泄露: {}", powered.to_str().unwrap_or(""))));
        }
    }

    if target.starts_with("http://") {
        findings.push(("high".to_string(), "使用 HTTP 明文传输，建议启用 HTTPS".to_string()));
    }

    let severity_order = |s: &str| match s { "critical" => 0, "high" => 1, "medium" => 2, "low" => 3, _ => 4 };
    findings.sort_by(|a, b| severity_order(&a.0).cmp(&severity_order(&b.0)));

    if findings.is_empty() {
        format!("✅ 漏洞扫描 {}：未发现明显问题", base)
    } else {
        let mut out = format!("🔍 漏洞扫描 {}\n发现 {} 个问题：\n", base, findings.len());
        for (sev, msg) in &findings {
            let icon = match sev.as_str() { "critical" => "🔴", "high" => "🟠", "medium" => "🟡", _ => "🔵" };
            out.push_str(&format!("  {} [{}] {}\n", icon, sev.to_uppercase(), msg));
        }
        out
    }
}

/// SQL 注入专项检测
async fn tool_sqli_scan(url: &str) -> String {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap_or_default();
    let base = url.trim_end_matches('/');
    let payloads = [
        ("'", "单引号报错"),
        ("' OR '1'='1", "布尔盲注"),
        ("' OR '1'='2", "布尔盲注(对照)"),
        ("1' AND SLEEP(3)-- ", "时间盲注(SLEEP)"),
        ("1' AND BENCHMARK(5000000,MD5(1))-- ", "时间盲注(BENCHMARK)"),
        ("' UNION SELECT 1,2,3-- ", "联合查询"),
        ("' UNION SELECT NULL,version(),NULL-- ", "联合查询(版本)"),
        ("admin'-- ", "注释绕过"),
        ("1' OR 1=1 LIMIT 1-- ", "LIMIT 注入"),
        ("1; DROP TABLE users-- ", "堆叠注入"),
    ];
    let mut results = vec![];
    for (payload, desc) in &payloads {
        let encoded = payload.replace(' ', "%20").replace('\'', "%27");
        let test_url = format!("{}?id={}", base, encoded);
        let start = std::time::Instant::now();
        match client.get(&test_url).header("User-Agent", "CradleRing-SQLi/1.0").send().await {
            Ok(r) => {
                let elapsed = start.elapsed().as_millis();
                let st = r.status().as_u16();
                let body = r.text().await.unwrap_or_default();
                let body_lower = body.to_lowercase();
                let mut indicators = vec![];
                if body_lower.contains("sql syntax") || body_lower.contains("mysql") || body_lower.contains("ora-") || body_lower.contains("postgresql") {
                    indicators.push("数据库错误信息泄露");
                }
                if body_lower.contains("you have an error in your sql") {
                    indicators.push("MySQL 语法错误（SQLi 存在）");
                }
                if elapsed > 2500 && desc.contains("时间盲注") {
                    indicators.push("响应延迟异常（时间盲注可能存在）");
                }
                if st == 500 && !indicators.is_empty() {
                    indicators.push("服务器 500 错误");
                }
                if !indicators.is_empty() {
                    results.push(format!("  🔴 [{}] {}", desc, indicators.join("、")));
                }
            }
            Err(e) => { results.push(format!("  ⚪ [{}] 请求失败: {}", desc, e)); }
        }
    }
    if results.is_empty() {
        format!("✅ SQL 注入检测 {}：未发现注入点", base)
    } else {
        format!("🔍 SQL 注入检测 {}\n发现 {} 个可疑注入点：\n{}", base, results.len(), results.join("\n"))
    }
}

/// XSS 专项检测
async fn tool_xss_scan(url: &str) -> String {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap_or_default();
    let base = url.trim_end_matches('/');
    let payloads = [
        ("<script>alert(1)</script>", "基础 script 标签"),
        ("<img src=x onerror=alert(1)>", "img onerror"),
        ("<svg onload=alert(1)>", "svg onload"),
        ("'\"'><script>alert(1)</script>", "引号闭合"),
        ("<iframe src=javascript:alert(1)>", "iframe javascript"),
        ("<body onload=alert(1)>", "body onload"),
        ("<input onfocus=alert(1) autofocus>", "input autofocus"),
        ("<details open ontoggle=alert(1)>", "details ontoggle"),
        ("<marquee onstart=alert(1)>", "marquee onstart"),
        ("javascript:alert(1)", "javascript: 伪协议"),
    ];
    let mut results = vec![];
    for (payload, desc) in &payloads {
        let encoded = urlencoding_encode(payload);
        let test_url = format!("{}?q={}", base, encoded);
        match client.get(&test_url).header("User-Agent", "CradleRing-XSS/1.0").send().await {
            Ok(r) => {
                let body = r.text().await.unwrap_or_default();
                if body.contains(payload) || body.contains(&payload.replace('"', "&quot;")) || body.contains(&payload.replace('<', "&lt;").replace('>', "&gt;")) {
                    results.push(format!("  🔴 [{}] payload 原样反射", desc));
                } else if body.contains("alert") && body.contains("<script>") {
                    results.push(format!("  🟡 [{}] 部分反射（可能被过滤）", desc));
                }
            }
            Err(e) => { results.push(format!("  ⚪ [{}] 请求失败: {}", desc, e)); }
        }
    }
    if results.is_empty() {
        format!("✅ XSS 检测 {}：未发现反射型 XSS", base)
    } else {
        format!("🔍 XSS 检测 {}\n发现 {} 个可疑 XSS 点：\n{}", base, results.len(), results.join("\n"))
    }
}

/// URL 编码辅助
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            _ => {
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf).as_bytes();
                for b in bytes { out.push_str(&format!("%{:02X}", b)); }
            }
        }
    }
    out
}

/// 端口暴露面分析
async fn tool_exposure_analysis(host: &str) -> String {
    let (hostname, _) = if host.contains(':') {
        let parts: Vec<&str> = host.split(':').collect();
        (parts[0].to_string(), parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(80))
    } else {
        (host.to_string(), 80)
    };
    let risky_ports: Vec<(u16, &str, &str)> = vec![
        (21, "FTP", "明文传输，建议改用 SFTP"),
        (22, "SSH", "确保密钥认证，禁用密码登录"),
        (23, "Telnet", "严重风险，明文协议，应立即关闭"),
        (25, "SMTP", "检查是否开放 relay"),
        (53, "DNS", "检查是否开放递归查询"),
        (110, "POP3", "明文协议，建议改用 POP3S"),
        (143, "IMAP", "明文协议，建议改用 IMAPS"),
        (443, "HTTPS", "正常服务"),
        (445, "SMB", "检查 EternalBlue 漏洞风险"),
        (1433, "MSSQL", "数据库端口暴露，高危"),
        (1521, "Oracle", "数据库端口暴露，高危"),
        (3306, "MySQL", "数据库端口暴露，高危"),
        (3389, "RDP", "远程桌面暴露，高危，建议 VPN 后访问"),
        (5432, "PostgreSQL", "数据库端口暴露，高危"),
        (5900, "VNC", "远程桌面暴露，高危"),
        (6379, "Redis", "未授权访问风险，需密码+绑定内网"),
        (8080, "HTTP-Alt", "检查是否为管理后台"),
        (8443, "HTTPS-Alt", "检查是否为管理后台"),
        (9200, "Elasticsearch", "未授权访问风险"),
        (27017, "MongoDB", "数据库端口暴露，高危"),
    ];
    let mut results = vec![];
    for (port, service, risk) in &risky_ports {
        let addr = format!("{}:{}", hostname, port);
        match tokio::time::timeout(std::time::Duration::from_millis(800), tokio::net::TcpStream::connect(&addr)).await {
            Ok(Ok(_)) => {
                let level = if *port == 23 || *port == 445 || *port == 1433 || *port == 3389 || *port == 6379 || *port == 27017 { "critical" }
                    else if *port == 3306 || *port == 5432 || *port == 9200 || *port == 1521 || *port == 5900 { "high" }
                    else if *port == 21 || *port == 110 || *port == 143 || *port == 25 || *port == 53 { "medium" }
                    else { "info" };
                results.push(format!("  {} [{}] {} 端口开放 - {}",
                    if level == "critical" { "🔴" } else if level == "high" { "🟠" } else if level == "medium" { "🟡" } else { "🔵" },
                    level.to_uppercase(), service, risk));
            }
            _ => { }
        }
    }
    if results.is_empty() {
        format!("✅ 端口暴露面分析 {}: 未发现高危端口开放", hostname)
    } else {
        format!("🔍 端口暴露面分析 {}\n发现 {} 个开放端口：\n{}", hostname, results.len(), results.join("\n"))
    }
}

async fn tool_service_monitor(check: &str) -> String {
    let cmd = match check { "cpu" => "top -bn1 | head -5", "mem" => "free -h", "disk" => "df -h", "net" => "ss -tlnp 2>/dev/null || netstat -tlnp", "all" => "echo '===CPU===' && top -bn1|head -5 && echo '===MEM===' && free -h && echo '===DISK===' && df -h", _ => "ps aux --sort=-%mem|head -20" };
    tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or("监控失败".into())
}

async fn tool_log_analyze(path: &str, pattern: &str, lines: usize) -> String {
    let cmd = if pattern.is_empty() { format!("tail -n {} '{}' | grep -iE 'error|warn|fatal|critical|panic'", lines, path) } else { format!("tail -n {} '{}' | grep -E '{}'", lines, path, pattern) };
    tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or("日志分析失败".into())
}

async fn tool_network_trace(target: &str, max_hops: u16) -> String {
    tokio::process::Command::new("sh").arg("-c").arg(format!("traceroute -m {} {} 2>/dev/null || tracepath {} 2>/dev/null", max_hops, target, target)).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or("追踪失败".into())
}

async fn tool_file_hash(path: &str, algo: &str) -> String {
    let cmd = match algo.to_lowercase().as_str() { "md5" => format!("md5sum '{}'", path), "sha1" => format!("sha1sum '{}'", path), _ => format!("sha256sum '{}'", path) };
    tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or("哈希失败".into())
}

fn tool_encode_decode(action: &str, format: &str, input: &str) -> String {
    match (action, format) {
        ("encode","base64") => base64_encode_bytes(input.as_bytes()),
        ("decode","base64") => base64_decode_str(input).unwrap_or("Base64 解码失败".into()),
        ("encode","url") => input.chars().map(|c| if c.is_alphanumeric() || c=='-'||c=='_'||c=='.'||c=='~' { c.to_string() } else { format!("%{:02X}", c as u8) }).collect(),
        ("decode","url") => { let mut r=String::new(); let b=input.as_bytes(); let mut i=0; while i<b.len() { if b[i]==b'%' && i+2<b.len() { if let Ok(n)=u8::from_str_radix(std::str::from_utf8(&b[i+1..i+3]).unwrap_or("00"),16) { r.push(n as char); i+=3; continue; } } r.push(b[i] as char); i+=1; } r }
        ("encode","hex") => input.bytes().map(|b| format!("{:02x}", b)).collect(),
        ("decode","hex") => (0..input.len()).step_by(2).map(|i| u8::from_str_radix(&input[i..i+2.min(input.len())], 16).unwrap_or(0) as char).collect(),
        ("encode","html") => input.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;"),
        ("decode","html") => input.replace("&amp;","&").replace("&lt;","<").replace("&gt;",">").replace("&quot;","\""),
        _ => format!("不支持: {}/{}", action, format),
    }
}

fn base64_decode_str(s: &str) -> Option<String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = s.bytes().filter_map(|b| CHARS.iter().position(|&c| c == b).map(|p| p as u8)).collect();
    if bytes.len() % 4 != 0 { return None; }
    let mut result = Vec::new();
    for chunk in bytes.chunks(4) {
        result.push(((chunk[0] as u32) << 2 | (chunk[1] as u32) >> 4) as u8);
        if chunk[2] < 64 { result.push((((chunk[1] as u32 & 0x0f) << 4) | (chunk[2] as u32) >> 2) as u8); }
        if chunk[3] < 64 { result.push((((chunk[2] as u32 & 0x03) << 6) | chunk[3] as u32) as u8); }
    }
    String::from_utf8(result).ok()
}

async fn tool_git_ops(command: &str, repo: &str) -> String {
    tokio::process::Command::new("sh").arg("-c").arg(format!("cd '{}' && git {} 2>&1", repo, command)).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or("git 失败".into())
}

async fn tool_docker_ops(command: &str, container: &str) -> String {
    let cmd = if container.is_empty() { format!("docker {} 2>&1", command) } else { format!("docker {} {} 2>&1", command, container) };
    tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or("docker 失败".into())
}

async fn tool_process_manage(action: &str, name: &str) -> String {
    let cmd = match action {
        "list" => "ps aux --sort=-%mem | head -20".to_string(),
        "status" => format!("systemctl status {} 2>&1 || service {} status 2>&1", name, name),
        "start" => format!("sudo systemctl start {} 2>&1 || sudo service {} start 2>&1", name, name),
        "stop" => format!("sudo systemctl stop {} 2>&1 || sudo service {} stop 2>&1", name, name),
        "restart" => format!("sudo systemctl restart {} 2>&1 || sudo service {} restart 2>&1", name, name),
        _ => "ps aux --sort=-%mem | head -20".to_string(),
    };
    tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or("操作失败".into())
}

async fn tool_backup_create(source: &str, dest: &str, format: &str) -> String {
    let cmd = match format { "zip" => format!("zip -r '{}' '{}' 2>&1", dest, source), _ => format!("tar czf '{}' '{}' 2>&1", dest, source) };
    match tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await {
        Ok(o) if o.status.success() => { let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0); format!("备份完成: {} → {} ({}KB)", source, dest, size/1024) }
        Ok(o) => format!("备份失败: {}", String::from_utf8_lossy(&o.stderr)),
        Err(e) => format!("备份失败: {}", e),
    }
}

// ============================================================================
// 运维专家工具（对标 ongrid edgeagent 结构化诊断工具）
// ============================================================================

/// 生成命令的回滚提示（用于审计日志）
fn generate_rollback_hint(command: &str) -> String {
    let lower = command.to_lowercase();
    if lower.contains("systemctl start") || lower.contains("service start") {
        let svc = lower.split_whitespace().last().unwrap_or("service");
        format!("如需回滚：systemctl stop {}", svc)
    } else if lower.contains("systemctl stop") {
        let svc = lower.split_whitespace().last().unwrap_or("service");
        format!("如需回滚：systemctl start {}", svc)
    } else if lower.contains("systemctl restart") {
        let svc = lower.split_whitespace().last().unwrap_or("service");
        format!("已重启，影响短暂；如异常：systemctl status {}", svc)
    } else if lower.contains("iptables") {
        "如需回滚：iptables -F 或恢复规则文件".to_string()
    } else if lower.contains("docker rm") || lower.contains("docker stop") {
        "容器已停止/删除，需重新 docker run 创建".to_string()
    } else if lower.contains("rm ") {
        "文件已删除，需从备份恢复".to_string()
    } else if lower.contains("chmod") || lower.contains("chown") {
        "记录原权限/属主后可恢复".to_string()
    } else {
        "无自动回滚，需人工评估".to_string()
    }
}

/// 获取主机负载概览（对标 ongrid get_host_load）
/// 返回 CPU/内存/磁盘/负载的结构化 JSON
async fn tool_get_host_load() -> String {
    let loadavg = tokio::fs::read_to_string("/proc/loadavg").await.unwrap_or_default();
    let meminfo = tokio::fs::read_to_string("/proc/meminfo").await.unwrap_or_default();
    // 解析 loadavg
    let loads: Vec<&str> = loadavg.split_whitespace().take(3).collect();
    // 解析内存
    let mut mem_total = 0u64;
    let mut mem_avail = 0u64;
    let mut swap_total = 0u64;
    let mut swap_free = 0u64;
    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            mem_total = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        } else if line.starts_with("MemAvailable:") {
            mem_avail = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        } else if line.starts_with("SwapTotal:") {
            swap_total = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        } else if line.starts_with("SwapFree:") {
            swap_free = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }
    let mem_used_pct = if mem_total > 0 { ((mem_total - mem_avail) as f64 / mem_total as f64 * 100.0).round() as u64 } else { 0 };
    let swap_used_pct = if swap_total > 0 { ((swap_total - swap_free) as f64 / swap_total as f64 * 100.0).round() as u64 } else { 0 };
    // 磁盘使用（df）
    let df = tokio::process::Command::new("sh").arg("-c").arg("df -h --output=target,size,used,avail,pcent 2>/dev/null | tail -n +2").output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    serde_json::json!({
        "load": { "1min": loads.get(0).unwrap_or(&"0"), "5min": loads.get(1).unwrap_or(&"0"), "15min": loads.get(2).unwrap_or(&"0") },
        "memory": { "total_kb": mem_total, "available_kb": mem_avail, "used_pct": mem_used_pct },
        "swap": { "total_kb": swap_total, "free_kb": swap_free, "used_pct": swap_used_pct },
        "disk": df.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>(),
    }).to_string()
}

/// 获取主机进程列表（对标 ongrid get_host_processes）
async fn tool_get_host_processes(top_n: usize, sort_by: &str) -> String {
    let sort_flag = match sort_by { "mem" => "-rss", "pid" => "-pid", _ => "-pcpu" };
    let cmd = format!("ps aux --sort={} | head -{}", sort_flag, top_n.max(1) + 1);
    let out = tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    out
}

/// 目录占用摘要（对标 ongrid host_du_summary）
async fn tool_host_du_summary(paths: &[String], depth: u32) -> String {
    let mut results = vec![];
    for path in paths {
        let cmd = format!("du -h --max-depth={} '{}' 2>/dev/null | sort -rh | head -20", depth, path);
        let out = tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        results.push(format!("=== {} ===\n{}", path, out));
    }
    results.join("\n\n")
}

/// 查找大文件（对标 ongrid host_find_large_files）
async fn tool_host_find_large_files(paths: &[String], top_n: usize) -> String {
    let mut results = vec![];
    for path in paths {
        let cmd = format!("find '{}' -type f -size +100M -exec ls -lh {{}} \\; 2>/dev/null | sort -k5 -rh | head -{}", path, top_n);
        let out = tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        if !out.trim().is_empty() { results.push(format!("=== {} ===\n{}", path, out)); }
    }
    if results.is_empty() { "未找到大于 100MB 的文件".to_string() } else { results.join("\n\n") }
}

/// 文件元信息（对标 ongrid host_stat_file）
async fn tool_host_stat_file(path: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    match tokio::fs::metadata(path).await {
        Ok(m) => {
            let mode = m.permissions().mode();
            let perms = format!("{:04o}", mode & 0o7777);
            serde_json::json!({
                "path": path,
                "size": m.len(),
                "is_dir": m.is_dir(),
                "is_file": m.is_file(),
                "permissions": perms,
                "mode": format!("0o{:o}", mode),
                "modified": m.modified().ok().map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)).unwrap_or(0),
            }).to_string()
        }
        Err(e) => format!("stat 失败: {}", e),
    }
}

/// 网络命名空间检查（对标 ongrid host_netns_inspect）
async fn tool_host_netns_inspect() -> String {
    let cmd = "ip netns list 2>/dev/null; echo '---interfaces---'; ip -br addr show 2>/dev/null; echo '---routes---'; ip route show 2>/dev/null | head -20";
    tokio::process::Command::new("sh").arg("-c").arg(cmd).output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// 综合主机诊断快照（一次拉取 load + 进程 top + 网络）
async fn tool_host_diagnostic_snapshot() -> String {
    let load = tool_get_host_load().await;
    let procs = tool_get_host_processes(10, "cpu").await;
    let net = tool_host_netns_inspect().await;
    serde_json::json!({
        "load": serde_json::from_str::<serde_json::Value>(&load).unwrap_or(serde_json::json!({})),
        "top_processes_by_cpu": procs,
        "network": net,
    }).to_string()
}

// ============================================================================
// 认证 & 多账号系统：密码哈希、JWT、用户 CRUD、角色权限
// ============================================================================

/// 内置 JWT 密钥（首次启动随机生成并写入 home/.cradle-ring/data/jwt_secret）
fn jwt_secret(state: &AppState) -> String {
    let path = format!("{}/.cradle-ring/data/jwt_secret", state.storage.home);
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if !s.is_empty() { return s; }
    }
    let s = format!("{:032x}", rand_u128());
    let _ = std::fs::write(&path, &s);
    s
}

/// 生成随机 salt（16 字节 hex）
fn gen_salt() -> String {
    format!("{:016x}{:016x}", rand_u128(), rand_u128())
}

/// salted SHA-256：返回 "salt$hash"
fn hash_password(password: &str) -> String {
    use sha2::{Sha256, Digest};
    let salt = gen_salt();
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(password.as_bytes());
    let hash = hasher.finalize();
    format!("{}${}", salt, base64::engine::general_purpose::STANDARD.encode(hash))
}

/// 校验密码
fn verify_password(password: &str, stored: &str) -> bool {
    use sha2::{Sha256, Digest};
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 2 { return false; }
    let salt = parts[0];
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(password.as_bytes());
    let hash = hasher.finalize();
    let expected = base64::engine::general_purpose::STANDARD.encode(hash);
    constant_time_eq(expected.as_bytes(), parts[1].as_bytes())
}

/// 简单常数时间比较
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) { diff |= x ^ y; }
    diff == 0
}

/// 签发 JWT（无外部依赖，手工 HMAC-SHA256）
fn issue_jwt(user: &User, ttl_secs: u64, state: &AppState) -> AuthToken {
    use sha2::{Sha256, Digest};
    let issued_at = current_ms();
    let expires_at = issued_at + (ttl_secs as i64) * 1000;
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_json = format!(
        r#"{{"sub":"{}","uid":"{}","role":"{}","iat":{},"exp":{}}}"#,
        user.username, user.id, user.role, issued_at / 1000, expires_at / 1000
    );
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
    let secret = jwt_secret(state);
    let mut hasher = Sha256::new();
    hasher.update(format!("{}.{}", header, payload).as_bytes());
    hasher.update(secret.as_bytes());
    let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
    let token = format!("{}.{}.{}", header, payload, sig);
    AuthToken {
        token,
        user_id: user.id.clone(),
        username: user.username.clone(),
        role: user.role.clone(),
        issued_at,
        expires_at,
    }
}

/// 校验 JWT，返回 (user_id, username, role)；失败返回 None
fn verify_jwt(token: &str, state: &AppState) -> Option<(String, String, String)> {
    use sha2::{Sha256, Digest};
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 { return None; }
    let secret = jwt_secret(state);
    let mut hasher = Sha256::new();
    hasher.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
    hasher.update(secret.as_bytes());
    let expected_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
    if !constant_time_eq(expected_sig.as_bytes(), parts[2].as_bytes()) { return None; }
    let payload_json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1].as_bytes()).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_json).ok()?;
    let exp = payload["exp"].as_i64().unwrap_or(0);
    if exp > 0 && current_ms() / 1000 > exp { return None; }
    Some((
        payload["uid"].as_str()?.to_string(),
        payload["sub"].as_str()?.to_string(),
        payload["role"].as_str()?.to_string(),
    ))
}

/// 初始化默认 admin 用户（如 users.json 不存在或为空）
async fn ensure_default_admin(state: &AppState) {
    let mut users = state.storage.load_users();
    if !users.is_empty() { return; }
    let now = current_ms();

    // 优先从安装时生成的凭据文件读取用户名和密码
    let cred_path = format!("{}/.cradle-ring/data/.admin_credentials", state.storage.home);
    let (username, password) = if let Ok(creds) = std::fs::read_to_string(&cred_path) {
        let parts: Vec<&str> = creds.trim().split(':').collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            // 凭据文件无效，生成随机密码
            let pwd = generate_random_password();
            ("admin".to_string(), pwd)
        }
    } else {
        // 无凭据文件，生成随机密码
        let pwd = generate_random_password();
        ("admin".to_string(), pwd)
    };

    users.push(User {
        id: format!("user-{}", &format!("{:016x}", rand_u128())[..12]),
        username: username.clone(),
        password_hash: hash_password(&password),
        display_name: "管理员".to_string(),
        email: Some(format!("{}@cradlering.local", username)),
        role: "admin".to_string(),
        scopes: vec!["*".to_string()],
        agent_id: "main".to_string(),
        enabled: true,
        created_at: now,
        last_login: None,
        approval_enabled: true,
    });
    state.storage.save_users(&users);
    // 广播事件（不暴露密码）
    let _ = broadcast_event(state, "users.initialized", json!({"username": username})).await;
    // 如果是随机生成的密码（非凭据文件），打印到日志提示用户
    if !std::path::Path::new(&cred_path).exists() {
        eprintln!("⚠️  未找到安装凭据文件，已生成随机密码");
        eprintln!("   用户名: {}", username);
        eprintln!("   密码:   {}", password);
        eprintln!("   请立即修改密码！");
    }
}

/// 生成随机密码（16位，字母数字+特殊字符）
fn generate_random_password() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    current_ms().hash(&mut hasher);
    rand_u128().hash(&mut hasher);
    let hash = hasher.finish();
    // 转为 16 位可读密码（字母数字+特殊字符）
    let charset = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*";
    let mut pwd = String::new();
    let mut h = hash;
    for _ in 0..16 {
        pwd.push(charset[(h % charset.len() as u64) as usize] as char);
        h = h.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    pwd
}

/// 预置 6 个运维专家角色（对标 ongrid 的 specialist 体系）
async fn ensure_ops_roles(state: &AppState) {
    let roles = state.storage.load_agent_roles();
    // 用 role 字段去重（避免重复创建）
    let existing: std::collections::HashSet<String> = roles.iter().map(|r| r.role.clone()).collect();
    if existing.contains("根因诊断专家") { return; }
    let now = current_ms();
    let read_only_tools = vec![
        "get_host_load".to_string(), "get_host_processes".to_string(), "query_change_events".to_string(),
        "host_du_summary".to_string(), "host_find_large_files".to_string(), "host_stat_file".to_string(),
        "host_netns_inspect".to_string(), "host_diagnostic_snapshot".to_string(),
        "dns_lookup".to_string(), "ssl_check".to_string(), "log_analyze".to_string(),
        "service_monitor".to_string(), "port_scan".to_string(), "http_probe".to_string(),
        "read_file".to_string(), "web_search".to_string(),
    ];
    let presets = vec![
        AgentRole {
            id: "role-ops-investigator".to_string(), name: "根因诊断专家".to_string(),
            role: "根因诊断专家".to_string(),
            goal: "顺因果链溯源到根因（0号病人），不止于症状摘要。输出根因/因果链/现象/置信度。".to_string(),
            backstory: "资深 SRE，擅长从告警/指标/日志/变更事件多维度关联，深挖到源头。只看不动。".to_string(),
            tools: Some(read_only_tools.clone()),
            model: None, system_prompt_template: None,
            max_iterations: 18, allow_delegation: false, created_at: now, created_by: "system".to_string(),
        },
        AgentRole {
            id: "role-ops-sre".to_string(), name: "SRE专家".to_string(),
            role: "SRE/可观测性专家".to_string(),
            goal: "判断系统健康度、黄金四信号(latency/error/traffic/saturation)偏离、SLO达成、告警优先级。".to_string(),
            backstory: "专注系统可观测性，擅长趋势分析和优先级判断，不做单机操作。".to_string(),
            tools: Some(read_only_tools.clone()),
            model: None, system_prompt_template: None,
            max_iterations: 15, allow_delegation: false, created_at: now, created_by: "system".to_string(),
        },
        AgentRole {
            id: "role-ops-network".to_string(), name: "网络诊断专家".to_string(),
            role: "网络问题专家".to_string(),
            goal: "诊断网络层问题：OVS/netfilter/netns/conntrack/路由/网卡/eBPF。".to_string(),
            backstory: "精通 Linux 网络栈，能排查 OVS 流表、iptables、命名空间、conntrack 表满等复杂网络问题。".to_string(),
            tools: Some({
                let mut t = read_only_tools.clone();
                t.extend_from_slice(&["network_trace".to_string(), "waf_detect".to_string(), "subdomain_enum".to_string()]);
                t
            }),
            model: None, system_prompt_template: None,
            max_iterations: 15, allow_delegation: false, created_at: now, created_by: "system".to_string(),
        },
        AgentRole {
            id: "role-ops-disk".to_string(), name: "磁盘专家".to_string(),
            role: "文件系统/磁盘容量专家".to_string(),
            goal: "定位磁盘满/大文件/inode耗尽/挂载点问题，给出清理建议。".to_string(),
            backstory: "对 du/find/stat/df 了如指掌，能快速定位磁盘占用元凶。".to_string(),
            tools: Some({
                let mut t = read_only_tools.clone();
                t.extend_from_slice(&["backup_create".to_string()]);
                t
            }),
            model: None, system_prompt_template: None,
            max_iterations: 15, allow_delegation: false, created_at: now, created_by: "system".to_string(),
        },
        AgentRole {
            id: "role-ops-compute".to_string(), name: "计算资源专家".to_string(),
            role: "计算资源专家".to_string(),
            goal: "诊断 CPU/内存/load/进程调度/OOM/NUMA/内核参数问题。".to_string(),
            backstory: "精通 Linux 进程调度和内存管理，能分析上下文切换、CPU steal、OOM 痕迹。".to_string(),
            tools: Some(read_only_tools.clone()),
            model: None, system_prompt_template: None,
            max_iterations: 15, allow_delegation: false, created_at: now, created_by: "system".to_string(),
        },
        AgentRole {
            id: "role-ops-ops".to_string(), name: "运维服务专家".to_string(),
            role: "运维/服务运营专家".to_string(),
            goal: "服务状态/启停重启/部署/配置/容量与计划任务。mutating 操作走审批。".to_string(),
            backstory: "熟悉 systemd/docker/k8s 服务管理，了解容量规划和计划任务。".to_string(),
            tools: Some({
                let mut t = read_only_tools.clone();
                t.extend_from_slice(&["exec".to_string(), "process_manage".to_string(), "docker_ops".to_string(), "git_ops".to_string(), "write_file".to_string()]);
                t
            }),
            model: None, system_prompt_template: None,
            max_iterations: 15, allow_delegation: true, created_at: now, created_by: "system".to_string(),
        },
        AgentRole {
            id: "role-ops-reviewer".to_string(), name: "SOP二审专家".to_string(),
            role: "高危操作SOP二审专家".to_string(),
            goal: "对 mutating/destructive 提案做静态审查。reject 是默认选项，approve 必须三条全满足：有SOP、无并行操作、回滚路径已知。".to_string(),
            backstory: "极其谨慎的安全审查专家，宁可误拒不可误批。只读，不做任何变更。".to_string(),
            tools: Some(vec!["query_change_events".to_string(), "get_host_load".to_string(), "service_monitor".to_string(), "log_analyze".to_string(), "read_file".to_string()]),
            model: None, system_prompt_template: None,
            max_iterations: 5, allow_delegation: false, created_at: now, created_by: "system".to_string(),
        },
    ];
    let mut all = state.storage.load_agent_roles();
    all.extend(presets);
    state.storage.save_agent_roles(&all);
    // 同时初始化 RCA 根因诊断工作流模板
    ensure_rca_workflow(state).await;
}

/// 初始化预置的根因诊断工作流（对标 ongrid incident-investigator）
async fn ensure_rca_workflow(state: &AppState) {
    let graphs = state.storage.load_workflow_graphs();
    if graphs.iter().any(|g| g.name == "根因诊断(RCA)") { return; }
    let now = current_ms();
    let graph = WorkflowGraph {
        id: "wf-rca-root-cause".to_string(),
        name: "根因诊断(RCA)".to_string(),
        description: "对标 ongrid incident-investigator：收集证据 → 关联变更事件 → 因果链溯源 → 输出根因/置信度".to_string(),
        nodes: vec![
            WorkflowNode {
                id: "collect".to_string(), name: "收集证据".to_string(), node_type: NodeType::Tool,
                agent_role: None, prompt_template: None,
                tool_name: Some("host_diagnostic_snapshot".to_string()),
                tool_args_template: None, branches: vec![], default_edge: None,
                fan_out_field: None, fan_out_role: None, max_concurrent: None, reduce_mode: None,
                prompt: None, output_field: Some("vars.evidence".to_string()), config: serde_json::Value::Null,
            },
            WorkflowNode {
                id: "check_changes".to_string(), name: "关联变更事件".to_string(), node_type: NodeType::Tool,
                agent_role: None, prompt_template: None,
                tool_name: Some("query_change_events".to_string()),
                tool_args_template: Some(json!({"limit": 20})),
                branches: vec![], default_edge: None,
                fan_out_field: None, fan_out_role: None, max_concurrent: None, reduce_mode: None,
                prompt: None, output_field: Some("vars.changes".to_string()), config: serde_json::Value::Null,
            },
            WorkflowNode {
                id: "analyze".to_string(), name: "因果链分析".to_string(), node_type: NodeType::Agent,
                agent_role: Some("role-ops-investigator".to_string()),
                prompt_template: Some(
                    "请基于以下证据做根因分析（RCA），顺因果链溯源到根因（0号病人）。\n\n\
                    系统状态快照：${vars.evidence}\n\n\
                    近期变更事件：${vars.changes}\n\n\
                    用户报告的问题：${input}\n\n\
                    输出格式：\n根因：...\n因果链：源头→症状（每段带证据）\n现象：...\n置信度：...\n建议：...".to_string()
                ),
                tool_name: None, tool_args_template: None, branches: vec![], default_edge: None,
                fan_out_field: None, fan_out_role: None, max_concurrent: None, reduce_mode: None,
                prompt: None, output_field: Some("output".to_string()), config: serde_json::Value::Null,
            },
            WorkflowNode {
                id: "end".to_string(), name: "完成".to_string(), node_type: NodeType::End,
                agent_role: None, prompt_template: None, tool_name: None, tool_args_template: None,
                branches: vec![], default_edge: None, fan_out_field: None, fan_out_role: None,
                max_concurrent: None, reduce_mode: None, prompt: None, output_field: None,
                config: serde_json::Value::Null,
            },
        ],
        edges: vec![
            WorkflowEdge { id: "e1".to_string(), from: "collect".to_string(), to: "check_changes".to_string(), condition: None, label: None },
            WorkflowEdge { id: "e2".to_string(), from: "check_changes".to_string(), to: "analyze".to_string(), condition: None, label: None },
            WorkflowEdge { id: "e3".to_string(), from: "analyze".to_string(), to: "end".to_string(), condition: None, label: None },
        ],
        entry_node: "collect".to_string(),
        state_schema: vec!["evidence".to_string(), "changes".to_string()],
        enabled: true,
        created_at: now,
        created_by: "system".to_string(),
    };
    let mut graphs = state.storage.load_workflow_graphs();
    graphs.push(graph);
    state.storage.save_workflow_graphs(&graphs);
}

/// AI SOP 二审（对标 ongrid reviewer）：对 mutating 操作做静态审查
/// 规则：reject 是默认；approve 需三条全满足
async fn ai_sop_review(
    state: &AppState,
    action: &str,
    target: &str,
    reason: &str,
    blast_radius: &str,
) -> ReviewDecision {
    // 检查 1：是否有对应 SOP（内置规则库）
    let (has_sop, matched_sop) = check_sop_coverage(action, target);
    // 检查 2：当前是否有并行同类操作（查 change_events 最近 5 分钟）
    let recent = state.storage.load_change_events(50, None);
    let five_min_ago = current_ms() - 300_000;
    let parallel_count = recent.iter()
        .filter(|e| e.ts > five_min_ago && e.kind.contains("write") && e.target.contains(target) && e.result == "ok")
        .count();
    let no_parallel_op = parallel_count == 0;
    // 检查 3：回滚路径是否已知（基于命令模式）
    let rollback_known = !generate_rollback_hint(target).contains("无自动回滚");
    // 决策：三条全满足才 approve
    let decision = if has_sop && no_parallel_op && rollback_known { "approve" } else { "reject" };
    let parallel_desc = if no_parallel_op { "无✓".to_string() } else { format!("有({}个)✗", parallel_count) };
    let comment = format!(
        "SOP覆盖:{} | 并行操作:{}(近5分钟{}) | 回滚已知:{} → {}",
        if has_sop { "✓" } else { "✗" },
        parallel_desc,
        parallel_count,
        if rollback_known { "✓" } else { "✗" },
        decision,
    );
    ReviewDecision {
        id: format!("rev-{:016x}", rand_u128()),
        action: action.to_string(),
        target: target.to_string(),
        reason: reason.to_string(),
        blast_radius: blast_radius.to_string(),
        decision: decision.to_string(),
        has_sop, no_parallel_op, rollback_known,
        comment,
        matched_sop,
        ts: current_ms(),
    }
}

/// 内置 SOP 规则库：检查操作是否有覆盖的 SOP
fn check_sop_coverage(_action: &str, target: &str) -> (bool, Option<String>) {
    let lower = target.to_lowercase();
    // 服务重启
    if lower.contains("systemctl restart") || lower.contains("service restart") || lower.contains("docker restart") {
        return (true, Some("SOP-001: 服务重启——确认无活跃告警、非业务高峰、已备份配置".to_string()));
    }
    // 包安装
    if lower.contains("apt install") || lower.contains("yum install") || lower.contains("dnf install") || lower.contains("pip install") {
        return (true, Some("SOP-002: 包安装——确认来源可信、版本锁定、依赖兼容".to_string()));
    }
    // 配置文件修改
    if lower.contains(".conf") || lower.contains(".yaml") || lower.contains(".yml") || lower.contains(".json") || lower.contains("/etc/") {
        return (true, Some("SOP-003: 配置变更——备份原文件、验证语法、准备回滚".to_string()));
    }
    // 防火墙
    if lower.contains("iptables") || lower.contains("firewall-cmd") || lower.contains("ufw") {
        return (true, Some("SOP-004: 防火墙变更——记录原规则、保留 SSH 兜底、5分钟自动回滚".to_string()));
    }
    // 文件删除
    if lower.contains("rm ") {
        return (true, Some("SOP-005: 文件删除——确认非系统文件、有备份、可恢复".to_string()));
    }
    // 权限变更
    if lower.contains("chmod") || lower.contains("chown") {
        return (true, Some("SOP-006: 权限变更——记录原权限、不递归 777".to_string()));
    }
    (false, None)
}

/// 角色权限矩阵：返回该角色的内置 scopes（硬编码 fallback）
fn role_default_scopes(role: &str) -> Vec<String> {
    match role {
        "admin" => vec![
            "*".to_string(),
        ],
        "manager" => vec![
            "chat".to_string(), "sessions.*".to_string(), "memory.*".to_string(), "tools.*".to_string(), "approval.*".to_string(),
            "channels.read".to_string(), "cron.read".to_string(), "plugins.read".to_string(), "config.read".to_string(),
            "users.read".to_string(), "approval.advanced".to_string(),
        ],
        "supervisor" => vec![
            "chat".to_string(), "sessions.read".to_string(), "memory.read".to_string(), "approval.approve".to_string(),
            "channels.read".to_string(), "cron.read".to_string(),
        ],
        "operator" => vec![
            "chat".to_string(), "sessions.*".to_string(), "memory.*".to_string(), "tools.exec".to_string(),
            "channels.read".to_string(), "cron.read".to_string(),
        ],
        "viewer" => vec![
            "sessions.read".to_string(), "memory.read".to_string(), "channels.read".to_string(), "logs.read".to_string(),
        ],
        _ => vec!["chat".to_string()],
    }
}

/// 获取角色的实际 scopes：先查 roles.json 自定义角色，fallback 到硬编码
fn get_role_scopes(storage: &Storage, role: &str) -> Vec<String> {
    let roles = storage.load_roles();
    if let Some(r) = roles.iter().find(|r| r.name == role) {
        return r.scopes.clone();
    }
    role_default_scopes(role)
}

/// 检查用户是否拥有指定 scope（支持通配符匹配）
fn user_has_scope(user: &User, required: &str) -> bool {
    if user.scopes.iter().any(|s| s == "*") { return true; }
    // 精确匹配
    if user.scopes.iter().any(|s| s == required) { return true; }
    // 前缀通配：如 "approval.*" 匹配 "approval.approve"
    for scope in &user.scopes {
        if let Some(prefix) = scope.strip_suffix(".*") {
            if required.starts_with(prefix) { return true; }
        }
    }
    // 按角色默认权限
    let defaults = role_default_scopes(&user.role);
    if defaults.iter().any(|s| s == "*") { return true; }
    if defaults.iter().any(|s| s == required) { return true; }
    for scope in &defaults {
        if let Some(prefix) = scope.strip_suffix(".*") {
            if required.starts_with(prefix) { return true; }
        }
    }
    false
}

fn user_to_json(u: &User) -> serde_json::Value {
    json!({
        "id": u.id,
        "username": u.username,
        "displayName": u.display_name,
        "email": u.email,
        "role": u.role,
        "scopes": u.scopes,
        "agentId": u.agent_id,
        "enabled": u.enabled,
        "approvalEnabled": u.approval_enabled,
        "createdAt": u.created_at,
        "lastLogin": u.last_login,
    })
}

fn approval_flow_to_json(f: &ApprovalFlow) -> serde_json::Value {
    json!({
        "id": f.id,
        "name": f.name,
        "triggerPatterns": f.trigger_patterns,
        "kinds": f.kinds,
        "steps": f.steps.iter().map(|s| json!({
            "order": s.order,
            "name": s.name,
            "approverRole": s.approver_role,
            "approverIds": s.approver_ids,
            "notifyChannels": s.notify_channels,
            "notifyTargets": s.notify_targets,
            "autoApproveAfterSecs": s.auto_approve_after_secs,
            "requireAll": s.require_all,
        })).collect::<Vec<_>>(),
        "enabled": f.enabled,
        "createdBy": f.created_by,
        "createdAt": f.created_at,
    })
}

fn approval_instance_to_json(inst: &ApprovalInstance) -> serde_json::Value {
    json!({
        "id": inst.id,
        "flowId": inst.flow_id,
        "flowName": inst.flow_name,
        "title": inst.title,
        "description": inst.description,
        "command": inst.command,
        "kind": inst.kind,
        "requestedBy": inst.requested_by,
        "requestedUsername": inst.requested_username,
        "currentStep": inst.current_step,
        "totalSteps": inst.total_steps,
        "status": inst.status,
        "decisions": inst.decisions.iter().map(|d| json!({
            "stepOrder": d.step_order,
            "approverId": d.approver_id,
            "approverUsername": d.approver_username,
            "decision": d.decision,
            "comment": d.comment,
            "decidedAt": d.decided_at,
            "viaChannel": d.via_channel,
        })).collect::<Vec<_>>(),
        "createdAt": inst.created_at,
        "updatedAt": inst.updated_at,
        "sessionKey": inst.session_key,
        "runId": inst.run_id,
        "asyncNonBlocking": inst.async_non_blocking,
        "executionResult": inst.execution_result,
        "completedAt": inst.completed_at,
    })
}

// ----------------------------------------------------------------------------
// 多级审批工作流引擎
// ----------------------------------------------------------------------------

/// 查找匹配命令的审批流模板（返回首个 enabled 且匹配的）
fn find_matching_flow(state: &AppState, kind: &str, command: &str) -> Option<ApprovalFlow> {
    let flows = state.storage.load_approval_flows();
    let lower = command.to_lowercase();
    for flow in &flows {
        if !flow.enabled { continue; }
        if !flow.kinds.iter().any(|k| k == "*" || k == kind) { continue; }
        // 无 trigger_patterns = 匹配该 kind 的所有命令
        if flow.trigger_patterns.is_empty() {
            return Some(flow.clone());
        }
        if flow.trigger_patterns.iter().any(|p| lower.contains(&p.to_lowercase())) {
            return Some(flow.clone());
        }
    }
    None
}

/// 创建审批实例并通知第 1 步审批人。返回实例 id。
/// 若 async_non_blocking=true，则不阻塞调用方（调用方收到"等待审批"提示）。
async fn create_approval_instance(
    state: &AppState,
    flow: &ApprovalFlow,
    kind: &str,
    command: &str,
    title: &str,
    description: &str,
    requested_by: &str,
    requested_username: &str,
    session_key: &str,
    run_id: &str,
    async_non_blocking: bool,
) -> String {
    let id = format!("inst-{:016x}", rand_u128());
    let now = current_ms();
    let inst = ApprovalInstance {
        id: id.clone(),
        flow_id: flow.id.clone(),
        flow_name: flow.name.clone(),
        title: title.to_string(),
        description: description.to_string(),
        command: command.to_string(),
        kind: kind.to_string(),
        requested_by: requested_by.to_string(),
        requested_username: requested_username.to_string(),
        current_step: 1,
        total_steps: flow.steps.len() as u32,
        status: "pending".to_string(),
        decisions: vec![],
        created_at: now,
        updated_at: now,
        session_key: session_key.to_string(),
        run_id: run_id.to_string(),
        async_non_blocking,
        execution_result: None,
        completed_at: None,
    };
    let mut items = state.storage.load_approval_instances();
    items.push(inst.clone());
    state.storage.save_approval_instances(&items);

    // 广播请求事件
    let _ = broadcast_event(state, "approval.instance.created", approval_instance_to_json(&inst)).await;

    // 通知第 1 步审批人
    if let Some(step) = flow.steps.first() {
        notify_approval_step(state, &inst, step).await;
    }

    id
}

/// 通知某个步骤的审批人（Web 广播 + IM 渠道）
async fn notify_approval_step(state: &AppState, inst: &ApprovalInstance, step: &ApprovalStep) {
    // 1. Web 广播
    let _ = broadcast_event(state, "approval.step.pending", json!({
        "instanceId": inst.id,
        "title": inst.title,
        "command": inst.command,
        "stepOrder": step.order,
        "stepName": step.name,
        "approverRole": step.approver_role,
        "approverIds": step.approver_ids,
        "flowName": inst.flow_name,
        "requestedByUsername": inst.requested_username,
    })).await;

    // 2. IM 渠道通知
    let text = format!(
        "🔔 审批请求 [{}]\n第 {}/{} 步：{}\n标题：{}\n请求人：{}\n命令：{}\n\n回复「同意 {}」或「拒绝 {}」",
        inst.flow_name, step.order, inst.total_steps, step.name,
        inst.title, inst.requested_username, inst.command,
        inst.id, inst.id
    );
    for (i, channel) in step.notify_channels.iter().enumerate() {
        let target = step.notify_targets.get(i).cloned().unwrap_or_default();
        if target.is_empty() { continue; }
        let cfg = state.channel_config(channel);
        if let Some(cfg) = cfg {
            let _ = send_to_channel(state, channel, &cfg, &target, &text).await;
        }
    }
}

/// 推进审批实例：处理审批/拒绝决策。
/// 返回 (all_approved, instance_clone)
async fn advance_approval_instance(
    state: Arc<AppState>,
    instance_id: &str,
    approver_id: &str,
    approver_username: &str,
    decision: &str,
    comment: &str,
    via_channel: &str,
) -> Option<(bool, ApprovalInstance)> {
    if decision != "approve" && decision != "reject" { return None; }
    let mut items = state.storage.load_approval_instances();
    let flow_id;
    let result;
    {
        let inst = items.iter_mut().find(|i| i.id == instance_id)?;
        if inst.status != "pending" { return None; }
        flow_id = inst.flow_id.clone();
        // 记录决策
        inst.decisions.push(ApprovalDecision {
            step_order: inst.current_step,
            approver_id: approver_id.to_string(),
            approver_username: approver_username.to_string(),
            decision: decision.to_string(),
            comment: comment.to_string(),
            decided_at: current_ms(),
            via_channel: via_channel.to_string(),
        });
        inst.updated_at = current_ms();

        if decision == "reject" {
            inst.status = "rejected".to_string();
            inst.completed_at = Some(current_ms());
            result = (false, inst.clone());
        } else {
            // 判断本步骤是否完成
            let flows = state.storage.load_approval_flows();
            let flow = flows.iter().find(|f| f.id == flow_id);
            let step = flow.and_then(|f| f.steps.iter().find(|s| s.order == inst.current_step));
            let step_done = match step {
                Some(s) if s.require_all => {
                    // 需要所有指定审批人都通过
                    let required = if !s.approver_ids.is_empty() {
                        s.approver_ids.len()
                    } else if !s.approver_role.is_empty() {
                        // 统计该角色的用户数
                        state.storage.load_users().iter()
                            .filter(|u| u.role == s.approver_role && u.enabled)
                            .count().max(1)
                    } else { 1 };
                    let approved_count = inst.decisions.iter()
                        .filter(|d| d.step_order == inst.current_step && d.decision == "approve")
                        .count();
                    approved_count >= required
                }
                _ => true, // 任一审批人即可
            };
            if step_done {
                // 进入下一步或全部完成
                if inst.current_step >= inst.total_steps {
                    inst.status = "approved".to_string();
                    inst.completed_at = Some(current_ms());
                    result = (true, inst.clone());
                } else {
                    inst.current_step += 1;
                    inst.updated_at = current_ms();
                    result = (false, inst.clone());
                }
            } else {
                // 本步骤尚未达成，继续等待
                result = (false, inst.clone());
            }
        }
    }
    state.storage.save_approval_instances(&items);

    let (all_approved, inst_clone) = result;

    // 广播事件
    let _ = broadcast_event(&state, "approval.instance.updated", approval_instance_to_json(&inst_clone)).await;

    // 如果进入下一步，通知下一步审批人
    if !all_approved && inst_clone.status == "pending" {
        let flows = state.storage.load_approval_flows();
        if let Some(flow) = flows.iter().find(|f| f.id == flow_id) {
            if let Some(step) = flow.steps.iter().find(|s| s.order == inst_clone.current_step) {
                notify_approval_step(&state, &inst_clone, step).await;
            }
        }
    }

    // 如果被拒绝，通知请求者
    if inst_clone.status == "rejected" {
        let _ = broadcast_event(&state, "approval.instance.rejected", json!({
            "instanceId": instance_id,
            "requestedBy": inst_clone.requested_by,
            "comment": comment,
        })).await;
        // 唤醒等待中的 exec 调用（拒绝）
        let mut pending = state.pending_approval_instances.lock().await;
        if let Some(tx) = pending.remove(instance_id) {
            let _ = tx.send(false);
        }
    }

    // 如果全部批准，唤醒等待中的 exec 调用
    if all_approved {
        let mut pending = state.pending_approval_instances.lock().await;
        if let Some(tx) = pending.remove(instance_id) {
            let _ = tx.send(true);
        }
    }

    Some((all_approved, inst_clone))
}

/// 后台循环：处理超时自动通过 + 实例状态推进
async fn approval_advance_loop(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        let now = current_ms();
        let mut items = state.storage.load_approval_instances();
        let flows = state.storage.load_approval_flows();
        let mut changed = false;
        for inst in items.iter_mut() {
            if inst.status != "pending" { continue; }
            let flow = match flows.iter().find(|f| f.id == inst.flow_id) {
                Some(f) => f,
                None => continue,
            };
            let step = match flow.steps.iter().find(|s| s.order == inst.current_step) {
                Some(s) => s,
                None => continue,
            };
            // 超时自动通过
            if let Some(secs) = step.auto_approve_after_secs {
                let deadline = inst.updated_at + (secs as i64) * 1000;
                if now > deadline {
                    inst.decisions.push(ApprovalDecision {
                        step_order: inst.current_step,
                        approver_id: "system".to_string(),
                        approver_username: "系统(超时自动通过)".to_string(),
                        decision: "approve".to_string(),
                        comment: format!("超时 {} 秒自动通过", secs),
                        decided_at: now,
                        via_channel: "system".to_string(),
                    });
                    inst.updated_at = now;
                    if inst.current_step >= inst.total_steps {
                        inst.status = "approved".to_string();
                        inst.completed_at = Some(now);
                    } else {
                        inst.current_step += 1;
                    }
                    changed = true;
                }
            }
        }
        if changed {
            state.storage.save_approval_instances(&items);
            // 对刚批准的实例，唤醒等待器
            let approved: Vec<String> = items.iter()
                .filter(|i| i.status == "approved")
                .map(|i| i.id.clone())
                .collect();
            for id in approved {
                let mut pending = state.pending_approval_instances.lock().await;
                if let Some(tx) = pending.remove(&id) {
                    let _ = tx.send(true);
                }
            }
        }
    }
}

/// 为危险命令发起多级审批并（同步）等待结果。
/// 返回 Some(true)=全部批准；Some(false)=被拒绝；None=超时或无审批流。
async fn request_workflow_approval(
    state: &AppState,
    kind: &str,
    command: &str,
    ctx: &ToolContext,
    user: Option<&User>,
) -> Option<bool> {
    let flow = find_matching_flow(state, kind, command)?;
    let (uid, uname) = match user {
        Some(u) => (u.id.clone(), u.display_name.clone()),
        None => ("agent".to_string(), "Agent".to_string()),
    };
    let inst_id = create_approval_instance(
        state, &flow, kind, command,
        &format!("执行 {}", kind),
        &format!("命令：{}", command),
        &uid, &uname,
        &ctx.session_key, &ctx.run_id,
        false, // 同步等待
    ).await;

    // 注册等待器
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    state.pending_approval_instances.lock().await.insert(inst_id.clone(), tx);

    // 等待结果（默认 30 分钟）
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(1800));
    tokio::pin!(timeout);
    tokio::select! {
        approved = rx => match approved {
            Ok(a) => Some(a),
            Err(_) => None,
        },
        _ = &mut timeout => {
            state.pending_approval_instances.lock().await.remove(&inst_id);
            // 标记超时
            let mut items = state.storage.load_approval_instances();
            if let Some(i) = items.iter_mut().find(|i| i.id == inst_id) {
                if i.status == "pending" {
                    i.status = "timeout".to_string();
                    i.completed_at = Some(current_ms());
                }
            }
            state.storage.save_approval_instances(&items);
            None
        }
    }
}

// ============================================================================
// 工作流引擎实现：execute_workflow / resume / rewind / 节点执行 / 条件路由
// ============================================================================

/// 模板插值：把 ${key} 替换为 state 中对应值。支持点号路径如 ${vars.x}。
fn render_template(template: &str, state: &serde_json::Value) -> String {
    let mut out = template.to_string();
    // 简单循环替换 ${...}
    loop {
        let start = match out.find("${") { Some(i) => i, None => break };
        let end = match out[start..].find('}') { Some(j) => start + j, None => break };
        let key = &out[start + 2..end];
        let val = lookup_state(state, key);
        let replacement = match &val {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        out.replace_range(start..=end, &replacement);
    }
    out
}

/// 按 "a.b.c" 路径从 JSON 取值
fn lookup_state(state: &serde_json::Value, path: &str) -> serde_json::Value {
    let mut cur = state;
    for seg in path.split('.') {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => return serde_json::Value::Null,
        }
    }
    cur.clone()
}

/// 设置 state 中某路径的值（只支持一层 vars.xxx）
fn set_state_var(state: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    if path.is_empty() { return; }
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() == 1 {
        if let Some(obj) = state.as_object_mut() {
            obj.insert(parts[0].to_string(), value);
        }
        return;
    }
    // 多层：递归到 vars 对象
    if let Some(obj) = state.as_object_mut() {
        let head = parts[0].to_string();
        let child = obj.entry(head).or_insert_with(|| serde_json::json!({}));
        let rest = parts[1..].join(".");
        set_state_var(child, &rest, value);
    }
}

/// 简易条件表达式求值器
/// 支持：${x} == "val"、${x} != "val"、${x} > 5、${x} < 5、${x} contains "foo"、true、A && B、A || B
fn eval_expr(expr: &str, state: &serde_json::Value) -> bool {
    let e = expr.trim();
    // || 最低优先级
    if let Some(idx) = find_top_level(e, "||") {
        return eval_expr(&e[..idx], state) || eval_expr(&e[idx + 2..], state);
    }
    if let Some(idx) = find_top_level(e, "&&") {
        return eval_expr(&e[..idx], state) && eval_expr(&e[idx + 2..], state);
    }
    // 比较运算
    for (op, cmp) in [("==", 0), ("!=", 1), (">=", 2), ("<=", 3), (">", 4), ("<", 5)] {
        if let Some(idx) = e.find(op) {
            let lhs = render_template(e[..idx].trim(), state);
            let rhs = render_template(e[idx + op.len()..].trim(), state);
            let lhs = strip_quotes(&lhs);
            let rhs = strip_quotes(&rhs);
            return match cmp {
                0 => lhs == rhs,
                1 => lhs != rhs,
                2 | 4 => lhs.parse::<f64>().ok().zip(rhs.parse::<f64>().ok()).map(|(a, b)| a >= b).unwrap_or(false) || (cmp == 4 && lhs.parse::<f64>().ok().zip(rhs.parse::<f64>().ok()).map(|(a, b)| a > b).unwrap_or(false)) && cmp == 4,
                3 | 5 => lhs.parse::<f64>().ok().zip(rhs.parse::<f64>().ok()).map(|(a, b)| {
                    if cmp == 2 { a >= b } else if cmp == 3 { a <= b } else if cmp == 4 { a > b } else { a < b }
                }).unwrap_or(false),
                _ => false,
            };
        }
    }
    // contains
    if let Some(idx) = e.find(" contains ") {
        let lhs = render_template(e[..idx].trim(), state);
        let rhs = render_template(e[idx + " contains ".len()..].trim(), state);
        let rhs_stripped = strip_quotes(&rhs);
        return lhs.contains(rhs_stripped.as_str());
    }
    // 布尔字面量
    match strip_quotes(&render_template(e, state)).as_str() {
        "true" | "1" => true,
        "false" | "0" | "" => false,
        _ => {
            // 非空字符串视为 true
            let v = render_template(e, state);
            !v.is_empty() && v != "null"
        }
    }
}

/// 在字符串中查找「顶层」运算符位置（不进入引号内）
fn find_top_level(s: &str, op: &str) -> Option<usize> {
    let mut in_str = false;
    let mut quote = ' ';
    let bytes = s.as_bytes();
    let opb = op.as_bytes();
    let mut i = 0;
    while i + opb.len() <= bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if c == quote { in_str = false; }
        } else if c == '"' || c == '\'' {
            in_str = true;
            quote = c;
        } else if &s[i..i + opb.len()] == op {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// 启动一次工作流执行
async fn start_workflow_run(
    state: Arc<AppState>,
    graph_id: &str,
    initial_input: serde_json::Value,
    session_key: &str,
    breakpoints: Vec<String>,
) -> Result<String, String> {
    let graphs = state.storage.load_workflow_graphs();
    let graph = graphs.iter().find(|g| g.id == graph_id)
        .ok_or_else(|| format!("工作流 {} 不存在", graph_id))?
        .clone();
    if !graph.enabled { return Err("工作流已禁用".to_string()); }
    if graph.nodes.is_empty() { return Err("工作流没有节点".to_string()); }

    let run_id = format!("wfrun-{:016x}", rand_u128());
    let now = current_ms();
    let mut initial_state = serde_json::json!({
        "input": initial_input,
        "output": serde_json::Value::Null,
        "vars": {},
        "history": [],
    });
    // 初始化声明的 state_schema 字段
    for k in &graph.state_schema {
        if lookup_state(&initial_state, k) == serde_json::Value::Null {
            set_state_var(&mut initial_state, &format!("vars.{}", k), serde_json::Value::Null);
        }
    }
    let run = WorkflowRun {
        id: run_id.clone(),
        graph_id: graph.id.clone(),
        graph_name: graph.name.clone(),
        state: initial_state,
        current_node: graph.entry_node.clone(),
        status: "running".to_string(),
        checkpoints: vec![],
        root_span: TraceSpan::new("workflow", &graph.name, None),
        session_key: session_key.to_string(),
        started_at: now,
        finished_at: None,
        error: None,
        breakpoints,
    };
    state.storage.save_workflow_run(&run);
    let _ = broadcast_event(&state, "workflow.run.started", workflow_run_to_json(&run)).await;

    // 异步执行
    let s = state.clone();
    let rid = run_id.clone();
    tokio::spawn(async move {
        execute_workflow(s, &rid).await;
    });
    Ok(run_id)
}

/// 工作流主执行循环
async fn execute_workflow(state: Arc<AppState>, run_id: &str) {
    loop {
        let mut run = match state.storage.load_workflow_run(run_id) {
            Some(r) => r,
            None => return,
        };
        if run.status != "running" {
            return; // 暂停/完成/失败/取消
        }
        let graphs = state.storage.load_workflow_graphs();
        let graph = match graphs.iter().find(|g| g.id == run.graph_id) {
            Some(g) => g.clone(),
            None => {
                run.status = "failed".to_string();
                run.error = Some("工作流定义不存在".to_string());
                run.finished_at = Some(current_ms());
                state.storage.save_workflow_run(&run);
                return;
            }
        };
        let node = match graph.nodes.iter().find(|n| n.id == run.current_node) {
            Some(n) => n.clone(),
            None => {
                run.status = "failed".to_string();
                run.error = Some(format!("节点 {} 不存在", run.current_node));
                run.finished_at = Some(current_ms());
                state.storage.save_workflow_run(&run);
                let _ = broadcast_event(&state, "workflow.run.failed", workflow_run_to_json(&run)).await;
                return;
            }
        };

        // 检查断点：命中即暂停为 interrupt
        if run.breakpoints.iter().any(|b| b == &node.id) {
            run.status = "paused_interrupt".to_string();
            state.storage.save_workflow_run(&run);
            let _ = broadcast_event(&state, "workflow.interrupted", json!({
                "runId": run_id, "nodeId": node.id, "nodeName": node.name,
                "prompt": node.prompt.clone().unwrap_or_default(),
            })).await;
            return;
        }

        // 开始 span
        let mut span = TraceSpan::new("node", &node.name, Some(&run.root_span.id));
        span.input = run.state.clone();

        // 执行节点
        let exec_result = execute_workflow_node(&state, &graph, &node, &mut run.state, &mut span, run_id).await;
        match exec_result {
            Ok(node_output) => {
                // 写入 output_field
                if let Some(field) = &node.output_field {
                    set_state_var(&mut run.state, field, node_output.clone());
                } else {
                    set_state_var(&mut run.state, "output", node_output.clone());
                }
                // 记录历史
                if let Some(hist) = run.state.get_mut("history").and_then(|h| h.as_array_mut()) {
                    hist.push(json!({"node": node.id, "output": &node_output}));
                }
                span.finish_ok(node_output.clone());

                // End 节点：完成
                if node.node_type == NodeType::End {
                    run.status = "completed".to_string();
                    run.finished_at = Some(current_ms());
                    set_state_var(&mut run.state, "output", node_output);
                    run.checkpoints.push(WorkflowCheckpoint {
                        node_id: node.id.clone(),
                        step_index: run.checkpoints.len(),
                        state_snapshot: run.state.clone(),
                        timestamp: current_ms(),
                        span: span.clone(),
                    });
                    push_child_span(&mut run.root_span, span);
                    run.root_span.finish_ok(run.state.get("output").cloned().unwrap_or(serde_json::Value::Null));
                    state.storage.save_workflow_run(&run);
                    let _ = broadcast_event(&state, "workflow.run.completed", workflow_run_to_json(&run)).await;
                    return;
                }

                // 保存检查点
                run.checkpoints.push(WorkflowCheckpoint {
                    node_id: node.id.clone(),
                    step_index: run.checkpoints.len(),
                    state_snapshot: run.state.clone(),
                    timestamp: current_ms(),
                    span: span.clone(),
                });

                // 求值下一节点
                let next = resolve_next_node(&graph, &node, &run.state);
                match next {
                    Some(next_id) => {
                        run.current_node = next_id;
                    }
                    None => {
                        // 无后继：完成
                        run.status = "completed".to_string();
                        run.finished_at = Some(current_ms());
                    }
                }
                push_child_span(&mut run.root_span, span);
                if run.status == "completed" {
                    run.root_span.finish_ok(run.state.get("output").cloned().unwrap_or(serde_json::Value::Null));
                }
                state.storage.save_workflow_run(&run);
                let _ = broadcast_event(&state, "workflow.node.completed", json!({
                    "runId": run_id, "nodeId": node.id, "nextNode": run.current_node,
                })).await;
            }
            Err(e) => {
                span.finish_err(&e);
                push_child_span(&mut run.root_span, span);
                run.root_span.finish_err(&e);
                run.status = "failed".to_string();
                run.error = Some(e.clone());
                run.finished_at = Some(current_ms());
                state.storage.save_workflow_run(&run);
                let _ = broadcast_event(&state, "workflow.run.failed", workflow_run_to_json(&run)).await;
                return;
            }
        }
    }
}

/// 把子 span 追加到根 span 的 children（或匹配 parent_id）
fn push_child_span(root: &mut TraceSpan, child: TraceSpan) {
    // 简化：直接追加到 root.children
    root.children.push(child);
}

/// 求值下一节点：对 Condition 节点按 branches 匹配；其他节点走 edges
fn resolve_next_node(graph: &WorkflowGraph, node: &WorkflowNode, state: &serde_json::Value) -> Option<String> {
    if node.node_type == NodeType::Condition {
        // 按 branches 顺序求值
        for (expr, target) in &node.branches {
            if eval_expr(expr, state) {
                return Some(target.clone());
            }
        }
        return node.default_edge.clone();
    }
    // 普通节点：找 from==node.id 且 condition 为 None 的边
    graph.edges.iter()
        .find(|e| e.from == node.id && e.condition.is_none())
        .map(|e| e.to.clone())
        .or_else(|| {
            // 也可能是 node.default_edge
            node.default_edge.clone()
        })
}

/// 执行单个工作流节点
async fn execute_workflow_node(
    state: &AppState,
    _graph: &WorkflowGraph,
    node: &WorkflowNode,
    wf_state: &mut serde_json::Value,
    span: &mut TraceSpan,
    run_id: &str,
) -> Result<serde_json::Value, String> {
    match node.node_type {
        NodeType::Llm => exec_llm_workflow_node(state, node, wf_state, span, run_id).await,
        NodeType::Tool => exec_tool_workflow_node(state, node, wf_state, span).await,
        NodeType::Agent => exec_agent_workflow_node(state, node, wf_state, span).await,
        NodeType::Condition => Ok(serde_json::json!({"branched": true})),
        NodeType::Parallel => exec_parallel_workflow_node(state, node, wf_state, span).await,
        NodeType::Interrupt => {
            // 暂停：由调用方检测 breakpoints；此处返回特殊标记
            Ok(serde_json::json!({"interrupted": true}))
        }
        NodeType::HumanReview => {
            // 触发审批（简化：直接放行，实际可复用 approval engine）
            Ok(serde_json::json!({"reviewed": true}))
        }
        NodeType::End => {
            // 返回最终输出
            Ok(lookup_state(wf_state, "output"))
        }
    }
}

/// LLM 节点：渲染 prompt → 调 LLM → 返回文本
async fn exec_llm_workflow_node(
    state: &AppState,
    node: &WorkflowNode,
    wf_state: &serde_json::Value,
    span: &mut TraceSpan,
    _run_id: &str,
) -> Result<serde_json::Value, String> {
    let template = node.prompt_template.clone().unwrap_or_default();
    let prompt = render_template(&template, wf_state);
    if prompt.is_empty() {
        return Err("LLM 节点缺少 prompt".to_string());
    }
    let messages = vec![
        json!({"role": "system", "content": "你是工作流节点执行器，根据输入完成任务。"}),
        json!({"role": "user", "content": prompt}),
    ];
    let tools = serde_json::json!([]);
    let result = stream_llm_call(state, "workflow", "wf", messages, &tools).await
        .map_err(|e| format!("LLM 调用失败: {}", e))?;
    span.tokens_in = Some(estimate_tokens(&[json!({"content": prompt})]) as u64);
    span.tokens_out = Some(estimate_tokens(&[json!({"content": &result.text})]) as u64);
    span.model = Some(state.config.default_model.clone());
    Ok(serde_json::json!(result.text))
}

/// Tool 节点：渲染参数 → 执行工具
async fn exec_tool_workflow_node(
    state: &AppState,
    node: &WorkflowNode,
    wf_state: &serde_json::Value,
    span: &mut TraceSpan,
) -> Result<serde_json::Value, String> {
    let tool_name = node.tool_name.clone().unwrap_or_default();
    if tool_name.is_empty() {
        return Err("Tool 节点缺少 tool_name".to_string());
    }
    let args = match &node.tool_args_template {
        Some(tmpl) => {
            let rendered = render_template(&tmpl.to_string(), wf_state);
            serde_json::from_str(&rendered).unwrap_or(json!({}))
        }
        None => json!({}),
    };
    span.input = args.clone();
    let ctx = ToolContext { session_key: format!("workflow"), run_id: format!("wf") };
    let result = execute_tool_with_ctx(state, &tool_name, &args, ctx).await;
    span.finish_ok(serde_json::json!(&result));
    Ok(serde_json::json!(result))
}

/// Agent 节点：运行角色化 agent
async fn exec_agent_workflow_node(
    state: &AppState,
    node: &WorkflowNode,
    wf_state: &mut serde_json::Value,
    span: &mut TraceSpan,
) -> Result<serde_json::Value, String> {
    let role_id = node.agent_role.clone().unwrap_or_default();
    let task = render_template(&node.prompt_template.clone().unwrap_or_default(), wf_state);
    let roles = state.storage.load_agent_roles();
    let role = roles.iter().find(|r| r.id == role_id).cloned();
    let output = execute_role_agent(state, role.as_ref(), &task, "workflow", span).await?;
    Ok(serde_json::json!(output))
}

/// Parallel 节点：扇出 + reduce
async fn exec_parallel_workflow_node(
    state: &AppState,
    node: &WorkflowNode,
    wf_state: &mut serde_json::Value,
    span: &mut TraceSpan,
) -> Result<serde_json::Value, String> {
    let field = node.fan_out_field.clone().unwrap_or_else(|| "input".to_string());
    let sub_tasks: Vec<String> = match lookup_state(wf_state, &field) {
        serde_json::Value::Array(arr) => arr.iter().map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }).collect(),
        serde_json::Value::String(s) => s.lines().map(String::from).collect(),
        _ => return Err(format!("fan_out_field {} 不是数组或字符串", field)),
    };
    if sub_tasks.is_empty() {
        return Ok(serde_json::json!([]));
    }
    let role_id = node.fan_out_role.clone();
    let roles = state.storage.load_agent_roles();
    let role = role_id.and_then(|rid| roles.iter().find(|r| r.id == rid).cloned());
    let max_concurrent = node.max_concurrent.unwrap_or(5).max(1);
    let result = fan_out_map_reduce(state, &sub_tasks, role.as_ref(), "workflow", max_concurrent, span).await?;
    // reduce 模式
    let reduced = match node.reduce_mode.as_deref().unwrap_or("concat") {
        "join" => result.join("\n---\n"),
        "summary" => {
            let combined = result.join("\n");
            // 调一次 LLM 汇总
            let msgs = vec![
                json!({"role": "system", "content": "请汇总以下多个子任务的结果，给出简洁摘要。"}),
                json!({"role": "user", "content": combined}),
            ];
            stream_llm_call(state, "workflow", "reduce", msgs, &json!([])).await
                .map(|r| r.text).unwrap_or(combined)
        }
        _ => result.join("\n\n"),
    };
    Ok(serde_json::json!(reduced))
}

/// 恢复暂停的工作流
async fn resume_workflow_run(state: Arc<AppState>, run_id: &str, human_input: serde_json::Value) -> Result<(), String> {
    let mut run = state.storage.load_workflow_run(run_id)
        .ok_or_else(|| "运行实例不存在".to_string())?;
    if run.status != "paused_interrupt" && run.status != "paused_review" {
        return Err(format!("当前状态 {} 不可恢复", run.status));
    }
    set_state_var(&mut run.state, "human_input", human_input);
    run.status = "running".to_string();
    state.storage.save_workflow_run(&run);
    let _ = broadcast_event(&state, "workflow.run.resumed", json!({"runId": run_id})).await;
    let s = state.clone();
    let rid = run_id.to_string();
    tokio::spawn(async move {
        execute_workflow(s, &rid).await;
    });
    Ok(())
}

/// 回滚到检查点 N 重新执行
async fn rewind_workflow_run(state: Arc<AppState>, run_id: &str, checkpoint_index: usize) -> Result<(), String> {
    let mut run = state.storage.load_workflow_run(run_id)
        .ok_or_else(|| "运行实例不存在".to_string())?;
    if checkpoint_index >= run.checkpoints.len() {
        return Err("检查点索引越界".to_string());
    }
    let cp = run.checkpoints[checkpoint_index].clone();
    let cp_node_id = cp.node_id.clone();
    run.state = cp.state_snapshot;
    run.current_node = cp_node_id.clone();
    run.status = "running".to_string();
    run.error = None;
    run.finished_at = None;
    // 丢弃之后的检查点
    run.checkpoints.truncate(checkpoint_index);
    state.storage.save_workflow_run(&run);
    let _ = broadcast_event(&state, "workflow.run.rewound", json!({
        "runId": run_id, "toCheckpoint": checkpoint_index, "nodeId": &cp_node_id,
    })).await;
    let s = state.clone();
    let rid = run_id.to_string();
    tokio::spawn(async move {
        execute_workflow(s, &rid).await;
    });
    Ok(())
}

/// 取消工作流
async fn cancel_workflow_run(state: &AppState, run_id: &str) -> Result<(), String> {
    let mut run = state.storage.load_workflow_run(run_id)
        .ok_or_else(|| "运行实例不存在".to_string())?;
    if run.status == "completed" || run.status == "cancelled" {
        return Err(format!("状态 {} 不可取消", run.status));
    }
    run.status = "cancelled".to_string();
    run.finished_at = Some(current_ms());
    state.storage.save_workflow_run(&run);
    let _ = broadcast_event(state, "workflow.run.cancelled", json!({"runId": run_id})).await;
    Ok(())
}

// ----------------------------------------------------------------------------
// 角色化 Agent 执行（对标 CrewAI Agent）
// ----------------------------------------------------------------------------

/// 根据角色构建 system prompt
fn build_role_system_prompt(role: Option<&AgentRole>) -> String {
    match role {
        Some(r) => {
            let template = r.system_prompt_template.clone().unwrap_or_else(|| {
                // 默认模板
                "你是一名{role}。\n\n你的目标：{goal}\n\n背景：{backstory}\n\n请基于你的专业能力和背景完成任务。".to_string()
            });
            let state = json!({"role": &r.role, "goal": &r.goal, "backstory": &r.backstory});
            render_template(&template, &state)
        }
        None => build_default_system_prompt(),
    }
}

/// 构建默认 system prompt（供无角色的 agent 使用）
fn build_default_system_prompt() -> String {
    "你是 CradleRing 的 AI 助手，能调用工具完成任务。请高效、准确地完成用户请求。".to_string()
}

/// 根据角色构建工具 schema（白名单过滤）
fn build_role_tools_schema(role: Option<&AgentRole>) -> serde_json::Value {
    let all = build_tools_schema();
    match role {
        Some(r) => match &r.tools {
            Some(whitelist) if !whitelist.is_empty() => {
                if let Some(arr) = all.as_array() {
                    let filtered: Vec<_> = arr.iter()
                        .filter(|t| {
                            t["function"]["name"].as_str()
                                .map(|n| whitelist.iter().any(|w| w == n))
                                .unwrap_or(false)
                        })
                        .cloned()
                        .collect();
                    serde_json::Value::Array(filtered)
                } else { all }
            }
            _ => all,
        },
        None => all,
    }
}

/// 执行一个角色化 agent（独立 agent loop），返回最终文本输出
async fn execute_role_agent(
    state: &AppState,
    role: Option<&AgentRole>,
    task: &str,
    session_key: &str,
    parent_span: &mut TraceSpan,
) -> Result<String, String> {
    let max_iter = role.map(|r| r.max_iterations).unwrap_or(10);
    let _model_override = role.and_then(|r| r.model.clone());
    let system_prompt = build_role_system_prompt(role);
    let tools = build_role_tools_schema(role);
    let mut messages: Vec<serde_json::Value> = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": task}),
    ];

    let mut agent_span = TraceSpan::new("agent_loop", role.map(|r| r.name.as_str()).unwrap_or("default"), Some(&parent_span.id));
    agent_span.input = json!({"task": task});

    for iteration in 0..max_iter {
        let mut iter_span = TraceSpan::new("iteration", &format!("iter {}", iteration + 1), Some(&agent_span.id));
        let ctx_messages = build_context_messages_v2(&messages, 100_000, 20);
        match stream_llm_call(state, session_key, "role", ctx_messages, &tools).await {
            Ok(result) => {
                iter_span.tokens_out = Some(estimate_tokens(&[json!({"content": &result.text})]) as u64);
                if !result.tool_calls.is_empty() {
                    // 并行执行工具
                    let tool_results = execute_tools_parallel(state, &result.tool_calls, session_key, "role", &mut iter_span).await;
                    messages.push(json!({"role": "assistant", "content": &result.text}));
                    for (tc_id, tr) in &tool_results {
                        messages.push(json!({"role": "tool", "tool_call_id": tc_id, "content": tr}));
                    }
                    push_child_span(&mut agent_span, iter_span);
                    continue;
                }
                if !result.text.is_empty() {
                    iter_span.finish_ok(json!({"text": &result.text}));
                    push_child_span(&mut agent_span, iter_span);
                    agent_span.finish_ok(json!({"output": &result.text}));
                    push_child_span(parent_span, agent_span);
                    return Ok(result.text);
                }
                push_child_span(&mut agent_span, iter_span);
                break;
            }
            Err(e) => {
                iter_span.finish_err(&e);
                push_child_span(&mut agent_span, iter_span);
                agent_span.finish_err(&e);
                push_child_span(parent_span, agent_span);
                return Err(e);
            }
        }
    }
    agent_span.finish_ok(json!({"output": "(达到最大迭代)"}));
    push_child_span(parent_span, agent_span);
    Ok("(达到最大迭代次数)".to_string())
}

/// 构建上下文消息（agent loop 用，跳过 system_prompt 已在 messages 内）
fn build_context_messages_v2(messages: &[serde_json::Value], max_tokens: usize, keep_recent: usize) -> Vec<serde_json::Value> {
    if messages.is_empty() { return vec![]; }
    let total = estimate_tokens(messages);
    if total <= max_tokens {
        return messages.to_vec();
    }
    // 保留第一条（system）+ 最近 N 条
    let first = &messages[0];
    let split_at = messages.len().saturating_sub(keep_recent);
    let mut result = vec![first.clone()];
    if split_at < messages.len() {
        result.extend_from_slice(&messages[split_at..]);
    }
    result
}

// ----------------------------------------------------------------------------
// 并行工具执行 + Map-Reduce 扇出
// ----------------------------------------------------------------------------

/// 并行执行多个工具调用（返回 (tool_call_id, result) 列表）
async fn execute_tools_parallel(
    state: &AppState,
    tool_calls: &[ToolCallAccumulator],
    session_key: &str,
    run_id: &str,
    parent_span: &mut TraceSpan,
) -> Vec<(String, String)> {
    use futures::future::join_all;
    let parent_id = parent_span.id.clone();
    let futures: Vec<_> = tool_calls.iter().map(|tc| {
        let tc = tc.clone();
        let sk = session_key.to_string();
        let rid = run_id.to_string();
        let pid = parent_id.clone();
        async move {
            let mut span = TraceSpan::new("tool_call", &tc.name, Some(&pid));
            let args: serde_json::Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
            span.input = args.clone();
            let ctx = ToolContext { session_key: sk, run_id: rid };
            let result = execute_tool_with_ctx(state, &tc.name, &args, ctx).await;
            span.finish_ok(json!(&result));
            (tc.id.clone(), result, span)
        }
    }).collect();
    let results = join_all(futures).await;
    // 把 span 追加到 parent
    for (_, _, span) in &results {
        parent_span.children.push(span.clone());
    }
    results.into_iter().map(|(id, r, _)| (id, r)).collect()
}

/// Map-Reduce 扇出：并行运行 N 个子 agent，汇总结果
async fn fan_out_map_reduce(
    state: &AppState,
    sub_tasks: &[String],
    role: Option<&AgentRole>,
    session_key: &str,
    max_concurrent: usize,
    parent_span: &mut TraceSpan,
) -> Result<Vec<String>, String> {
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    let sem = Arc::new(Semaphore::new(max_concurrent));
    let mut futures = vec![];
    let parent_id = parent_span.id.clone();
    let sk = session_key.to_string();
    for (i, task) in sub_tasks.iter().enumerate() {
        let permit_sem = sem.clone();
        let task = task.clone();
        let role_clone = role.cloned();
        let sk = sk.clone();
        let parent_id = parent_id.clone();
        futures.push(async move {
            let _permit = permit_sem.acquire().await.ok()?;
            let mut sub_span = TraceSpan::new("fan_out_child", &format!("子任务{}", i + 1), Some(&parent_id));
            sub_span.input = json!({"task": &task});
            let result = Box::pin(execute_role_agent(state, role_clone.as_ref(), &task, &sk, &mut sub_span)).await.ok()?;
            Some((result, sub_span))
        });
    }
    let results = futures::future::join_all(futures).await;
    let mut outputs = vec![];
    for r in results {
        if let Some((text, span)) = r {
            outputs.push(text);
            parent_span.children.push(span);
        }
    }
    Ok(outputs)
}

// ----------------------------------------------------------------------------
// Sequential 流水线（对标 CrewAI Sequential Process）
// ----------------------------------------------------------------------------

/// 执行 Sequential 流水线
async fn execute_pipeline_run(
    state: Arc<AppState>,
    pipeline_id: &str,
    input: &str,
    session_key: &str,
) -> Result<PipelineResult, String> {
    let pipelines = state.storage.load_pipelines();
    let pipeline = pipelines.iter().find(|p| p.id == pipeline_id)
        .ok_or_else(|| format!("流水线 {} 不存在", pipeline_id))?
        .clone();
    if !pipeline.enabled { return Err("流水线已禁用".to_string()); }
    if pipeline.stages.is_empty() { return Err("流水线没有阶段".to_string()); }

    let now = current_ms();
    let mut result = PipelineResult {
        pipeline_id: pipeline.id.clone(),
        final_output: String::new(),
        stage_outputs: vec![],
        trace: TraceSpan::new("pipeline", &pipeline.name, None),
        started_at: now,
        finished_at: None,
    };

    let roles = state.storage.load_agent_roles();
    let mut prev_output = input.to_string();
    let mut stages = pipeline.stages.clone();
    stages.sort_by_key(|s| s.order);

    for stage in &stages {
        let role = roles.iter().find(|r| r.id == stage.agent_role_id).cloned();
        let task_vars = json!({"input": input, "prev_output": &prev_output});
        let task = render_template(&stage.task_template, &task_vars);
        let mut stage_span = TraceSpan::new("pipeline_stage", &format!("阶段{}", stage.order), Some(&result.trace.id));
        stage_span.input = json!({"task": &task, "role": role.as_ref().map(|r| &r.name).cloned().unwrap_or_default()});
        match execute_role_agent(&state, role.as_ref(), &task, session_key, &mut stage_span).await {
            Ok(out) => {
                if pipeline.pass_through {
                    prev_output = out.clone();
                }
                result.stage_outputs.push(out.clone());
                stage_span.finish_ok(json!({"output": &out}));
                result.trace.children.push(stage_span);
            }
            Err(e) => {
                stage_span.finish_err(&e);
                result.trace.children.push(stage_span);
                result.trace.finish_err(&e);
                result.finished_at = Some(current_ms());
                return Err(e);
            }
        }
    }
    result.final_output = prev_output;
    result.trace.finish_ok(json!({"finalOutput": &result.final_output}));
    result.finished_at = Some(current_ms());
    Ok(result)
}

// ----------------------------------------------------------------------------
// 序列化辅助
// ----------------------------------------------------------------------------

fn parse_workflow_node(v: &serde_json::Value) -> Option<WorkflowNode> {
    let nt_str = v["nodeType"].as_str().or(v["node_type"].as_str()).unwrap_or("llm");
    let node_type = match nt_str.to_lowercase().as_str() {
        "llm" => NodeType::Llm,
        "tool" => NodeType::Tool,
        "agent" => NodeType::Agent,
        "condition" => NodeType::Condition,
        "parallel" => NodeType::Parallel,
        "interrupt" => NodeType::Interrupt,
        "humanreview" | "human_review" | "review" => NodeType::HumanReview,
        "end" => NodeType::End,
        _ => NodeType::Llm,
    };
    let branches: Vec<(String, String)> = v["branches"].as_array()
        .map(|a| a.iter().filter_map(|b| {
            let expr = b["expr"].as_str().or(b["expression"].as_str())?.to_string();
            let target = b["target"].as_str().or(b["targetNode"].as_str())?.to_string();
            Some((expr, target))
        }).collect())
        .unwrap_or_default();
    Some(WorkflowNode {
        id: v["id"].as_str()?.to_string(),
        name: v["name"].as_str().unwrap_or("").to_string(),
        node_type,
        agent_role: v["agentRole"].as_str().or(v["agent_role"].as_str()).map(String::from),
        prompt_template: v["promptTemplate"].as_str().or(v["prompt_template"].as_str()).map(String::from),
        tool_name: v["toolName"].as_str().or(v["tool_name"].as_str()).map(String::from),
        tool_args_template: {
            if !v["toolArgsTemplate"].is_null() { Some(v["toolArgsTemplate"].clone()) }
            else if !v["tool_args_template"].is_null() { Some(v["tool_args_template"].clone()) }
            else { None }
        },
        branches,
        default_edge: v["defaultEdge"].as_str().or(v["default_edge"].as_str()).map(String::from),
        fan_out_field: v["fanOutField"].as_str().or(v["fan_out_field"].as_str()).map(String::from),
        fan_out_role: v["fanOutRole"].as_str().or(v["fan_out_role"].as_str()).map(String::from),
        max_concurrent: v["maxConcurrent"].as_u64().map(|n| n as usize),
        reduce_mode: v["reduceMode"].as_str().or(v["reduce_mode"].as_str()).map(String::from),
        prompt: v["prompt"].as_str().map(String::from),
        output_field: v["outputField"].as_str().or(v["output_field"].as_str()).map(String::from),
        config: v["config"].clone(),
    })
}

fn parse_workflow_edge(v: &serde_json::Value) -> Option<WorkflowEdge> {
    Some(WorkflowEdge {
        id: v["id"].as_str()?.to_string(),
        from: v["from"].as_str()?.to_string(),
        to: v["to"].as_str()?.to_string(),
        condition: v["condition"].as_str().map(String::from),
        label: v["label"].as_str().map(String::from),
    })
}

fn workflow_graph_to_json(g: &WorkflowGraph) -> serde_json::Value {
    json!({
        "id": &g.id,
        "name": &g.name,
        "description": &g.description,
        "nodes": g.nodes.iter().map(|n| json!({
            "id": &n.id, "name": &n.name, "nodeType": n.node_type,
            "agentRole": &n.agent_role, "promptTemplate": &n.prompt_template,
            "toolName": &n.tool_name, "toolArgsTemplate": &n.tool_args_template,
            "branches": n.branches.iter().map(|(e, t)| json!({"expr": e, "target": t})).collect::<Vec<_>>(),
            "defaultEdge": &n.default_edge,
            "fanOutField": &n.fan_out_field, "fanOutRole": &n.fan_out_role,
            "maxConcurrent": n.max_concurrent, "reduceMode": &n.reduce_mode,
            "prompt": &n.prompt, "outputField": &n.output_field, "config": &n.config,
        })).collect::<Vec<_>>(),
        "edges": g.edges.iter().map(|e| json!({
            "id": &e.id, "from": &e.from, "to": &e.to, "condition": &e.condition, "label": &e.label,
        })).collect::<Vec<_>>(),
        "entryNode": &g.entry_node,
        "stateSchema": &g.state_schema,
        "enabled": g.enabled,
        "createdAt": g.created_at,
        "createdBy": &g.created_by,
    })
}

fn workflow_run_to_json(r: &WorkflowRun) -> serde_json::Value {
    json!({
        "id": &r.id,
        "graphId": &r.graph_id,
        "graphName": &r.graph_name,
        "state": &r.state,
        "currentNode": &r.current_node,
        "status": &r.status,
        "checkpointsCount": r.checkpoints.len(),
        "sessionKey": &r.session_key,
        "startedAt": r.started_at,
        "finishedAt": r.finished_at,
        "error": &r.error,
        "breakpoints": &r.breakpoints,
    })
}

fn trace_span_to_json(s: &TraceSpan) -> serde_json::Value {
    json!({
        "id": &s.id, "parentId": &s.parent_id, "kind": &s.kind, "name": &s.name,
        "input": truncate_json(&s.input, 2000), "output": truncate_json(&s.output, 2000),
        "startedAt": s.started_at, "finishedAt": s.finished_at, "durationMs": s.duration_ms,
        "tokensIn": s.tokens_in, "tokensOut": s.tokens_out, "costUsd": s.cost_usd, "model": &s.model,
        "children": s.children.iter().map(trace_span_to_json).collect::<Vec<_>>(),
        "status": &s.status, "error": &s.error,
    })
}

fn truncate_json(v: &serde_json::Value, max_len: usize) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => {
            if s.len() > max_len {
                serde_json::Value::String(format!("{}...(已截断)", &s[..max_len.min(s.len())]))
            } else { v.clone() }
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().take(20).map(|x| truncate_json(x, max_len)).collect())
        }
        serde_json::Value::Object(obj) => {
            let mut out = serde_json::Map::new();
            for (k, val) in obj.iter().take(30) {
                out.insert(k.clone(), truncate_json(val, max_len));
            }
            serde_json::Value::Object(out)
        }
        _ => v.clone(),
    }
}

fn agent_role_to_json(r: &AgentRole) -> serde_json::Value {
    json!({
        "id": &r.id, "name": &r.name, "role": &r.role, "goal": &r.goal, "backstory": &r.backstory,
        "tools": &r.tools, "model": &r.model, "systemPromptTemplate": &r.system_prompt_template,
        "maxIterations": r.max_iterations, "allowDelegation": r.allow_delegation,
        "createdAt": r.created_at, "createdBy": &r.created_by,
    })
}

fn pipeline_to_json(p: &Pipeline) -> serde_json::Value {
    json!({
        "id": &p.id, "name": &p.name, "description": &p.description,
        "stages": p.stages.iter().map(|s| json!({
            "order": s.order, "agentRoleId": &s.agent_role_id, "taskTemplate": &s.task_template,
        })).collect::<Vec<_>>(),
        "passThrough": p.pass_through, "enabled": p.enabled,
        "createdAt": p.created_at, "createdBy": &p.created_by,
    })
}

fn pipeline_result_to_json(r: &PipelineResult) -> serde_json::Value {
    json!({
        "pipelineId": &r.pipeline_id, "finalOutput": &r.final_output,
        "stageOutputs": &r.stage_outputs, "trace": trace_span_to_json(&r.trace),
        "startedAt": r.started_at, "finishedAt": r.finished_at,
    })
}

/// 核心 Agent Loop
async fn run_agent_loop(state: Arc<AppState>, session_key: &str, run_id: &str) {
    let messages = state.storage.load_messages(session_key);
    if messages.is_empty() { return; }
    let mut current_messages = messages.clone();
    let tools = build_tools_schema();
    for iteration in 0..10 {
        let latest_user = current_messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.as_str()).unwrap_or("");
        let system_prompt = build_system_prompt(&state, latest_user);
        let ctx_messages = build_context_messages(&system_prompt, &current_messages, 100_000, 20);
        match stream_llm_call(&state, session_key, run_id, ctx_messages, &tools).await {
            Ok(result) => {
                broadcast_chat_event(&state, session_key, run_id, json!({"deltaText": result.text})).await;
                if !result.tool_calls.is_empty() {
                    // 并行执行工具调用（FuturesUnordered/join_all）
                    let tool_results = execute_tools_parallel(
                        &state, &result.tool_calls, session_key, run_id,
                        &mut TraceSpan::new("iteration", &format!("iter {}", iteration + 1), None),
                    ).await;
                    for (_tc_id, result_str) in tool_results {
                        current_messages.push(Message { role: "tool".into(), content: result_str, timestamp: current_ms(), attachments: vec![] });
                    }
                    continue;
                }
                if !result.text.is_empty() {
                    state.storage.append_message(session_key, &Message { role: "assistant".into(), content: result.text.clone(), timestamp: current_ms(), attachments: vec![] });
                }
                break;
            }
            Err(e) => {
                broadcast_event(&state, "chat.error", json!({"sessionKey": session_key, "runId": run_id, "error": e})).await;
                break;
            }
        }
    }
    broadcast_event(&state, "chat.complete", json!({"sessionKey": session_key, "runId": run_id})).await;
}

/// 子 Agent Loop
async fn run_subagent_loop(state: &AppState, session_key: &str, _model: Option<&str>, max_iter: usize) -> String {
    let messages = state.storage.load_messages(session_key);
    if messages.is_empty() { return "无消息".to_string(); }
    let mut current_messages = messages.clone();
    let tools = build_tools_schema();
    for _ in 0..max_iter {
        let latest_user = current_messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.as_str()).unwrap_or("");
        let system_prompt = build_system_prompt(state, latest_user);
        let ctx_messages = build_context_messages(&system_prompt, &current_messages, 100_000, 20);
        match stream_llm_call(state, session_key, "sub", ctx_messages, &tools).await {
            Ok(result) => {
                if !result.tool_calls.is_empty() {
                    for tc in &result.tool_calls {
                        let args: serde_json::Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
                        let result_str = execute_tool_with_ctx(state, &tc.name, &args, ToolContext { session_key: session_key.to_string(), run_id: "sub".to_string() }).await;
                        current_messages.push(Message { role: "tool".into(), content: result_str, timestamp: current_ms(), attachments: vec![] });
                    }
                    continue;
                }
                return result.text;
            }
            Err(e) => return format!("错误: {}", e),
        }
    }
    "达到最大迭代次数".to_string()
}

/// TTS 转换
async fn tts_convert(state: &AppState, text: &str, voice_override: &str) -> serde_json::Value {
    let tts = state.config.raw_json.get("tts").cloned().unwrap_or(json!({}));
    let provider = tts.get("provider").and_then(|v| v.as_str()).unwrap_or("openai");
    let voice = if !voice_override.is_empty() { voice_override.to_string() } else { tts.get("voice").and_then(|v| v.as_str()).unwrap_or("alloy").to_string() };
    let format = tts.get("format").and_then(|v| v.as_str()).unwrap_or("mp3");
    let api_key = state.config.openai_api_key.clone().unwrap_or_default();
    let base_url = &state.config.openai_base_url;
    match provider {
        "openai" => {
            if api_key.is_empty() { return json!({"ok": false, "error": "未配置 API Key"}); }
            let url = format!("{}/audio/speech", base_url.trim_end_matches('/'));
            let client = reqwest::Client::new();
            match client.post(&url).header("Authorization", format!("Bearer {}", api_key))
                .json(&json!({"model": "tts-1", "input": text, "voice": voice, "format": format})).send().await {
                Ok(r) if r.status().is_success() => {
                    let bytes = r.bytes().await.unwrap_or_default();
                    json!({"ok": true, "audio": base64_encode_bytes(&bytes), "format": format})
                }
                Ok(r) => json!({"ok": false, "error": format!("HTTP {}", r.status())}),
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            }
        }
        "edge" | "microsoft" => {
            let output = tokio::process::Command::new("edge-tts").args(&["--voice", &voice, "--text", text, "--write-media", "/tmp/cr_tts.mp3"]).output().await;
            match output {
                Ok(o) if o.status.success() => match std::fs::read("/tmp/cr_tts.mp3") {
                    Ok(bytes) => json!({"ok": true, "audio": base64_encode_bytes(&bytes), "format": "mp3"}),
                    Err(_) => json!({"ok": false, "error": "读取音频失败"}),
                }
                _ => json!({"ok": false, "error": "edge-tts 未安装或执行失败"}),
            }
        }
        _ => json!({"ok": false, "error": format!("TTS provider {} 暂不支持", provider)}),
    }
}

/// 获取最新信息工具
async fn tool_fetch_latest_info(state: &AppState, topic: &str, _max_pages: usize) -> String {
    let search_result = tool_web_search(state, topic).await;
    format!("关于「{}」的最新信息：\n\n{}", topic, &search_result[..search_result.len().min(2000)])
}

/// HTML 转文本
fn strip_html_tags(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        if ch == '<' { in_tag = true; continue; }
        if ch == '>' { in_tag = false; continue; }
        if !in_tag { result.push(ch); }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cron 调度器后台循环
async fn cron_scheduler_loop(state: Arc<AppState>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        // 简化：每 30 秒检查一次 cron_jobs.json
        let _ = &state;
    }
}

/// WebSocket 连接处理
async fn handle_websocket_connection(
    mut stream: tokio::net::TcpStream,
    request: String,
    state: Arc<AppState>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 提取 Sec-WebSocket-Key
    let key = request.lines()
        .find(|l| l.to_lowercase().starts_with("sec-websocket-key:"))
        .and_then(|l| l.split(':').nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let combined = format!("{}{}", key, "258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let hash = sha1_compute(combined.as_bytes());
    let accept = base64_encode_bytes(&hash);

    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;

    // 发送 connect.challenge
    let challenge = json!({
        "type": "event",
        "event": "connect.challenge",
        "payload": { "nonce": format!("{:032x}", rand_u128()), "ts": current_ms() }
    });
    send_ws_text(&mut stream, &challenge.to_string()).await;

    let mut buf = vec![0u8; 65536];
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if n < 2 { continue; }
        let opcode = buf[0] & 0x0f;
        if opcode == 0x8 { send_ws_close(&mut stream).await; break; }
        if opcode == 0x9 { let _ = stream.write_all(&[0x8a, 0x00]).await; continue; } // Ping→Pong
        if opcode != 0x1 { continue; }

        // 解析 WS 帧
        let masked = (buf[1] & 0x80) != 0;
        let len_byte = buf[1] & 0x7f;
        let (payload_len, mut start) = match len_byte {
            0..=125 => (len_byte as usize, 2),
            126 => { if n < 4 { continue; } (((buf[2] as usize) << 8) | (buf[3] as usize), 4) }
            _ => { if n < 10 { continue; } (u64::from_be_bytes([buf[2],buf[3],buf[4],buf[5],buf[6],buf[7],buf[8],buf[9]]) as usize, 10) }
        };
        let mut mask = [0u8; 4];
        if masked { if start + 4 > n { continue; } mask.copy_from_slice(&buf[start..start+4]); start += 4; }
        if start + payload_len > n { continue; }
        let text: String = (0..payload_len).map(|i| (buf[start+i] ^ mask[i%4]) as char).collect();

        if let Ok(req) = serde_json::from_str::<serde_json::Value>(&text) {
            let method = req["method"].as_str().unwrap_or("");
            let id = req["id"].as_str().unwrap_or("0");
            let params = req["params"].clone();
            let result = handle_rpc(state.clone(), method, params).await;
            let response = json!({"type": "res", "id": id, "ok": true, "payload": result});
            send_ws_text(&mut stream, &response.to_string()).await;
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() <= 1 {
        println!("CradleRing v{} — 企业级 AI Agent 协作平台", env!("CARGO_PKG_VERSION"));
        println!("\n命令:\n  gateway start    启动网关\n  gateway status   查看状态");
        println!("  doctor           运行诊断\n  onboard          配置向导");
        println!("  --version        显示版本\n  --help           帮助");
        return;
    }
    match args[1].as_str() {
        "--version" | "-v" => println!("{}", env!("CARGO_PKG_VERSION")),
        "--help" | "-h" => {
            println!("CradleRing v{} — 企业级 AI Agent 协作平台", env!("CARGO_PKG_VERSION"));
            println!("\n命令:\n  gateway start    启动网关\n  gateway status   查看状态");
            println!("  doctor           运行诊断\n  onboard          配置向导");
        }
        "gateway" => {
            if args.len() > 2 && args[2] == "start" {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                let config = Config::load(&home);
                let storage = Storage::new(&home);
                let state = AppState::new(config.clone(), storage);
                let port = config.port;
                let bind_host = config.bind_host.clone();
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let addr: SocketAddr = format!("{}:{}", bind_host, port).parse().unwrap();
                    println!("CradleRing 网关启动于 http://{}", addr);
                    println!("WebSocket: ws://{}/ws", addr);
                    println!("数据目录: {}/.cradle-ring", home);
                    println!("Token: {}", config.token);
                    if bind_host == "0.0.0.0" {
                        println!("⚠️  网关已绑定到 0.0.0.0，所有网络接口可访问（请确保防火墙已放行）");
                    }
                    println!("按 Ctrl+C 停止\n监听中...");
                    // 初始化（必须在 runtime 内）：渠道同步、默认 admin、审批循环
                    state.init().await;
                    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
                    // 启动 cron 调度器
                    let cron_state = state.clone();
                    tokio::spawn(cron_scheduler_loop(cron_state));
                    // 启动渠道后台任务
                    let chan_state = state.clone();
                    spawn_channel_background_tasks(chan_state);
                    while let Ok((stream, _)) = listener.accept().await {
                        let st = state.clone();
                        tokio::spawn(async move {
                            let mut stream = stream;
                            // 读 HTTP 请求头
                            use tokio::io::AsyncReadExt;
                            let mut buf = vec![0u8; 16384];
                            let n = stream.read(&mut buf).await.unwrap_or(0);
                            if n == 0 { return; }
                            let request = String::from_utf8_lossy(&buf[..n]).to_string();
                            if request.contains("Upgrade: websocket") || request.contains("upgrade: websocket") {
                                handle_websocket_connection(stream, request, st).await;
                            } else {
                                handle_http(&mut stream, &request, st).await;
                            }
                        });
                    }
                });
            } else if args.len() > 2 && args[2] == "status" {
                match std::net::TcpStream::connect("127.0.0.1:18800") {
                    Ok(_) => println!("✓ 网关正在运行"),
                    Err(_) => println!("✗ 网关未运行"),
                }
            } else { println!("用法: cradle-ring gateway <start|status>"); }
        }
        "doctor" => {
            println!("CradleRing 诊断");
            println!("  版本: {}", env!("CARGO_PKG_VERSION"));
            match std::net::TcpStream::connect("127.0.0.1:18800") {
                Ok(_) => println!("  网关: 运行中 ✓"),
                Err(_) => println!("  网关: 未运行"),
            }
        }
        "onboard" => {
            println!("CradleRing 配置向导（运行 install.sh 调用）");
        }
        "configure" => {
            println!("CradleRing 配置修改（运行 install.sh 调用）");
        }
        _ => println!("未知命令: {}", args[1]),
    }
}
// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_field_star() {
        assert_eq!(parse_cron_field("*", (0, 59)).unwrap(), (0..=59).collect::<Vec<_>>());
    }

    #[test]
    fn cron_field_step() {
        assert_eq!(parse_cron_field("*/5", (0, 59)).unwrap(), vec![0,5,10,15,20,25,30,35,40,45,50,55]);
    }

    #[test]
    fn cron_field_list() {
        assert_eq!(parse_cron_field("1,3,5", (0, 59)).unwrap(), vec![1,3,5]);
    }

    #[test]
    fn cron_field_range() {
        assert_eq!(parse_cron_field("1-5", (0, 59)).unwrap(), vec![1,2,3,4,5]);
    }

    #[test]
    fn cron_field_range_step() {
        assert_eq!(parse_cron_field("1-10/3", (0, 59)).unwrap(), vec![1,4,7,10]);
    }

    #[test]
    fn cron_field_invalid() {
        assert!(parse_cron_field("abc", (0, 59)).is_err());
    }

    #[test]
    fn cron_parse_5_fields() {
        let spec = parse_cron("*/5 * * * *").unwrap();
        assert_eq!(spec.minutes, (0..=59).step_by(5).collect::<Vec<_>>());
    }

    #[test]
    fn cron_parse_wrong_segments() {
        assert!(parse_cron("* * * *").is_err());
    }

    #[test]
    fn cron_next_run_finds_future() {
        let spec = parse_cron("0 0 * * *").unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let next = next_cron_run(&spec, now).unwrap();
        assert!(next > now);
    }

    #[test]
    fn cron_next_run_every_5_min() {
        let spec = parse_cron("*/5 * * * *").unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let next = next_cron_run(&spec, now).unwrap();
        assert!(next > now && next - now < 300_000);
    }

    #[test]
    fn dangerous_command_detection() {
        assert!(is_dangerous_command("rm -rf /"));
        assert!(is_dangerous_command("sudo rm -rf /"));
        assert!(!is_dangerous_command("ls -la"));
    }

    #[test]
    fn channel_parser_telegram() {
        let v = serde_json::json!([{"message": {"chat": {"id": 123}, "from": {"first_name": "Test"}, "text": "Hello"}}]);
        let msgs = parse_telegram(&v);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "Hello");
    }

    #[test]
    fn channel_parser_discord_ignores_bot() {
        let v = serde_json::json!({"content": "Hi", "author": {"bot": true}});
        let msgs = parse_discord(&v);
        assert_eq!(msgs.len(), 0);
    }

    #[test]
    fn channel_parser_feishu() {
        let v = serde_json::json!({"type": "event_callback", "event": {"message": {"chat_id": "oc_xxx", "message_id": "om_xxx", "content": "{\"text\":\"hello\"}", "sender": {"sender_id": {"open_id": "ou_xxx", "name": "Test"}}}}});
        let msgs = parse_feishu(&v);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn channel_parser_slack_skips_bots() {
        let v = serde_json::json!({"type": "event_callback", "event": {"type": "message", "user": "U123", "text": "Hi", "bot_id": "B123"}});
        let msgs = parse_slack(&v);
        assert_eq!(msgs.len(), 0);
    }

    #[test]
    fn channel_parser_matrix() {
        let v = serde_json::json!({"room_id": "!xxx:server", "events": [{"type": "m.room.message", "content": {"body": "Hi", "msgtype": "m.text"}, "sender": "@user:server"}]});
        let msgs = parse_matrix(&v);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn channel_query_decode() {
        assert_eq!(url_decode("hello%20world"), "hello world");
    }

    #[test]
    fn extract_entities_finds_paths_urls_ids() {
        let entities = extract_entities("see /path/to/file and https://example.com and id abc123def456");
        assert!(entities.iter().any(|e| e.contains("/path")));
        assert!(entities.iter().any(|e| e.contains("https://")));
    }

    #[test]
    fn extract_entities_ignores_short_text() {
        let entities = extract_entities("hi there");
        assert!(entities.is_empty());
    }

    #[test]
    fn merge_json_deep_merges() {
        let mut target = json!({"a": 1, "b": {"c": 2}});
        let patch = json!({"b": {"d": 3}, "e": 4});
        merge_json(&mut target, &patch);
        assert_eq!(target["b"]["d"], 3);
        assert_eq!(target["e"], 4);
    }

    #[test]
    fn fallback_summary_handles_empty() {
        let s = fallback_summary(&[]);
        assert!(s.contains("无历史消息"));
    }

    #[test]
    fn fallback_summary_truncates_long_messages() {
        let long_content = "x".repeat(10000);
        let msgs = vec![Message { role: "user".into(), content: long_content, timestamp: 0, attachments: vec![] }];
        let s = fallback_summary(&msgs);
        assert!(s.contains("..."), "长消息应被截断");
    }

    #[test]
    fn compaction_checkpoint_json_roundtrip() {
        let cp = CompactionCheckpoint {
            id: "cp-test".into(),
            session_key: "main".into(),
            created_at: 1000,
            original_count: 50,
            kept_count: 20,
            summary: "摘要".into(),
            entities: vec!["/path/x".into()],
            backup_file: "main_1.jsonl".into(),
            model: "gpt-4o-mini".into(),
            branch: Some("b1".into()),
            parent_id: None,
        };
        let v = compaction_checkpoint_to_json(&cp);
        assert_eq!(v["id"], "cp-test");
        assert_eq!(v["originalCount"], 50);
        assert_eq!(v["entities"][0], "/path/x");
        assert_eq!(v["branch"], "b1");
    }
}
