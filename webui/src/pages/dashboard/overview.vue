<template>
  <div class="page-container">
    <a-row :gutter="24">
      <!-- 欢迎卡（Materialize Congratulations card） -->
      <a-col :xs="24" :lg="8">
        <a-card class="welcome-card">
          <div class="welcome-body">
            <h5 class="welcome-title">{{ greeting }}，{{ username }}！🎉</h5>
            <p class="welcome-sub">欢迎使用 CradleRing 控制台</p>
            <h4 class="welcome-stat">{{ totalCalls }}</h4>
            <p class="welcome-sub2">累计会话 {{ totalCalls > 0 ? '已就绪' : '待开始' }} 🚀</p>
            <a-button type="primary" size="small" @click="$router.push('/chat')">开始对话</a-button>
          </div>
          <img src="/illustrations/trophy.png" class="welcome-img" alt="" />
        </a-card>
      </a-col>

      <!-- 统计卡（Materialize Transactions card） -->
      <a-col :xs="24" :lg="16">
        <a-card class="h-100">
          <template #title>
            <span class="card-title">系统概览</span>
          </template>
          <p class="card-sub"><span class="card-sub-hl">{{ summaryText }}</span> 当前状态</p>
          <div class="stat-grid">
            <div class="stat-item" v-for="s in statItems" :key="s.label">
              <div class="stat-icon" :class="s.iconClass">
                <component :is="s.icon" />
              </div>
              <div class="stat-info">
                <div class="stat-label">{{ s.label }}</div>
                <div class="stat-value">{{ s.value }}</div>
              </div>
            </div>
          </div>
        </a-card>
      </a-col>
    </a-row>

    <!-- 图表行 -->
    <a-row :gutter="24" class="mt-24">
      <a-col :xs="24" :lg="10">
        <a-card class="h-100">
          <template #title><span class="card-title">每周调用概览</span></template>
          <v-chart class="chart-bar" :option="barOption" autoresize />
          <div class="chart-footer">
            <span class="chart-hl">45%</span>
            <span class="chart-note">本周调用量较上周提升 45% 😎</span>
          </div>
          <a-button type="primary" long class="mt-16" @click="$router.push('/logs')">查看详情</a-button>
        </a-card>
      </a-col>
      <a-col :xs="24" :lg="9">
        <a-card class="h-100">
          <template #title><span class="card-title">记忆系统</span></template>
          <div class="memory-head">
            <h3 class="memory-num">{{ memoryTotal }}</h3>
            <span class="badge-up" v-if="cacheHitRate > 0">↑{{ (cacheHitRate * 100).toFixed(0) }}%</span>
          </div>
          <p class="memory-sub">缓存命中率 {{ (cacheHitRate * 100).toFixed(1) }}%</p>
          <a-progress
            :percent="cacheHitRate"
            :show-text="false"
            :stroke-width="6"
            color="#8c57ff"
            track-color="#f2f0f5"
          />
          <a-button type="outline" long class="mt-24" @click="$router.push('/memory')">打开记忆库</a-button>
        </a-card>
      </a-col>
      <a-col :xs="24" :lg="5">
        <a-card class="h-100">
          <template #title><span class="card-title">用量分布</span></template>
          <v-chart class="chart-pie" :option="distOption" autoresize />
        </a-card>
      </a-col>
    </a-row>

    <!-- 待审批 + 最近会话 -->
    <a-row :gutter="24" class="mt-24">
      <a-col :xs="24" :lg="12">
        <a-card>
          <template #title><span class="card-title">待处理审批</span></template>
          <template #extra><a-link @click="$router.push('/approvals/instances')">查看全部</a-link></template>
          <a-empty v-if="!pendingApprovals.length" description="暂无待审批，一切安全" />
          <a-list v-else :bordered="false">
            <a-list-item v-for="i in pendingApprovals" :key="i.id">
              <a-list-item-meta :title="i.title" :description="`${i.requestedUsername} · ${(i.command || '').slice(0, 50)}`">
                <template #avatar>
                  <div class="stat-icon stat-icon-yellow"><icon-clock-circle /></div>
                </template>
              </a-list-item-meta>
              <template #actions>
                <a-button type="primary" size="small" @click="quickApprove(i.id)">同意</a-button>
                <a-button status="danger" size="small" @click="quickReject(i.id)">拒绝</a-button>
              </template>
            </a-list-item>
          </a-list>
        </a-card>
      </a-col>
      <a-col :xs="24" :lg="12">
        <a-card>
          <template #title><span class="card-title">最近会话</span></template>
          <template #extra><a-link @click="$router.push('/sessions')">查看全部</a-link></template>
          <a-empty v-if="!recentSessions.length" description="暂无会话，去开始第一个对话吧" />
          <a-list v-else :bordered="false">
            <a-list-item v-for="s in recentSessions" :key="s.key" @click="$router.push('/chat')" class="session-item">
              <a-list-item-meta :title="s.displayName || s.key" :description="dayjs(s.updatedAt).fromNow()">
                <template #avatar>
                  <div class="stat-icon stat-icon-purple">{{ (s.kind || 'S').charAt(0).toUpperCase() }}</div>
                </template>
              </a-list-item-meta>
            </a-list-item>
          </a-list>
        </a-card>
      </a-col>
    </a-row>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, markRaw } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { BarChart, PieChart } from 'echarts/charts';
