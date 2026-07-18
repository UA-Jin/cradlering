<template>
  <div class="page-container">
    <a-page-header title="渠道管理" subtitle="40+ IM 渠道真实连接，一键配置" :show-back="false" />

    <a-row :gutter="16" class="mt-16">
      <a-col v-for="ch in channels" :key="ch.id" :xs="24" :sm="12" :md="8" :lg="6">
        <a-card hoverable class="channel-card" @click="openEdit(ch)">
          <div class="ch-card">
            <a-avatar :size="48" :style="{ backgroundColor: ch.color }">{{ ch.label.charAt(0) }}</a-avatar>
            <div class="ch-info">
              <div class="ch-name">
                {{ ch.label }}
                <a-badge :status="ch.state?.enabled ? (ch.state?.status === 'connected' ? 'success' : (ch.state?.status === 'error' ? 'danger' : 'warning')) : 'default'" />
              </div>
              <div class="ch-status">{{ statusText(ch) }}</div>
              <div class="ch-stats" v-if="ch.state?.enabled">
                <span>收 {{ ch.state?.receivedCount || 0 }}</span>
                <span>发 {{ ch.state?.sentCount || 0 }}</span>
              </div>
            </div>
            <a-switch v-model="ch.enabled" @change="toggle(ch)" @click.stop />
          </div>
        </a-card>
      </a-col>
    </a-row>

    <!-- 配置抽屉 -->
    <a-drawer :visible="visible" :width="560" @cancel="visible = false" @ok="save" :ok-loading="saving">
      <template #title>
        <div class="drawer-title">
          <a-avatar :size="32" :style="{ backgroundColor: current?.color }">{{ current?.label.charAt(0) }}</a-avatar>
          <span>配置 {{ current?.label }}</span>
        </div>
      </template>
      <a-form v-if="current" :model="current" layout="vertical">
        <!-- 启用开关 -->
        <a-form-item label="启用该渠道">
          <a-switch v-model="current.enabled" />
        </a-form-item>

        <!-- Webhook URL（一键复制） -->
        <a-form-item label="Webhook 回调地址">
          <a-space>
            <a-input :model-value="webhookUrl" disabled style="flex: 1" />
            <a-button @click="copyWebhook"><template #icon><icon-copy /></template>复制</a-button>
          </a-space>
          <div class="hint">将此 URL 配置到 {{ current.label }} 的回调设置中</div>
        </a-form-item>

        <!-- Webhook 密钥（防伪造消息注入，可选但推荐） -->
        <a-form-item label="Webhook 密钥（防伪造）">
          <a-space>
            <a-input-password v-model="current.config.webhookSecret" placeholder="留空则不校验（不推荐）" allow-clear style="flex: 1" />
            <a-button @click="genSecret"><template #icon><icon-refresh /></template>生成</a-button>
          </a-space>
          <div class="hint">配置后回调 URL 会带上该密钥，不知道密钥的人无法伪造消息注入到 Agent。修改后需重新复制上方 URL</div>
        </a-form-item>

        <!-- 渠道专属字段 -->
        <a-divider>连接配置</a-divider>
        <template v-for="field in current.fields" :key="field.key">
          <a-form-item :label="field.label" :required="field.required">
            <a-input-password v-if="field.type === 'password'" v-model="current.config[field.key]" :placeholder="field.placeholder" allow-clear />
            <a-input v-else v-model="current.config[field.key]" :placeholder="field.placeholder" allow-clear />
            <div v-if="field.hint" class="hint">{{ field.hint }}</div>
          </a-form-item>
        </template>

        <!-- 测试按钮 -->
        <a-form-item>
          <a-button type="outline" @click="testConnection" :loading="testing">
            <template #icon><icon-thunder /></template>
            测试连接
          </a-button>
          <span v-if="testResult" class="test-result" :class="testResult.ok ? 'ok' : 'fail'">
            {{ testResult.message }}
          </span>
        </a-form-item>
      </a-form>
    </a-drawer>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { Message } from '@arco-design/web-vue';
import { rpc } from '@/api/rpc';

interface ChannelField {
  key: string;
  label: string;
  type: 'text' | 'password';
  placeholder: string;
  required: boolean;
  hint?: string;
}

interface Channel {
  id: string;
  label: string;
  color: string;
  enabled: boolean;
  fields: ChannelField[];
  config: Record<string, any>;
  state?: any;
}

