<template>
  <div class="chat-page">
    <!-- 会话侧栏（Materialize app-chat 结构） -->
    <div class="chat-sidebar">
      <!-- 顶部：头像 + 搜索 -->
      <div class="sidebar-top">
        <div class="my-avatar">
          {{ (userStore.user?.displayName || 'U').charAt(0).toUpperCase() }}
          <span class="avatar-status"></span>
        </div>
        <a-input v-model="searchKey" placeholder="搜索会话..." allow-clear class="sidebar-search">
          <template #prefix><icon-search /></template>
        </a-input>
      </div>

      <!-- 会话列表 -->
      <div class="sidebar-list">
        <div class="list-group-title">
          <span>会话</span>
          <a-tooltip content="新建会话">
            <button class="new-chat-btn" @click="newSession"><icon-plus /></button>
          </a-tooltip>
        </div>
        <div
          v-for="s in filteredSessions"
          :key="s.key"
          class="contact-item"
          :class="{ active: s.key === currentKey }"
          @click="selectSession(s.key)"
        >
          <div class="contact-avatar" :style="{ background: avatarBg(s.key) }">
            {{ (s.displayName || s.key).charAt(0).toUpperCase() }}
            <span class="contact-status" :class="s.kind"></span>
          </div>
          <div class="contact-info">
            <div class="contact-row">
              <span class="contact-name">{{ s.displayName || s.key }}</span>
              <span class="contact-time">{{ shortTime(s.updatedAt) }}</span>
            </div>
            <div class="contact-preview">{{ s.lastMessage || s.kind }}</div>
          </div>
        </div>
        <a-empty v-if="!filteredSessions.length" description="暂无会话，点击 + 新建" />
      </div>
    </div>

    <!-- 主聊天区 -->
    <div class="chat-main">
      <!-- 头部 -->
      <div class="chat-header">
        <div class="ch-info">
          <div class="ch-avatar" :style="{ background: avatarBg(currentKey) }">
            {{ (currentSession?.displayName || currentKey || '?').charAt(0).toUpperCase() }}
          </div>
          <div>
            <div class="ch-name">{{ currentSession?.displayName || currentKey || '请选择会话' }}</div>
            <div class="ch-status">{{ sending ? '正在输入...' : (currentKey ? '在线' : '') }}</div>
          </div>
        </div>
        <a-space>
          <a-tooltip content="压缩会话（无损摘要）">
            <button class="ch-action" :disabled="!currentKey" @click="compact"><icon-compress /></button>
          </a-tooltip>
          <a-tooltip content="重命名会话">
            <button class="ch-action" :disabled="!currentKey" @click="renameVisible = true"><icon-edit /></button>
          </a-tooltip>
          <a-popconfirm content="确认删除该会话？" @ok="deleteSession">
            <a-tooltip content="删除会话">
              <button class="ch-action danger" :disabled="!currentKey"><icon-delete /></button>
            </a-tooltip>
          </a-popconfirm>
        </a-space>
      </div>

      <!-- 消息区 -->
      <div class="chat-history" ref="messagesEl">
        <!-- 空状态（Materialize 大圆图标） -->
        <div v-if="!currentKey" class="chat-empty">
          <div class="empty-icon"><icon-message /></div>
          <p>选择左侧会话，或新建一个开始对话</p>
        </div>
        <div v-else-if="!messages.length && !sending" class="chat-empty">
          <div class="empty-icon"><icon-message /></div>
          <p>暂无消息，发送第一条开始对话吧</p>
        </div>

        <!-- 消息气泡 -->
        <template v-for="(m, idx) in messages" :key="m.ts + '-' + idx">
          <!-- 日期分隔 -->
          <div v-if="showDateDivider(idx)" class="date-divider">
            <span>{{ dayjs(m.ts).format('MM月DD日') }}</span>
          </div>
          <div class="chat-message" :class="{ 'msg-right': m.role === 'user' }">
            <div class="msg-avatar" :style="{ background: m.role === 'user' ? 'var(--brand-primary)' : avatarBg(currentKey) }">
              {{ m.role === 'user' ? (userStore.user?.displayName || '我').charAt(0).toUpperCase() : 'AI' }}
            </div>
            <div class="msg-wrapper">
              <div class="msg-bubble" v-html="renderMd(m.content)"></div>
              <div class="msg-meta">
                <icon-check-double v-if="m.role === 'user'" class="read-icon" />
                <span>{{ dayjs(m.ts).format('HH:mm') }}</span>
              </div>
            </div>
          </div>
        </template>

        <!-- 正在输入 -->
        <div v-if="sending" class="chat-message">
          <div class="msg-avatar" :style="{ background: avatarBg(currentKey) }">AI</div>
          <div class="msg-wrapper">
            <div class="msg-bubble typing-bubble">
              <span class="typing-dot"></span><span class="typing-dot"></span><span class="typing-dot"></span>
            </div>
          </div>
        </div>
      </div>

      <!-- 输入区 -->
      <div class="chat-footer">
        <a-textarea
          v-model="input"
          :auto-size="{ minRows: 1, maxRows: 5 }"
          placeholder="输入消息，Enter 发送，Shift+Enter 换行"
          @keydown.enter.exact.prevent="send"
          :disabled="!currentKey || sending"
          class="msg-input"
        />
        <a-tooltip content="发送">
          <button class="send-btn" :disabled="!currentKey || !input.trim() || sending" @click="send">
            <icon-send v-if="!sending" />
            <icon-loading v-else />
          </button>
        </a-tooltip>
      </div>
    </div>

    <!-- 重命名对话框 -->
    <a-modal :visible="renameVisible" title="重命名会话" @cancel="renameVisible = false" @ok="onRename" :width="420">
      <a-input v-model="renameText" placeholder="输入新的会话名称" allow-clear />
    </a-modal>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick, watch } from 'vue';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import 'dayjs/locale/zh-cn';
