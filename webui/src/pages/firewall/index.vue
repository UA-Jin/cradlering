<template>
  <div class="page-container">
    <a-page-header title="防火墙" subtitle="iptables / ufw 规则管理 · 规则导入 · 状态详情" :show-back="false">
      <template #extra>
        <a-space>
          <a-button @click="loadFirewall"><template #icon><icon-refresh /></template>刷新</a-button>
          <a-button type="primary" @click="showAddRule = true"><template #icon><icon-plus /></template>添加规则</a-button>
          <a-button @click="showImport = true"><template #icon><icon-upload /></template>导入规则</a-button>
        </a-space>
      </template>
    </a-page-header>

    <!-- 防火墙状态卡 -->
    <a-row :gutter="24" class="mt-16">
      <a-col :xs="24" :lg="8">
        <a-card class="status-card">
          <div class="status-icon" :class="{ active: firewallStatus.active }">
            <icon-safe />
          </div>
          <div class="status-info">
            <div class="status-title">{{ firewallStatus.active ? '防火墙已启用' : '防火墙未启用' }}</div>
            <div class="status-desc">{{ firewallStatus.backend === 'ufw' ? 'UFW (Uncomplicated Firewall)' : 'iptables' }} · {{ firewallStatus.rules }} 条规则</div>
          </div>
          <a-switch :model-value="firewallStatus.active" @change="toggleFirewall" :loading="toggling" />
        </a-card>
      </a-col>
      <a-col :xs="24" :lg="16">
        <a-card title="最近日志" class="log-card">
          <div class="log-list">
            <div v-for="(log, i) in firewallStatus.recentLogs" :key="i" class="log-item">
              <a-tag :color="log.action === 'ALLOW' ? 'green' : log.action === 'DROP' ? 'red' : 'orange'" size="small">{{ log.action }}</a-tag>
              <span class="log-text">{{ log.text }}</span>
              <span class="log-time">{{ log.time }}</span>
            </div>
            <a-empty v-if="!firewallStatus.recentLogs?.length" description="暂无日志" />
          </div>
        </a-card>
      </a-col>
    </a-row>

    <!-- 规则列表 -->
    <a-card title="防火墙规则" class="mt-24">
      <template #extra>
        <a-input v-model="searchKey" placeholder="搜索规则..." allow-clear style="width: 200px" />
      </template>
      <a-table :data="filteredRules" :pagination="{ pageSize: 15 }" row-key="id" :loading="loading">
        <template #columns>
          <a-table-column title="#" :width="60" data-index="num" />
          <a-table-column title="动作" :width="90">
            <template #cell="{ record }">
              <a-tag :color="record.action === 'ALLOW' ? 'green' : record.action === 'DROP' ? 'red' : 'orange'" size="small">
                {{ record.action }}
              </a-tag>
            </template>
          </a-table-column>
          <a-table-column title="协议" :width="80" data-index="protocol" />
          <a-table-column title="来源" :width="160" data-index="source" />
          <a-table-column title="目标" :width="160" data-index="destination" />
          <a-table-column title="端口" :width="100" data-index="port" />
          <a-table-column title="接口" :width="80" data-index="interface" />
          <a-table-column title="备注" data-index="comment" ellipsis tooltip />
          <a-table-column title="操作" :width="100" fixed="right">
            <template #cell="{ record }">
              <a-popconfirm content="确认删除该规则？" @ok="deleteRule(record.num)">
                <a-button size="small" status="danger" type="text"><icon-delete /></a-button>
              </a-popconfirm>
            </template>
          </a-table-column>
        </template>
      </a-table>
    </a-card>

    <!-- 添加规则对话框 -->
    <a-modal :visible="showAddRule" title="添加防火墙规则" @cancel="showAddRule = false" @ok="addRule" :ok-loading="addingRule" :width="560">
      <a-form :model="ruleForm" layout="vertical">
        <a-row :gutter="12">
          <a-col :span="12">
            <a-form-item label="动作" required>
              <a-select v-model="ruleForm.action">
                <a-option value="ALLOW">ALLOW（允许）</a-option>
                <a-option value="DROP">DROP（丢弃）</a-option>
                <a-option value="REJECT">REJECT（拒绝）</a-option>
              </a-select>
            </a-form-item>
          </a-col>
          <a-col :span="12">
            <a-form-item label="协议">
              <a-select v-model="ruleForm.protocol">
                <a-option value="tcp">TCP</a-option>
                <a-option value="udp">UDP</a-option>
                <a-option value="all">全部</a-option>
              </a-select>
            </a-form-item>
          </a-col>
        </a-row>
        <a-row :gutter="12">
          <a-col :span="12">
            <a-form-item label="来源 IP" extra="留空表示全部，如 192.168.1.0/24">
              <a-input v-model="ruleForm.source" placeholder="0.0.0.0/0" />
            </a-form-item>
          </a-col>
          <a-col :span="12">
            <a-form-item label="目标端口" extra="如 80, 443, 8080-8090">
              <a-input v-model="ruleForm.port" placeholder="80" />
            </a-form-item>
          </a-col>
        </a-row>
        <a-form-item label="备注">
          <a-input v-model="ruleForm.comment" placeholder="规则说明（可选）" />
        </a-form-item>
      </a-form>
    </a-modal>

    <!-- 导入规则对话框 -->
    <a-modal :visible="showImport" title="导入防火墙规则" @cancel="showImport = false" @ok="importRules" :ok-loading="importing" :width="640">
      <a-alert type="info" class="mb-16">
        支持 iptables-save 格式（-A INPUT ...）或每行一条规则。导入前自动备份当前规则。
      </a-alert>
      <a-form layout="vertical">
        <a-form-item label="规则内容">
          <a-textarea v-model="importText" :auto-size="{ minRows: 12 }" placeholder="-A INPUT -p tcp --dport 22 -j ACCEPT&#10;-A INPUT -p tcp --dport 80 -j ACCEPT&#10;-A INPUT -p tcp --dport 443 -j ACCEPT" style="font-family:monospace;font-size:12px" />
        </a-form-item>
        <a-form-item>
          <a-checkbox v-model="importReplace">替换模式（清空现有规则后导入，谨慎使用）</a-checkbox>
        </a-form-item>
        <a-form-item label="从文件导入">
          <a-upload :auto-upload="false" :show-file-list="false" accept=".txt,.rules,.iptables" @change="onImportFile">
            <template #upload-button>
              <a-button size="small"><template #icon><icon-folder-add /></template>选择文件</a-button>
            </template>
          </a-upload>
        </a-form-item>
      </a-form>
    </a-modal>
  </div>
