<script setup lang="ts">
import { ref } from 'vue'
import Connect from './views/Connect.vue'
import Servers from './views/Servers.vue'
import Resources from './views/Resources.vue'
import Logs from './views/Logs.vue'
import { useUpdaterStore } from './stores/updater'

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
const updater = useUpdaterStore()

/** 手动检查更新（sidebar 底部入口）。 */
function onCheckUpdate() {
  updater.check(true)
}
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
      <div class="sidebar-footer">
        <button class="check-update" :disabled="updater.status === 'checking' || updater.status === 'downloading'" @click="onCheckUpdate">
          {{ updater.status === 'checking' ? '检查中...' : '检查更新' }}
        </button>
        <div v-if="updater.status === 'uptodate'" class="update-feedback ok">已是最新版本</div>
        <div v-else-if="updater.status === 'error'" class="update-feedback err" :title="updater.errorMsg">检查失败</div>
        <div class="version">v{{ updater.currentVersion || '...' }}</div>
      </div>
    </nav>
    <main class="content">
      <!-- 更新横幅：非阻断，可忽略 -->
      <div v-if="updater.showBanner" class="update-banner">
        <div class="update-info">
          <span class="update-title">发现新版本 v{{ updater.version }}</span>
          <span v-if="updater.notes" class="update-notes">{{ updater.notes }}</span>
        </div>
        <div class="update-actions">
          <template v-if="updater.status === 'available'">
            <button class="btn primary" @click="updater.downloadAndInstall()">立即更新</button>
            <button class="btn" @click="updater.dismiss()">稍后</button>
          </template>
          <template v-else-if="updater.status === 'downloading'">
            <div class="progress">
              <div class="progress-bar" :style="{ width: updater.progressPercent + '%' }"></div>
            </div>
            <span class="progress-text">
              {{ updater.total > 0 ? `${updater.progressPercent}%` : '下载中...' }}
            </span>
          </template>
        </div>
      </div>
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

/* sidebar 底部：检查更新入口 + 版本号 */
.sidebar-footer {
  margin-top: auto;
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-top: 8px;
}
.sidebar-footer .check-update {
  padding: 6px 10px;
  border: 1px solid var(--border);
  background: var(--bg);
  border-radius: 4px;
  cursor: pointer;
  font-size: 12px;
  color: var(--text);
  text-align: center;
}
.sidebar-footer .check-update:hover:not(:disabled) {
  background: #e5e7eb;
}
.sidebar-footer .check-update:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.update-feedback {
  font-size: 11px;
  text-align: center;
}
.update-feedback.ok {
  color: var(--success);
}
.update-feedback.err {
  color: var(--danger);
}
.sidebar-footer .version {
  font-size: 11px;
  color: var(--text-soft);
  text-align: center;
}

/* 更新横幅（非阻断） */
.update-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 14px;
  margin-bottom: 14px;
  background: #e8f0fe;
  border: 1px solid #c5d8f0;
  border-radius: 6px;
  font-size: 13px;
}
.update-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.update-title {
  font-weight: 600;
  color: #1a56b0;
}
.update-notes {
  color: var(--text-soft);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 460px;
}
.update-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.update-actions .progress {
  width: 140px;
  height: 8px;
  background: #c9d9ef;
  border-radius: 4px;
  overflow: hidden;
}
.update-actions .progress-bar {
  height: 100%;
  background: var(--primary);
  border-radius: 4px;
  transition: width 0.2s ease;
}
.progress-text {
  color: var(--text-soft);
  font-size: 12px;
  white-space: nowrap;
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
