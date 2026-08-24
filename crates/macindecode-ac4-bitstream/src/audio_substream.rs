//! `ac4_substream()` 的框架与 `metadata()`。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.2.2`（框架）、`6.2.7`（metadata），语义见
//! `TS103190-1:v1.4.1:4.3.4`、`4.3.12`。
//!
//! # 为什么能跳过音频数据
//!
//! `ac4_substream()` 以 `audio_size` 开头，`4.3.4.1` 规定它是 `audio_data` 连同
//! 其后 `fill_bits` 与 `byte_align` 的字节数，**不含 `metadata` 及最后的
//! `byte_align`**，并附 NOTE：「这使解码器无需解析音频数据即可直接访问
//! metadata」。本模块正是走这条路。
//!
//! 因此本模块**不解码音频**，也不触及频谱前端。A-JOC 路径下逐对象的 OAMD
//! 动态数据位于 `audio_data_ajoc` 内部（表 7），跳过音频数据同时也跳过了它们；
//! 取得那部分仍需完整的音频比特解析。
//!
//! # 判定性自检
//!
//! `ac4_substream()` 以 `byte_align` 结尾，且其总长由 `substream_index_table()`
//! 独立声明。因此「跳过 `audio_size` 字节，解析 `metadata()`，再对齐」之后的
//! 位置必须**恰好**落在 substream 末尾。任何一个可变长字段错位都会破坏该等式，
//! 且这次的约束是整字节级的，比 OAMD 的 `byte_align` 残余强得多。

use crate::emdf::EmdfPayloadsSubstream;
use crate::reader::{BitReader, ReadError};
use core::fmt;

/// 解析 `ac4_substream()` 失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSubstreamError {
    /// 底层读取失败。
    Read(ReadError),
    /// `audio_size` 超出该 substream 的实际长度。
    AudioSizeOutOfRange {
        /// 声明的音频数据字节数。
        audio_size: u64,
        /// substream 的总字节数。
        substream_len: u64,
    },
    /// `further_loudness_info()` 的扩展长度不足以容纳已声明字段。
    InvalidExtensionSize {
        /// `e_bits_size` 声明的扩展区段长度。
        declared: u32,
        /// 当前标志组合至少需要的长度。
        required: u32,
        /// 检测到不一致时的比特偏移。
        bit_position: u64,
    },
    /// 解析结束后未落在 substream 末尾。
    ///
    /// 这是本模块的主要判定条件，说明某个可变长字段解析错位。
    TrailingBits {
        /// 解析并对齐后剩余的比特数。
        remaining_bits: u64,
    },
    /// 遇到本实现尚未覆盖的分支。
    Unsupported {
        /// 分支名称。
        what: &'static str,
        /// 遇到时的比特偏移。
        bit_position: u64,
    },
}

impl fmt::Display for AudioSubstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            AudioSubstreamError::Read(error) => write!(f, "ac4_substream 读取失败：{error}"),
            AudioSubstreamError::AudioSizeOutOfRange {
                audio_size,
                substream_len,
            } => write!(
                f,
                "audio_size 为 {audio_size} 字节，但 substream 只有 {substream_len} 字节"
            ),
            AudioSubstreamError::InvalidExtensionSize {
                declared,
                required,
                bit_position,
            } => write!(
                f,
                "偏移 {bit_position} 处的 e_bits_size 为 {declared}，至少需要 {required} 比特"
            ),
            AudioSubstreamError::TrailingBits { remaining_bits } => write!(
                f,
                "metadata 解析后仍剩 {remaining_bits} 比特，未落在 substream 末尾"
            ),
            AudioSubstreamError::Unsupported { what, bit_position } => {
                write!(f, "偏移 {bit_position} 处遇到未覆盖的分支：{what}")
            }
        }
    }
}

impl core::error::Error for AudioSubstreamError {}

impl From<ReadError> for AudioSubstreamError {
    fn from(error: ReadError) -> Self {
        AudioSubstreamError::Read(error)
    }
}

/// 解析 `metadata()` 所需的上下文。
///
/// `channel_mode` 为 `None` 表示未由前置 info 元素设定。按 `6.2.2.2` 的 NOTE 2，
/// 此时它应视为负值，所有以 `channel_mode` 为条件的分支都不成立——A-JOC 与
/// 直接编码对象的 substream 正是这种情形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubstreamContext {
    /// `sus_ver`。`bitstream_version = 2` 下恒为 1（`6.3.2.5.4`）。
    pub sus_ver: u8,
    /// presentation 的 `b_alternative`。
    pub alternative: bool,
    /// 该 substream 是否 A-JOC 编码。
    pub ajoc: bool,
    /// `channel_mode`；未设定时为 `None`。
    pub channel_mode: Option<u32>,
}

// TS103190-2:v1.3.1:6.3.8.4（Pseudocode 30–36）。这些判据使用表 56 的
// `ch_mode`，不是可变长的原始 `channel_mode` 码字。
const fn channel_mode_contains_lfe(ch_mode: u32) -> bool {
    matches!(ch_mode, 4 | 6 | 8 | 10 | 12 | 14 | 15)
}

