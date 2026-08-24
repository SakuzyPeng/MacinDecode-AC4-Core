//! `AC4SpecificBox`（`dac4`）中的解码器专属信息。
//!
//! 对应 `TS103190-1:v1.4.1:E.4`（表 E.3、E.4、E.5）。
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
}

impl fmt::Display for DsiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            DsiError::Truncated(error) => write!(f, "Truncated dac4 data: {error}"),
            DsiError::UnsupportedSamplingFrequency { fs_index } => {
                write!(f, "fs_index {fs_index} is undefined")
            }
        }
    }
}

impl core::error::Error for DsiError {}

/// `ac4_dsi()` 的固定头部，对应表 E.4。
///
/// 后续的 `ac4_presentation_v0_dsi()` 是容器侧表示，不是 M2 的 TOC
/// 拓扑；在规范版本分支语义确认前，此处只保留原始字节。
#[derive(Debug, Clone)]
pub struct Ac4Dsi<'a> {
    /// DSI 版本。表 E.5 要求符合本规范的 DSI 取 `0b000`。
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
    /// 固定头部之后的剩余字节，含各 presentation 的 DSI。
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
}

#[cfg(test)]
#[expect(clippy::unusual_byte_groupings, reason = "字面量按 DSI 字段边界分组")]
mod tests {
    use super::*;

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
}
