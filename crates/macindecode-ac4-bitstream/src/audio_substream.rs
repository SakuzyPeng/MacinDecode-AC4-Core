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
//!
//! `tools_metadata_size` 另以比特数严格定界 DRC 与 dialogue enhancement。当前
//! `sus_ver >= 1` 路径解析 `b_de_data_present`、I/dependent configuration gate 与
//! `de_config()`，并把 `de_data()`/simulcast body 保留为零拷贝 bit view；启用
//! `audio-decode` 后可显式熵解码完整帧内 data 语法，并以调用方按物理 substream 隔离的状态
//! 还原有效参数索引；仍不反量化或执行 dialogue enhancement。

use crate::emdf::{EmdfError, EmdfPayloadsSubstream};
use crate::reader::{BitReader, ReadError};
use core::fmt;

#[cfg(feature = "audio-decode")]
mod dialog_enhancement;
#[cfg(feature = "audio-decode")]
pub use dialog_enhancement::{
    DIALOG_ENHANCEMENT_PARAMETER_BANDS, DialogEnhancementDataBlock, DialogEnhancementDataError,
    DialogEnhancementDecodedData, DialogEnhancementEffectiveData,
    DialogEnhancementEffectiveDataBlock, DialogEnhancementEffectiveParameterData,
    DialogEnhancementEffectiveSimulcastData, DialogEnhancementMixCoefficients,
    DialogEnhancementParameterData, DialogEnhancementParameterUpdate,
    DialogEnhancementPositionUpdate, DialogEnhancementSimulcastData, DialogEnhancementState,
    DialogEnhancementStateError, MAX_DIALOG_ENHANCEMENT_PARAMETER_CHANNELS,
    MAX_DIALOG_ENHANCEMENT_PARAMETER_CODES,
};

/// 解析 `ac4_substream()` 失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSubstreamError {
    /// 底层读取失败。
    Read(ReadError),
    /// 内嵌 `emdf_payloads_substream()` 失败。
    Emdf(EmdfError),
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
    /// `tools_metadata_size` 不足以容纳必需的 dialogue-enhancement presence 或 body 前缀。
    InvalidToolsMetadataSize {
        /// 码流声明的 tools metadata 长度。
        declared: u32,
        /// 当前已知语法要求的最小长度。
        minimum: u32,
        /// `tools_metadata_size_value` 的比特偏移。
        bit_position: u64,
    },
    /// dialogue enhancement 明确缺席，但有界 tools metadata 内仍有尾随比特。
    TrailingToolsMetadataBits {
        /// 首个尾随比特在 substream payload 内的偏移。
        bit_position: u64,
        /// 尚未消费的 tools metadata 比特数。
        remaining_bits: u32,
    },
    /// `de_channel_config` 不适用于 metadata 上下文声明的 mono/stereo 模式。
    InvalidDialogEnhancementChannelConfiguration {
        /// 3 比特 `de_channel_config` 原值。
        declared: u8,
        /// `ac4_substream_info_chan()` 声明的 `ch_mode`。
        channel_mode: u32,
        /// `de_channel_config` 的比特偏移。
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
            AudioSubstreamError::Read(error) => write!(f, "Failed to read ac4_substream: {error}"),
            AudioSubstreamError::Emdf(error) => {
                write!(f, "Failed to parse inline EMDF payloads: {error}")
            }
            AudioSubstreamError::AudioSizeOutOfRange {
                audio_size,
                substream_len,
            } => write!(
                f,
                "audio_size is {audio_size} bytes, but the substream is only {substream_len} bytes"
            ),
            AudioSubstreamError::InvalidExtensionSize {
                declared,
                required,
                bit_position,
            } => write!(
                f,
                "e_bits_size is {declared} at bit offset {bit_position}; at least {required} bits are required"
            ),
            AudioSubstreamError::InvalidToolsMetadataSize {
                declared,
                minimum,
                bit_position,
            } => write!(
                f,
                "Audio tools metadata size is {declared} bits at offset {bit_position}; at least {minimum} bits are required"
            ),
            AudioSubstreamError::TrailingToolsMetadataBits {
                bit_position,
                remaining_bits,
            } => write!(
                f,
                "Audio tools metadata has {remaining_bits} trailing bits after absent dialogue enhancement at bit offset {bit_position}"
            ),
            AudioSubstreamError::InvalidDialogEnhancementChannelConfiguration {
                declared,
                channel_mode,
                bit_position,
            } => write!(
                f,
                "Dialogue-enhancement channel configuration {declared} is invalid for channel mode {channel_mode} at bit offset {bit_position}"
            ),
            AudioSubstreamError::TrailingBits { remaining_bits } => write!(
                f,
                "{remaining_bits} bits remain after parsing metadata; parser did not end at the substream boundary"
            ),
            AudioSubstreamError::Unsupported { what, bit_position } => {
                write!(f, "Unsupported branch at bit offset {bit_position}: {what}")
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

impl From<EmdfError> for AudioSubstreamError {
    fn from(error: EmdfError) -> Self {
        AudioSubstreamError::Emdf(error)
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
    /// 传给 `metadata()` 的 `b_iframe`，即前置 info 元素的 `b_audio_ndot`。
    ///
    /// `None` 表示一个 info 元素覆盖多个 substream，而拓扑层只保留了这些 ndot 位的合取，
    /// 无法确定当前物理 substream 的逐一取值。dialogue enhancement 缺席时不需要该上下文；
    /// 活动分支会失败关闭。
    pub b_iframe: Option<bool>,
}

/// `tools_metadata_size` 严格定界的原始 bit view。
///
/// 视图借用调用 [`AudioToolsMetadata::bits`] 或
/// [`DialogEnhancementMetadata::unparsed_body`] 时传入的原 substream payload，不分配或复制。
#[derive(Debug, Clone, Copy)]
pub struct AudioToolsMetadataBits<'a> {
    source: &'a [u8],
    bit_offset: u64,
    bit_len: u32,
}

impl<'a, 'b> PartialEq<AudioToolsMetadataBits<'b>> for AudioToolsMetadataBits<'a> {
    fn eq(&self, other: &AudioToolsMetadataBits<'b>) -> bool {
        self.bit_len == other.bit_len && (*self).iter().eq((*other).iter())
    }
}

impl Eq for AudioToolsMetadataBits<'_> {}

impl<'a> AudioToolsMetadataBits<'a> {
    fn new(source: &'a [u8], bit_offset: u64, bit_len: u32) -> Option<Self> {
        let end = bit_offset.checked_add(u64::from(bit_len))?;
        if end > (source.len() as u64).saturating_mul(8) {
            return None;
        }
        Some(Self {
            source,
            bit_offset,
            bit_len,
        })
    }

    /// 视图起点相对原 substream payload 的比特偏移。
    #[must_use]
    pub const fn bit_offset(self) -> u64 {
        self.bit_offset
    }

    /// 视图末尾相对原 substream payload 的比特偏移。
    #[must_use]
    pub const fn end_bit_offset(self) -> u64 {
        self.bit_offset.saturating_add(self.bit_len as u64)
    }

    /// 视图包含的比特数。
    #[must_use]
    pub const fn len_bits(self) -> u32 {
        self.bit_len
    }

    /// 视图是否为空。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bit_len == 0
    }

    /// 读取一个原始 bit。
    #[must_use]
    pub fn get(self, index: u32) -> Option<bool> {
        if index >= self.bit_len {
            return None;
        }
        let mut reader = BitReader::new(self.source);
        reader
            .skip_bits(self.bit_offset.checked_add(u64::from(index))?)
            .ok()?;
        reader.read_flag().ok()
    }

    /// 若起点和长度都位于字节边界，返回零拷贝字节切片。
    #[must_use]
    pub fn as_aligned_slice(self) -> Option<&'a [u8]> {
        if !self.bit_offset.is_multiple_of(8) || !self.bit_len.is_multiple_of(8) {
            return None;
        }
        let start = usize::try_from(self.bit_offset / 8).ok()?;
        let len = usize::try_from(self.bit_len / 8).ok()?;
        self.source.get(start..start.checked_add(len)?)
    }

    /// 按码流顺序遍历原始 bit。
    #[must_use]
    pub fn iter(self) -> AudioToolsMetadataBitIter<'a> {
        AudioToolsMetadataBitIter::new(self)
    }
}

