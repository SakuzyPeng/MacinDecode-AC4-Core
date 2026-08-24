//! AC-4 table of contents。
//!
//! 对应 `TS103190-1:v1.4.1:4.2.3.1`（表 4）与 `4.3.3.2`。
//!
//! 本模块只解析 TOC 的前置字段，即到 `payload_base` 为止。其后的
//! presentation、group 与 `substream_index_table()` 由 [`crate::topology`] 解析。
//!
//! 解码器配置只能取自 TOC：`dac4` 中的同名字段仅供容器层参考，不得用于
//! 配置解码器（见 `E.4`）。

use crate::reader::{BitReader, ReadError};
use core::fmt;

/// TOC 解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TocError {
    /// 读取越界或变长字段溢出。
    Read(ReadError),
}

impl fmt::Display for TocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TocError::Read(error) => write!(f, "Failed to read TOC: {error}"),
        }
    }
}

impl core::error::Error for TocError {}

impl From<ReadError> for TocError {
    fn from(error: ReadError) -> Self {
        TocError::Read(error)
    }
}

/// `wait_frames` 的解释结果，见 `4.3.3.2.4`（表 81）。
///
/// 规范 `4.3.0` 规定：后文引用某个有解释表的字段时，指的是解释后的值而非
/// 传输的位模式。因此码流中的 3 比特码字不应直接对外暴露——同一个码字在
/// 不同 `frame_rate_index` 下代表不同的帧数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodingDelay {
    /// 码字 0：流为 CBR，无需等待。
    ConstantBitRate,
    /// 码字 1…6：输出前需等待的帧数。
    Frames(u8),
    /// 码字 7：流为 VBR，无法给出等待帧数。
    VariableBitRate,
}

/// 表 83：`frame_rate_index` 到 `frame_len_base` 的映射（48 kHz 基准）。
///
/// 索引 14、15 为保留，故只有前十四项。索引 13 的 `frame_rate` 一栏在规范中
/// 写作 `(23,44)`，但 `frame_len_base` 与索引 2 同为 2 048。
const FRAME_LEN_BASE_48K: [u16; 14] = [
    1920, 1920, 2048, 1536, 1536, 960, 960, 1024, 768, 768, 512, 384, 384, 2048,
];

/// 表 87：各 `frame_rate_index` 允许的最大 `frame_rate_factor`。
///
/// 与 [`crate::presentation`] 中 `frame_rate_multiply_info()` 的读取分支同源：
/// 那里决定读几位，这里决定合法取值集合。
#[must_use]
pub const fn max_frame_rate_factor(frame_rate_index: u8) -> u32 {
    match frame_rate_index {
        2..=4 => 4,
        0 | 1 | 7..=9 => 2,
        _ => 1,
    }
}

/// 相邻 AC-4 帧的 `sequence_counter` 关系。
///
/// 该字段不是普通的 10 比特循环计数器。规范只允许编码器写到 1 020，
/// `1 020 -> 1` 是正常回绕；前一帧写 0 则标记 splice，下一帧任意非零值
/// 仍属于正常运行。其余变化表示检测到流来源变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceTransition {
    /// 当前帧是观察到的第一帧，没有前值可比较。
    Initial,
    /// 正常递增、规范回绕，或 splice 标记后的恢复。
    Continuous,
    /// 规范定义的正常条件均不满足，检测到来源变化。
    SourceChange,
}

impl DecodingDelay {
    /// 按表 81 解释码字。
    ///
    /// `frame_rate_index` 为 10、11、12 时每级步进为 2 帧，其余索引为 1 帧。
    #[must_use]
    pub const fn from_code(code: u8, frame_rate_index: u8) -> Self {
        match code {
            0 => DecodingDelay::ConstantBitRate,
            7 => DecodingDelay::VariableBitRate,
            other => {
                let steps = other.saturating_sub(1);
                if matches!(frame_rate_index, 10..=12) {
                    DecodingDelay::Frames(steps.saturating_mul(2))
                } else {
                    DecodingDelay::Frames(steps)
                }
            }
        }
    }

