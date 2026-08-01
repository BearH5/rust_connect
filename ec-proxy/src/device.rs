//! L3ConnDevice：把同步阻塞的 L3Conn 包成 smoltcp 的 phy::Device。
//!
//! 独立 OS 线程跑 L3Conn 读写循环；Device 内部用互斥队列缓冲入站包、
//! 用 std::sync::mpsc 发出站包给桥接线程。

use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

/// 入站队列：桥接线程推入，Device::receive 取出（喂给 smoltcp）。
type IngressQueue = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// L3Conn 桥接线程的句柄。
pub struct BridgeHandle {
    /// 发出站包给桥接线程（栈→L3Conn）。
    #[allow(dead_code)]
    egress_tx: mpsc::Sender<Vec<u8>>,
}

/// smoltcp Device 实现：以 L3Conn 为 IP 包源/汇。
pub struct L3ConnDevice {
    ingress: IngressQueue,
    egress_tx: mpsc::Sender<Vec<u8>>,
    mtu: usize,
    /// 入站包到达时通知 poll 循环立即醒来（性能关键：不能等 sleep 周期）。
    wake: Arc<tokio::sync::Notify>,
}

impl L3ConnDevice {
    /// 启动桥接：把 L3Conn 拆成读写两半，各起一个 OS 线程。
    ///
    /// 读线程：阻塞在 recv 上，读到 IP 包推入 ingress 队列。
    /// 写线程：阻塞在 egress channel 上，取出站包写给 send_conn。
    /// 两线程独立，读阻塞不影响写（修复单线程版本 SYN 发不出的死锁）。
    pub fn spawn(l3conn: ec_protocol::L3Conn, mtu: usize) -> (Self, BridgeHandle) {
        let (read_half, write_half) = l3conn.split();

        let ingress: IngressQueue = Arc::new(Mutex::new(VecDeque::new()));
        let (egress_tx, egress_rx) = mpsc::channel::<Vec<u8>>();

        // 读线程：L3Conn recv → ingress
        let ingress_read = Arc::clone(&ingress);
        let wake = Arc::new(tokio::sync::Notify::new());
        let wake_read = Arc::clone(&wake);
        let mut read_half = read_half;
        thread::Builder::new()
            .name("l3conn-read".into())
            .spawn(move || {
                use std::io::Read;
                let mut buf = [0u8; 1400];
                loop {
                    match read_half.read(&mut buf) {
                        Ok(0) => {
                            log::info!("[bridge] recv EOF，读线程退出");
                            return;
                        }
                        Ok(n) => {
                            ingress_read.lock().unwrap().push_back(buf[..n].to_vec());
                            // 性能关键：立即唤醒 poll 循环，不等 sleep 周期
                            wake_read.notify_one();
                        }
                        Err(e) => {
                            log::error!("[bridge] recv 出错，读线程退出: {e}");
                            return;
                        }
                    }
                }
            })
            .expect("spawn l3conn-read");

        // 写线程：egress channel → L3Conn send
        let mut write_half = write_half;
        thread::Builder::new()
            .name("l3conn-write".into())
            .spawn(move || {
                use std::io::Write;
                while let Ok(pkt) = egress_rx.recv() {
                    if let Err(e) = write_half.write_all(&pkt) {
                        log::error!("[bridge] send 出错，写线程退出: {e}");
                        return;
                    }
                }
                eprintln!("[bridge] egress channel 关闭，写线程退出");
            })
            .expect("spawn l3conn-write");

        let device = L3ConnDevice {
            ingress: ingress.clone(),
            egress_tx: egress_tx.clone(),
            mtu,
            wake,
        };
        let handle = BridgeHandle { egress_tx };
        (device, handle)
    }

    /// 给 poll 循环用的「是否有入站包」查询（避免空转 poll）。
    pub fn has_ingress(&self) -> bool {
        !self.ingress.lock().unwrap().is_empty()
    }

    /// 返回 wake Notify 的引用（poll 循环 select 监听，入站包到达时立即醒来）。
    pub fn wake_notify(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.wake)
    }
}

impl Device for L3ConnDevice {
    type RxToken<'a> = L3RxToken;
    type TxToken<'a> = L3TxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let pkt = self.ingress.lock().unwrap().pop_front();
        pkt.map(|p| {
            (
                L3RxToken { packet: Some(p) },
                L3TxToken {
                    tx: &self.egress_tx,
                },
            )
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(L3TxToken {
            tx: &self.egress_tx,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip; // L3 模式，无 Ethernet 头
        caps.max_transmission_unit = self.mtu;
        caps
    }
}

/// 入站 token：持有一个 IP 包，consume 时交给 smoltcp 处理。
pub struct L3RxToken {
    packet: Option<Vec<u8>>,
}

impl RxToken for L3RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        let pkt = self.packet.take().expect("rx token consumed twice");
        f(&pkt)
    }
}

/// 出站 token：持有 egress 发送端，consume 时把包发给桥接线程。
pub struct L3TxToken<'a> {
    tx: &'a mpsc::Sender<Vec<u8>>,
}

impl<'a> TxToken for L3TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        let _ = self.tx.send(buf);
        result
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_receives_packets_from_ingress_queue() {
        let ingress: IngressQueue = Arc::new(Mutex::new(VecDeque::new()));
        ingress.lock().unwrap().push_back(vec![0x45; 20]);

        let (egress_tx, _egress_rx) = mpsc::channel();
        let mut device = L3ConnDevice {
            ingress,
            egress_tx,
            mtu: 1400,
        };
        let (rx, _tx) = device
            .receive(Instant::from_millis(0))
            .expect("应有入站包");
        let pkt = rx.consume(|b| b.to_vec());
        assert_eq!(pkt, vec![0x45; 20]);
        assert!(device.receive(Instant::from_millis(1)).is_none());
    }

    #[test]
    fn device_transmit_sends_to_egress() {
        let ingress: IngressQueue = Arc::new(Mutex::new(VecDeque::new()));
        let (egress_tx, egress_rx) = mpsc::channel();
        let mut device = L3ConnDevice {
            ingress,
            egress_tx,
            mtu: 1400,
        };
        let tx = device
            .transmit(Instant::from_millis(0))
            .expect("transmit 应返回 token");
        tx.consume(5, |buf| {
            buf.copy_from_slice(&[1, 2, 3, 4, 5]);
        });
        let sent = egress_rx.recv().expect("应收到出站包");
        assert_eq!(sent, vec![1, 2, 3, 4, 5]);
    }
}
