//! EMDF 信息元素与 opaque payload envelope。
//!
//! 对应 `TS103190-1:v1.4.1:4.2.3.5`（表 8）、`4.2.3.10`（表 13）、
//! `4.2.4.4`（表 18）、`4.2.14.14`（表 79）与 `4.2.14.15`（表 80）。
//!
//! 未注册或私有 payload ID 不在本层解释。解析器只验证有界 envelope，保留时序、
//! transcoding 路由字段和原始 8 比特 payload 元素；不会把 metadata 应用到 PCM。

use crate::reader::{BitReader, ReadError};
use crate::topology::{TopologyError, read_substream_index};
use core::fmt;

/// 一个 `emdf_payloads_substream()` 可保留的 payload 数量上限。
///
/// P1 的循环没有给出数量上限；本 crate 默认不分配内存，因此采用与
/// substream index table 相同的固定容量 32。
pub const MAX_EMDF_PAYLOADS: usize = 32;

/// 单个 opaque EMDF payload 的字节数上限。
///
/// 该上限等于 Annex G 扩展 `frame_size` 的 24 比特最大值。payload 不可能独占超过
/// 整个 sync frame；对无 sync wrapper 的调用也沿用同一资源边界。
pub const MAX_EMDF_PAYLOAD_BYTES: u32 = 0x00ff_ffff;

/// `emdf_payloads_substream()` 解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmdfError {
    /// 底层读取失败或变长字段溢出。
    Read(ReadError),
    /// 在有界 substream 结束前没有读到 ID 0 终止符。
    MissingTerminator {
        /// 本应开始读取下一个 5 比特 ID 的位置。
        bit_position: u64,
        /// 该位置之后实际剩余的比特数。
        remaining_bits: u64,
    },
    /// payload 数量超过固定容量。
    TooManyPayloads {
        /// 本实现可保留的 payload 数量。
        limit: usize,
        /// 超额 payload ID 的起始位置。
        bit_position: u64,
    },
    /// 单个 payload 声明的字节数超过实现上限。
    PayloadTooLarge {
        /// 码流声明的字节数。
        declared: u32,
        /// 本实现允许的最大字节数。
        limit: u32,
        /// `emdf_payload_size` 的起始位置。
        bit_position: u64,
    },
}

impl fmt::Display for EmdfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            EmdfError::Read(error) => write!(formatter, "Failed to read EMDF: {error}"),
            EmdfError::MissingTerminator {
                bit_position,
                remaining_bits,
            } => write!(
                formatter,
                "EMDF payload substream has no complete ID 0 terminator at bit offset {bit_position}; {remaining_bits} bits remain"
            ),
            EmdfError::TooManyPayloads {
                limit,
                bit_position,
            } => write!(
                formatter,
                "EMDF payload at bit offset {bit_position} exceeds the fixed capacity of {limit} payloads"
            ),
            EmdfError::PayloadTooLarge {
                declared,
                limit,
                bit_position,
            } => write!(
                formatter,
                "EMDF payload declares {declared} bytes at bit offset {bit_position}, exceeding the limit of {limit} bytes"
            ),
        }
    }
}

impl core::error::Error for EmdfError {}

impl From<ReadError> for EmdfError {
    fn from(error: ReadError) -> Self {
        Self::Read(error)
    }
}

/// `emdf_info()`，见 `TS103190-1:v1.4.1:4.2.3.5`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmdfInfo {
    /// EMDF 语法版本；本规范定义的负载中该值为 0。
    pub emdf_version: u32,
    /// 认证 ID；规范未定义其语义。
    pub key_id: u32,
    /// `emdf_payloads_substream_info()` 给出的 substream 下标。
    pub payloads_substream_index: Option<u32>,
    /// `emdf_reserved()` 跳过的字节数。
    pub reserved_bytes: u32,
}

impl EmdfInfo {
    /// 未解析状态的占位值，用于固定容量数组。
    pub const EMPTY: Self = Self {
        emdf_version: 0,
        key_id: 0,
        payloads_substream_index: None,
        reserved_bytes: 0,
    };

