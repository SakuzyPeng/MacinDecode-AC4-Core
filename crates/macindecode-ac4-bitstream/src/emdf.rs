//! EMDF 信息元素。
//!
//! 对应 `TS103190-1:v1.4.1:4.2.3.5`（表 8）、`4.2.3.10`（表 13）与
//! `4.2.14.15`（表 80）。
//!
//! TOC 中出现的 `emdf_info()` 只用于定位与跳过 EMDF 负载，本模块因此只解析
//! 到能准确前进的程度，不触及负载内容。

use crate::reader::{BitReader, ReadError};
use crate::topology::{TopologyError, read_substream_index};

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

/// 内嵌 `emdf_payloads_substream()` 的有界解析摘要。
///
/// Payload bytes 目前按长度透明跳过；摘要保留 envelope 的数量、总量与首个 ID，
/// 足以证明解析落点并为后续 opaque payload 保留层提供稳定边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmdfPayloadsSubstream {
    /// 终止 ID 之前的 payload 数量。
    pub payload_count: u32,
    /// 所有 payload 的字节总数。
    pub payload_bytes: u64,
    /// 单个 payload 的最大字节数。
    pub max_payload_bytes: u32,
    /// 首个非零 `emdf_payload_id`；空 substream 为 `None`。
    pub first_payload_id: Option<u32>,
    /// substream 末尾 `byte_align` 消耗的比特数。
    pub align_bits: u8,
}

impl EmdfPayloadsSubstream {
    /// 解析 `TS103190-1:v1.4.1:4.2.4.4` 的 payload envelope。
    ///
    /// 未知 payload ID 的内容按声明长度跳过；终止 ID 缺失、配置或 payload
    /// 截断以及变长整数溢出均返回 [`ReadError`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, ReadError> {
        let mut out = Self::default();
        loop {
            let id_position = reader.bit_position();
            let mut payload_id = u32::try_from(reader.read_bits(5)?).unwrap_or(u32::MAX);
            if payload_id == 31 {
                payload_id = reader.variable_bits_scaled_u32(5, payload_id, 0)?;
            }
            if payload_id == 0 {
                out.align_bits = u8::try_from(reader.byte_align()?).unwrap_or(u8::MAX);
                return Ok(out);
            }

            skip_emdf_payload_config(reader)?;
            let payload_size = reader.variable_bits_u32(8)?;
            reader.skip_bits(u64::from(payload_size).saturating_mul(8))?;

            out.payload_count =
                out.payload_count
                    .checked_add(1)
                    .ok_or(ReadError::ValueOverflow {
                        bit_position: id_position,
                    })?;
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

/// `TS103190-1:v1.4.1:4.2.14.14`（表 79）。当前只需准确前进，配置值在
/// M4.5 opaque payload API 落地时再进入公共模型。
fn skip_emdf_payload_config(reader: &mut BitReader<'_>) -> Result<(), ReadError> {
    let sample_offset_present = reader.read_flag()?;
    if sample_offset_present {
        let _sample_offset = reader.variable_bits_u32(11)?;
    }
    if reader.read_flag()? {
        let _duration = reader.variable_bits_u32(11)?;
    }
    if reader.read_flag()? {
        let _group_id = reader.variable_bits_u32(2)?;
    }
    if reader.read_flag()? {
        let _codec_data = reader.read_bits(8)?;
    }

    let discard_unknown_payload = reader.read_flag()?;
    if discard_unknown_payload {
        return Ok(());
    }

    let mut payload_frame_aligned = false;
    if !sample_offset_present {
        payload_frame_aligned = reader.read_flag()?;
        if payload_frame_aligned {
            let _create_duplicate = reader.read_flag()?;
            let _remove_duplicate = reader.read_flag()?;
        }
    }
    if sample_offset_present || payload_frame_aligned {
        let _priority = reader.read_bits(5)?;
        let _processing_allowed = reader.read_bits(2)?;
    }
    Ok(())
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
        assert_eq!(reader.bit_position(), 8);
    }

    #[test]
    fn skips_configured_payload_and_finds_terminator() {
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
        assert_eq!(parsed.align_bits, 2);
        assert_eq!(reader.remaining_bits(), 0);

        // 扩展值为 0：真实 ID 是 31，不是终止符。
        let data = pack("11111 00000 0 0 0 0 0 1 00000000 0 00000");
        let mut reader = BitReader::new(&data[..4]);
        let parsed = EmdfPayloadsSubstream::parse(&mut reader).unwrap();

        assert_eq!(parsed.payload_count, 1, "扩展 ID 31 不得被当成终止符");
        assert_eq!(parsed.first_payload_id, Some(31));
        assert_eq!(reader.remaining_bits(), 0);
    }

    #[test]
    fn rejects_payload_size_beyond_the_substream() {
        // 最简配置后声明两字节，但只提供一字节且没有终止 ID。
        let data = pack("00001 0 0 0 0 1 00000010 0 10100101");
        let mut reader = BitReader::new(&data[..4]);
        assert!(matches!(
            EmdfPayloadsSubstream::parse(&mut reader),
            Err(ReadError::OutOfBounds { .. })
        ));
    }
}