const fn channel_mode_contains_c(ch_mode: u32) -> bool {
    ch_mode == 0 || matches!(ch_mode, 2..=15)
}

const fn channel_mode_contains_lr(ch_mode: u32) -> bool {
    matches!(ch_mode, 1..=15)
}

const fn channel_mode_contains_ls_rs(ch_mode: u32) -> bool {
    matches!(ch_mode, 3..=15)
}

const fn channel_mode_contains_lb_rb(ch_mode: u32) -> bool {
    matches!(ch_mode, 5 | 6 | 11..=15)
}

const fn channel_mode_contains_lw_rw(ch_mode: u32) -> bool {
    matches!(ch_mode, 7 | 8 | 15)
}

const fn channel_mode_contains_tfl_tfr(ch_mode: u32) -> bool {
    matches!(ch_mode, 9 | 10)
}

// 以下两个不是 `6.3.8.4` 的查询函数。P2 `6.2.7.2` 使用 `5_X`/`7_X`
// 符号，等价的 P1 语法则给出了可落到 `ch_mode` 的边界：表 19 映射 5.0/5.1，
// `4.2.14.2` 表 67 直接限定 7_X 字段为 `5 <= ch_mode <= 10`。

/// `5_X`。取表 56 中名为 5.0/5.1 的两个模式。
///
/// `TS103190-1:v1.4.1:4.2.5` 表 19 的 `audio_data()` 用同一个
/// `5_X_channel_element` 分派 `case 5.0` 与 `case 5.1`，且表 56 里再无其他
/// 5.x 模式，故这两个就是全部。
const fn channel_mode_is_five_x(ch_mode: u32) -> bool {
    matches!(ch_mode, 3 | 4)
}

/// `7_X`。取表 56 中 5–10 这六个 7.0_*/7.1_* 模式。
///
/// `TS103190-1:v1.4.1:4.2.14.2` 表 67 直接以 `5 <= ch_mode <= 10`
/// 守卫 `b_upmixtyp_7ch`，并在内部把 5–6 分给 `pre_upmixtyp_3_4`、9–10 分给
/// `pre_upmixtyp_3_2_2`。因此 11–15 明确不属于这里的 `7_X`。
const fn channel_mode_is_seven_x(ch_mode: u32) -> bool {
    matches!(ch_mode, 5..=10)
}

/// `further_loudness_info()` 中的原始码值，见 `6.2.7.3`。
///
/// 全部保留量化码值，不换算为 LKFS。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FurtherLoudnessInfo {
    /// `loudrelgat`，11 比特。
    pub loudrelgat: Option<u16>,
    /// `loudspchgat`，11 比特，附带 `dialgate_prac_type`。
    pub loudspchgat: Option<(u16, u8)>,
    /// `loudstrm3s`，11 比特。
    pub loudstrm3s: Option<u16>,
    /// `max_loudstrm3s`，11 比特。
    pub max_loudstrm3s: Option<u16>,
    /// `truepk`，11 比特。
    pub truepk: Option<u16>,
    /// `max_truepk`，11 比特。
    pub max_truepk: Option<u16>,
    /// `lra`，10 比特，附带 `lra_prac_type`。
    pub lra: Option<(u16, u8)>,
    /// `loudmntry`，11 比特。
    pub loudmntry: Option<u16>,
    /// `max_loudmntry`，11 比特。
    pub max_loudmntry: Option<u16>,
    /// `rtll_comp`，8 比特；`sus_ver == 0` 时位于扩展区段内。
    pub rtll_comp: Option<u8>,
    /// 未解释的 `extensions_bits` 比特数。
    pub extension_bits: Option<u32>,
}