    /// 用于解码器配置代次比较的规范化副本。
    ///
    /// payload substream 可以只在携带负载的帧中出现；它的下标属于逐帧路由，
    /// 不改变音频、OAMD 或 presentation 的跨帧解码配置。保留版本与 key ID，
    /// 但清除该下标与纯填充长度。
    pub(crate) const fn configuration_copy(&self) -> Self {
        Self {
            payloads_substream_index: None,
            reserved_bytes: 0,
            ..*self
        }
    }

    /// 解析 `emdf_info()`。
    ///
    /// # Errors
    ///
    /// 读取越界或变长字段溢出时返回错误。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, TopologyError> {
        let mut emdf_version = u32::try_from(reader.read_bits(2)?).unwrap_or(u32::MAX);
        if emdf_version == 3 {
            emdf_version = reader.variable_bits_scaled_u32(2, emdf_version, 0)?;
        }

        let mut key_id = u32::try_from(reader.read_bits(3)?).unwrap_or(u32::MAX);
        if key_id == 7 {
            key_id = reader.variable_bits_scaled_u32(3, key_id, 0)?;
        }

        let payloads_substream_index = if reader.read_flag()? {
            Some(read_substream_index(reader)?)
        } else {
            None
        };

        let reserved_bytes = skip_emdf_reserved(reader)?;

        Ok(Self {
            emdf_version,
            key_id,
            payloads_substream_index,
            reserved_bytes,
        })
    }
}

/// `emdf_payload_config()` 的原始时序与 transcoding 路由字段。
///
/// Optional 字段精确保留 presence。gated Boolean 在未传输时保持 `false`；其 presence 可由
/// `discard_unknown_payload`、`sample_offset` 和 `payload_frame_aligned` 唯一反推。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmdfPayloadConfig {
    /// payload 首次适用的帧内 PCM sample offset；缺席表示帧首 sample。
    pub sample_offset: Option<u32>,
    /// payload 适用的 PCM sample 数；缺席表示持续到帧尾。
    pub duration: Option<u32>,
    /// 关联 payload 的 group ID。
    pub group_id: Option<u32>,
    /// 私有 codec-specific 8 比特码值。
    pub codec_data: Option<u8>,
    /// 未识别 ID 时是否应在 transcoding 中丢弃该 payload。
    pub discard_unknown_payload: bool,
    /// payload 是否必须与本 AC-4 音频帧对齐。
    pub payload_frame_aligned: bool,
    /// 输出帧更短时是否为当前时间段创建重复 payload。
    pub create_duplicate: bool,
    /// 输出帧更长时是否移除同 ID 的后续重复 payload。
    pub remove_duplicate: bool,
    /// 5 比特 payload priority 原值；gate 不成立时为 `None`。
    pub priority: Option<u8>,
    /// 2 比特 `proc_allowed` 原值；gate 不成立时为 `None`。
    pub processing_allowed: Option<u8>,
}

impl EmdfPayloadConfig {
    /// 解析 P1 `4.2.14.14` 的 `emdf_payload_config()`。
    ///
    /// # Errors
    ///
    /// 配置截断或任一变长字段超出 `u32` 时返回 [`ReadError`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, ReadError> {
        let sample_offset = if reader.read_flag()? {
            Some(reader.variable_bits_u32(11)?)
        } else {
            None
        };
        let duration = if reader.read_flag()? {
            Some(reader.variable_bits_u32(11)?)
        } else {
            None
        };
        let group_id = if reader.read_flag()? {
            Some(reader.variable_bits_u32(2)?)
        } else {
            None
        };
        let codec_data = if reader.read_flag()? {
            Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX))
        } else {
            None
        };

        let discard_unknown_payload = reader.read_flag()?;
        let mut out = Self {
            sample_offset,
            duration,
            group_id,
            codec_data,
            discard_unknown_payload,
            ..Self::default()
        };
        if discard_unknown_payload {
            return Ok(out);
        }

        if sample_offset.is_none() {
            out.payload_frame_aligned = reader.read_flag()?;
            if out.payload_frame_aligned {
                out.create_duplicate = reader.read_flag()?;
                out.remove_duplicate = reader.read_flag()?;
            }
        }
        if sample_offset.is_some() || out.payload_frame_aligned {
            out.priority = Some(u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX));
            out.processing_allowed = Some(u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX));
        }
        Ok(out)
    }
}