impl<'a> IntoIterator for AudioToolsMetadataBits<'a> {
    type Item = bool;
    type IntoIter = AudioToolsMetadataBitIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`AudioToolsMetadataBits`] 的无分配迭代器。
#[derive(Debug, Clone)]
pub struct AudioToolsMetadataBitIter<'a> {
    reader: BitReader<'a>,
    remaining: u32,
}

impl<'a> AudioToolsMetadataBitIter<'a> {
    fn new(bits: AudioToolsMetadataBits<'a>) -> Self {
        let mut reader = BitReader::new(bits.source);
        let remaining = if reader.skip_bits(bits.bit_offset).is_ok() {
            bits.bit_len
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for AudioToolsMetadataBitIter<'_> {
    type Item = bool;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = self.reader.read_flag().ok();
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

/// `de_config()` 的三个原始码值。
///
/// 配置用于继续定界 `de_data()`，并在 `audio-decode` 状态层选择参数形状；本类型不换算最大
/// 增益，也不执行 dialogue enhancement。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogEnhancementConfiguration {
    /// 2 比特 `de_method`。
    pub method: u8,
    /// 2 比特 `de_max_gain`。
    pub max_gain: u8,
    /// 3 比特 `de_channel_config`。
    pub channel_config: u8,
}

impl DialogEnhancementConfiguration {
    /// 表 171 由 `de_channel_config` 派生的 `de_nr_channels`。
    #[must_use]
    pub const fn channel_count(self) -> u8 {
        self.channel_config.count_ones() as u8
    }
}

/// 当前帧传输的 dialogue-enhancement 配置形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogEnhancementConfigurationUpdate {
    /// `b_de_data_present` 为假，语法中不传输 configuration 或 data。
    NotPresent,
    /// dependent frame 的 `b_de_config_flag` 为假，应使用前一有效配置。
    KeepPrevious,
    /// I-frame 的必传配置，或 dependent frame 显式更新的配置。
    New(DialogEnhancementConfiguration),
}

/// `dialog_enhancement()` 的 presence、configuration 与活动 data body。
///
/// 默认构建以原始 bit view 保留 `de_data()`；启用 `audio-decode` 后可显式解码帧内语法，或交给
/// `DialogEnhancementState` 延续有效索引。本类型不执行 dialogue enhancement。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogEnhancementMetadata {
    /// `b_de_data_present`。
    pub data_present: bool,
    /// 当前帧的 configuration 更新形态。
    pub configuration: DialogEnhancementConfigurationUpdate,
    unparsed_body_bit_offset: u64,
    unparsed_body_bits: u32,
    frame_iframe: Option<bool>,
    simulcast_gate: bool,
}

