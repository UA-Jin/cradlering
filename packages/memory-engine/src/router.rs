//! 级联模型路由（RouteLLM 方案）
//!
//! 决策流程：
//! 1. 缓存命中（L1/L2）→ 不调用模型
//! 2. 否则评估问题难度（<5ms 启发式）
//!    - 简单（<0.4）→ 小模型 / 便宜 API
//!    - 中等（0.4-0.7）→ 先试小模型，不满意升级
//!    - 复杂（>0.7）→ 直接大模型
//!
//! 难度评估维度：
//! - 问题长度（短→简单）
//! - 是否多步推理（关键词检测）
//! - 是否需要创造性（创意写作关键词）
//! - 历史命中率（高频→简单）
//! - 是否有知识库支撑（有→简单）

use serde::{Deserialize, Serialize};

/// 路由层级
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteTier {
    /// 缓存命中（0 成本）
    Cache,
    /// 小模型 / 便宜 API（低成本）
    Small,
    /// 大模型（高成本，高质量）
    Large,
}

/// 路由决策
#[derive(Clone, Debug, Serialize)]
pub struct RouteDecision {
    pub tier: RouteTier,
    /// 建议使用的模型 ID（对应 Config.providers 中的某个）
    pub suggested_model: String,
    /// 难度评分（0-1，仅 Small/Large 时有意义）
    pub difficulty: f32,
    /// 决策原因
    pub reason: String,
}

/// 路由配置
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouterConfig {
    /// 默认（大）模型 ID
    pub primary_model: String,
    /// 便宜/小模型 ID（可选；None 表示不分级，全部走主模型）
    pub small_model: Option<String>,
    /// 简单问题阈值（<此值走小模型）
    pub easy_threshold: f32,
    /// 复杂问题阈值（>此值走大模型）
    pub hard_threshold: f32,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            primary_model: "gpt-4o".to_string(),
            small_model: None, // 默认不启用级联，保持原行为
            easy_threshold: 0.4,
            hard_threshold: 0.7,
        }
    }
}

/// 级联路由器
pub struct CascadingRouter {
    config: RouterConfig,
}

