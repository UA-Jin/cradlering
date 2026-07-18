//! 时序知识图谱（借鉴 Zep/Graphiti）
//!
//! 轻量级实现：JSON 存储 + BFS 遍历。
//! 实体节点 + 关系边（带时间戳和有效期），支持：
//! - 多跳推理（BFS）
//! - 时序查询（按时间排序取最新状态）
//! - 实体消歧（同名实体按 source 区分）

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::RwLock;

/// 实体节点
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub kind: String, // user / project / tech / service / event / concept / ...
    #[serde(default)]
    pub attributes: HashMap<String, serde_json::Value>,
    pub source: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 关系边（带时序）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Relation {
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    /// 关系类型（喜欢/使用/依赖/导致/属于/...）
    pub kind: String,
    /// 关系强度（0-1）
    #[serde(default = "default_strength")]
    pub strength: f32,
    #[serde(default)]
    pub attributes: HashMap<String, serde_json::Value>,
    /// 生效时间（unix 秒）
    pub valid_from: i64,
    /// 失效时间（None 表示仍然有效；Some 表示已失效）
    #[serde(default)]
    pub valid_until: Option<i64>,
    pub source: String,
}

fn default_strength() -> f32 { 1.0 }

/// 图谱快照（用于持久化）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
    /// name -> entity_ids（同名实体可能有多个，按 source 区分）
    #[serde(skip)]
    pub name_index_data: HashMap<String, Vec<String>>,
}

/// 时序知识图谱
pub struct KnowledgeGraph {
    inner: RwLock<GraphSnapshot>,
    path: Option<PathBuf>,
}

impl KnowledgeGraph {
    pub fn in_memory() -> Self {
        Self {
            inner: RwLock::new(GraphSnapshot::default()),
            path: None,
        }
    }

