//! 各 substream info 共享的尾部与读取 helper。

use super::*;

/// 读取 `bitrate_indicator`，见 `TS103190-1:v1.4.1:表 90`。
///
/// 该字段是 3 比特或 5 比特：3 比特码的最低位恒为 0，为 1 时再读 2 比特。
pub(super) fn read_bitrate_indicator(reader: &mut BitReader<'_>) -> Result<u32, TopologyError> {
    let head = u32::try_from(reader.read_bits(3)?).unwrap_or(0);
    if head & 1 == 0 {
        return Ok(head);
    }
    let tail = u32::try_from(reader.read_bits(2)?).unwrap_or(0);
    Ok(head.checked_shl(2).unwrap_or(u32::MAX).saturating_add(tail))
}

/// 读取 `fs_index == 1` 时的采样频率乘子字段。
pub(super) fn read_sf_multiplier(
    reader: &mut BitReader<'_>,
    fs_index: u8,
) -> Result<Option<u8>, TopologyError> {
    if fs_index != 1 {
        return Ok(None);
    }
    if !reader.read_flag()? {
        return Ok(None);
    }
    Ok(Some(u8::from(reader.read_flag()?)))
}

/// 读取 `frame_rate_factor` 个 `b_audio_ndot`，全部为真才算无时间依赖。
///
/// ndot = no dependency over time，见 `TS103190-2:v1.3.1:4.5.2`。一个
/// `ac4_substream_info` 元素可以指向多个连续解码的 substream（表 87），
/// 只要其中任何一个依赖前序帧，该元素整体就不能作为随机访问点。
pub(super) fn read_audio_ndot(
    reader: &mut BitReader<'_>,
    frame_rate_factor: u32,
) -> Result<bool, TopologyError> {
    let mut all = true;
    for _ in 0..frame_rate_factor {
        all &= reader.read_flag()?;
    }
    Ok(all)
}

/// 各 substream_info 共有的尾部：码率、无时间依赖标志与 substream 下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct SubstreamTail {
    pub(super) sf_multiplier: Option<u8>,
    pub(super) bitrate_indicator: Option<u32>,
    pub(super) audio_ndot: bool,
    pub(super) substream_index: Option<u32>,
}

impl SubstreamTail {
    pub(super) fn parse(
        reader: &mut BitReader<'_>,
        fs_index: u8,
        frame_rate_factor: u32,
        substreams_present: bool,
    ) -> Result<Self, TopologyError> {
        let sf_multiplier = read_sf_multiplier(reader, fs_index)?;
        let bitrate_indicator = if reader.read_flag()? {
            Some(read_bitrate_indicator(reader)?)
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
            sf_multiplier,
            bitrate_indicator,
            audio_ndot,
            substream_index,
        })
    }

    pub(super) const fn configuration_copy(&self) -> Self {
        Self {
            audio_ndot: false,
            ..*self
        }
    }

    /// 表 89 的 substream 采样频率乘子。
    pub(super) const fn sampling_frequency_multiplier(&self) -> u32 {
        match self.sf_multiplier {
            None => 1,
            Some(0) => 2,
            Some(_) => 4,
        }
    }
}