impl FurtherLoudnessInfo {
    /// 解析 `further_loudness_info(sus_ver, b_presentation_ldn)`。
    ///
    /// # Errors
    ///
    /// 读取越界时返回 [`AudioSubstreamError::Read`]。
    pub fn parse(
        reader: &mut BitReader<'_>,
        sus_ver: u8,
        presentation_ldn: bool,
    ) -> Result<Self, AudioSubstreamError> {
        let mut out = Self::default();

        if presentation_ldn || sus_ver == 0 {
            // loudness_version 取 3 时由 extended_loudness_version 扩展；
            // 两者都不影响后续比特消耗，只按语法读走。
            if reader.read_bits(2)? == 3 {
                let _extended_loudness_version = reader.read_bits(4)?;
            }
            let loud_prac_type = reader.read_bits(4)?;
            if loud_prac_type != 0 {
                if reader.read_flag()? {
                    let _dialgate_prac_type = reader.read_bits(3)?;
                }
                let _b_loudcorr_type = reader.read_flag()?;
            }
        } else {
            let _b_loudcorr_dialgate = reader.read_flag()?;
        }

        if reader.read_flag()? {
            out.loudrelgat = Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX));
        }
        if reader.read_flag()? {
            let value = u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX);
            let practice = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
            out.loudspchgat = Some((value, practice));
        }
        if reader.read_flag()? {
            out.loudstrm3s = Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX));
        }
        if reader.read_flag()? {
            out.max_loudstrm3s = Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX));
        }
        if reader.read_flag()? {
            out.truepk = Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX));
        }
        if reader.read_flag()? {
            out.max_truepk = Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX));
        }

        if (presentation_ldn || sus_ver == 0) && reader.read_flag()? {
            // prgmbndy 以一元编码给出 2 的幂次，读到首个 1 为止。
            loop {
                if reader.read_flag()? {
                    break;
                }
            }
            let _b_end_or_start = reader.read_flag()?;
            if reader.read_flag()? {
                let _prgmbndy_offset = reader.read_bits(11)?;
            }
        }

        if reader.read_flag()? {
            let value = u16::try_from(reader.read_bits(10)?).unwrap_or(u16::MAX);
            let practice = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
            out.lra = Some((value, practice));
        }
        if reader.read_flag()? {
            out.loudmntry = Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX));
        }
        if reader.read_flag()? {
            out.max_loudmntry = Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX));
        }

        if sus_ver >= 1 {
            if reader.read_flag()? {
                out.rtll_comp = Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX));
            }
            if reader.read_flag()? {
                let mut size = u32::try_from(reader.read_bits(5)?).unwrap_or(u32::MAX);
                if size == 31 {
                    size = reader.variable_bits_scaled_u32(4, size, 0)?;
                }
                out.extension_bits = Some(size);
                reader.skip_bits(u64::from(size))?;
            }
        } else if reader.read_flag()? {
            let mut size = u32::try_from(reader.read_bits(5)?).unwrap_or(u32::MAX);
            if size == 31 {
                size = reader.variable_bits_scaled_u32(4, size, 0)?;
            }

            // sus_ver == 0 时，b_rtllcomp 与 rtll_comp 被封装在 e_bits_size
            // 声明的扩展区段内；extension_bits 只表示其后的未知扩展位。
            let rtll_present = reader.read_flag()?;
            let required = if rtll_present { 9 } else { 1 };
            let extension_bits =
                size.checked_sub(required)
                    .ok_or(AudioSubstreamError::InvalidExtensionSize {
                        declared: size,
                        required,
                        bit_position: reader.bit_position(),
                    })?;
            if rtll_present {
                out.rtll_comp = Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX));
            }
            out.extension_bits = Some(extension_bits);
            reader.skip_bits(u64::from(extension_bits))?;
        }
        Ok(out)
    }
}

/// `basic_metadata()`，见 `6.2.7.2`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BasicMetadata {
    /// `dialnorm_bits`，7 比特；仅 `sus_ver == 0` 传输。
    ///
    /// `bitstream_version = 2` 下 `sus_ver` 恒为 1，因此该字段恒为 `None`。
    pub dialnorm_bits: Option<u8>,
    /// `substream_loudness_bits`，8 比特；仅 `sus_ver >= 1`。
    pub substream_loudness_bits: Option<u8>,
    /// 进一步的响度信息。
    pub further_loudness: Option<FurtherLoudnessInfo>,
    /// `dc_block_on`；`b_dc_blocking` 为假时不传输。
    pub dc_block_on: Option<bool>,
}

