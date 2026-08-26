//! `AC4SpecificBox`（`dac4`）中的解码器专属信息。
//!
//! 对应 `TS103190-1:v1.4.1:E.4` 与 `TS103190-2:v1.3.1:E.5`–`E.7`。
//!
//! 规范明确指出 `ac4_dsi` 不得用于配置解码器：解码器只能从每个 sample 内
//! 的 `ac4_toc` 取得配置。此处解析它仅用于容器层的检视与交叉核对。

use core::fmt;
use macindecode_ac4_bitstream::{BitReader, ReadError};

/// 基础采样频率，对应 `4.3.3.2.5` 表 82。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseSamplingFrequency {
    /// `fs_index == 0`
    Hz44100,
    /// `fs_index == 1`
    Hz48000,
}

impl BaseSamplingFrequency {
    const fn from_index(fs_index: u8) -> Option<Self> {
        match fs_index {
            0 => Some(Self::Hz44100),
            1 => Some(Self::Hz48000),
            _ => None,
        }
    }

    /// 以赫兹表示的频率。
    #[must_use]
    pub const fn hz(self) -> u32 {
        match self {
            Self::Hz44100 => 44_100,
            Self::Hz48000 => 48_000,
        }
    }
}

/// 帧率与内部帧长，对应 `4.3.3.2.6` 表 83、表 84。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRate {
    /// 帧率分子。
    pub numerator: u32,
    /// 帧率分母。非整数帧率以精确有理数表示，不做浮点近似。
    pub denominator: u32,
    /// 外部采样率为 48 kHz 时的内部帧长，即 `frame_len_base`。
    ///
    /// 该值是编解码器内部长度，与容器时间轴上的 `sample_delta` 不同：
    /// 两者相差一个解码器重采样比。
    pub frame_length_base: u32,
}

/// 表 83：48 kHz、96 kHz、192 kHz 的 `frame_rate_index`。
const FRAME_RATE_48K: [Option<FrameRate>; 16] = [
    Some(FrameRate {
        numerator: 24_000,
        denominator: 1_001,
        frame_length_base: 1_920,
    }),
    Some(FrameRate {
        numerator: 24,
        denominator: 1,
        frame_length_base: 1_920,
    }),
    Some(FrameRate {
        numerator: 25,
        denominator: 1,
        frame_length_base: 2_048,
    }),
    Some(FrameRate {
        numerator: 30_000,
        denominator: 1_001,
        frame_length_base: 1_536,
    }),
    Some(FrameRate {
        numerator: 30,
        denominator: 1,
        frame_length_base: 1_536,
    }),
    Some(FrameRate {
        numerator: 48_000,
        denominator: 1_001,
        frame_length_base: 960,
    }),
    Some(FrameRate {
        numerator: 48,
        denominator: 1,
        frame_length_base: 960,
    }),
    Some(FrameRate {
        numerator: 50,
        denominator: 1,
        frame_length_base: 1_024,
    }),
    Some(FrameRate {
        numerator: 60_000,
        denominator: 1_001,
        frame_length_base: 768,
    }),
    Some(FrameRate {
        numerator: 60,
        denominator: 1,
        frame_length_base: 768,
    }),
    Some(FrameRate {
        numerator: 100,
        denominator: 1,
        frame_length_base: 512,
    }),
    Some(FrameRate {
        numerator: 120_000,
        denominator: 1_001,
        frame_length_base: 384,
    }),
    Some(FrameRate {
        numerator: 120,
        denominator: 1,
        frame_length_base: 384,
    }),
    // 索引 13 的重采样比为 1，内部帧长与外部帧长相同
    Some(FrameRate {
        numerator: 48_000,
        denominator: 2_048,
        frame_length_base: 2_048,
    }),
    None,
    None,
];

/// 表 84：44,1 kHz 仅索引 13 有效。
const FRAME_RATE_44K1: [Option<FrameRate>; 16] = {
    let mut table = [None; 16];
    table[13] = Some(FrameRate {
        numerator: 11_025,
        denominator: 512,
        frame_length_base: 2_048,
    });
    table
};

