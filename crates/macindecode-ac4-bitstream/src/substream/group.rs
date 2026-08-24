//! OAMD/content type 与 substream group。

use super::common::SubstreamTail;
use super::*;

/// `oamd_substream_info()`，见 `TS103190-2:v1.3.1:6.2.1.13`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OamdSubstream {
    /// OAMD 数据是否无时间依赖，见 `4.5.2` 的 `b_oamd_ndot`。
    pub ndot: bool,
    /// substream 索引表中的下标。
    pub substream_index: Option<u32>,
}

/// `content_type()`，见 `TS103190-1:v1.4.1:4.2.3.7`（表 10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentType {
    /// 内容分类码。
    pub content_classifier: u8,
    /// 是否携带语言标签。
    pub language_present: bool,
}

impl ContentType {
    pub(super) fn parse(reader: &mut BitReader<'_>) -> Result<Self, TopologyError> {
        let content_classifier = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
        let language_present = reader.read_flag()?;
        if language_present {
            if reader.read_flag()? {
                // 序列化标签：起始标志加 16 比特分片
                reader.read_flag()?;
                reader.read_bits(16)?;
            } else {
                let bytes = reader.read_bits(6)?;
                reader.skip_bits(bytes.saturating_mul(8))?;
            }
        }
        Ok(Self {
            content_classifier,
            language_present,
        })
    }
}

/// `ac4_substream_group_info()`，见 `6.2.1.6`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4SubstreamGroupInfo {
    /// 是否携带 substream 下标。
    pub substreams_present: bool,
    /// 是否存在高采样率扩展。
    pub hsf_ext: bool,
    /// 低频 substream 数。
    pub n_lf_substreams: u32,
    /// 是否为声道编码。
    pub channel_coded: bool,
    /// 该 group 的音频 info 所引用的连续 substream 数。
    pub frame_rate_factor: u32,
    /// OAMD substream；`b_oamd_substream` 为假时不存在。
    pub oamd_substream: Option<OamdSubstream>,
    /// 内容类型。
    pub content_type: Option<ContentType>,
    substreams: [SubstreamInfo; MAX_LF_SUBSTREAMS],
    hsf_substream_indices: [Option<u32>; MAX_LF_SUBSTREAMS],
    written: usize,
}

impl Ac4SubstreamGroupInfo {
    /// 未解析状态的占位值，用于初始化固定容量数组。
    pub const EMPTY: Self = Self {
        substreams_present: false,
        hsf_ext: false,
        n_lf_substreams: 0,
        channel_coded: false,
        frame_rate_factor: 1,
        oamd_substream: None,
        content_type: None,
        substreams: [SubstreamInfo::Chan(SubstreamInfoChan {
            channel_mode: ChannelMode {
                codeword: 0,
                ch_mode: 0,
            },
            four_back_channels_present: None,
            centre_present: None,
            top_channels_present: None,
            add_ch_base: None,
            tail: SubstreamTail {
                sf_multiplier: None,
                bitrate_indicator: None,
                audio_ndot: false,
                substream_index: None,
            },
        }); MAX_LF_SUBSTREAMS],
        hsf_substream_indices: [None; MAX_LF_SUBSTREAMS],
        written: 0,
    };

    /// 本 group 内的 substream。
    #[must_use]
    pub fn substreams(&self) -> &[SubstreamInfo] {
        self.substreams.get(..self.written).unwrap_or(&[])
    }

    /// 各低频 substream 对应的 HSF 扩展下标；没有扩展时元素为 `None`。
    #[must_use]
    pub fn hsf_substream_indices(&self) -> &[Option<u32>] {
        self.hsf_substream_indices
            .get(..self.written)
            .unwrap_or(&[])
    }

    /// 是否含 A-JOC 编码的 substream。
    #[must_use]
    pub fn has_ajoc(&self) -> bool {
        self.substreams()
            .iter()
            .any(|info| matches!(*info, SubstreamInfo::Ajoc(_)))
    }

    /// 是否含 direct-coded object substream。
    #[must_use]
    pub fn has_direct_object(&self) -> bool {
        self.substreams()
            .iter()
            .any(|info| matches!(*info, SubstreamInfo::Obj(_)))
    }

    /// 本 group 内的对象总数。
    #[must_use]
    pub fn n_objects(&self) -> u32 {
        self.substreams()
            .iter()
            .fold(0u32, |acc, info| acc.saturating_add(info.n_objects()))
    }