impl BasicMetadata {
    /// 解析 `basic_metadata(channel_mode, sus_ver)`。
    ///
    /// # Errors
    ///
    /// 读取越界返回 [`AudioSubstreamError::Read`]。
    pub fn parse(
        reader: &mut BitReader<'_>,
        context: SubstreamContext,
    ) -> Result<Self, AudioSubstreamError> {
        let mut out = Self::default();
        if context.sus_ver == 0 {
            out.dialnorm_bits = Some(u8::try_from(reader.read_bits(7)?).unwrap_or(u8::MAX));
        }
        if !reader.read_flag()? {
            return Ok(out);
        }

        if context.sus_ver == 0 {
            if reader.read_flag()? {
                out.further_loudness = Some(FurtherLoudnessInfo::parse(reader, 0, false)?);
            }
        } else if reader.read_flag()? {
            out.substream_loudness_bits =
                Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX));
            if reader.read_flag()? {
                out.further_loudness =
                    Some(FurtherLoudnessInfo::parse(reader, context.sus_ver, false)?);
            }
        }

        // channel_mode 未设定时按负值处理，所有声道模式条件均不成立
        //（6.2.2.2 NOTE 2），但末尾的 DC blocking 仍然传输。
        if let Some(ch_mode) = context.channel_mode {
            if ch_mode == 1 && reader.read_flag()? {
                let _pre_dmixtyp_2ch = reader.read_bits(3)?;
                let _phase90_info_2ch = reader.read_bits(2)?;
            }

            if ch_mode > 1 {
                // sus_ver == 0 沿用 Part 1 的完整 stereo downmix 信息；IMS 的
                // bitstream_version=2 取 sus_ver=1，因此不会进入本段。
                if context.sus_ver == 0 && reader.read_flag()? {
                    let _loro_centre_mixgain = reader.read_bits(3)?;
                    let _loro_surround_mixgain = reader.read_bits(3)?;
                    if reader.read_flag()? {
                        let _loro_dmx_loud_corr = reader.read_bits(5)?;
                    }
                    if reader.read_flag()? {
                        let _ltrt_centre_mixgain = reader.read_bits(3)?;
                        let _ltrt_surround_mixgain = reader.read_bits(3)?;
                    }
                    if reader.read_flag()? {
                        let _ltrt_dmx_loud_corr = reader.read_bits(5)?;
                    }
                    if channel_mode_contains_lfe(ch_mode) && reader.read_flag()? {
                        let _lfe_mixgain = reader.read_bits(5)?;
                    }
                    let _preferred_dmx_method = reader.read_bits(2)?;
                }

                if channel_mode_is_five_x(ch_mode) {
                    if reader.read_flag()? {
                        let _pre_dmixtyp_5ch = reader.read_bits(3)?;
                    }
                    if reader.read_flag()? {
                        let _pre_upmixtyp_5ch = reader.read_bits(4)?;
                    }
                }

                if channel_mode_is_seven_x(ch_mode) && reader.read_flag()? {
                    if matches!(ch_mode, 5 | 6) {
                        let _pre_upmixtyp_3_4 = reader.read_bits(2)?;
                    } else if matches!(ch_mode, 9 | 10) {
                        let _pre_upmixtyp_3_2_2 = reader.read_flag()?;
                    }
                }

                let _phase90_info_mc = reader.read_bits(2)?;
                let _surround_attenuation_known = reader.read_flag()?;
                let _lfe_attenuation_known = reader.read_flag()?;
            }
        }

        if reader.read_flag()? {
            out.dc_block_on = Some(reader.read_flag()?);
        }
        Ok(out)
    }
}

/// `extended_metadata()`，见 `6.2.7.4`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExtendedMetadata {
    /// `b_dialog`。
    pub dialog: bool,
    /// `dialog_max_gain`，2 比特。
    pub dialog_max_gain: Option<u8>,
    /// `pan_dialog[0]`、`pan_dialog[1]` 与 `pan_signal_selector`。
    pub pan_dialog: Option<(u8, u8, u8)>,
    /// mono 声道模式下的单个 `pan_dialog`。
    pub pan_dialog_mono: Option<u8>,
    /// `b_channels_classifier`。
    pub channels_classifier: bool,
    /// `event_probability`，4 比特。
    pub event_probability: Option<u8>,
}

impl ExtendedMetadata {
    /// 解析 `extended_metadata(channel_mode, sus_ver)`。
    ///
    /// # Errors
    ///
    /// 读取越界返回 [`AudioSubstreamError::Read`]；`sus_ver == 0` 的关联音频分支
    /// 未覆盖，返回 [`AudioSubstreamError::Unsupported`]。
    pub fn parse(
        reader: &mut BitReader<'_>,
        context: SubstreamContext,
    ) -> Result<Self, AudioSubstreamError> {
        let mut out = Self::default();
        if context.sus_ver >= 1 {
            out.dialog = reader.read_flag()?;
        } else {
            // b_associated 由 P1 4.3.12.4.1 决定，当前不产生该分支的码流。
            return Err(AudioSubstreamError::Unsupported {
                what: "extended_metadata 的 sus_ver == 0 关联音频分支",
                bit_position: reader.bit_position(),
            });
        }

        if out.dialog {
            if reader.read_flag()? {
                out.dialog_max_gain = Some(u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX));
            }
            if reader.read_flag()? {
                if context.channel_mode == Some(0) {
                    out.pan_dialog_mono =
                        Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX));
                } else {
                    // 未设定的 channel_mode 是负值，不等于 mono，同样走双值分支。
                    let first = u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX);
                    let second = u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX);
                    let selector = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
                    out.pan_dialog = Some((first, second, selector));
                }
            }
        }

        out.channels_classifier = reader.read_flag()?;
        if out.channels_classifier {
            // channel_mode 未设定时按负值处理，全部 contains 查询均为假。
            if let Some(ch_mode) = context.channel_mode {
                if channel_mode_contains_c(ch_mode) && reader.read_flag()? {
                    let _c_has_dialog = reader.read_flag()?;
                }
                if channel_mode_contains_lr(ch_mode) {
                    if reader.read_flag()? {
                        let _l_has_dialog = reader.read_flag()?;
                    }
                    if reader.read_flag()? {
                        let _r_has_dialog = reader.read_flag()?;
                    }
                }
                if channel_mode_contains_ls_rs(ch_mode) {
                    let _ls_active = reader.read_flag()?;
                    let _rs_active = reader.read_flag()?;
                }
                if channel_mode_contains_lb_rb(ch_mode) {
                    let _lb_active = reader.read_flag()?;
                    let _rb_active = reader.read_flag()?;
                }
                if channel_mode_contains_lw_rw(ch_mode) {
                    let _lw_active = reader.read_flag()?;
                    let _rw_active = reader.read_flag()?;
                }
                if channel_mode_contains_tfl_tfr(ch_mode) {
                    let _tfl_active = reader.read_flag()?;
                    let _tfr_active = reader.read_flag()?;
                }
                if channel_mode_contains_lfe(ch_mode) {
                    let _lfe_active = reader.read_flag()?;
                }
            }
        }

        if reader.read_flag()? {
            out.event_probability = Some(u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX));
        }
        Ok(out)
    }
}