    pub fn persistent(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut snapshot = GraphSnapshot::default();
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            if let Ok(s) = serde_json::from_str::<GraphSnapshot>(&data) {
                snapshot = s;
            }
        }
        // 重建 name 索引
        let mut name_idx: HashMap<String, Vec<String>> = HashMap::new();
        for e in &snapshot.entities {
            name_idx.entry(e.name.clone()).or_default().push(e.id.clone());
        }
        snapshot.name_index_data = name_idx;
        Ok(Self {
            inner: RwLock::new(snapshot),
            path: Some(path),
        })
    }

    /// 新增或更新实体（按 name+source 去重）
    pub fn upsert_entity(&self, mut entity: Entity) -> anyhow::Result<String> {
        let result_id = {
            let mut snap = self.inner.write().unwrap();
            // 查找现有同名+同 source 实体
            let existing_pos = snap.entities.iter().position(|e| e.name == entity.name && e.source == entity.source);
            match existing_pos {
                Some(pos) => {
                    let e = &mut snap.entities[pos];
                    e.kind = entity.kind.clone();
                    e.attributes.extend(entity.attributes.drain());
                    e.updated_at = entity.updated_at;
                    entity.id = e.id.clone();
                    entity.id.clone()
                }
                None => {
                    if entity.id.is_empty() {
                        entity.id = format!("e-{}", snap.entities.len() + 1);
                    }
                    let id = entity.id.clone();
                    snap.name_index_data.entry(entity.name.clone()).or_default().push(id.clone());
                    snap.entities.push(entity);
                    id
                }
            }
        };
        self.persist()?;
        Ok(result_id)
    }

    /// 新增关系（默认会自动失效同 from+kind 的旧关系，模拟"偏好变化"时序语义）
    ///
    /// 例如：用户"喜欢"Python → 用户"喜欢"Rust 会自动让 Python 关系失效。
    /// 如果是"拥有"这种可同时存在的累积关系，请用 `add_relation_cumulative`。
    pub fn add_relation(&self, mut rel: Relation) -> anyhow::Result<String> {
        let id = {
            let mut snap = self.inner.write().unwrap();
            if rel.id.is_empty() {
                rel.id = format!("r-{}", snap.relations.len() + 1);
            }
            let now = rel.valid_from;
            // 时序语义：同 from+kind 的旧关系自动失效（偏好变化场景）
            // 例如 "用户 喜欢 Python" → "用户 喜欢 Rust" 会让前者失效
            for r in snap.relations.iter_mut() {
                if r.from_id == rel.from_id && r.kind == rel.kind && r.valid_until.is_none() {
                    r.valid_until = Some(now);
                }
            }
            let id = rel.id.clone();
            snap.relations.push(rel);
            id
        };
        self.persist()?;
        Ok(id)
    }

    /// 累积式新增关系（不失效旧关系，用于"拥有/创建/包含"等可同时存在的关系）
    pub fn add_relation_cumulative(&self, mut rel: Relation) -> anyhow::Result<String> {
        let id = {
            let mut snap = self.inner.write().unwrap();
            if rel.id.is_empty() {
                rel.id = format!("r-{}", snap.relations.len() + 1);
            }
            let id = rel.id.clone();
            snap.relations.push(rel);
            id
        };
        self.persist()?;
        Ok(id)
    }

    /// 获取实体当前状态（按 name 查找，返回最新更新的那个）
    pub fn get_entity_by_name(&self, name: &str) -> Option<Entity> {
        let snap = self.inner.read().unwrap();
        let ids = snap.name_index_data.get(name)?;
        let mut candidates: Vec<&Entity> = snap.entities.iter().filter(|e| ids.contains(&e.id)).collect();
        candidates.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        candidates.first().map(|e| (*e).clone())
    }

    pub fn get_entity(&self, id: &str) -> Option<Entity> {
        self.inner.read().unwrap().entities.iter().find(|e| e.id == id).cloned()
    }

    /// 获取实体在某时间点的所有有效关系
    pub fn entity_relations_at(&self, entity_id: &str, at_time: i64, kind_filter: Option<&str>) -> Vec<Relation> {
        let snap = self.inner.read().unwrap();
        snap.relations.iter()
            .filter(|r| r.from_id == entity_id || r.to_id == entity_id)
            .filter(|r| r.valid_from <= at_time)
            .filter(|r| r.valid_until.map_or(true, |u| u > at_time))
            .filter(|r| kind_filter.map_or(true, |k| r.kind == k))
            .cloned()
            .collect()
    }

    /// 多跳推理（BFS）：从某个实体出发，找 depth 跳内所有可达实体
    pub fn multi_hop(&self, start_id: &str, depth: usize, at_time: i64) -> Vec<(Entity, usize)> {
        let snap = self.inner.read().unwrap();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(start_id.to_string());
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((start_id.to_string(), 0));
        let mut result: Vec<(Entity, usize)> = Vec::new();

        while let Some((cur_id, cur_depth)) = queue.pop_front() {
            if cur_depth >= depth { continue; }
            // 找邻居
            let neighbors: Vec<&Relation> = snap.relations.iter()
                .filter(|r| r.valid_from <= at_time && r.valid_until.map_or(true, |u| u > at_time))
                .filter(|r| r.from_id == cur_id || r.to_id == cur_id)
                .collect();
            for rel in neighbors {
                let other_id = if rel.from_id == cur_id { &rel.to_id } else { &rel.from_id };
                if visited.contains(other_id) { continue; }
                visited.insert(other_id.clone());
                if let Some(e) = snap.entities.iter().find(|e| e.id == *other_id) {
                    result.push((e.clone(), cur_depth + 1));
                    queue.push_back((other_id.clone(), cur_depth + 1));
                }
            }
        }
        result
    }

    /// 从文本提取实体和关系（启发式 + 可选大模型）
    ///
    /// 简化实现：识别"X 喜欢/使用/依赖 Y" 模式。
    /// 生产环境应接入大模型做 NER + 关系抽取。
    ///
    /// 关键修复：
    /// 1. 否定词检测：动词左侧紧邻 不/没/无/别/未 时跳过（"我不喜欢X" 不再抽取为"喜欢"，
    ///    避免错误关系把真实的"喜欢"关系置为失效 —— 之前会静默破坏数据）
    /// 2. "是" 类短动词用词边界检查（"但是/是否/不是" 不再误命中）
    /// 3. 抽取的关系 strength 降为 0.6（启发式候选，不与人工/权威来源关系同等对待）
    pub fn extract_from_text(&self, text: &str, source: &str, now: i64) -> Vec<(Entity, Entity, Relation)> {
        let mut triples: Vec<(Entity, Entity, Relation)> = Vec::new();
        // 简单规则：动词分割
        let verbs = ["喜欢", "使用", "依赖", "导致", "属于", "创建", "包含", "位于", "是"];
        // 否定前缀（紧邻动词左侧即视为否定）
        let negations = ["不", "没", "无", "别", "未", "莫"];
        // 对单字动词"是"的边界保护词（这些词包含"是"但不是独立动词）
        let shi_guards = ["但是", "是否", "不是", "就是", "只是", "算是", "还是", "或是", "而是", "总是", "要是", "于是", "倒是", "乃是"];

        for v in &verbs {
            let mut search_from = 0usize;
            while let Some(rel_idx) = text[search_from..].find(v) {
                let idx = search_from + rel_idx;
                search_from = idx + v.len();

                // 否定检测：动词左侧紧邻的字符是否定词 → 跳过
                let before_text = &text[..idx];
                let prev_char = before_text.chars().last();
                if let Some(pc) = prev_char {
                    if negations.iter().any(|n| n.chars().next() == Some(pc)) {
                        continue;
                    }
                }
                // "是" 的边界保护：检查前后是否构成保护词（用字符感知窗口，避免 UTF-8 字节边界 panic）
                if *v == "是" {
                    let window_start = text[..idx].chars().last().map(|c| idx - c.len_utf8()).unwrap_or(idx);
                    let mut window_end = (idx + v.len() + 3).min(text.len());
                    while window_end < text.len() && !text.is_char_boundary(window_end) {
                        window_end += 1;
                    }
                    let window = &text[window_start..window_end];
                    if shi_guards.iter().any(|g| window.contains(g)) {
                        continue;
                    }
                }

                let before = before_text.trim();
                let after = text[idx + v.len()..].trim();
                // 取分词的第一个名词短语（简化：到逗号/句号为止）
                let subj = before.split(|c: char| c == '，' || c == '。' || c == ',' || c == '；' || c == ';').last().unwrap_or("").trim();
                let obj = after.split(|c: char| c == '，' || c == '。' || c == ',' || c == '；' || c == ';').next().unwrap_or("").trim();
                // 过滤过短/过长的候选（"我不" 这类残片、整句话）
                let subj_ok = (1..=20).contains(&subj.chars().count());
                let obj_ok = (1..=30).contains(&obj.chars().count());
                if subj_ok && obj_ok {
                    let subj_e = Entity {
                        id: String::new(),
                        name: subj.to_string(),
                        kind: "unknown".to_string(),
                        attributes: HashMap::new(),
                        source: source.to_string(),
                        created_at: now,
                        updated_at: now,
                    };
                    let obj_e = Entity {
                        id: String::new(),
                        name: obj.to_string(),
                        kind: "unknown".to_string(),
                        attributes: HashMap::new(),
                        source: source.to_string(),
                        created_at: now,
                        updated_at: now,
                    };
                    let rel = Relation {
                        id: String::new(),
                        from_id: String::new(), // 由 upsert 填充
                        to_id: String::new(),
                        kind: v.to_string(),
                        strength: 0.6, // 启发式候选，低于人工/权威来源（1.0）
                        attributes: HashMap::new(),
                        valid_from: now,
                        valid_until: None,
                        source: source.to_string(),
                    };
                    triples.push((subj_e, obj_e, rel));
                }
            }
        }
        triples
    }

    /// 统计
    pub fn stats(&self) -> GraphStats {
        let snap = self.inner.read().unwrap();
        GraphStats {
            entities: snap.entities.len(),
            relations: snap.relations.len(),
            active_relations: snap.relations.iter().filter(|r| r.valid_until.is_none()).count(),
        }
    }

    /// 导出快照
    pub fn snapshot(&self) -> GraphSnapshot {
        let snap = self.inner.read().unwrap();
        let mut out = snap.clone();
        out.name_index_data = snap.name_index_data.clone();
        out
    }

    /// 清空
    pub fn clear(&self) -> anyhow::Result<()> {
        self.inner.write().unwrap().entities.clear();
        self.inner.write().unwrap().relations.clear();
        self.inner.write().unwrap().name_index_data.clear();
        self.persist()?;
        Ok(())
    }

    fn persist(&self) -> std::io::Result<()> {
        if let Some(path) = &self.path {
            let snap = self.inner.read().unwrap();
            let mut serializable = GraphSnapshot {
                entities: snap.entities.clone(),
                relations: snap.relations.clone(),
                name_index_data: HashMap::new(),
            };
            let _ = &mut serializable;
            let json = serde_json::to_string_pretty(&serializable)?;
            std::fs::write(path, json)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GraphStats {
    pub entities: usize,
    pub relations: usize,
    pub active_relations: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temporal_relation() {
        let g = KnowledgeGraph::in_memory();
        let now = 1000i64;
        let u = g.upsert_entity(Entity {
            id: String::new(), name: "用户".to_string(), kind: "user".to_string(),
            attributes: HashMap::new(), source: "test".to_string(),
            created_at: now, updated_at: now,
        }).unwrap();
        let p1 = g.upsert_entity(Entity {
            id: String::new(), name: "Python".to_string(), kind: "tech".to_string(),
            attributes: HashMap::new(), source: "test".to_string(),
            created_at: now, updated_at: now,
        }).unwrap();
        let r1 = g.add_relation(Relation {
            id: String::new(), from_id: u.clone(), to_id: p1.clone(),
            kind: "喜欢".to_string(), strength: 1.0, attributes: HashMap::new(),
            valid_from: now, valid_until: None, source: "test".to_string(),
        }).unwrap();

        // 早期查询：喜欢 Python
        let rels = g.entity_relations_at(&u, now + 100, Some("喜欢"));
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].to_id, p1);

        // 后来改为喜欢 Rust
        let later = now + 10000;
        let r2 = g.upsert_entity(Entity {
            id: String::new(), name: "Rust".to_string(), kind: "tech".to_string(),
            attributes: HashMap::new(), source: "test".to_string(),
            created_at: later, updated_at: later,
        }).unwrap();
        let _ = g.add_relation(Relation {
            id: String::new(), from_id: u.clone(), to_id: r2.clone(),
            kind: "喜欢".to_string(), strength: 1.0, attributes: HashMap::new(),
            valid_from: later, valid_until: None, source: "test".to_string(),
        }).unwrap();

        // 现在查询：喜欢 Rust（旧关系已失效）
        let rels_now = g.entity_relations_at(&u, later + 100, Some("喜欢"));
        assert_eq!(rels_now.len(), 1);
        assert_eq!(rels_now[0].to_id, r2);

        // 但早期时刻查询仍然返回 Python
        let rels_old = g.entity_relations_at(&u, now + 100, Some("喜欢"));
        assert_eq!(rels_old[0].to_id, p1);

        // 多跳：用户 -> Python
        let multi = g.multi_hop(&u, 2, later + 100);
        assert!(multi.iter().any(|(e, _)| e.name == "Rust"));
        let _ = r1;
    }

    #[test]
    fn test_negation_extraction() {
        let g = KnowledgeGraph::in_memory();
        let now = 1000i64;
        // 否定句不应抽取出"喜欢"关系
        let triples = g.extract_from_text("我不喜欢Python", "test", now);
        assert!(triples.iter().all(|(_, _, r)| r.kind != "喜欢"), "否定句不能抽取'喜欢'");
        // 肯定句正常抽取
        let triples2 = g.extract_from_text("我喜欢Rust", "test", now);
        assert!(triples2.iter().any(|(_, _, r)| r.kind == "喜欢"), "肯定句应抽取'喜欢'");
        // "是" 的保护词不误匹配
        let triples3 = g.extract_from_text("但是这不是问题", "test", now);
        assert!(triples3.iter().all(|(_, _, r)| r.kind != "是"), "保护词不能抽取'是'");
    }
}
