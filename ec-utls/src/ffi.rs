//! utls-bridge 的 C ABI 声明。
//! 函数实现在 utls-bridge.dll（build.rs 链接）。

// 建立连接，返回句柄（>0 成功，<=0 失败）
extern "C" {
    pub fn ec_handshake(server: *const std::os::raw::c_char, mode: i32) -> i64;
}

// 读 session_id，返回写入字节数（<0 失败）
extern "C" {
    pub fn ec_conn_session_id(
        handle: i64,
        buf: *mut std::os::raw::c_char,
        buf_len: i64,
    ) -> i64;
}

// 读字节，返回读取数（<0 失败，0 EOF）
extern "C" {
    pub fn ec_conn_read(
        handle: i64,
        buf: *mut std::os::raw::c_char,
        buf_len: i64,
    ) -> i64;
}

// 写字节，返回写入数（<0 失败）
extern "C" {
    pub fn ec_conn_write(
        handle: i64,
        buf: *const std::os::raw::c_char,
        buf_len: i64,
    ) -> i64;
}

// 关闭连接
extern "C" {
    pub fn ec_conn_close(handle: i64);
}

// 读取最近一次 ec_handshake 的错误信息（NULL 表示无错误）。
// 指针由库管理，调用方不应 free。
extern "C" {
    pub fn ec_last_error() -> *const std::os::raw::c_char;
}