impl DialogEnhancementMetadata {
    /// 前置 info 为当前物理 substream 提供的 `b_audio_ndot`/`b_iframe`。
    ///
    /// 一个 info 覆盖多个物理 substream 且无法恢复逐一 ndot 时为 `None`。活动 DE 的帧内解析
    /// 会拒绝该情况；DE 缺席仍可无状态解析，但跨帧状态无法判定是否应在随机访问点清空。
    #[must_use]
    pub const fn b_iframe(self) -> Option<bool> {
        self.frame_iframe
    }

    /// 已解析 configuration 之后尚未解释的 `de_data()`/simulcast body 比特数。
    #[must_use]
    pub const fn unparsed_body_len_bits(self) -> u32 {
        self.unparsed_body_bits
    }

    /// 从解析时使用的同一 substream payload 取得尚未解释的 `de_data()`/simulcast body。
    #[must_use]
    pub fn unparsed_body<'a>(self, payload: &'a [u8]) -> Option<AudioToolsMetadataBits<'a>> {
        AudioToolsMetadataBits::new(
            payload,
            self.unparsed_body_bit_offset,
            self.unparsed_body_bits,
        )
    }
}

/// `tools_metadata_size` 定界并完成 dialogue-enhancement 配置前缀解析的 audio tools metadata。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioToolsMetadata {
    size_value_bit_offset: u64,
    bit_offset: u64,
    bit_len: u32,
    /// `sus_ver >= 1` 路径中的 `dialog_enhancement()`。
    pub dialog_enhancement: DialogEnhancementMetadata,
}

impl AudioToolsMetadata {
    /// `tools_metadata_size_value` 在 substream payload 内的比特偏移。
    #[must_use]
    pub const fn size_value_bit_offset(self) -> u64 {
        self.size_value_bit_offset
    }

    /// tools metadata body 在 substream payload 内的比特偏移。
    #[must_use]
    pub const fn bit_offset(self) -> u64 {
        self.bit_offset
    }

    /// `tools_metadata_size` 声明的比特数。
    #[must_use]
    pub const fn len_bits(self) -> u32 {
        self.bit_len
    }

    /// 从解析时使用的同一 substream payload 取得完整 tools metadata 原始视图。
    #[must_use]
    pub fn bits<'a>(self, payload: &'a [u8]) -> Option<AudioToolsMetadataBits<'a>> {
        AudioToolsMetadataBits::new(payload, self.bit_offset, self.bit_len)
    }
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

/// `further_loudness_info()` 中的 programme boundary 原始信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoudnessProgrammeBoundary {
    /// `prgmbndy`，即当前帧与 programme boundary 所在帧的距离。
    pub frame_distance: u64,
    /// `b_end_or_start`；为真时 boundary 位于当前帧之后，否则位于之前。
    pub upcoming: bool,
    /// `b_prgmbndy_offset` 控制的 11 比特 sample offset。
    pub sample_offset: Option<u16>,
}

/// `further_loudness_info()` 中未定义的 `extensions_bits` 视图。
///
/// 视图借用调用 [`FurtherLoudnessInfo::extension_data`] 时传入的原 payload，不分配或复制。
#[derive(Debug, Clone, Copy)]
pub struct LoudnessExtensionBits<'a> {
    source: &'a [u8],
    bit_offset: u64,
    bit_len: u32,
}

impl<'a, 'b> PartialEq<LoudnessExtensionBits<'b>> for LoudnessExtensionBits<'a> {
    fn eq(&self, other: &LoudnessExtensionBits<'b>) -> bool {
        self.bit_len == other.bit_len && (*self).iter().eq((*other).iter())
    }
}

impl Eq for LoudnessExtensionBits<'_> {}

impl<'a> LoudnessExtensionBits<'a> {
    /// 视图包含的比特数。
    #[must_use]
    pub const fn len_bits(self) -> u32 {
        self.bit_len
    }

    /// 视图是否为空。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bit_len == 0
    }

    /// 读取一个 extension bit。
    #[must_use]
    pub fn get(self, index: u32) -> Option<bool> {
        if index >= self.bit_len {
            return None;
        }
        let mut reader = BitReader::new(self.source);
        reader
            .skip_bits(self.bit_offset.checked_add(u64::from(index))?)
            .ok()?;
        reader.read_flag().ok()
    }

    /// 若起点和长度都位于字节边界，返回零拷贝字节切片。
    #[must_use]
    pub fn as_aligned_slice(self) -> Option<&'a [u8]> {
        if !self.bit_offset.is_multiple_of(8) || !self.bit_len.is_multiple_of(8) {
            return None;
        }
        let start = usize::try_from(self.bit_offset / 8).ok()?;
        let len = usize::try_from(self.bit_len / 8).ok()?;
        self.source.get(start..start.checked_add(len)?)
    }

    /// 按码流顺序遍历 extension bits。
    #[must_use]
    pub fn iter(self) -> LoudnessExtensionBitIter<'a> {
        LoudnessExtensionBitIter::new(self)
    }
}