</template>

<script setup lang="ts">
import { ref, reactive, computed, onMounted } from 'vue';
import { Message } from '@arco-design/web-vue';
import { rpc } from '@/api/rpc';
import { IconRefresh, IconPlus, IconUpload, IconDelete, IconSafe, IconFolderAdd } from '@arco-design/web-vue/es/icon';

const loading = ref(false);
const toggling = ref(false);
const addingRule = ref(false);
const importing = ref(false);
const showAddRule = ref(false);
const showImport = ref(false);
const searchKey = ref('');

const firewallStatus = reactive({
  active: false, backend: 'iptables', rules: 0, recentLogs: [] as any[],
});

const rules = ref<any[]>([]);
const ruleForm = reactive({ action: 'ALLOW', protocol: 'tcp', source: '', port: '', comment: '' });
const importText = ref('');
const importReplace = ref(false);

const filteredRules = computed(() =>
  rules.value.filter((r) => !searchKey.value || JSON.stringify(r).toLowerCase().includes(searchKey.value.toLowerCase())),
);

async function loadFirewall() {
  loading.value = true;
  try {
    const res = await rpc.call<any>('firewall.status');
    Object.assign(firewallStatus, res);
    rules.value = res.rules || [];
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    loading.value = false;
  }
}

async function toggleFirewall(active: boolean) {
  toggling.value = true;
  try {
    await rpc.call('firewall.toggle', { enabled: active });
    Message.success(active ? '防火墙已启用' : '防火墙已禁用');
    await loadFirewall();
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    toggling.value = false;
  }
}

async function addRule() {
  addingRule.value = true;
  try {
    await rpc.call('firewall.add', { ...ruleForm });
    Message.success('规则已添加');
    showAddRule.value = false;
    ruleForm.action = 'ALLOW'; ruleForm.protocol = 'tcp'; ruleForm.source = ''; ruleForm.port = ''; ruleForm.comment = '';
    await loadFirewall();
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    addingRule.value = false;
  }
}

async function deleteRule(num: number) {
  try {
    await rpc.call('firewall.delete', { ruleNum: num });
    Message.success('规则已删除');
    await loadFirewall();
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function importRules() {
  if (!importText.value.trim()) { Message.warning('请输入规则内容'); return; }
  importing.value = true;
  try {
    const res = await rpc.call<any>('firewall.import', { rules: importText.value, replace: importReplace.value });
    if (res.ok) {
      Message.success(`已导入 ${res.imported} 条规则`);
      showImport.value = false;
      importText.value = '';
      await loadFirewall();
    } else {
      Message.error(res.error || '导入失败');
    }
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    importing.value = false;
  }
}

function onImportFile(_fileList: any, fileItem: any) {
  const file = fileItem?.file;
  if (!file) return;
  const reader = new FileReader();
  reader.onload = (e) => {
    importText.value = String(e.target?.result || '');
    Message.success(`已读取 ${file.name}`);
  };
  reader.readAsText(file);
}

onMounted(loadFirewall);
</script>

<style lang="less" scoped>
.status-card {
  display: flex;
  align-items: center;
  gap: 16px;
  .status-icon {
    width: 48px;
    height: 48px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 24px;
    background: #e0dce8;
    color: #6d6777;
    flex-shrink: 0;
    &.active {
      background: linear-gradient(135deg, #56ca00, #82e040);
      color: #fff;
    }
  }
  .status-title {
    font-size: 16px;
    font-weight: 600;
    color: var(--color-text-1);
  }
  .status-desc {
    font-size: 13px;
    color: var(--color-text-3);
    margin-top: 4px;
  }
}
.log-card {
  max-height: 200px;
  overflow-y: auto;
}
.log-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.log-item {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  .log-text {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-text-2);
  }
  .log-time {
    color: var(--color-text-4);
    flex-shrink: 0;
  }
}
.mb-16 { margin-bottom: 16px; }
</style>
