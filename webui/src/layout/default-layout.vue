<template>
  <a-layout class="layout-wrapper">
    <!-- 侧栏（白色，Materialize 风格） -->
    <a-layout-sider
      :width="260"
      :collapsed-width="64"
      :collapsible="true"
      :collapsed="appStore.menuCollapse"
      @collapse="appStore.menuCollapse = $event"
      class="layout-sider"
      :style="{ position: 'fixed', height: '100vh', left: 0, top: 0, zIndex: 100 }"
    >
      <!-- Logo -->
      <div class="app-brand">
        <div class="app-brand-logo">
          <svg viewBox="0 0 34 28" fill="none" xmlns="http://www.w3.org/2000/svg">
            <path d="M2 4 L12 10 L12 22 L2 16 Z" fill="#8c57ff"/>
            <path d="M12 10 L22 4 L22 16 L12 22 Z" fill="#7e4ee6"/>
            <path d="M22 4 L32 10 L32 22 L22 16 Z" fill="#a785fa"/>
            <path d="M12 10 L12 22 L17 25 L17 13 Z" fill="#6d40d8" opacity="0.7"/>
          </svg>
        </div>
        <transition name="fade">
          <span v-if="!appStore.menuCollapse" class="app-brand-text">CradleRing</span>
        </transition>
      </div>
      <app-menu :collapsed="appStore.menuCollapse" />
    </a-layout-sider>

    <!-- 主内容区 -->
    <a-layout class="layout-page" :style="{ marginLeft: appStore.menuCollapse ? '64px' : '260px', transition: 'margin-left 0.25s ease' }">
      <!-- 顶部导航（detached 风格：透明底，悬浮于内容之上） -->
      <a-layout-header class="layout-header">
        <app-navbar />
      </a-layout-header>

      <!-- 页面内容 -->
      <a-layout-content class="layout-content">
        <router-view v-slot="{ Component }">
          <transition name="fade" mode="out-in">
            <component :is="Component" />
          </transition>
        </router-view>
      </a-layout-content>
    </a-layout>
  </a-layout>
</template>

<script setup lang="ts">
import { useAppStore } from '@/stores/app';
import AppNavbar from './navbar/index.vue';
import AppMenu from './menu/index.vue';

const appStore = useAppStore();
</script>

<style lang="less" scoped>
.layout-wrapper {
  height: 100vh;
  background-color: var(--color-bg-2);
}

/* 侧栏：白色底 + 右侧轻阴影（Materialize layout-menu） */
.layout-sider {
  background-color: var(--color-bg-1);
  box-shadow: 0 0.125rem 0.5rem 0 rgba(46, 38, 61, 0.08);
  :deep(.arco-layout-sider-children) {
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
}

/* Logo 区 */
.app-brand {
  height: var(--navbar-height);
  display: flex;
  align-items: center;
  padding: 0 22px;
  gap: 12px;
  flex-shrink: 0;

  .app-brand-logo {
    width: 34px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    svg { width: 34px; height: 28px; }
  }

  .app-brand-text {
    font-size: 18px;
    font-weight: 700;
    color: var(--color-text-1);
    letter-spacing: 0.5px;
    white-space: nowrap;
  }
}

/* 顶栏：detached 风格（透明底，无边框） */
.layout-header {
  height: var(--navbar-height);
  padding: 0;
  background-color: transparent;
  border: none;
  position: sticky;
  top: 0;
  z-index: 99;
  flex-shrink: 0;
}

.layout-content {
  background-color: var(--color-bg-2);
  overflow-y: auto;
  flex: 1;
}

@media (max-width: 768px) {
  .layout-page {
    margin-left: 64px !important;
  }
}
</style>
