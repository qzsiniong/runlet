import { createRouter, createWebHashHistory } from 'vue-router'

export const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    // 根路径没有独立页面（总览已移除）：落到脚本窗口
    { path: '/', redirect: '/scripting' },
    // 脚本主窗口(label `scripting`)，见 tauri.conf.json 与 pages/ScriptsWindow.vue
    { path: '/scripting', name: 'scripting', component: () => import('@/pages/ScriptsWindow.vue') },
    { path: '/progress', name: 'progress', component: () => import('@/pages/Progress.vue') },
    // 脚本进度条窗口（label `script-progress`，$.progress* 专用，见 scripting.rs）
    {
      path: '/script-progress',
      name: 'script-progress',
      component: () => import('@/pages/ScriptProgress.vue'),
    },
    { path: '/standby', name: 'standby', component: () => import('@/pages/Standby.vue') },
  ],
})
