<script setup lang="ts">
import { computed, ref } from 'vue'
import { useVpnStore, isActive } from '../stores/vpn'

const store = useVpnStore()

// 本地覆盖输入：用户可在连接前临时改 server/username（不写回 profile）。
// 空字符串表示用 profile 原值。
const overrideServer = ref('')
const overrideUsername = ref('')
// 连接错误信息（显示在页面上，替代只 console.error）。
const errorMsg = ref('')

const selected = computed(() => store.selectedProfile)

const statusText = computed(() => {
  switch (store.status.state) {
    case 'Disconnected':
      return '未连接'
    case 'Connecting':
      return '连接中...'
    case 'Connected':
      return '已连接'
    case 'Error':
      return `错误: ${store.status.message}`
    default:
      return '未知'
  }
})

const statusColor = computed(() => {
  switch (store.status.state) {
    case 'Connected':
      return 'var(--success)'
    case 'Connecting':
      return 'var(--warn)'
    case 'Error':
      return 'var(--danger)'
    default:
      return 'var(--text-soft)'
  }
})

const busy = computed(() => store.status.state === 'Connecting')

async function onConnect() {
  if (!selected.value) return
  errorMsg.value = ''
  try {
    await store.connect(selected.value.id)
  } catch (e) {
    console.error('connect failed:', e)
    errorMsg.value = String(e)
  }
}

async function onDisconnect() {
  try {
    await store.disconnect()
  } catch (e) {
    console.error('disconnect failed:', e)
    errorMsg.value = String(e)
  }
}

async function onModeChange(e: Event) {
  const mode = (e.target as HTMLSelectElement).value as 'pac' | 'tun'
  await store.saveSettings({ ...store.settings, proxy_mode: mode })
}
</script>

<template>
  <div class="connect-page">
    <h2>连接</h2>

    <div v-if="store.profiles.length === 0" class="empty">
      还没有服务器配置，请先到「服务器」页添加一个。
    </div>

    <template v-else>
      <div class="field-row">
        <label>配置</label>
        <select
          v-model="store.selectedProfileId"
          class="field"
          :disabled="isActive(store.status)"
        >
          <option v-for="p in store.profiles" :key="p.id" :value="p.id">
            {{ p.name }} ({{ p.server }})
          </option>
        </select>
      </div>

      <div v-if="selected" class="profile-detail">
        <div class="field-row">
          <label>服务器</label>
          <input
            v-model="overrideServer"
            class="field"
            :placeholder="selected.server"
            :disabled="isActive(store.status)"
          />
        </div>
        <div class="field-row">
          <label>用户名</label>
          <input
            v-model="overrideUsername"
            class="field"
            :placeholder="selected.username"
            :disabled="isActive(store.status)"
          />
        </div>
        <div class="field-row">
          <label>SOCKS5 端口</label>
          <input class="field" :value="selected.socks_port" disabled />
        </div>
      </div>

      <div class="field-row">
        <label>代理模式</label>
        <select
          class="field"
          :disabled="isActive(store.status)"
          :value="store.settings.proxy_mode"
          @change="onModeChange($event)"
        >
          <option value="pac">系统代理（无需管理员）</option>
          <option value="tun">TUN 全局代理（需管理员）</option>
        </select>
      </div>

      <div class="actions">
        <button
          v-if="!isActive(store.status)"
          class="btn primary"
          :disabled="busy || !selected"
          @click="onConnect"
        >
          连接
        </button>
        <button
          v-else
          class="btn danger"
          :disabled="busy && store.status.state !== 'Connecting'"
          @click="onDisconnect"
        >
          断开
        </button>
      </div>

      <div v-if="errorMsg" class="error-box">
        {{ errorMsg }}
      </div>

      <div class="status-card" :style="{ borderColor: statusColor }">
        <div class="status-head">
          <span class="dot" :style="{ background: statusColor }"></span>
          <span class="status-text" :style="{ color: statusColor }">{{ statusText }}</span>
        </div>
        <div v-if="store.status.state === 'Connected'" class="status-info">
          <div><span class="k">内网 IP</span><span class="v">{{ store.status.client_ip || '-' }}</span></div>
          <div><span class="k">SOCKS5</span><span class="v">{{ store.status.socks_bind }}</span></div>
        </div>
        <div v-else-if="store.status.state === 'Error'" class="status-info">
          <div><span class="k">错误信息</span><span class="v">{{ store.status.message }}</span></div>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.connect-page {
  max-width: 560px;
}
h2 {
  margin: 0 0 16px;
}
.error-box {
  background: var(--danger, #dc3545);
  color: white;
  padding: 10px 14px;
  border-radius: 6px;
  margin-bottom: 12px;
  font-size: 13px;
  word-break: break-all;
}
.empty {
  color: var(--text-soft);
  padding: 20px;
  background: var(--bg-soft);
  border-radius: 6px;
  border: 1px dashed var(--border);
}
.field-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}
.field-row label {
  width: 90px;
  color: var(--text-soft);
  font-size: 14px;
  flex-shrink: 0;
}
.field-row .field {
  flex: 1;
}
.profile-detail {
  margin-top: 4px;
}
.actions {
  margin-top: 8px;
  display: flex;
  gap: 8px;
}
.status-card {
  margin-top: 20px;
  padding: 16px;
  border: 1px solid var(--border);
  border-left: 4px solid var(--border);
  border-radius: 6px;
  background: var(--bg-soft);
}
.status-head {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
}
.dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  display: inline-block;
}
.status-info {
  margin-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: 14px;
}
.status-info .k {
  display: inline-block;
  width: 80px;
  color: var(--text-soft);
}
.status-info .v {
  font-family: ui-monospace, 'Cascadia Code', Consolas, monospace;
}
</style>
