import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  // 端口 1420 对应 tauri.conf.json 的 devUrl
  server: { port: 1420 },
  build: {
    outDir: 'dist',
    // Tauri 期望用相对路径加载资源
    emptyOutDir: true,
  },
})
