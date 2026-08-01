pub mod ffi;

use std::ffi::CString;
use std::io::{self, Read, Write};

/// TLS 连接模式。对照 utls-bridge 的 mode 参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// 普通 TLS（HelloGolang），用于 requestToken 拿 session_id。
    Normal = 0,
    /// 特殊 TLS（L3IP/RC4/伪扩展），用于隧道握手。
    Special = 1,
}

/// 错误类型。
#[derive(Debug)]
pub enum UtlsError {
    /// 建立连接失败。
    HandshakeFailed,
    /// 建立连接失败，附带 DLL 返回的详细原因。
    HandshakeFailedWith(String),
    /// 句柄无效（连接已关闭或不存在）。
    InvalidHandle,
    /// session_id 缓冲区过小。
    BufferTooSmall,
    /// I/O 错误。
    Io(io::Error),
}

impl std::fmt::Display for UtlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UtlsError::HandshakeFailed => write!(f, "TLS 握手失败"),
            UtlsError::HandshakeFailedWith(d) => write!(f, "TLS 握手失败: {d}"),
            UtlsError::InvalidHandle => write!(f, "无效连接句柄"),
            UtlsError::BufferTooSmall => write!(f, "缓冲区过小"),
            UtlsError::Io(e) => write!(f, "I/O 错误: {e}"),
        }
    }
}

impl std::error::Error for UtlsError {}

impl From<io::Error> for UtlsError {
    fn from(e: io::Error) -> Self {
        UtlsError::Io(e)
    }
}

/// utls TLS 连接。实现 Read/Write，对上层呈现为字节流。
pub struct UtlsConn {
    handle: i64,
}

impl UtlsConn {
    /// 建立连接。
    /// server 形如 "rvpn.zju.edu.cn:443"，无协议前缀。
    pub fn connect(server: &str, mode: TlsMode) -> Result<Self, UtlsError> {
        let c_server = CString::new(server).map_err(|_| UtlsError::HandshakeFailed)?;
        let handle = unsafe { ffi::ec_handshake(c_server.as_ptr(), mode as i32) };
        if handle <= 0 {
            // 读取 DLL 内部保存的最近一次错误，便于诊断。
            let detail = unsafe {
                let ptr = ffi::ec_last_error();
                if ptr.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
                }
            };
            return Err(UtlsError::HandshakeFailedWith(detail));
        }
        Ok(UtlsConn { handle })
    }

    /// 读取 ServerHello 的 session_id（token 构造用）。
    /// 必须在 connect 之后、read/write 之前调用。
    pub fn session_id(&self) -> Result<Vec<u8>, UtlsError> {
        let mut buf = [0u8; 64];
        let n = unsafe {
            ffi::ec_conn_session_id(
                self.handle,
                buf.as_mut_ptr() as *mut std::os::raw::c_char,
                buf.len() as i64,
            )
        };
        if n < 0 {
            return Err(UtlsError::InvalidHandle);
        }
        Ok(buf[..n as usize].to_vec())
    }
}

impl Read for UtlsConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe {
            ffi::ec_conn_read(
                self.handle,
                buf.as_mut_ptr() as *mut std::os::raw::c_char,
                buf.len() as i64,
            )
        };
        if n < 0 {
            return Err(io::Error::new(io::ErrorKind::Other, "ec_conn_read 失败"));
        }
        Ok(n as usize)
    }
}

impl Write for UtlsConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe {
            ffi::ec_conn_write(
                self.handle,
                buf.as_ptr() as *const std::os::raw::c_char,
                buf.len() as i64,
            )
        };
        if n < 0 {
            return Err(io::Error::new(io::ErrorKind::Other, "ec_conn_write 失败"));
        }
        Ok(n as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for UtlsConn {
    fn drop(&mut self) {
        unsafe { ffi::ec_conn_close(self.handle) }
    }
}
