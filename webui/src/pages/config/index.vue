<template>
  <div class="page-container">
    <a-page-header title="配置管理" subtitle="无脑配置 · 选服务商 → 填 Key → 测试 → 保存" :show-back="false">
      <template #extra>
        <a-space>
          <a-button @click="loadConfig"><template #icon><icon-refresh /></template>重新加载</a-button>
          <a-button type="primary" :loading="saving" @click="saveAll">
            <template #icon><icon-save /></template>保存全部
          </a-button>
        </a-space>
      </template>
    </a-page-header>

    <!-- pill 标签页（Materialize account-settings 风格） -->
    <div class="pill-tabs mt-16">
      <div
        v-for="t in tabs"
        :key="t.key"
        class="pill-tab"
        :class="{ active: activeTab === t.key }"
        @click="activeTab = t.key"
      >
        <component :is="t.icon" />
        <span>{{ t.label }}</span>
      </div>
    </div>

    <!-- 大模型配置（无脑模式） -->
    <div v-show="activeTab === 'llm'">
      <!-- 已配置的 Provider 列表 -->
      <a-card v-for="(p, idx) in formData.providers" :key="idx" class="provider-card mt-16">
        <div class="provider-header">
          <div class="provider-title">
            <div class="provider-icon" :style="{ background: providerPreset(p.name)?.color || '#6d6777' }">
              {{ (providerPreset(p.name)?.label || p.name).charAt(0).toUpperCase() }}
            </div>
            <div>
              <div class="provider-name">{{ providerPreset(p.name)?.label || p.name }}</div>
              <div class="provider-url">{{ p.baseUrl || '默认端点' }}</div>
            </div>
          </div>
          <a-space>
            <a-tag :color="p.enabled ? 'green' : 'gray'">{{ p.enabled ? '已启用' : '已禁用' }}</a-tag>
            <a-switch v-model="p.enabled" />
            <a-button status="danger" size="small" type="text" @click="formData.providers.splice(idx, 1)"><icon-delete /></a-button>
          </a-space>
        </div>
        <a-row :gutter="16" class="mt-16">
          <a-col :span="12">
            <a-form-item label="API Key" extra="从服务商控制台获取，只保存在本机">
              <a-input-password v-model="p.apiKey" placeholder="sk-..." allow-clear />
            </a-form-item>
          </a-col>
          <a-col :span="12">
            <a-form-item label="默认模型" extra="对话使用的模型，🌐 多模态 / 🧠 思考 / 📝 纯文本">
              <a-select
                v-model="p.model"
                allow-create
                placeholder="选择或输入模型"
                @change="onModelChange(p)"
              >
                <a-option v-for="m in providerPreset(p.name)?.models || []" :key="m" :value="m">
                  <span style="display: inline-flex; align-items: center; gap: 8px;">
                    <span>{{ m }}</span>
                    <a-tag size="small" :color="modelTag(m).color" style="margin: 0;">{{ modelTag(m).icon }} {{ modelTag(m).label }}</a-tag>
                    <a-tag v-if="contextLabel(m)" size="small" color="gray" style="margin: 0;">{{ contextLabel(m) }}</a-tag>
                  </span>
                </a-option>
              </a-select>
              <!-- 已选模型的能力标签回显 -->
              <div v-if="p.model" class="model-cap-hint">
                <a-tag size="small" :color="modelTag(p.model).color">{{ modelTag(p.model).icon }} {{ modelTag(p.model).label }}</a-tag>
                <a-tag v-if="contextLabel(p.model)" size="small" color="gray">{{ contextLabel(p.model) }} 上下文</a-tag>
              </div>
            </a-form-item>
          </a-col>
        </a-row>
        <a-row :gutter="16">
          <a-col :span="12">
            <a-form-item label="Base URL" extra="一般无需修改，预设已自动填充">
              <a-input v-model="p.baseUrl" placeholder="https://..." />
            </a-form-item>
          </a-col>
          <a-col :span="6">
            <a-form-item label="思考等级" extra="仅在原生支持思考的模型上生效">
              <a-select v-model="p.thinkingLevel" :disabled="!modelSupportsThinking(p)">
                <a-option v-for="lv in thinkingLevels" :key="lv.value" :value="lv.value">{{ lv.label }}</a-option>
              </a-select>
            </a-form-item>
          </a-col>
          <a-col :span="6">
            <a-form-item label="多模态" extra="模型原生能力，自动识别">
              <a-tag :color="modelCap(p.model).vision ? 'green' : 'gray'">
                {{ modelCap(p.model).vision ? '支持图像输入' : '仅文本' }}
              </a-tag>
            </a-form-item>
          </a-col>
        </a-row>
        <!-- 自定义 Provider：协议类型 + 模型名 -->
        <a-row v-if="providerPreset(p.name)?.custom" :gutter="16">
          <a-col :span="8">
            <a-form-item label="Provider 协议" extra="OpenAI 兼容协议类型">
              <a-select v-model="p.providerType" allow-create placeholder="openai / anthropic / gemini ...">
                <a-option value="openai">openai（默认）</a-option>
                <a-option value="anthropic">anthropic</a-option>
                <a-option value="gemini">gemini</a-option>
                <a-option value="ollama">ollama</a-option>
              </a-select>
            </a-form-item>
          </a-col>
          <a-col :span="16">
            <a-form-item label="模型名" extra="手动输入 API 文档中的模型 ID">
              <a-input v-model="p.model" placeholder="例如 gpt-4o 或 glm-4.6" />
            </a-form-item>
          </a-col>
        </a-row>
        <!-- 测试连接 -->
        <div class="provider-test">
          <a-button :loading="p._testing" @click="testProvider(p)">
            <template #icon><icon-thunderbolt /></template>
            测试连接
          </a-button>
          <span v-if="p._testResult" class="test-result" :class="{ ok: p._testResult.connected, fail: !p._testResult.connected }">
            <icon-check-circle-fill v-if="p._testResult.connected" />
            <icon-close-circle-fill v-else />
            {{ p._testResult.info || p._testResult.error }}
          </span>
        </div>
      </a-card>

      <!-- 添加 Provider（无脑：选服务商 → 自动填充） -->
      <a-card class="mt-16">
        <div class="add-provider-title">添加模型服务商</div>
        <div class="preset-grid">
          <div
            v-for="preset in providerPresets"
            :key="preset.id"
            class="preset-item"
            @click="addProviderFromPreset(preset)"
          >
            <div class="preset-icon" :style="{ background: preset.color }">{{ preset.label.charAt(0) }}</div>
            <div class="preset-label">{{ preset.label }}</div>
            <div class="preset-desc">{{ preset.desc }}</div>
          </div>
        </div>
      </a-card>
    </div>

    <!-- Embedding 配置 -->
    <div v-show="activeTab === 'embedding'">
      <a-card class="mt-16">
        <a-alert type="info" class="mb-16">Embedding 用于记忆系统的向量语义检索。本地模式零成本开箱即用；API 模式精度更高。</a-alert>
        <a-form :model="formData.embedding" layout="vertical" style="max-width: 640px">
          <a-form-item label="Embedding 模式">
            <a-radio-group v-model="formData.embedding.provider" type="button">
              <a-radio value="local">本地模型（零成本）</a-radio>
              <a-radio value="siliconflow">硅基流动 API</a-radio>
            </a-radio-group>
          </a-form-item>
          <template v-if="formData.embedding.provider === 'local'">
            <a-form-item label="本地模型" extra="首次使用自动下载约 100MB，完全离线运行">
              <a-select v-model="formData.embedding.model">
                <a-option value="BAAI/bge-small-zh-v1.5">BAAI/bge-small-zh-v1.5（推荐，中文优化）</a-option>
                <a-option value="BAAI/bge-base-zh-v1.5">BAAI/bge-base-zh-v1.5（更大更准）</a-option>
              </a-select>
            </a-form-item>
          </template>
          <template v-else>
            <a-form-item label="硅基流动 API Key" extra="从 siliconflow.cn 获取">
              <a-input-password v-model="formData.embedding.apiKey" placeholder="sk-..." allow-clear />
            </a-form-item>
            <a-form-item label="模型">
              <a-select v-model="formData.embedding.model">
                <a-option value="Qwen/Qwen3-VL-Embedding-8B">Qwen/Qwen3-VL-Embedding-8B（默认推荐）</a-option>
                <a-option value="BAAI/bge-large-zh-v1.5">BAAI/bge-large-zh-v1.5</a-option>
              </a-select>
            </a-form-item>
          </template>
        </a-form>
      </a-card>
    </div>

    <!-- 网关配置 -->
    <div v-show="activeTab === 'gateway'">
      <a-card class="mt-16">
        <a-form :model="formData.gateway" layout="vertical" style="max-width: 640px">
          <a-form-item label="绑定地址" extra="127.0.0.1 仅本机访问（推荐）；0.0.0.0 开放局域网/公网（需防火墙放行 + 强密码）">
            <a-radio-group v-model="formData.gateway.bind" type="button">
              <a-radio value="loopback">仅本机 127.0.0.1（推荐）</a-radio>
              <a-radio value="all">开放外网 0.0.0.0</a-radio>
            </a-radio-group>
          </a-form-item>
          <a-row :gutter="16">
            <a-col :span="12">
              <a-form-item label="端口">
                <a-input-number v-model="formData.gateway.port" :min="1" :max="65535" />
              </a-form-item>
            </a-col>
            <a-col :span="12">
              <a-form-item label="Gateway Token" extra="API 访问令牌（非登录密码）">
                <a-input-password v-model="formData.gateway.token" />
              </a-form-item>
            </a-col>
          </a-row>
          <a-alert v-if="formData.gateway.bind === 'all'" type="warning">
            开放外网后任何人都能访问网关页面，请确保登录密码强度足够，并配置防火墙只放行可信 IP
          </a-alert>
        </a-form>
      </a-card>
    </div>

    <!-- 搜索配置 -->
    <div v-show="activeTab === 'search'">
      <a-card class="mt-16">
        <a-form :model="formData.search" layout="vertical" style="max-width: 640px">
          <a-form-item label="默认搜索引擎">
            <a-select v-model="formData.search.default">
              <a-option value="duckduckgo">DuckDuckGo（免费无需 Key）</a-option>
              <a-option value="brave">Brave（需 Key，质量好）</a-option>
              <a-option value="tavily">Tavily（AI 优化，需 Key）</a-option>
              <a-option value="searxng">SearXNG（自托管）</a-option>
              <a-option value="google">Google</a-option>
              <a-option value="bing">Bing</a-option>
            </a-select>
          </a-form-item>
          <a-form-item v-if="formData.search.default === 'searxng'" label="SearXNG URL">
            <a-input v-model="formData.search.searxngUrl" placeholder="https://searx.example.com" />
          </a-form-item>
          <a-form-item v-if="formData.search.default === 'brave'" label="Brave API Key">
            <a-input-password v-model="formData.search.braveKey" placeholder="BSA..." />
          </a-form-item>
          <a-form-item v-if="formData.search.default === 'tavily'" label="Tavily API Key">
            <a-input-password v-model="formData.search.tavilyKey" placeholder="tvly-..." />
          </a-form-item>
        </a-form>
      </a-card>
    </div>

    <!-- 高级 JSON -->
    <div v-show="activeTab === 'json'">
      <a-alert type="warning" class="mt-16">高级模式：直接编辑 JSON 配置。改错了可能导致网关无法启动，建议用表单模式</a-alert>
      <a-card class="mt-8">
        <a-textarea
          v-model="jsonText"
          :auto-size="{ minRows: 20, maxRows: 40 }"
          style="font-family: 'Menlo', 'Monaco', monospace; font-size: 13px"
        />
      </a-card>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, onMounted, markRaw } from 'vue';
