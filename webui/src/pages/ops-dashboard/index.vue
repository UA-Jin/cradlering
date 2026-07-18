<template>
  <div class="ops-dashboard" :class="{ 'dark-mode': appStore.isDark }">
    <!-- 顶部标题栏 -->
    <div class="dashboard-header">
      <div class="header-left">
        <icon-dashboard class="header-icon" />
        <h1>运维监控中心</h1>
        <a-tag :color="summary.online > 0 ? 'green' : 'gray'" size="large">
          {{ summary.online }}/{{ summary.total }} 在线
        </a-tag>
      </div>
      <div class="header-right">
        <a-space>
          <a-button @click="refresh"><template #icon><icon-refresh /></template>刷新</a-button>
          <a-button @click="toggleFullscreen">
            <template #icon><icon-fullscreen v-if="!isFullscreen" /><icon-fullscreen-exit v-else /></template>
            {{ isFullscreen ? '退出全屏' : '全屏' }}
          </a-button>
        </a-space>
      </div>
    </div>

    <!-- 统计卡片行 -->
    <div class="stats-row">
      <div class="stat-card" :style="{ background: statCards[0].bg }">
        <div class="stat-icon"><icon-check-circle /></div>
        <div class="stat-info">
          <div class="stat-value" ref="onlineValue">{{ animatedValues.online }}</div>
          <div class="stat-label">在线设备</div>
        </div>
      </div>
      <div class="stat-card" :style="{ background: statCards[1].bg }">
        <div class="stat-icon"><icon-close-circle /></div>
        <div class="stat-info">
          <div class="stat-value" ref="offlineValue">{{ animatedValues.offline }}</div>
          <div class="stat-label">掉线设备</div>
        </div>
      </div>
      <div class="stat-card" :style="{ background: statCards[2].bg }">
        <div class="stat-icon"><icon-clock-circle /></div>
        <div class="stat-info">
          <div class="stat-value" ref="latencyValue">{{ animatedValues.highLatency }}</div>
          <div class="stat-label">高延迟</div>
        </div>
      </div>
      <div class="stat-card" :style="{ background: statCards[3].bg }">
        <div class="stat-icon"><icon-exclamation-circle /></div>
        <div class="stat-info">
          <div class="stat-value" ref="riskValue">{{ animatedValues.atRisk }}</div>
          <div class="stat-label">有风险</div>
        </div>
      </div>
      <div class="stat-card" :style="{ background: statCards[4].bg }">
        <div class="stat-icon"><icon-share-internal /></div>
        <div class="stat-info">
          <div class="stat-value" ref="channelValue">{{ animatedValues.channelsError }}</div>
          <div class="stat-label">渠道异常</div>
        </div>
      </div>
    </div>

    <!-- 主机监控行（1Panel 风格：CPU/内存/磁盘/负载/网络 IO 仪表） -->
    <div class="host-stats-row" v-if="hostStats">
      <div class="host-card">
        <div class="host-card-title">CPU 使用率</div>
        <a-progress type="circle" :percent="hostStats.cpu.usage / 100" :stroke-width="8" :color="usageColor(hostStats.cpu.usage)">
          <template #text>{{ hostStats.cpu.usage }}%</template>
        </a-progress>
        <div class="host-card-sub">{{ hostStats.cpu.cores }} 核 · 负载 {{ hostStats.load.load1 }}</div>
      </div>
      <div class="host-card">
        <div class="host-card-title">内存</div>
        <a-progress type="circle" :percent="hostStats.memory.usagePct / 100" :stroke-width="8" :color="usageColor(hostStats.memory.usagePct)">
          <template #text>{{ hostStats.memory.usagePct }}%</template>
        </a-progress>
        <div class="host-card-sub">{{ formatMb(hostStats.memory.usedMb) }} / {{ formatMb(hostStats.memory.totalMb) }}</div>
      </div>
      <div class="host-card">
        <div class="host-card-title">磁盘（{{ hostStats.disks[0]?.mount || '/' }}）</div>
        <a-progress type="circle" :percent="(hostStats.disks[0]?.usagePct || 0) / 100" :stroke-width="8" :color="usageColor(hostStats.disks[0]?.usagePct || 0)">
          <template #text>{{ hostStats.disks[0]?.usagePct || 0 }}%</template>
        </a-progress>
        <div class="host-card-sub">{{ hostStats.disks[0]?.usedGb || 0 }}G / {{ hostStats.disks[0]?.totalGb || 0 }}G</div>
      </div>
      <div class="host-card">
        <div class="host-card-title">系统负载</div>
        <div class="load-values">
          <div class="load-item"><span class="load-num">{{ hostStats.load.load1 }}</span><span class="load-label">1分钟</span></div>
          <div class="load-item"><span class="load-num">{{ hostStats.load.load5 }}</span><span class="load-label">5分钟</span></div>
          <div class="load-item"><span class="load-num">{{ hostStats.load.load15 }}</span><span class="load-label">15分钟</span></div>
        </div>
        <div class="host-card-sub">{{ hostStats.hostname }} · {{ hostStats.os }}</div>
      </div>
      <div class="host-card">
        <div class="host-card-title">网络 IO</div>
        <div class="net-io">
          <div class="net-item rx"><icon-arrow-down /> {{ hostStats.network.rxKbps }} KB/s</div>
          <div class="net-item tx"><icon-arrow-up /> {{ hostStats.network.txKbps }} KB/s</div>
        </div>
        <div class="host-card-sub">内核 {{ hostStats.kernel }}</div>
      </div>
    </div>

    <!-- 图表区域 -->
    <div class="charts-row">
      <a-card class="chart-card" title="设备分布" :bordered="false">
        <v-chart class="chart" :option="mapOption" autoresize />
      </a-card>
      <a-card class="chart-card" title="延迟趋势（近24小时）" :bordered="false">
        <v-chart class="chart" :option="latencyTrendOption" autoresize />
      </a-card>
      <a-card class="chart-card" title="风险排行" :bordered="false">
        <v-chart class="chart" :option="riskRankOption" autoresize />
      </a-card>
    </div>

    <!-- 设备状态表格 -->
    <a-card class="table-card" :bordered="false">
      <template #title>
        <div class="table-title">
          <icon-list /> 设备状态详情
          <a-input v-model="searchText" placeholder="搜索设备..." allow-clear style="width: 240px; margin-left: 16px" />
        </div>
      </template>
      <a-table
        :data="filteredNodes"
        :pagination="{ pageSize: 15, showTotal: true }"
        row-key="id"
        :loading="loading"
      >
        <template #columns>
          <a-table-column title="状态" :width="80">
            <template #cell="{ record }">
              <span class="status-dot" :class="record.status"></span>
              <span class="status-text">{{ statusLabel(record.status) }}</span>
            </template>
          </a-table-column>
          <a-table-column title="设备" :width="160">
            <template #cell="{ record }">
              <div class="device-name">{{ record.name }}</div>
              <div class="device-id">{{ record.id }}</div>
            </template>
          </a-table-column>
          <a-table-column title="类型" data-index="kind" :width="100">
            <template #cell="{ record }">
              <a-tag :color="record.kind === 'device' ? 'arcoblue' : 'green'">{{ record.kind }}</a-tag>
            </template>
          </a-table-column>
          <a-table-column title="延迟" :width="100">
            <template #cell="{ record }">
              <span :class="{ 'latency-high': record.latencyMs > 500, 'latency-ok': record.latencyMs <= 500 }">
                {{ record.latencyMs }}ms
              </span>
            </template>
          </a-table-column>
          <a-table-column title="风险" :width="100">
            <template #cell="{ record }">
              <a-tag :color="riskColor(record.riskScore)">{{ riskLabel(record.riskScore) }}</a-tag>
            </template>
          </a-table-column>
          <a-table-column title="CPU" :width="80">
            <template #cell="{ record }">
              <span v-if="record.cpuPercent !== null">{{ record.cpuPercent }}%</span>
              <span v-else class="muted">-</span>
            </template>
          </a-table-column>
          <a-table-column title="内存" :width="80">
            <template #cell="{ record }">
              <span v-if="record.memPercent !== null">{{ record.memPercent }}%</span>
              <span v-else class="muted">-</span>
            </template>
          </a-table-column>
          <a-table-column title="最后在线" :width="140">
            <template #cell="{ record }">
              {{ formatTime(record.lastSeen) }}
            </template>
          </a-table-column>
          <a-table-column title="风险原因" :width="200" ellipsis tooltip>
            <template #cell="{ record }">
              <span v-if="record.riskReasons?.length">{{ record.riskReasons.join('、') }}</span>
              <span v-else class="muted">无</span>
            </template>
          </a-table-column>
        </template>
      </a-table>
    </a-card>

    <!-- 最近安全事件 -->
    <a-card class="alerts-card" :bordered="false">
      <template #title>
        <icon-safe /> 最近安全事件
      </template>
      <a-list v-if="recentAlerts.length" :data="recentAlerts" size="small">
        <template #item="{ item }">
          <a-list-item>
            <a-list-item-meta
              :title="item.ruleName"
              :description="`${item.type === 'waf' ? 'WAF' : '告警'} · ${formatTime(item.ts)}`"
            >
              <template #avatar>
                <a-avatar :size="28" :style="{ backgroundColor: severityColor(item.severity) }">
                  <icon-exclamation-circle />
                </a-avatar>
              </template>
            </a-list-item-meta>
            <template #actions>
              <a-tag :color="severityColor(item.severity)" size="small">{{ item.severity }}</a-tag>
            </template>
          </a-list-item>
        </template>
      </a-list>
      <a-empty v-else description="暂无安全事件" />
    </a-card>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, reactive } from 'vue';