// 渠道定义（含字段模板）
const channelDefs: Omit<Channel, 'enabled' | 'config' | 'state'>[] = [
  {
    id: 'feishu', label: '飞书', color: '#3370ff',
    fields: [
      { key: 'appId', label: 'App ID', type: 'text', placeholder: 'cli_a5xxxxx', required: true },
      { key: 'appSecret', label: 'App Secret', type: 'password', placeholder: 'App Secret', required: true },
      { key: 'verificationToken', label: 'Verification Token', type: 'password', placeholder: '事件订阅验证 Token（可选）', required: false, hint: '飞书开放平台 → 事件订阅 → 验证 Token' },
    ],
  },
  {
    id: 'dingtalk', label: '钉钉', color: '#1677ff',
    fields: [
      { key: 'appKey', label: 'App Key', type: 'text', placeholder: 'dingxxxxxx', required: true },
      { key: 'appSecret', label: 'App Secret', type: 'password', placeholder: 'App Secret', required: true },
    ],
  },
  {
    id: 'wecom', label: '企业微信', color: '#07c160',
    fields: [
      { key: 'corpId', label: 'Corp ID', type: 'text', placeholder: 'wwxxxxxx', required: true },
      { key: 'agentId', label: 'Agent ID', type: 'text', placeholder: '1000002', required: true },
      { key: 'secret', label: 'Secret', type: 'password', placeholder: '应用 Secret', required: true },
    ],
  },
  {
    id: 'telegram', label: 'Telegram', color: '#2aabee',
    fields: [
      { key: 'botToken', label: 'Bot Token', type: 'password', placeholder: '123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11', required: true, hint: '@BotFather 创建 bot 后获取' },
    ],
  },
  {
    id: 'discord', label: 'Discord', color: '#5865f2',
    fields: [
      { key: 'botToken', label: 'Bot Token', type: 'password', placeholder: 'MTIzNDU2Nzg5MA.Gxxxxx.xxxxx', required: true, hint: 'Discord Developer Portal → Bot → Token' },
    ],
  },
  {
    id: 'slack', label: 'Slack', color: '#4a154b',
    fields: [
      { key: 'botToken', label: 'Bot Token', type: 'password', placeholder: 'xoxb-xxxxxxxxx', required: true },
      { key: 'signingSecret', label: 'Signing Secret', type: 'password', placeholder: '签名密钥（可选）', required: false },
    ],
  },
  {
    id: 'whatsapp', label: 'WhatsApp', color: '#25d366',
    fields: [
      { key: 'phoneNumberId', label: 'Phone Number ID', type: 'text', placeholder: '1234567890', required: true },
      { key: 'accessToken', label: 'Access Token', type: 'password', placeholder: 'EAAxxxxx', required: true },
      { key: 'verifyToken', label: 'Verify Token', type: 'password', placeholder: '自定义验证 Token', required: true },
    ],
  },
  {
    id: 'signal', label: 'Signal', color: '#3a76f0',
    fields: [
      { key: 'number', label: '手机号', type: 'text', placeholder: '+1234567890', required: true },
    ],
  },
  {
    id: 'qq', label: 'QQ', color: '#12b7f5',
    fields: [
      { key: 'appId', label: 'App ID', type: 'text', placeholder: '102000000', required: true },
      { key: 'appSecret', label: 'App Secret', type: 'password', placeholder: 'App Secret', required: true },
    ],
  },
  {
    id: 'matrix', label: 'Matrix', color: '#0dbd8b',
    fields: [
      { key: 'homeserver', label: 'Homeserver', type: 'text', placeholder: 'https://matrix.org', required: true },
      { key: 'accessToken', label: 'Access Token', type: 'password', placeholder: 'syt_xxxx', required: true },
    ],
  },
  {
    id: 'teams', label: 'Teams', color: '#5059c9',
    fields: [
      { key: 'appId', label: 'App ID', type: 'text', placeholder: 'xxxx-xxxx-xxxx', required: true },
      { key: 'appPassword', label: 'App Password', type: 'password', placeholder: 'App Password', required: true },
    ],
  },
  {
    id: 'webhook', label: 'Webhook', color: '#86909c',
    fields: [
      { key: 'url', label: 'Webhook URL', type: 'text', placeholder: 'https://your-server.com/webhook', required: true },
    ],
  },
];

const channels = ref<Channel[]>([]);
const visible = ref(false);
const saving = ref(false);
const testing = ref(false);
const current = ref<Channel | null>(null);
const testResult = ref<{ ok: boolean; message: string } | null>(null);

const webhookUrl = computed(() => {
  if (!current.value) return '';
  const base = window.location.origin;
  const secret = current.value.config?.webhookSecret;
  // 配置了密钥则带在路径里（防伪造消息注入）
  return secret ? `${base}/webhook/${current.value.id}/${secret}` : `${base}/webhook/${current.value.id}`;
});

function statusText(ch: Channel) {
  if (!ch.state?.enabled) return '未启用';
  const s = ch.state?.status;
  return ({ configured: '已配置', connected: '已连接', disconnected: '未连接', error: '错误', polling: '轮询中' } as any)[s] || s;
}