/// 一个 `ac4_substream()` 的框架与 metadata。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ac4AudioSubstream {
    /// `audio_size`，即音频数据连同其填充与对齐的字节数。
    pub audio_size: u32,
    /// `audio_data` 在 substream 载荷内的起始字节。
    ///
    /// 头部是 `audio_size_value`(15) 加 `b_more_bits`(1)，其后每个
    /// `variable_bits(7)` 分片恰好 8 位，故头部长度恒为整字节，音频数据总是
    /// 从字节边界开始。P1 表 16 在此处的 `byte_align` 因而恒为零位。
    pub audio_offset: u32,
    /// `basic_metadata()`。
    pub basic: BasicMetadata,
    /// `extended_metadata()`。
    pub extended: ExtendedMetadata,
    /// `tools_metadata_size`，单位为**比特**，覆盖 DRC 与对话增强（`4.3.12.1.1`）。
    pub tools_metadata_bits: u32,
    /// `b_emdf_payloads_substream`。
    pub emdf_payloads_substream: bool,
    /// 内嵌 EMDF payload envelope 的摘要；标志为假时不存在。
    pub emdf_payloads: Option<EmdfPayloadsSubstream>,
    /// metadata 区段（含末尾 `byte_align`）的字节数。
    pub metadata_bytes: u32,
}

