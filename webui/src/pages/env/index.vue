<template>
  <div class="page-container">
    <a-page-header title="环境部署" subtitle="选择节点安装运行环境（本地 / 远程 SSH 节点）" :show-back="false">
      <template #extra>
        <a-space>
          <a-button @click="showNodeManager = true"><template #icon><icon-desktop /></template>节点管理</a-button>
          <a-button @click="loadEnvs"><template #icon><icon-refresh /></template>重新检测</a-button>
        </a-space>
      </template>
    </a-page-header>

    <!-- 节点选择器 -->
    <a-card class="mt-16 node-selector">
      <div class="ns-label">当前节点：</div>
      <a-select v-model="currentNodeId" style="width: 280px" @change="loadEnvs">
        <a-option value="local">
          <div class="node-option">
            <icon-computer /> 本机（{{ localHostname }}）
          </div>
        </a-option>
        <a-option v-for="n in nodes" :key="n.id" :value="n.id">
          <div class="node-option">
            <icon-desktop /> {{ n.name }}（{{ n.host }}:{{ n.port }}）
          </div>
        </a-option>
      </a-select>
      <a-tag v-if="currentNodeId === 'local'" color="green">本地</a-tag>
      <a-tag v-else color="arcoblue">远程</a-tag>
    </a-card>

    <!-- 环境卡片 -->
    <a-row :gutter="24" class="mt-16">
      <a-col :xs="24" :sm="12" :lg="8" v-for="env in envs" :key="env.id">
        <a-card class="env-card">
          <div class="env-head">
            <div class="env-icon" :style="{ background: env.installed ? env.color : '#e0dce8' }">{{ env.label.charAt(0) }}</div>
            <div class="env-info">
              <div class="env-name">{{ env.label }}</div>
              <div class="env-status">
                <a-tag :color="env.installed ? 'green' : 'gray'" size="small">{{ env.installed ? env.version : '未安装' }}</a-tag>
              </div>
            </div>
          </div>
          <div class="env-actions">
            <a-button v-if="!env.installed" type="primary" size="small" :loading="env._installing" @click="installEnv(env)">
              <template #icon><icon-download /></template>安装到{{ currentNodeId === 'local' ? '本机' : '远程' }}
            </a-button>
            <template v-else>
              <a-button size="small" @click="installEnv(env)" :loading="env._installing">升级</a-button>
              <a-popconfirm :content="`确认卸载 ${env.label}?`" @ok="uninstallEnv(env)">
                <a-button size="small" status="danger">卸载</a-button>
              </a-popconfirm>
            </template>
          </div>
          <div v-if="env.path" class="env-path">{{ env.path }}</div>
        </a-card>
      </a-col>
    </a-row>

    <!-- 节点管理抽屉 -->
    <a-drawer :visible="showNodeManager" :width="640" @cancel="showNodeManager = false" :footer="false">
      <template #title>SSH 节点管理</template>
      <a-alert type="info" class="mb-16">
        添加远程 SSH 节点后，可在远程主机上检测和安装环境。节点通过 SSH 免密登录（需先配置密钥）。
      </a-alert>
      <a-button type="primary" class="mb-16" @click="openAddNode"><template #icon><icon-plus /></template>添加节点</a-button>
      <a-table :data="nodes" :pagination="{ pageSize: 10 }" row-key="id">
        <template #columns>
          <a-table-column title="节点" :width="180">
            <template #cell="{ record }">
              <a-space>
                <a-avatar :size="28" :style="{ backgroundColor: record.status === 'online' ? '#56ca00' : '#ff4c51' }">
                  <icon-desktop />
                </a-avatar>
                <div>
                  <div>{{ record.name }}</div>
                  <div class="muted">{{ record.host }}:{{ record.port }}</div>
                </div>
              </a-space>
            </template>
          </a-table-column>
          <a-table-column title="用户" :width="100" data-index="user" />
          <a-table-column title="状态" :width="90">
            <template #cell="{ record }">
              <a-badge :status="record.status === 'online' ? 'success' : 'error'" :text="record.status === 'online' ? '在线' : '离线'" />
            </template>
          </a-table-column>
          <a-table-column title="操作" :width="140" fixed="right">
            <template #cell="{ record }">
              <a-space>
                <a-button size="small" @click="testNode(record)">测试</a-button>
                <a-popconfirm content="确认删除该节点？" @ok="deleteNode(record.id)">
                  <a-button size="small" status="danger">删除</a-button>
                </a-popconfirm>
              </a-space>
            </template>
          </a-table-column>
        </template>
      </a-table>
    </a-drawer>

    <!-- 添加节点对话框 -->
    <a-modal :visible="addNodeVisible" title="添加 SSH 节点" @cancel="addNodeVisible = false" @ok="saveNode" :ok-loading="savingNode" :width="480">
      <a-form :model="nodeForm" layout="vertical">
        <a-form-item label="节点名称" required><a-input v-model="nodeForm.name" placeholder="生产服务器-01" /></a-form-item>
        <a-row :gutter="12">
          <a-col :span="16"><a-form-item label="主机地址" required><a-input v-model="nodeForm.host" placeholder="192.168.1.100" /></a-form-item></a-col>
          <a-col :span="8"><a-form-item label="SSH 端口"><a-input-number v-model="nodeForm.port" :min="1" :max="65535" /></a-form-item></a-col>
        </a-row>
        <a-form-item label="SSH 用户" required><a-input v-model="nodeForm.user" placeholder="root" /></a-form-item>
        <a-form-item label="认证方式">
          <a-radio-group v-model="nodeForm.authType" type="button">
            <a-radio value="key">SSH 密钥</a-radio>
            <a-radio value="password">密码</a-radio>
          </a-radio-group>
        </a-form-item>
        <a-form-item v-if="nodeForm.authType === 'key'" label="私钥内容">
          <a-textarea v-model="nodeForm.privateKey" :auto-size="{ minRows: 4 }" placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" />
        </a-form-item>
        <a-form-item v-else label="密码">
          <a-input-password v-model="nodeForm.password" />
        </a-form-item>
      </a-form>
    </a-modal>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, onMounted } from 'vue';