import { Message } from '@arco-design/web-vue';
import { rpc } from '@/api/rpc';
import {
  IconRefresh, IconSave, IconDelete, IconThunderbolt, IconCheckCircleFill,
  IconCloseCircleFill, IconRobot, IconBookmark, IconSettings, IconSearch, IconCode,
} from '@arco-design/web-vue/es/icon';

const activeTab = ref('llm');
const saving = ref(false);
const jsonText = ref('');

const tabs = [
  { key: 'llm', label: '大模型', icon: markRaw(IconRobot) },
  { key: 'embedding', label: 'Embedding', icon: markRaw(IconBookmark) },
  { key: 'gateway', label: '网关', icon: markRaw(IconSettings) },
  { key: 'search', label: '搜索', icon: markRaw(IconSearch) },
  { key: 'json', label: '高级 JSON', icon: markRaw(IconCode) },
];

// ---------- 模型能力元数据（vision / thinking / context，单位 K tokens） ----------
// models 数组保持纯字符串，能力标签通过此处统一查表，未命中的模型默认当纯文本处理。
interface ModelCap { vision?: boolean; thinking?: boolean; context?: number }
const modelCapabilities: Record<string, ModelCap> = {
  // OpenAI GPT-5.6 系列（最新，2026年7月）
  'gpt-5.6-sol': { vision: true, thinking: true, context: 1000 },
  'gpt-5.6-terra': { vision: true, thinking: true, context: 1000 },
  'gpt-5.6-luna': { vision: true, context: 1000 },
  'gpt-4.1': { vision: true, context: 1000 },
  'gpt-4.1-mini': { vision: true, context: 1000 },
  'o3': { vision: true, thinking: true, context: 200 },
  'o4-mini': { vision: true, thinking: true, context: 200 },
  // Anthropic Claude（Fable 5 = Mythos 级最新；Opus 4.7 = Opus 最新）
  'claude-fable-5': { vision: true, thinking: true, context: 1000 },
  'claude-mythos-5': { vision: true, thinking: true, context: 1000 },
  'claude-opus-4-7': { vision: true, thinking: true, context: 200 },
  'claude-opus-4-5': { vision: true, thinking: true, context: 200 },
  'claude-sonnet-4-5': { vision: true, thinking: true, context: 200 },
  'claude-haiku-4-5': { vision: true, thinking: true, context: 200 },
  // DeepSeek
  'deepseek-chat': { context: 1000 },
  'deepseek-reasoner': { thinking: true, context: 64 },
  // 通义千问 Qwen3.7（百炼最新，多模态+思考+编码）
  'qwen3.7-plus': { vision: true, thinking: true, context: 1000 },
  'qwen3.7-max': { vision: true, thinking: true, context: 1000 },
  'qwen3-max': { vision: true, thinking: true, context: 256 },
  'qwen3-plus': { vision: true, thinking: true, context: 256 },
  'qwen3-turbo': { context: 1000 },
  'qwen3-coder-plus': { thinking: true, context: 256 },
  // 智谱 GLM
  'glm-4.6': { thinking: true, context: 200 },
  'glm-4.5': { thinking: true, context: 128 },
  'glm-4.5-air': { thinking: true, context: 128 },
  'glm-4-plus': { context: 128 },
  'glm-4-flash': { context: 128 },
  // Kimi K3（2.8T 参数，1M context，原生多模态）
  'kimi-k3': { vision: true, thinking: true, context: 1000 },
  'kimi-k2-0905-preview': { context: 128 },
  'moonshot-v1-128k': { context: 128 },
  // 豆包
  'doubao-seed-2-0-pro-260215': { thinking: true, context: 256 },
  'doubao-seed-1-5-pro': { thinking: true, context: 256 },
  'doubao-1-5-thinking-pro': { thinking: true, context: 32 },
  'doubao-1-5-vision-pro': { vision: true, context: 32 },
  // 百度 ERNIE
  'ernie-4.0-8k': { context: 8 },
  'ernie-4.0-turbo-8k': { context: 8 },
  'ernie-3.5-8k': { context: 8 },
  // 讯飞
  '4.0Ultra': { context: 8 },
  'generalv3.5': { context: 8 },
  // MiniMax M3（428B/23B 激活，1M context，原生多模态）
  'MiniMax-M3': { vision: true, thinking: true, context: 1000 },
  'MiniMax-Text-01': { context: 1000 },
  'abab6.5s-chat': { context: 245 },
  // 阶跃星辰
  'step-1-8k': { context: 8 },
  'step-1-32k': { context: 32 },
  'step-2-16k': { context: 16 },
  // 腾讯混元
  'hunyuan-pro': { context: 32 },
  'hunyuan-standard': { context: 32 },
  'hunyuan-lite': { context: 256 },
  // Google Gemini（全系列 1M、视觉、思考）
  'gemini-2.5-pro': { vision: true, thinking: true, context: 1000 },
  'gemini-2.5-flash': { vision: true, thinking: true, context: 1000 },
  'gemini-2.5-flash-lite': { vision: true, thinking: true, context: 1000 },
  'gemini-2.0-flash': { vision: true, context: 1000 },
  // Groq（Llama / Gemma 系列，超快推理）
  'llama-3.3-70b-versatile': { context: 128 },
  'llama-3.1-8b-instant': { context: 128 },
  'llama-3.1-70b-versatile': { context: 128 },
  'llama-3.2-3b-preview': { vision: true, context: 128 },
  'llama-3.2-1b-preview': { context: 128 },
  'llama-3.2-11b-vision-preview': { vision: true, context: 128 },
  'llama-3.2-90b-vision-preview': { vision: true, context: 128 },
  'deepseek-r1-distill-llama-70b': { thinking: true, context: 128 },
  'qwen-2.5-32b': { context: 128 },
  'gemma2-9b-it': { context: 8 },
  // 硅基流动 SiliconFlow（聚合）
  'deepseek-ai/DeepSeek-V3': { context: 64 },
  'deepseek-ai/DeepSeek-R1': { thinking: true, context: 64 },
  'Qwen/Qwen3-32B': { thinking: true, context: 128 },
  'zai-org/GLM-4.6': { thinking: true, context: 200 },
  'Kimi/Kimi-K3': { vision: true, thinking: true, context: 1000 },
  'minimax/MiniMax-M3': { vision: true, thinking: true, context: 1000 },
  // 无问芯穹
  'llama-3.3-70b-instruct': { context: 128 },
  'qwen2.5-72b-instruct': { context: 128 },
  // Ollama 常用本地模型
  'llama3.3': { context: 128 },
  'llama3.2': { context: 128 },
  'qwen2.5': { context: 128 },
  'qwen2.5-coder': { context: 128 },
  'qwen3': { thinking: true, context: 128 },
  'deepseek-r1': { thinking: true, context: 128 },
  'gemma3': { vision: true, context: 128 },
  'mistral': { context: 128 },
  'phi4': { context: 16 },
  // OpenRouter（透传命名，按上游能力近似）
  'openai/gpt-5.6-sol': { vision: true, thinking: true, context: 1000 },
  'anthropic/claude-fable-5': { vision: true, thinking: true, context: 1000 },
  'google/gemini-2.5-pro': { vision: true, thinking: true, context: 1000 },
  'minimax/minimax-m3': { vision: true, thinking: true, context: 1000 },
  'moonshotai/kimi-k3': { vision: true, thinking: true, context: 1000 },
};

