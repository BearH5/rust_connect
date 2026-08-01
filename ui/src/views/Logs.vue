<script setup lang="ts">
import { nextTick, ref, watch } from 'vue'
import { useVpnStore } from '../stores/vpn'

const store = useVpnStore()
const container = ref<HTMLDivElement | null>(null)

// 日志变化时自动滚到底部。
watch(
  () => store.logs.length,
  async () => {
    await nextTick()
    const el = container.value
    if (el) el.scrollTop = el.scrollHeight
  },
)

function levelClass(level: string): string {
  const l = String(level || '').toLowerCase()
  if (l === 'error' || l === 'err') return 'lvl-error'
  if (l === 'warn' || l === 'warning') return 'lvl-warn'
  return 'lvl-info'
}

function fmtTime(ts: string): string {
  if (!ts) return ''
  // 已经是 ISO 字符串，只取时分秒毫秒部分。
  const d = new Date(ts)
  if (Number.isNaN(d.getTime())) return ts
  const pad = (n: number, w = 2) => String(n).padStart(w, '0')
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(
    d.getMilliseconds(),
    3,
  )}`
}

function onClear() {
  store.logs.splice(0, store.logs.length)
}
</script>

<template>
  <div class="logs-page">
    <div class="page-head">
      <h2>日志</h2>
      <button class="btn" @click="onClear">清空</button>
    </div>
    <div ref="container" class="log-box">
      <div v-if="store.logs.length === 0" class="empty">暂无日志</div>
      <div v-for="(log, i) in store.logs" :key="i" class="log-line" :class="levelClass(log.level)">
        <span class="ts">{{ fmtTime(log.timestamp) }}</span>
        <span class="lvl">{{ log.level }}</span>
        <span class="msg">{{ log.message }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.logs-page {
  display: flex;
  flex-direction: column;
  height: 100%;
}
.page-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}
h2 {
  margin: 0;
}
.log-box {
  flex: 1;
  overflow-y: auto;
  background: #1e1e1e;
  border-radius: 6px;
  padding: 10px;
  font-family: ui-monospace, 'Cascadia Code', Consolas, monospace;
  font-size: 13px;
  line-height: 1.6;
}
.empty {
  color: #888;
  text-align: center;
  padding: 24px;
}
.log-line {
  display: flex;
  gap: 8px;
  color: #d4d4d4;
  word-break: break-all;
}
.log-line .ts {
  color: #888;
  flex-shrink: 0;
}
.log-line .lvl {
  flex-shrink: 0;
  width: 56px;
  text-transform: uppercase;
  font-weight: 600;
}
.log-line .msg {
  flex: 1;
}
.lvl-info .lvl {
  color: #9cdcfe;
}
.lvl-warn {
  color: #ffd700;
}
.lvl-warn .lvl {
  color: #ffd700;
}
.lvl-error {
  color: #f48771;
}
.lvl-error .lvl {
  color: #f48771;
}
</style>