    /// 输出前应等待的帧数；VBR 时无法确定，返回 `None`。
    ///
    /// 规范建议在源变化且延迟未知时按最大值处理，但那是产品策略，
    /// 不在解析层决定。
    #[must_use]
    pub const fn frames(&self) -> Option<u8> {
        match *self {
            DecodingDelay::ConstantBitRate => Some(0),
            DecodingDelay::Frames(count) => Some(count),
            DecodingDelay::VariableBitRate => None,
        }
    }
}

/// `ac4_toc()` 的前置字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4Toc {
    /// 比特流版本，见 `4.3.3.2.1`。
    ///
    /// 原始字段为 2 比特；取值 3 时以 `variable_bits(2)` 扩展，因此该值
    /// 可以大于 3。
    pub bitstream_version: u32,
    /// 序列计数器，见 `4.3.3.2.2`。
    pub sequence_counter: u16,
    /// `wait_frames` 的原始 3 比特码字，见 `4.3.3.2.4`；未传输时为 `None`。
    ///
    /// 该值只用于比特级核对。判断解码延迟一律使用 [`Ac4Toc::decoding_delay`]，
    /// 因为同一码字在不同 `frame_rate_index` 下代表不同的帧数。
    pub wait_frames_code: Option<u8>,
    /// 采样频率索引，见 `4.3.3.2.5`。
    pub fs_index: u8,
    /// 帧率索引，见 `4.3.3.2.6`。
    pub frame_rate_index: u8,
    /// 全局 I-frame 标志，见 `4.3.3.2.7`。
    ///
    /// 该标志为真的帧即容器意义上的同步样本，见 `E.2`。
    pub iframe_global: bool,
    /// presentation 数量，由 `b_single_presentation` 与
    /// `b_more_presentations` 推导，见 `4.3.3.2.8`、`4.3.3.2.9`。
    pub n_presentations: u32,
    /// 负载基准偏移，见 `4.3.3.2.10`、`4.3.3.2.11`。
    pub payload_base: u32,
    /// 已消耗的比特数，即后续 `ac4_presentation_info()` 的起点。
    pub bits_consumed: u64,
}

impl Ac4Toc {
    /// 按 `TS103190-1:v1.4.1:4.3.3.2.2` 判断与前一帧的计数关系。
    #[must_use]
    pub const fn sequence_transition(&self, previous: Option<u16>) -> SequenceTransition {
        let Some(previous) = previous else {
            return SequenceTransition::Initial;
        };
        let current = self.sequence_counter;

        // 编码器不得写出大于 1 020 的值；损坏或未来扩展值不能被普通整数
        // 回绕规则误判为连续。
        if previous > 1_020 || current > 1_020 {
            return SequenceTransition::SourceChange;
        }
        if (previous < 1_020 && current == previous.saturating_add(1))
            || (previous == 1_020 && current == 1)
            || (previous == 0 && current != 0)
        {
            SequenceTransition::Continuous
        } else {
            SequenceTransition::SourceChange
        }
    }

    /// 按表 81 解释出的解码延迟。
    ///
    /// `b_wait_frames` 为假时规范未给出延迟信息，返回 `None`；此时按
    /// `4.3.3.2.4` 应假定音频连续，除非检测到源变化。
    #[must_use]
    pub const fn decoding_delay(&self) -> Option<DecodingDelay> {
        match self.wait_frames_code {
            Some(code) => Some(DecodingDelay::from_code(code, self.frame_rate_index)),
            None => None,
        }
    }

    /// 表 82 的 `base_samp_freq`，单位 Hz。
    ///
    /// `fs_index` 只有一位，两个取值都有定义，故不会返回 `None`；仍以
    /// `Option` 表达，是为了与其余可保留的派生量保持同一形状。
    #[must_use]
    pub const fn base_sampling_frequency_hz(&self) -> Option<u32> {
        match self.fs_index {
            0 => Some(44_100),
            1 => Some(48_000),
            _ => None,
        }
    }