import { rpc } from '@/api/rpc';
import { Message } from '@arco-design/web-vue';
import { useUserStore } from '@/stores/user';
import {
  IconSearch, IconPlus, IconMessage, IconSend, IconLoading, IconDelete,
  IconArchive, IconEdit, IconCheck,
} from '@arco-design/web-vue/es/icon';

dayjs.extend(relativeTime);
dayjs.locale('zh-cn');

interface SessionInfo { key: string; kind: string; displayName?: string; updatedAt: number; lastMessage?: string; }
interface ChatMsg { role: string; content: string; ts: number; }

const userStore = useUserStore();
const sessions = ref<SessionInfo[]>([]);
const searchKey = ref('');
const currentKey = ref('');
const currentSession = computed(() => sessions.value.find((s) => s.key === currentKey.value));
const filteredSessions = computed(() =>
  sessions.value.filter((s) =>
    !searchKey.value || (s.displayName || s.key).toLowerCase().includes(searchKey.value.toLowerCase()),
  ),
);

const messages = ref<ChatMsg[]>([]);
const input = ref('');
const sending = ref(false);
const messagesEl = ref<HTMLElement>();
const renameVisible = ref(false);
const renameText = ref('');

// 头像背景（按 key 稳定取色）
const avatarColors = ['#8c57ff', '#16b1ff', '#56ca00', '#ffb400', '#ff4c51', '#7340e0'];
function avatarBg(seed: string): string {
  let h = 0;
  for (const c of seed) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return avatarColors[h % avatarColors.length];
}

function shortTime(ts: number): string {
  const d = dayjs(ts);
  const now = dayjs();
  if (d.isSame(now, 'day')) return d.format('HH:mm');
  if (d.isSame(now.subtract(1, 'day'), 'day')) return '昨天';
  if (d.isAfter(now.subtract(7, 'day'))) return d.format('ddd');
  return d.format('MM-DD');
}

function showDateDivider(idx: number): boolean {
  if (idx === 0) return true;
  const prev = messages.value[idx - 1];
  const cur = messages.value[idx];
  return !dayjs(prev.ts).isSame(dayjs(cur.ts), 'day');
}

