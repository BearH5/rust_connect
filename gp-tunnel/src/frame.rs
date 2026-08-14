//! GPST 16 字节帧头封/解。
//!
//! 严格对照 openconnect gpst.c 的字节级实现（行号见注释）。
//!
//! 帧头布局（offset 0-7 大端，offset 8-15 小端）：
//! ```text
//! 0000: Magic "\x1a\x2b\x3c\x4d"        (4B, big-endian)
//! 0004: Big-endian EtherType            (2B: 0x0800=IPv4, 0x86DD=IPv6, 0x0000=DPD)
//! 0006: Big-endian 16-bit length        (2B, 不含 16 字节头)
//! 0008: Little-endian u32 = 1           (数据帧) / = 0 (DPD 帧)
//! 000C: Little-endian u32 = 0           (固定)
//! 0010: data payload
//! ```

use crate::error::GpTunnelError;

/// 帧头魔数（gpst.c 第 62 行）。
pub const MAGIC: [u8; 4] = [0x1a, 0x2b, 0x3c, 0x4d];

/// 帧头长度。
pub const HEADER_LEN: usize = 16;

/// EtherType 取值（gpst.c 第 1200-1245 行）。
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_IPV6: u16 = 0x86DD;
pub const ETHERTYPE_DPD: u16 = 0x0000;

/// START_TUNNEL 握手成功标志（恰好 12 字节，无 NUL，gpst.c 第 723 行）。
pub const START_TUNNEL: &[u8] = b"START_TUNNEL";

/// 解析后的帧头信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    /// EtherType：0x0800=IPv4, 0x86DD=IPv6, 0x0000=DPD
    pub ethertype: u16,
    /// payload 长度（不含 16 字节头）
    pub payload_len: usize,
    /// 是否为 DPD/keepalive 帧
    pub is_dpd: bool,
}

/// 封装一个 IP 包为 GPST 数据帧（返回 16 字节头 + payload）。
///
/// 对照 gpst.c 第 1359-1371 行发送侧：
///   - EtherType 由 IP 包首字节高 4 位判断（0x60 -> IPv6，否则 IPv4）
///   - magic/ethertype/length 大端，one/zero 小端
pub fn encode_data_frame(ip_packet: &[u8]) -> Vec<u8> {
    let ethertype = if !ip_packet.is_empty() && (ip_packet[0] & 0xF0) == 0x60 {
        ETHERTYPE_IPV6
    } else {
        ETHERTYPE_IPV4
    };
    let mut frame = Vec::with_capacity(HEADER_LEN + ip_packet.len());
    // offset 0-3: magic（大端，固定字节序列）
    frame.extend_from_slice(&MAGIC);
    // offset 4-5: ethertype（大端）
    frame.extend_from_slice(&ethertype.to_be_bytes());
    // offset 6-7: payload length（大端，不含头）
    frame.extend_from_slice(&(ip_packet.len() as u16).to_be_bytes());
    // offset 8-11: one（小端 = 1）
    frame.extend_from_slice(&1u32.to_le_bytes());
    // offset 12-15: zero（小端 = 0）
    frame.extend_from_slice(&0u32.to_le_bytes());
    // payload
    frame.extend_from_slice(ip_packet);
    frame
}

/// 封装 DPD/keepalive 帧（固定 16 字节，无 payload）。
///
/// 对照 gpst.c 第 74-77 行 dpd_pkt + 第 1346-1350 行 KA_DPD 分支。
/// 完整字节：`1a 2b 3c 4d 00 00 00 00 00 00 00 00 00 00 00 00`
pub fn encode_dpd_frame() -> [u8; HEADER_LEN] {
    let mut hdr = [0u8; HEADER_LEN];
    // offset 0-3: magic
    hdr[..4].copy_from_slice(&MAGIC);
    // offset 4-7: ethertype=0(大端) + len=0(大端)，已是 0
    // offset 8-15: one=0(小端) + zero=0(小端)，已是 0
    hdr
}