/// 查询帧率信息。
///
/// 保留值与该采样率下未定义的组合返回 `None`。
#[must_use]
pub fn frame_rate(base: BaseSamplingFrequency, frame_rate_index: u8) -> Option<FrameRate> {
    let table = match base {
        BaseSamplingFrequency::Hz48000 => &FRAME_RATE_48K,
        BaseSamplingFrequency::Hz44100 => &FRAME_RATE_44K1,
    };
    table.get(usize::from(frame_rate_index)).copied().flatten()
}

/// 媒体时间轴上每个 sample 的时长，对应 `E.1` 表 E.1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleDelta {
    /// 每个 sample 时长相同。
    Constant(u32),
    /// 时长在两个值之间交替，见表 E.1 的 NOTE 2。
    ///
    /// 单独取任一值都无法正确推算时间轴，必须逐 sample 从 `stts` 读取。
    Alternating(u32, u32),
}

/// 媒体时间轴参数，对应表 E.1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaTimeline {
    /// `mdhd` 中应设置的时间刻度。
    pub timescale: u32,
    /// 每 sample 的时长。
    pub sample_delta: SampleDelta,
}

/// 查询表 E.1 给出的媒体时间轴参数。
///
/// 索引 3 在 48 kHz 下有两种合法选择：时间刻度 240 000 配恒定
/// `sample_delta`，或时间刻度 48 000 配交替值。此处返回后者，因为它是
/// 与其余索引一致的 48 000 时间刻度；调用方仍应以容器中 `mdhd` 的实际
/// 值为准，本函数只用于交叉核对。
#[must_use]
pub fn media_timeline(base: BaseSamplingFrequency, frame_rate_index: u8) -> Option<MediaTimeline> {
    use SampleDelta::{Alternating, Constant};
    let entry = match (base, frame_rate_index) {
        (BaseSamplingFrequency::Hz48000, 0) => (48_000, Constant(2_002)),
        (BaseSamplingFrequency::Hz48000, 1) => (48_000, Constant(2_000)),
        (BaseSamplingFrequency::Hz48000, 2) => (48_000, Constant(1_920)),
        (BaseSamplingFrequency::Hz48000, 3) => (48_000, Alternating(1_601, 1_602)),
        (BaseSamplingFrequency::Hz48000, 4) => (48_000, Constant(1_600)),
        (BaseSamplingFrequency::Hz48000, 5) => (48_000, Constant(1_001)),
        (BaseSamplingFrequency::Hz48000, 6) => (48_000, Constant(1_000)),
        (BaseSamplingFrequency::Hz48000, 7) => (48_000, Constant(960)),
        (BaseSamplingFrequency::Hz48000, 8) => (240_000, Constant(4_004)),
        (BaseSamplingFrequency::Hz48000, 9) => (48_000, Constant(800)),
        (BaseSamplingFrequency::Hz48000, 10) => (48_000, Constant(480)),
        (BaseSamplingFrequency::Hz48000, 11) => (240_000, Constant(2_002)),
        (BaseSamplingFrequency::Hz48000, 12) => (48_000, Constant(400)),
        (BaseSamplingFrequency::Hz48000, 13) => (48_000, Constant(2_048)),
        (BaseSamplingFrequency::Hz44100, 13) => (44_100, Constant(2_048)),
        _ => return None,
    };
    Some(MediaTimeline {
        timescale: entry.0,
        sample_delta: entry.1,
    })
}

/// `ac4_bitrate_dsi()` 的码率控制模式，对应 Part 2 表 E.8。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ac4BitrateMode {
    /// 未指定码率控制模式。
    Unspecified,
    /// 恒定码率。
    Constant,
    /// 平均码率。
    Average,
    /// 可变码率。
    Variable,
}