function renderMd(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/```(\w*)\n?([\s\S]*?)```/g, (_, lang, code) => `<pre class="code-block"><code>${code.trim()}</code></pre>`)
    .replace(/`([^`]+)`/g, '<code class="inline-code">$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/\n/g, '<br>');
}

async function loadSessions() {
  try {
    const res = await rpc.call<{ sessions: SessionInfo[] }>('sessions.list');
    sessions.value = (res.sessions || []).sort((a, b) => b.updatedAt - a.updatedAt);
    if (!currentKey.value && sessions.value.length) {
      await selectSession(sessions.value[0].key);
    }
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function selectSession(key: string) {
  currentKey.value = key;
  try {
    const res = await rpc.call<{ messages: any[] }>('chat.history', { sessionKey: key });
    messages.value = (res.messages || []).map((m: any) => ({ role: m.role, content: m.content, ts: m.timestamp }));
    await scrollBottom();
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function newSession() {
  const key = `web-${Date.now().toString(36)}`;
  try {
    await rpc.call('sessions.create', {
      key, kind: 'web', displayName: `新会话 ${dayjs().format('MM-DD HH:mm')}`, agentId: 'main',
    });
    await loadSessions();
    await selectSession(key);
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function send() {
  if (!currentKey.value || !input.value.trim()) return;
  const text = input.value.trim();
  input.value = '';
  sending.value = true;
  messages.value.push({ role: 'user', content: text, ts: Date.now() });
  await scrollBottom();

  try {
    // chat.send：触发 Cache-First agent loop（流式结果通过 chat 事件 deltaText 推送）
    const res = await rpc.call<any>('chat.send', { sessionKey: currentKey.value, message: text });
    if (res.ok === false) {
      messages.value.push({ role: 'assistant', content: `错误：${res.error?.message || '发送失败'}`, ts: Date.now() });
      sending.value = false;
    }
    // 流式：等待 chat.complete 事件结束 sending（onMounted 里订阅）
  } catch (e: any) {
    messages.value.push({ role: 'assistant', content: `错误：${e.message}`, ts: Date.now() });
    sending.value = false;
  }
}

async function compact() {
  try {
    await rpc.call('sessions.compact', { sessionKey: currentKey.value });
    Message.success('已压缩');
    await selectSession(currentKey.value);
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function deleteSession() {
  try {
    await rpc.call('sessions.delete', { sessionKey: currentKey.value });
    Message.success('已删除');
    currentKey.value = '';
    messages.value = [];
    await loadSessions();
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function onRename() {
  if (!renameText.value.trim()) { renameVisible.value = false; return; }
  try {
    await rpc.call('sessions.patch', { sessionKey: currentKey.value, displayName: renameText.value.trim() });
    Message.success('已重命名');
    renameVisible.value = false;
    await loadSessions();
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function scrollBottom() {
  await nextTick();
  if (messagesEl.value) messagesEl.value.scrollTop = messagesEl.value.scrollHeight;
}

let unsubs: (() => void)[] = [];

// 首次引导已移至个人中心页面（不再侵入聊天）

onMounted(async () => {
  await loadSessions();
  // 首次引导已移至个人中心（不再劫持聊天页面）
  // 订阅流式增量（Cache-First agent loop 推送 deltaText）
  unsubs.push(rpc.on('chat', (p: any) => {
    if (p.sessionKey !== currentKey.value) return;
    if (p.deltaText) {
      const last = messages.value[messages.value.length - 1];
      if (last && last.role === 'assistant') {
        last.content = p.deltaText;  // deltaText 是累计文本（后端每次推全量）
      } else {
        messages.value.push({ role: 'assistant', content: p.deltaText, ts: Date.now() });
      }
      scrollBottom();
    }
  }));
  unsubs.push(rpc.on('chat.complete', (p: any) => {
    if (p.sessionKey === currentKey.value) {
      sending.value = false;
      // 缓存命中的回答也刷新会话列表
      loadSessions();
    }
  }));
  unsubs.push(rpc.on('chat.error', (p: any) => {
    if (p.sessionKey === currentKey.value) {
      sending.value = false;
      messages.value.push({ role: 'assistant', content: `错误：${p.error || '未知错误'}`, ts: Date.now() });
    }
  }));
});

onUnmounted(() => {
  unsubs.forEach((u) => u());
  unsubs = [];
});

watch(currentKey, () => scrollBottom());
</script>

<style lang="less" scoped>
.chat-page {
  display: flex;
  height: calc(100vh - var(--navbar-height) - 48px);
  padding: 0;
  gap: 0;
}

/* ========== 会话侧栏（Materialize app-chat 结构） ========== */
.chat-sidebar {
  width: 340px;
  background: var(--color-bg-1);
  border-radius: var(--card-radius);
  box-shadow: var(--card-shadow);
  margin: 16px 0 16px 16px;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  flex-shrink: 0;
}

.sidebar-top {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 16px;
  border-bottom: 1px solid var(--color-border-1);
}

.my-avatar {
  position: relative;
  width: 40px;
  height: 40px;
  border-radius: 50%;
  background: var(--brand-primary);
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 600;
  flex-shrink: 0;
}
.avatar-status, .contact-status {
  position: absolute;
  right: 0;
  bottom: 0;
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: var(--brand-success);
  border: 2px solid var(--color-bg-1);
}

.sidebar-search { flex: 1; }

.sidebar-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px;
  &::-webkit-scrollbar { width: 4px; }
  &::-webkit-scrollbar-thumb { background: var(--color-border-3); border-radius: 2px; }
}

.list-group-title {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 12px 6px;
  font-size: 13px;
  font-weight: 600;
  color: var(--brand-primary);
}
.new-chat-btn {
  border: none;
  background: transparent;
  color: var(--brand-primary);
  cursor: pointer;
  font-size: 16px;
  width: 24px;
  height: 24px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  &:hover { background: rgba(140, 87, 255, 0.1); }
}

.contact-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 8px;
  cursor: pointer;
  transition: background 0.15s;
  &:hover { background: var(--color-bg-3); }
  &.active { background: rgba(140, 87, 255, 0.1); }
}

.contact-avatar {
  position: relative;
  width: 40px;
  height: 40px;
  border-radius: 50%;
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 600;
  flex-shrink: 0;
  font-size: 15px;
}
.contact-status {
  &.web, &.main { background: var(--brand-success); }
  &.cron { background: var(--brand-warning); }
}

.contact-info { flex: 1; min-width: 0; }
.contact-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.contact-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.contact-time {
  font-size: 11px;
  color: var(--color-text-4);
  flex-shrink: 0;
  margin-left: 8px;
}
.contact-preview {
  font-size: 12px;
  color: var(--color-text-3);
  margin-top: 2px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* ========== 主聊天区 ========== */
.chat-main {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
  background: var(--color-bg-1);
  border-radius: var(--card-radius);
  box-shadow: var(--card-shadow);
  margin: 16px 16px 16px 12px;
  overflow: hidden;
}

.chat-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 12px 20px;
  border-bottom: 1px solid var(--color-border-1);
}
.ch-info {
  display: flex;
  align-items: center;
  gap: 12px;
}
.ch-avatar {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 600;
}
.ch-name {
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text-1);
}
.ch-status {
  font-size: 12px;
  color: var(--brand-success);
  margin-top: 2px;
}
.ch-action {
  width: 36px;
  height: 36px;
  border: none;
  background: transparent;
  border-radius: 50%;
  color: var(--color-text-3);
  cursor: pointer;
  font-size: 17px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.2s, color 0.2s;
  &:hover:not(:disabled) { background: var(--color-bg-3); color: var(--brand-primary); }
  &.danger:hover:not(:disabled) { color: var(--brand-danger); }
  &:disabled { opacity: 0.4; cursor: not-allowed; }
}

/* 消息区 */
.chat-history {
  flex: 1;
  overflow-y: auto;
  padding: 24px 20px;
  &::-webkit-scrollbar { width: 5px; }
  &::-webkit-scrollbar-thumb { background: var(--color-border-3); border-radius: 3px; }
}

.chat-empty {
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  color: var(--color-text-4);
  gap: 16px;
}
.empty-icon {
  width: 96px;
  height: 96px;
  border-radius: 50%;
  background: rgba(140, 87, 255, 0.1);
  color: var(--brand-primary);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 40px;
}

.date-divider {
  text-align: center;
  margin: 16px 0;
  span {
    background: var(--color-bg-3);
    color: var(--color-text-3);
    font-size: 11px;
    padding: 3px 12px;
    border-radius: 10px;
  }
}

/* 消息气泡（Materialize：自己紫色右对齐/对方白底左对齐） */
.chat-message {
  display: flex;
  gap: 12px;
  margin-bottom: 20px;
  align-items: flex-start;
  &.msg-right {
    flex-direction: row-reverse;
    .msg-wrapper { align-items: flex-end; }
    .msg-bubble {
      background: var(--brand-primary);
      color: #fff;
      border-start-end-radius: 0;
      border-start-start-radius: 10px;
      :deep(.inline-code) { background: rgba(255,255,255,0.2); color: #fff; }
      :deep(.code-block) { background: rgba(0,0,0,0.2); code { color: #e7e3fc; } }
    }
    .msg-meta { justify-content: flex-end; }
  }
}

.msg-avatar {
  width: 34px;
  height: 34px;
  border-radius: 50%;
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 12px;
  font-weight: 600;
  flex-shrink: 0;
}

.msg-wrapper {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  max-width: 70%;
  min-width: 0;
}

.msg-bubble {
  background: var(--color-bg-3);
  color: var(--color-text-1);
  padding: 10px 14px;
  border-radius: 10px;
  border-start-start-radius: 0;
  font-size: 14px;
  line-height: 1.6;
  word-break: break-word;
  box-shadow: var(--shadow-xs);
  :deep(.inline-code) {
    background: rgba(140, 87, 255, 0.12);
    color: var(--brand-primary);
    padding: 1px 6px;
    border-radius: 4px;
    font-family: monospace;
    font-size: 13px;
  }
  :deep(.code-block) {
    background: #2e263d;
    padding: 12px;
    border-radius: 6px;
    overflow-x: auto;
    margin: 8px 0;
    code { color: #e7e3fc; font-family: monospace; font-size: 13px; }
  }
}

.msg-meta {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 4px;
  font-size: 11px;
  color: var(--color-text-4);
  padding: 0 4px;
  .read-icon { color: var(--brand-success); font-size: 13px; }
}

.typing-bubble {
  display: flex;
  gap: 5px;
  padding: 14px 16px;
}
.typing-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--color-text-4);
  animation: typing-bounce 1.4s infinite;
  &:nth-child(2) { animation-delay: 0.2s; }
  &:nth-child(3) { animation-delay: 0.4s; }
}
@keyframes typing-bounce {
  0%, 60%, 100% { transform: translateY(0); opacity: 0.5; }
  30% { transform: translateY(-5px); opacity: 1; }
}

/* 输入区 */
.chat-footer {
  display: flex;
  align-items: flex-end;
  gap: 10px;
  padding: 14px 20px;
  border-top: 1px solid var(--color-border-1);
}
.msg-input {
  flex: 1;
  :deep(textarea) {
    border-radius: 8px;
  }
}
.send-btn {
  width: 42px;
  height: 42px;
  border: none;
  border-radius: 8px;
  background: var(--brand-primary);
  color: #fff;
  cursor: pointer;
  font-size: 17px;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  transition: background 0.2s;
  &:hover:not(:disabled) { background: var(--brand-primary-hover); }
  &:disabled { opacity: 0.5; cursor: not-allowed; }
}

@media (max-width: 900px) {
  .chat-sidebar { width: 280px; }
  .msg-wrapper { max-width: 82%; }
}
</style>
