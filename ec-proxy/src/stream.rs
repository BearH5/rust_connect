//! NetStream：出站 TCP 连接的 async 读写封装。
//!
//! 与 poll 循环共享 TcpSocketControl（RingBuffer + Waker）。
//! 参考 netstack-smoltcp/src/tcp.rs 的 TcpStream。

use std::task::Waker;
use std::sync::Arc;

use smoltcp::iface::SocketHandle;
use smoltcp::storage::RingBuffer;
use spin::Mutex as SpinMutex;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TcpSocketState {
    Normal,
    Close,
    Closing,
    Closed,
}

/// per-socket 控制块：poll 循环与 NetStream 之间的数据通道。
pub struct TcpSocketControl {
    pub send_buffer: RingBuffer<'static, u8>,  // 应用 → socket
    pub send_waker: Option<Waker>,
    pub recv_buffer: RingBuffer<'static, u8>,  // socket → 应用
    pub recv_waker: Option<Waker>,
    pub recv_state: TcpSocketState,
    pub send_state: TcpSocketState,
}

pub type SharedControl = Arc<SpinMutex<TcpSocketControl>>;

/// 出站 TCP 连接流。实现 AsyncRead/AsyncWrite。
pub struct NetStream {
    #[allow(dead_code)]
    handle: SocketHandle,
    control: SharedControl,
    /// 写入 send_buffer 后通知 poll 循环立即处理（性能关键：不等 sleep 周期）。
    wake: Option<Arc<tokio::sync::Notify>>,
}

impl NetStream {
    pub(crate) fn new(handle: SocketHandle, control: SharedControl) -> Self {
        Self {
            handle,
            control,
            wake: None,
        }
    }

    /// 设置 wake Notify（由 NetStackHandle::dial_tcp 传入）。
    pub(crate) fn set_wake(&mut self, wake: Arc<tokio::sync::Notify>) {
        self.wake = Some(wake);
    }
}

impl Drop for NetStream {
    fn drop(&mut self) {
        let mut control = self.control.lock();
        if matches!(control.recv_state, TcpSocketState::Normal) {
            control.recv_state = TcpSocketState::Close;
        }
        if matches!(control.send_state, TcpSocketState::Normal) {
            control.send_state = TcpSocketState::Close;
        }
    }
}

impl AsyncRead for NetStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut control = self.control.lock();
        if control.recv_buffer.is_empty() {
            if matches!(control.recv_state, TcpSocketState::Closed) {
                return std::task::Poll::Ready(Ok(()));
            }
            if let Some(old) = control.recv_waker.replace(cx.waker().clone()) {
                if !old.will_wake(cx.waker()) {
                    old.wake();
                }
            }
            return std::task::Poll::Pending;
        }
        let n = control.recv_buffer.dequeue_slice(buf.initialize_unfilled());
        buf.advance(n);
        std::task::Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for NetStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut control = self.control.lock();
        if !matches!(control.send_state, TcpSocketState::Normal) {
            return std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }
        if control.send_buffer.is_full() {
            if let Some(old) = control.send_waker.replace(cx.waker().clone()) {
                if !old.will_wake(cx.waker()) {
                    old.wake();
                }
            }
            return std::task::Poll::Pending;
        }
        let n = control.send_buffer.enqueue_slice(buf);
        if n > 0 {
            // 性能关键：写入后立即唤醒 poll 循环处理，不等 sleep 周期
            if let Some(wake) = &self.wake {
                wake.notify_one();
            }
        }
        std::task::Poll::Ready(Ok(n))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let mut control = self.control.lock();
        if matches!(control.send_state, TcpSocketState::Closed) {
            return std::task::Poll::Ready(Ok(()));
        }
        if matches!(control.send_state, TcpSocketState::Normal) {
            control.send_state = TcpSocketState::Close;
        }
        if let Some(old) = control.send_waker.replace(cx.waker().clone()) {
            old.wake();
        }
        std::task::Poll::Pending
    }
}
