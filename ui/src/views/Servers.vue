<script setup lang="ts">
import { reactive, ref } from 'vue'
import { useVpnStore, type Profile } from '../stores/vpn'

const store = useVpnStore()

// 表单弹窗状态
const showForm = ref(false)
const editing = ref(false)
const form = reactive<Profile>(emptyForm())

function emptyForm(): Profile {
  return {
    id: '',
    name: '',
    server: '',
    username: '',
    password: '',
    socks_port: 1080,
    protocol: 'easyconnect',
  }
}

function resetForm() {
  Object.assign(form, emptyForm())
}

function onAdd() {
  resetForm()
  editing.value = false
  showForm.value = true
}

function onEdit(p: Profile) {
  Object.assign(form, p)
  editing.value = true
  showForm.value = true
}

async function onSave() {
  if (!form.name.trim() || !form.server.trim()) {
    alert('名称和服务器不能为空')
    return
  }
  try {
    await store.saveProfile({ ...form })
    showForm.value = false
  } catch (e) {
    alert('保存失败: ' + String(e))
  }
}

async function onDelete(p: Profile) {
  if (!confirm(`确定删除配置「${p.name}」吗？`)) return
  try {
    await store.deleteProfile(p.id)
  } catch (e) {
    alert('删除失败: ' + String(e))
  }
}

function onCancel() {
  showForm.value = false
}
</script>

<template>
  <div class="servers-page">
    <div class="page-head">
      <h2>服务器</h2>
      <button class="btn primary" @click="onAdd">+ 新增</button>
    </div>

    <table class="tbl">
      <thead>
        <tr>
          <th>名称</th>
          <th>协议</th>
          <th>服务器</th>
          <th>用户名</th>
          <th>SOCKS5 端口</th>
          <th class="ops-col">操作</th>
        </tr>
      </thead>
      <tbody>
        <tr v-if="store.profiles.length === 0">
          <td colspan="6" class="empty-row">暂无配置，点「新增」添加</td>
        </tr>
        <tr v-for="p in store.profiles" :key="p.id">
          <td>{{ p.name }}</td>
          <td>
            <span class="proto-tag" :class="p.protocol === 'globalprotect' ? 'gp' : 'ec'">
              {{ p.protocol === 'globalprotect' ? 'GlobalProtect' : 'EasyConnect' }}
            </span>
          </td>
          <td class="mono">{{ p.server }}</td>
          <td>{{ p.username }}</td>
          <td>{{ p.protocol === 'globalprotect' ? '—' : p.socks_port }}</td>
          <td class="ops">
            <button class="btn" @click="onEdit(p)">编辑</button>
            <button class="btn danger" @click="onDelete(p)">删除</button>
          </td>
        </tr>
      </tbody>
    </table>

    <!-- 表单弹窗 -->
    <div v-if="showForm" class="modal-mask" @click.self="onCancel">
      <div class="modal">
        <h3>{{ editing ? '编辑配置' : '新增配置' }}</h3>
        <div class="form-grid">
          <label>名称</label>
          <input v-model="form.name" class="field" placeholder="如：浙大 RVPN" />
          <label>协议</label>
          <select v-model="form.protocol" class="field">
            <option value="easyconnect">EasyConnect（深信服）</option>
            <option value="globalprotect">GlobalProtect（Palo Alto）</option>
          </select>
          <label>服务器</label>
          <input
            v-model="form.server"
            class="field"
            :placeholder="form.protocol === 'globalprotect' ? 'GP 地址:端口，如 114.250.31.2:4430' : 'rvpn.zju.edu.cn:443'"
          />
          <label>用户名</label>
          <input v-model="form.username" class="field" />
          <label>密码</label>
          <input v-model="form.password" class="field" type="password" />
          <label>SOCKS5 端口</label>
          <input
            v-model.number="form.socks_port"
            class="field"
            type="number"
            min="1"
            max="65535"
            :disabled="form.protocol === 'globalprotect'"
          />
          <small v-if="form.protocol === 'globalprotect'" class="hint gp-hint">
            GlobalProtect 协议通过 openconnect 建立 TUN 隧道（需系统安装 openconnect ≥8.0），不使用 SOCKS5 端口。
          </small>
        </div>
        <div class="modal-actions">
          <button class="btn" @click="onCancel">取消</button>
          <button class="btn primary" @click="onSave">保存</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.servers-page {
  max-width: 800px;
}
.page-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}
h2 {
  margin: 0;
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
.ops {
  white-space: nowrap;
}
.ops-col {
  width: 140px;
}
.ops button {
  margin-right: 6px;
}
.ops button:last-child {
  margin-right: 0;
}

.modal-mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.4);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
}
.modal {
  background: var(--bg);
  border-radius: 8px;
  padding: 20px 24px;
  width: 420px;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.2);
}
.modal h3 {
  margin: 0 0 16px;
}
.form-grid {
  display: grid;
  grid-template-columns: 100px 1fr;
  gap: 10px 12px;
  align-items: center;
}
.form-grid label {
  color: var(--text-soft);
  font-size: 14px;
}
.modal-actions {
  margin-top: 18px;
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
/* 协议标签 */
.proto-tag {
  display: inline-block;
  padding: 2px 8px;
  border-radius: 4px;
  font-size: 12px;
  font-weight: 500;
}
.proto-tag.ec {
  background: var(--bg-soft);
  color: var(--text-soft);
}
.proto-tag.gp {
  background: #e8f0fe;
  color: #1a73e8;
}
/* 表单内提示 */
.hint {
  grid-column: 2 / 3;
  color: var(--text-soft);
  font-size: 12px;
  margin-top: -4px;
}
.gp-hint {
  white-space: normal;
  line-height: 1.4;
}
.field:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