impl<'a> IntoIterator for LoudnessExtensionBits<'a> {
    type Item = bool;
    type IntoIter = LoudnessExtensionBitIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`LoudnessExtensionBits`] 的无分配迭代器。
#[derive(Debug, Clone)]
pub struct LoudnessExtensionBitIter<'a> {
    reader: BitReader<'a>,
    remaining: u32,
}

impl<'a> LoudnessExtensionBitIter<'a> {
    fn new(bits: LoudnessExtensionBits<'a>) -> Self {
        let mut reader = BitReader::new(bits.source);
        let remaining = if reader.skip_bits(bits.bit_offset).is_ok() {
            bits.bit_len
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for LoudnessExtensionBitIter<'_> {
    type Item = bool;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = self.reader.read_flag().ok();
        if value.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        } else {
            self.remaining = 0;
        }
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for LoudnessExtensionBitIter<'_> {}

/// `further_loudness_info()` 中的原始码值，见 `6.2.7.3`。
///
/// 全部保留量化码值，不换算为 LKFS。`extensions_bits` 的内容未由规范定义，
/// 可通过 [`Self::extension_data`] 从原 payload 取得。结构相等性比较已定义字段与 extension
/// 长度，不比较 payload 内的位置或 opaque 内容；后者应比较返回的 [`LoudnessExtensionBits`]。
#[derive(Debug, Clone, Copy, Default)]
pub struct FurtherLoudnessInfo {
    /// 2 比特 `loudness_version`；该分支不传输时为 `None`。
    pub loudness_version: Option<u8>,
    /// `loudness_version == 3` 时传输的 4 比特扩展原值。
    pub extended_loudness_version: Option<u8>,
    /// 4 比特 `loud_prac_type`；该分支不传输时为 `None`。
    pub loud_prac_type: Option<u8>,
    /// `b_loudcorr_dialgate`；由 practice 或调用上下文决定是否传输。
    pub loudcorr_dialgate: Option<bool>,
    /// 上一个 `b_loudcorr_dialgate` 为真时的 3 比特 practice type。
    pub loudcorr_dialgate_prac_type: Option<u8>,
    /// `b_loudcorr_type`；为真表示实时响度测量，为假表示 file-based correction。
    pub loudcorr_type: Option<bool>,
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
    /// 可选 programme boundary。
    pub programme_boundary: Option<LoudnessProgrammeBoundary>,
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
    /// `extensions_bits` 相对构造 [`BitReader`] 所用切片的精确比特偏移。
    pub extension_bits_offset: Option<u64>,
}

impl PartialEq for FurtherLoudnessInfo {
    fn eq(&self, other: &Self) -> bool {
        self.loudness_version == other.loudness_version
            && self.extended_loudness_version == other.extended_loudness_version
            && self.loud_prac_type == other.loud_prac_type
            && self.loudcorr_dialgate == other.loudcorr_dialgate
            && self.loudcorr_dialgate_prac_type == other.loudcorr_dialgate_prac_type
            && self.loudcorr_type == other.loudcorr_type
            && self.loudrelgat == other.loudrelgat
            && self.loudspchgat == other.loudspchgat
            && self.loudstrm3s == other.loudstrm3s
            && self.max_loudstrm3s == other.max_loudstrm3s
            && self.truepk == other.truepk
            && self.max_truepk == other.max_truepk
            && self.programme_boundary == other.programme_boundary
            && self.lra == other.lra
            && self.loudmntry == other.loudmntry
            && self.max_loudmntry == other.max_loudmntry
            && self.rtll_comp == other.rtll_comp
            && self.extension_bits == other.extension_bits
    }
}

impl Eq for FurtherLoudnessInfo {}

impl FurtherLoudnessInfo {
    /// 取得扩展后的有效 loudness version。
    #[must_use]
    pub const fn effective_loudness_version(self) -> Option<u8> {
        match self.loudness_version {
            Some(3) => match self.extended_loudness_version {
                Some(extension) => 3u8.checked_add(extension),
                None => None,
            },
            version => version,
        }
    }

    /// 从解析时使用的同一 payload 取得未定义的 `extensions_bits`。
    ///
    /// `payload` 必须是构造该 [`BitReader`] 时使用的同一切片；无法覆盖原 bit range 时
    /// 返回 `None`。
    #[must_use]
    pub fn extension_data<'a>(self, payload: &'a [u8]) -> Option<LoudnessExtensionBits<'a>> {
        let bit_offset = self.extension_bits_offset?;
        let bit_len = self.extension_bits?;
        let end = bit_offset.checked_add(u64::from(bit_len))?;
        if end > (payload.len() as u64).saturating_mul(8) {
            return None;
        }
        Some(LoudnessExtensionBits {
            source: payload,
            bit_offset,
            bit_len,
        })
    }

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
        Self::parse_with_error(
            reader,
            sus_ver,
            presentation_ldn,
            |declared, required, bit_position| AudioSubstreamError::InvalidExtensionSize {
                declared,
                required,
                bit_position,
            },
        )
    }

    pub(crate) fn parse_with_error<E, F>(
        reader: &mut BitReader<'_>,
        sus_ver: u8,
        presentation_ldn: bool,
        invalid_extension_size: F,
    ) -> Result<Self, E>
    where
        E: From<ReadError>,
        F: Fn(u32, u32, u64) -> E,
    {
        let mut out = Self::default();

        if presentation_ldn || sus_ver == 0 {
            let loudness_version = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
            out.loudness_version = Some(loudness_version);
            if loudness_version == 3 {
                out.extended_loudness_version =
                    Some(u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX));
            }
            let loud_prac_type = u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX);
            out.loud_prac_type = Some(loud_prac_type);
            if loud_prac_type != 0 {
                let loudcorr_dialgate = reader.read_flag()?;
                out.loudcorr_dialgate = Some(loudcorr_dialgate);
                if loudcorr_dialgate {
                    out.loudcorr_dialgate_prac_type =
                        Some(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX));
                }
                out.loudcorr_type = Some(reader.read_flag()?);
            }
        } else {
            out.loudcorr_dialgate = Some(reader.read_flag()?);
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
            let start = reader.bit_position();
            let mut frame_distance = 1u64;
            loop {
                frame_distance = frame_distance.checked_mul(2).ok_or_else(|| {
                    E::from(ReadError::ValueOverflow {
                        bit_position: start,
                    })
                })?;
                if reader.read_flag()? {
                    break;
                }
            }
            let upcoming = reader.read_flag()?;
            let sample_offset = if reader.read_flag()? {
                Some(u16::try_from(reader.read_bits(11)?).unwrap_or(u16::MAX))
            } else {
                None
            };
            out.programme_boundary = Some(LoudnessProgrammeBoundary {
                frame_distance,
                upcoming,
                sample_offset,
            });
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
                out.extension_bits_offset = Some(reader.bit_position());
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
            let extension_bits = size
                .checked_sub(required)
                .ok_or_else(|| invalid_extension_size(size, required, reader.bit_position()))?;
            if rtll_present {
                out.rtll_comp = Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX));
            }
            out.extension_bits = Some(extension_bits);
            out.extension_bits_offset = Some(reader.bit_position());
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
                what: "sus_ver == 0 audio branch associated with extended_metadata",
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
    /// 严格按声明长度定界并解析 dialogue-enhancement 配置前缀的 tools metadata。
    pub tools_metadata: AudioToolsMetadata,
    /// `b_emdf_payloads_substream`。
    pub emdf_payloads_substream: bool,
    /// 内嵌 EMDF payload envelope；标志为假时不存在。
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
    /// `audio_size` 越界返回 [`AudioSubstreamError::AudioSizeOutOfRange`]；tools metadata
    /// 长度不足或与 presence 不一致分别返回
    /// [`AudioSubstreamError::InvalidToolsMetadataSize`] 或
    /// [`AudioSubstreamError::TrailingToolsMetadataBits`]；mono/stereo 的 DE channel configuration
    /// 不适用时返回 [`AudioSubstreamError::InvalidDialogEnhancementChannelConfiguration`]；内嵌
    /// EMDF 的 envelope、容量或长度非法时返回 [`AudioSubstreamError::Emdf`]；解析后未落在
    /// substream 末尾返回 [`AudioSubstreamError::TrailingBits`]。
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
                what: "oamd_dyndata_single in metadata (b_alternative and non-A-JOC)",
                bit_position: reader.bit_position(),
            });
        }

        let tools_metadata_size_value_offset = reader.bit_position();
        let mut tools_metadata_bits = u32::try_from(reader.read_bits(7)?).unwrap_or(u32::MAX);
        if reader.read_flag()? {
            tools_metadata_bits = reader.variable_bits_scaled_u32(3, tools_metadata_bits, 7)?;
        }
        let tools_metadata_bit_offset = reader.bit_position();
        let tools_metadata = parse_tools_metadata(
            payload,
            tools_metadata_size_value_offset,
            tools_metadata_bit_offset,
            tools_metadata_bits,
            context,
        )?;
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
            tools_metadata,
            emdf_payloads_substream,
            emdf_payloads,
            metadata_bytes: u32::try_from(metadata_bits.div_ceil(8)).unwrap_or(u32::MAX),
        })
    }
}

