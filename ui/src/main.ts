import { createApp } from 'vue'
import { createPinia } from 'pinia'
import App from './App.vue'
import { useVpnStore } from './stores/vpn'

const app = createApp(App)
app.use(createPinia())
app.mount('#app')

// 初始化 VPN store：加载 profiles、监听事件、拉取初始状态。
// 放在 mount 之后，确保错误不会阻塞首屏渲染。
const store = useVpnStore()
store.init().catch((e) => {
  // 非 Tauri 环境（纯浏览器调试）invoke 会失败，这里只打日志不抛。
  console.error('[vpn] init failed:', e)
})
