<template>
  <div class="page-container">
    <a-page-header title="进程管理" subtitle="实时进程监控 · CPU/内存排序 · 节点选择" :show-back="false">
      <template #extra>
        <a-space>
          <a-select v-model="currentNodeId" style="width: 200px" @change="loadProcesses" size="small">
            <a-option value="local">本机</a-option>
            <a-option v-for="n in nodes" :key="n.id" :value="n.id">{{ n.name }}</a-option>
          </a-select>
          <a-button @click="loadProcesses"><template #icon><icon-refresh /></template></a-button>
          <a-radio-group v-model="sortBy" type="button" size="small" @change="loadProcesses">
            <a-radio value="cpu">CPU</a-radio>
            <a-radio value="mem">内存</a-radio>
            <a-radio value="pid">PID</a-radio>
          </a-radio-group>
        </a-space>
      </template>
    </a-page-header>

    <a-card class="mt-16">
      <a-table :data="processes" :loading="loading" :pagination="{ pageSize: 20, showTotal: true }" row-key="pid">
        <template #columns>
          <a-table-column title="PID" :width="80" data-index="pid" />
          <a-table-column title="用户" :width="90" data-index="user" />
          <a-table-column title="进程" :width="200">
            <template #cell="{ record }">
              <div class="proc-name">{{ record.name }}</div>
              <div class="proc-cmd" :title="record.cmd">{{ record.cmd }}</div>
            </template>
          </a-table-column>
          <a-table-column title="CPU%" :width="90" sortable>
            <template #cell="{ record }">
              <span :class="{ 'high-cpu': record.cpu > 50 }">{{ record.cpu }}%</span>
            </template>
          </a-table-column>
          <a-table-column title="内存%" :width="90" sortable>
            <template #cell="{ record }">
              <span :class="{ 'high-mem': record.mem > 30 }">{{ record.mem }}%</span>
            </template>
          </a-table-column>
          <a-table-column title="RSS" :width="90">
            <template #cell="{ record }">{{ formatSize(record.rss) }}</template>
          </a-table-column>
          <a-table-column title="状态" :width="80" data-index="state" />
          <a-table-column title="操作" :width="120" fixed="right">
            <template #cell="{ record }">
              <a-space>
                <a-tooltip content="终止进程">
                  <a-popconfirm :content="`确认终止 PID ${record.pid} (${record.name})?`" @ok="killProcess(record.pid)">
                    <a-button size="small" status="danger" type="text"><icon-delete /></a-button>
                  </a-popconfirm>
                </a-tooltip>
              </a-space>
            </template>
          </a-table-column>
        </template>
      </a-table>
    </a-card>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { Message } from '@arco-design/web-vue';
import { rpc } from '@/api/rpc';
import { IconRefresh, IconDelete } from '@arco-design/web-vue/es/icon';

const currentNodeId = ref('local');
const nodes = ref<any[]>([]);
const processes = ref<any[]>([]);
const loading = ref(false);
const sortBy = ref('cpu');
let timer: any = null;

async function loadProcesses() {
  loading.value = true;
  try {
    const res = await rpc.call<any>('process.list', { sortBy: sortBy.value, limit: 50, nodeId: currentNodeId.value });
    processes.value = res.processes || [];
  } catch (e: any) {
    Message.error(e.message);
  } finally {
    loading.value = false;
  }
}

async function loadNodes() {
  try {
    const res = await rpc.call<any>('nodes.list');
    nodes.value = (res.nodes || []).filter((n: any) => n.status === 'online');
  } catch { /* ignore */ }
}

async function killProcess(pid: number) {
  try {
    await rpc.call('process.kill', { pid, nodeId: currentNodeId.value });
    Message.success(`已终止 PID ${pid}`);
    await loadProcesses();
  } catch (e: any) {
    Message.error(e.message);
  }
}

function formatSize(kb: number): string {
  if (kb >= 1048576) return (kb / 1048576).toFixed(1) + 'G';
  if (kb >= 1024) return (kb / 1024).toFixed(1) + 'M';
  return kb + 'K';
}

onMounted(() => {
  loadNodes();
  loadProcesses();
  timer = setInterval(loadProcesses, 10000); // 10s 刷新
});
onUnmounted(() => { if (timer) clearInterval(timer); });
</script>

<style lang="less" scoped>
.proc-name { font-weight: 500; color: var(--color-text-1); font-size: 13px; }
.proc-cmd { font-size: 11px; color: var(--color-text-4); max-width: 400px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.high-cpu { color: var(--brand-danger); font-weight: 600; }
.high-mem { color: var(--brand-warning); font-weight: 600; }
</style>
