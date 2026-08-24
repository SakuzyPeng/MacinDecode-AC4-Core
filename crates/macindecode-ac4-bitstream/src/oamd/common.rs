//! OAMD timing、trim、headphone、bed 与公共数据。

use super::*;

/// `sample_offset` 的来源，见表 92。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleOffsetSource {
    /// `0b0`：偏移固定为 0。
    Implicit,
    /// `0b10`：由 `oa_sample_offset_code` 给出，见表 93。
    Code,
    /// `0b11`：由 5 比特 `oa_sample_offset` 直接给出。
    Explicit,
}

/// `ramp_duration` 的原始编码路径。
///
/// `ramp_duration_code == 0b11` 时，表索引与 11 比特显式值可能解释为相同
/// 采样数；只保存解释结果无法恢复码流实际采用的路径。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RampDurationEncoding {
    /// `ramp_duration_code == 0b00`。
    #[default]
    Zero,
    /// `ramp_duration_code == 0b01`，固定为 512 个采样。
    Fixed512,
    /// `ramp_duration_code == 0b10`，固定为 1 536 个采样。
    Fixed1536,
    /// `ramp_duration_code == 0b11`，随后传输表 95 的四比特索引。
    Table {
        /// 表 95 的零基索引。
        index: u8,
    },
    /// `ramp_duration_code == 0b11`，随后直接传输 11 比特采样数。
    Explicit {
        /// 码流中的 11 比特值。
        value: u16,
    },
}

/// 单个 `object_info_block` 的时间信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObjInfoBlockTiming {
    /// `block_offset_factor` 原始码值。
    pub block_offset_factor: u8,
    /// `ramp_duration_code` 原始码值，见表 94。
    pub ramp_duration_code: u8,
    /// 表路径、显式路径及其原始索引或码值。
    pub ramp_duration_encoding: RampDurationEncoding,
    /// 解释后的 `ramp_duration`，单位为音频采样；表路径最大为 2 048，
    /// 11 比特显式路径最大为 2 047。
    pub ramp_duration: u16,
}

impl ObjInfoBlockTiming {
    /// 该块相对 `sample_offset` 的更新位置，单位为音频采样。
    ///
    /// 按 `4.8.3.4.1`：`update_sample = sample_offset + 32 × block_offset_factor`。
    /// 此处只给出后一项。
    #[must_use]
    pub const fn offset_samples(&self) -> u32 {
        (self.block_offset_factor as u32).saturating_mul(32)
    }
}

/// 表 95 的 `ramp_duration_table`，单位为音频采样。
///
/// 表中最大值 2 048 略超 `6.3.9.3.8` 为 11 比特 `ramp_duration` 元素声明的
/// `[0, 2047]` 范围；该范围约束的是直接编码的元素，不是本表。
const RAMP_DURATION_TABLE: [u16; 16] = [
    32, 64, 128, 256, 320, 480, 1000, 1001, 1024, 1600, 1601, 1602, 1920, 2000, 2002, 2048,
];

/// `oamd_timing_data()`，见 `6.2.8.2`。
///
/// 一个 `oamd_timing_data` 适用于整个 substream group（`6.3.9.3.1`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdTimingData {
    /// `sample_offset` 的编码方式。
    pub offset_source: SampleOffsetSource,
    /// 解释后的 `sample_offset`，单位为音频采样。
    pub sample_offset: u16,
    /// 本帧每个对象携带的 `object_info_block` 数。
    pub num_obj_info_blocks: u8,
    blocks: [ObjInfoBlockTiming; MAX_OBJ_INFO_BLOCKS],
}

impl OamdTimingData {
    /// 各 `object_info_block` 的时间信息。
    #[must_use]
    pub fn blocks(&self) -> &[ObjInfoBlockTiming] {
        self.blocks
            .get(..usize::from(self.num_obj_info_blocks))
            .unwrap_or(&[])
    }

