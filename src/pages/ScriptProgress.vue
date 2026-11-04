<script setup lang="ts">
import type { UnlistenFn } from '@tauri-apps/api/event'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Check, X } from 'lucide-vue-next'
import { computed, onMounted, onUnmounted, ref } from 'vue'

/**
 * 脚本进度条窗口（label `script-progress`，`$.progress*` 专用）。
 *
 * 数据只来自 Rust 侧的 `script-progress` 事件（见 scripting.rs 的 ScriptBar 状态机）：
 * 渲染对齐 indicatif 模板 `{spinner} {msg} [{bar}] {pos}/{len} ({percent}) 已用 {elapsed} 剩余 {eta}`。
 * total 缺省为不定长模式（spinner + msg + 已用）；收到 finished / abandoned 3 秒后自动收起
 * （用户碰过窗口就一直留着，由 ✕ 手动关）—— 与 Progress.vue 的约定一致。
 */

interface ScriptProgressPayload {
  /** running | finished | abandoned */
  state: 'running' | 'finished' | 'abandoned'
  /** 第几条进度条（每次 $.progress 递增）：用来重置计时，不用猜是不是新条。 */
  generation: number
  pos: number
  /** null = 不定长模式。 */
  total: number | null
  msg: string
  ts: number
}

/** 终态后、用户没碰过窗口时，多久自动收起。 */
const AUTO_HIDE_MS = 3000

const state = ref<ScriptProgressPayload['state'] | 'idle'>('idle')
const pos = ref(0)
const total = ref<number | null>(null)
const msg = ref('')
/** 当前条目代数；generation 变化即视为新条。 */
const generation = ref(-1)
/** 本条进度条首事件的时间戳（elapsed 起点）。 */
const startedTs = ref(0)
const now = ref(Date.now())
/** 用户点过/拖过这个窗口 = 关心这次结果 → 终态后不再自动收起。 */
const userInteracted = ref(false)

const unlistens: UnlistenFn[] = []
let tickTimer: number | null = null
let hideTimer: number | null = null
/** 上一条事件样本（pos + ts），估算速率用。 */
const prevSample = ref<{ pos: number, ts: number } | null>(null)

function startTick() {
  if (tickTimer !== null)
    return
  tickTimer = window.setInterval(() => {
    now.value = Date.now()
  }, 500)
}

function stopTick() {
  if (tickTimer !== null) {
    clearInterval(tickTimer)
    tickTimer = null
  }
}

function scheduleHide() {
  if (userInteracted.value)
    return
  if (hideTimer !== null)
    clearTimeout(hideTimer)
  hideTimer = window.setTimeout(async () => {
    stopTick()
    await getCurrentWindow().hide()
  }, AUTO_HIDE_MS)
}

function clearHide() {
  if (hideTimer !== null) {
    clearTimeout(hideTimer)
    hideTimer = null
  }
}

/** ✕：立即收起（位置由 Rust 侧记住，下次建条还在原地弹出）。 */
async function hideNow() {
  clearHide()
  stopTick()
  await getCurrentWindow().hide()
}

/**
 * 按住窗口任意空白处拖动（✕ 按钮除外），并标记「用户在关心这次结果」。
 * 浏览器 mock 下没有真实窗口，`startDragging` 会静默失败，忽略即可。
 */
async function onPointerDown(e: PointerEvent) {
  markInterested()
  const target = e.target as HTMLElement | null
  if (target?.closest('[data-no-drag]'))
    return
  try {
    await getCurrentWindow().startDragging()
  }
  catch {
    // dev mock / 无窗口环境下没有 startDragging，忽略
  }
}

/**
 * 标记「用户在关心这次结果」：终态后不再自动收起。
 *
 * 两个来源：这里的鼠标事件，以及 Rust 侧发来的 `progress-interacted`
 * （macOS 上点击未激活窗口时鼠标事件可能被系统吞掉，只有窗口焦点是可靠的）。
 */
function markInterested() {
  userInteracted.value = true
  clearHide()
}

function onScriptProgress(p: ScriptProgressPayload) {
  if (p.generation !== generation.value) {
    // 新条（$.progress 重复调用 = 重置）：重置计时与速率样本
    generation.value = p.generation
    startedTs.value = p.ts
    prevSample.value = null
    userInteracted.value = false
    clearHide()
    startTick()
  }
  prevSample.value = { pos: p.pos, ts: p.ts }
  state.value = p.state
  pos.value = p.pos
  total.value = p.total
  msg.value = p.msg
  now.value = Date.now()
  if (p.state !== 'running') {
    stopTick()
    scheduleHide()
  }
}