    /// 解析 `ac4_substream_group_info()`。
    ///
    /// # Errors
    ///
    /// 读取越界、substream 数超过 [`MAX_LF_SUBSTREAMS`]，或遇到保留的
    /// `channel_mode` 时返回错误。
    pub fn parse(
        reader: &mut BitReader<'_>,
        bitstream_version: u32,
        fs_index: u8,
        frame_rate_factor: u32,
    ) -> Result<Self, TopologyError> {
        let substreams_present = reader.read_flag()?;
        let hsf_ext = reader.read_flag()?;

        let n_lf_substreams = if reader.read_flag()? {
            1
        } else {
            let mut count = u32::try_from(reader.read_bits(2)?)
                .unwrap_or(0)
                .saturating_add(2);
            if count == 5 {
                count = reader.variable_bits_scaled_u32(2, count, 0)?;
            }
            count
        };

        let limit = u32::try_from(MAX_LF_SUBSTREAMS).unwrap_or(u32::MAX);
        if n_lf_substreams > limit {
            return Err(TopologyError::CapacityExceeded {
                what: Capacity::LfSubstreams,
                declared: n_lf_substreams,
                limit: MAX_LF_SUBSTREAMS,
            });
        }

        let channel_coded = reader.read_flag()?;
        let mut out = Self {
            substreams_present,
            hsf_ext,
            n_lf_substreams,
            channel_coded,
            frame_rate_factor,
            ..Self::EMPTY
        };

        if channel_coded {
            for _ in 0..n_lf_substreams {
                let position = out.written;
                if bitstream_version == 1 {
                    reader.read_flag()?; // sus_ver
                }
                let info = SubstreamInfoChan::parse(
                    reader,
                    fs_index,
                    frame_rate_factor,
                    substreams_present,
                )?;
                out.push(SubstreamInfo::Chan(info), n_lf_substreams)?;
                if hsf_ext && substreams_present {
                    let index = read_substream_index(reader)?;
                    let slot = out.hsf_substream_indices.get_mut(position).ok_or(
                        TopologyError::CapacityExceeded {
                            what: Capacity::LfSubstreams,
                            declared: n_lf_substreams,
                            limit: MAX_LF_SUBSTREAMS,
                        },
                    )?;
                    *slot = Some(index);
                }
            }
        } else {
            out.oamd_substream = if reader.read_flag()? {
                let ndot = reader.read_flag()?;
                let substream_index = if substreams_present {
                    Some(read_substream_index(reader)?)
                } else {
                    None
                };
                Some(OamdSubstream {
                    ndot,
                    substream_index,
                })
            } else {
                None
            };

            for _ in 0..n_lf_substreams {
                let position = out.written;
                // 这一位是整个 M2 的判定点：A-JOC 还是 direct-coded object
                let info = if reader.read_flag()? {
                    SubstreamInfo::Ajoc(SubstreamInfoAjoc::parse(
                        reader,
                        fs_index,
                        frame_rate_factor,
                        substreams_present,
                    )?)
                } else {
                    SubstreamInfo::Obj(SubstreamInfoObj::parse(
                        reader,
                        fs_index,
                        frame_rate_factor,
                        substreams_present,
                    )?)
                };
                out.push(info, n_lf_substreams)?;
                if hsf_ext && substreams_present {
                    let index = read_substream_index(reader)?;
                    let slot = out.hsf_substream_indices.get_mut(position).ok_or(
                        TopologyError::CapacityExceeded {
                            what: Capacity::LfSubstreams,
                            declared: n_lf_substreams,
                            limit: MAX_LF_SUBSTREAMS,
                        },
                    )?;
                    *slot = Some(index);
                }
            }
        }

        out.content_type = if reader.read_flag()? {
            Some(ContentType::parse(reader)?)
        } else {
            None
        };

        Ok(out)
    }

    /// 用于配置代次比较的规范化副本；各 ndot 标志不属于解码器配置。
    pub(crate) fn configuration_copy(&self) -> Self {
        let mut out = *self;
        out.oamd_substream = out.oamd_substream.map(|mut oamd| {
            oamd.ndot = false;
            oamd
        });
        for info in out.substreams.iter_mut().take(out.written) {
            *info = info.configuration_copy();
        }
        out
    }

    pub(super) fn push(&mut self, info: SubstreamInfo, declared: u32) -> Result<(), TopologyError> {
        let slot =
            self.substreams
                .get_mut(self.written)
                .ok_or(TopologyError::CapacityExceeded {
                    what: Capacity::LfSubstreams,
                    declared,
                    limit: MAX_LF_SUBSTREAMS,
                })?;
        *slot = info;
        self.written = self.written.saturating_add(1);
        Ok(())
    }
}