    /// 解析 `oamd_timing_data()`。
    ///
    /// # Errors
    ///
    /// 读取越界返回 [`OamdError::Read`]；块数超出容量返回
    /// [`OamdError::TooManyBlocks`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, OamdError> {
        // oa_sample_offset_type 是 1/2 比特前缀码，见表 92。
        let (offset_source, sample_offset) = if reader.read_flag()? {
            if reader.read_flag()? {
                (
                    SampleOffsetSource::Explicit,
                    u16::try_from(reader.read_bits(5)?).unwrap_or(u16::MAX),
                )
            } else {
                // oa_sample_offset_code 同样是 1/2 比特前缀码，见表 93。
                let offset = if reader.read_flag()? {
                    if reader.read_flag()? { 24 } else { 8 }
                } else {
                    16
                };
                (SampleOffsetSource::Code, offset)
            }
        } else {
            (SampleOffsetSource::Implicit, 0)
        };

        let declared = u32::try_from(reader.read_bits(3)?).unwrap_or(u32::MAX);
        let count = usize::try_from(declared).unwrap_or(usize::MAX);
        if count > MAX_OBJ_INFO_BLOCKS {
            return Err(OamdError::TooManyBlocks { declared });
        }

        let mut blocks = [ObjInfoBlockTiming::default(); MAX_OBJ_INFO_BLOCKS];
        for slot in blocks.get_mut(..count).unwrap_or(&mut []) {
            let block_offset_factor = u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX);
            let ramp_duration_code = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
            let (ramp_duration_encoding, ramp_duration) = match ramp_duration_code {
                0 => (RampDurationEncoding::Zero, 0),
                1 => (RampDurationEncoding::Fixed512, 512),
                2 => (RampDurationEncoding::Fixed1536, 1_536),
                _ => {
                    if reader.read_flag()? {
                        let index = u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX);
                        let duration = RAMP_DURATION_TABLE
                            .get(usize::from(index))
                            .copied()
                            .unwrap_or(0);
                        (RampDurationEncoding::Table { index }, duration)
                    } else {
                        let value = u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX);
                        (RampDurationEncoding::Explicit { value }, value)
                    }
                }
            };
            *slot = ObjInfoBlockTiming {
                block_offset_factor,
                ramp_duration_code,
                ramp_duration_encoding,
                ramp_duration,
            };
        }

        Ok(Self {
            offset_source,
            sample_offset,
            num_obj_info_blocks: u8::try_from(declared).unwrap_or(u8::MAX),
            blocks,
        })
    }
}

/// 自定义 trim 中的带符号 Y 轴平衡码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrimBalanceCode {
    /// 符号码；`false` 朝前，`true` 朝后。
    pub sign_code: bool,
    /// 四比特幅度码。
    pub amount: u8,
}

/// 单个输出配置的 trim 模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrimConfigMode {
    /// 沿用全局模式。
    #[default]
    Inherit,
    /// 使用规范默认 trim。
    Default,
    /// 禁用 trim。
    Disabled,
    /// 使用本配置携带的自定义值。
    Custom,
}

/// 单个输出配置的完整 trim 码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrimConfig {
    /// 本配置的模式。
    pub mode: TrimConfigMode,
    /// `trim_centre`。
    pub centre: Option<u8>,
    /// `trim_surround`。
    pub surround: Option<u8>,
    /// `trim_height`。
    pub height: Option<u8>,
    /// 顶/底平面的前后平衡。
    pub top_bottom_y: Option<TrimBalanceCode>,
    /// 听音平面的前后平衡。
    pub listener_y: Option<TrimBalanceCode>,
}

/// `trim()`，见 `6.2.8.9`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Trim {
    /// 是否传输了 trim 数据。
    pub present: bool,
    /// `warp_mode` 原始码值。
    pub warp_mode: u8,
    /// `global_trim_mode` 原始码值。
    pub global_trim_mode: u8,
    /// 九种输出配置的 trim 数据。
    pub configs: [TrimConfig; NUM_TRIM_CONFIGS],
}