fn parse_tools_metadata(
    payload: &[u8],
    size_value_bit_offset: u64,
    bit_offset: u64,
    bit_len: u32,
    context: SubstreamContext,
) -> Result<AudioToolsMetadata, AudioSubstreamError> {
    if bit_len == 0 {
        return Err(AudioSubstreamError::InvalidToolsMetadataSize {
            declared: bit_len,
            minimum: 1,
            bit_position: size_value_bit_offset,
        });
    }

    let mut reader = BitReader::new_bounded(payload, bit_offset, u64::from(bit_len))?;
    let data_present = reader.read_flag()?;
    let configuration = if data_present {
        require_tools_metadata_bits(bit_len, 2, size_value_bit_offset)?;
        let Some(b_iframe) = context.b_iframe else {
            return Err(AudioSubstreamError::Unsupported {
                what: "active dialog_enhancement without an exact b_iframe context",
                bit_position: reader.bit_position(),
            });
        };
        if b_iframe {
            require_tools_metadata_bits(bit_len, 8, size_value_bit_offset)?;
            DialogEnhancementConfigurationUpdate::New(parse_dialog_enhancement_configuration(
                &mut reader,
                context.channel_mode,
            )?)
        } else {
            if reader.read_flag()? {
                require_tools_metadata_bits(bit_len, 9, size_value_bit_offset)?;
                DialogEnhancementConfigurationUpdate::New(parse_dialog_enhancement_configuration(
                    &mut reader,
                    context.channel_mode,
                )?)
            } else {
                DialogEnhancementConfigurationUpdate::KeepPrevious
            }
        }
    } else {
        let remaining_bits = u32::try_from(reader.remaining_bits()).unwrap_or(u32::MAX);
        if remaining_bits != 0 {
            return Err(AudioSubstreamError::TrailingToolsMetadataBits {
                bit_position: reader.bit_position(),
                remaining_bits,
            });
        }
        DialogEnhancementConfigurationUpdate::NotPresent
    };
    let unparsed_body_bit_offset = reader.bit_position();
    let unparsed_body_bits = u32::try_from(reader.remaining_bits()).unwrap_or(u32::MAX);

    Ok(AudioToolsMetadata {
        size_value_bit_offset,
        bit_offset,
        bit_len,
        dialog_enhancement: DialogEnhancementMetadata {
            data_present,
            configuration,
            unparsed_body_bit_offset,
            unparsed_body_bits,
            frame_iframe: context.b_iframe,
            simulcast_gate: data_present && matches!(context.channel_mode, Some(13 | 14)),
        },
    })
}