// 思考等级选项（无/低/中/高）
const thinkingLevels = [
  { value: 'none', label: '无' },
  { value: 'low', label: '低' },
  { value: 'medium', label: '中' },
  { value: 'high', label: '高' },
];

// 模型能力查询：未登记的模型默认纯文本、上下文未知
function modelCap(model: string): ModelCap {
  return modelCapabilities[model] || {};
}
// 模型标签：多模态 / 思考 / 纯文本
function modelTag(model: string): { icon: string; label: string; color: string } {
  const cap = modelCap(model);
  if (cap.vision && cap.thinking) return { icon: '🌐🧠', label: '多模态+思考', color: 'arcoblue' };
  if (cap.vision) return { icon: '🌐', label: '多模态', color: 'green' };
  if (cap.thinking) return { icon: '🧠', label: '思考', color: 'purple' };
  return { icon: '📝', label: '纯文本', color: 'gray' };
}
// 上下文展示（K → M）
function contextLabel(model: string): string {
  const ctx = modelCap(model).context;
  if (!ctx) return '';
  return ctx >= 1000 ? `${(ctx / 1000).toFixed(ctx % 1000 === 0 ? 0 : 1)}M` : `${ctx}K`;
}

// ---------- 20+ Provider 预设（选完自动填充 baseUrl + 模型） ----------
const providerPresets = [
  { id: 'openai', label: 'OpenAI', desc: 'GPT-5.6 Sol/Terra/Luna', color: 'linear-gradient(135deg, #10a37f, #0d8a6c)', baseUrl: 'https://api.openai.com/v1', models: ['gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna', 'gpt-4.1', 'gpt-4.1-mini', 'o3', 'o4-mini'] },
  { id: 'anthropic', label: 'Anthropic', desc: 'Claude Fable 5 / Opus 4.7', color: 'linear-gradient(135deg, #d97757, #c15f3c)', baseUrl: 'https://api.anthropic.com/v1', models: ['claude-fable-5', 'claude-mythos-5', 'claude-opus-4-7', 'claude-opus-4-5', 'claude-sonnet-4-5', 'claude-haiku-4-5'] },
  { id: 'deepseek', label: 'DeepSeek', desc: 'V3 / R1 国产高性价比', color: 'linear-gradient(135deg, #4d6bfe, #3b54d6)', baseUrl: 'https://api.deepseek.com/v1', models: ['deepseek-chat', 'deepseek-reasoner'] },
  { id: 'qwen', label: '通义千问', desc: 'Qwen3.7 百炼 Coding Plan', color: 'linear-gradient(135deg, #615ced, #4a45c4)', baseUrl: 'https://coding.dashscope.aliyuncs.com/v1', models: ['qwen3.7-plus', 'qwen3.7-max', 'qwen3-max', 'qwen3-plus', 'qwen3-turbo', 'qwen3-coder-plus'] },
  { id: 'zhipu', label: '智谱 GLM', desc: 'GLM-4.6 智能体', color: 'linear-gradient(135deg, #3b6cff, #2b54cc)', baseUrl: 'https://open.bigmodel.cn/api/paas/v4', models: ['glm-4.6', 'glm-4.5', 'glm-4.5-air', 'glm-4-plus', 'glm-4-flash'] },
  { id: 'moonshot', label: 'Kimi', desc: 'K3 2.8T 多模态 1M context', color: 'linear-gradient(135deg, #1f1f1f, #3a3a3a)', baseUrl: 'https://api.moonshot.cn/v1', models: ['kimi-k3', 'kimi-k2-0905-preview', 'moonshot-v1-128k'] },
  { id: 'doubao', label: '豆包', desc: 'Seed 2.0 字节火山方舟', color: 'linear-gradient(135deg, #3370ff, #1f5ce6)', baseUrl: 'https://ark.cn-beijing.volces.com/api/v3', models: ['doubao-seed-2-0-pro-260215', 'doubao-seed-1-5-pro', 'doubao-1-5-thinking-pro', 'doubao-1-5-vision-pro'] },
  { id: 'baidu', label: '文心一言', desc: '百度 ERNIE', color: 'linear-gradient(135deg, #2932e1, #1a21b8)', baseUrl: 'https://qianfan.baidubce.com/v2', models: ['ernie-4.0-8k', 'ernie-4.0-turbo-8k', 'ernie-3.5-8k'] },
  { id: 'spark', label: '讯飞星火', desc: '科大讯飞', color: 'linear-gradient(135deg, #2878ff, #1a5fd6)', baseUrl: 'https://spark-api-open.xf-yun.com/v1', models: ['4.0Ultra', 'generalv3.5'] },
  { id: 'minimax', label: 'MiniMax', desc: 'M3 多模态 1M context', color: 'linear-gradient(135deg, #ff6b35, #e05520)', baseUrl: 'https://api.minimax.chat/v1', models: ['MiniMax-M3', 'MiniMax-Text-01', 'abab6.5s-chat'] },
  { id: 'stepfun', label: '阶跃星辰', desc: 'Step 系列', color: 'linear-gradient(135deg, #7046cc, #5a36a8)', baseUrl: 'https://api.stepfun.com/v1', models: ['step-2-16k', 'step-1-32k'] },
  { id: 'hunyuan', label: '腾讯混元', desc: 'Hunyuan', color: 'linear-gradient(135deg, #00c8dc, #00a3b8)', baseUrl: 'https://api.hunyuan.cloud.tencent.com/v1', models: ['hunyuan-pro', 'hunyuan-standard', 'hunyuan-lite'] },
  { id: 'sensenova', label: '商汤日日新', desc: 'SenseNova', color: 'linear-gradient(135deg, #00b42a, #009a22)', baseUrl: 'https://api.sensenova.cn/v1', models: ['SenseChat-5', 'SenseChat-Turbo'] },
  { id: 'skywork', label: '天工', desc: 'Skywork', color: 'linear-gradient(135deg, #722ed1, #5a1fb8)', baseUrl: 'https://api.skywork.ai/v1', models: ['skywork-o1-preview'] },
  { id: 'siliconflow', label: '硅基流动', desc: '聚合 DeepSeek-V3/GLM-4.6/Kimi-K3', color: 'linear-gradient(135deg, #8c57ff, #7046cc)', baseUrl: 'https://api.siliconflow.cn/v1', models: ['deepseek-ai/DeepSeek-V3', 'deepseek-ai/DeepSeek-R1', 'Qwen/Qwen3-32B', 'zai-org/GLM-4.6', 'Kimi/Kimi-K3', 'minimax/MiniMax-M3'] },
  { id: 'infinigence', label: '无问芯穹', desc: 'Infinigence', color: 'linear-gradient(135deg, #ff7d00, #e06700)', baseUrl: 'https://cloud.infini-ai.com/maas/v1', models: ['llama-3.3-70b-instruct', 'qwen2.5-72b-instruct'] },
  { id: 'groq', label: 'Groq', desc: 'Llama 超快推理', color: 'linear-gradient(135deg, #f55036, #d63d26)', baseUrl: 'https://api.groq.com/openai/v1', models: ['llama-3.3-70b-versatile', 'llama-3.1-8b-instant', 'deepseek-r1-distill-llama-70b', 'qwen-2.5-32b'] },
  { id: 'openrouter', label: 'OpenRouter', desc: '聚合路由（可调所有模型）', color: 'linear-gradient(135deg, #6366f1, #4f46e5)', baseUrl: 'https://openrouter.ai/api/v1', models: ['openai/gpt-5.6-sol', 'anthropic/claude-fable-5', 'google/gemini-2.5-pro', 'minimax/minimax-m3', 'moonshotai/kimi-k3'] },
  { id: 'gemini', label: 'Gemini', desc: 'Google 2.5 Pro/Flash 1M', color: 'linear-gradient(135deg, #1c69d4, #1553ab)', baseUrl: 'https://generativelanguage.googleapis.com/v1beta/openai', models: ['gemini-2.5-pro', 'gemini-2.5-flash', 'gemini-2.5-flash-lite'] },
  { id: 'ollama', label: 'Ollama', desc: '本地运行', color: 'linear-gradient(135deg, #6d6777, #4d4868)', baseUrl: 'http://127.0.0.1:11434/v1', models: ['llama3.3', 'qwen2.5', 'qwen3', 'deepseek-r1', 'gemma3', 'mistral'] },
  // 自定义：用户自己填 baseUrl / model / provider type
  { id: 'custom', label: '自定义', desc: '手动填写端点与模型', color: 'linear-gradient(135deg, #8c8c8c, #595959)', baseUrl: '', models: [] as string[], custom: true },
];

