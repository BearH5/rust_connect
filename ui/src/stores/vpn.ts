import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

// ---- 后端类型镜像（见 ec-app/src/config.rs / state.rs）----

/** VPN 连接配置。字段名与后端 Profile 结构体一致。 */
export interface Profile {
  id: string
  name: string
  /** 形如 "rvpn.zju.edu.cn:443"，无协议前缀。 */
  server: string
  username: string
  password: string
  socks_port: number
  /** 协议类型："easyconnect"（默认，Sangfor EasyConnect）或 "globalprotect"（Palo Alto GP）。 */
  protocol: 'easyconnect' | 'globalprotect'
}

/** 全局设置。 */
export interface Settings {
  auto_reconnect: boolean
  auto_start: boolean
  minimize_to_tray: boolean
  /** 代理模式："pac"（系统代理）或 "tun"（TUN 全局代理）。 */
  proxy_mode: 'pac' | 'tun'
}

/** 统一的 VPN 状态（tagged union，state 用 PascalCase）。 */
export type VpnStatus =
  | { state: 'Disconnected' }
  | { state: 'Connecting' }
  | { state: 'Connected'; client_ip: string; socks_bind: string }
  | { state: 'Error'; message: string }

/** 日志条目（vpn:log 事件 payload）。 */
export interface LogEntry {
  level: string
  message: string
  timestamp: string
}

/** 资源条目（vpn:resources 事件 payload）。 */
export interface ResourceEntry {
  name: string
  host: string
  port: number
}

/** 进度事件 payload（vpn:progress）。 */
interface ProgressPayload {
  stage: string
  message: string
}

/**
 * 把后端事件里的 lowercase state 归一化成 PascalCase。
 *
 * 注意：get_status command 返回 PascalCase（Disconnected/Connecting/Connected/Error），
 * 但 vpn:status **事件** emit 的是 lowercase（disconnected/connecting/connected/error）。
 * 这里统一成 PascalCase，前端只处理一种形式。
 */
function normalizeState(raw: unknown): VpnStatus {
  const obj = (raw ?? {}) as Record<string, unknown>
  const lower = String(obj.state ?? '').toLowerCase()
  switch (lower) {
    case 'connecting':
      return { state: 'Connecting' }
    case 'connected':
      return {
        state: 'Connected',
        client_ip: String(obj.client_ip ?? ''),
        socks_bind: String(obj.socks_bind ?? ''),
      }
    case 'error':
      return { state: 'Error', message: String(obj.message ?? '未知错误') }
    case 'disconnected':
    default:
      return { state: 'Disconnected' }
  }
}

/** 是否处于可断开的活跃状态。 */
export function isActive(status: VpnStatus): boolean {
  return status.state === 'Connecting' || status.state === 'Connected'
}

export const useVpnStore = defineStore('vpn', {
  state: () => ({
    status: { state: 'Disconnected' } as VpnStatus,
    profiles: [] as Profile[],
    settings: { auto_reconnect: false, auto_start: false, minimize_to_tray: false, proxy_mode: 'pac' } as Settings,
    logs: [] as LogEntry[],
    /** 日志上限，超出后丢弃最早的。 */
    resources: [] as ResourceEntry[],
    ready: false as boolean,
    /** 当前选中的 profile id（连接页用）。 */
    selectedProfileId: '' as string,
    unlistenFns: [] as UnlistenFn[],
  }),
  getters: {
    selectedProfile(state): Profile | undefined {
      return state.profiles.find((p) => p.id === state.selectedProfileId)
    },
  },
  actions: {
    /** 初始化：加载 profiles/settings、监听事件、拉取初始状态。 */
    async init() {
      // 监听事件（先监听再拉状态，避免漏掉中间事件）
      this.unlistenFns.push(
        await listen('vpn:status', (e) => {
          this.status = normalizeState(e.payload)
        }),
      )
      this.unlistenFns.push(
        await listen('vpn:log', (e) => {
          this.logs.push(e.payload as LogEntry)
          this.trimLogs()
        }),
      )
      this.unlistenFns.push(
        await listen('vpn:progress', (e) => {
          const p = e.payload as ProgressPayload
          this.logs.push({
            level: 'info',
            message: p.message,
            timestamp: new Date().toISOString(),
          })
          this.trimLogs()
        }),
      )
      this.unlistenFns.push(
        await listen('vpn:resources', (e) => {
          this.resources = (e.payload as ResourceEntry[]) ?? []
        }),
      )

      // 拉取初始数据（命令可能失败，逐个 try 不互相阻塞）
      try {
        this.profiles = await invoke<Profile[]>('list_profiles')
        if (!this.selectedProfileId && this.profiles.length > 0) {
          this.selectedProfileId = this.profiles[0].id
        }
      } catch (e) {
        console.error('[vpn] list_profiles failed:', e)
      }
      try {
        this.settings = await invoke<Settings>('get_settings')
      } catch (e) {
        console.error('[vpn] get_settings failed:', e)
      }
      try {
        this.status = normalizeState(await invoke('get_status'))
      } catch (e) {
        console.error('[vpn] get_status failed:', e)
      }
      // 提权重启后自动连接：后端 get_pending_auto_connect 返回 profile_id（取后即空）。
      // 仅提权实例启动时非 None，普通启动返回 null 不触发。
      try {
        const pendingId = await invoke<string | null>('get_pending_auto_connect')
        if (pendingId) {
          // 确认 profile 仍存在（用户可能在提权窗口未弹出期间删了它）
          if (this.profiles.some((p) => p.id === pendingId)) {
            this.selectedProfileId = pendingId
            await this.connect(pendingId)
          }
        }
      } catch (e) {
        console.error('[vpn] get_pending_auto_connect failed:', e)
      }
      this.ready = true
    },

    /** 卸载所有事件监听（组件销毁时调用）。 */
    dispose() {
      for (const fn of this.unlistenFns) {
        try {
          fn()
        } catch {
          /* ignore */
        }
      }
      this.unlistenFns = []
    },

    /** 限制日志条数，避免内存膨胀。 */
    trimLogs() {
      const MAX = 1000
      if (this.logs.length > MAX) {
        this.logs.splice(0, this.logs.length - MAX)
      }
    },

    async connect(profileId: string) {
      // 找 profile 看协议类型：GP 走 connect（后端按 protocol 分派到 gp_mode），
      // EasyConnect 按 proxy_mode 分 connect_tun（管理员+wintun）/ connect（pac）。
      const profile = this.profiles.find((p) => p.id === profileId)
      const isGp = profile?.protocol === 'globalprotect'
      if (isGp) {
        // GP 协议：固定走 openconnect-tun，统一调 connect（后端内部分派）
        await invoke('connect', { profileId })
      } else if (this.settings.proxy_mode === 'tun') {
        // EasyConnect tun 模式：connect_tun（管理员+wintun）
        await invoke('connect_tun', { profileId })
      } else {
        // EasyConnect pac 模式
        await invoke('connect', { profileId })
      }
    },
    async disconnect() {
      await invoke('disconnect')
    },
    async refreshProfiles() {
      this.profiles = await invoke<Profile[]>('list_profiles')
    },
    async saveProfile(profile: Profile) {
      await invoke('save_profile', { profile })
      await this.refreshProfiles()
    },
    async deleteProfile(id: string) {
      await invoke('delete_profile', { id })
      await this.refreshProfiles()
    },
    async refreshSettings() {
      this.settings = await invoke<Settings>('get_settings')
    },
    async saveSettings(settings: Settings) {
      await invoke('save_settings', { settings })
      this.settings = settings
    },
  },
})
