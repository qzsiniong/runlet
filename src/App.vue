<script setup lang="ts">
import { getCurrentWindow } from '@tauri-apps/api/window'
import { computed } from 'vue'

// 按**窗口 label**（而不是路由）决定布局：进度/待命是纯独立窗口；
// 脚本窗口自带侧边栏（ScriptsSidebar），App 只提供一个整屏容器。
const windowLabel = getCurrentWindow().label

/** 裸窗：整页就是它自己，连外层容器都不要（进度与待命是浮动小窗）。 */
const bareWindow = computed(
  () =>
    windowLabel === 'progress'
    || windowLabel === 'standby',
)
</script>

<template>
  <!-- 独立窗口(进度/待命):纯页面 -->
  <router-view v-if="bareWindow" />

  <!-- 脚本窗口:外层提供 h-screen，页面自己负责横向分栏 -->
  <div v-else class="h-screen overflow-hidden">
    <router-view />
  </div>
</template>
