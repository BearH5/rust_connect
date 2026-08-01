//! EasyConnect 隧道协议层（阶段 D-2）。
//!
//! 对照 zju-connect/client/easyconnect：
//! - `token` 模块 → request.go 的 requestToken（构造 48 字节 token）
//! - `tunnel` 模块 → request.go 的 requestIP + protocol.go 的 RecvConn/SendConn
//! - `l3conn` 模块 → l3conn.go 的 L3Conn（收发封装 + 出错重连）
//!
//! 整体流程：登录拿 twfID → request_token 拿 session_id hex → build_token 拼出 48 字节 token
//! → request_ip 拿客户端 IP → recv_conn/send_conn 建隧道 → L3Conn 收发。

pub mod error;
pub mod l3conn;
pub mod token;
pub mod tunnel;

pub use error::{ProtocolError, TunnelError};
pub use l3conn::{L3Conn, L3ConnLike, L3ReadHalf, L3WriteHalf};