async function load() {
  try {
    const [channelsRes, statesRes] = await Promise.all([
      rpc.call<{ channels: any[] }>('channels.list').catch(() => ({ channels: [] })),
      rpc.call<{ states: any }>('channels.states').catch(() => ({ states: {} })),
    ]);
    const cfgMap = new Map((channelsRes.channels || []).map((c: any) => [c.id, c]));
    channels.value = channelDefs.map((d) => {
      const cfg = cfgMap.get(d.id) || {};
      return {
        ...d,
        enabled: !!cfg.enabled,
        config: { ...cfg },
        state: statesRes.states?.[d.id] || { status: cfg.enabled ? 'configured' : 'disconnected', enabled: !!cfg.enabled },
      };
    });
  } catch (e) {
    channels.value = channelDefs.map((d) => ({ ...d, enabled: false, config: {} }));
  }
}

function openEdit(ch: Channel) {
  current.value = JSON.parse(JSON.stringify(ch));
  testResult.value = null;
  visible.value = true;
}

async function toggle(ch: Channel) {
  try {
    await rpc.call('channels.set', { id: ch.id, enabled: ch.enabled });
    Message.success(ch.enabled ? '已启用' : '已禁用');
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function save() {
  if (!current.value) return;
  saving.value = true;
  try {
    // 校验必填字段
    const missing = current.value.fields.filter((f) => f.required && !current.value!.config[f.key]);
    if (missing.length > 0) {
      Message.warning(`请填写必填字段：${missing.map((f) => f.label).join('、')}`);
      saving.value = false;
      return;
    }
    await rpc.call('channels.set', {
      id: current.value.id,
      enabled: current.value.enabled,
      config: current.value.config,
    });
    Message.success('已保存');
    visible.value = false;
    await load();
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    saving.value = false;
  }
}

async function testConnection() {
  if (!current.value) return;
  // 必填字段校验：不输入不允许测试（防止空配置也"测试通过"的假象）
  const missing = current.value.fields.filter((f) => f.required && !String(current.value!.config[f.key] || '').trim());
  if (missing.length) {
    Message.warning(`请先填写必填项：${missing.map((f) => f.label).join('、')}`);
    testResult.value = { ok: false, message: `缺少必填项：${missing.map((f) => f.label).join('、')}` };
    return;
  }
  testing.value = true;
  testResult.value = null;
  try {
    // 后端真实连接测试：按渠道类型调平台 API 验证凭据（飞书 tenant_token / Telegram getMe / Discord @me / 钉钉 gettoken ...）
    const res = await rpc.call<any>('channels.test', {
      id: current.value.id,
      config: current.value.config,
    });
    if (res.connected) {
      testResult.value = { ok: true, message: res.info || '连接成功' };
      Message.success(res.info || '连接成功');
    } else {
      testResult.value = { ok: false, message: res.error || '连接失败' };
      Message.error(res.error || '连接失败');
    }
  } catch (e: any) {
    testResult.value = { ok: false, message: e.message };
    Message.error(e.message);
  } finally {
    testing.value = false;
  }
}

function copyWebhook() {
  navigator.clipboard.writeText(webhookUrl.value).then(() => {
    Message.success('已复制到剪贴板');
  });
}

function genSecret() {
  if (!current.value) return;
  // 生成 32 位随机密钥（URL 安全字符）
  const chars = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  const arr = new Uint8Array(24);
  crypto.getRandomValues(arr);
  current.value.config.webhookSecret = Array.from(arr).map((b) => chars[b % chars.length]).join('');
  Message.success('已生成新密钥，请保存配置并重新复制 Webhook URL');
}

onMounted(load);
</script>

<style lang="less" scoped>
.channel-card {
  cursor: pointer;
  transition: transform 0.2s, box-shadow 0.2s;
  &:hover {
    transform: translateY(-4px);
  }
}
.ch-card {
  display: flex;
  align-items: center;
  gap: 16px;
}
.ch-info {
  flex: 1;
}
.ch-name {
  font-weight: 600;
  font-size: 15px;
  color: var(--color-text-1);
}
.ch-status {
  font-size: 12px;
  color: var(--color-text-3);
  margin-top: 4px;
}
.ch-stats {
  display: flex;
  gap: 12px;
  font-size: 11px;
  color: var(--color-text-3);
  margin-top: 8px;
}
.drawer-title {
  display: flex;
  align-items: center;
  gap: 12px;
}
.hint {
  font-size: 12px;
  color: var(--color-text-3);
  margin-top: 4px;
}
.test-result {
  margin-left: 12px;
  font-size: 13px;
  &.ok { color: var(--color-success); }
  &.fail { color: var(--color-danger); }
}
</style>