/// 解析帧头（16 字节）。
///
/// 对照 gpst.c 第 1200-1239 行接收侧校验逻辑。
pub fn parse_frame_header(hdr: &[u8]) -> Result<FrameHeader, GpTunnelError> {
    if hdr.len() < HEADER_LEN {
        return Err(GpTunnelError::Frame(format!(
            "帧头过短: {} 字节（需 {}）",
            hdr.len(),
            HEADER_LEN
        )));
    }
    let hdr: &[u8; HEADER_LEN] = hdr[..HEADER_LEN].try_into().unwrap();

    // offset 0-3: magic（大端）
    let magic = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    if magic != 0x1a2b3c4d {
        return Err(GpTunnelError::Frame(format!(
            "magic 不匹配: 0x{magic:08x}（期望 0x1a2b3c4d）"
        )));
    }

    // offset 4-5: ethertype（大端）
    let ethertype = u16::from_be_bytes([hdr[4], hdr[5]]);
    // offset 6-7: payload_len（大端）
    let payload_len = u16::from_be_bytes([hdr[6], hdr[7]]) as usize;
    // offset 8-11: one（小端）
    let one = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    // offset 12-15: zero（小端）
    let zero = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]);

    match ethertype {
        ETHERTYPE_DPD => {
            // DPD 帧：期望 one==0 && zero==0（gpst.c 第 1224 行，不匹配仅警告不丢包）
            if one != 0 || zero != 0 {
                log::debug!(
                    "[gpst] DPD 帧 one/zero 非零: one=0x{one:08x} zero=0x{zero:08x}"
                );
            }
            Ok(FrameHeader {
                ethertype,
                payload_len,
                is_dpd: true,
            })
        }
        ETHERTYPE_IPV4 | ETHERTYPE_IPV6 => {
            // 数据帧：期望 one==1 && zero==0（gpst.c 第 1237 行）
            if one != 1 || zero != 0 {
                log::debug!(
                    "[gpst] 数据帧 one/zero 异常: one=0x{one:08x} zero=0x{zero:08x}"
                );
            }
            Ok(FrameHeader {
                ethertype,
                payload_len,
                is_dpd: false,
            })
        }
        _ => Err(GpTunnelError::Frame(format!(
            "未知 EtherType: 0x{ethertype:04x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证数据帧字节序列精确匹配 openconnect 发送侧（gpst.c 第 1360-1367 行）。
    #[test]
    fn test_encode_data_frame_ipv4() {
        // 一个最小 IPv4 包（20 字节，全 0x45 开头表示 IPv4）
        let ip_packet = [0x45u8; 20];
        let frame = encode_data_frame(&ip_packet);

        assert_eq!(frame.len(), HEADER_LEN + 20);
        // offset 0-3: magic
        assert_eq!(&frame[0..4], &[0x1a, 0x2b, 0x3c, 0x4d]);
        // offset 4-5: ethertype IPv4 大端
        assert_eq!(&frame[4..6], &[0x08, 0x00]);
        // offset 6-7: length=20 大端
        assert_eq!(&frame[6..8], &[0x00, 0x14]);
        // offset 8-11: one=1 小端
        assert_eq!(&frame[8..12], &[0x01, 0x00, 0x00, 0x00]);
        // offset 12-15: zero=0 小端
        assert_eq!(&frame[12..16], &[0x00, 0x00, 0x00, 0x00]);
        // payload
        assert_eq!(&frame[16..], &[0x45; 20]);
    }

    /// 验证 IPv6 帧（首字节 0x6x）。
    #[test]
    fn test_encode_data_frame_ipv6() {
        let ip_packet = [0x60u8; 40]; // IPv6 包首字节 0x60
        let frame = encode_data_frame(&ip_packet);

        assert_eq!(&frame[4..6], &[0x86, 0xDD]); // IPv6 ethertype 大端
        assert_eq!(&frame[6..8], &(40u16).to_be_bytes());
    }

    /// 验证 DPD 帧完整 16 字节。
    #[test]
    fn test_encode_dpd_frame() {
        let dpd = encode_dpd_frame();
        assert_eq!(dpd.len(), HEADER_LEN);
        // 完整字节序列
        assert_eq!(
            dpd,
            [
                0x1a, 0x2b, 0x3c, 0x4d, // magic
                0x00, 0x00, // ethertype=0
                0x00, 0x00, // len=0
                0x00, 0x00, 0x00, 0x00, // one=0
                0x00, 0x00, 0x00, 0x00, // zero=0
            ]
        );
    }

    /// 验证解析 IPv4 数据帧。
    #[test]
    fn test_parse_data_frame_ipv4() {
        let ip_packet = [0x45u8; 20];
        let frame = encode_data_frame(&ip_packet);
        let hdr = parse_frame_header(&frame).unwrap();

        assert_eq!(hdr.ethertype, ETHERTYPE_IPV4);
        assert_eq!(hdr.payload_len, 20);
        assert!(!hdr.is_dpd);
    }

    /// 验证解析 DPD 帧。
    #[test]
    fn test_parse_dpd_frame() {
        let dpd = encode_dpd_frame();
        let hdr = parse_frame_header(&dpd).unwrap();

        assert_eq!(hdr.ethertype, ETHERTYPE_DPD);
        assert_eq!(hdr.payload_len, 0);
        assert!(hdr.is_dpd);
    }

    /// 验证 magic 不匹配报错。
    #[test]
    fn test_parse_bad_magic() {
        let mut bad = encode_dpd_frame();
        bad[0] = 0x00; // 破坏 magic
        assert!(parse_frame_header(&bad).is_err());
    }

    /// 验证未知 EtherType 报错。
    #[test]
    fn test_parse_unknown_ethertype() {
        let mut bad = encode_dpd_frame();
        bad[4] = 0xFF; // 未知 ethertype
        bad[5] = 0xFF;
        assert!(parse_frame_header(&bad).is_err());
    }

    /// 验证帧头过短报错。
    #[test]
    fn test_parse_short_header() {
        let short = [0u8; 8];
        assert!(parse_frame_header(&short).is_err());
    }
}