impl Ac4BitrateMode {
    const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Unspecified,
            1 => Self::Constant,
            2 => Self::Average,
            _ => Self::Variable,
        }
    }

    /// 码流中的 2 比特码值。
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Unspecified => 0,
            Self::Constant => 1,
            Self::Average => 2,
            Self::Variable => 3,
        }
    }
}

/// `ac4_bitrate_dsi()`，见 `TS103190-2:v1.3.1:E.7`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4BitrateDsi {
    /// 码率控制模式。
    pub mode: Ac4BitrateMode,
    /// 比特率，单位 bit/s；0 表示未知。
    pub bit_rate: u32,
    /// 比特率精度，单位 bit/s；`u32::MAX` 表示未知。
    pub precision: u32,
}

impl Ac4BitrateDsi {
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ReadError> {
        let mode = Ac4BitrateMode::from_code(reader.read_bits(2)? as u8);
        let bit_rate = reader.read_bits(32)? as u32;
        let precision = reader.read_bits(32)? as u32;
        Ok(Self {
            mode,
            bit_rate,
            precision,
        })
    }

    /// 精度字段是否使用规范的“未知”哨兵值。
    #[must_use]
    pub const fn precision_unknown(self) -> bool {
        self.precision == u32::MAX
    }
}

/// v1 DSI 中可选的节目标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4ProgramId {
    /// 本地唯一的 16 比特节目 ID。
    pub short_id: u16,
    /// 可选的 128 比特全局 UUID，按码流字节序保存。
    pub uuid: Option<[u8; 16]>,
}

/// 一个由 `presentation_version` 与 `pres_bytes` 定界的 DSI presentation。
///
/// 当前提交只负责可靠定界。`payload` 是恰好 `declared_bytes` 字节的内部 DSI，
/// 后续提交在此边界内解析 presentation v1 的语义字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiPresentation<'a> {
    /// 在 `ac4_dsi_v1()` 数组中的零基下标。
    pub index: u16,
    /// presentation DSI 语法版本。
    pub version: u8,
    /// `pres_bytes` 加可选 `add_pres_bytes` 后的声明长度。
    pub declared_bytes: u32,
    /// 由声明长度严格裁出的 presentation body。
    pub payload: &'a [u8],
}

/// `ac4_dsi_v1()` 的逐 presentation 迭代器。
#[derive(Debug, Clone)]
pub struct Ac4DsiPresentationIter<'a> {
    remaining: &'a [u8],
    remaining_count: u16,
    index: u16,
    failed: bool,
}

impl<'a> Ac4DsiPresentationIter<'a> {
    fn new(bytes: &'a [u8], count: u16) -> Self {
        Self {
            remaining: bytes,
            remaining_count: count,
            index: 0,
            failed: false,
        }
    }

    fn trailing_bytes(&self) -> usize {
        self.remaining.len()
    }
}

impl<'a> Iterator for Ac4DsiPresentationIter<'a> {
    type Item = Result<Ac4DsiPresentation<'a>, DsiError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining_count == 0 {
            return None;
        }

        let index = self.index;
        let mut reader = BitReader::new(self.remaining);
        let parsed = (|| {
            let version = reader.read_bits(8).map_err(DsiError::Truncated)? as u8;
            let initial = reader.read_bits(8).map_err(DsiError::Truncated)? as u32;
            let declared_bytes = if initial == 255 {
                initial.saturating_add(
                    u32::try_from(reader.read_bits(16).map_err(DsiError::Truncated)?)
                        .unwrap_or(u32::MAX),
                )
            } else {
                initial
            };

            let header_bytes = usize::try_from(reader.bit_position() / 8).unwrap_or(usize::MAX);
            let available = self.remaining.len().saturating_sub(header_bytes);
            let declared = usize::try_from(declared_bytes).unwrap_or(usize::MAX);
            if declared > available {
                return Err(DsiError::PresentationSizeOutOfRange {
                    index,
                    declared: declared_bytes,
                    available,
                });
            }
            let end =
                header_bytes
                    .checked_add(declared)
                    .ok_or(DsiError::PresentationSizeOutOfRange {
                        index,
                        declared: declared_bytes,
                        available,
                    })?;
            let payload = self.remaining.get(header_bytes..end).ok_or(
                DsiError::PresentationSizeOutOfRange {
                    index,
                    declared: declared_bytes,
                    available,
                },
            )?;
            self.remaining = self.remaining.get(end..).unwrap_or(&[]);
            Ok(Ac4DsiPresentation {
                index,
                version,
                declared_bytes,
                payload,
            })
        })();

        if parsed.is_err() {
            self.failed = true;
        } else {
            self.remaining_count = self.remaining_count.saturating_sub(1);
            self.index = self.index.saturating_add(1);
        }
        Some(parsed)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = if self.failed {
            0
        } else {
            usize::from(self.remaining_count)
        };
        (remaining, Some(remaining))
    }
}