import { GridComponent, TooltipComponent, LegendComponent } from 'echarts/components';
import VChart from 'vue-echarts';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import 'dayjs/locale/zh-cn';
import { rpc } from '@/api/rpc';
import { Message } from '@arco-design/web-vue';
import { useUserStore } from '@/stores/user';
import {
  IconStorage, IconCheckCircle, IconMindMapping, IconTrophy,
  IconClockCircle, IconBookmark, IconUserGroup, IconRobot,
} from '@arco-design/web-vue/es/icon';

dayjs.extend(relativeTime);
dayjs.locale('zh-cn');

use([CanvasRenderer, BarChart, PieChart, GridComponent, TooltipComponent, LegendComponent]);

const userStore = useUserStore();

const pendingApprovals = ref<any[]>([]);
const recentSessions = ref<any[]>([]);
const memoryTotal = ref(0);
const cacheHitRate = ref(0);
const sessionCount = ref(0);
const flowCount = ref(0);
const agentCount = ref(0);

const username = computed(() => userStore.user?.displayName || '用户');
const greeting = computed(() => {
  const h = new Date().getHours();
  if (h < 6) return '夜深了';
  if (h < 12) return '早上好';
  if (h < 14) return '中午好';
  if (h < 18) return '下午好';
  return '晚上好';
});
const totalCalls = computed(() => sessionCount.value || 0);
const summaryText = computed(() => `${sessionCount.value} 个会话 · ${memoryTotal.value} 条记忆`);

const statItems = computed(() => [
  { label: '总会话数', value: sessionCount.value, icon: markRaw(IconStorage), iconClass: 'stat-icon-purple' },
  { label: '待审批', value: pendingApprovals.value.length, icon: markRaw(IconCheckCircle), iconClass: 'stat-icon-yellow' },
  { label: '审批流模板', value: flowCount.value, icon: markRaw(IconMindMapping), iconClass: 'stat-icon-green' },
  { label: '记忆条目', value: memoryTotal.value, icon: markRaw(IconBookmark), iconClass: 'stat-icon-blue' },
]);

// 柱状图（Materialize 风格：一根高亮紫，其余浅灰）
const barOption = computed(() => ({
  tooltip: { trigger: 'axis' },
  grid: { left: 40, right: 20, top: 20, bottom: 30 },
  xAxis: {
    type: 'category',
    data: ['周一', '周二', '周三', '周四', '周五', '周六', '周日'],
    axisLine: { show: false },
    axisTick: { show: false },
    axisLabel: { color: '#6d6777' },
  },
  yAxis: {
    type: 'value',
    splitLine: { lineStyle: { color: '#f2f0f5', type: 'dashed' } },
    axisLabel: { color: '#6d6777' },
  },
  series: [{
    type: 'bar',
    barWidth: 14,
    data: [
      { value: 32, itemStyle: { color: '#e8e0fe' } },
      { value: 48, itemStyle: { color: '#e8e0fe' } },
      { value: 41, itemStyle: { color: '#e8e0fe' } },
      { value: 88, itemStyle: { color: '#8c57ff' } },
      { value: 52, itemStyle: { color: '#e8e0fe' } },
      { value: 30, itemStyle: { color: '#e8e0fe' } },
      { value: 65, itemStyle: { color: '#e8e0fe' } },
    ],
    itemStyle: { borderRadius: [4, 4, 0, 0] },
  }],
}));

const distOption = computed(() => ({
  tooltip: { trigger: 'item' },
  legend: {
    orient: 'vertical',
    right: 4,
    top: 'center',
    itemWidth: 10,
    itemHeight: 10,
    textStyle: { color: '#6d6777', fontSize: 12 },
  },
  series: [{
    type: 'pie',
    center: ['38%', '50%'],
    radius: ['45%', '72%'],
    itemStyle: { borderRadius: 4, borderWidth: 2, borderColor: '#fff' },
    label: { show: false },
    data: [
      { value: 40, name: '对话', itemStyle: { color: '#8c57ff' } },
      { value: 25, name: '工具调用', itemStyle: { color: '#56ca00' } },
      { value: 20, name: '搜索', itemStyle: { color: '#ffb400' } },
      { value: 15, name: '其他', itemStyle: { color: '#d6c6fd' } },
    ],
  }],
}));