/// 一个 opaque EMDF payload 的 ID、配置、长度与原 substream 位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmdfPayload {
    /// 非零 `emdf_payload_id`；未知或私有 ID 同样原样保留。
    pub id: u32,
    /// payload 的时序与 transcoding 路由配置。
    pub config: EmdfPayloadConfig,
    /// 后续原始 8 比特元素的数量。
    pub size_bytes: u32,
    payload_bit_offset: u64,
}

impl EmdfPayload {
    const EMPTY: Self = Self {
        id: 0,
        config: EmdfPayloadConfig {
            sample_offset: None,
            duration: None,
            group_id: None,
            codec_data: None,
            discard_unknown_payload: false,
            payload_frame_aligned: false,
            create_duplicate: false,
            remove_duplicate: false,
            priority: None,
            processing_allowed: None,
        },
        size_bytes: 0,
        payload_bit_offset: 0,
    };

    /// payload bytes 相对解析时原始输入的起始比特偏移。
    #[must_use]
    pub const fn bit_offset(self) -> u64 {
        self.payload_bit_offset
    }

    /// 从解析时使用的同一原始输入重建无损 8 比特元素视图。
    ///
    /// Payload 起点不保证按字节对齐，因此调用方不能一律取得 `&[u8]`；
    /// [`EmdfPayloadBytes::iter`] 在两种对齐形态下都返回原码值。
    #[must_use]
    pub fn bytes<'a>(self, source: &'a [u8]) -> Option<EmdfPayloadBytes<'a>> {
        EmdfPayloadBytes::new(source, self.payload_bit_offset, self.size_bytes)
    }
}

/// 可能不在字节边界上的 opaque EMDF payload 视图。
#[derive(Debug, Clone, Copy)]
pub struct EmdfPayloadBytes<'a> {
    source: &'a [u8],
    bit_offset: u64,
    len: u32,
}

impl<'a, 'b> PartialEq<EmdfPayloadBytes<'b>> for EmdfPayloadBytes<'a> {
    fn eq(&self, other: &EmdfPayloadBytes<'b>) -> bool {
        self.len == other.len && (*self).iter().eq((*other).iter())
    }
}

impl Eq for EmdfPayloadBytes<'_> {}

impl<'a> EmdfPayloadBytes<'a> {
    fn new(source: &'a [u8], bit_offset: u64, len: u32) -> Option<Self> {
        let bit_len = u64::from(len).checked_mul(8)?;
        let end = bit_offset.checked_add(bit_len)?;
        if end > (source.len() as u64).saturating_mul(8) {
            return None;
        }
        Some(Self {
            source,
            bit_offset,
            len,
        })
    }

    /// 原始 8 比特元素数量。
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// 视图是否为空。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// 读取一个原始 8 比特元素。
    #[must_use]
    pub fn get(self, index: u32) -> Option<u8> {
        if index >= self.len {
            return None;
        }
        let relative = u64::from(index).checked_mul(8)?;
        let mut reader = BitReader::new(self.source);
        reader
            .skip_bits(self.bit_offset.checked_add(relative)?)
            .ok()?;
        u8::try_from(reader.read_bits(8).ok()?).ok()
    }

    /// 起点位于字节边界时返回零拷贝切片。
    #[must_use]
    pub fn as_aligned_slice(self) -> Option<&'a [u8]> {
        if !self.bit_offset.is_multiple_of(8) {
            return None;
        }
        let start = usize::try_from(self.bit_offset / 8).ok()?;
        let end = start.checked_add(self.len())?;
        self.source.get(start..end)
    }

    /// 按码流顺序遍历原始 8 比特元素。
    #[must_use]
    pub fn iter(self) -> EmdfPayloadByteIter<'a> {
        EmdfPayloadByteIter::new(self)
    }
}

impl<'a> IntoIterator for EmdfPayloadBytes<'a> {
    type Item = u8;
    type IntoIter = EmdfPayloadByteIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`EmdfPayloadBytes`] 的无分配迭代器。
#[derive(Debug, Clone)]
pub struct EmdfPayloadByteIter<'a> {
    reader: BitReader<'a>,
    remaining: u32,
}