interface ProviderForm {
  name: string; apiKey: string; baseUrl: string; model: string;
  enabled: boolean; supportsThinking: boolean; thinkingLevel: string;
  providerType?: string; // 自定义 provider 的 OpenAI 兼容协议类型
  _testing?: boolean; _testResult?: any;
}

const formData = reactive({
  providers: [] as ProviderForm[],
  gateway: { bind: 'loopback', token: '', port: 18800 },
  embedding: { provider: 'local', model: 'BAAI/bge-small-zh-v1.5', apiKey: '', baseUrl: 'https://api.siliconflow.cn/v1' },
  search: { default: 'duckduckgo', searxngUrl: '', braveKey: '', tavilyKey: '' },
});

function providerPreset(name: string) {
  return providerPresets.find((p) => p.id === name);
}

function addProviderFromPreset(preset: any) {
  // 已存在则提示，不重复添加
  if (formData.providers.some((p) => p.name === preset.id)) {
    Message.info(`${preset.label} 已添加，直接在上方卡片中填写 API Key 即可`);
    return;
  }
  formData.providers.push({
    name: preset.id,
    apiKey: '',
    baseUrl: preset.baseUrl,
    model: preset.models[0] || '',
    enabled: true,
    supportsThinking: false,
    thinkingLevel: 'none',
    providerType: preset.custom ? 'openai' : undefined,
  });
  Message.success(`已添加 ${preset.label}，请填写 API Key 后测试连接`);
}

