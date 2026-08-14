import { defineStore } from 'pinia'
import { check, type Update } from '@tauri-apps/plugin-updater'
import { relaunch } from '@tauri-apps/plugin-process'
import { getVersion } from '@tauri-apps/api/app'

/** 更新检查状态机。 */
export type UpdaterStatus =
  | 'idle' // 未检查
  | 'checking' // 检查中
  | 'available' // 有新版本，等用户确认
  | 'downloading' // 下载安装中
  | 'uptodate' // 已是最新（手动检查后的反馈）
  | 'error' // 检查/下载失败

/** 手动检查后无更新/失败的反馈自动消失时间。 */
const FEEDBACK_RESET_MS = 4000

/** check() 拿到的 Update 对象（含 downloadAndInstall 方法）。
 * 放模块级而非 store state：类实例不宜进响应式 state。 */
let pendingUpdate: Update | null = null

export const useUpdaterStore = defineStore('updater', {
  state: () => ({
    status: 'idle' as UpdaterStatus,
    /** 当前应用版本号（启动时拉取）。 */
    currentVersion: '',
    /** 新版本号。 */
    version: '',
    /** 更新说明（release notes）。 */
    notes: '',
    /** 下载进度（字节）。 */
    downloaded: 0,
    total: 0,
    /** 错误信息。 */
    errorMsg: '',
  }),
  getters: {
    /** 是否显示更新横幅。 */
    showBanner(state): boolean {
      return state.status === 'available' || state.status === 'downloading'
    },
    /** 下载进度百分比（0-100，total 未知时为 0）。 */
    progressPercent(state): number {
      if (state.total <= 0) return 0
      return Math.min(100, Math.round((state.downloaded / state.total) * 100))
    },
  },
  actions: {
    /** 启动时初始化：拉版本号 + 延迟静默检查（失败不打扰）。 */
    async init() {
      try {
        this.currentVersion = await getVersion()
      } catch (e) {
        console.error('[updater] getVersion failed:', e)
      }
      // 延迟 3s：不阻塞首屏，避开启动网络高峰
      setTimeout(() => {
        this.check(false)
      }, 3000)
    },

    /** 检查更新。manual=true 时无更新/失败给 UI 反馈（几秒后自动消失）。 */
    async check(manual: boolean) {
      if (this.status === 'checking' || this.status === 'downloading') return
      this.status = 'checking'
      this.errorMsg = ''
      try {
        const update = await check({ timeout: 15000 })
        if (update?.available) {
          pendingUpdate = update
          this.version = update.version
          this.notes = update.body ?? ''
          this.status = 'available'
        } else {
          pendingUpdate = null
          this.status = 'uptodate'
          if (manual) this.scheduleReset()
        }
      } catch (e) {
        console.error('[updater] check failed:', e)
        this.errorMsg = String(e)
        this.status = 'error'
        if (manual) this.scheduleReset()
      }
    },

    /** 手动检查的临时反馈（uptodate/error）自动回到 idle。 */
    scheduleReset() {
      setTimeout(() => {
        if (this.status === 'uptodate' || this.status === 'error') {
          this.status = 'idle'
        }
      }, FEEDBACK_RESET_MS)
    },

    /** 下载并安装更新（用户在横幅上点「立即更新」触发）。
     *
     * Windows：安装时应用退出，NSIS 安装器装完自动重启（带上原有参数），
     * relaunch() 是 macOS/Linux 的兜底。
     */
    async downloadAndInstall() {
      if (this.status !== 'available') return
      this.status = 'downloading'
      this.downloaded = 0
      this.total = 0
      try {
        // pendingUpdate 为空（不该发生）时兜底重新 check
        const update = pendingUpdate ?? (await check({ timeout: 15000 }))
        if (!update?.available) {
          this.status = 'uptodate'
          return
        }
        this.version = update.version
        await update.downloadAndInstall((event) => {
          switch (event.event) {
            case 'Started':
              this.total = event.data.contentLength ?? 0
              break
            case 'Progress':
              this.downloaded += event.data.chunkLength
              break
            case 'Finished':
              break
          }
        })
        // Windows 走不到这里（安装器已接管并退出进程）；其他平台手动重启
        await relaunch()
      } catch (e) {
        console.error('[updater] downloadAndInstall failed:', e)
        this.errorMsg = String(e)
        this.status = 'error'
      }
    },

    /** 忽略本次更新提示（横幅关闭按钮；下次启动会再提示）。 */
    dismiss() {
      if (this.status === 'available') {
        this.status = 'idle'
        pendingUpdate = null
      }
    },
  },
})