/// `ac4_dsi_v1()` 固定节目与码率信息，以及长度已验证的 presentation 数组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4DsiV1<'a> {
    /// `b_program_id` 为真时存在。
    pub program_id: Option<Ac4ProgramId>,
    /// 整条 AC-4 流的码率摘要。
    pub bitrate: Ac4BitrateDsi,
    /// `ac4_bitrate_dsi()` 后的 `byte_align` 位数。
    pub align_bits: u8,
    n_presentations: u16,
    presentation_bytes: &'a [u8],
}

impl<'a> Ac4DsiV1<'a> {
    fn parse(
        bytes: &'a [u8],
        bitstream_version: u8,
        n_presentations: u16,
    ) -> Result<Self, DsiError> {
        let mut reader = BitReader::new(bytes);
        let program_id =
            if bitstream_version > 1 && reader.read_flag().map_err(DsiError::Truncated)? {
                let short_id = reader.read_bits(16).map_err(DsiError::Truncated)? as u16;
                let uuid = if reader.read_flag().map_err(DsiError::Truncated)? {
                    let mut value = [0u8; 16];
                    for byte in &mut value {
                        *byte = reader.read_bits(8).map_err(DsiError::Truncated)? as u8;
                    }
                    Some(value)
                } else {
                    None
                };
                Some(Ac4ProgramId { short_id, uuid })
            } else {
                None
            };

        let bitrate = Ac4BitrateDsi::parse(&mut reader).map_err(DsiError::Truncated)?;
        let align_bits =
            u8::try_from(reader.byte_align().map_err(DsiError::Truncated)?).unwrap_or(u8::MAX);
        let presentation_offset = usize::try_from(reader.bit_position() / 8).unwrap_or(usize::MAX);
        let presentation_bytes = bytes.get(presentation_offset..).ok_or(DsiError::Truncated(
            ReadError::OutOfBounds {
                requested_bits: 0,
                bit_position: reader.bit_position(),
                remaining_bits: reader.remaining_bits(),
            },
        ))?;

        let parsed = Self {
            program_id,
            bitrate,
            align_bits,
            n_presentations,
            presentation_bytes,
        };
        let mut presentations = parsed.presentations();
        for item in presentations.by_ref() {
            item?;
        }
        if presentations.trailing_bytes() != 0 {
            return Err(DsiError::TrailingBytes {
                remaining: presentations.trailing_bytes(),
            });
        }
        Ok(parsed)
    }

    /// 声明的 presentation 数量。
    #[must_use]
    pub const fn n_presentations(self) -> u16 {
        self.n_presentations
    }

    /// presentation 数组的原始字节，已排除 program、bitrate 与对齐位。
    #[must_use]
    pub const fn presentation_bytes(self) -> &'a [u8] {
        self.presentation_bytes
    }

    /// 按 DSI 自身顺序遍历 presentation；该顺序不要求与 TOC 相同。
    #[must_use]
    pub fn presentations(self) -> Ac4DsiPresentationIter<'a> {
        Ac4DsiPresentationIter::new(self.presentation_bytes, self.n_presentations)
    }
}