async function loadOverview() {
  try {
    const [stats, sessions, approvals] = await Promise.all([
      rpc.call<any>('approval.stats'),
      rpc.call<any>('sessions.list'),
      rpc.call<any>('approval.instances.list', { status: 'pending' }),
    ]);
    sessionCount.value = sessions.count || 0;
    flowCount.value = stats.flowsCount || 0;
    pendingApprovals.value = (approvals.instances || []).slice(0, 5);
    recentSessions.value = (sessions.sessions || []).slice(0, 6);
  } catch (e) {
    // 降级
  }
  // 记忆统计（可选，失败不阻塞）
  try {
    const mem = await rpc.call<any>('memory.stats');
    memoryTotal.value = mem.stats?.total || 0;
    cacheHitRate.value = Math.min(1, Math.max(0, mem.stats?.cache?.hitRate || 0));
  } catch (e) {
    // 降级
  }
}

async function quickApprove(id: string) {
  try {
    await rpc.call('approval.instances.approve', { id, comment: '概览页快速通过' });
    Message.success('已同意');
    loadOverview();
  } catch (e: any) {
    Message.error(e.message);
  }
}

async function quickReject(id: string) {
  try {
    await rpc.call('approval.instances.reject', { id, comment: '概览页拒绝' });
    Message.success('已拒绝');
    loadOverview();
  } catch (e: any) {
    Message.error(e.message);
  }
}

onMounted(loadOverview);
</script>

<style lang="less" scoped>
.h-100 { height: 100%; }

.card-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--color-text-1);
}
.card-sub {
  margin: 0 0 20px;
  font-size: 13px;
  color: var(--color-text-3);
  .card-sub-hl {
    font-size: 15px;
    font-weight: 600;
    color: var(--color-text-1);
    margin-right: 4px;
  }
}

/* 欢迎卡 */
.welcome-card {
  position: relative;
  overflow: hidden;
  :deep(.arco-card-body) {
    padding: 24px;
  }
}
.welcome-body {
  position: relative;
  z-index: 2;
  max-width: 60%;
}
.welcome-title {
  font-size: 17px;
  font-weight: 600;
  color: var(--color-text-1);
  margin: 0 0 4px;
  white-space: nowrap;
}
.welcome-sub {
  font-size: 13px;
  color: var(--color-text-3);
  margin: 0 0 12px;
}
.welcome-stat {
  font-size: 28px;
  font-weight: 700;
  color: var(--primary-6);
  margin: 0 0 4px;
}
.welcome-sub2 {
  font-size: 13px;
  color: var(--color-text-3);
  margin: 0 0 16px;
}
.welcome-img {
  position: absolute;
  right: 20px;
  bottom: 16px;
  width: 90px;
  z-index: 1;
}

/* 统计网格（Materialize Transactions） */
.stat-grid {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 24px;
  @media (max-width: 992px) {
    grid-template-columns: repeat(2, 1fr);
  }
}
.stat-item {
  display: flex;
  align-items: center;
  gap: 14px;
}
.stat-info {
  min-width: 0;
}
.stat-label {
  font-size: 13px;
  color: var(--color-text-3);
  line-height: 1.3;
}
.stat-value {
  font-size: 20px;
  font-weight: 700;
  color: var(--color-text-1);
  line-height: 1.3;
}

/* 柱状图 */
.chart-bar {
  height: 240px;
}
.chart-footer {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-top: 12px;
  .chart-hl {
    font-size: 24px;
    font-weight: 700;
    color: var(--color-text-1);
  }
  .chart-note {
    font-size: 13px;
    color: var(--color-text-3);
  }
}

/* 记忆卡 */
.memory-head {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 4px;
}
.memory-num {
  font-size: 30px;
  font-weight: 700;
  color: var(--color-text-1);
  margin: 0;
}
.memory-sub {
  font-size: 13px;
  color: var(--color-text-3);
  margin: 0 0 12px;
}

/* 饼图 */
.chart-pie {
  height: 240px;
}

/* 会话列表 hover */
.session-item {
  cursor: pointer;
  border-radius: 6px;
  padding: 8px;
  margin: 0 -8px;
  transition: background 0.2s;
  &:hover {
    background-color: var(--color-bg-3);
  }
}

/* 徽章 */
.badge-up {
  color: var(--success-6);
  font-size: 13px;
  font-weight: 600;
}
</style>
