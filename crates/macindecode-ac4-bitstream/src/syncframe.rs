//! AC-4 sync frame 层。
//!
//! 对应 `TS103190-1:v1.4.1:Annex G`（normative）。
//!
//! sync frame 是可选的封装层，用于自带边界的传输。ISO BMFF 中不使用它，
//! 帧边界由 sample table 给出（见 `Annex E`）。

use crate::reader::BitReader;
use core::fmt;

/// `sync_word` 取值，对应 `G.4.1`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncWord {
    /// `0xAC40`，不带 CRC。
    Plain,
    /// `0xAC41`，帧尾带 `crc_word`。
    WithCrc,
}

impl SyncWord {
    const PLAIN: u16 = 0xAC40;
    const WITH_CRC: u16 = 0xAC41;

    const fn from_u16(value: u16) -> Option<Self> {
        match value {
            Self::PLAIN => Some(Self::Plain),
            Self::WITH_CRC => Some(Self::WithCrc),
            _ => None,
        }
    }

    /// 该 sync word 对应的原始值。
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::Plain => Self::PLAIN,
            Self::WithCrc => Self::WITH_CRC,
        }
    }
}

/// sync frame 解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFrameError {
    /// 该偏移处的 16 比特不是合法 `sync_word`。
    InvalidSyncWord {
        /// 帧在输入中的字节偏移。
        offset: usize,
        /// 实际读到的值。
        value: u16,
    },
    /// 输入在帧结束前耗尽。
    Truncated {
        /// 帧在输入中的字节偏移。
        offset: usize,
        /// 完成解析所需的字节数。
        needed: usize,
        /// 该偏移之后实际可用的字节数。
        available: usize,
    },
    /// `frame_size` 为 0，无法构成 `raw_ac4_frame`。
    EmptyFrame {
        /// 帧在输入中的字节偏移。
        offset: usize,
    },
}

impl fmt::Display for SyncFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            SyncFrameError::InvalidSyncWord { offset, value } => {
                write!(f, "偏移 {offset} 处 sync_word 非法：0x{value:04X}")
            }
            SyncFrameError::Truncated {
                offset,
                needed,
                available,
            } => {
                write!(
                    f,
                    "偏移 {offset} 处需要 {needed} 字节，实际可用 {available} 字节"
                )
            }
            SyncFrameError::EmptyFrame { offset } => {
                write!(f, "偏移 {offset} 处 frame_size 为 0")
            }
        }
    }
}

impl core::error::Error for SyncFrameError {}

/// 一个已定界的 AC-4 sync frame。
///
/// `raw_frame` 借用输入切片，不发生拷贝。
#[derive(Debug, Clone)]
pub struct SyncFrame<'a> {
    /// 帧在输入中的字节偏移。
    pub offset: usize,
    /// 使用的 sync word。
    pub sync_word: SyncWord,
    /// `raw_ac4_frame()` 的字节数，对应 `G.4.3`。
    pub frame_size: u32,
    /// `raw_ac4_frame()` 的内容。
    pub raw_frame: &'a [u8],
    /// 帧尾的 `crc_word`，仅当 sync word 为 `0xAC41` 时存在。
    pub crc_word: Option<u16>,
    /// 含 sync word、frame_size 与可选 CRC 在内的整帧字节数。
    pub total_size: usize,
    /// 受 CRC 保护的字节范围在输入中的起止，见 `G.4.2`。
    protected: (usize, usize),
}

impl<'a> SyncFrame<'a> {
    /// 校验 `crc_word`，对应 `G.4.2`。
    ///
    /// 返回 `None` 表示该帧未携带 CRC。
    ///
    /// 规范要求：把受保护载荷连同传输的 `crc_word` 一起过一遍算法，结果应为
    /// `0x0000`。此处按该方式校验，而非比较两个独立算出的值。
    #[must_use]
    pub fn verify_crc(&self, source: &[u8]) -> Option<bool> {
        let crc = self.crc_word?;
        let (start, end) = self.protected;
        let protected = source.get(start..end)?;
        let mut register = crc16(protected, 0);
        register = crc16(&crc.to_be_bytes(), register);
        Some(register == 0)
    }
}

/// CRC-16，生成多项式 x^16 + x^15 + x^2 + 1（`0x8005`）。
///
/// 对应 `G.4.2`：初始状态 `0x0000`，输入与输出均不做反射，末尾不做异或。
#[must_use]
fn crc16(data: &[u8], initial: u16) -> u16 {
    const POLYNOMIAL: u16 = 0x8005;
    let mut register = initial;
    for &byte in data {
        register ^= u16::from(byte) << 8;
        for _ in 0..8 {
            let overflow = register & 0x8000 != 0;
            register <<= 1;
            if overflow {
                register ^= POLYNOMIAL;
            }
        }
    }
    register
}