impl Ac4AudioSubstream {
    /// 从同一个 substream 载荷中切出 `audio_data` 区段。
    ///
    /// 区段长度即 `audio_size`，含 `audio_data` 之后的 `fill_bits` 与
    /// `byte_align`。`payload` 必须是解析本结构时传入的同一切片。
    #[must_use]
    pub fn audio_payload<'a>(&self, payload: &'a [u8]) -> Option<&'a [u8]> {
        let start = usize::try_from(self.audio_offset).ok()?;
        let end = start.checked_add(usize::try_from(self.audio_size).ok()?)?;
        payload.get(start..end)
    }

    /// 解析一个完整的 `ac4_substream()` 载荷。
    ///
    /// `payload` 必须恰好是该 substream 的字节，可由
    /// [`crate::topology::Ac4Topology::substream_payload`] 取得。音频数据按 `audio_size`
    /// 跳过，不做解码。
    ///
    /// # Errors
    ///
    /// `audio_size` 越界返回 [`AudioSubstreamError::AudioSizeOutOfRange`]；解析后
    /// 未落在 substream 末尾返回 [`AudioSubstreamError::TrailingBits`]。
    pub fn parse(payload: &[u8], context: SubstreamContext) -> Result<Self, AudioSubstreamError> {
        let mut reader = BitReader::new(payload);

        let mut audio_size = u32::try_from(reader.read_bits(15)?).unwrap_or(u32::MAX);
        if reader.read_flag()? {
            audio_size = reader.variable_bits_scaled_u32(7, audio_size, 15)?;
        }

        // audio_size 自 ac4_substream 的头部之后、按字节计。固定头部为 16 位，
        // variable_bits(7) 的每个分片也是 8 位，故音频数据恒从字节边界开始。
        let header_bits = reader.bit_position();
        let substream_len = payload.len() as u64;
        let audio_bits = u64::from(audio_size).saturating_mul(8);
        if header_bits.saturating_add(audio_bits) > substream_len.saturating_mul(8) {
            return Err(AudioSubstreamError::AudioSizeOutOfRange {
                audio_size: u64::from(audio_size),
                substream_len,
            });
        }
        reader.skip_bits(audio_bits)?;

        let metadata_start = reader.bit_position();
        let basic = BasicMetadata::parse(&mut reader, context)?;
        let extended = ExtendedMetadata::parse(&mut reader, context)?;

        if context.alternative && !context.ajoc {
            return Err(AudioSubstreamError::Unsupported {
                what: "metadata 中的 oamd_dyndata_single（b_alternative 且非 A-JOC）",
                bit_position: reader.bit_position(),
            });
        }

        let mut tools_metadata_bits = u32::try_from(reader.read_bits(7)?).unwrap_or(u32::MAX);
        if reader.read_flag()? {
            tools_metadata_bits = reader.variable_bits_scaled_u32(3, tools_metadata_bits, 7)?;
        }
        // tools_metadata_size 以比特计，覆盖 drc_frame 与 dialog_enhancement。
        reader.skip_bits(u64::from(tools_metadata_bits))?;

        let emdf_payloads_substream = reader.read_flag()?;
        let emdf_payloads = if emdf_payloads_substream {
            Some(EmdfPayloadsSubstream::parse(&mut reader)?)
        } else {
            None
        };

        reader.byte_align()?;
        let remaining_bits = reader.remaining_bits();
        if remaining_bits != 0 {
            return Err(AudioSubstreamError::TrailingBits { remaining_bits });
        }

        let metadata_bits = reader.bit_position().saturating_sub(metadata_start);
        Ok(Self {
            audio_size,
            audio_offset: u32::try_from(header_bits.div_ceil(8)).unwrap_or(u32::MAX),
            basic,
            extended,
            tools_metadata_bits,
            emdf_payloads_substream,
            emdf_payloads,
            metadata_bytes: u32::try_from(metadata_bits.div_ceil(8)).unwrap_or(u32::MAX),
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "测试内的位串切片，长度由 pack 的返回值决定"
)]
mod tests {
    use super::*;

    #[expect(
        clippy::arithmetic_side_effects,
        reason = "测试内的位串打包，索引受输入长度约束"
    )]
    fn pack(bits: &str) -> ([u8; 64], usize) {
        let mut out = [0u8; 64];
        let mut index = 0usize;
        for ch in bits.chars() {
            if ch == '0' || ch == '1' {
                if ch == '1' {
                    out[index / 8] |= 1 << (7 - index % 8);
                }
                index += 1;
            }
        }
        (out, index.div_ceil(8))
    }

    const AJOC: SubstreamContext = SubstreamContext {
        sus_ver: 1,
        alternative: false,
        ajoc: true,
        channel_mode: None,
    };

    /// 最简帧：audio_size = 1 字节，metadata 全部取最短分支。
    ///
    /// 组成：audio_size(15)=1、b_more_bits=0、音频 8 比特、
    /// basic_metadata: b_more_basic_metadata=0、
    /// extended_metadata: b_dialog=0、b_channels_classifier=0、b_event_probability=0、
    /// tools_metadata_size_value(7)=0、b_more_bits=0、b_emdf_payloads_substream=0、
    /// byte_align。
    #[test]
    fn parses_minimal_substream() {
        let (bits, len) = pack(
            "000000000000001 0 \
             10101010 \
             0 \
             0 0 0 \
             0000000 0 \
             0 \
             000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.audio_size, 1);
        assert_eq!(parsed.tools_metadata_bits, 0);
        assert!(!parsed.emdf_payloads_substream);
        assert_eq!(
            parsed.basic.dialnorm_bits, None,
            "sus_ver = 1 不传 dialnorm"
        );
    }

    #[test]
    fn parses_inline_empty_emdf_payloads_substream() {
        let (bits, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000000 0 \
             1 \
             00000 000000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        let emdf = parsed.emdf_payloads.expect("标志为真时应保留 EMDF 摘要");

        assert!(parsed.emdf_payloads_substream);
        assert_eq!(emdf.payload_count, 0);
        assert_eq!(emdf.payload_bytes, 0);
        assert_eq!(emdf.align_bits, 6);
        assert_eq!(parsed.metadata_bytes, 3);
    }

    /// 音频区段总是从字节边界开始，`b_more_bits` 展开后也是。
    ///
    /// `variable_bits(7)` 每分片恰好 8 位，故头部长度恒为 16 + 8k 位。若不然，
    /// `audio_offset` 的 `div_ceil` 会与实际起点错开而无从察觉。
    #[test]
    fn audio_payload_starts_on_a_byte_boundary() {
        // audio_size = 2；音频区段两字节写作 0xA5 0x5A 以便与前后字段区分。
        let (bits, len) = pack(
            "000000000000010 0 \
             10100101 01011010 \
             0 0 0 0 0000000 0 0 000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.audio_offset, 2);
        assert_eq!(parsed.audio_payload(&bits[..len]), Some(&[0xA5, 0x5A][..]));

        // b_more_bits = 1，variable_bits(7) 再加一个分片：头部长 24 位。
        let (bits, len) = pack(
            "000000000000010 1 0000000 0 \
             11110000 00001111 \
             0 0 0 0 0000000 0 0 000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.audio_size, 2, "扩展分片为零时不改变 audio_size");
        assert_eq!(parsed.audio_offset, 3);
        assert_eq!(parsed.audio_payload(&bits[..len]), Some(&[0xF0, 0x0F][..]));
    }

    /// 落点必须恰好在 substream 末尾，多一个字节即报错。
    #[test]
    fn detects_trailing_bytes() {
        let (bits, len) = pack(
            "000000000000001 0 10101010 0 0 0 0 0000000 0 0 000 \
             00000000",
        );
        let error = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap_err();
        assert!(
            matches!(error, AudioSubstreamError::TrailingBits { remaining_bits } if remaining_bits == 8),
            "实际为 {error:?}"
        );
    }

    /// audio_size 超出 substream 长度必须立刻报错，而不是读进 metadata。
    #[test]
    fn rejects_oversized_audio_data() {
        let (bits, len) = pack("111111111111111 0 00000000");
        let error = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap_err();
        assert!(
            matches!(error, AudioSubstreamError::AudioSizeOutOfRange { .. }),
            "实际为 {error:?}"
        );
    }

    /// 扩展本身可落入 `u32`，但左移缩放后的 `audio_size` 仍必须检查溢出。
    #[test]
    fn rejects_audio_size_extension_that_overflows_after_scaling() {
        // variable_bits(7) 三段得到 131_072；乘以 2^15 后恰为 2^32。
        // 错误地使用 checked_shl(15) 只检查移位量，会把它截成 0，并让后面的
        // 最短 metadata 作为 audio_size=0 的合法载荷解析成功。
        let (bits, len) = pack(
            "000000000000000 1 \
             0000110 1 1111111 1 0000000 0 \
             0 0 0 0 0000000 0 0 000",
        );

        assert!(matches!(
            Ac4AudioSubstream::parse(&bits[..len], AJOC),
            Err(AudioSubstreamError::Read(ReadError::ValueOverflow { .. }))
        ));
    }

    /// tools_metadata_size 以比特计，覆盖 DRC 与对话增强。
    #[test]
    fn skips_tools_metadata_by_bit_count() {
        // tools_metadata_size = 5，其后 5 比特被跳过。
        let (bits, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000101 0 \
             11111 \
             0 \
             0000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.audio_size, 0);
        assert_eq!(parsed.tools_metadata_bits, 5);
    }

    /// sus_ver = 1 走 substream_loudness 分支，并保留原始码值。
    #[test]
    fn reads_substream_loudness_raw_code() {
        // b_more_basic_metadata=1, b_substream_loudness_info=1,
        // substream_loudness_bits=0b10110011, b_further_substream_loudness_info=0,
        // b_dc_blocking=1, dc_block_on=1
        let (bits, len) = pack(
            "000000000000000 0 \
             1 1 10110011 0 1 1 \
             0 0 0 \
             0000000 0 \
             0 \
             0000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.basic.substream_loudness_bits, Some(0b1011_0011));
        assert_eq!(parsed.basic.dc_block_on, Some(true));
    }

    /// 超出 `u32` 的 `e_bits_size` 扩展不得窄化后作为 31 位扩展接受。
    ///
    /// 与 OAMD 的 `add_data_bytes` 同型：`variable_bits` 返回 `u64`，窄化失败
    /// 时若回落为 0，`size` 会停在触发扩展的 31，畸形码流反而解析成功。
    #[test]
    fn loudness_extension_rejects_length_above_u32() {
        // sus_ver=1、presentation_ldn=false：b_loudcorr_dialgate=0；六个可选
        // 响度值缺席；lra/loudmntry/max_loudmntry 缺席；b_rtllcomp=0；
        // b_extension=1；e_bits_size=31 触发 variable_bits(4)；七组 1111+more
        // 后 0000+stop 得 4 581 298 432 > u32::MAX。末尾的零位足以让错误实现
        // 跳完它以为的 31 位并成功返回。
        let (bits, len) = pack(
            "0 0 0 0 0 0 0 0 0 0 0 1 11111 \
             11111 11111 11111 11111 11111 11111 11111 00000 \
             0000000000000000000000000000000",
        );
        let mut reader = BitReader::new(&bits[..len]);

        assert!(matches!(
            FurtherLoudnessInfo::parse(&mut reader, 1, false),
            Err(AudioSubstreamError::Read(ReadError::ValueOverflow { .. }))
        ));
    }

    /// sus_ver = 0 的扩展区段走另一条分支，同样不得接受溢出长度。
    #[test]
    fn legacy_loudness_extension_rejects_length_above_u32() {
        // loudness_version=0、loud_prac_type=0；六个可选响度值缺席；prgmbndy
        // 缺席；lra/loudmntry/max_loudmntry 缺席；b_extension=1；
        // e_bits_size=31 后同样以 variable_bits(4) 溢出 u32。
        let (bits, len) = pack(
            "00 0000 0 0 0 0 0 0 0 0 0 0 1 11111 \
             11111 11111 11111 11111 11111 11111 11111 00000 \
             0000000000000000000000000000000",
        );
        let mut reader = BitReader::new(&bits[..len]);

        assert!(matches!(
            FurtherLoudnessInfo::parse(&mut reader, 0, false),
            Err(AudioSubstreamError::Read(ReadError::ValueOverflow { .. }))
        ));
    }

    /// sus_ver = 0 的扩展区段把 b_rtllcomp 与 rtll_comp 计入 e_bits_size。
    #[test]
    fn legacy_loudness_extension_consumes_embedded_rtllcomp() {
        let (bits, _) = pack(
            // loudness_version=0, loud_prac_type=0；六个可选响度值、
            // prgmbndy、lra、loudmntry 与 max_loudmntry 均缺席。
            "00 0000 0 0 0 0 0 0 0 0 0 0 \
             1 01011 1 10100101 10 \
             1101",
        );
        let mut reader = BitReader::new(&bits);

        let parsed = FurtherLoudnessInfo::parse(&mut reader, 0, false).unwrap();

        assert_eq!(parsed.rtll_comp, Some(0b1010_0101));
        assert_eq!(parsed.extension_bits, Some(2));
        assert_eq!(reader.bit_position(), 33);
        assert_eq!(reader.read_bits(4).unwrap(), 0b1101, "不得吞掉后续字段");
    }

    #[test]
    fn parses_seven_x_channel_metadata() {
        let context = SubstreamContext {
            channel_mode: Some(6),
            ajoc: false,
            ..AJOC
        };
        let (bits, len) = pack(
            // audio_size=0；basic: loudness 缺席，7_X upmix=3/4/0，随后
            // phase/attenuation 与 DC blocking。
            "000000000000000 0 \
             1 0 1 10 01 1 0 1 1 \
             0 1 \
             1 0 1 1 0 1 0 0 1 1 \
             1 1010 \
             0000000 0 \
             0 \
             000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], context).unwrap();

        assert_eq!(parsed.basic.dc_block_on, Some(true));
        assert!(parsed.extended.channels_classifier);
        assert_eq!(parsed.extended.event_probability, Some(0b1010));
        assert_eq!(parsed.metadata_bytes, 5);
    }

    #[test]
    fn parses_stereo_and_mono_pan_metadata() {
        let stereo = SubstreamContext {
            channel_mode: Some(1),
            ajoc: false,
            ..AJOC
        };
        let (bits, len) = pack(
            // basic: previous stereo downmix；extended: dialog gain、双值 pan，
            // L/R classifier。
            "000000000000000 0 \
             1 0 1 101 10 0 \
             1 1 10 1 10100101 01011010 11 \
             1 1 0 1 1 0 \
             0000000 0 \
             0 \
             0",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], stereo).unwrap();
        assert_eq!(parsed.extended.dialog_max_gain, Some(0b10));
        assert_eq!(
            parsed.extended.pan_dialog,
            Some((0b1010_0101, 0b0101_1010, 0b11))
        );
        assert_eq!(parsed.extended.pan_dialog_mono, None);

        let mono = SubstreamContext {
            channel_mode: Some(0),
            ajoc: false,
            ..AJOC
        };
        let (bits, len) = pack(
            "000000000000000 0 \
             0 \
             1 0 1 10100101 \
             1 1 1 0 \
             0000000 0 \
             0 \
             0000000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], mono).unwrap();
        assert_eq!(parsed.extended.pan_dialog_mono, Some(0b1010_0101));
        assert_eq!(parsed.extended.pan_dialog, None);
    }

    #[test]
    fn channel_mode_queries_follow_part_two_pseudocode() {
        fn assert_modes(predicate: fn(u32) -> bool, expected: &[u32]) {
            for mode in 0..=15 {
                assert_eq!(predicate(mode), expected.contains(&mode), "ch_mode={mode}");
            }
        }

        assert_modes(channel_mode_contains_lfe, &[4, 6, 8, 10, 12, 14, 15]);
        assert_modes(
            channel_mode_contains_c,
            &[0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        );
        assert_modes(
            channel_mode_contains_lr,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        );
        assert_modes(
            channel_mode_contains_ls_rs,
            &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        );
        assert_modes(channel_mode_contains_lb_rb, &[5, 6, 11, 12, 13, 14, 15]);
        assert_modes(channel_mode_contains_lw_rw, &[7, 8, 15]);
        assert_modes(channel_mode_contains_tfl_tfr, &[9, 10]);
    }

    /// P2 使用符号化的 `5_X`/`7_X`，本判据钉住 P1 等价语法给出的模式集合。
    ///
    /// 5_X 由 P1 表 19 的 channel element 分派给出，7_X 由 P1 表 67 的
    /// `5 <= ch_mode <= 10` 直接给出；两者仍需独立覆盖，避免符号化转写漂移。
    #[test]
    fn symbolic_channel_mode_families_match_part_one_ranges() {
        fn assert_modes(predicate: fn(u32) -> bool, expected: &[u32]) {
            for mode in 0..=15 {
                assert_eq!(predicate(mode), expected.contains(&mode), "ch_mode={mode}");
            }
        }

        assert_modes(channel_mode_is_five_x, &[3, 4]);
        assert_modes(channel_mode_is_seven_x, &[5, 6, 7, 8, 9, 10]);

        // 两族必须互斥：ch_mode 同时落进 5_X 与 7_X 会让 basic_metadata 连读
        // 两组下混/上混字段，子流末尾等式随之失配。
        for mode in 0..=15 {
            assert!(
                !(channel_mode_is_five_x(mode) && channel_mode_is_seven_x(mode)),
                "ch_mode={mode} 不得同时属于 5_X 与 7_X"
            );
        }
    }
}