    /// 表 83 与表 84 的 `frame_len_base`，即 48 kHz 外部采样率下的帧长。
    ///
    /// `fs_index == 0`（44,1 kHz）时表 84 只定义 `frame_rate_index == 13`，
    /// 其余索引为保留值，返回 `None`。
    #[must_use]
    pub fn frame_len_base(&self) -> Option<u16> {
        if self.fs_index == 0 {
            // 表 84：44,1 kHz 只有一行，frame_length 即 frame_len_base。
            return (self.frame_rate_index == 13).then_some(2048);
        }
        if self.fs_index != 1 {
            return None;
        }
        FRAME_LEN_BASE_48K
            .get(usize::from(self.frame_rate_index))
            .copied()
    }

    /// 单个 `ac4_substream()` 覆盖的编解码帧长。
    ///
    /// `frame_rate_factor` 不为 1 时，一个 `ac4_substream_info()` 指向 2 或 4 个
    /// **连续解码**的 substream（`4.3.3.5.3`、`4.3.3.7.9` 的 NOTE），每个都是一
    /// 个独立的编解码帧，因此单帧长度是 `frame_len_base` 除以该因子。
    ///
    /// 表 87 允许的每个组合，其商都恰好等于表 83 中另一行的 `frame_len_base`；
    /// 该冗余由 `codec_frame_length_stays_in_the_table` 逐项核对。
    ///
    /// # Errors
    ///
    /// 无。`frame_len_base` 未定义、因子超出表 87 允许范围，或除不尽时返回
    /// `None`。
    #[must_use]
    pub fn codec_frame_len_base(&self, frame_rate_factor: u32) -> Option<u16> {
        // 表 87 只定义 1、2、4 三个取值。仅比较上界会放行 3，而 1 536 恰好
        // 能被 3 整除，那样会得到一个看似合理却不存在的帧长。
        if !matches!(frame_rate_factor, 1 | 2 | 4)
            || frame_rate_factor > max_frame_rate_factor(self.frame_rate_index)
        {
            return None;
        }
        let base = self.frame_len_base()?;
        let factor = u16::try_from(frame_rate_factor).ok()?;
        let length = base.checked_div(factor)?;
        (length.saturating_mul(factor) == base).then_some(length)
    }

    /// 解析 `raw_ac4_frame()` 开头的 TOC 前置字段。
    ///
    /// # Errors
    ///
    /// 数据不足以读出各字段时返回 [`TocError::Read`]。
    pub fn parse(raw_frame: &[u8]) -> Result<Self, TocError> {
        let mut reader = BitReader::new(raw_frame);

        // bitstream_version：2 比特，值为 3 时用 variable_bits(2) 扩展
        let mut bitstream_version = u32::try_from(reader.read_bits(2)?).unwrap_or(u32::MAX);
        if bitstream_version == 3 {
            bitstream_version = reader.variable_bits_scaled_u32(2, bitstream_version, 0)?;
        }

        let sequence_counter = u16::try_from(reader.read_bits(10)?).unwrap_or(u16::MAX);

        // b_wait_frames 为真时才传输 wait_frames；wait_frames 非零时另有
        // 2 比特保留字段
        let wait_frames_code = if reader.read_flag()? {
            let value = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
            if value > 0 {
                reader.read_bits(2)?;
            }
            Some(value)
        } else {
            None
        };

        let fs_index = u8::try_from(reader.read_bits(1)?).unwrap_or(u8::MAX);
        let frame_rate_index = u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX);
        let iframe_global = reader.read_flag()?;

        let n_presentations = if reader.read_flag()? {
            1
        } else if reader.read_flag()? {
            reader.variable_bits_scaled_u32(2, 2, 0)?
        } else {
            0
        };