impl<'a> EmdfPayloadByteIter<'a> {
    fn new(bytes: EmdfPayloadBytes<'a>) -> Self {
        let mut reader = BitReader::new(bytes.source);
        let remaining = if reader.skip_bits(bytes.bit_offset).is_ok() {
            bytes.len
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for EmdfPayloadByteIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = self
            .reader
            .read_bits(8)
            .ok()
            .and_then(|value| u8::try_from(value).ok());
        if value.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        } else {
            self.remaining = 0;
        }
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match usize::try_from(self.remaining) {
            Ok(remaining) => (remaining, Some(remaining)),
            Err(_) => (usize::MAX, None),
        }
    }
}

/// 一个 `emdf_payloads_substream()` 的有界 opaque envelope。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmdfPayloadsSubstream {
    /// ID 0 终止符之前的 payload 数量。
    pub payload_count: u32,
    /// 所有 payload 的字节总数。
    pub payload_bytes: u64,
    /// 单个 payload 的最大字节数。
    pub max_payload_bytes: u32,
    /// 首个非零 `emdf_payload_id`；空 substream 为 `None`。
    pub first_payload_id: Option<u32>,
    /// substream 末尾 `byte_align` 消耗的比特数。
    pub align_bits: u8,
    payloads: [EmdfPayload; MAX_EMDF_PAYLOADS],
    payloads_written: usize,
}

impl Default for EmdfPayloadsSubstream {
    fn default() -> Self {
        Self {
            payload_count: 0,
            payload_bytes: 0,
            max_payload_bytes: 0,
            first_payload_id: None,
            align_bits: 0,
            payloads: [EmdfPayload::EMPTY; MAX_EMDF_PAYLOADS],
            payloads_written: 0,
        }
    }
}

impl EmdfPayloadsSubstream {
    /// 按码流顺序取得全部非终止 payload。
    #[must_use]
    pub fn payloads(&self) -> &[EmdfPayload] {
        self.payloads.get(..self.payloads_written).unwrap_or(&[])
    }

    /// 解析 `TS103190-1:v1.4.1:4.2.4.4` 的 payload envelope。
    ///
    /// 未知 payload ID 保留为 opaque bytes；终止 ID 缺失、配置或 payload 截断、
    /// 容量超限以及变长整数溢出均失败关闭。
    ///
    /// # Errors
    ///
    /// 见 [`EmdfError`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, EmdfError> {
        let mut out = Self::default();
        loop {
            let id_position = reader.bit_position();
            if reader.remaining_bits() < 5 {
                return Err(EmdfError::MissingTerminator {
                    bit_position: id_position,
                    remaining_bits: reader.remaining_bits(),
                });
            }
            let mut payload_id = u32::try_from(reader.read_bits(5)?).unwrap_or(u32::MAX);
            if payload_id == 31 {
                payload_id = reader.variable_bits_scaled_u32(5, payload_id, 0)?;
            }
            if payload_id == 0 {
                out.align_bits = u8::try_from(reader.byte_align()?).unwrap_or(u8::MAX);
                return Ok(out);
            }
            if out.payloads_written >= MAX_EMDF_PAYLOADS {
                return Err(EmdfError::TooManyPayloads {
                    limit: MAX_EMDF_PAYLOADS,
                    bit_position: id_position,
                });
            }

            let config = EmdfPayloadConfig::parse(reader)?;
            let size_position = reader.bit_position();
            let payload_size = reader.variable_bits_u32(8)?;
            if payload_size > MAX_EMDF_PAYLOAD_BYTES {
                return Err(EmdfError::PayloadTooLarge {
                    declared: payload_size,
                    limit: MAX_EMDF_PAYLOAD_BYTES,
                    bit_position: size_position,
                });
            }
            let payload_bit_offset = reader.bit_position();
            let payload_bits =
                u64::from(payload_size)
                    .checked_mul(8)
                    .ok_or(ReadError::ValueOverflow {
                        bit_position: size_position,
                    })?;
            reader.skip_bits(payload_bits)?;

            let slot =
                out.payloads
                    .get_mut(out.payloads_written)
                    .ok_or(EmdfError::TooManyPayloads {
                        limit: MAX_EMDF_PAYLOADS,
                        bit_position: id_position,
                    })?;
            *slot = EmdfPayload {
                id: payload_id,
                config,
                size_bytes: payload_size,
                payload_bit_offset,
            };
            out.payloads_written = out.payloads_written.saturating_add(1);
            out.payload_count = u32::try_from(out.payloads_written).unwrap_or(u32::MAX);
            out.payload_bytes = out
                .payload_bytes
                .checked_add(u64::from(payload_size))
                .ok_or(ReadError::ValueOverflow {
                    bit_position: id_position,
                })?;
            out.max_payload_bytes = out.max_payload_bytes.max(payload_size);
            if out.first_payload_id.is_none() {
                out.first_payload_id = Some(payload_id);
            }
        }
    }
}