// 当前模型是否原生支持思考
function modelSupportsThinking(p: ProviderForm): boolean {
  return !!modelCap(p.model).thinking;
}
// 当模型切换到支持思考的模型时，自动把思考等级默认设为"中"
function onModelChange(p: ProviderForm) {
  if (modelSupportsThinking(p) && p.thinkingLevel === 'none') {
    p.thinkingLevel = 'medium';
    p.supportsThinking = true;
  } else if (!modelSupportsThinking(p)) {
    p.thinkingLevel = 'none';
    p.supportsThinking = false;
  }
}

async function testProvider(p: ProviderForm) {
  if (!p.apiKey) {
    Message.warning('请先填写 API Key');
    return;
  }
  p._testing = true;
  p._testResult = null;
  try {
    const res = await rpc.call<any>('providers.test', {
      provider: p.name, baseUrl: p.baseUrl, apiKey: p.apiKey, model: p.model,
    });
    p._testResult = res;
    if (res.connected) {
      Message.success(`${providerPreset(p.name)?.label || p.name} 连接成功`);
    } else {
      Message.error(`连接失败：${res.error}`);
    }
  } catch (e: any) {
    p._testResult = { connected: false, error: e.message };
    Message.error(e.message);
  } finally {
    p._testing = false;
  }
}