/// `dac4` 解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsiError {
    /// 数据不足以读出固定头部。
    Truncated(ReadError),
    /// `fs_index` 之外的保留取值，或该采样率下未定义的组合。
    UnsupportedSamplingFrequency {
        /// 读到的原始索引。
        fs_index: u8,
    },
    /// presentation 的长度声明越过 `dac4` box。
    PresentationSizeOutOfRange {
        /// DSI presentation 数组中的下标。
        index: u16,
        /// `pres_bytes` 与 `add_pres_bytes` 合成的声明值。
        declared: u32,
        /// presentation 头之后实际剩余的字节数。
        available: usize,
    },
    /// 声明的全部 presentation 之后仍有未归属字节。
    TrailingBytes {
        /// 未归属字节数。
        remaining: usize,
    },
}

impl fmt::Display for DsiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            DsiError::Truncated(error) => write!(f, "Truncated dac4 data: {error}"),
            DsiError::UnsupportedSamplingFrequency { fs_index } => {
                write!(f, "fs_index {fs_index} is undefined")
            }
            DsiError::PresentationSizeOutOfRange {
                index,
                declared,
                available,
            } => write!(
                f,
                "dac4 presentation {index} declares {declared} bytes, but only {available} remain"
            ),
            DsiError::TrailingBytes { remaining } => {
                write!(
                    f,
                    "{remaining} trailing bytes remain after dac4 presentations"
                )
            }
        }
    }
}

impl core::error::Error for DsiError {}

/// `ac4_dsi()`/`ac4_dsi_v1()` 的公共头部。
#[derive(Debug, Clone)]
pub struct Ac4Dsi<'a> {
    /// DSI 版本。Part 2 E.6 定义的当前版本为 1。
    pub dsi_version: u8,
    /// 比特流版本，应与 `ac4_toc` 中一致。
    pub bitstream_version: u8,
    /// 采样频率索引原值。
    pub fs_index: u8,
    /// 基础采样频率。
    pub base_sampling_frequency: BaseSamplingFrequency,
    /// 帧率索引原值。保留值不构成解析错误，只是查表得不到结果。
    pub frame_rate_index: u8,
    /// presentation 数量。
    pub n_presentations: u16,
    /// 固定 24 比特头之后的全部原始字节。
    ///
    /// v1 下它还包含 program 与 bitrate，不能直接当作 presentation 数组；
    /// 应通过 [`Ac4Dsi::v1`] 取得准确边界。保留本字段以兼容既有调用方。
    pub presentation_bytes: &'a [u8],
}

impl<'a> Ac4Dsi<'a> {
    /// 解析 `dac4` 的负载。
    ///
    /// # Errors
    ///
    /// 数据不足返回 [`DsiError::Truncated`]；`fs_index` 无法映射到已定义的
    /// 采样频率返回 [`DsiError::UnsupportedSamplingFrequency`]。
    pub fn parse(payload: &'a [u8]) -> Result<Self, DsiError> {
        let mut reader = BitReader::new(payload);
        let dsi_version = reader.read_bits(3).map_err(DsiError::Truncated)? as u8;
        let bitstream_version = reader.read_bits(7).map_err(DsiError::Truncated)? as u8;
        let fs_index = reader.read_bits(1).map_err(DsiError::Truncated)? as u8;
        let frame_rate_index = reader.read_bits(4).map_err(DsiError::Truncated)? as u8;
        let n_presentations = reader.read_bits(9).map_err(DsiError::Truncated)? as u16;

        let base_sampling_frequency = BaseSamplingFrequency::from_index(fs_index)
            .ok_or(DsiError::UnsupportedSamplingFrequency { fs_index })?;

        // 固定头部共 24 比特，正好字节对齐
        let consumed = usize::try_from(reader.bit_position() / 8).unwrap_or(usize::MAX);
        let presentation_bytes = payload.get(consumed..).unwrap_or(&[]);

        if dsi_version == 1 {
            Ac4DsiV1::parse(presentation_bytes, bitstream_version, n_presentations)?;
        }

        Ok(Self {
            dsi_version,
            bitstream_version,
            fs_index,
            base_sampling_frequency,
            frame_rate_index,
            n_presentations,
            presentation_bytes,
        })
    }