impl Trim {
    /// 解析 `trim()` 并返回消耗的比特数。
    ///
    /// # Errors
    ///
    /// 读取越界时返回 [`OamdError::Read`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<(Self, u64), OamdError> {
        let start = reader.bit_position();
        let mut out = Self {
            present: reader.read_flag()?,
            ..Self::default()
        };
        if out.present {
            out.warp_mode = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
            let _reserved = reader.read_bits(2)?;
            out.global_trim_mode = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
            if out.global_trim_mode == 0b10 {
                for config in &mut out.configs {
                    if reader.read_flag()? {
                        config.mode = TrimConfigMode::Default;
                        continue;
                    }
                    if reader.read_flag()? {
                        config.mode = TrimConfigMode::Disabled;
                        continue;
                    }
                    config.mode = TrimConfigMode::Custom;
                    let presence = reader.read_bits(5)?;
                    if presence & 0b1_0000 != 0 {
                        config.centre = Some(u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX));
                    }
                    if presence & 0b0_1000 != 0 {
                        config.surround =
                            Some(u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX));
                    }
                    if presence & 0b0_0100 != 0 {
                        config.height = Some(u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX));
                    }
                    if presence & 0b0_0010 != 0 {
                        config.top_bottom_y = Some(TrimBalanceCode {
                            sign_code: reader.read_flag()?,
                            amount: u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX),
                        });
                    }
                    if presence & 0b0_0001 != 0 {
                        config.listener_y = Some(TrimBalanceCode {
                            sign_code: reader.read_flag()?,
                            amount: u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX),
                        });
                    }
                }
            }
        }
        Ok((out, reader.bit_position().saturating_sub(start)))
    }
}

/// `headphone()`，见 `6.2.8.9a`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Headphone {
    /// 是否传输了耳机渲染数据。
    pub present: bool,
    /// `hp_operation_mode` 原始码值。
    pub hp_operation_mode: u8,
    /// 是否禁用全部头部跟踪；仅在模式 `0b001`/`0b010` 下传输。
    pub head_track_disable_all: Option<bool>,
}

impl Headphone {
    /// 解析 `headphone()` 并返回消耗的比特数。
    ///
    /// # Errors
    ///
    /// 读取越界时返回 [`OamdError::Read`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<(Self, u64), OamdError> {
        let start = reader.bit_position();
        let mut out = Self {
            present: reader.read_flag()?,
            ..Self::default()
        };
        if out.present {
            out.hp_operation_mode = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
            if matches!(out.hp_operation_mode, 0b001 | 0b010) {
                out.head_track_disable_all = Some(reader.read_flag()?);
            }
        }
        Ok((out, reader.bit_position().saturating_sub(start)))
    }
}

/// `stereo_dmx_coeff()` 的原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StereoDownmixCoefficients {
    pub loro_centre: u8,
    pub loro_surround: u8,
    pub ltrt_centre: Option<u8>,
    pub ltrt_surround: Option<u8>,
    pub lfe_mixgain: Option<u8>,
    pub preferred_method: u8,
}

pub(super) fn parse_stereo_dmx_coeff(
    reader: &mut BitReader<'_>,
) -> Result<StereoDownmixCoefficients, OamdError> {
    let loro_centre = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
    let loro_surround = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
    let (ltrt_centre, ltrt_surround) = if reader.read_flag()? {
        (
            Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX)),
            Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX)),
        )
    } else {
        (None, None)
    };
    let lfe_mixgain = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX))
    } else {
        None
    };
    let preferred_method = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
    Ok(StereoDownmixCoefficients {
        loro_centre,
        loro_surround,
        ltrt_centre,
        ltrt_surround,
        lfe_mixgain,
        preferred_method,
    })
}