fn require_tools_metadata_bits(
    declared: u32,
    minimum: u32,
    bit_position: u64,
) -> Result<(), AudioSubstreamError> {
    if declared < minimum {
        return Err(AudioSubstreamError::InvalidToolsMetadataSize {
            declared,
            minimum,
            bit_position,
        });
    }
    Ok(())
}

fn parse_dialog_enhancement_configuration(
    reader: &mut BitReader<'_>,
    channel_mode: Option<u32>,
) -> Result<DialogEnhancementConfiguration, AudioSubstreamError> {
    let method = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
    let max_gain = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);
    let bit_position = reader.bit_position();
    let channel_config = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
    let valid = match channel_mode {
        Some(0) => matches!(channel_config, 0 | 1),
        Some(1) => matches!(channel_config, 0 | 2 | 4 | 6),
        _ => true,
    };
    if !valid {
        return Err(
            AudioSubstreamError::InvalidDialogEnhancementChannelConfiguration {
                declared: channel_config,
                channel_mode: channel_mode.unwrap_or(u32::MAX),
                bit_position,
            },
        );
    }
    Ok(DialogEnhancementConfiguration {
        method,
        max_gain,
        channel_config,
    })
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "测试内的位串切片，长度由 pack 的返回值决定"
)]
mod tests {
    extern crate std;

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
        b_iframe: Some(false),
    };

    /// 最简帧：audio_size = 1 字节，metadata 全部取最短分支。
    ///
    /// 组成：audio_size(15)=1、b_more_bits=0、音频 8 比特、
    /// basic_metadata: b_more_basic_metadata=0、
    /// extended_metadata: b_dialog=0、b_channels_classifier=0、b_event_probability=0、
    /// tools_metadata_size_value(7)=1、b_more_bits=0、b_de_data_present=0、
    /// b_emdf_payloads_substream=0、byte_align。
    #[test]
    fn parses_minimal_substream() {
        let (bits, len) = pack(
            "000000000000001 0 \
             10101010 \
             0 \
             0 0 0 \
             0000001 0 \
             0 \
             0 \
             00",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.audio_size, 1);
        assert_eq!(parsed.tools_metadata_bits, 1);
        assert_eq!(parsed.tools_metadata.size_value_bit_offset(), 28);
        assert_eq!(parsed.tools_metadata.bit_offset(), 36);
        let tools = parsed.tools_metadata.bits(&bits[..len]).unwrap();
        assert_eq!(tools.bit_offset(), 36);
        assert_eq!(tools.end_bit_offset(), 37);
        assert_eq!(tools.len_bits(), 1);
        assert!(!tools.is_empty());
        assert_eq!(tools.get(0), Some(false));
        assert_eq!(tools.get(1), None);
        assert_eq!(tools.as_aligned_slice(), None);
        assert_eq!(tools.iter().collect::<std::vec::Vec<_>>(), [false]);
        assert!(!parsed.tools_metadata.dialog_enhancement.data_present);
        assert_eq!(
            parsed.tools_metadata.dialog_enhancement.configuration,
            DialogEnhancementConfigurationUpdate::NotPresent
        );
        let body = parsed
            .tools_metadata
            .dialog_enhancement
            .unparsed_body(&bits[..len])
            .unwrap();
        assert!(body.is_empty());
        assert_eq!(body.bit_offset(), 37);
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
             0000001 0 \
             0 \
             1 \
             00000 00000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        let emdf = parsed
            .emdf_payloads
            .expect("标志为真时应保留 EMDF envelope");

        assert!(parsed.emdf_payloads_substream);
        assert_eq!(emdf.payload_count, 0);
        assert_eq!(emdf.payload_bytes, 0);
        assert_eq!(emdf.align_bits, 5);
        assert_eq!(parsed.metadata_bytes, 3);
    }

    #[test]
    fn preserves_inline_emdf_config_and_opaque_bytes() {
        // audio_size=0、最短 metadata；内嵌 EMDF ID 1 使用最简 discard config，
        // payload 为两个不在字节边界上的 8 比特元素，随后 ID 0 与两位对齐。
        let (bits, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000001 0 \
             0 \
             1 \
             00001 \
             0 0 0 0 1 \
             00000010 0 \
             10100101 01011010 \
             00000 00",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        let emdf = parsed.emdf_payloads.unwrap();
        let payload = *emdf.payloads().first().unwrap();

        assert_eq!(emdf.payload_count, 1);
        assert_eq!(payload.id, 1);
        assert!(payload.config.discard_unknown_payload);
        assert_eq!(payload.size_bytes, 2);
        assert_eq!(payload.bit_offset(), 49);
        assert_eq!(
            payload
                .bytes(&bits[..len])
                .unwrap()
                .iter()
                .collect::<std::vec::Vec<_>>(),
            [0xa5, 0x5a]
        );
        assert_eq!(emdf.align_bits, 2);
        assert_eq!(parsed.metadata_bytes, 7);
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
             0 0 0 0 0000001 0 0 0 00",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.audio_offset, 2);
        assert_eq!(parsed.audio_payload(&bits[..len]), Some(&[0xA5, 0x5A][..]));

        // b_more_bits = 1，variable_bits(7) 再加一个分片：头部长 24 位。
        let (bits, len) = pack(
            "000000000000010 1 0000000 0 \
             11110000 00001111 \
             0 0 0 0 0000001 0 0 0 00",
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
            "000000000000001 0 10101010 0 0 0 0 0000001 0 0 0 00 \
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
             0 0 0 0 0000001 0 0 0 00",
        );

        assert!(matches!(
            Ac4AudioSubstream::parse(&bits[..len], AJOC),
            Err(AudioSubstreamError::Read(ReadError::ValueOverflow { .. }))
        ));
    }

    /// dependent frame 可更新配置，后续 de_data body 仍保持在独立边界内。
    #[test]
    fn parses_dependent_dialogue_enhancement_configuration_and_preserves_data_body() {
        // tools_metadata_size = 13：presence、config flag、7-bit config 与四个 data bit。
        let (bits, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0001101 0 \
             1 1 10 11 110 1011 \
             0 \
             000000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(parsed.audio_size, 0);
        assert_eq!(parsed.tools_metadata_bits, 13);
        assert!(parsed.tools_metadata.dialog_enhancement.data_present);
        assert_eq!(
            parsed.tools_metadata.dialog_enhancement.configuration,
            DialogEnhancementConfigurationUpdate::New(DialogEnhancementConfiguration {
                method: 2,
                max_gain: 3,
                channel_config: 6,
            })
        );
        let DialogEnhancementConfigurationUpdate::New(configuration) =
            parsed.tools_metadata.dialog_enhancement.configuration
        else {
            panic!("应得到新配置")
        };
        assert_eq!(configuration.channel_count(), 2);
        let tools = parsed.tools_metadata.bits(&bits[..len]).unwrap();
        assert_eq!(tools.len_bits(), 13);
        let body = parsed
            .tools_metadata
            .dialog_enhancement
            .unparsed_body(&bits[..len])
            .unwrap();
        assert_eq!(body.len_bits(), 4);
        assert_eq!(body.bit_offset(), 37);
        assert_eq!(
            body.iter().collect::<std::vec::Vec<_>>(),
            [true, false, true, true]
        );
        assert_eq!(
            body.end_bit_offset(),
            parsed
                .tools_metadata
                .bits(&bits[..len])
                .unwrap()
                .end_bit_offset()
        );
    }

    #[test]
    fn iframe_configuration_is_mandatory_while_dependent_frames_can_keep_it() {
        let iframe = SubstreamContext {
            b_iframe: Some(true),
            ..AJOC
        };
        let (bits, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0001000 0 \
             1 01 10 000 \
             0 \
             000",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], iframe).unwrap();
        assert_eq!(
            parsed.tools_metadata.dialog_enhancement.configuration,
            DialogEnhancementConfigurationUpdate::New(DialogEnhancementConfiguration {
                method: 1,
                max_gain: 2,
                channel_config: 0,
            })
        );
        assert!(
            parsed
                .tools_metadata
                .dialog_enhancement
                .unparsed_body(&bits[..len])
                .unwrap()
                .is_empty()
        );

        let (bits, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000010 0 \
             1 0 \
             0 \
             0",
        );
        let parsed = Ac4AudioSubstream::parse(&bits[..len], AJOC).unwrap();
        assert_eq!(
            parsed.tools_metadata.dialog_enhancement.configuration,
            DialogEnhancementConfigurationUpdate::KeepPrevious
        );
        assert!(
            parsed
                .tools_metadata
                .dialog_enhancement
                .unparsed_body(&bits[..len])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn validates_de_channel_configuration_against_mono_and_stereo() {
        for code in 0u8..=7 {
            let source = [code << 1];
            let mono =
                parse_dialog_enhancement_configuration(&mut BitReader::new(&source), Some(0));
            assert_eq!(mono.is_ok(), matches!(code, 0 | 1), "mono code {code}");

            let stereo =
                parse_dialog_enhancement_configuration(&mut BitReader::new(&source), Some(1));
            assert_eq!(
                stereo.is_ok(),
                matches!(code, 0 | 2 | 4 | 6),
                "stereo code {code}"
            );

            assert!(
                parse_dialog_enhancement_configuration(&mut BitReader::new(&source), Some(2),)
                    .is_ok(),
                "multichannel code {code}"
            );
            assert!(
                parse_dialog_enhancement_configuration(&mut BitReader::new(&source), None).is_ok(),
                "undefined channel mode code {code}"
            );
        }

        let error =
            parse_dialog_enhancement_configuration(&mut BitReader::new(&[0b0000_0100]), Some(0))
                .unwrap_err();
        assert_eq!(
            error,
            AudioSubstreamError::InvalidDialogEnhancementChannelConfiguration {
                declared: 2,
                channel_mode: 0,
                bit_position: 4,
            }
        );
    }

    #[test]
    fn rejects_zero_short_and_trailing_tools_metadata_without_borrowing_following_bits() {
        let (zero, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000000 0 \
             1 00000 000000",
        );
        assert_eq!(
            Ac4AudioSubstream::parse(&zero[..len], AJOC).unwrap_err(),
            AudioSubstreamError::InvalidToolsMetadataSize {
                declared: 0,
                minimum: 1,
                bit_position: 20,
            }
        );

        let (short_active, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000001 0 \
             1 \
             0 00",
        );
        assert_eq!(
            Ac4AudioSubstream::parse(&short_active[..len], AJOC).unwrap_err(),
            AudioSubstreamError::InvalidToolsMetadataSize {
                declared: 1,
                minimum: 2,
                bit_position: 20,
            }
        );

        let iframe = SubstreamContext {
            b_iframe: Some(true),
            ..AJOC
        };
        let (short_iframe, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000111 0 \
             1 000000 \
             0 \
             0000",
        );
        assert_eq!(
            Ac4AudioSubstream::parse(&short_iframe[..len], iframe).unwrap_err(),
            AudioSubstreamError::InvalidToolsMetadataSize {
                declared: 7,
                minimum: 8,
                bit_position: 20,
            }
        );

        let (short_dependent_update, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0001000 0 \
             1 1 000000 \
             0 \
             000",
        );
        assert_eq!(
            Ac4AudioSubstream::parse(&short_dependent_update[..len], AJOC).unwrap_err(),
            AudioSubstreamError::InvalidToolsMetadataSize {
                declared: 8,
                minimum: 9,
                bit_position: 20,
            }
        );

        let unknown_iframe = SubstreamContext {
            b_iframe: None,
            ..AJOC
        };
        assert!(
            Ac4AudioSubstream::parse(&[0, 0, 0, 0x20], unknown_iframe).is_ok(),
            "DE 缺席时不得无谓要求逐 substream b_iframe"
        );
        let (active_without_context, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000010 0 \
             1 0 \
             0 \
             0",
        );
        assert!(matches!(
            Ac4AudioSubstream::parse(&active_without_context[..len], unknown_iframe),
            Err(AudioSubstreamError::Unsupported {
                what: "active dialog_enhancement without an exact b_iframe context",
                bit_position: 29,
            })
        ));

        let (inactive_tail, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000010 0 \
             0 1 \
             0 0",
        );
        assert_eq!(
            Ac4AudioSubstream::parse(&inactive_tail[..len], AJOC).unwrap_err(),
            AudioSubstreamError::TrailingToolsMetadataBits {
                bit_position: 29,
                remaining_bits: 1,
            }
        );

        let (truncated, len) = pack(
            "000000000000000 0 \
             0 \
             0 0 0 \
             0000101 0 \
             0000",
        );
        assert_eq!(
            Ac4AudioSubstream::parse(&truncated[..len], AJOC).unwrap_err(),
            AudioSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 5,
                bit_position: 28,
                remaining_bits: 4,
            })
        );
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
             0000001 0 \
             0 \
             0 \
             000",
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

    #[test]
    fn current_substream_loudness_preserves_standalone_dialgate_flag() {
        let (bits, _) = pack(
            // sus_ver=1、presentation_ldn=false：只有 standalone
            // b_loudcorr_dialgate 为真，其余 presence flags 均为假。
            "1 0 0 0 0 0 0 0 0 0 0 0",
        );
        let mut reader = BitReader::new(&bits);

        let parsed = FurtherLoudnessInfo::parse(&mut reader, 1, false).unwrap();

        assert_eq!(parsed.loudness_version, None);
        assert_eq!(parsed.loud_prac_type, None);
        assert_eq!(parsed.loudcorr_dialgate, Some(true));
        assert_eq!(parsed.loudcorr_dialgate_prac_type, None);
        assert_eq!(parsed.loudcorr_type, None);
        assert_eq!(reader.bit_position(), 12);
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

        assert_eq!(parsed.loudness_version, Some(0));
        assert_eq!(parsed.extended_loudness_version, None);
        assert_eq!(parsed.effective_loudness_version(), Some(0));
        assert_eq!(parsed.loud_prac_type, Some(0));
        assert_eq!(parsed.loudcorr_dialgate, None);
        assert_eq!(parsed.loudcorr_type, None);
        assert_eq!(parsed.rtll_comp, Some(0b1010_0101));
        assert_eq!(parsed.extension_bits, Some(2));
        assert_eq!(parsed.extension_bits_offset, Some(31));
        let extension = parsed.extension_data(&bits).unwrap();
        assert_eq!(extension.len_bits(), 2);
        assert_eq!(extension.get(0), Some(true));
        assert_eq!(extension.get(1), Some(false));
        assert_eq!(extension.get(2), None);
        assert_eq!(extension.as_aligned_slice(), None);
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
             0000001 0 \
             0 \
             0 \
             00",
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
             0000001 0 \
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
             0000001 0 \
             0 \
             0 \
             000000",
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
