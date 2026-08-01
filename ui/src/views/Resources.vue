<script setup lang="ts">
import { ref } from 'vue'
import { useVpnStore, type ResourceEntry } from '../stores/vpn'

const store = useVpnStore()
const copiedHost = ref('')

async function copyRow(r: ResourceEntry) {
  const text = `${r.host}:${r.port}`
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text)
    } else {
      // 兜底：用临时 textarea
      const ta = document.createElement('textarea')
      ta.value = text
      ta.style.position = 'fixed'
      ta.style.opacity = '0'
      document.body.appendChild(ta)
      ta.select()
      document.execCommand('copy')
      document.body.removeChild(ta)
    }
    copiedHost.value = text
    setTimeout(() => {
      if (copiedHost.value === text) copiedHost.value = ''
    }, 1500)
  } catch (e) {
    console.error('copy failed:', e)
  }
}
</script>

<template>
  <div class="resources-page">
    <div class="page-head">
      <h2>资源</h2>
      <span class="hint">点击行复制 host:port 到剪贴板</span>
    </div>

    <table class="tbl">
      <thead>
        <tr>
          <th>名称</th>
          <th>Host</th>
          <th>端口</th>
        </tr>
      </thead>
      <tbody>
        <tr v-if="store.resources.length === 0">
          <td colspan="3" class="empty-row">暂无资源（连接成功后由后端推送）</td>
        </tr>
        <tr
          v-for="r in store.resources"
          :key="`${r.host}:${r.port}:${r.name}`"
          class="row"
          :title="`复制 ${r.host}:${r.port}`"
          @click="copyRow(r)"
        >
          <td>{{ r.name }}</td>
          <td class="mono">{{ r.host }}</td>
          <td class="mono">{{ r.port }}</td>
        </tr>
      </tbody>
    </table>

    <div v-if="copiedHost" class="toast">已复制：{{ copiedHost }}</div>
  </div>
</template>

<style scoped>
.resources-page {
  max-width: 700px;
}
.page-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 16px;
}
h2 {
  margin: 0;
}
.hint {
  color: var(--text-soft);
  font-size: 13px;
}
.tbl {
  width: 100%;
  border-collapse: collapse;
  font-size: 14px;
}
.tbl th,
.tbl td {
  text-align: left;
  padding: 8px 10px;
  border-bottom: 1px solid var(--border);
}
.tbl th {
  color: var(--text-soft);
  font-weight: 600;
  background: var(--bg-soft);
}
.mono {
  font-family: ui-monospace, 'Cascadia Code', Consolas, monospace;
}
.empty-row {
  text-align: center;
  color: var(--text-soft);
  padding: 24px;
}
.row {
  cursor: pointer;
  transition: background 0.1s;
}
.row:hover {
  background: var(--bg-soft);
}
.toast {
  position: fixed;
  bottom: 20px;
  left: 50%;
  transform: translateX(-50%);
  background: rgba(0, 0, 0, 0.8);
  color: #fff;
  padding: 8px 16px;
  border-radius: 4px;
  font-size: 13px;
  font-family: ui-monospace, 'Cascadia Code', Consolas, monospace;
}
</style>
