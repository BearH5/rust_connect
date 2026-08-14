//! 隐藏控制台：让所有子进程继承一个隐藏的控制台，避免弹黑窗。
//!
//! 根因：wintun-bindings 库内部用 `std::process::Command::new("netsh")` 设置网卡
//! IP/DNS，且不带 `CREATE_NO_WINDOW`。netsh 作为控制台程序会派生 conhost.exe
//! 可见窗口。我们无法修改第三方库，但可以让 ec-app 分配一个隐藏的控制台，
//! 子进程（netsh/net/sc 等）继承该隐藏控制台，不再创建新窗口。
//!
//! 这是 Windows GUI 程序需要 spawn 控制台子进程时的经典做法。

#[cfg(windows)]
mod ffi {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn AllocConsole() -> i32;
        pub fn GetConsoleWindow() -> *mut std::ffi::c_void;
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn ShowWindow(hwnd: *mut std::ffi::c_void, n_cmd_show: i32) -> i32;
    }

    pub const SW_HIDE: i32 = 0;
}

/// 在进程启动早期调用：分配控制台并立即隐藏。
///
/// 幂等：重复调用无副作用。仅 Windows 生效，其他平台空操作。
///
/// 注意 debug 构建是控制台子系统（main.rs 的 windows_subsystem 属性只对
/// release 启用），进程自带控制台，AllocConsole 会返回 0——因此不能以
/// AllocConsole 的返回值决定是否隐藏，否则 debug 下黑窗不会被隐藏。
/// 正确做法：无论 AllocConsole 结果，只要 GetConsoleWindow 拿到句柄就隐藏。
#[cfg(windows)]
pub fn hide_console() {
    unsafe {
        // release（GUI 子系统）：无控制台，AllocConsole 分配一个供子进程继承。
        // debug（控制台子系统）：已有控制台，AllocConsole 返回 0，无妨。
        let _ = ffi::AllocConsole();
        let hwnd = ffi::GetConsoleWindow();
        if !hwnd.is_null() {
            ffi::ShowWindow(hwnd, ffi::SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
pub fn hide_console() {}