impl CascadingRouter {
    pub fn new(config: RouterConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &RouterConfig { &self.config }

    /// 缓存命中决策
    pub fn cache_hit(&self) -> RouteDecision {
        RouteDecision {
            tier: RouteTier::Cache,
            suggested_model: String::new(),
            difficulty: 0.0,
            reason: "cache hit (L1/L2)".to_string(),
        }
    }

    /// 根据问题特征 + 上下文做路由决策
    ///
    /// - `query`: 用户问题
    /// - `has_kb_support`: 是否有知识库记忆命中（L4）
    /// - `cache_hit_rate`: 历史缓存命中率（0-1）
    pub fn route(
        &self,
        query: &str,
        has_kb_support: bool,
        cache_hit_rate: f32,
    ) -> RouteDecision {
        // 如果未配置 small_model，全部走主模型（保持原行为）
        // 修复：difficulty 返回实际估算值（不硬编码 1.0，避免污染上层日志/统计）
        if self.config.small_model.is_none() {
            let difficulty = self.estimate_difficulty(query, has_kb_support, cache_hit_rate);
            return RouteDecision {
                tier: RouteTier::Large,
                suggested_model: self.config.primary_model.clone(),
                difficulty,
                reason: "no small_model configured".to_string(),
            };
        }

        let difficulty = self.estimate_difficulty(query, has_kb_support, cache_hit_rate);

        if difficulty < self.config.easy_threshold {
            RouteDecision {
                tier: RouteTier::Small,
                suggested_model: self.config.small_model.clone().unwrap_or_default(),
                difficulty,
                reason: format!("easy question (score={:.2}, kb={}, cache_rate={:.2})", difficulty, has_kb_support, cache_hit_rate),
            }
        } else if difficulty > self.config.hard_threshold {
            RouteDecision {
                tier: RouteTier::Large,
                suggested_model: self.config.primary_model.clone(),
                difficulty,
                reason: format!("hard question (score={:.2})", difficulty),
            }
        } else {
            // 中等：先试小模型，不满意升级（由 caller 处理升级）
            RouteDecision {
                tier: RouteTier::Small,
                suggested_model: self.config.small_model.clone().unwrap_or_default(),
                difficulty,
                reason: format!("medium question (score={:.2}), try small first", difficulty),
            }
        }
    }

    /// 启发式难度评估（<5ms）
    fn estimate_difficulty(&self, query: &str, has_kb_support: bool, cache_hit_rate: f32) -> f32 {
        let mut score = 0.0f32;
        let mut weights = 0.0f32;

        // 1. 问题长度（更激进的阈值）
        let len = query.chars().count();
        let len_score = if len < 10 { 0.15 } else if len < 30 { 0.25 } else if len < 60 { 0.45 } else if len < 100 { 0.65 } else if len < 200 { 0.85 } else { 0.95 };
        score += len_score * 0.25;
        weights += 0.25;

        // 2. 多步推理关键词（出现越多越复杂）
        let multi_step_keywords = ["为什么", "分析", "对比", "比较", "推理", "计算", "证明", "解释", "如何", "怎么办", "步骤", "完整", "详细", "多个维度", "优劣", "架构"];
        let multi_hits = multi_step_keywords.iter().filter(|k| query.contains(*k)).count();
        let multi_score = match multi_hits {
            0 => 0.25,
            1 => 0.55,
            2 => 0.80,
            _ => 0.95,
        };
        score += multi_score * 0.30;
        weights += 0.30;

        // 3. 创造性关键词（创意写作 / 设计 / 头脑风暴）
        let creative_keywords = ["写", "创作", "设计", "头脑风暴", "故事", "诗歌", "创意", "方案", "规划", "策划", "一首", "小说"];
        let has_creative = creative_keywords.iter().any(|k| query.contains(k));
        let creative_score = if has_creative { 0.70 } else { 0.30 };
        score += creative_score * 0.20;
        weights += 0.20;

        // 4. 知识库支撑（有支撑 → 更简单）
        let kb_score = if has_kb_support { 0.25 } else { 0.60 };
        score += kb_score * 0.15;
        weights += 0.15;

        // 5. 历史命中率（高 → 简单，问题模式已被覆盖）
        let hist_score = 1.0 - cache_hit_rate.min(1.0);
        score += hist_score * 0.10;
        weights += 0.10;

        let mut final_score = if weights > 0.0 { score / weights } else { 0.5 };

        // 6. 显式复杂信号：长问题 + 多个复杂关键词 → 直接拉高
        if len > 50 && multi_hits >= 2 {
            final_score = final_score.max(0.75);
        }
        // 7. 极短问题（<5 字符）+ 无关键词 → 直接简单
        if len < 5 && multi_hits == 0 && !has_creative {
            final_score = final_score.min(0.30);
        }

        final_score.min(1.0).max(0.0)
    }

    /// 质量评估（决定是否升级）
    ///
    /// 简化：检查回答是否过短 / 包含不确定词 / 与问题无关
    pub fn should_escalate(&self, query: &str, answer: &str, _model: &str) -> bool {
        // 过短回答
        if answer.chars().count() < 20 { return true; }
        // 明显的不确定信号
        let uncertain = ["我不知道", "无法回答", "不清楚", "I don't know", "I cannot"];
        if uncertain.iter().any(|u| answer.contains(u)) { return true; }
        // 重复问题（说明模型没理解）
        if answer.trim() == query.trim() { return true; }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_simple_question() {
        let router = CascadingRouter::new(RouterConfig {
            primary_model: "gpt-4o".to_string(),
            small_model: Some("gpt-4o-mini".to_string()),
            easy_threshold: 0.4,
            hard_threshold: 0.7,
        });
        let d = router.route("你好", false, 0.5);
        assert!(d.difficulty < 0.5);
        assert_eq!(d.tier, RouteTier::Small);
    }

    #[test]
    fn test_router_complex_question() {
        let router = CascadingRouter::new(RouterConfig {
            primary_model: "gpt-4o".to_string(),
            small_model: Some("gpt-4o-mini".to_string()),
            easy_threshold: 0.4,
            hard_threshold: 0.7,
        });
        let d = router.route("请详细分析并对比 Rust、Go、Python 三种语言在并发编程、内存安全、性能、生态、学习曲线等多个维度的优劣，给出一份完整的对比分析报告", false, 0.0);
        assert!(d.difficulty > 0.5);
    }
}