/// 从字节流中依次取出 sync frame。
///
/// 迭代器不做重同步：一旦某处不是合法帧起点即返回错误并停止。裸流的
/// 重同步策略属于容器层决策，不在此处隐式执行。
#[derive(Debug, Clone)]
pub struct SyncFrameIter<'a> {
    data: &'a [u8],
    position: usize,
    finished: bool,
}

impl<'a> SyncFrameIter<'a> {
    /// 在给定字节流上创建迭代器。
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            finished: false,
        }
    }

    /// 下一帧的起始字节偏移。
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    fn truncated(&self, needed: usize) -> SyncFrameError {
        SyncFrameError::Truncated {
            offset: self.position,
            needed,
            available: self.data.len().saturating_sub(self.position),
        }
    }

    fn parse_at(&mut self) -> Result<SyncFrame<'a>, SyncFrameError> {
        let offset = self.position;
        let tail = self.data.get(offset..).ok_or_else(|| self.truncated(2))?;

        let mut reader = BitReader::new(tail);
        let sync_raw = reader.read_bits(16).map_err(|_| self.truncated(2))? as u16;
        let sync_word = SyncWord::from_u16(sync_raw).ok_or(SyncFrameError::InvalidSyncWord {
            offset,
            value: sync_raw,
        })?;

        // frame_size()：16 比特，转义值 0xFFFF 时改由后续 24 比特给出。
        // 转义值是替换而非累加，见 G.3.2。
        let protected_start = offset.saturating_add(2);
        let short = reader.read_bits(16).map_err(|_| self.truncated(4))? as u32;
        let frame_size = if short == 0xFFFF {
            reader.read_bits(24).map_err(|_| self.truncated(7))? as u32
        } else {
            short
        };
        if frame_size == 0 {
            return Err(SyncFrameError::EmptyFrame { offset });
        }

        let header_bits = reader.bit_position();
        debug_assert!(header_bits % 8 == 0, "sync frame 头部必须字节对齐");
        let header_len = (header_bits / 8) as usize;

        let payload_start = offset.saturating_add(header_len);
        let payload_end = payload_start.saturating_add(frame_size as usize);
        let raw_frame = self
            .data
            .get(payload_start..payload_end)
            .ok_or_else(|| self.truncated(header_len.saturating_add(frame_size as usize)))?;

        let (crc_word, total_size) = match sync_word {
            SyncWord::Plain => (None, payload_end.saturating_sub(offset)),
            SyncWord::WithCrc => {
                let crc_end = payload_end.saturating_add(2);
                let bytes = self
                    .data
                    .get(payload_end..crc_end)
                    .ok_or_else(|| self.truncated(crc_end.saturating_sub(offset)))?;
                let crc =
                    u16::from_be_bytes([*bytes.first().unwrap_or(&0), *bytes.get(1).unwrap_or(&0)]);
                (Some(crc), crc_end.saturating_sub(offset))
            }
        };

        self.position = offset.saturating_add(total_size);
        Ok(SyncFrame {
            offset,
            sync_word,
            frame_size,
            raw_frame,
            crc_word,
            total_size,
            protected: (protected_start, payload_end),
        })
    }
}

impl<'a> Iterator for SyncFrameIter<'a> {
    type Item = Result<SyncFrame<'a>, SyncFrameError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished || self.position >= self.data.len() {
            return None;
        }
        let result = self.parse_at();
        if result.is_err() {
            self.finished = true;
        }
        Some(result)
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "测试内构造固定长度的帧，索引与长度均为常量"
)]
mod tests {
    use super::*;