const isRunning = computed(() => state.value === 'running')

const elapsedMs = computed(() =>
  startedTs.value ? Math.max(0, now.value - startedTs.value) : 0,
)

/** 速率（单位/秒）：用最近两个事件样本线性估计。 */
const speed = computed(() => {
  if (!prevSample.value)
    return 0
  const dt = (prevSample.value.ts - startedTs.value) / 1000
  if (dt < 0.5 || prevSample.value.pos === 0)
    return 0
  return prevSample.value.pos / dt
})

const remainingMs = computed(() => {
  if (!isRunning.value || total.value === null || total.value === 0 || speed.value <= 0)
    return 0
  return Math.max(0, ((total.value - pos.value) / speed.value) * 1000)
})

const progressPercent = computed(() =>
  total.value && total.value > 0 ? Math.min(100, (pos.value / total.value) * 100) : 0,
)

const stateText = computed(() => {
  if (state.value === 'running')
    return '进行中'
  if (state.value === 'finished')
    return '完成'
  if (state.value === 'abandoned')
    return '已放弃'
  return ''
})

const stateClass = computed(() => {
  if (state.value === 'running')
    return 'bg-primary/10 text-primary'
  if (state.value === 'finished')
    return 'bg-green-500/10 text-green-600'
  return 'bg-muted text-muted-foreground'
})

function fmtDuration(ms: number): string {
  const s = Math.floor(ms / 1000)
  if (s < 60)
    return `${s}s`
  const m = Math.floor(s / 60)
  return `${m}m${s % 60}s`
}

onMounted(async () => {
  unlistens.push(await listen<ScriptProgressPayload>('script-progress', e => onScriptProgress(e.payload)))
  // Rust 侧发现窗口被点/拖过（窗口获得焦点）→ 与上面的鼠标事件同一个含义
  unlistens.push(await listen('progress-interacted', markInterested))
})

onUnmounted(() => {
  unlistens.forEach(fn => fn())
  stopTick()
  clearHide()
})
</script>

<template>
  <!-- 悬浮面板（420×110）：spinner/终态图标 + 状态 + 百分比 + 关闭；bar；msg + 统计。整块可拖，✕ 除外。 -->
  <div
    class="flex h-screen flex-col justify-center gap-1.5 rounded-xl bg-background px-4 select-none shadow-lg"
    @pointerdown="onPointerDown"
    @mouseenter="markInterested"
  >
    <!-- 第一行：spinner/图标 + 状态 + 百分比 + 关闭 -->
    <div class="flex items-center gap-2">
      <span
        v-if="state === 'running'"
        class="h-3 w-3 shrink-0 animate-spin rounded-full border-2 border-primary border-t-transparent"
      />
      <Check v-else-if="state === 'finished'" class="h-4 w-4 shrink-0 text-green-600" />
      <X v-else class="h-4 w-4 shrink-0 text-muted-foreground" />
      <span class="shrink-0 text-sm font-semibold">脚本</span>
      <span v-if="stateText" class="shrink-0 rounded px-1.5 py-px text-[11px]" :class="stateClass">
        {{ stateText }}
      </span>
      <span class="ml-auto shrink-0 text-xs font-medium tabular-nums text-muted-foreground">
        <template v-if="total !== null">{{ progressPercent.toFixed(0) }}%</template>
      </span>
      <button
        data-no-drag
        type="button"
        class="shrink-0 rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
        title="关闭"
        @click="hideNow"
      >
        <X class="h-4 w-4" />
      </button>
    </div>

    <!-- 第二行：进度条（不定长模式没有 bar，让位给 msg） -->
    <div v-if="total !== null" class="h-2 w-full overflow-hidden rounded-full bg-muted">
      <div
        class="h-full bg-primary transition-all duration-300"
        :style="{ width: `${progressPercent}%` }"
      />
    </div>

    <!-- 第三行：消息 + pos/total + 剩余/已用 -->
    <div class="flex items-center gap-2 text-[11px] text-muted-foreground">
      <span class="min-w-0 flex-1 truncate font-medium" :class="state === 'finished' ? 'text-foreground' : ''" :title="msg">
        {{ msg || '…' }}
      </span>
      <template v-if="total !== null">
        <span class="shrink-0 tabular-nums">{{ pos }}/{{ total }}</span>
        <span v-if="remainingMs > 0" class="shrink-0 tabular-nums">剩余 {{ fmtDuration(remainingMs) }}</span>
      </template>
      <span class="shrink-0 tabular-nums">已用 {{ fmtDuration(elapsedMs) }}</span>
    </div>
  </div>
</template>