    /// 查表得到的帧率，保留索引返回 `None`。
    #[must_use]
    pub fn frame_rate(&self) -> Option<FrameRate> {
        frame_rate(self.base_sampling_frequency, self.frame_rate_index)
    }

    /// 查表得到的媒体时间轴参数，保留索引返回 `None`。
    #[must_use]
    pub fn media_timeline(&self) -> Option<MediaTimeline> {
        media_timeline(self.base_sampling_frequency, self.frame_rate_index)
    }

    /// 取得 Part 2 定义的 v1 DSI。
    ///
    /// 版本 0 或大于 1 时返回 `Ok(None)`；规范要求遇到大于 1 的版本停止解析
    /// box 剩余内容。`Ac4Dsi::parse` 已验证过 v1，本方法仍返回 `Result`，避免
    /// 将该不变量转化为潜在 panic。
    pub fn v1(&self) -> Result<Option<Ac4DsiV1<'a>>, DsiError> {
        if self.dsi_version != 1 {
            return Ok(None);
        }
        Ac4DsiV1::parse(
            self.presentation_bytes,
            self.bitstream_version,
            self.n_presentations,
        )
        .map(Some)
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unusual_byte_groupings,
    reason = "测试位打包器容量固定，字面量按 DSI 字段边界分组"
)]
mod tests {
    extern crate std;

    use super::*;

    struct BitBuf {
        bytes: [u8; 512],
        bits: usize,
    }

    impl BitBuf {
        fn new() -> Self {
            Self {
                bytes: [0; 512],
                bits: 0,
            }
        }

        fn push_bits(&mut self, value: u64, width: usize) {
            for shift in (0..width).rev() {
                let bit = (value >> shift) & 1;
                if bit != 0 {
                    self.bytes[self.bits / 8] |= 1 << (7 - self.bits % 8);
                }
                self.bits += 1;
            }
        }

        fn push_bytes(&mut self, bytes: &[u8]) {
            for &byte in bytes {
                self.push_bits(u64::from(byte), 8);
            }
        }

        fn byte_align(&mut self) {
            while !self.bits.is_multiple_of(8) {
                self.push_bits(0, 1);
            }
        }

        fn as_slice(&self) -> &[u8] {
            &self.bytes[..self.bits.div_ceil(8)]
        }
    }

    fn push_header(buf: &mut BitBuf, dsi_version: u8, n_presentations: u16) {
        buf.push_bits(u64::from(dsi_version), 3);
        buf.push_bits(2, 7); // bitstream_version
        buf.push_bits(1, 1); // 48 kHz
        buf.push_bits(13, 4);
        buf.push_bits(u64::from(n_presentations), 9);
    }

    fn push_bitrate(buf: &mut BitBuf, mode: u8, bit_rate: u32, precision: u32) {
        buf.push_bits(u64::from(mode), 2);
        buf.push_bits(u64::from(bit_rate), 32);
        buf.push_bits(u64::from(precision), 32);
        buf.byte_align();
    }

    #[test]
    fn table_82_maps_fs_index() {
        assert_eq!(BaseSamplingFrequency::from_index(0).unwrap().hz(), 44_100);
        assert_eq!(BaseSamplingFrequency::from_index(1).unwrap().hz(), 48_000);
        assert_eq!(BaseSamplingFrequency::from_index(2), None);
    }

    #[test]
    fn table_83_frame_lengths() {
        let base = BaseSamplingFrequency::Hz48000;
        assert_eq!(frame_rate(base, 1).unwrap().frame_length_base, 1_920);
        assert_eq!(frame_rate(base, 2).unwrap().frame_length_base, 2_048);
        assert_eq!(frame_rate(base, 13).unwrap().frame_length_base, 2_048);
        // 非整数帧率保持精确有理数
        let ntsc = frame_rate(base, 0).unwrap();
        assert_eq!((ntsc.numerator, ntsc.denominator), (24_000, 1_001));
        assert_eq!(frame_rate(base, 14), None, "保留索引无定义");
        assert_eq!(frame_rate(base, 15), None);
    }