    /// 构造 `0xAC40` 帧：sync(2) + size(2) + payload。
    fn plain_frame(payload: &[u8]) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0] = 0xAC;
        out[1] = 0x40;
        let size = payload.len() as u16;
        out[2] = (size >> 8) as u8;
        out[3] = size as u8;
        for (index, &byte) in payload.iter().enumerate() {
            if let Some(slot) = out.get_mut(4 + index) {
                *slot = byte;
            }
        }
        out
    }

    #[test]
    fn parses_plain_frame() {
        let data = plain_frame(&[0x01, 0x02, 0x03, 0x04]);
        let mut iter = SyncFrameIter::new(&data);
        let frame = iter.next().unwrap().unwrap();
        assert_eq!(frame.sync_word, SyncWord::Plain);
        assert_eq!(frame.frame_size, 4);
        assert_eq!(frame.raw_frame, &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(frame.crc_word, None);
        assert_eq!(frame.total_size, 8);
        assert!(iter.next().is_none());
    }

    #[test]
    fn parses_consecutive_frames() {
        let mut data = [0u8; 16];
        data[..8].copy_from_slice(&plain_frame(&[1, 2, 3, 4]));
        data[8..].copy_from_slice(&plain_frame(&[5, 6, 7, 8]));
        let frames: [_; 2] = core::array::from_fn(|_| ());
        let mut iter = SyncFrameIter::new(&data);
        let _ = frames;
        let first = iter.next().unwrap().unwrap();
        let second = iter.next().unwrap().unwrap();
        assert_eq!(first.offset, 0);
        assert_eq!(second.offset, 8);
        assert_eq!(second.raw_frame, &[5, 6, 7, 8]);
        assert!(iter.next().is_none());
    }

    /// frame_size 的 0xFFFF 是转义值，真实长度由随后的 24 比特替换给出。
    #[test]
    fn parses_extended_frame_size() {
        let mut data = [0u8; 7 + 5];
        data[0] = 0xAC;
        data[1] = 0x40;
        data[2] = 0xFF;
        data[3] = 0xFF;
        data[4] = 0x00;
        data[5] = 0x00;
        data[6] = 0x05;
        data[7..].copy_from_slice(&[9, 8, 7, 6, 5]);
        let mut iter = SyncFrameIter::new(&data);
        let frame = iter.next().unwrap().unwrap();
        assert_eq!(frame.frame_size, 5, "转义值应被替换而非累加");
        assert_eq!(frame.raw_frame, &[9, 8, 7, 6, 5]);
        assert_eq!(frame.total_size, 12);
    }

    #[test]
    fn rejects_invalid_sync_word() {
        let data = [0x00, 0x01, 0x00, 0x02];
        let mut iter = SyncFrameIter::new(&data);
        assert_eq!(
            iter.next().unwrap().unwrap_err(),
            SyncFrameError::InvalidSyncWord {
                offset: 0,
                value: 0x0001
            }
        );
        assert!(iter.next().is_none(), "出错后不得继续产出");
    }

    #[test]
    fn reports_truncated_payload() {
        // 声明 8 字节载荷但只给了 2 字节
        let data = [0xAC, 0x40, 0x00, 0x08, 0x01, 0x02];
        let mut iter = SyncFrameIter::new(&data);
        assert!(matches!(
            iter.next().unwrap().unwrap_err(),
            SyncFrameError::Truncated { offset: 0, .. }
        ));
    }

    #[test]
    fn rejects_zero_frame_size() {
        let data = [0xAC, 0x40, 0x00, 0x00];
        let mut iter = SyncFrameIter::new(&data);
        assert_eq!(
            iter.next().unwrap().unwrap_err(),
            SyncFrameError::EmptyFrame { offset: 0 }
        );
    }

    /// CRC-16/0x8005，初值 0，无反射、无末异或。
    /// 该向量取自广泛使用的校验串 "123456789"，用于确认参数组合正确。
    #[test]
    fn crc16_matches_known_vector() {
        assert_eq!(crc16(b"123456789", 0), 0xFEE8);
    }

    #[test]
    fn verifies_crc_round_trip() {
        let payload = [0x11u8, 0x22, 0x33];
        let mut data = [0u8; 4 + 3 + 2];
        data[0] = 0xAC;
        data[1] = 0x41;
        data[2] = 0x00;
        data[3] = 0x03;
        data[4..7].copy_from_slice(&payload);
        // 受保护范围是 frame_size 字段加载荷，不含 sync_word
        let crc = crc16(&data[2..7], 0);
        data[7..].copy_from_slice(&crc.to_be_bytes());

        let mut iter = SyncFrameIter::new(&data);
        let frame = iter.next().unwrap().unwrap();
        assert_eq!(frame.sync_word, SyncWord::WithCrc);
        assert_eq!(frame.crc_word, Some(crc));
        assert_eq!(frame.verify_crc(&data), Some(true));
    }

    #[test]
    fn detects_corrupted_payload() {
        let payload = [0x11u8, 0x22, 0x33];
        let mut data = [0u8; 9];
        data[0] = 0xAC;
        data[1] = 0x41;
        data[3] = 0x03;
        data[4..7].copy_from_slice(&payload);
        let crc = crc16(&data[2..7], 0);
        data[7..].copy_from_slice(&crc.to_be_bytes());
        data[5] ^= 0xFF; // 翻转载荷中的一个字节

        let mut iter = SyncFrameIter::new(&data);
        let frame = iter.next().unwrap().unwrap();
        assert_eq!(frame.verify_crc(&data), Some(false));
    }

    #[test]
    fn plain_frame_has_no_crc_to_verify() {
        let data = plain_frame(&[1, 2, 3, 4]);
        let mut iter = SyncFrameIter::new(&data);
        let frame = iter.next().unwrap().unwrap();
        assert_eq!(frame.verify_crc(&data), None);
    }
}