/// `tool_t2_to_f_s_b()`，见 `6.2.9.9`。
///
/// 三个分支分别写入 `gain_t2a_code`、`gain_t2b_code` 与 `gain_t2c_code`，宽度
/// 同为 3 比特，因此按比特消耗可合并：`b_top_to_front` 为假时再读
/// `b_top_to_side`，随后一律是 3 比特增益码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopToFrontSideBackTool {
    pub top_to_front: bool,
    pub top_to_side: Option<bool>,
    pub gain_code: u8,
}

pub(super) fn parse_tool_t2_to_f_s_b(
    reader: &mut BitReader<'_>,
) -> Result<TopToFrontSideBackTool, OamdError> {
    let top_to_front = reader.read_flag()?;
    let top_to_side = if top_to_front {
        None
    } else {
        Some(reader.read_flag()?)
    };
    let gain_code = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
    Ok(TopToFrontSideBackTool {
        top_to_front,
        top_to_side,
        gain_code,
    })
}

/// `tool_t2_to_f_s()`，见 `6.2.9.10`。`tool_tb_*`/`tool_tf_*` 结构相同。
///
/// 两个分支的增益码同为 3 比特，按比特消耗合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopToFrontSideTool {
    pub top_to_front: bool,
    pub gain_code: u8,
}

pub(super) fn parse_tool_t2_to_f_s(
    reader: &mut BitReader<'_>,
) -> Result<TopToFrontSideTool, OamdError> {
    Ok(TopToFrontSideTool {
        top_to_front: reader.read_flag()?,
        gain_code: u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX),
    })
}

/// 一对 `b_cdmx_*_to_f_s_b` / `b_cdmx_*_to_f_s` 开关及其工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelDownmixPair {
    pub front_side_back: Option<TopToFrontSideBackTool>,
    pub front_side: Option<TopToFrontSideTool>,
}

pub(super) fn parse_cdmx_pair(reader: &mut BitReader<'_>) -> Result<ChannelDownmixPair, OamdError> {
    let front_side_back = if reader.read_flag()? {
        Some(parse_tool_t2_to_f_s_b(reader)?)
    } else {
        None
    };
    let front_side = if reader.read_flag()? {
        Some(parse_tool_t2_to_f_s(reader)?)
    } else {
        None
    };
    Ok(ChannelDownmixPair {
        front_side_back,
        front_side,
    })
}

/// `bed_render_info()`，见 `6.2.8.8`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BedRenderInfo {
    /// 是否传输了床渲染信息。
    pub present: bool,
    /// 可选立体声下混系数。
    pub stereo: Option<StereoDownmixCoefficients>,
    /// W 到前方的增益码。
    pub gain_w_to_f: Option<u8>,
    /// B4 到 B2 的增益码。
    pub gain_b4_to_b2: Option<u8>,
    /// Tm 的条件下混工具。
    pub tm: Option<ChannelDownmixPair>,
    /// Tb 的条件下混工具。
    pub tb: Option<ChannelDownmixPair>,
    /// Tf 的条件下混工具。
    pub tf: Option<ChannelDownmixPair>,
    /// Tfb 到 Tm 的增益码。
    pub gain_tfb_to_tm: Option<u8>,
}

impl BedRenderInfo {
    /// 解析 `bed_render_info()` 并返回消耗的比特数。
    ///
    /// # Errors
    ///
    /// 读取越界时返回 [`OamdError::Read`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<(Self, u64), OamdError> {
        let start = reader.bit_position();
        let mut out = Self {
            present: reader.read_flag()?,
            ..Self::default()
        };
        if out.present {
            if reader.read_flag()? {
                out.stereo = Some(parse_stereo_dmx_coeff(reader)?);
            }
            if reader.read_flag()? {
                if reader.read_flag()? {
                    out.gain_w_to_f = Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX));
                }
                if reader.read_flag()? {
                    out.gain_b4_to_b2 = Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX));
                }
                let tm_present = reader.read_flag()?;
                if tm_present {
                    out.tm = Some(parse_cdmx_pair(reader)?);
                }
                let tb_present = reader.read_flag()?;
                if tb_present {
                    out.tb = Some(parse_cdmx_pair(reader)?);
                }
                let tf_present = reader.read_flag()?;
                if tf_present {
                    out.tf = Some(parse_cdmx_pair(reader)?);
                }
                if (tb_present || tf_present) && reader.read_flag()? {
                    out.gain_tfb_to_tm =
                        Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX));
                }
            }
        }
        Ok((out, reader.bit_position().saturating_sub(start)))
    }
}