    #[test]
    fn table_84_only_defines_index_13() {
        let base = BaseSamplingFrequency::Hz44100;
        for index in 0..13 {
            assert_eq!(
                frame_rate(base, index),
                None,
                "44,1 kHz 下索引 {index} 为保留"
            );
        }
        assert_eq!(frame_rate(base, 13).unwrap().frame_length_base, 2_048);
        assert_eq!(frame_rate(base, 14), None);
    }

    #[test]
    fn table_e1_sample_delta() {
        let base = BaseSamplingFrequency::Hz48000;
        assert_eq!(
            media_timeline(base, 1).unwrap(),
            MediaTimeline {
                timescale: 48_000,
                sample_delta: SampleDelta::Constant(2_000)
            }
        );
        assert_eq!(
            media_timeline(base, 13).unwrap(),
            MediaTimeline {
                timescale: 48_000,
                sample_delta: SampleDelta::Constant(2_048)
            }
        );
        // 索引 8 与 11 使用 240 000 时间刻度
        assert_eq!(media_timeline(base, 8).unwrap().timescale, 240_000);
        assert_eq!(media_timeline(base, 11).unwrap().timescale, 240_000);
    }

    /// 表 E.1 的 NOTE 2：索引 3 在 48 000 时间刻度下 sample_delta 非恒定。
    /// 类型层面必须体现这一点，否则调用方会用单一值推算时间轴。
    #[test]
    fn index_3_sample_delta_is_not_constant() {
        let timeline = media_timeline(BaseSamplingFrequency::Hz48000, 3).unwrap();
        assert_eq!(
            timeline.sample_delta,
            SampleDelta::Alternating(1_601, 1_602)
        );
    }

    /// 内部帧长与媒体时间刻度单位不是同一个量，差一个重采样比。
    #[test]
    fn frame_length_differs_from_sample_delta() {
        let base = BaseSamplingFrequency::Hz48000;
        // 24 fps：内部 1920，时间轴 2000（比值 25/24）
        assert_eq!(frame_rate(base, 1).unwrap().frame_length_base, 1_920);
        assert_eq!(
            media_timeline(base, 1).unwrap().sample_delta,
            SampleDelta::Constant(2_000)
        );
        // 索引 13：重采样比为 1，两者相同
        assert_eq!(frame_rate(base, 13).unwrap().frame_length_base, 2_048);
        assert_eq!(
            media_timeline(base, 13).unwrap().sample_delta,
            SampleDelta::Constant(2_048)
        );
    }

    #[test]
    fn parses_fixed_header() {
        // dsi_version=0, bitstream_version=2, fs_index=1, frame_rate_index=13,
        // n_presentations=1
        // 000 0000010 1 1101 000000001
        let payload = [0b000_00000, 0b10_1_1101_0, 0b00000001, 0xAA];
        let dsi = Ac4Dsi::parse(&payload).unwrap();
        assert_eq!(dsi.dsi_version, 0);
        assert_eq!(dsi.bitstream_version, 2);
        assert_eq!(dsi.fs_index, 1);
        assert_eq!(dsi.base_sampling_frequency, BaseSamplingFrequency::Hz48000);
        assert_eq!(dsi.frame_rate_index, 13);
        assert_eq!(dsi.n_presentations, 1);
        assert_eq!(dsi.presentation_bytes, &[0xAA], "固定头部占满 3 字节");
    }

    #[test]
    fn rejects_truncated_payload() {
        assert!(matches!(
            Ac4Dsi::parse(&[0x00, 0x01]).unwrap_err(),
            DsiError::Truncated(_)
        ));
    }

