<script setup lang="ts">
import { ref } from 'vue'
import Connect from './views/Connect.vue'
import Servers from './views/Servers.vue'
import Resources from './views/Resources.vue'
import Logs from './views/Logs.vue'

interface Tab {
  key: string
  label: string
}

const tabs: Tab[] = [
  { key: 'connect', label: '连接' },
  { key: 'servers', label: '服务器' },
  { key: 'resources', label: '资源' },
  { key: 'logs', label: '日志' },
]

const current = ref('connect')
</script>

<template>
  <div class="app">
    <nav class="sidebar">
      <button
        v-for="t in tabs"
        :key="t.key"
        :class="{ active: current === t.key }"
        @click="current = t.key"
      >
        {{ t.label }}
      </button>
    </nav>
    <main class="content">
      <Connect v-if="current === 'connect'" />
      <Servers v-else-if="current === 'servers'" />
      <Resources v-else-if="current === 'resources'" />
      <Logs v-else-if="current === 'logs'" />
    </main>
  </div>
</template>

<style>
:root {
  --bg: #ffffff;
  --bg-soft: #f5f6f8;
  --border: #e1e4e8;
  --text: #1f2328;
  --text-soft: #6a737d;
  --primary: #0078d7;
  --primary-dark: #005a9e;
  --danger: #d13438;
  --success: #107c10;
  --warn: #ffb900;
}

* {
  box-sizing: border-box;
}

html,
body {
  margin: 0;
  padding: 0;
  height: 100%;
}

#app {
  height: 100%;
}

.app {
  display: flex;
  height: 100vh;
  font-family: system-ui, -apple-system, 'Segoe UI', sans-serif;
  color: var(--text);
  background: var(--bg);
}

.sidebar {
  width: 120px;
  background: var(--bg-soft);
  border-right: 1px solid var(--border);
  padding: 10px;
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex-shrink: 0;
}

.sidebar button {
  padding: 8px 10px;
  border: none;
  background: none;
  cursor: pointer;
  text-align: left;
  border-radius: 4px;
  font-size: 14px;
  color: var(--text);
}

.sidebar button:hover {
  background: #e5e7eb;
}

.sidebar button.active {
  background: var(--primary);
  color: #fff;
}

.content {
  flex: 1;
  padding: 20px;
  overflow-y: auto;
}

button.btn {
  padding: 6px 14px;
  border: 1px solid var(--border);
  background: var(--bg);
  border-radius: 4px;
  cursor: pointer;
  font-size: 14px;
}

button.btn:hover {
  background: var(--bg-soft);
}

button.btn.primary {
  background: var(--primary);
  color: #fff;
  border-color: var(--primary);
}

button.btn.primary:hover {
  background: var(--primary-dark);
}

button.btn.primary:disabled {
  background: #c7d6e8;
  border-color: #c7d6e8;
  cursor: not-allowed;
}

button.btn.danger {
  color: var(--danger);
  border-color: var(--danger);
}

button.btn.danger:hover {
  background: var(--danger);
  color: #fff;
}

input.field,
select.field {
  padding: 6px 8px;
  border: 1px solid var(--border);
  border-radius: 4px;
  font-size: 14px;
  font-family: inherit;
}

input.field:focus,
select.field:focus {
  outline: none;
  border-color: var(--primary);
}
</style>