import { use } from 'echarts/core';
import { CanvasRenderer } from 'echarts/renderers';
import { LineChart, MapChart, BarChart } from 'echarts/charts';
import { GridComponent, TooltipComponent, LegendComponent, GeoComponent } from 'echarts/components';
import VChart from 'vue-echarts';
import dayjs from 'dayjs';
import { rpc } from '@/api/rpc';
import { useAppStore } from '@/stores/app';

use([CanvasRenderer, LineChart, MapChart, BarChart, GridComponent, TooltipComponent, LegendComponent, GeoComponent]);

const appStore = useAppStore();

interface NodeInfo {
  id: string; name: string; kind: string;
  status: string; latencyMs: number; riskScore: number;
  riskReasons: string[]; lastSeen: number;
  cpuPercent?: number; memPercent?: number;
}

const loading = ref(false);
const nodes = ref<NodeInfo[]>([]);
const recentAlerts = ref<any[]>([]);
const searchText = ref('');
const isFullscreen = ref(false);

const summary = reactive({
  total: 0, online: 0, offline: 0, highLatency: 0, atRisk: 0,
  avgLatencyMs: 0, channelsConnected: 0, channelsError: 0, channelsTotal: 0,
});

// 数字滚动动画
const animatedValues = reactive({ online: 0, offline: 0, highLatency: 0, atRisk: 0, channelsError: 0 });
function animateValue(key: keyof typeof animatedValues, target: number, duration = 800) {
  const start = animatedValues[key];
  const startTime = Date.now();
  const tick = () => {
    const elapsed = Date.now() - startTime;
    const progress = Math.min(elapsed / duration, 1);
    animatedValues[key] = Math.round(start + (target - start) * progress);
    if (progress < 1) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

const statCards = [
  { bg: 'linear-gradient(135deg, #56ca00, #56ca00)' },
  { bg: 'linear-gradient(135deg, #ff4c51, #ff4c51)' },
  { bg: 'linear-gradient(135deg, #ffb400, #ffb400)' },
  { bg: 'linear-gradient(135deg, #7340e0, #7340e0)' },
  { bg: 'linear-gradient(135deg, #8c57ff, #8c57ff)' },
];

const filteredNodes = computed(() =>
  nodes.value.filter((n) => !searchText.value || n.name.toLowerCase().includes(searchText.value.toLowerCase()) || n.id.toLowerCase().includes(searchText.value.toLowerCase())),
);

function statusLabel(s: string) {
  return ({ online: '在线', offline: '掉线', high_latency: '高延迟' } as any)[s] || s;
}
function riskColor(score: number) {
  if (score >= 50) return 'red';
  if (score >= 30) return 'orange';
  if (score >= 15) return 'purple';
  return 'green';
}
function riskLabel(score: number) {
  if (score >= 50) return '高危';
  if (score >= 30) return '中危';
  if (score >= 15) return '低危';
  return '正常';
}
function severityColor(sev: string) {
  return ({ critical: '#ff4c51', high: '#ffb400', medium: '#7340e0', low: '#8c57ff', info: '#6d6777' } as any)[sev] || '#6d6777';
}
function formatTime(ts: number) {
  if (!ts) return '未知';
  return dayjs(ts).format('MM-DD HH:mm:ss');
}

// ECharts 配置
const mapOption = computed(() => ({
  tooltip: { trigger: 'item' },
  visualMap: {
    min: 0, max: 100,
    text: ['高风险', '低风险'],
    inRange: { color: ['#56ca00', '#ffb400', '#ff4c51'] },
    textStyle: { color: appStore.isDark ? '#c0bec5' : '#433c50' },
  },
  series: [{
    type: 'scatter',
    coordinateSystem: 'geo',
    data: nodes.value.slice(0, 20).map((n, i) => ({
      name: n.name,
      value: [100 + i * 10, 30 + (i % 5) * 8, n.riskScore],
    })),
    symbolSize: (val: any) => Math.max(8, val[2] / 5),
    itemStyle: { color: '#8c57ff' },
  }],
  geo: {
    map: 'world',
    roam: true,
    label: { show: false },
    itemStyle: { areaColor: appStore.isDark ? '#403c5a' : '#f2f0f5', borderColor: appStore.isDark ? '#333' : '#ddd' },
  },
}));

const latencyTrendOption = computed(() => {
  const hours = Array.from({ length: 24 }, (_, i) => `${i}:00`);
  const data = hours.map((_, i) => 50 + Math.sin(i / 3) * 30 + Math.random() * 20);
  return {
    tooltip: { trigger: 'axis' },
    grid: { left: 40, right: 20, top: 30, bottom: 30 },
    xAxis: { type: 'category', data: hours, axisLabel: { color: appStore.isDark ? '#6d6777' : '#433c50' } },
    yAxis: { type: 'value', name: 'ms', axisLabel: { color: appStore.isDark ? '#6d6777' : '#433c50' } },
    series: [{
      type: 'line', smooth: true, data,
      areaStyle: { opacity: 0.3, color: '#8c57ff' },
      itemStyle: { color: '#8c57ff' },
    }],
  };
});

const riskRankOption = computed(() => {
  const top = [...nodes.value].sort((a, b) => b.riskScore - a.riskScore).slice(0, 5);
  return {
    tooltip: { trigger: 'axis' },
    grid: { left: 100, right: 20, top: 20, bottom: 30 },
    xAxis: { type: 'value', axisLabel: { color: appStore.isDark ? '#6d6777' : '#433c50' } },
    yAxis: { type: 'category', data: top.map((n) => n.name), axisLabel: { color: appStore.isDark ? '#6d6777' : '#433c50' } },
    series: [{
      type: 'bar',
      data: top.map((n) => ({ value: n.riskScore, itemStyle: { color: riskColor(n.riskScore) } })),
      barWidth: 20,
    }],
  };
});

// 主机监控（1Panel 风格，host.stats RPC 真实读 /proc）
const hostStats = ref<any>(null);

async function loadHostStats() {
  try {
    hostStats.value = await rpc.call<any>('host.stats');
  } catch (e) {
    // 首次可能失败
  }
}

function usageColor(pct: number): string {
  if (pct >= 90) return '#ff4c51';
  if (pct >= 70) return '#ffb400';
  return '#8c57ff';
}

function formatMb(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)}G`;
  return `${mb}M`;
}

async function refresh() {
  loading.value = true;
  try {
    const res = await rpc.call<any>('dashboard.metrics');
    Object.assign(summary, res.summary || {});
    nodes.value = res.nodes || [];
    recentAlerts.value = res.recentAlerts || [];
    // 数字动画
    animateValue('online', summary.online);
    animateValue('offline', summary.offline);
    animateValue('highLatency', summary.highLatency);
    animateValue('atRisk', summary.atRisk);
    animateValue('channelsError', summary.channelsError);
  } catch (e) {
    // 首次可能无数据
  } finally {
    loading.value = false;
  }
}

function toggleFullscreen() {
  if (!document.fullscreenElement) {
    document.documentElement.requestFullscreen();
    isFullscreen.value = true;
  } else {
    document.exitFullscreen();
    isFullscreen.value = false;
  }
}

let timer: any = null;
let hostTimer: any = null;
onMounted(() => {
  refresh();
  loadHostStats();
  timer = setInterval(refresh, 30000); // 30s 自动刷新
  hostTimer = setInterval(loadHostStats, 5000); // 主机监控 5s 刷新
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
  if (hostTimer) clearInterval(hostTimer);
});
</script>

<style lang="less" scoped>
.ops-dashboard {
  min-height: calc(100vh - var(--navbar-height));
  background: var(--color-bg-2);
  padding: 16px 20px;
  &.dark-mode {
    background: #28243d;
  }
}

/* 主机监控行（1Panel 风格） */
.host-stats-row {
  display: grid;
  grid-template-columns: repeat(5, 1fr);
  gap: 16px;
  margin-bottom: 20px;
  @media (max-width: 1200px) { grid-template-columns: repeat(3, 1fr); }
  @media (max-width: 768px) { grid-template-columns: repeat(2, 1fr); }
}
.host-card {
  background: var(--color-bg-1);
  border-radius: var(--card-radius);
  box-shadow: var(--card-shadow);
  padding: 16px;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  transition: box-shadow 0.2s;
  &:hover { box-shadow: var(--card-shadow-hover); }
  .host-card-title {
    font-size: 13px;
    color: var(--color-text-3);
    margin-bottom: 12px;
    align-self: flex-start;
  }
  .host-card-sub {
    font-size: 11px;
    color: var(--color-text-4);
    margin-top: 10px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
}
.load-values {
  display: flex;
  gap: 18px;
  align-items: center;
  justify-content: center;
  flex: 1;
  .load-item {
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .load-num {
    font-size: 22px;
    font-weight: 700;
    color: var(--brand-primary);
    line-height: 1.2;
  }
  .load-label {
    font-size: 11px;
    color: var(--color-text-4);
    margin-top: 2px;
  }
}
.net-io {
  display: flex;
  flex-direction: column;
  gap: 10px;
  flex: 1;
  justify-content: center;
  .net-item {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 16px;
    font-weight: 600;
    &.rx { color: var(--brand-success); }
    &.tx { color: var(--brand-info); }
    svg { font-size: 15px; }
  }
}

.dashboard-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 20px;
  .header-left {
    display: flex;
    align-items: center;
    gap: 12px;
    h1 { margin: 0; font-size: 22px; color: var(--color-text-1); }
    .header-icon { font-size: 28px; color: rgb(var(--primary-6)); }
  }
}

.stats-row {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 16px;
  margin-bottom: 20px;
}

.stat-card {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 20px;
  border-radius: 12px;
  color: #fff;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
  transition: transform 0.2s, box-shadow 0.2s;
  &:hover {
    transform: translateY(-4px);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.2);
  }
  .stat-icon { font-size: 32px; opacity: 0.9; }
  .stat-value { font-size: 32px; font-weight: 700; line-height: 1; }
  .stat-label { font-size: 13px; opacity: 0.85; margin-top: 4px; }
}

.charts-row {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  gap: 16px;
  margin-bottom: 20px;
}

.chart-card {
  :deep(.arco-card-header) { border-bottom: none; }
  .chart { height: 280px; }
}

.table-card, .alerts-card {
  margin-bottom: 20px;
  :deep(.arco-card-header) { border-bottom: none; }
}

.table-title {
  display: flex;
  align-items: center;
  font-size: 15px;
}

.status-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  margin-right: 6px;
  &.online { background: #56ca00; box-shadow: 0 0 6px #56ca00; }
  &.offline { background: #ff4c51; box-shadow: 0 0 6px #ff4c51; }
  &.high_latency { background: #ffb400; box-shadow: 0 0 6px #ffb400; }
}
.status-text { font-size: 12px; color: var(--color-text-2); }

.device-name { font-weight: 500; color: var(--color-text-1); }
.device-id { font-size: 11px; color: var(--color-text-3); font-family: monospace; }

.latency-high { color: #ff4c51; font-weight: 600; }
.latency-ok { color: #56ca00; }
.muted { color: var(--color-text-3); }

:deep(.arco-card) {
  border-radius: 12px;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.06);
  transition: box-shadow 0.2s;
  &:hover { box-shadow: 0 4px 16px rgba(0, 0, 0, 0.1); }
}
</style>
