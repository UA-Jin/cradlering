<template>
  <div class="layout-navbar">
    <!-- 左侧：折叠按钮 + 面包屑 -->
    <div class="navbar-left">
      <a-button type="text" shape="circle" @click="appStore.menuCollapse = !appStore.menuCollapse">
        <template #icon>
          <icon-menu-fold v-if="!appStore.menuCollapse" />
          <icon-menu-unfold v-else />
        </template>
      </a-button>
      <a-breadcrumb class="navbar-breadcrumb">
        <a-breadcrumb-item v-for="item in breadcrumbs" :key="item.path">
          {{ item.label }}
        </a-breadcrumb-item>
      </a-breadcrumb>
    </div>

    <!-- 中间：搜索 -->
    <div class="navbar-center">
      <a-input-search
        v-model="searchQuery"
        :style="{ width: '320px' }"
        placeholder="搜索会话、记忆、任务..."
        allow-clear
        @search="onSearch"
      />
    </div>

    <!-- 右侧：通知 + 主题 + 设置 + 用户 -->
    <div class="navbar-right">
      <a-badge :count="pendingApprovals" :max-count="99" dot>
        <a-button type="text" shape="circle" @click="$router.push('/approvals/instances')">
          <icon-notification />
        </a-button>
      </a-badge>

      <a-tooltip :content="appStore.isDark ? '浅色模式' : '深色模式'">
        <a-button type="text" shape="circle" @click="appStore.toggleDark()">
          <icon-moon-fill v-if="!appStore.isDark" />
          <icon-sun-fill v-else />
        </a-button>
      </a-tooltip>

      <a-dropdown trigger="click">
        <div class="navbar-user">
          <div class="user-avatar" :style="{ background: 'var(--primary-6)', boxShadow: 'var(--shadow-xs)' }">
            {{ (userStore.user?.displayName || 'U').charAt(0) }}
          </div>
          <span class="user-name">{{ userStore.user?.displayName || '未登录' }}</span>
          <a-tag v-if="userStore.user" :color="roleColor" size="small">{{ roleLabel }}</a-tag>
        </div>
        <template #content>
          <a-doption @click="$router.push('/settings')">
            <template #icon><icon-settings /></template>
            个人设置
          </a-doption>
          <a-doption v-if="userStore.isAdmin" @click="$router.push('/users')">
            <template #icon><icon-user-group /></template>
            用户管理
          </a-doption>
          <a-doption @click="onLogout">
            <template #icon><icon-export /></template>
            退出登录
          </a-doption>
        </template>
      </a-dropdown>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { useRouter, useRoute } from 'vue-router';
import { useAppStore } from '@/stores/app';
import { useUserStore } from '@/stores/user';
import { rpc } from '@/api/rpc';

const router = useRouter();
const route = useRoute();
const appStore = useAppStore();
const userStore = useUserStore();

const searchQuery = ref('');
const pendingApprovals = ref(0);

const roleLabel = computed(() => {
  const map: Record<string, string> = {
    admin: '管理员', manager: '经理', supervisor: '主管',
    operator: '操作员', viewer: '访客',
  };
  return map[userStore.role] || userStore.role;
});

const roleColor = computed(() => {
  const map: Record<string, string> = {
    admin: 'red', manager: 'purple', supervisor: 'blue',
    operator: 'green', viewer: 'gray',
  };
  return map[userStore.role] || 'gray';
});

const breadcrumbs = computed(() =>
  route.matched.filter((r) => r.meta?.label).map((r) => ({ path: r.path, label: r.meta.label as string }))
);

function onSearch(v: string) {
  if (v) router.push({ path: '/sessions', query: { q: v } });
}

function onLogout() {
  userStore.logout();
  router.push('/login');
}

async function refreshPending() {
  try {
    const res = await rpc.call<{ pending: number }>('approval.stats');
    pendingApprovals.value = res.pending || 0;
  } catch { /* ignore */ }
}

let timer: any = null;
onMounted(() => {
  refreshPending();
  timer = setInterval(refreshPending, 30000);
});
onUnmounted(() => { if (timer) clearInterval(timer); });
</script>

<style lang="less" scoped>
.layout-navbar {
  height: var(--navbar-height);
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 24px;
  gap: 16px;
}

.navbar-left {
  display: flex;
  align-items: center;
  gap: 12px;
}

.navbar-breadcrumb {
  font-size: 14px;
}

.navbar-center {
  flex: 1;
  display: flex;
  justify-content: center;
}

.navbar-right {
  display: flex;
  align-items: center;
  gap: 8px;
}

.navbar-user {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-radius: 10px;
  cursor: pointer;
  transition: background 0.2s;
  &:hover { background-color: var(--color-bg-3); }

  .user-avatar {
    width: 32px;
    height: 32px;
    border-radius: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #fff;
    font-weight: 600;
    font-size: 14px;
    flex-shrink: 0;
  }

  .user-name {
    font-size: 14px;
    color: var(--color-text-1);
    font-weight: 500;
  }
}
</style>