    #[test]
    fn parses_v1_program_bitrate_and_presentation_envelope() {
        let mut payload = BitBuf::new();
        push_header(&mut payload, 1, 1);
        payload.push_bits(1, 1); // b_program_id
        payload.push_bits(0x1234, 16);
        payload.push_bits(1, 1); // b_uuid
        payload.push_bytes(&[0xa5; 16]);
        push_bitrate(&mut payload, 2, 768_000, 1_000);
        payload.push_bits(1, 8); // presentation_version
        payload.push_bits(3, 8); // pres_bytes
        payload.push_bytes(&[0xaa, 0xbb, 0xcc]);

        let dsi = Ac4Dsi::parse(payload.as_slice()).unwrap();
        let v1 = dsi.v1().unwrap().unwrap();
        assert_eq!(
            v1.program_id,
            Some(Ac4ProgramId {
                short_id: 0x1234,
                uuid: Some([0xa5; 16]),
            })
        );
        assert_eq!(v1.bitrate.mode, Ac4BitrateMode::Average);
        assert_eq!(v1.bitrate.bit_rate, 768_000);
        assert_eq!(v1.bitrate.precision, 1_000);
        assert_eq!(v1.align_bits, 4);
        assert_eq!(v1.n_presentations(), 1);

        let presentations = v1
            .presentations()
            .collect::<Result<std::vec::Vec<_>, _>>()
            .unwrap();
        assert_eq!(presentations.len(), 1);
        assert_eq!(presentations[0].index, 0);
        assert_eq!(presentations[0].version, 1);
        assert_eq!(presentations[0].declared_bytes, 3);
        assert_eq!(presentations[0].payload, &[0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn parses_extended_presentation_size() {
        let mut payload = BitBuf::new();
        push_header(&mut payload, 1, 1);
        payload.push_bits(0, 1); // b_program_id
        push_bitrate(&mut payload, 3, 0, u32::MAX);
        payload.push_bits(9, 8); // 未知 presentation 版本仍由外层准确跳过
        payload.push_bits(255, 8);
        payload.push_bits(1, 16); // 255 + 1 bytes
        payload.push_bytes(&[0x5a; 256]);

        let dsi = Ac4Dsi::parse(payload.as_slice()).unwrap();
        let presentation = dsi
            .v1()
            .unwrap()
            .unwrap()
            .presentations()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(presentation.version, 9);
        assert_eq!(presentation.declared_bytes, 256);
        assert_eq!(presentation.payload, &[0x5a; 256]);
    }

    #[test]
    fn rejects_presentation_length_beyond_box() {
        let mut payload = BitBuf::new();
        push_header(&mut payload, 1, 1);
        payload.push_bits(0, 1);
        push_bitrate(&mut payload, 1, 64_000, 0);
        payload.push_bits(1, 8);
        payload.push_bits(2, 8);
        payload.push_bits(0xaa, 8);

        assert!(matches!(
            Ac4Dsi::parse(payload.as_slice()),
            Err(DsiError::PresentationSizeOutOfRange {
                index: 0,
                declared: 2,
                available: 1,
            })
        ));
    }

    #[test]
    fn rejects_bytes_after_declared_presentations() {
        let mut payload = BitBuf::new();
        push_header(&mut payload, 1, 0);
        payload.push_bits(0, 1);
        push_bitrate(&mut payload, 0, 0, u32::MAX);
        payload.push_bits(0xff, 8);

        assert_eq!(
            Ac4Dsi::parse(payload.as_slice()).unwrap_err(),
            DsiError::TrailingBytes { remaining: 1 }
        );
    }

    #[test]
    fn newer_dsi_version_stops_before_unknown_body() {
        let mut payload = BitBuf::new();
        push_header(&mut payload, 2, 1);
        payload.push_bytes(&[0xff]);

        let dsi = Ac4Dsi::parse(payload.as_slice()).unwrap();
        assert_eq!(dsi.dsi_version, 2);
        assert_eq!(dsi.presentation_bytes, &[0xff]);
        assert!(dsi.v1().unwrap().is_none());
    }
}
