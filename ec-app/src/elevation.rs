//! UAC 提权的跨进程握手：Windows 命名事件（CreateEventW/OpenEventW/SetEvent）。
//!
//! 原进程（非管理员）发起提权前创建命名事件，提权实例（管理员）启动后 SetEvent。
//! 原进程 WaitForSingleObject 等待信号：收到→退出，超时→保留并提示取消。
//!
//! 事件名格式：`Local\ec-app-elev-{uuid}`（扁平命名，见 event_name 注释）。
//! Local 前缀限定当前登录会话，UUID v4 保证每次唯一。
//! 内核对象在最后一个句柄关闭后自动回收，不残留。
//!
//! FFI 风格沿用 silent_console.rs 的手写 #[link] extern "system"。

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        /// 创建命名或匿名事件。返回句柄，NULL 表示失败。
        pub fn CreateEventW(
            lpeventattributes: *const c_void, // SECURITY_ATTRIBUTES，NULL=默认
            bmanualreset: i32,                // TRUE=手动复位，FALSE=自动复位
            binitialstate: i32,               // TRUE=初始有信号
            lpname: *const u16,               // 事件名（UTF-16），NULL=匿名
        ) -> *mut c_void;

        /// 打开已存在的命名事件。返回句柄，NULL 表示失败。
        pub fn OpenEventW(
            dwdesiredaccess: u32, // EVENT_MODIFY_STATE=0x0002 可 SetEvent
            binherithandle: i32,
            lpname: *const u16,
        ) -> *mut c_void;

        /// 设置事件为有信号状态。对 auto-reset 事件，唤醒一个等待者后自动复位。
        pub fn SetEvent(hevent: *mut c_void) -> i32;

        /// 等待对象有信号。返回 WAIT_OBJECT_0(0)/WAIT_TIMEOUT(0x102)/WAIT_FAILED。
        pub fn WaitForSingleObject(hevent: *mut c_void, dwmilliseconds: u32) -> u32;

        /// 关闭句柄。
        pub fn CloseHandle(hobject: *mut c_void) -> i32;

        /// 取最近一次 API 错误码（诊断用）。
        pub fn GetLastError() -> u32;
    }

    pub const EVENT_MODIFY_STATE: u32 = 0x0002;
    pub const WAIT_OBJECT_0: u32 = 0x0000;
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn event_name(uuid: &str) -> Vec<u16> {
    // 必须用扁平命名（'-' 连接，无中间 '\'）：BaseNamedObjects 命名空间把
    // '\' 当路径分隔符，父"目录"不存在时 CreateEventW 返回 NULL
    // （GetLastError=3 ERROR_PATH_NOT_FOUND，已实测）。
    to_wide(&format!("Local\\ec-app-elev-{}", uuid))
}

/// 原进程（A）侧：创建命名事件并返回句柄。
///
/// 必须在调用 PowerShell Start-Process **之前**创建，B 才能 OpenEvent 到。
/// manual_reset=false（auto-reset）：B SetEvent 后自动复位，适合一次性握手。
#[cfg(windows)]
pub fn create_handshake_event(uuid: &str) -> Result<*mut std::ffi::c_void, String> {
    unsafe {
        let name = event_name(uuid);
        let h = ffi::CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
        if h.is_null() {
            let code = ffi::GetLastError();
            Err(format!("CreateEventW 返回 NULL（GetLastError={code}）"))
        } else {
            Ok(h)
        }
    }
}

/// 原进程（A）侧：等待 B 发出信号。
///
/// 返回 true=B 已启动（A 可退出），false=超时或失败（A 应保留，提示提权被取消）。
/// 内部已 CloseHandle，调用方无需再关。
#[cfg(windows)]
pub fn wait_handshake(handle: *mut std::ffi::c_void, timeout_ms: u32) -> bool {
    unsafe {
        let r = ffi::WaitForSingleObject(handle, timeout_ms);
        ffi::CloseHandle(handle);
        r == ffi::WAIT_OBJECT_0
    }
}

/// 提权实例（B）侧：打开命名事件并 SetEvent，通知 A "我已启动"。
///
/// 失败不 panic：仅静默。A 有超时兜底。
#[cfg(windows)]
pub fn signal_handshake(uuid: &str) {
    unsafe {
        let name = event_name(uuid);
        let h = ffi::OpenEventW(ffi::EVENT_MODIFY_STATE, 0, name.as_ptr());
        if !h.is_null() {
            ffi::SetEvent(h);
            ffi::CloseHandle(h);
        }
    }
}

// 非 Windows 平台空实现，保证 cfg 隔离编译通过。
#[cfg(not(windows))]
#[allow(unused_variables)]
pub fn signal_handshake(uuid: &str) {}