import { Message } from '@arco-design/web-vue';
import { rpc } from '@/api/rpc';
import { IconRefresh, IconDownload, IconDesktop, IconComputer, IconPlus } from '@arco-design/web-vue/es/icon';

interface Env {
  id: string; label: string; color: string;
  installed: boolean; version: string; path: string;
  _installing?: boolean;
}

interface Node {
  id: string; name: string; host: string; port: number;
  user: string; status: string; authType: string;
}

const envs = ref<Env[]>([]);
const nodes = ref<Node[]>([]);
const currentNodeId = ref('local');
const localHostname = ref('');
const showNodeManager = ref(false);
const addNodeVisible = ref(false);
const savingNode = ref(false);

const nodeForm = reactive({
  name: '', host: '', port: 22, user: 'root',
  authType: 'key', privateKey: '', password: '',
});

const envPresets = [
  { id: 'php', label: 'PHP', color: '#777bb4' },
  { id: 'nodejs', label: 'Node.js', color: '#339933' },
  { id: 'python', label: 'Python', color: '#3776ab' },
  { id: 'go', label: 'Go', color: '#00add8' },
  { id: 'java', label: 'Java', color: '#f89820' },
  { id: 'nginx', label: 'Nginx', color: '#009639' },
  { id: 'redis', label: 'Redis', color: '#dc382d' },
  { id: 'mysql', label: 'MySQL', color: '#4479a1' },
  { id: 'docker', label: 'Docker', color: '#2496ed' },
];

async function loadEnvs() {
  try {
    const res = await rpc.call<any>('env.list', { nodeId: currentNodeId.value });
    const envMap = res.environments || {};
    envs.value = envPresets.map((p) => {
      const info = envMap[p.id] || {};
      return { ...p, installed: info.installed || false, version: info.version || '', path: info.path || '' };
    });
    if (res.hostname) localHostname.value = res.hostname;
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function installEnv(env: Env) {
  env._installing = true;
  try {
    const res = await rpc.call<any>('env.install', { id: env.id, nodeId: currentNodeId.value });
    if (res.ok) {
      Message.success(res.message || `${env.label} 安装完成`);
      await loadEnvs();
    } else {
      Message.error(res.error || '安装失败');
    }
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    env._installing = false;
  }
}

async function uninstallEnv(env: Env) {
  try {
    const res = await rpc.call<any>('env.uninstall', { id: env.id, nodeId: currentNodeId.value });
    if (res.ok) {
      Message.success(`${env.label} 已卸载`);
      await loadEnvs();
    } else {
      Message.error(res.error || '卸载失败');
    }
  } catch (e: any) {
    Message.error(e.message);
  }
}

// 节点管理
async function loadNodes() {
  try {
    const res = await rpc.call<any>('nodes.list');
    nodes.value = res.nodes || [];
  } catch { /* ignore */ }
}

function openAddNode() {
  Object.assign(nodeForm, { name: '', host: '', port: 22, user: 'root', authType: 'key', privateKey: '', password: '' });
  addNodeVisible.value = true;
}

async function saveNode() {
  if (!nodeForm.name || !nodeForm.host || !nodeForm.user) {
    Message.warning('请填写节点名称、主机和用户');
    return;
  }
  savingNode.value = true;
  try {
    await rpc.call('nodes.create', { ...nodeForm });
    Message.success('节点已添加');
    addNodeVisible.value = false;
    await loadNodes();
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    savingNode.value = false;
  }
}

async function testNode(node: Node) {
  try {
    const res = await rpc.call<any>('nodes.test', { id: node.id });
    if (res.ok) {
      Message.success(`节点 ${node.name} 连接正常`);
    } else {
      Message.error(`连接失败: ${res.error}`);
    }
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function deleteNode(id: string) {
  try {
    await rpc.call('nodes.delete', { id });
    Message.success('已删除');
    if (currentNodeId.value === id) currentNodeId.value = 'local';
    await loadNodes();
  } catch (e: any) {
    Message.error(e.message);
  }
}

onMounted(() => {
  loadEnvs();
  loadNodes();
});
</script>

<style lang="less" scoped>
.node-selector {
  display: flex;
  align-items: center;
  gap: 12px;
  .ns-label {
    font-size: 14px;
    color: var(--color-text-1);
    font-weight: 500;
  }
}
.node-option {
  display: flex;
  align-items: center;
  gap: 8px;
}
.env-card {
  margin-bottom: 16px;
}
.env-head {
  display: flex;
  align-items: center;
  gap: 14px;
  margin-bottom: 16px;
}
.env-icon {
  width: 44px;
  height: 44px;
  border-radius: 10px;
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 20px;
  font-weight: 700;
  flex-shrink: 0;
  box-shadow: var(--shadow-xs);
}
.env-name {
  font-size: 16px;
  font-weight: 600;
  color: var(--color-text-1);
}
.env-status {
  margin-top: 4px;
}
.env-actions {
  display: flex;
  gap: 8px;
}
.env-path {
  font-size: 11px;
  color: var(--color-text-4);
  margin-top: 8px;
  font-family: monospace;
}
.muted { color: var(--color-text-3); font-size: 12px; }
.mb-16 { margin-bottom: 16px; }
</style>
