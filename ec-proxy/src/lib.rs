//! EasyConnect SOCKS5 代理出口（阶段 3）。
//!
//! 用 smoltcp 用户态 TCP/IP 栈把 L3Conn（IP 包流）转成 TCP 会话，
//! 外挂本地 SOCKS5 server，让应用通过 socks5 访问内网。

pub mod device;
pub mod netstack;
pub mod proxy;
pub mod socks5;
pub mod stream;
pub mod tun;