/// 跳过 `emdf_reserved()`，返回被跳过的字节数。
///
/// 表 80 的表题是 `emdf_reserved()`，表体却写作 `emdf_protection()`；这是规范
/// 中的编辑不一致。两者的读取行为相同，此处按表体实现。
fn skip_emdf_reserved(reader: &mut BitReader<'_>) -> Result<u32, TopologyError> {
    let primary = u32::try_from(reader.read_bits(2)?).unwrap_or(0);
    let secondary = u32::try_from(reader.read_bits(2)?).unwrap_or(0);

    // 两个 2 比特长度各自贡献 1 << (2*(len-1)) 字节，合计不超过 32。
    let span = |length: u32| -> u32 {
        length
            .checked_sub(1)
            .and_then(|shift| shift.checked_mul(2))
            .and_then(|shift| 1u32.checked_shl(shift))
            .unwrap_or(0)
    };
    let n_skip_bytes = span(primary).saturating_add(span(secondary));

    reader.skip_bits(u64::from(n_skip_bytes).saturating_mul(8))?;
    Ok(n_skip_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::ReadError;
    extern crate std;

    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "测试内的位串打包，索引受输入长度约束"
    )]
    fn pack(bits: &str) -> [u8; 16] {
        let mut out = [0u8; 16];
        let mut index = 0usize;
        for ch in bits.chars() {
            if ch == '0' || ch == '1' {
                if ch == '1' {
                    out[index / 8] |= 1 << (7 - index % 8);
                }
                index += 1;
            }
        }
        out
    }

    #[derive(Debug, Default)]
    struct TestBits {
        bytes: std::vec::Vec<u8>,
        bit_len: usize,
    }

    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::integer_division_remainder_used,
        reason = "测试位写入器的宽度和值均由构造用例控制"
    )]
    impl TestBits {
        fn push(&mut self, value: u64, width: u32) {
            assert!(width <= 64);
            for shift in (0..width).rev() {
                if self.bit_len.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let bit = (value >> shift) & 1;
                if bit != 0 {
                    let byte_index = self.bit_len / 8;
                    let bit_index = 7 - self.bit_len % 8;
                    self.bytes[byte_index] |= 1 << bit_index;
                }
                self.bit_len += 1;
            }
        }

        fn push_variable_bits(&mut self, mut value: u64, width: u32) {
            let radix = 1u64 << width;
            let mut chunks = [0u64; 8];
            let mut written = 0usize;
            loop {
                chunks[written] = value % radix;
                written += 1;
                if value < radix {
                    break;
                }
                value = value / radix - 1;
            }
            for index in (0..written).rev() {
                self.push(chunks[index], width);
                self.push(u64::from(index != 0), 1);
            }
        }

        fn push_empty_payload(&mut self, id: u32) {
            self.push(u64::from(id), 5);
            self.push(0b00001, 5); // 最简 config，丢弃未知 payload
            self.push_variable_bits(0, 8);
        }

        fn byte_align(&mut self) {
            while !self.bit_len.is_multiple_of(8) {
                self.push(0, 1);
            }
        }

        fn reader(&self) -> BitReader<'_> {
            BitReader::new_bounded(&self.bytes, 0, self.bit_len as u64).unwrap()
        }
    }

    // emdf_version | key_id | b_payloads | reserved_primary | reserved_secondary
    #[test]
    fn parses_minimal_emdf_info() {
        let data = pack("00 000 0 00 00");
        let mut reader = BitReader::new(&data);
        let info = EmdfInfo::parse(&mut reader).unwrap();
        assert_eq!(info.emdf_version, 0);
        assert_eq!(info.key_id, 0);
        assert_eq!(info.payloads_substream_index, None);
        assert_eq!(info.reserved_bytes, 0);
        assert_eq!(reader.bit_position(), 10);
    }

    /// 版本与 key ID 是值字段；超出 `u32` 时必须拒绝，不能静默饱和并别名。
    #[test]
    fn rejects_emdf_version_above_u32() {
        // emdf_version=3；15 个 `11 + more` 后以 `00 + stop` 终止，扩展值为
        // 5_726_623_056。其余字段均取最短形式，错误实现会成功返回 u32::MAX。
        let data = pack(
            "11 \
             111 111 111 111 111 111 111 111 111 111 111 111 111 111 111 000 \
             000 0 00 00",
        );
        let mut reader = BitReader::new(&data);

        assert!(matches!(
            EmdfInfo::parse(&mut reader),
            Err(TopologyError::Read(ReadError::ValueOverflow { .. }))
        ));
    }

    #[test]
    fn reads_payloads_substream_index() {
        let data = pack("00 000 1 10 00 00");
        let mut reader = BitReader::new(&data);
        let info = EmdfInfo::parse(&mut reader).unwrap();
        assert_eq!(info.payloads_substream_index, Some(2));
    }

    #[test]
    fn configuration_copy_ignores_per_frame_payload_routing() {
        let info = EmdfInfo {
            emdf_version: 2,
            key_id: 5,
            payloads_substream_index: Some(7),
            reserved_bytes: 32,
        };
        assert_eq!(
            info.configuration_copy(),
            EmdfInfo {
                emdf_version: 2,
                key_id: 5,
                payloads_substream_index: None,
                reserved_bytes: 0,
            }
        );
    }

    /// substream_index 为 3 时以 variable_bits(2) 扩展：01 加停止位得 1。
    #[test]
    fn extends_payloads_substream_index() {
        let data = pack("00 000 1 11 01 0 00 00");
        let mut reader = BitReader::new(&data);
        let info = EmdfInfo::parse(&mut reader).unwrap();
        assert_eq!(info.payloads_substream_index, Some(4));
    }

    /// primary=01 贡献 1 字节，secondary=10 贡献 4 字节，共 5 字节。
    #[test]
    fn skips_reserved_bytes() {
        let data = pack("00 000 0 01 10 00000000 00000000 00000000 00000000 00000000");
        let mut reader = BitReader::new(&data);
        let info = EmdfInfo::parse(&mut reader).unwrap();
        assert_eq!(info.reserved_bytes, 5);
        assert_eq!(reader.bit_position(), 10 + 40);
    }

    #[test]
    fn extends_emdf_version_and_key_id() {
        // emdf_version=11 + variable_bits(2)=01,stop → 3+1=4
        // key_id=111 + variable_bits(3)=001,stop → 7+1=8
        let data = pack("11 01 0 111 001 0 0 00 00");
        let mut reader = BitReader::new(&data);
        let info = EmdfInfo::parse(&mut reader).unwrap();
        assert_eq!(info.emdf_version, 4);
        assert_eq!(info.key_id, 8);
    }

    #[test]
    fn parses_empty_payloads_substream() {
        let data = pack("00000 000");
        let mut reader = BitReader::new(&data);
        let parsed = EmdfPayloadsSubstream::parse(&mut reader).unwrap();

        assert_eq!(parsed.payload_count, 0);
        assert_eq!(parsed.payload_bytes, 0);
        assert_eq!(parsed.first_payload_id, None);
        assert_eq!(parsed.align_bits, 3);
        assert!(parsed.payloads().is_empty());
        assert_eq!(reader.bit_position(), 8);
    }

    #[test]
    fn preserves_configured_payload_and_finds_terminator() {
        // ID 1；sample offset=3；duration/group/codec 缺席；保留未知 payload；
        // frame-aligned 字段因 sample offset 存在而不传；priority/proc；size=2；
        // 两字节 opaque payload；终止 ID 0；末尾 3 位对齐。
        let data = pack(
            "00001 \
             1 00000000011 0 0 0 0 0 \
             10101 10 \
             00000010 0 \
             10100101 01011010 \
             00000 00000",
        );
        let mut reader = BitReader::new(&data[..8]);
        let parsed = EmdfPayloadsSubstream::parse(&mut reader).unwrap();

        assert_eq!(parsed.payload_count, 1);
        assert_eq!(parsed.payload_bytes, 2);
        assert_eq!(parsed.max_payload_bytes, 2);
        assert_eq!(parsed.first_payload_id, Some(1));
        let payload = *parsed.payloads().first().unwrap();
        assert_eq!(payload.id, 1);
        assert_eq!(payload.size_bytes, 2);
        assert_eq!(payload.config.sample_offset, Some(3));
        assert_eq!(payload.config.duration, None);
        assert_eq!(payload.config.group_id, None);
        assert_eq!(payload.config.codec_data, None);
        assert!(!payload.config.discard_unknown_payload);
        assert!(!payload.config.payload_frame_aligned);
        assert!(!payload.config.create_duplicate);
        assert!(!payload.config.remove_duplicate);
        assert_eq!(payload.config.priority, Some(21));
        assert_eq!(payload.config.processing_allowed, Some(2));
        let bytes = payload.bytes(&data[..8]).unwrap();
        assert_eq!(bytes.len(), 2);
        assert!(!bytes.is_empty());
        assert_eq!(bytes.get(0), Some(0xa5));
        assert_eq!(bytes.get(1), Some(0x5a));
        assert_eq!(bytes.get(2), None);
        assert_eq!(bytes.as_aligned_slice(), None);
        assert_eq!(bytes.iter().collect::<std::vec::Vec<_>>(), [0xa5, 0x5a]);
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[test]
    fn preserves_frame_aligned_config_and_private_codec_data() {
        // ID 2；sample offset 缺席；duration=5；group ID=3；codecdata=0xab；
        // 未知 payload 不直接丢弃且要求 frame-aligned；create=1/remove=0；
        // priority=3、proc_allowed=2；payload=0x7e；终止符后 7 位对齐。
        let data = pack(
            "00010 \
             0 \
             1 00000000101 0 \
             1 11 0 \
             1 10101011 \
             0 \
             1 1 0 \
             00011 10 \
             00000001 0 \
             01111110 \
             00000 0000000",
        );
        let mut reader = BitReader::new(&data[..9]);
        let parsed = EmdfPayloadsSubstream::parse(&mut reader).unwrap();
        let payload = *parsed.payloads().first().unwrap();

        assert_eq!(payload.id, 2);
        assert_eq!(payload.config.sample_offset, None);
        assert_eq!(payload.config.duration, Some(5));
        assert_eq!(payload.config.group_id, Some(3));
        assert_eq!(payload.config.codec_data, Some(0xab));
        assert!(!payload.config.discard_unknown_payload);
        assert!(payload.config.payload_frame_aligned);
        assert!(payload.config.create_duplicate);
        assert!(!payload.config.remove_duplicate);
        assert_eq!(payload.config.priority, Some(3));
        assert_eq!(payload.config.processing_allowed, Some(2));
        assert_eq!(payload.bytes(&data[..9]).unwrap().get(0), Some(0x7e));
        assert_eq!(parsed.align_bits, 7);
        assert_eq!(reader.remaining_bits(), 0);
    }

    /// 表 18 的 `emdf_payload_id += variable_bits(5)`：扩展值加在 31 上。
    ///
    /// 把基数写成 0 不改变消耗的比特数，因此落点判据完全看不见它；只有 ID 的
    /// 取值会变。而 ID 又兼任终止符，基数写错时「31 + 扩展 0」会被误判成终止
    /// 符，把后面的 payload 整段丢掉——所以第二段用的正是扩展值为 0 的码流。
    #[test]
    fn extended_payload_id_is_offset_from_thirty_one() {
        // ID 31 + variable_bits(5)=3 → 34；配置取最简且丢弃未知 payload；
        // 长度 0；随后终止 ID 与 2 位对齐。
        let data = pack("11111 00011 0 0 0 0 0 1 00000000 0 00000");
        let mut reader = BitReader::new(&data[..4]);
        let parsed = EmdfPayloadsSubstream::parse(&mut reader).unwrap();

        assert_eq!(parsed.payload_count, 1);
        assert_eq!(parsed.first_payload_id, Some(34));
        assert_eq!(
            parsed.payloads().first().map(|payload| payload.id),
            Some(34)
        );
        assert_eq!(parsed.align_bits, 2);
        assert_eq!(reader.remaining_bits(), 0);

        // 扩展值为 0：真实 ID 是 31，不是终止符。
        let data = pack("11111 00000 0 0 0 0 0 1 00000000 0 00000");
        let mut reader = BitReader::new(&data[..4]);
        let parsed = EmdfPayloadsSubstream::parse(&mut reader).unwrap();

        assert_eq!(parsed.payload_count, 1, "扩展 ID 31 不得被当成终止符");
        assert_eq!(parsed.first_payload_id, Some(31));
        assert_eq!(
            parsed.payloads().first().map(|payload| payload.id),
            Some(31)
        );
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[test]
    fn accepts_exact_payload_capacity_and_rejects_the_next_id() {
        let mut exact = TestBits::default();
        for _ in 0..MAX_EMDF_PAYLOADS {
            exact.push_empty_payload(1);
        }
        exact.push(0, 5);
        exact.byte_align();
        let mut reader = exact.reader();
        let parsed = EmdfPayloadsSubstream::parse(&mut reader).unwrap();
        assert_eq!(parsed.payloads().len(), MAX_EMDF_PAYLOADS);
        assert_eq!(reader.remaining_bits(), 0);

        let mut overflow = TestBits::default();
        for _ in 0..MAX_EMDF_PAYLOADS {
            overflow.push_empty_payload(1);
        }
        let overflow_position = overflow.bit_len as u64;
        overflow.push(1, 5);
        let mut reader = overflow.reader();
        assert_eq!(
            EmdfPayloadsSubstream::parse(&mut reader),
            Err(EmdfError::TooManyPayloads {
                limit: MAX_EMDF_PAYLOADS,
                bit_position: overflow_position,
            })
        );
    }

    #[test]
    fn rejects_payload_above_annex_g_frame_limit() {
        let mut bits = TestBits::default();
        bits.push(1, 5);
        bits.push(0b00001, 5); // 最简 config，丢弃未知 payload
        let size_position = bits.bit_len as u64;
        bits.push_variable_bits(u64::from(MAX_EMDF_PAYLOAD_BYTES) + 1, 8);
        let mut reader = bits.reader();

        assert_eq!(
            EmdfPayloadsSubstream::parse(&mut reader),
            Err(EmdfError::PayloadTooLarge {
                declared: MAX_EMDF_PAYLOAD_BYTES + 1,
                limit: MAX_EMDF_PAYLOAD_BYTES,
                bit_position: size_position,
            })
        );
    }

    #[test]
    fn rejects_missing_payload_terminator() {
        let mut bits = TestBits::default();
        bits.push_empty_payload(1);
        let end = bits.bit_len as u64;
        let mut reader = bits.reader();

        assert_eq!(
            EmdfPayloadsSubstream::parse(&mut reader),
            Err(EmdfError::MissingTerminator {
                bit_position: end,
                remaining_bits: 0,
            })
        );
    }

    #[test]
    fn rejects_payload_size_above_u32_without_aliasing() {
        let mut bits = TestBits::default();
        bits.push(1, 5);
        bits.push(0b00001, 5); // 最简 config，丢弃未知 payload
        bits.push_variable_bits(u64::from(u32::MAX) + 1, 8);
        let mut reader = bits.reader();

        assert!(matches!(
            EmdfPayloadsSubstream::parse(&mut reader),
            Err(EmdfError::Read(ReadError::ValueOverflow { .. }))
        ));
    }

    #[test]
    fn rejects_payload_size_beyond_the_substream() {
        // 最简配置后声明两字节，但只提供一字节且没有终止 ID。
        let data = pack("00001 0 0 0 0 1 00000010 0 10100101");
        let mut reader = BitReader::new(&data[..4]);
        assert!(matches!(
            EmdfPayloadsSubstream::parse(&mut reader),
            Err(EmdfError::Read(ReadError::OutOfBounds { .. }))
        ));
    }
}
