# CradleRing

> 企业级 AI Agent 协作平台 — 多 Agent 编排、多级审批、40+ IM 渠道、WAF 安全防护、可视化工作流

[![License](https://img.shields.io/badge/license-商业源码许可-blue)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.0.1-green)](https://github.com/UA-Jin/CradleRing/releases)
[![Rust](https://img.shields.io/badge/rust-1.70+-orange)](https://www.rust-lang.org)

## 🚀 快速部署

### 一键安装（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/UA-Jin/CradleRing/main/install.sh | bash
```

> **说明**：一键安装脚本会自动检测并安装所有依赖：
> - **Rust 工具链**：未安装时自动通过 rustup 安装
> - **C 编译器（gcc/cc）**：未安装时自动通过系统包管理器安装（apt/yum/dnf/apk/pacman）
> - **源码下载**：本地无源码时自动从 GitHub 下载（支持 git clone 或 curl tarball）
> - **前端构建**：自动安装 pnpm 依赖并构建（--ignore-scripts 跳过构建脚本检查）
>
> 全程无需手动安装任何依赖，适合全新服务器一键部署。

### 手动安装

```bash
# 1. 克隆仓库
git clone https://github.com/UA-Jin/CradleRing.git
cd CradleRing

# 2. 编译（需要 Rust 1.70+）
cargo build --release --bin cradle-ring

# 3. 安装
./install.sh

# 4. 启动
cradle-ring gateway start

# 5. 浏览器访问
# http://127.0.0.1:18800
```

**默认登录**：安装完成后，系统会自动生成随机用户名和密码（如 `admin_1822677b` / `bmf0WhXIc1FrGRzm`），仅显示一次，请妥善保管。

---

## ✨ 功能特点

### 🤖 多 Agent 编排引擎

| 能力 | 说明 |
|------|------|
| **有状态图引擎** | 对标 LangGraph：9 种节点类型（LLM/Tool/Agent/Condition/Parallel/Interrupt/HumanReview/End），条件路由 + 检查点 + 回滚 + interrupt 断点 |
| **角色化 Agent** | 对标 CrewAI：role + goal + backstory + 工具白名单 + 独立 system prompt，7 个预置运维专家（根因诊断/SRE/网络/磁盘/计算/服务/SOP 二审） |
| **Sequential 流水线** | 多 Agent 按顺序接力执行，前一阶段输出自动传入下一阶段 |
| **并行工具调用** | 同一轮 LLM 返回多个 tool_call 时并行执行（FuturesUnordered），Map-Reduce 扇出支持批量子任务并行 |
| **执行追踪** | 完整 span 树：workflow → node → tool_call/llm_call，含耗时/token/cost/IO |

### 🔒 安全与审批

| 能力 | 说明 |
|------|------|
| **WAF 安全防护** | 对标 ModSecurity：10 条 OWASP CRS 核心规则（SQL 注入/XSS/命令注入/路径遍历/Log4j），WAF 类型识别（Cloudflare/AWS/阿里云盾等 10 种） |
| **多级审批工作流** | 对标钉钉审批：主管→领导→执行链式审批，支持 IM 渠道审批（钉钉/飞书/Telegram 回复"同意/拒绝"即可处理） |
| **命令策略沙箱** | 6 级命令分类（read_fs/read_system/mixed/net_diag/write/destructive），60+ 命令白名单，DeniedArgs 精确拦截（find -delete/-exec 等） |
| **AI SOP 二审** | 高危操作自动检查：有 SOP 覆盖？无并行操作？回滚路径已知？三条全过才 approve，否则 reject |
| **变更事件审计** | 所有 mutating 操作记录 audit log，支持 RCA 根因分析"谁改了什么" |

### 🌐 全渠道接入

- **40+ IM 渠道**：飞书、钉钉、企业微信、Telegram、Discord、Slack、WhatsApp、Signal、QQ、Matrix、Teams、Webhook 等
- **真实连接**：Webhook 接收 + API 发送 + 后台轮询（Telegram/Discord/Matrix 长轮询）
- **消息去重**：防重复处理，渠道状态实时监控

### 🛠️ 内置工具（28+）

| 类别 | 工具 |
|------|------|
| **网络搜索** | web_search、fetch_latest_info（15+ 搜索引擎：SearXNG/Brave/Tavily/DuckDuckGo/Google/Bing/Exa/Firecrawl/Gemini/Grok/Kimi/MiniMax/Ollama） |
| **代码执行** | exec、run_code（Python/JavaScript/Rust 沙箱）、git_ops、docker_ops、process_manage |
| **文件操作** | read_file、write_file、read_document（PDF/Word/Excel/PPT）、file_hash、backup_create |
| **网络安全** | port_scan、http_probe、vuln_scan、dns_lookup、ssl_check、subdomain_enum、waf_detect、sqli_scan、xss_scan、exposure_analysis |
| **运维诊断** | get_host_load、get_host_processes、host_du_summary、host_find_large_files、host_stat_file、host_netns_inspect、host_diagnostic_snapshot、service_monitor、log_analyze、network_trace |
| **多模态** | analyze_image（OpenAI Vision）、transcribe_audio（Whisper）、TTS（OpenAI/Edge/Azure/ElevenLabs） |
| **Agent 协作** | spawn_subagent、fan_out、delegate_task、memory_save |

### 📊 可视化运维

- **运维大屏**：设备在线/掉线/高延迟/有风险统计、ECharts 地图、延迟趋势、风险排行榜、30s 自动轮询
- **工作流编辑器**：可视化节点编辑、条件分支配置、检查点回滚、执行轨迹查看
- **审批中心**：审批实例列表、多级进度、决策历史、IM 通知状态
- **配置编辑器**：傻瓜式表单（Provider/Gateway/Models/Channels/Search/TTS）+ JSON 高级模式 + 自定义路径

### 👥 多账号权限

- **5 种预置角色**：admin/manager/supervisor/operator/viewer
- **自定义角色**：可增删改，scopes 权限精确到每个操作
- **JWT 认证**：HMAC-SHA256，7 天有效期
- **细粒度权限**：支持通配符（`approval.*`、`sessions.read`）

---

## 🏗️ 技术架构

```
┌─────────────────────────────────────────┐
│           Vue3 + Arco Design Pro         │
│  （运维大屏 / 工作流 / 审批 / 配置 / 用户）  │
├─────────────────────────────────────────┤
│      WebSocket JSON-RPC (197+ 方法)      │
├─────────────────────────────────────────┤
│  CradleRing Gateway (Rust 单文件 13k+行)  │
│  ├── Agent Loop（多轮工具调用+流式输出）    │
│  ├── 工作流引擎（状态图+检查点+回滚）      │
│  ├── 审批引擎（多级+IM 渠道+超时自动通过）   │
│  ├── WAF 规则引擎（OWASP CRS）           │
│  ├── 命令策略沙箱（6 级分类+白名单）       │
│  ├── 角色化 Agent（role/goal/backstory）  │
│  └── 40+ IM 渠道（Webhook+轮询+去重）     │
├─────────────────────────────────────────┤
│         文件持久化（JSON/JSONL）          │
│  sessions/messages/approvals/workflows/  │
│  users/roles/waf_rules/change_events/    │
└─────────────────────────────────────────┘
```

**核心亮点**：
- **单二进制部署**：Rust 编译，无运行时依赖（Node/Python 不需要）
- **高性能**：tokio 异步运行时，流式 LLM 调用（SSE 解析），并行工具执行
- **无损压缩**：LLM 摘要 + 实体保留 + 检查点恢复，突破上下文长度限制

---

## 📖 文档

| 文档 | 说明 |
|------|------|
| [安装指南](docs/install.md) | 详细安装步骤（systemd/launchd 自启） |
| [配置说明](docs/config.md) | 配置文件结构和示例 |
| [API 文档](docs/api.md) | WebSocket JSON-RPC 197+ 方法 |
| [工作流指南](docs/workflow.md) | 状态图引擎使用教程 |
| [审批流程](docs/approval.md) | 多级审批配置和 IM 渠道审批 |
| [运维手册](docs/ops.md) | WAF/审计/大屏使用指南 |

---

## 🔧 开发

### 环境要求

- Rust 1.70+
- Node.js 20+ / pnpm（前端构建）
- Linux / macOS（Windows 支持开发中）

### 本地开发

```bash
# 后端
cd crates/cradle-ring
cargo run --bin cradle-ring gateway start

# 前端（开发模式）
cd webui
pnpm install
pnpm dev

# 前端（生产构建）
pnpm build
```

### 项目结构

```
CradleRing/
├── crates/cradle-ring/     # 主二进制（网关+Agent+工具）
├── packages/               # 21 个内部 Rust crate
│   ├── gateway-protocol/   # WebSocket 协议定义
│   ├── agent-core/         # Agent 核心类型
│   └── ...
├── webui/                  # Vue3 + Arco Design Pro 前端
│   ├── src/pages/          # 20+ 页面
│   ├── src/layout/         # 布局组件
│   └── src/stores/         # Pinia 状态
└── install.sh              # 一键安装脚本
```

---

## 📜 开源协议

**CradleRing 商业源码许可协议（Business Source License 1.1 修改版）**

### ✅ 允许

- **企业使用**：企业内部可自由部署和使用，无限制
- **二次开发商用**：可基于 CradleRing 开发自己的产品并销售
- **学习研究**：可自由阅读、学习、研究源代码

### ❌ 禁止

- **二次开发后收费**：不得将 CradleRing 修改后作为 SaaS 服务收费（即不得提供"CradleRing 托管服务"）
- **去除版权信息**：不得移除或修改源代码中的版权和许可声明

### 具体条款

1. **使用**：任何个人或组织可在内部使用 CradleRing，包括生产环境
2. **修改**：可自由修改源代码以满足自身需求
3. **分发**：可分发修改后的版本，但必须保留原始版权和许可声明
4. **商用限制**：不得将 CradleRing 或其修改版本作为**服务**（SaaS）向第三方收费提供
5. **专利授权**：贡献者授予使用其专利的免费许可

**例外**：如需将 CradleRing 作为 SaaS 服务提供，请联系 [cradlering@example.com](mailto:cradlering@example.com) 获取商业授权。

---

## 🤝 贡献

欢迎提交 Issue 和 Pull Request！

1. Fork 本仓库
2. 创建功能分支 (`git checkout -b feature/amazing`)
3. 提交更改 (`git commit -m 'feat: amazing feature'`)
4. 推送到分支 (`git push origin feature/amazing`)
5. 创建 Pull Request

---

## 📝 更新日志

### v0.0.1（2026-07-17）

**初始发布**

- ✅ 多 Agent 编排引擎（状态图+角色化+流水线+并行）
- ✅ WAF 安全防护（OWASP CRS 规则+类型识别）
- ✅ 多级审批工作流（IM 渠道审批）
- ✅ 40+ IM 渠道真实连接
- ✅ 28+ 内置工具（搜索/代码/文件/网络/运维/多模态）
- ✅ 运维大屏（设备状态+延迟趋势+风险排行）
- ✅ 可视化工作流编辑器
- ✅ 傻瓜式配置编辑器（双模式）
- ✅ 多账号权限（预置+自定义角色）
- ✅ 命令策略沙箱（6 级分类+白名单）
- ✅ AI SOP 二审（高危操作自动审查）
- ✅ 变更事件审计（RCA 根因分析）

---

**CradleRing** — 让 AI Agent 真正为企业所用 🚀