async function loadConfig() {
  try {
    const res = await rpc.call<{ config: any }>('config.get');
    const cfg = res.config || {};
    jsonText.value = JSON.stringify(cfg, null, 2);

    // providers
    const providers = cfg.providers || {};
    formData.providers = Object.entries(providers)
      .filter(([k, v]: [string, any]) => v && typeof v === 'object' && !['search', 'tts', 'stt', 'embedding', 'rerank', 'vision', 'image'].includes(k))
      .map(([k, v]: [string, any]) => ({
        name: k,
        apiKey: v.apiKey || '',
        baseUrl: v.baseUrl || providerPreset(k)?.baseUrl || '',
        model: v.model || providerPreset(k)?.models?.[0] || '',
        enabled: v.enabled !== false,
        supportsThinking: v.supportsThinking || false,
        thinkingLevel: v.thinkingLevel || (v.supportsThinking ? 'medium' : 'none'),
        providerType: v.providerType || (providerPreset(k)?.custom ? 'openai' : undefined),
      }));

    // gateway
    formData.gateway.bind = cfg.gateway?.bind || (cfg.gateway?.host === '0.0.0.0' ? 'all' : 'loopback');
    formData.gateway.token = cfg.gateway?.token || cfg.gateway?.auth?.token || '';
    formData.gateway.port = cfg.gateway?.port || 18800;

    // embedding
    const emb = cfg.memory?.embedding || cfg.embedding || {};
    formData.embedding.provider = emb.provider || 'local';
    formData.embedding.model = emb.model || (emb.provider === 'siliconflow' ? 'Qwen/Qwen3-VL-Embedding-8B' : 'BAAI/bge-small-zh-v1.5');
    formData.embedding.apiKey = emb.apiKey || '';
    formData.embedding.baseUrl = emb.baseUrl || 'https://api.siliconflow.cn/v1';

    // search
    formData.search.default = cfg.tools?.web?.search?.default || cfg.search?.default || 'duckduckgo';
    formData.search.searxngUrl = cfg.tools?.web?.search?.searxngUrl || cfg.search?.searxngUrl || '';
    formData.search.braveKey = cfg.tools?.web?.search?.braveKey || cfg.search?.braveKey || '';
    formData.search.tavilyKey = cfg.tools?.web?.search?.tavilyKey || cfg.search?.tavilyKey || '';
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function saveAll() {
  saving.value = true;
  try {
    if (activeTab.value === 'json') {
      await rpc.call('config.set', { config: JSON.parse(jsonText.value) });
      Message.success('已保存（JSON 模式）');
      saving.value = false;
      return;
    }
    // 组装配置
    const providers: Record<string, any> = {};
    for (const p of formData.providers) {
      if (p.name.trim()) {
        providers[p.name] = {
          apiKey: p.apiKey || undefined,
          baseUrl: p.baseUrl || undefined,
          model: p.model || undefined,
          enabled: p.enabled,
          supportsThinking: p.supportsThinking || undefined,
          thinkingLevel: p.thinkingLevel && p.thinkingLevel !== 'none' ? p.thinkingLevel : undefined,
          ...(p.providerType ? { providerType: p.providerType } : {}),
        };
      }
    }
    // 主模型 = 第一个启用的 provider 的模型
    const firstEnabled = formData.providers.find((p) => p.enabled);
    const primaryModel = firstEnabled?.model || 'gpt-4o-mini';

    const config = {
      gateway: {
        bind: formData.gateway.bind,
        port: formData.gateway.port,
        auth: { mode: 'token', token: formData.gateway.token },
      },
      models: { primary: primaryModel },
      providers,
      memory: {
        engine: 'builtin',
        embedding: formData.embedding.provider === 'local'
          ? { provider: 'local', model: formData.embedding.model }
          : { provider: 'siliconflow', model: formData.embedding.model, baseUrl: formData.embedding.baseUrl, apiKey: formData.embedding.apiKey },
      },
      tools: { web: { search: { default: formData.search.default, searxngUrl: formData.search.searxngUrl, braveKey: formData.search.braveKey, tavilyKey: formData.search.tavilyKey } } },
    };
    await rpc.call('config.set', { config });
    Message.success('已保存，重启网关后生效');
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    saving.value = false;
  }
}

onMounted(loadConfig);
</script>

<style lang="less" scoped>
/* pill 标签页（Materialize account-settings） */
.pill-tabs {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.pill-tab {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 9px 18px;
  border-radius: 8px;
  font-size: 14px;
  font-weight: 500;
  color: var(--color-text-2);
  cursor: pointer;
  transition: all 0.2s;
  border: 1px solid transparent;
  svg { font-size: 16px; }
  &:hover {
    background: var(--color-bg-1);
    color: var(--brand-primary);
  }
  &.active {
    background: var(--brand-primary);
    color: #fff;
    box-shadow: var(--shadow-xs);
  }
}

/* Provider 卡片 */
.provider-card {
  .provider-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .provider-title {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .provider-icon {
    width: 42px;
    height: 42px;
    border-radius: 10px;
    color: #fff;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
    font-weight: 700;
    box-shadow: var(--shadow-xs);
  }
  .provider-name {
    font-size: 15px;
    font-weight: 600;
    color: var(--color-text-1);
  }
  .provider-url {
    font-size: 12px;
    color: var(--color-text-4);
    margin-top: 2px;
    font-family: monospace;
  }
  .provider-test {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 8px;
    padding-top: 12px;
    border-top: 1px dashed var(--color-border-1);
  }
  .test-result {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    &.ok { color: var(--brand-success); }
    &.fail { color: var(--brand-danger); }
  }
}

/* 预设网格 */
.add-provider-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text-1);
  margin-bottom: 16px;
}
.preset-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
  gap: 12px;
}
.preset-item {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 16px 8px;
  border: 1px solid var(--color-border-1);
  border-radius: 8px;
  cursor: pointer;
  transition: all 0.2s;
  text-align: center;
  &:hover {
    border-color: var(--brand-primary);
    box-shadow: var(--card-shadow-hover);
    transform: translateY(-2px);
  }
}
.preset-icon {
  width: 44px;
  height: 44px;
  border-radius: 10px;
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 19px;
  font-weight: 700;
  margin-bottom: 10px;
  box-shadow: var(--shadow-xs);
}
.preset-label {
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text-1);
}
.preset-desc {
  font-size: 11px;
  color: var(--color-text-4);
  margin-top: 3px;
}

.mb-16 { margin-bottom: 16px; }

/* 模型能力标签回显 */
.model-cap-hint {
  display: flex;
  gap: 6px;
  margin-top: 6px;
  flex-wrap: wrap;
}
</style>