/// `oamd_common_data()`，见 `6.2.8.1`。
///
/// 该元素既可能出现在 `oamd_substream()`，也可能出现在
/// `ac4_substream_info_ajoc()`（表 7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OamdCommonData {
    /// 是否使用默认屏幕尺寸比例。
    pub default_screen_size_ratio: bool,
    /// `master_screen_size_ratio_code` 原始码值；使用默认值时不传输。
    pub master_screen_size_ratio_code: Option<u8>,
    /// 是否把床对象分配到声道。
    pub bed_object_chan_distribute: bool,
    /// 附加数据的字节数；未传输时为 `None`。
    pub add_data_bytes: Option<u32>,
    /// `trim()` 的结构性字段。
    pub trim: Trim,
    /// `bed_render_info()` 的结构性字段。
    pub bed_render_info: BedRenderInfo,
    /// `headphone()` 的结构性字段。
    pub headphone: Headphone,
}

impl OamdCommonData {
    /// 解析 `oamd_common_data()`。
    ///
    /// `add_data` 的长度由 `add_data_bytes` 给出，其中依次容纳 `trim()`、
    /// `bed_render_info()` 与 `headphone()`；每个子元素只在前一个解析后仍有
    /// 剩余比特时才存在。剩余比特作为保留数据跳过。
    ///
    /// # Errors
    ///
    /// 读取越界返回 [`OamdError::Read`]；子元素消耗超过声明字节数返回
    /// [`OamdError::AdditionalDataUnderflow`]。
    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, OamdError> {
        let default_screen_size_ratio = reader.read_flag()?;
        let master_screen_size_ratio_code = if default_screen_size_ratio {
            None
        } else {
            Some(u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX))
        };
        let bed_object_chan_distribute = reader.read_flag()?;

        let mut out = Self {
            default_screen_size_ratio,
            master_screen_size_ratio_code,
            bed_object_chan_distribute,
            add_data_bytes: None,
            trim: Trim::default(),
            bed_render_info: BedRenderInfo::default(),
            headphone: Headphone::default(),
        };

        if !reader.read_flag()? {
            return Ok(out);
        }

        // add_data_bytes_minus1 为 1 比特，值 2 时由 variable_bits(2) 扩展。
        let mut add_data_bytes = u32::try_from(reader.read_bits(1)?)
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        if add_data_bytes == 2 {
            add_data_bytes = reader.variable_bits_scaled_u32(2, add_data_bytes, 0)?;
        }
        out.add_data_bytes = Some(add_data_bytes);

        let total_bits = u64::from(add_data_bytes).saturating_mul(8);
        let mut used = 0u64;

        let (trim, bits) = Trim::parse(reader)?;
        out.trim = trim;
        used = used.saturating_add(bits);

        if used < total_bits {
            let (info, bits) = BedRenderInfo::parse(reader)?;
            out.bed_render_info = info;
            used = used.saturating_add(bits);
        }
        if used < total_bits {
            let (headphone, bits) = Headphone::parse(reader)?;
            out.headphone = headphone;
            used = used.saturating_add(bits);
        }

        let remaining = total_bits
            .checked_sub(used)
            .ok_or(OamdError::AdditionalDataUnderflow {
                declared_bytes: u64::from(add_data_bytes),
                used_bits: used,
            })?;
        reader.skip_bits(remaining)?;
        Ok(out)
    }
}
