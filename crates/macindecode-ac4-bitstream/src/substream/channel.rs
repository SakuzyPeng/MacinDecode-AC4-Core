//! 声道模式与声道编码 substream info。

use super::common::{SubstreamTail, read_audio_ndot, read_bitrate_indicator, read_sf_multiplier};
use super::*;

/// `channel_mode`，见 `TS103190-2:v1.3.1:表 56`。
///
/// 该字段是 1、2、4、7、8 或 9 比特的前缀码。保留原始码字是因为后续的
/// `b_4_back_channels_present` 等条件判断针对的是码字本身而非 `ch_mode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelMode {
    /// 原始码字，按读取顺序左对齐为整数。
    pub codeword: u32,
    /// 表 56 中的 `ch_mode` 序号。
    pub ch_mode: u32,
}

impl ChannelMode {
    /// 声道模式的可读名称；保留取值返回 `None`。
    #[must_use]
    pub const fn label(&self) -> Option<&'static str> {
        Some(match self.ch_mode {
            0 => "mono",
            1 => "stereo",
            2 => "3.0",
            3 => "5.0",
            4 => "5.1",
            5 => "7.0_3/4/0",
            6 => "7.1_3/4/0.1",
            7 => "7.0_5/2/0",
            8 => "7.1_5/2/0.1",
            9 => "7.0_3/2/2",
            10 => "7.1_3/2/2.1",
            11 => "7.0.4",
            12 => "7.1.4",
            13 => "9.0.4",
            14 => "9.1.4",
            15 => "22.2",
            _ => return None,
        })
    }

    /// 解码 `channel_mode` 前缀码。
    ///
    /// # Errors
    ///
    /// 读取越界，或码字落在 `0b111111111` 之后的保留区间时返回错误。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, TopologyError> {
        if !reader.read_flag()? {
            return Ok(Self {
                codeword: 0b0,
                ch_mode: 0,
            });
        }
        if !reader.read_flag()? {
            return Ok(Self {
                codeword: 0b10,
                ch_mode: 1,
            });
        }
        // 已读入 0b11，接下来 2 比特区分 4 比特码与更长的码
        let four = reader.read_bits(2)?;
        match four {
            0b00 => {
                return Ok(Self {
                    codeword: 0b1100,
                    ch_mode: 2,
                });
            }
            0b01 => {
                return Ok(Self {
                    codeword: 0b1101,
                    ch_mode: 3,
                });
            }
            0b10 => {
                return Ok(Self {
                    codeword: 0b1110,
                    ch_mode: 4,
                });
            }
            _ => {}
        }

        // 已读入 0b1111
        let seven = reader.read_bits(3)?;
        match seven {
            0b000..=0b101 => {
                let ch_mode = u32::try_from(seven).unwrap_or(0).saturating_add(5);
                let codeword = 0b1111000u32.saturating_add(u32::try_from(seven).unwrap_or(0));
                return Ok(Self { codeword, ch_mode });
            }
            0b110 => {
                // 8 比特码 0b11111100 / 0b11111101
                let low = reader.read_flag()?;
                return Ok(Self {
                    codeword: if low { 0b11111101 } else { 0b11111100 },
                    ch_mode: if low { 12 } else { 11 },
                });
            }
            _ => {}
        }

        // 已读入 0b1111111，9 比特码
        let tail = reader.read_bits(2)?;
        let ch_mode = u32::try_from(tail).unwrap_or(0).saturating_add(13);
        let codeword = 0b111111100u32.saturating_add(u32::try_from(tail).unwrap_or(0));
        if tail == 0b11 {
            // 0b111111111 为保留，后接 variable_bits(2)
            let position = reader.bit_position();
            let ch_mode = reader.variable_bits_scaled_u32(2, ch_mode, 0)?;
            return Err(TopologyError::Unsupported {
                what: Unsupported::ReservedChannelMode { ch_mode },
                bit_position: position,
            });
        }
        Ok(Self { codeword, ch_mode })
    }
}

/// `ac4_substream_info_chan()`，见 `6.2.1.8`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubstreamInfoChan {
    /// 声道模式。
    pub channel_mode: ChannelMode,
    /// 后置声道是否携带原始内容。
    pub four_back_channels_present: Option<bool>,
    /// 中置声道是否携带原始内容。
    pub centre_present: Option<bool>,
    /// 顶部声道的携带方式。
    pub top_channels_present: Option<u8>,
    /// 附加声道的 A-CPL 耦合基准。
    pub add_ch_base: Option<bool>,
    pub(super) tail: SubstreamTail,
}

impl SubstreamInfoChan {
    /// substream 索引表中的下标。
    #[must_use]
    pub const fn substream_index(&self) -> Option<u32> {
        self.tail.substream_index
    }

    /// 该 substream 是否无时间依赖，可独立于前序帧解码。
    #[must_use]
    pub const fn audio_ndot(&self) -> bool {
        self.tail.audio_ndot
    }

    pub(super) fn parse(
        reader: &mut BitReader<'_>,
        fs_index: u8,
        frame_rate_factor: u32,
        substreams_present: bool,
    ) -> Result<Self, TopologyError> {
        let channel_mode = ChannelMode::parse(reader)?;

        // 7.0.4/7.1.4/9.0.4/9.1.4 额外声明各声道是否真的携带内容
        let extended = matches!(
            channel_mode.codeword,
            0b11111100 | 0b11111101 | 0b111111100 | 0b111111101
        );
        let (four_back, centre, top) = if extended {
            (
                Some(reader.read_flag()?),
                Some(reader.read_flag()?),
                Some(u8::try_from(reader.read_bits(2)?).unwrap_or(0)),
            )
        } else {
            (None, None, None)
        };

        let sf_multiplier = read_sf_multiplier(reader, fs_index)?;
        let bitrate_indicator = if reader.read_flag()? {
            Some(read_bitrate_indicator(reader)?)
        } else {
            None
        };

        // add_ch_base 只在带 Lw/Rw 或 Tfl/Tfr 的 7 声道模式下出现，且位置在
        // bitrate_indicator 之后，不能并入通用尾部
        let add_ch_base = if matches!(channel_mode.codeword, 0b1111010..=0b1111101) {
            Some(reader.read_flag()?)
        } else {
            None
        };

        let audio_ndot = read_audio_ndot(reader, frame_rate_factor)?;
        let substream_index = if substreams_present {
            Some(read_substream_index(reader)?)
        } else {
            None
        };

        Ok(Self {
            channel_mode,
            four_back_channels_present: four_back,
            centre_present: centre,
            top_channels_present: top,
            add_ch_base,
            tail: SubstreamTail {
                sf_multiplier,
                bitrate_indicator,
                audio_ndot,
                substream_index,
            },
        })
    }
}