        let payload_base = if reader.read_flag()? {
            let minus1 = u32::try_from(reader.read_bits(5)?).unwrap_or(u32::MAX);
            let mut base = minus1.saturating_add(1);
            if base == 0x20 {
                base = reader.variable_bits_scaled_u32(3, base, 0)?;
            }
            base
        } else {
            0
        };

        Ok(Self {
            bitstream_version,
            sequence_counter,
            wait_frames_code,
            fs_index,
            frame_rate_index,
            iframe_global,
            n_presentations,
            payload_base,
            bits_consumed: reader.bit_position(),
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "位串打包 helper 的索引与移位受输入长度约束"
)]
mod tests {
    use super::*;

    /// 把可读的位串打包成字节，空格与竖线作分隔符忽略。
    ///
    /// 手写十六进制向量极易在位序上出错，此处直接书写语法表中的字段顺序。
    fn pack(bits: &str) -> [u8; 8] {
        let mut out = [0u8; 8];
        let mut index = 0usize;
        for ch in bits.chars() {
            match ch {
                '0' | '1' => {
                    if ch == '1' {
                        let byte = index / 8;
                        let shift = 7 - (index % 8);
                        if let Some(slot) = out.get_mut(byte) {
                            *slot |= 1 << shift;
                        }
                    }
                    index += 1;
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn pack_helper_is_msb_first() {
        assert_eq!(pack("1010 0000")[0], 0b1010_0000);
        assert_eq!(pack("0000 0001 1000 0000")[0..2], [0x01, 0x80]);
    }

    // ver | sequence_counter | wait | fs | rate | iframe | single | base
    #[test]
    fn parses_minimal_toc() {
        let data = pack("10 0000000000 0 1 1101 1 1 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.bitstream_version, 2);
        assert_eq!(toc.sequence_counter, 0);
        assert_eq!(toc.wait_frames_code, None);
        assert_eq!(toc.decoding_delay(), None);
        assert_eq!(toc.fs_index, 1);
        assert_eq!(toc.frame_rate_index, 13);
        assert!(toc.iframe_global);
        assert_eq!(toc.n_presentations, 1);
        assert_eq!(toc.payload_base, 0);
        assert_eq!(toc.bits_consumed, 21);
    }

    #[test]
    fn reads_sequence_counter() {
        let data = pack("10 1000000001 0 1 1101 1 1 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.sequence_counter, 0b1000000001);
    }

    /// bitstream_version 为 3 时以 variable_bits(2) 扩展：01 后接停止位
    /// 得到 1，故版本为 3 + 1 = 4。
    #[test]
    fn extends_bitstream_version() {
        let data = pack("11 01 0 0000000000 0 1 1101 1 1 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.bitstream_version, 4);
        assert_eq!(toc.fs_index, 1);
        assert_eq!(toc.frame_rate_index, 13);
    }

    /// wait_frames 非零时另有 2 比特保留字段
    #[test]
    fn wait_frames_consumes_reserved_bits() {
        let data = pack("10 0000000000 1 011 00 1 0001 0 1 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.wait_frames_code, Some(3));
        assert_eq!(toc.fs_index, 1);
        assert_eq!(toc.frame_rate_index, 1);
        assert!(!toc.iframe_global);
        assert_eq!(toc.n_presentations, 1);
    }

    /// wait_frames 为 0 时不读保留字段
    #[test]
    fn wait_frames_zero_skips_reserved() {
        let data = pack("10 0000000000 1 000 1 0001 0 1 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.wait_frames_code, Some(0));
        assert_eq!(toc.decoding_delay(), Some(DecodingDelay::ConstantBitRate));
        assert_eq!(toc.fs_index, 1);
        assert_eq!(toc.frame_rate_index, 1);
        assert_eq!(toc.n_presentations, 1);
    }

    /// 非单 presentation 时由 b_more_presentations 与 variable_bits 推导
    #[test]
    fn derives_multiple_presentations() {
        let data = pack("10 0000000000 0 1 1101 1 0 1 01 0 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.n_presentations, 3, "variable_bits 得 1，加 2");
    }

    /// 两个标志都为 0 时 presentation 数量为 0
    #[test]
    fn zero_presentations() {
        let data = pack("10 0000000000 0 1 1101 1 0 0 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.n_presentations, 0);
    }

    /// payload_base 由 5 比特的 minus1 加一得到
    #[test]
    fn reads_payload_base() {
        let data = pack("10 0000000000 0 1 1101 1 1 1 00011");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.payload_base, 4);
    }

    /// payload_base 达到 0x20 时以 variable_bits(3) 扩展
    #[test]
    fn extends_payload_base() {
        let data = pack("10 0000000000 0 1 1101 1 1 1 11111 010 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.payload_base, 0x20 + 2);
    }

    /// 表 81：同一码字在不同 frame_rate_index 下代表不同的帧数。
    #[test]
    fn decoding_delay_follows_table_81() {
        // 索引 13 每级 1 帧
        assert_eq!(
            DecodingDelay::from_code(6, 13),
            DecodingDelay::Frames(5),
            "码字 6 在索引 13 下为 5 帧"
        );
        assert_eq!(DecodingDelay::from_code(1, 13), DecodingDelay::Frames(0));
        // 索引 10…12 每级 2 帧
        assert_eq!(DecodingDelay::from_code(6, 11), DecodingDelay::Frames(10));
        // 规范 4.3.0 的 EXAMPLE 1：索引 10 下码字 3 对应 4 帧
        assert_eq!(DecodingDelay::from_code(3, 10), DecodingDelay::Frames(4));
        // 两端为特殊含义，不是帧数
        assert_eq!(
            DecodingDelay::from_code(0, 13),
            DecodingDelay::ConstantBitRate
        );
        assert_eq!(
            DecodingDelay::from_code(7, 13),
            DecodingDelay::VariableBitRate
        );
        assert_eq!(DecodingDelay::VariableBitRate.frames(), None);
    }

    /// 实测码流：wait_frames 码字 6、frame_rate_index 13 → 等待 5 帧。
    #[test]
    fn interprets_probe_stream_delay() {
        let data = pack("10 0000000000 1 110 00 1 1101 1 1 0");
        let toc = Ac4Toc::parse(&data).unwrap();
        assert_eq!(toc.wait_frames_code, Some(6));
        assert_eq!(toc.frame_rate_index, 13);
        assert_eq!(toc.decoding_delay(), Some(DecodingDelay::Frames(5)));
    }

    #[test]
    fn sequence_counter_uses_normative_wrap_and_splice_rules() {
        let toc = |counter| Ac4Toc {
            bitstream_version: 2,
            sequence_counter: counter,
            wait_frames_code: None,
            fs_index: 1,
            frame_rate_index: 13,
            iframe_global: false,
            n_presentations: 1,
            payload_base: 0,
            bits_consumed: 0,
        };

        assert_eq!(
            toc(1_020).sequence_transition(Some(1_019)),
            SequenceTransition::Continuous
        );
        assert_eq!(
            toc(1).sequence_transition(Some(1_020)),
            SequenceTransition::Continuous,
            "1 020 -> 1 是规范定义的正常回绕"
        );
        assert_eq!(
            toc(731).sequence_transition(Some(0)),
            SequenceTransition::Continuous,
            "splice 标记后的任意非零计数均可继续"
        );
        assert_eq!(
            toc(9).sequence_transition(Some(7)),
            SequenceTransition::SourceChange
        );
        assert_eq!(
            toc(0).sequence_transition(Some(1_020)),
            SequenceTransition::SourceChange
        );
        assert_eq!(
            toc(1_021).sequence_transition(Some(1_020)),
            SequenceTransition::SourceChange
        );
    }

    #[test]
    fn reports_truncated_input() {
        assert!(matches!(
            Ac4Toc::parse(&[0b1000_0000]).unwrap_err(),
            TocError::Read(_)
        ));
    }

    fn toc_with(fs_index: u8, frame_rate_index: u8) -> Ac4Toc {
        Ac4Toc {
            bitstream_version: 2,
            sequence_counter: 0,
            wait_frames_code: None,
            fs_index,
            frame_rate_index,
            iframe_global: true,
            n_presentations: 1,
            payload_base: 0,
            bits_consumed: 0,
        }
    }

    /// 表 82：一位的 `fs_index` 两个取值都有定义。
    #[test]
    fn base_sampling_frequency_follows_table_82() {
        assert_eq!(toc_with(0, 13).base_sampling_frequency_hz(), Some(44_100));
        assert_eq!(toc_with(1, 13).base_sampling_frequency_hz(), Some(48_000));
    }

    /// 表 83 的十四行逐项核对；保留索引返回 `None`。
    #[test]
    fn frame_len_base_follows_table_83() {
        for (index, expected) in [
            (0u8, 1920u16),
            (1, 1920),
            (2, 2048),
            (3, 1536),
            (4, 1536),
            (5, 960),
            (6, 960),
            (7, 1024),
            (8, 768),
            (9, 768),
            (10, 512),
            (11, 384),
            (12, 384),
            (13, 2048),
        ] {
            assert_eq!(
                toc_with(1, index).frame_len_base(),
                Some(expected),
                "frame_rate_index {index}"
            );
        }
        assert_eq!(toc_with(1, 14).frame_len_base(), None);
        assert_eq!(toc_with(1, 15).frame_len_base(), None);
    }

    /// 表 84：44,1 kHz 只定义 `frame_rate_index == 13`。
    #[test]
    fn frame_len_base_at_44k_only_defines_index_13() {
        assert_eq!(toc_with(0, 13).frame_len_base(), Some(2048));
        for index in 0..13u8 {
            assert_eq!(
                toc_with(0, index).frame_len_base(),
                None,
                "44,1 kHz 的索引 {index} 为保留"
            );
        }
    }

    /// 表 87 允许的每个 `frame_rate_factor`，其商都落在表 83 的取值集合内。
    ///
    /// 这是两张表之间的冗余：因子把帧率乘上去，商正是另一行更高帧率的
    /// `frame_len_base`。任一行抄错都会破坏该等式。
    #[test]
    fn codec_frame_length_stays_in_the_table() {
        for index in 0..14u8 {
            let toc = toc_with(1, index);
            let limit = max_frame_rate_factor(index);
            for factor in [1u32, 2, 4] {
                let derived = toc.codec_frame_len_base(factor);
                if factor > limit {
                    assert_eq!(derived, None, "索引 {index} 不允许因子 {factor}");
                    continue;
                }
                let Some(length) = derived else {
                    panic!("索引 {index} 因子 {factor} 应有定义");
                };
                assert!(
                    FRAME_LEN_BASE_48K.contains(&length),
                    "索引 {index} 因子 {factor} 得到 {length}，不在表 83 内"
                );
            }
            // 3 不在表 87 内，但 1 536 能被它整除；只比较上界会放行。
            assert_eq!(toc.codec_frame_len_base(3), None, "索引 {index} 的因子 3");
            assert_eq!(toc.codec_frame_len_base(0), None);
        }
    }

    /// 因子把 25 fps 的 2 048 分成 50 fps 的 1 024 与 100 fps 的 512。
    #[test]
    fn codec_frame_length_divides_by_the_factor() {
        let toc = toc_with(1, 2);
        assert_eq!(toc.codec_frame_len_base(1), Some(2048));
        assert_eq!(toc.codec_frame_len_base(2), Some(1024));
        assert_eq!(toc.codec_frame_len_base(4), Some(512));
        assert_eq!(
            toc_with(1, 13).codec_frame_len_base(2),
            None,
            "索引 13 的因子恒为 1"
        );
    }
}
