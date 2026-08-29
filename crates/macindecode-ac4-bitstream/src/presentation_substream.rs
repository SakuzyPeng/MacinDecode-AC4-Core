//! `ac4_presentation_substream()` 的选择、additional-data、响度、DRC、group gain、关联音频、
//! custom downmix 与 loudness correction。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.2.3`、`6.2.2.5`；选择字段语义见
//! `6.3.3.1.1` 至 `6.3.3.1.15`，additional-data 字段见
//! `6.3.3.1.16` 至 `6.3.3.1.18`；响度语法见 `6.2.7.3`，共享字段语义见
//! `TS103190-1:v1.4.1:4.3.12.3`；DRC envelope 见 `6.3.3.1.19` 至
//! `6.3.3.1.21`，其 I-frame 配置与 data envelope 见
//! `TS103190-1:v1.4.1:4.2.14.5` 至 `4.2.14.9`、`4.3.13.1` 至 `4.3.13.5`；
//! substream-group gain 见
//! `6.3.3.1.22` 至 `6.3.3.1.24`，
//! associated-audio 字段见 `6.3.3.1.25` 至 `6.3.3.1.26`，其码值语义见
//! `TS103190-1:v1.4.1:4.3.12.4.3` 至 `4.3.12.4.9`；custom downmix 语法与语义见
//! `TS103190-2:v1.3.1:6.2.9.2` 至 `6.2.9.10`、`6.3.10.2` 至 `6.3.10.3`，共享的
//! stereo downmix 码值见 `TS103190-1:v1.4.1:4.3.12.2.8` 至 `4.3.12.2.19`；
//! loudness correction 语法与码值见 `TS103190-2:v1.3.1:6.2.9.1`、`6.3.10.1`。
//!
//! 本模块解析 presentation 名称分片、播放目标、逐音频 substream 的
//! activation/dataset map，以及有界 additional-data 区域中的 immersive/OAMD timing 与
//! advanced dialogue-enhancement 原始码值，并保留 dialnorm、further loudness 和严格定界的
//! `drc_frame()` 原始比特；I-frame 的 `drc_config()` 会解析 decoder modes、profile 与
//! compression curve 原始参数，`drc_data()` 再解析 repeat profile、gain-set 长度/版本和
//! curve reset/reserved，并保留 gain-set body。模块还解析逐帧 substream-group gain 更新、
//! associated-audio scale/pan 码值、custom downmix 的配置、路由与 gain 码值，以及 loudness
//! correction 的 presence 与 5 比特原始码值。`PresentationDrcState` 可按 presentation 隔离
//! 前一有效配置并解析 dependent-frame data；`PresentationSubstreamGroupGainState` 以同样的
//! presentation 作用域延续 group gain 六比特码值。启用 `audio-decode` 时还可解码 DRC Huffman
//! gains 并还原整数码值。本模块不换算或应用任何 gain，也不执行其他处理。

use crate::audio_substream::FurtherLoudnessInfo;
use crate::presentation::MAX_GROUPS_PER_PRESENTATION;
use crate::reader::{BitReader, ReadError};
use crate::substream::MAX_LF_SUBSTREAMS;
use core::fmt;

#[cfg(feature = "audio-decode")]
mod drc_gains;
#[cfg(feature = "audio-decode")]
pub use drc_gains::{
    MAX_PRESENTATION_DRC_BANDS, MAX_PRESENTATION_DRC_CHANNEL_GROUPS,
    MAX_PRESENTATION_DRC_GAIN_VALUES, MAX_PRESENTATION_DRC_SUBFRAMES,
    PresentationDrcDecodedGainSet, PresentationDrcGains, PresentationDrcGainsContext,
    PresentationDrcGainsError,
};

/// alternative presentation 可保存的 target 数上限。
///
/// 规范用 `variable_bits(2)` 扩展该计数而未给出上限。本 crate 不分配内存，并把
/// 单 presentation 的 target 数限制为与 substream index table 相同的 32。
pub const MAX_ALTERNATIVE_PRESENTATION_TARGETS: usize = 32;

/// 当前拓扑容量下，一个 presentation 按语法顺序可包含的音频 substream 数上限。
///
/// `n_substreams_in_presentation` 按 `ac4_sgi_specifier()` 的外层顺序和每个 group 的
/// `ac4_substream_group_info()` 内层顺序计数，包含 dialogue-enhancement substream；
/// 它不是去重后的物理 substream 数。
pub const MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION: usize =
    MAX_GROUPS_PER_PRESENTATION * MAX_LF_SUBSTREAMS;

/// 一个 `custom_dmx_data()` 可声明的 downmix configuration 数上限。
///
/// `n_cdmx_configs_minus1` 为 2 比特，因此规范范围固定为 `1..=4`。
pub const MAX_CUSTOM_DOWNMIX_CONFIGURATIONS: usize = 4;

/// 一个 `drc_config()` 可声明的 decoder mode 数上限。
///
/// `drc_decoder_nr_modes` 为 3 比特且语义为 count minus one，因此固定为 `1..=8`。
pub const MAX_PRESENTATION_DRC_DECODER_MODES: usize = 8;

/// `drc_version` 与所有已知版本必需的首个 `drc_gain_val` 总位数。
const MIN_KNOWN_DRC_GAIN_SET_BITS: u32 = 2 + 7;

/// 已验证的 independent object/A-JOC presentation DRC 兼容尾字节。
const INDEPENDENT_OBJECT_DRC_COMPATIBILITY_BYTES: [u8; 2] = [0x00, 0x80];

/// presentation substream 前缀中超出固定容量的结构。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSubstreamCapacity {
    /// alternative target 数。
    Targets,
    /// 一个 presentation 中按语法顺序出现的音频 substream 数。
    AudioSubstreams,
    /// 一个 presentation 声明的 substream group 数。
    SubstreamGroups,
    /// presentation `drc_config()` 声明的 decoder mode 数。
    DrcDecoderModes,
    /// presentation `drc_data()` 携带的 gain set 数。
    DrcGainSets,
}

impl fmt::Display for PresentationSubstreamCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match *self {
            PresentationSubstreamCapacity::Targets => "alternative presentation targets",
            PresentationSubstreamCapacity::AudioSubstreams => "audio substreams in a presentation",
            PresentationSubstreamCapacity::SubstreamGroups => "substream groups in a presentation",
            PresentationSubstreamCapacity::DrcDecoderModes => "presentation DRC decoder modes",
            PresentationSubstreamCapacity::DrcGainSets => "presentation DRC gain sets",
        })
    }
}

/// presentation substream 前缀解析失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSubstreamError {
    /// 底层读取失败或变长字段溢出。
    Read(ReadError),
    /// alternative presentation 没有可映射的音频 substream。
    ///
    /// 这通常表示调用方没有取得完整的 SGI/group 拓扑，不能把未知计数当成零继续解析。
    MissingAudioSubstreams,
    /// `further_loudness_info()` 的扩展长度不足以容纳已声明字段。
    InvalidLoudnessExtensionSize {
        /// `e_bits_size` 声明的扩展区段长度。
        declared: u32,
        /// 当前标志组合至少需要的长度。
        required: u32,
        /// 检测到不一致时的比特偏移。
        bit_position: u64,
    },
    /// `drc_metadata_size` 不足以容纳必需的 `b_drc_present`。
    InvalidDrcMetadataSize {
        /// 码流声明的 `drc_frame()` 长度。
        declared: u32,
        /// 当前语法要求的最小长度。
        minimum: u32,
        /// `drc_metadata_size_value` 的比特偏移。
        bit_position: u64,
    },
    /// `drc_gainset_size` 不足以容纳当前版本的必需字段。
    InvalidDrcGainSetSize {
        /// 码流声明的 gain set 长度。
        declared: u32,
        /// 当前语法要求的最小长度。
        minimum: u32,
        /// `drc_gainset_size_value` 的比特偏移。
        bit_position: u64,
    },
    /// version 0 的单一 wideband gain-set 长度不是固定的 9 比特。
    InvalidFixedDrcGainSetSize {
        /// 码流声明的 gain set 长度。
        declared: u32,
        /// 当前语法要求的精确长度。
        expected: u32,
        /// `drc_gainset_size_value` 的比特偏移。
        bit_position: u64,
    },
    /// dependent frame 携带 DRC data，但当前 presentation 没有前一有效配置。
    MissingDrcConfiguration {
        /// `drc_data()` 在 presentation payload 内的起始比特偏移。
        bit_position: u64,
    },
    /// repeat profile 引用了配置中不存在的 decoder mode ID。
    MissingDrcRepeatProfile {
        /// 携带 repeat profile 的 decoder mode ID。
        mode_id: u8,
        /// 未找到的 3 比特 `drc_repeat_id`。
        repeat_id: u8,
    },
    /// repeat profile 引用形成循环，无法派生 gain/curve 传输形态。
    CyclicDrcRepeatProfile {
        /// 检测到循环的 decoder mode ID。
        mode_id: u8,
    },
    /// 已知 `drc_frame()` 语法解析完成后仍有尾随比特。
    TrailingDrcFrameBits {
        /// 首个尾随比特在 presentation payload 内的偏移。
        bit_position: u64,
        /// 尚未消费的 DRC frame 比特数。
        remaining_bits: u64,
    },
    /// `pan_associated` 使用了规范禁止的 `0xf0..=0xff` 码值。
    ReservedAssociatedPan {
        /// 8 比特 `pan_associated` 原值。
        pan_associated: u8,
        /// `pan_associated` 在 payload 内的比特偏移。
        bit_position: u64,
    },
    /// `out_ch_config` 使用了表 127 标为 unused 的 `5..=7`。
    UnusedCustomDownmixOutputChannelConfig {
        /// 3 比特 `out_ch_config` 原值。
        output_channel_config: u8,
        /// `out_ch_config` 在 payload 内的比特偏移。
        bit_position: u64,
    },
    /// LoRo/LtRt surround mix gain 使用了表 149a 的保留码 `0` 或 `1`。
    ReservedStereoSurroundMixGain {
        /// 出错的是 LoRo 还是 LtRt 系数。
        kind: PresentationStereoDownmixKind,
        /// 3 比特 surround mix gain 原值。
        gain_code: u8,
        /// surround mix gain 在 payload 内的比特偏移。
        bit_position: u64,
    },
    /// `loud_corr()` 后的 `byte_align` 未落在 presentation payload 末尾。
    TrailingBits {
        /// 对齐后仍未消费的比特数；完整字节 payload 下必为 8 的倍数。
        remaining_bits: u64,
    },
    /// 结构规模超出固定容量。
    CapacityExceeded {
        /// 超限的结构种类。
        what: PresentationSubstreamCapacity,
        /// 码流或上下文声明的数量。
        declared: u32,
        /// 实现上限。
        limit: usize,
    },
}

impl fmt::Display for PresentationSubstreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            PresentationSubstreamError::Read(error) => {
                write!(
                    formatter,
                    "Failed to read presentation substream metadata: {error}"
                )
            }
            PresentationSubstreamError::MissingAudioSubstreams => formatter.write_str(
                "Alternative presentation selection requires at least one mapped audio substream",
            ),
            PresentationSubstreamError::InvalidLoudnessExtensionSize {
                declared,
                required,
                bit_position,
            } => write!(
                formatter,
                "Presentation loudness e_bits_size is {declared} at bit offset {bit_position}; at least {required} bits are required"
            ),
            PresentationSubstreamError::InvalidDrcMetadataSize {
                declared,
                minimum,
                bit_position,
            } => write!(
                formatter,
                "Presentation DRC metadata size is {declared} bits at offset {bit_position}; at least {minimum} bit is required"
            ),
            PresentationSubstreamError::InvalidDrcGainSetSize {
                declared,
                minimum,
                bit_position,
            } => write!(
                formatter,
                "Presentation DRC gain-set size is {declared} bits at offset {bit_position}; at least {minimum} bits are required"
            ),
            PresentationSubstreamError::InvalidFixedDrcGainSetSize {
                declared,
                expected,
                bit_position,
            } => write!(
                formatter,
                "Presentation fixed DRC gain-set size is {declared} bits at offset {bit_position}; exactly {expected} bits are required"
            ),
            PresentationSubstreamError::MissingDrcConfiguration { bit_position } => write!(
                formatter,
                "Presentation DRC data at bit offset {bit_position} requires a previous independent-frame configuration"
            ),
            PresentationSubstreamError::MissingDrcRepeatProfile { mode_id, repeat_id } => write!(
                formatter,
                "Presentation DRC decoder mode {mode_id} repeats missing mode {repeat_id}"
            ),
            PresentationSubstreamError::CyclicDrcRepeatProfile { mode_id } => write!(
                formatter,
                "Presentation DRC repeat profiles form a cycle at decoder mode {mode_id}"
            ),
            PresentationSubstreamError::TrailingDrcFrameBits {
                bit_position,
                remaining_bits,
            } => write!(
                formatter,
                "Presentation DRC frame has {remaining_bits} trailing bits at bit offset {bit_position}"
            ),
            PresentationSubstreamError::ReservedAssociatedPan {
                pan_associated,
                bit_position,
            } => write!(
                formatter,
                "Presentation associated-audio pan code {pan_associated:#04x} is reserved at bit offset {bit_position}"
            ),
            PresentationSubstreamError::UnusedCustomDownmixOutputChannelConfig {
                output_channel_config,
                bit_position,
            } => write!(
                formatter,
                "Presentation custom-downmix output channel configuration {output_channel_config} is unused at bit offset {bit_position}"
            ),
            PresentationSubstreamError::ReservedStereoSurroundMixGain {
                kind,
                gain_code,
                bit_position,
            } => write!(
                formatter,
                "Presentation {kind} surround mix-gain code {gain_code} is reserved at bit offset {bit_position}"
            ),
            PresentationSubstreamError::TrailingBits { remaining_bits } => write!(
                formatter,
                "Presentation substream has {remaining_bits} trailing bits after loud_corr and byte_align"
            ),
            PresentationSubstreamError::CapacityExceeded {
                what,
                declared,
                limit,
            } => write!(
                formatter,
                "{what} count {declared} exceeds implementation limit {limit}"
            ),
        }
    }
}

impl core::error::Error for PresentationSubstreamError {}

impl From<ReadError> for PresentationSubstreamError {
    fn from(error: ReadError) -> Self {
        Self::Read(error)
    }
}

/// 解析 alternative selection 前缀所需的 TOC 上下文。
///
/// 应优先由
/// [`crate::topology::Ac4Topology::presentation_substream_selection_context`] 取得；构造
/// 测试也可直接使用 [`Self::new`]。substream 数按规范的 outer-loop/inner-loop 顺序
/// 计数，不按物理 index 去重。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationSubstreamSelectionContext {
    alternative: bool,
    n_audio_substreams: u32,
}

impl PresentationSubstreamSelectionContext {
    /// 构造 selection 前缀上下文。
    #[must_use]
    pub const fn new(alternative: bool, n_audio_substreams: u32) -> Self {
        Self {
            alternative,
            n_audio_substreams,
        }
    }

    /// TOC 中的 `b_alternative`。
    #[must_use]
    pub const fn alternative(self) -> bool {
        self.alternative
    }

    /// presentation 内按规范顺序出现的音频 substream 数。
    #[must_use]
    pub const fn n_audio_substreams(self) -> u32 {
        self.n_audio_substreams
    }
}

/// presentation 的声道与 core downmix 派生上下文。
///
/// 这五个值分别对应 P2 `6.3.3.1.27`–`6.3.3.1.31` 中传给
/// `custom_dmx_data()` 的同名 helper。`None` 精确表示规范伪码中的 `-1`，
/// 不是「已定义但具体值未知」。应优先从
/// [`crate::topology::Ac4Topology::presentation_substream_context`] 取得。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationChannelContext {
    presentation_channel_mode: Option<u8>,
    core_channel_mode: Option<u8>,
    four_back_channels_present: bool,
    top_channel_pairs: u8,
    has_lfe: bool,
}

impl PresentationChannelContext {
    /// 没有可形成声道模式的 presentation 上下文。
    pub const UNDEFINED: Self = Self {
        presentation_channel_mode: None,
        core_channel_mode: None,
        four_back_channels_present: false,
        top_channel_pairs: 0,
        has_lfe: false,
    };

    /// 以已派生的规范 helper 构造上下文。
    #[must_use]
    pub const fn new(
        presentation_channel_mode: Option<u8>,
        core_channel_mode: Option<u8>,
        four_back_channels_present: bool,
        top_channel_pairs: u8,
        has_lfe: bool,
    ) -> Self {
        Self {
            presentation_channel_mode,
            core_channel_mode,
            four_back_channels_present,
            top_channel_pairs,
            has_lfe,
        }
    }

    /// `pres_ch_mode`；`None` 对应 `-1`。
    #[must_use]
    pub const fn presentation_channel_mode(self) -> Option<u8> {
        self.presentation_channel_mode
    }

    /// `pres_ch_mode_core`；`None` 对应 `-1`。
    #[must_use]
    pub const fn core_channel_mode(self) -> Option<u8> {
        self.core_channel_mode
    }

    /// `b_pres_4_back_channels_present`。
    #[must_use]
    pub const fn four_back_channels_present(self) -> bool {
        self.four_back_channels_present
    }

    /// `pres_top_channel_pairs`，规范取值为 `0..=2`。
    #[must_use]
    pub const fn top_channel_pairs(self) -> u8 {
        self.top_channel_pairs
    }

    /// `b_pres_has_lfe`。
    #[must_use]
    pub const fn has_lfe(self) -> bool {
        self.has_lfe
    }
}

/// 解析完整 presentation substream 前缀所需的 TOC/拓扑上下文。
///
/// `n_substream_groups` 是 presentation 语法声明的角色 group 数，用于读取逐 group gain；它与
/// [`PresentationSubstreamSelectionContext::n_audio_substreams`] 的音频 substream 数以及 SGI
/// specifier 数都不是同一概念。
///
/// `presentation_is_independent` 必须取自同一 presentation substream info 的 `b_pres_ndot`，
/// 用于判定当前 `drc_frame()` 是否携带配置。
///
/// [`PresentationChannelContext::presentation_channel_mode`] 为 `None` 时，
/// additional-data envelope 内会多传一个 `b_oamd_common_timing`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationSubstreamContext {
    selection: PresentationSubstreamSelectionContext,
    presentation_is_independent: bool,
    n_substream_groups: u32,
    channel: PresentationChannelContext,
}

impl Default for PresentationSubstreamContext {
    fn default() -> Self {
        Self::new(false, false, 0, 0, PresentationChannelContext::UNDEFINED)
    }
}

impl PresentationSubstreamContext {
    /// 构造完整解析上下文。
    #[must_use]
    pub const fn new(
        alternative: bool,
        presentation_is_independent: bool,
        n_audio_substreams: u32,
        n_substream_groups: u32,
        channel: PresentationChannelContext,
    ) -> Self {
        Self {
            selection: PresentationSubstreamSelectionContext::new(alternative, n_audio_substreams),
            presentation_is_independent,
            n_substream_groups,
            channel,
        }
    }

    /// selection 前缀所需的上下文子集。
    #[must_use]
    pub const fn selection_context(self) -> PresentationSubstreamSelectionContext {
        self.selection
    }

    /// presentation substream 的 `b_pres_ndot`，即 `drc_frame()` 的 `b_iframe` 参数。
    #[must_use]
    pub const fn presentation_is_independent(self) -> bool {
        self.presentation_is_independent
    }

    /// presentation 的规范 `n_substream_groups`，用于判定并读取 group gain 数组。
    ///
    /// 该值不一定等于 SGI specifier 数；config 1/4 的 dialogue-enhancement SGI 不增加此计数。
    #[must_use]
    pub const fn n_substream_groups(self) -> u32 {
        self.n_substream_groups
    }

    /// presentation 的完整声道与 core downmix 派生上下文。
    #[must_use]
    pub const fn channel_context(self) -> PresentationChannelContext {
        self.channel
    }

    /// presentation 的 `pres_ch_mode` 是否为规范中的未定义值 `-1`。
    #[must_use]
    pub const fn pres_ch_mode_undefined(self) -> bool {
        self.channel.presentation_channel_mode().is_none()
    }
}

/// `advanced_de_data()` 中仅在配置存在时传输的 compressor 配置原值。
///
/// 这些字段只被解析和保留；本 crate 不在 presentation 层执行 dialogue enhancement。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvancedDeConfig {
    /// `advanced_de_compr_tc_attack`，6 比特。
    pub compressor_time_constant_attack: u8,
    /// `advanced_de_compr_tc_release`，6 比特。
    pub compressor_time_constant_release: u8,
    /// `advanced_de_compr_ratio`，4 比特。
    pub compressor_ratio: u8,
}

/// `advanced_de_data()` 的原始参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvancedDeData {
    /// `b_advanced_de_config_present` 控制的 compressor 配置。
    pub config: Option<AdvancedDeConfig>,
    /// `advanced_de_compr_thresh`，按 6 比特二进制补码解释，范围 `-32..=31`。
    pub compressor_threshold: i8,
    /// `advanced_de_compr_gain`，5 比特原值。
    pub compressor_gain: u8,
}

impl AdvancedDeData {
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, ReadError> {
        let config = if reader.read_flag()? {
            Some(AdvancedDeConfig {
                compressor_time_constant_attack: u8::try_from(reader.read_bits(6)?).unwrap_or(0),
                compressor_time_constant_release: u8::try_from(reader.read_bits(6)?).unwrap_or(0),
                compressor_ratio: u8::try_from(reader.read_bits(4)?).unwrap_or(0),
            })
        } else {
            None
        };
        let threshold_raw = u8::try_from(reader.read_bits(6)?).unwrap_or(0);
        let threshold =
            i16::from(threshold_raw).saturating_sub(if threshold_raw >= 32 { 64 } else { 0 });
        Ok(Self {
            config,
            compressor_threshold: i8::try_from(threshold).unwrap_or(0),
            compressor_gain: u8::try_from(reader.read_bits(5)?).unwrap_or(0),
        })
    }
}

/// presentation additional-data envelope 中保留的 `add_data` 比特视图。
///
/// 已知字段结束位置通常不在字节边界，因此本类型保留精确 bit offset/length 并按需读取，
/// 不分配或复制。该区域由声明的 `add_data_bytes` 严格定界。
#[derive(Debug, Clone, Copy)]
pub struct PresentationAddDataBits<'a> {
    source: &'a [u8],
    bit_offset: u64,
    bit_len: u64,
}

impl<'a, 'b> PartialEq<PresentationAddDataBits<'b>> for PresentationAddDataBits<'a> {
    fn eq(&self, other: &PresentationAddDataBits<'b>) -> bool {
        self.bit_len == other.bit_len && (*self).iter().eq((*other).iter())
    }
}

impl Eq for PresentationAddDataBits<'_> {}

impl<'a> PresentationAddDataBits<'a> {
    /// 视图起点相对原 payload 的比特偏移。
    #[must_use]
    pub const fn bit_offset(self) -> u64 {
        self.bit_offset
    }

    /// 视图末尾相对原 payload 的比特偏移。
    #[must_use]
    pub const fn end_bit_offset(self) -> u64 {
        self.bit_offset.saturating_add(self.bit_len)
    }

    /// 视图包含的比特数。
    #[must_use]
    pub const fn len_bits(self) -> u64 {
        self.bit_len
    }

    /// 视图是否为空。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bit_len == 0
    }

    /// 读取一个比特。
    #[must_use]
    pub fn get(self, index: u64) -> Option<bool> {
        if index >= self.bit_len {
            return None;
        }
        let mut reader = BitReader::new(self.source);
        reader.skip_bits(self.bit_offset.checked_add(index)?).ok()?;
        reader.read_flag().ok()
    }

    /// 若起点和长度都在字节边界上，返回零拷贝字节切片。
    #[must_use]
    pub fn as_aligned_slice(self) -> Option<&'a [u8]> {
        if !self.bit_offset.is_multiple_of(8) || !self.bit_len.is_multiple_of(8) {
            return None;
        }
        let start = usize::try_from(self.bit_offset / 8).ok()?;
        let len = usize::try_from(self.bit_len / 8).ok()?;
        self.source.get(start..start.checked_add(len)?)
    }

    /// 按码流顺序遍历保留比特。
    #[must_use]
    pub fn iter(self) -> PresentationAddDataBitIter<'a> {
        PresentationAddDataBitIter::new(self)
    }
}

/// `drc_metadata_size` 严格定界的完整 `drc_frame()` 原始比特视图。
///
/// 完整 frame 始终保留；I-frame 还会解析 `drc_config()` 与 `drc_data()` 的结构包络，同时继续
/// 以同型 bit view 保留完整 data 和尚未解释的 gain-set body。本模块不换算或应用 DRC。
pub type PresentationDrcFrameBits<'a> = PresentationAddDataBits<'a>;

/// 自定义 DRC decoder mode 的参考输出电平范围原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationDrcOutputLevelRange {
    /// 5 比特 `drc_output_level_from`。
    pub level_from: u8,
    /// 5 比特 `drc_output_level_to`。
    pub level_to: u8,
}

/// compression curve 中可选的额外 control point 原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationDrcCurveSection {
    /// `drc_gain_section_boost`（4 比特）或 `drc_gain_section_cut`（5 比特）。
    pub gain: u8,
    /// 5 比特 `drc_lev_section_boost` 或 `drc_lev_section_cut`。
    pub level: u8,
}

/// 非默认 DRC time constants 与可选 adaptive thresholds 的原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationDrcTimeConstants {
    /// 8 比特 `drc_tc_attack`。
    pub attack: u8,
    /// 8 比特 `drc_tc_release`。
    pub release: u8,
    /// 8 比特 `drc_tc_attack_fast`。
    pub attack_fast: u8,
    /// 8 比特 `drc_tc_release_fast`。
    pub release_fast: u8,
    /// adaptive smoothing 为真时的 `(drc_attack_threshold, drc_release_threshold)`。
    pub adaptive_thresholds: Option<(u8, u8)>,
}

/// `drc_compression_curve()` 的全部原始参数。
///
/// `level_max_*` 的 `None` 表示对应 `gain_max_*` 为零而未传输该分支；`*_section` 的
/// `None` 在该分支适用时表示 section presence bit 为零。`time_constants == None` 表示
/// `drc_tc_default_flag == 1`。本类型不把码值换算为 dB 或时间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationDrcCompressionCurve {
    /// 4 比特 `drc_lev_nullband_low`。
    pub nullband_low: u8,
    /// 4 比特 `drc_lev_nullband_high`。
    pub nullband_high: u8,
    /// 4 比特 `drc_gain_max_boost`。
    pub gain_max_boost: u8,
    /// `drc_gain_max_boost > 0` 时的 5 比特 `drc_lev_max_boost`。
    pub level_max_boost: Option<u8>,
    /// boost section presence 控制的额外 control point。
    pub boost_section: Option<PresentationDrcCurveSection>,
    /// 5 比特 `drc_gain_max_cut`。
    pub gain_max_cut: u8,
    /// `drc_gain_max_cut > 0` 时的 6 比特 `drc_lev_max_cut`。
    pub level_max_cut: Option<u8>,
    /// cut section presence 控制的额外 control point。
    pub cut_section: Option<PresentationDrcCurveSection>,
    /// 非默认 time constants；`None` 表示使用规范默认值。
    pub time_constants: Option<PresentationDrcTimeConstants>,
}

/// 一个 DRC decoder mode 的 profile 传输形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationDrcProfile {
    /// `drc_repeat_profile_flag == 1`，复用指定 mode ID。
    Repeat {
        /// 3 比特 `drc_repeat_id`。
        repeat_id: u8,
    },
    /// `drc_default_profile_flag == 1`，使用 `drc_eac3_profile` 指定的默认 profile。
    DefaultEac3,
    /// 显式传输 compression curve。
    CompressionCurve(PresentationDrcCompressionCurve),
    /// 按帧传输 DRC gains，并保留 2 比特 `drc_gains_config`。
    Gains {
        /// 2 比特 gains configuration 原值。
        configuration: u8,
    },
}

/// 一个 `drc_decoder_mode_config()` 的原始配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationDrcDecoderMode {
    /// 3 比特 `drc_decoder_mode_id`。
    pub mode_id: u8,
    /// `mode_id > 3` 时传输的自定义参考输出电平范围。
    pub output_levels: Option<PresentationDrcOutputLevelRange>,
    /// repeat/default/curve/gains 四种互斥 profile 形态。
    pub profile: PresentationDrcProfile,
}

impl PresentationDrcDecoderMode {
    const EMPTY: Self = Self {
        mode_id: 0,
        output_levels: None,
        profile: PresentationDrcProfile::DefaultEac3,
    };
}

/// I-frame 中 `drc_config()` 的 decoder modes 与 (E-)AC-3 profile 原始码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationDrcConfiguration {
    decoder_mode_count_minus_one: u8,
    decoder_modes: [PresentationDrcDecoderMode; MAX_PRESENTATION_DRC_DECODER_MODES],
    /// 3 比特 `drc_eac3_profile` 原值；保留码 `6..=7` 也原样保留。
    pub eac3_profile: u8,
}

impl PresentationDrcConfiguration {
    /// 3 比特 `drc_decoder_nr_modes` 原值。
    #[must_use]
    pub const fn decoder_mode_count_minus_one(self) -> u8 {
        self.decoder_mode_count_minus_one
    }

    /// 按码流顺序取得 `1..=8` 个 decoder mode 配置。
    #[must_use]
    pub fn decoder_modes(&self) -> &[PresentationDrcDecoderMode] {
        let len = usize::from(self.decoder_mode_count_minus_one).saturating_add(1);
        self.decoder_modes.get(..len).unwrap_or(&[])
    }
}

/// 一个 presentation 的前一有效 `drc_config()`。
///
/// 状态必须按 presentation 隔离；seek、换源、拓扑变化或调用方检测到不连续时应调用
/// [`reset`](Self::reset)。[`Ac4PresentationSubstream::parse_with_drc_state`] 在完整 I-frame
/// 成功后替换配置，在 DRC 缺席的 I-frame 成功后清空配置，并用已有配置解析 dependent frame
/// 的 `drc_data()`。所有更新均在完整 presentation payload 验证成功后事务性提交。
///
/// 本类型只维护解析语法所需的配置，不维护 DRC gain、平滑器或任何 PCM 处理状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationDrcState {
    configuration: Option<PresentationDrcConfiguration>,
}

impl PresentationDrcState {
    /// 创建不含历史配置的状态。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            configuration: None,
        }
    }

    /// 当前 presentation 最近一次成功提交的有效 DRC 配置。
    #[must_use]
    pub const fn configuration(self) -> Option<PresentationDrcConfiguration> {
        self.configuration
    }

    /// 在 seek、换源、拓扑变化或不连续处清除历史配置。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn resolve<'a>(
        &mut self,
        frame: &mut Ac4PresentationSubstream<'a>,
        independent: bool,
    ) -> Result<(), PresentationSubstreamError> {
        let mut next = *self;
        if independent {
            next.configuration = None;
        }

        if frame.drc_present {
            if let Some(configuration) = frame.drc_configuration {
                next.configuration = Some(configuration);
            } else {
                let configuration = next.configuration.ok_or(
                    PresentationSubstreamError::MissingDrcConfiguration {
                        bit_position: frame.drc_data.bit_offset,
                    },
                )?;
                frame.drc_data_elements =
                    Some(parse_drc_data_view(frame.drc_data, &configuration)?);
            }
        }

        *self = next;
        Ok(())
    }
}

/// 一个 decoder mode 的有界 DRC gain-set envelope。
///
/// [`payload`](Self::payload) 从 2 比特 `drc_version` 开始，长度严格等于码流声明的
/// `drc_gainset_size`。默认构建只解析版本并保留其后的 body；启用 `audio-decode` 后可调用
/// `decode_gains()` 解码版本 `0..=1` 的 `drc_gains()` 并定出 `drc2_bits` 边界。
#[derive(Debug, Clone, Copy)]
pub struct PresentationDrcGainSet<'a> {
    /// 当前 gain set 对应的 3 比特 decoder mode ID。
    pub decoder_mode_id: u8,
    /// repeat profile 解析后的 2 比特 `drc_gains_config`。
    pub gains_configuration: u8,
    /// `drc_gainset_size_value` 在 presentation payload 内的精确比特偏移。
    pub size_value_offset: u64,
    /// 2 比特 `drc_version` 原值。
    pub version: u8,
    /// 由 `drc_gainset_size` 严格定界、包含 `drc_version` 的完整 payload。
    pub payload: PresentationDrcFrameBits<'a>,
}

impl<'a, 'b> PartialEq<PresentationDrcGainSet<'b>> for PresentationDrcGainSet<'a> {
    fn eq(&self, other: &PresentationDrcGainSet<'b>) -> bool {
        self.decoder_mode_id == other.decoder_mode_id
            && self.gains_configuration == other.gains_configuration
            && self.size_value_offset == other.size_value_offset
            && self.version == other.version
            && self.payload == other.payload
    }
}

impl Eq for PresentationDrcGainSet<'_> {}

impl<'a> PresentationDrcGainSet<'a> {
    const EMPTY: Self = Self {
        decoder_mode_id: 0,
        gains_configuration: 0,
        size_value_offset: 0,
        version: 0,
        payload: PresentationAddDataBits {
            source: &[],
            bit_offset: 0,
            bit_len: 0,
        },
    };

    /// `drc_version` 之后尚未解释的 gain/extension body。
    #[must_use]
    pub const fn body(self) -> PresentationDrcFrameBits<'a> {
        PresentationAddDataBits {
            source: self.payload.source,
            bit_offset: self.payload.bit_offset.saturating_add(2),
            bit_len: self.payload.bit_len.saturating_sub(2),
        }
    }
}

/// compression-curve mode 共用的逐帧 reset 与保留码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationDrcCurveData {
    /// `drc_reset_flag`。
    pub reset: bool,
    /// 2 比特 `drc_reserved` 原值。
    pub reserved: u8,
}

/// I-frame 中按有效 DRC 配置解析的 `drc_data()` 结构包络。
///
/// gain-set body 始终保持原始 bit view；`audio-decode` 下可另行熵解码，但仍不执行或维护 DRC
/// 状态。
#[derive(Debug, Clone, Copy)]
pub struct PresentationDrcData<'a> {
    gain_set_count: u8,
    gain_sets: [PresentationDrcGainSet<'a>; MAX_PRESENTATION_DRC_DECODER_MODES],
    /// 至少一个有效 decoder mode 使用 compression curve 时的共用 frame data。
    pub curve: Option<PresentationDrcCurveData>,
}

impl<'a, 'b> PartialEq<PresentationDrcData<'b>> for PresentationDrcData<'a> {
    fn eq(&self, other: &PresentationDrcData<'b>) -> bool {
        self.gain_set_count == other.gain_set_count
            && self
                .gain_sets()
                .iter()
                .zip(other.gain_sets())
                .all(|(left, right)| left == right)
            && self.curve == other.curve
    }
}

impl Eq for PresentationDrcData<'_> {}

impl<'a> PresentationDrcData<'a> {
    /// 按 `drc_config()` 的 decoder-mode 顺序取得所有 gain-set envelope。
    #[must_use]
    pub fn gain_sets(&self) -> &[PresentationDrcGainSet<'a>] {
        let len = usize::from(self.gain_set_count);
        self.gain_sets.get(..len).unwrap_or(&[])
    }
}

/// 当前帧传输的逐 substream-group `sg_gain` 六比特码值。
///
/// 最多保存 [`MAX_GROUPS_PER_PRESENTATION`] 个值，不做 dB 换算或增益应用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationSubstreamGroupGainCodes {
    codes: [u8; MAX_GROUPS_PER_PRESENTATION],
    len: usize,
}

impl PresentationSubstreamGroupGainCodes {
    const fn zeros(len: usize) -> Self {
        Self {
            codes: [0; MAX_GROUPS_PER_PRESENTATION],
            len,
        }
    }

    /// 按 presentation group 顺序取得全部六比特码值。
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.codes.get(..self.len).unwrap_or(&[])
    }

    /// 码值数量。
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// 是否没有码值。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 取得一个 group 的六比特码值。
    #[must_use]
    pub fn get(&self, index: usize) -> Option<u8> {
        self.as_slice().get(index).copied()
    }
}

/// 当前帧的 substream-group gain 原始更新形态。
///
/// 本枚举保留 `b_substream_group_gains_present`/`b_keep` 的码流语义；可交给
/// [`PresentationSubstreamGroupGainState::apply`] 取得当前帧的有效六比特码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSubstreamGroupGainUpdate {
    /// `n_substream_groups <= 1`，语法中不传输 group-gain presence bit。
    NotSignaled,
    /// presence bit 为零，本帧不携带 group-gain 更新。
    NotPresent,
    /// `b_keep` 为真，应沿用上一帧有效值；首次接收前的规范默认值为 0 dB。
    KeepPrevious,
    /// `b_keep` 为假，本帧按 group 顺序传输一组新码值。
    NewValues(PresentationSubstreamGroupGainCodes),
}

/// 延续 presentation substream-group gain 时的状态错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSubstreamGroupGainStateError {
    /// group 数超过固定容量。
    CapacityExceeded {
        /// 上下文声明的 group 数。
        declared: u32,
        /// 实现上限。
        limit: usize,
    },
    /// 逐帧更新形态与 `n_substream_groups` 的语法 gate 不一致。
    InconsistentUpdate {
        /// 上下文声明的 group 数。
        declared: u32,
        /// 调用方提供的逐帧更新。
        update: PresentationSubstreamGroupGainUpdate,
    },
    /// dependent frame 的 group 数与已有状态不同，无法无歧义映射旧码值。
    SubstreamGroupCountChanged {
        /// 状态中前一有效 group 数。
        previous: usize,
        /// 当前上下文声明的 group 数。
        current: usize,
    },
}

impl fmt::Display for PresentationSubstreamGroupGainStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CapacityExceeded { declared, limit } => write!(
                formatter,
                "Substream-group gain count {declared} exceeds implementation limit {limit}"
            ),
            Self::InconsistentUpdate { declared, update } => write!(
                formatter,
                "Substream-group gain update {update:?} is inconsistent with declared group count {declared}"
            ),
            Self::SubstreamGroupCountChanged { previous, current } => write!(
                formatter,
                "Dependent presentation changed substream-group count from {previous} to {current}; reset the gain state at the topology boundary"
            ),
        }
    }
}

impl core::error::Error for PresentationSubstreamGroupGainStateError {}

/// 一个 presentation 的当前有效 substream-group gain 六比特码值。
///
/// 状态必须按 presentation 隔离。`b_keep` 在首次新值前使用表 70 的 0 dB 码值 `0`；
/// [`PresentationSubstreamGroupGainUpdate::NewValues`] 替换全部值；未携带 group gain 或语法
/// gate 不存在时，本帧有效值为 `0`。`b_pres_ndot` 为真的独立帧先丢弃历史，保证从随机访问点
/// 仅凭当前 presentation substream 即可得到有效值。
///
/// seek、换源、拓扑变化或调用方检测到不连续时应调用 [`reset`](Self::reset)。dependent frame
/// 在未 reset 的状态下改变 group 数会失败关闭，避免把旧数组静默映射到新拓扑。所有更新均为
/// 事务性；本类型只保留六比特码值，不换算 dB，也不应用 gain。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationSubstreamGroupGainState {
    effective_codes: Option<PresentationSubstreamGroupGainCodes>,
}

impl PresentationSubstreamGroupGainState {
    /// 创建尚未接收 presentation 上下文与 gain 更新的状态。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            effective_codes: None,
        }
    }

    /// 最近一次成功提交的有效六比特码值；应用首帧前为 `None`。
    #[must_use]
    pub const fn effective_codes(self) -> Option<PresentationSubstreamGroupGainCodes> {
        self.effective_codes
    }

    /// 在 seek、换源、拓扑变化或不连续处清除 group 形状与有效码值。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// 按当前 presentation 上下文应用一帧原始 group-gain 更新。
    ///
    /// 返回当前帧生效的逐 group 六比特码值。`KeepPrevious` 在没有历史时返回全零；独立帧同样
    /// 从全零状态处理当前更新。`NotPresent` 与 `NotSignaled` 都使当前有效数组归零。
    ///
    /// # Errors
    ///
    /// group 数超过固定容量、`update` 不是由相同 group 数的 parser 上下文产生，或未 reset 的
    /// dependent frame 改变 group 数时返回错误。任何失败都不会修改状态。
    pub fn apply(
        &mut self,
        update: PresentationSubstreamGroupGainUpdate,
        context: PresentationSubstreamContext,
    ) -> Result<PresentationSubstreamGroupGainCodes, PresentationSubstreamGroupGainStateError> {
        let declared = context.n_substream_groups();
        let count = usize::try_from(declared).map_err(|_| {
            PresentationSubstreamGroupGainStateError::CapacityExceeded {
                declared,
                limit: MAX_GROUPS_PER_PRESENTATION,
            }
        })?;
        if count > MAX_GROUPS_PER_PRESENTATION {
            return Err(PresentationSubstreamGroupGainStateError::CapacityExceeded {
                declared,
                limit: MAX_GROUPS_PER_PRESENTATION,
            });
        }

        let update_matches_context = match update {
            PresentationSubstreamGroupGainUpdate::NotSignaled => count <= 1,
            PresentationSubstreamGroupGainUpdate::NotPresent
            | PresentationSubstreamGroupGainUpdate::KeepPrevious => count > 1,
            PresentationSubstreamGroupGainUpdate::NewValues(codes) => {
                count > 1 && codes.len() == count
            }
        };
        if !update_matches_context {
            return Err(
                PresentationSubstreamGroupGainStateError::InconsistentUpdate { declared, update },
            );
        }

        let previous = if context.presentation_is_independent() {
            None
        } else {
            self.effective_codes
        };
        if let Some(previous) = previous
            && previous.len() != count
        {
            return Err(
                PresentationSubstreamGroupGainStateError::SubstreamGroupCountChanged {
                    previous: previous.len(),
                    current: count,
                },
            );
        }

        let previous = previous.unwrap_or(PresentationSubstreamGroupGainCodes::zeros(count));
        let effective = match update {
            PresentationSubstreamGroupGainUpdate::NotSignaled
            | PresentationSubstreamGroupGainUpdate::NotPresent => {
                PresentationSubstreamGroupGainCodes::zeros(count)
            }
            PresentationSubstreamGroupGainUpdate::KeepPrevious => previous,
            PresentationSubstreamGroupGainUpdate::NewValues(codes) => codes,
        };
        self.effective_codes = Some(effective);
        Ok(effective)
    }
}

/// 当前 presentation 的 associated-audio scaling 与 mono pan 原始码值。
///
/// 三个 scale 的 presence 独立；`0x00..=0xfe` 及静音码 `0xff` 均原样保留，不做 dB
/// 换算或应用。`pan_associated` 仅在 [`associate_is_mono`](Self::associate_is_mono) 为真时
/// 存在，并已拒绝规范禁止的 `0xf0..=0xff`，但不换算为角度或执行声像处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationAssociatedAudio {
    /// `b_scale_main` 控制的 8 比特 `scale_main`。
    pub scale_main: Option<u8>,
    /// `b_scale_main_centre` 控制的 8 比特 `scale_main_centre`。
    pub scale_main_centre: Option<u8>,
    /// `b_scale_main_front` 控制的 8 比特 `scale_main_front`。
    pub scale_main_front: Option<u8>,
    /// `b_associate_is_mono`。
    pub associate_is_mono: bool,
    /// mono associated audio 的 8 比特 `pan_associated`。
    pub pan_associated: Option<u8>,
}

/// stereo downmix 系数所属的矩阵类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationStereoDownmixKind {
    /// 普通 Lo/Ro stereo downmix。
    LoRo,
    /// Dolby Pro Logic II compatible Lt/Rt stereo downmix。
    LtRt,
}

impl fmt::Display for PresentationStereoDownmixKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match *self {
            Self::LoRo => "LoRo",
            Self::LtRt => "LtRt",
        })
    }
}

/// `tool_scr_to_c_l()` 的 screen-channel 路由与 3 比特 gain 原值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationScreenDownmix {
    /// `b_put_screen_to_c = 1`，使用 `gain_f1_code` 混入 centre。
    ToCentre {
        /// 3 比特 `gain_f1_code` 原值；`7` 表示静音。
        gain_f1_code: u8,
    },
    /// `b_put_screen_to_c = 0`，使用 `gain_f2_code` 混入 L/R。
    ToFrontPair {
        /// 3 比特 `gain_f2_code` 原值；`7` 表示静音。
        gain_f2_code: u8,
    },
}

/// top-channel pair 在 downmix 中的目标声道对。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationTopPairDestination {
    /// 前方 L/R。
    Front,
    /// 侧方 Ls/Rs。
    Side,
    /// 后方 Lb/Rb。
    Back,
}

/// 一对 top channels 的目标路由与随该分支传输的 3 比特 gain 原值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationTopPairDownmix {
    /// 该 top pair 混入的目标声道对。
    pub destination: PresentationTopPairDestination,
    /// 分支选择的 `gain_t2*()` 三比特码值；`7` 表示静音。
    pub gain_code: u8,
}

/// `cdmx_parameters()` 选择的 top-channel downmix tool 及其原始参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationTopDownmix {
    /// `tool_t4_to_t2()`：四个 top channels 合并成两个 top channels。
    FourToTwo {
        /// 3 比特 `gain_t1_code` 原值；`7` 表示静音。
        gain_t1_code: u8,
    },
    /// `tool_t4_to_f_s_b()`：top-front 与 top-back pair 可分别进入 front/side/back。
    FourToFrontSideBack {
        /// top-front pair 的路由与 gain。
        front: PresentationTopPairDownmix,
        /// top-back pair 的路由与 gain。
        back: PresentationTopPairDownmix,
    },
    /// `tool_t4_to_f_s()`：top-front 与 top-back pair 可分别进入 front/side。
    FourToFrontSide {
        /// top-front pair 的路由与 gain。
        front: PresentationTopPairDownmix,
        /// top-back pair 的路由与 gain。
        back: PresentationTopPairDownmix,
    },
    /// `tool_t2_to_f_s_b()`：一个 top-side pair 可进入 front/side/back。
    TwoToFrontSideBack {
        /// top-side pair 的路由与 gain。
        pair: PresentationTopPairDownmix,
    },
    /// `tool_t2_to_f_s()`：一个 top-side pair 可进入 front/side。
    TwoToFrontSide {
        /// top-side pair 的路由与 gain。
        pair: PresentationTopPairDownmix,
    },
}

/// 单个 output channel configuration 的 custom downmix 原始参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationCustomDownmixParameters {
    /// 仅 `bs_ch_config` 0/3 携带的 screen-channel 路由。
    pub screen: Option<PresentationScreenDownmix>,
    /// 由输入/输出配置决定是否携带的 top-channel tool。
    pub top: Option<PresentationTopDownmix>,
    /// `tool_b4_to_b2()` 的 3 比特 `gain_b_code` 原值。
    pub back_four_to_two_gain_code: Option<u8>,
}

impl PresentationCustomDownmixParameters {
    const EMPTY: Self = Self {
        screen: None,
        top: None,
        back_four_to_two_gain_code: None,
    };
}

/// `custom_dmx_data()` 中一个 output channel configuration 及其参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationCustomDownmixConfiguration {
    /// 表 127 的 `out_ch_config` 原值，成功解析时为 `0..=4`。
    pub output_channel_config: u8,
    /// 由 `bs_ch_config`/`out_ch_config` 共同选择的原始参数。
    pub parameters: PresentationCustomDownmixParameters,
}

impl PresentationCustomDownmixConfiguration {
    const EMPTY: Self = Self {
        output_channel_config: 0,
        parameters: PresentationCustomDownmixParameters::EMPTY,
    };
}

/// presentation stereo downmix 的 LoRo/LtRt/LFE 原始系数。
///
/// LoRo 总是存在；`ltrt_*` 由 `b_ltrt_mixinfo` 控制。`lfe_mixinfo_present` 的 `None`
/// 表示 presentation 没有 LFE、因此连 presence bit 都未传输；`Some(false)` 表示传输了
/// `b_lfe_mixinfo = 0`。所有字段都只保留码值，不换算或应用 gain。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationStereoDownmixCoefficients {
    /// 3 比特 `loro_centre_mixgain`。
    pub loro_centre_mixgain: u8,
    /// 3 比特 `loro_surround_mixgain`；成功解析时为 `2..=7`。
    pub loro_surround_mixgain: u8,
    /// `b_ltrt_mixinfo` 为真时的 3 比特 `ltrt_centre_mixgain`。
    pub ltrt_centre_mixgain: Option<u8>,
    /// `b_ltrt_mixinfo` 为真时的 3 比特 `ltrt_surround_mixgain`，取值为 `2..=7`。
    pub ltrt_surround_mixgain: Option<u8>,
    /// `b_lfe_mixinfo` 是否传输及其值；`None` 表示该 gate 不适用。
    pub lfe_mixinfo_present: Option<bool>,
    /// `b_lfe_mixinfo = 1` 时的 5 比特 `lfe_mixgain` 原值。
    pub lfe_mixgain: Option<u8>,
    /// 2 比特 `preferred_dmx_method` 原值。
    pub preferred_downmix_method: u8,
}

/// presentation 的完整 `custom_dmx_data()` 解析结果。
///
/// 两个 `Option<bool>` 分别保留「presence gate 不适用」「gate 存在但值为零」和「数据存在」
/// 三种状态。配置使用固定容量数组保存，不分配内存，也不执行 downmix。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationCustomDownmixData {
    bitstream_channel_config: Option<u8>,
    custom_data_present: Option<bool>,
    configurations: [PresentationCustomDownmixConfiguration; MAX_CUSTOM_DOWNMIX_CONFIGURATIONS],
    configuration_count: usize,
    stereo_coefficients_present: Option<bool>,
    stereo_coefficients: Option<PresentationStereoDownmixCoefficients>,
}

impl PresentationCustomDownmixData {
    /// 由 presentation channel context 派生的 `bs_ch_config`；`None` 对应规范的 `-1`。
    #[must_use]
    pub const fn bitstream_channel_config(self) -> Option<u8> {
        self.bitstream_channel_config
    }

    /// `b_cdmx_data_present` 是否传输及其值；`None` 表示 `bs_ch_config == -1`。
    #[must_use]
    pub const fn custom_data_present(self) -> Option<bool> {
        self.custom_data_present
    }

    /// 按码流顺序取得 `1..=4` 个 custom downmix configuration；无数据时为空。
    #[must_use]
    pub fn configurations(&self) -> &[PresentationCustomDownmixConfiguration] {
        self.configurations
            .get(..self.configuration_count)
            .unwrap_or(&[])
    }

    /// `b_stereo_dmx_coeff` 是否传输及其值；`None` 表示 stereo gate 不适用。
    #[must_use]
    pub const fn stereo_coefficients_present(self) -> Option<bool> {
        self.stereo_coefficients_present
    }

    /// `b_stereo_dmx_coeff = 1` 时传输的完整原始系数。
    #[must_use]
    pub const fn stereo_coefficients(self) -> Option<PresentationStereoDownmixCoefficients> {
        self.stereo_coefficients
    }
}

/// 一个 `b_loud_comp`/downmix loud-comp presence bit 及其可选 5 比特 correction 码值。
///
/// `Value(31)` 仍是合法码流：规范把该 reserved value 解释为 0 dB。本类型不换算或应用 gain。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationLoudnessCorrectionCode {
    /// 对应 presence bit 为零，没有传输 5 比特码值。
    NotPresent,
    /// presence bit 为一，并保留紧随其后的 5 比特原值。
    Value(u8),
}

/// core LoRo/LtRt 共用的 `b_loud_comp` 及其两个 5 比特 correction 码值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationCoreStereoLoudnessCorrection {
    /// 共用 presence bit 为零，没有传输两个码值。
    NotPresent,
    /// presence bit 为一，LoRo 与 LtRt 两个码值均存在。
    Values {
        /// 5 比特 `loud_corr_core_loro` 原值。
        loro: u8,
        /// 5 比特 `loud_corr_core_ltrt` 原值。
        ltrt: u8,
    },
}

/// presentation `loud_corr()` 的全部 presence gate 与原始 correction 码值。
///
/// correction 字段外层的 `None` 表示其 presence bit 因 channel/object 条件不适用而未传输；
/// `Some(NotPresent)` 表示 presence bit 明确为零。对象与 immersive-output 两个布尔字段也用
/// `None` 区分 gate 不适用。所有 5 比特码值均原样保留，不换算或应用 gain。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationLoudnessCorrectionData {
    /// `b_obj_loud_corr` 是否传输及其值；仅 `pres_ch_mode == -1` 时传输。
    pub object_loudness_correction: Option<bool>,
    /// `b_corr_for_immersive_out` 是否传输及其值。
    pub corrections_for_immersive_output: Option<bool>,
    /// `b_loro_loud_comp` 与可选 `loro_dmx_loud_corr`。
    pub loro_downmix: Option<PresentationLoudnessCorrectionCode>,
    /// `b_ltrt_loud_comp` 与可选 `ltrt_dmx_loud_corr`。
    pub ltrt_downmix: Option<PresentationLoudnessCorrectionCode>,
    /// `b_loud_comp` 与可选 `loud_corr_5_X`。
    pub five_x: Option<PresentationLoudnessCorrectionCode>,
    /// immersive-output gate 下的 `loud_corr_5_X_2`。
    pub five_x_two: Option<PresentationLoudnessCorrectionCode>,
    /// immersive-output gate 下的 `loud_corr_7_X`。
    pub seven_x: Option<PresentationLoudnessCorrectionCode>,
    /// immersive-output gate 下的 `loud_corr_7_X_4`。
    pub seven_x_four: Option<PresentationLoudnessCorrectionCode>,
    /// immersive-output gate 下的 `loud_corr_7_X_2`。
    pub seven_x_two: Option<PresentationLoudnessCorrectionCode>,
    /// immersive-output gate 下的 `loud_corr_5_X_4`。
    pub five_x_four: Option<PresentationLoudnessCorrectionCode>,
    /// `pres_ch_mode_core >= 5` 时的 `loud_corr_core_5_X_2`。
    pub core_five_x_two: Option<PresentationLoudnessCorrectionCode>,
    /// `pres_ch_mode_core >= 3` 时的 `loud_corr_core_5_X`。
    pub core_five_x: Option<PresentationLoudnessCorrectionCode>,
    /// core LoRo/LtRt 共用 presence bit 的两个 correction 码值。
    pub core_stereo: Option<PresentationCoreStereoLoudnessCorrection>,
    /// object loudness-correction gate 下的 `loud_corr_9_X_4`。
    pub nine_x_four: Option<PresentationLoudnessCorrectionCode>,
}

impl<'a> IntoIterator for PresentationAddDataBits<'a> {
    type Item = bool;
    type IntoIter = PresentationAddDataBitIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`PresentationAddDataBits`] 的无分配迭代器。
#[derive(Debug, Clone)]
pub struct PresentationAddDataBitIter<'a> {
    reader: BitReader<'a>,
    remaining: u64,
}

impl<'a> PresentationAddDataBitIter<'a> {
    fn new(bits: PresentationAddDataBits<'a>) -> Self {
        let mut reader = BitReader::new(bits.source);
        let remaining = if reader.skip_bits(bits.bit_offset).is_ok() {
            bits.bit_len
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for PresentationAddDataBitIter<'_> {
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

/// `b_additional_data` 控制的有界 presentation metadata。
#[derive(Debug, Clone, Copy)]
pub struct PresentationAdditionalData<'a> {
    /// envelope 声明的 `add_data_bytes`。
    pub add_data_bytes: u32,
    /// 纯信息性的 `immersive_audio_indicator`。
    pub immersive_audio: bool,
    /// 仅 `pres_ch_mode == -1` 时传输的 `b_oamd_common_timing`。
    pub oamd_common_timing: Option<bool>,
    /// 可选 `advanced_de_data()`；仅解析原始参数，不执行处理。
    pub advanced_de_data: Option<AdvancedDeData>,
    /// 已知字段之后、envelope 末尾之前的保留 `add_data`。
    pub add_data: PresentationAddDataBits<'a>,
}

impl<'a, 'b> PartialEq<PresentationAdditionalData<'b>> for PresentationAdditionalData<'a> {
    fn eq(&self, other: &PresentationAdditionalData<'b>) -> bool {
        self.add_data_bytes == other.add_data_bytes
            && self.immersive_audio == other.immersive_audio
            && self.oamd_common_timing == other.oamd_common_timing
            && self.advanced_de_data == other.advanced_de_data
            && self.add_data == other.add_data
    }
}

impl Eq for PresentationAdditionalData<'_> {}

/// presentation name 在当前帧中的分片形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationNameChunkKind {
    /// 长度为零；码流没有提供可判读的终止或分片标记。
    Empty,
    /// 最后一个字节为零，当前分片就是完整名称。
    Complete,
    /// 名称仍会在后续 codec frame 中继续。
    Intermediate,
    /// 序列化名称的最后一块；末字节给出总分片数。
    Final {
        /// 完整名称包含的分片总数。
        total_chunks: u8,
    },
}

/// 可能不在字节边界上的 presentation name 原始 8 比特元素视图。
///
/// 名称可能跨 codec frame 分片，单个分片也可能切开 UTF-8 code point，因此本层不做
/// 有损 UTF-8 替换或跨帧拼接。
#[derive(Debug, Clone, Copy)]
pub struct PresentationNameBytes<'a> {
    source: &'a [u8],
    bit_offset: u64,
    len: u8,
}

impl<'a, 'b> PartialEq<PresentationNameBytes<'b>> for PresentationNameBytes<'a> {
    fn eq(&self, other: &PresentationNameBytes<'b>) -> bool {
        self.len == other.len && (*self).iter().eq((*other).iter())
    }
}

impl Eq for PresentationNameBytes<'_> {}

impl<'a> PresentationNameBytes<'a> {
    fn read(
        reader: &mut BitReader<'a>,
        source: &'a [u8],
        len: u8,
    ) -> Result<Self, PresentationSubstreamError> {
        let bit_offset = reader.bit_position();
        reader.skip_bits(u64::from(len).saturating_mul(8))?;
        Ok(Self {
            source,
            bit_offset,
            len,
        })
    }

    /// 原始 8 比特元素数量。
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// 是否为空分片。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// 读取一个原始 8 比特元素。
    #[must_use]
    pub fn get(self, index: usize) -> Option<u8> {
        if index >= self.len() {
            return None;
        }
        let relative = u64::try_from(index).ok()?.checked_mul(8)?;
        let mut reader = BitReader::new(self.source);
        reader
            .skip_bits(self.bit_offset.checked_add(relative)?)
            .ok()?;
        u8::try_from(reader.read_bits(8).ok()?).ok()
    }

    /// 若名称分片恰好位于字节边界，返回零拷贝切片。
    #[must_use]
    pub fn as_aligned_slice(self) -> Option<&'a [u8]> {
        if !self.bit_offset.is_multiple_of(8) {
            return None;
        }
        let start = usize::try_from(self.bit_offset / 8).ok()?;
        let end = start.checked_add(self.len())?;
        self.source.get(start..end)
    }

    /// 按码流顺序遍历原始 8 比特元素。
    #[must_use]
    pub fn iter(self) -> PresentationNameByteIter<'a> {
        PresentationNameByteIter::new(self)
    }

    /// 根据 `6.3.3.1.4` 判断当前名称分片的形态。
    #[must_use]
    pub fn chunk_kind(self) -> PresentationNameChunkKind {
        let Some(last_index) = self.len().checked_sub(1) else {
            return PresentationNameChunkKind::Empty;
        };
        let Some(last) = self.get(last_index) else {
            return PresentationNameChunkKind::Empty;
        };
        if last == 0 {
            return PresentationNameChunkKind::Complete;
        }
        if last_index > 0 && self.get(last_index.saturating_sub(1)) == Some(0) {
            return PresentationNameChunkKind::Final { total_chunks: last };
        }
        PresentationNameChunkKind::Intermediate
    }
}

impl<'a> IntoIterator for PresentationNameBytes<'a> {
    type Item = u8;
    type IntoIter = PresentationNameByteIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`PresentationNameBytes`] 的无分配迭代器。
#[derive(Debug, Clone)]
pub struct PresentationNameByteIter<'a> {
    reader: BitReader<'a>,
    remaining: usize,
}

impl<'a> PresentationNameByteIter<'a> {
    fn new(name: PresentationNameBytes<'a>) -> Self {
        let mut reader = BitReader::new(name.source);
        let remaining = if reader.skip_bits(name.bit_offset).is_ok() {
            name.len()
        } else {
            0
        };
        Self { reader, remaining }
    }
}

impl Iterator for PresentationNameByteIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = self
            .reader
            .read_bits(8)
            .ok()
            .and_then(|value| u8::try_from(value).ok());
        if value.is_some() {
            self.remaining = self.remaining.saturating_sub(1);
        } else {
            self.remaining = 0;
        }
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for PresentationNameByteIter<'_> {}

/// 表 67 的四个 target device category 位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlternativeTargetDeviceCategories(u8);

impl AlternativeTargetDeviceCategories {
    fn parse(reader: &mut BitReader<'_>) -> Result<Self, PresentationSubstreamError> {
        Ok(Self(u8::try_from(reader.read_bits(4)?).unwrap_or(0)))
    }

    /// 取得规范数组下标 `0..=3` 对应的类别位。
    #[must_use]
    pub const fn contains(self, index: u8) -> bool {
        if index >= 4 {
            return false;
        }
        let shift = 3u32.saturating_sub(index as u32);
        self.0 & (1u8 << shift) != 0
    }

    /// 四个传输位的原值；最高位对应规范数组下标 0。
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// 一个 target 对某个音频 substream 的 activation/dataset 选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlternativeSubstreamActivation {
    /// 在 `n_substreams_in_presentation` 规范顺序中的零基下标。
    pub substream_index: u32,
    /// `b_active`。
    pub active: bool,
    /// active 时传输的 `alt_data_set_index`；零表示不使用 alternative dataset。
    pub alternative_data_set_index: Option<u32>,
}

/// alternative presentation 的一个播放 target。
#[derive(Debug, Clone, Copy)]
pub struct AlternativePresentationTarget<'a> {
    /// 3 比特 decoder compatibility target level。
    pub target_level: u8,
    /// 表 67 的四个 target device category 位。
    pub device_categories: AlternativeTargetDeviceCategories,
    /// `b_tdc_extension` 为真时的 4 比特保留扩展；否则为 `None`。
    pub device_category_extension: Option<u8>,
    /// 最大 ducking depth 的 6 比特码值。
    pub max_ducking_depth: Option<u8>,
    /// target-specific loudness correction 的 5 比特码值。
    pub loudness_correction_target: Option<u8>,
    source: &'a [u8],
    activations_bit_offset: u64,
    n_audio_substreams: u32,
}

impl<'a, 'b> PartialEq<AlternativePresentationTarget<'b>> for AlternativePresentationTarget<'a> {
    fn eq(&self, other: &AlternativePresentationTarget<'b>) -> bool {
        self.target_level == other.target_level
            && self.device_categories == other.device_categories
            && self.device_category_extension == other.device_category_extension
            && self.max_ducking_depth == other.max_ducking_depth
            && self.loudness_correction_target == other.loudness_correction_target
            && (*self)
                .substream_activations()
                .eq((*other).substream_activations())
    }
}

impl Eq for AlternativePresentationTarget<'_> {}

impl<'a> AlternativePresentationTarget<'a> {
    /// 按规范的 presentation substream 顺序遍历 activation/dataset map。
    #[must_use]
    pub fn substream_activations(self) -> AlternativeSubstreamActivationIter<'a> {
        AlternativeSubstreamActivationIter::new(
            self.source,
            self.activations_bit_offset,
            self.n_audio_substreams,
        )
    }
}

/// 单个 target 的 activation/dataset map 迭代器。
#[derive(Debug, Clone)]
pub struct AlternativeSubstreamActivationIter<'a> {
    reader: BitReader<'a>,
    remaining: u32,
    index: u32,
    failed: bool,
}

impl<'a> AlternativeSubstreamActivationIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u32) -> Self {
        let mut reader = BitReader::new(source);
        let failed = reader.skip_bits(bit_offset).is_err();
        Self {
            reader,
            remaining: if failed { 0 } else { count },
            index: 0,
            failed,
        }
    }
}

impl Iterator for AlternativeSubstreamActivationIter<'_> {
    type Item = Result<AlternativeSubstreamActivation, PresentationSubstreamError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let parsed = parse_activation(&mut self.reader, self.index);
        match parsed {
            Ok(activation) => {
                self.remaining = self.remaining.saturating_sub(1);
                self.index = self.index.saturating_add(1);
                Some(Ok(activation))
            }
            Err(error) => {
                self.failed = true;
                self.remaining = 0;
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AlternativeSubstreamActivationIter<'_> {}

/// `b_alternative` 为真时携带的名称与 target 列表。
#[derive(Debug, Clone, Copy)]
pub struct AlternativePresentationSelection<'a> {
    /// 可选 presentation name 分片。
    pub name: Option<PresentationNameBytes<'a>>,
    /// 播放 target 数量。
    pub n_targets: u32,
    /// 每个 target 的 activation map 长度。
    pub n_audio_substreams: u32,
    source: &'a [u8],
    targets_bit_offset: u64,
}

impl<'a, 'b> PartialEq<AlternativePresentationSelection<'b>>
    for AlternativePresentationSelection<'a>
{
    fn eq(&self, other: &AlternativePresentationSelection<'b>) -> bool {
        let names_equal = match (self.name, other.name) {
            (None, None) => true,
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
        if !names_equal
            || self.n_targets != other.n_targets
            || self.n_audio_substreams != other.n_audio_substreams
        {
            return false;
        }

        let mut left = (*self).targets();
        let mut right = (*other).targets();
        loop {
            match (left.next(), right.next()) {
                (None, None) => return true,
                (Some(Ok(left)), Some(Ok(right))) if left == right => {}
                (Some(Err(left)), Some(Err(right))) if left == right => {}
                _ => return false,
            }
        }
    }
}

impl Eq for AlternativePresentationSelection<'_> {}

impl<'a> AlternativePresentationSelection<'a> {
    /// 按码流顺序遍历播放 target。
    #[must_use]
    pub fn targets(self) -> AlternativePresentationTargetIter<'a> {
        AlternativePresentationTargetIter::new(
            self.source,
            self.targets_bit_offset,
            self.n_targets,
            self.n_audio_substreams,
        )
    }
}

/// alternative presentation target 迭代器。
#[derive(Debug, Clone)]
pub struct AlternativePresentationTargetIter<'a> {
    reader: BitReader<'a>,
    source: &'a [u8],
    remaining: u32,
    n_audio_substreams: u32,
    failed: bool,
}

impl<'a> AlternativePresentationTargetIter<'a> {
    fn new(source: &'a [u8], bit_offset: u64, count: u32, n_audio_substreams: u32) -> Self {
        let mut reader = BitReader::new(source);
        let failed = reader.skip_bits(bit_offset).is_err();
        Self {
            reader,
            source,
            remaining: if failed { 0 } else { count },
            n_audio_substreams,
            failed,
        }
    }
}

impl<'a> Iterator for AlternativePresentationTargetIter<'a> {
    type Item = Result<AlternativePresentationTarget<'a>, PresentationSubstreamError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining == 0 {
            return None;
        }
        let parsed = parse_target(&mut self.reader, self.source, self.n_audio_substreams);
        match parsed {
            Ok(target) => {
                self.remaining = self.remaining.saturating_sub(1);
                Some(Ok(target))
            }
            Err(error) => {
                self.failed = true;
                self.remaining = 0;
                Some(Err(error))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AlternativePresentationTargetIter<'_> {}

/// `ac4_presentation_substream()` 的 selection 前缀。
///
/// 普通 presentation 没有该前缀，`alternative` 为 `None` 且公共 metadata 从 bit 0
/// 开始。alternative presentation 完整验证名称、target 与 activation/dataset map 后，
/// `common_metadata_bit_offset` 指向紧随其后的 `b_additional_data`。
#[derive(Debug, Clone, Copy)]
pub struct Ac4PresentationSubstreamSelection<'a> {
    /// alternative presentation 的选择信令；普通 presentation 为 `None`。
    pub alternative: Option<AlternativePresentationSelection<'a>>,
    /// 公共 presentation metadata 后缀在该 substream payload 内的比特偏移。
    pub common_metadata_bit_offset: u64,
}

impl<'a, 'b> PartialEq<Ac4PresentationSubstreamSelection<'b>>
    for Ac4PresentationSubstreamSelection<'a>
{
    fn eq(&self, other: &Ac4PresentationSubstreamSelection<'b>) -> bool {
        if self.common_metadata_bit_offset != other.common_metadata_bit_offset {
            return false;
        }
        match (self.alternative, other.alternative) {
            (None, None) => true,
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for Ac4PresentationSubstreamSelection<'_> {}

impl<'a> Ac4PresentationSubstreamSelection<'a> {
    /// 解析 `b_alternative` 控制的 selection 前缀。
    ///
    /// `payload` 必须恰好是 TOC 中 presentation substream index 对应的有界 payload。
    /// 本函数不解析或验证 `common_metadata_bit_offset` 之后的公共 metadata 后缀。
    ///
    /// # Errors
    ///
    /// 名称、target 或 activation map 截断，变长字段溢出，或计数超过固定容量时
    /// 返回错误。
    pub fn parse(
        payload: &'a [u8],
        context: PresentationSubstreamSelectionContext,
    ) -> Result<Self, PresentationSubstreamError> {
        if context.alternative {
            if context.n_audio_substreams == 0 {
                return Err(PresentationSubstreamError::MissingAudioSubstreams);
            }
            let limit = u32::try_from(MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION).unwrap_or(u32::MAX);
            if context.n_audio_substreams > limit {
                return Err(PresentationSubstreamError::CapacityExceeded {
                    what: PresentationSubstreamCapacity::AudioSubstreams,
                    declared: context.n_audio_substreams,
                    limit: MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION,
                });
            }
        }

        let mut reader = BitReader::new(payload);
        let alternative = if context.alternative {
            Some(parse_alternative(
                &mut reader,
                payload,
                context.n_audio_substreams,
            )?)
        } else {
            None
        };

        Ok(Self {
            alternative,
            common_metadata_bit_offset: reader.bit_position(),
        })
    }
}

/// `ac4_presentation_substream()` 已解析的 selection、additional-data、响度、DRC、group gain、
/// associated audio、custom downmix 与 loudness correction。
///
/// [`drc_metadata_size_value_offset`](Self::drc_metadata_size_value_offset) 指向响度字段之后的
/// `drc_metadata_size_value`；[`b_associated_offset`](Self::b_associated_offset) 指向 group gain
/// 之后的 associated-audio metadata；[`custom_downmix_offset`](Self::custom_downmix_offset) 指向
/// `custom_dmx_data()`，[`loudness_correction_offset`](Self::loudness_correction_offset) 指向
/// `loud_corr()`。成功解析会继续消费末尾 `byte_align` 并严格落在 payload 末尾；DRC I-frame
/// 配置与 I-frame `drc_data()` 包络已解析；使用 `parse_with_drc_state()` 时 dependent frame
/// 也会按前一有效配置解析，`audio-decode` 下可再另行解码 Huffman gains。逐帧 group gain
/// 更新可另交 [`PresentationSubstreamGroupGainState`] 得到跨帧有效六比特码值。
#[derive(Debug, Clone, Copy)]
pub struct Ac4PresentationSubstream<'a> {
    /// 普通或 alternative presentation 的 selection 视图。
    pub selection: Ac4PresentationSubstreamSelection<'a>,
    /// `b_additional_data` 为真时的有界 additional-data metadata。
    pub additional_data: Option<PresentationAdditionalData<'a>>,
    /// `dialnorm_bits` 在 presentation substream payload 内的精确比特偏移。
    pub dialnorm_bits_offset: u64,
    /// 7 比特 presentation `dialnorm_bits` 原值。
    pub dialnorm_bits: u8,
    /// `b_further_loudness_info` 控制的进一步响度信息。
    pub further_loudness: Option<FurtherLoudnessInfo>,
    /// `drc_metadata_size_value` 在 payload 内的精确比特偏移。
    pub drc_metadata_size_value_offset: u64,
    /// `drc_frame()` 的首个 `b_drc_present`。
    pub drc_present: bool,
    /// 由 `drc_metadata_size` 严格定界的完整 `drc_frame()` 原始比特。
    pub drc_frame: PresentationDrcFrameBits<'a>,
    /// I-frame 且 DRC present 时传输的 `drc_config()`；dependent frame 或 DRC absent 时为 `None`。
    pub drc_configuration: Option<PresentationDrcConfiguration>,
    /// `drc_config()` 之后的完整 `drc_data()` bit view。
    ///
    /// dependent frame 从 `b_drc_present` 后立即开始；DRC absent 时保留 envelope 中任何剩余比特，
    /// 但这些比特不属于规范 `drc_data()`，留待完整 frame 校验拒绝。
    pub drc_data: PresentationDrcFrameBits<'a>,
    /// 按当前有效配置解析的 `drc_data()` gain-set/reset 包络。
    ///
    /// 无状态 [`parse`](Self::parse) 只解析 I-frame；[`parse_with_drc_state`](Self::parse_with_drc_state)
    /// 也解析 dependent frame。gain-set 内的 Huffman gains 仅在调用 `audio-decode` 提供的显式
    /// 解码 API 时解析。
    pub drc_data_elements: Option<PresentationDrcData<'a>>,
    /// 当前帧传输的 substream-group gain 原始更新。
    pub substream_group_gain_update: PresentationSubstreamGroupGainUpdate,
    /// `b_associated` 在 payload 内的精确比特偏移。
    pub b_associated_offset: u64,
    /// `b_associated` 控制的 associated-audio scale/pan 原始码值。
    pub associated_audio: Option<PresentationAssociatedAudio>,
    /// `custom_dmx_data()` 在 payload 内的精确比特偏移。
    pub custom_downmix_offset: u64,
    /// 已解析的 custom downmix presence、configuration、路由与 gain 原始码值。
    pub custom_downmix: PresentationCustomDownmixData,
    /// `loud_corr()` 在 payload 内的精确比特偏移。
    pub loudness_correction_offset: u64,
    /// 已解析的 loudness-correction presence 与 5 比特原始码值。
    pub loudness_correction: PresentationLoudnessCorrectionData,
    /// presentation 末尾 `byte_align` 开始前的精确比特偏移。
    pub byte_alignment_offset: u64,
    /// 末尾 `byte_align` 消耗的填充比特数，取值为 `0..=7`。
    pub alignment_bits: u32,
}

impl<'a, 'b> PartialEq<Ac4PresentationSubstream<'b>> for Ac4PresentationSubstream<'a> {
    fn eq(&self, other: &Ac4PresentationSubstream<'b>) -> bool {
        self.selection == other.selection
            && self.additional_data == other.additional_data
            && self.dialnorm_bits_offset == other.dialnorm_bits_offset
            && self.dialnorm_bits == other.dialnorm_bits
            && self.further_loudness == other.further_loudness
            && self.drc_metadata_size_value_offset == other.drc_metadata_size_value_offset
            && self.drc_present == other.drc_present
            && self.drc_frame == other.drc_frame
            && self.drc_configuration == other.drc_configuration
            && self.drc_data == other.drc_data
            && self.drc_data_elements == other.drc_data_elements
            && self.substream_group_gain_update == other.substream_group_gain_update
            && self.b_associated_offset == other.b_associated_offset
            && self.associated_audio == other.associated_audio
            && self.custom_downmix_offset == other.custom_downmix_offset
            && self.custom_downmix == other.custom_downmix
            && self.loudness_correction_offset == other.loudness_correction_offset
            && self.loudness_correction == other.loudness_correction
            && self.byte_alignment_offset == other.byte_alignment_offset
            && self.alignment_bits == other.alignment_bits
    }
}

impl Eq for Ac4PresentationSubstream<'_> {}

impl<'a> Ac4PresentationSubstream<'a> {
    /// 解析 selection、common additional-data、presentation 响度、DRC envelope、group gain、
    /// associated audio、custom downmix 与 loudness correction，并消费末尾 `byte_align`。
    ///
    /// `payload` 必须恰好是 TOC 中 presentation substream index 对应的有界 payload。
    /// additional-data 声明的完整字节区域会先验界；`advanced_de_data()` 仅保留原始码值，
    /// 不执行 dialogue enhancement。进一步响度字段同样只保留原值，不执行归一化；
    /// `drc_metadata_size` 严格定界完整 `drc_frame()`；I-frame 会解析 `drc_config()` 与
    /// `drc_data()` 的 gain-set/reset 包络，gain-set body 仍保持原始视图，且不执行 DRC；group
    /// gain 只保留逐帧传输形态；跨帧有效码值由 [`PresentationSubstreamGroupGainState`] 另行
    /// 延续，且不应用增益；associated-audio scale/pan 同样只保留原值，不执行 gain、pan 或
    /// renderer 处理；custom downmix 同样只保留配置、路由与
    /// gain 码值，不执行矩阵运算；loudness correction 也只保留原始码值，不做 dB 换算或应用。
    ///
    /// # Errors
    ///
    /// selection 字段、additional-data/DRC/group gain/associated audio/custom downmix/
    /// loudness correction 或其已知字段截断，变长字段溢出，或计数超过固定容量时返回错误。
    /// 零长度 DRC envelope 不能容纳
    /// 必需的 presence bit，以及 associated pan、custom output config 或 stereo surround gain
    /// 使用禁止码值时，同样返回错误。`byte_align` 后仍有完整尾随字节也会失败关闭。
    pub fn parse(
        payload: &'a [u8],
        context: PresentationSubstreamContext,
    ) -> Result<Self, PresentationSubstreamError> {
        let n_substream_groups = checked_substream_group_count(context.n_substream_groups)?;
        let selection =
            Ac4PresentationSubstreamSelection::parse(payload, context.selection_context())?;
        let mut reader = BitReader::new(payload);
        reader.skip_bits(selection.common_metadata_bit_offset)?;
        let additional_data = parse_additional_data(&mut reader, payload, context)?;
        let dialnorm_bits_offset = reader.bit_position();
        let dialnorm_bits = u8::try_from(reader.read_bits(7)?).unwrap_or(u8::MAX);
        let further_loudness = if reader.read_flag()? {
            Some(FurtherLoudnessInfo::parse_with_error(
                &mut reader,
                1,
                true,
                |declared, required, bit_position| {
                    PresentationSubstreamError::InvalidLoudnessExtensionSize {
                        declared,
                        required,
                        bit_position,
                    }
                },
            )?)
        } else {
            None
        };
        let drc_metadata_size_value_offset = reader.bit_position();
        let drc =
            parse_drc_frame_envelope(&mut reader, payload, context.presentation_is_independent())?;
        let substream_group_gain_update =
            parse_substream_group_gain_update(&mut reader, n_substream_groups)?;
        let b_associated_offset = reader.bit_position();
        let associated_audio = parse_associated_audio(&mut reader)?;
        let custom_downmix_offset = reader.bit_position();
        let custom_downmix = parse_custom_downmix_data(&mut reader, context.channel_context())?;
        let loudness_correction_offset = reader.bit_position();
        let loudness_correction =
            parse_loudness_correction(&mut reader, context.channel_context())?;
        let byte_alignment_offset = reader.bit_position();
        let alignment_bits = reader.byte_align()?;
        let remaining_bits = reader.remaining_bits();
        if remaining_bits != 0 {
            return Err(PresentationSubstreamError::TrailingBits { remaining_bits });
        }
        Ok(Self {
            selection,
            additional_data,
            dialnorm_bits_offset,
            dialnorm_bits,
            further_loudness,
            drc_metadata_size_value_offset,
            drc_present: drc.present,
            drc_frame: drc.frame,
            drc_configuration: drc.configuration,
            drc_data: drc.data,
            drc_data_elements: drc.data_elements,
            substream_group_gain_update,
            b_associated_offset,
            associated_audio,
            custom_downmix_offset,
            custom_downmix,
            loudness_correction_offset,
            loudness_correction,
            byte_alignment_offset,
            alignment_bits,
        })
    }

    /// 解析完整 presentation payload，并事务性延续该 presentation 的 DRC 配置。
    ///
    /// 与 [`parse`](Self::parse) 相同，但 dependent frame 中存在 DRC data 时会使用 `state`
    /// 保存的前一有效 I-frame 配置填充 [`drc_data_elements`](Self::drc_data_elements)。成功的
    /// I-frame 会替换配置；DRC 缺席的 I-frame 会清空配置。调用方必须为每个 presentation
    /// 分别维护状态，并在 seek、换源、拓扑变化或不连续处调用 [`PresentationDrcState::reset`]。
    ///
    /// # Errors
    ///
    /// 除 [`parse`](Self::parse) 的错误外，dependent frame 携带 DRC data 却没有前一有效配置，
    /// 或其 data 与保存的配置不一致时也返回错误。任何失败都不会修改 `state`。
    pub fn parse_with_drc_state(
        payload: &'a [u8],
        context: PresentationSubstreamContext,
        state: &mut PresentationDrcState,
    ) -> Result<Self, PresentationSubstreamError> {
        let mut parsed = Self::parse(payload, context)?;
        state.resolve(&mut parsed, context.presentation_is_independent())?;
        Ok(parsed)
    }

    /// 解析完整 presentation payload，并接纳一种窄限定的生产链兼容尾部。
    ///
    /// 首先调用与 [`parse_with_drc_state`](Self::parse_with_drc_state) 相同的严格路径。仅当
    /// 严格路径恰好剩余 8 比特、presentation 为 independent object/A-JOC，末字节为
    /// `0x00` 或 `0x80`，且去除该字节后解析结果实际携带 DRC configuration 时，才把该
    /// 字节认作非规范兼容尾部。其他尾部继续返回原始严格解析错误。
    ///
    /// 返回值第二项是规范 syntax payload 消耗的字节数；严格输入等于 `payload.len()`，
    /// 兼容输入则少一个字节。任何失败都不会修改 `state`。
    ///
    /// 该兼容形态不是 `TS103190-2:v1.3.1:6.2.2.3` 的字段；调用方应保留原 bounded
    /// payload，并仅用返回的字节数区分 syntax 与兼容尾部。
    pub fn parse_with_drc_state_compat(
        payload: &'a [u8],
        context: PresentationSubstreamContext,
        state: &mut PresentationDrcState,
    ) -> Result<(Self, usize), PresentationSubstreamError> {
        let initial_state = *state;
        let mut candidate_state = initial_state;
        match Self::parse_with_drc_state(payload, context, &mut candidate_state) {
            Ok(parsed) => {
                *state = candidate_state;
                Ok((parsed, payload.len()))
            }
            Err(error @ PresentationSubstreamError::TrailingBits { remaining_bits: 8 })
                if context.presentation_is_independent() && context.pres_ch_mode_undefined() =>
            {
                let Some((&compatibility_byte, syntax_payload)) = payload.split_last() else {
                    return Err(error);
                };
                if !INDEPENDENT_OBJECT_DRC_COMPATIBILITY_BYTES.contains(&compatibility_byte) {
                    return Err(error);
                }
                let mut compatibility_state = initial_state;
                let parsed =
                    Self::parse_with_drc_state(syntax_payload, context, &mut compatibility_state)?;
                if !parsed.drc_present || parsed.drc_configuration.is_none() {
                    return Err(error);
                }
                *state = compatibility_state;
                Ok((parsed, syntax_payload.len()))
            }
            Err(error) => Err(error),
        }
    }
}

fn checked_substream_group_count(declared: u32) -> Result<usize, PresentationSubstreamError> {
    let count =
        usize::try_from(declared).map_err(|_| PresentationSubstreamError::CapacityExceeded {
            what: PresentationSubstreamCapacity::SubstreamGroups,
            declared,
            limit: MAX_GROUPS_PER_PRESENTATION,
        })?;
    if count > MAX_GROUPS_PER_PRESENTATION {
        return Err(PresentationSubstreamError::CapacityExceeded {
            what: PresentationSubstreamCapacity::SubstreamGroups,
            declared,
            limit: MAX_GROUPS_PER_PRESENTATION,
        });
    }
    Ok(count)
}

fn parse_substream_group_gain_update(
    reader: &mut BitReader<'_>,
    n_substream_groups: usize,
) -> Result<PresentationSubstreamGroupGainUpdate, PresentationSubstreamError> {
    if n_substream_groups <= 1 {
        return Ok(PresentationSubstreamGroupGainUpdate::NotSignaled);
    }
    if !reader.read_flag()? {
        return Ok(PresentationSubstreamGroupGainUpdate::NotPresent);
    }
    if reader.read_flag()? {
        return Ok(PresentationSubstreamGroupGainUpdate::KeepPrevious);
    }

    let mut codes = [0u8; MAX_GROUPS_PER_PRESENTATION];
    for code in codes.iter_mut().take(n_substream_groups) {
        *code = u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX);
    }
    Ok(PresentationSubstreamGroupGainUpdate::NewValues(
        PresentationSubstreamGroupGainCodes {
            codes,
            len: n_substream_groups,
        },
    ))
}

fn parse_associated_audio(
    reader: &mut BitReader<'_>,
) -> Result<Option<PresentationAssociatedAudio>, PresentationSubstreamError> {
    if !reader.read_flag()? {
        return Ok(None);
    }

    let scale_main = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX))
    } else {
        None
    };
    let scale_main_centre = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX))
    } else {
        None
    };
    let scale_main_front = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX))
    } else {
        None
    };
    let associate_is_mono = reader.read_flag()?;
    let pan_associated = if associate_is_mono {
        let bit_position = reader.bit_position();
        let pan_associated = u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX);
        if pan_associated >= 0xf0 {
            return Err(PresentationSubstreamError::ReservedAssociatedPan {
                pan_associated,
                bit_position,
            });
        }
        Some(pan_associated)
    } else {
        None
    };

    Ok(Some(PresentationAssociatedAudio {
        scale_main,
        scale_main_centre,
        scale_main_front,
        associate_is_mono,
        pan_associated,
    }))
}

fn derive_bitstream_channel_config(context: PresentationChannelContext) -> Option<u8> {
    let mode = context.presentation_channel_mode()?;
    if !(11..=14).contains(&mode) {
        return None;
    }

    match (
        context.top_channel_pairs(),
        mode,
        context.four_back_channels_present(),
    ) {
        (2, 13..=14, true) => Some(0),
        (2, 11..=12, true) => Some(1),
        (2, 11..=12, false) => Some(2),
        (1, 13..=14, true) => Some(3),
        (1, 11..=12, true) => Some(4),
        (1, 11..=12, false) => Some(5),
        _ => None,
    }
}

fn parse_custom_downmix_data(
    reader: &mut BitReader<'_>,
    context: PresentationChannelContext,
) -> Result<PresentationCustomDownmixData, PresentationSubstreamError> {
    let bitstream_channel_config = derive_bitstream_channel_config(context);
    let mut configurations =
        [PresentationCustomDownmixConfiguration::EMPTY; MAX_CUSTOM_DOWNMIX_CONFIGURATIONS];
    let mut configuration_count = 0usize;
    let custom_data_present = if let Some(bitstream_channel_config) = bitstream_channel_config {
        let present = reader.read_flag()?;
        if present {
            configuration_count = usize::try_from(reader.read_bits(2)?)
                .unwrap_or(0)
                .saturating_add(1);
            for configuration in configurations.iter_mut().take(configuration_count) {
                *configuration =
                    parse_custom_downmix_configuration(reader, bitstream_channel_config)?;
            }
        }
        Some(present)
    } else {
        None
    };

    let stereo_gate = context
        .presentation_channel_mode()
        .is_some_and(|mode| mode >= 3)
        || context.core_channel_mode().is_some_and(|mode| mode >= 3);
    let (stereo_coefficients_present, stereo_coefficients) = if stereo_gate {
        let present = reader.read_flag()?;
        let coefficients = if present {
            Some(parse_presentation_stereo_downmix_coefficients(
                reader,
                context.has_lfe(),
            )?)
        } else {
            None
        };
        (Some(present), coefficients)
    } else {
        (None, None)
    };

    Ok(PresentationCustomDownmixData {
        bitstream_channel_config,
        custom_data_present,
        configurations,
        configuration_count,
        stereo_coefficients_present,
        stereo_coefficients,
    })
}

fn parse_custom_downmix_configuration(
    reader: &mut BitReader<'_>,
    bitstream_channel_config: u8,
) -> Result<PresentationCustomDownmixConfiguration, PresentationSubstreamError> {
    let output_channel_config_offset = reader.bit_position();
    let output_width = if matches!(bitstream_channel_config, 2 | 5) {
        1
    } else {
        3
    };
    let output_channel_config = u8::try_from(reader.read_bits(output_width)?).unwrap_or(u8::MAX);
    if output_channel_config >= 5 {
        return Err(
            PresentationSubstreamError::UnusedCustomDownmixOutputChannelConfig {
                output_channel_config,
                bit_position: output_channel_config_offset,
            },
        );
    }
    let parameters =
        parse_custom_downmix_parameters(reader, bitstream_channel_config, output_channel_config)?;
    Ok(PresentationCustomDownmixConfiguration {
        output_channel_config,
        parameters,
    })
}

fn parse_custom_downmix_parameters(
    reader: &mut BitReader<'_>,
    bitstream_channel_config: u8,
    output_channel_config: u8,
) -> Result<PresentationCustomDownmixParameters, PresentationSubstreamError> {
    let mut parameters = PresentationCustomDownmixParameters::EMPTY;
    if matches!(bitstream_channel_config, 0 | 3) {
        parameters.screen = Some(parse_screen_downmix(reader)?);
    }

    match bitstream_channel_config {
        0 | 1 => match output_channel_config {
            0 => {
                parameters.top = Some(parse_top_four_to_front_side(reader)?);
                parameters.back_four_to_two_gain_code = Some(read_custom_gain_code(reader)?);
            }
            1 => {
                parameters.top = Some(PresentationTopDownmix::FourToTwo {
                    gain_t1_code: read_custom_gain_code(reader)?,
                });
                parameters.back_four_to_two_gain_code = Some(read_custom_gain_code(reader)?);
            }
            2 => {
                parameters.back_four_to_two_gain_code = Some(read_custom_gain_code(reader)?);
            }
            3 => {
                parameters.top = Some(parse_top_four_to_front_side_back(reader)?);
            }
            4 => {
                parameters.top = Some(PresentationTopDownmix::FourToTwo {
                    gain_t1_code: read_custom_gain_code(reader)?,
                });
            }
            _ => {}
        },
        2 => match output_channel_config {
            0 => parameters.top = Some(parse_top_four_to_front_side(reader)?),
            1 => {
                parameters.top = Some(PresentationTopDownmix::FourToTwo {
                    gain_t1_code: read_custom_gain_code(reader)?,
                });
            }
            _ => {}
        },
        3 | 4 => match output_channel_config {
            0 => {
                parameters.top = Some(parse_top_two_to_front_side(reader)?);
                parameters.back_four_to_two_gain_code = Some(read_custom_gain_code(reader)?);
            }
            1 | 2 => {
                parameters.back_four_to_two_gain_code = Some(read_custom_gain_code(reader)?);
            }
            3 => parameters.top = Some(parse_top_two_to_front_side_back(reader)?),
            _ => {}
        },
        5 if output_channel_config == 0 => {
            parameters.top = Some(parse_top_two_to_front_side(reader)?);
        }
        5 => {}
        _ => {}
    }

    Ok(parameters)
}

fn read_custom_gain_code(reader: &mut BitReader<'_>) -> Result<u8, PresentationSubstreamError> {
    Ok(u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX))
}

fn parse_screen_downmix(
    reader: &mut BitReader<'_>,
) -> Result<PresentationScreenDownmix, PresentationSubstreamError> {
    if reader.read_flag()? {
        Ok(PresentationScreenDownmix::ToCentre {
            gain_f1_code: read_custom_gain_code(reader)?,
        })
    } else {
        Ok(PresentationScreenDownmix::ToFrontPair {
            gain_f2_code: read_custom_gain_code(reader)?,
        })
    }
}

fn parse_top_pair_to_front_side(
    reader: &mut BitReader<'_>,
) -> Result<PresentationTopPairDownmix, PresentationSubstreamError> {
    let destination = if reader.read_flag()? {
        PresentationTopPairDestination::Front
    } else {
        PresentationTopPairDestination::Side
    };
    Ok(PresentationTopPairDownmix {
        destination,
        gain_code: read_custom_gain_code(reader)?,
    })
}

fn parse_top_pair_to_front_side_back(
    reader: &mut BitReader<'_>,
) -> Result<PresentationTopPairDownmix, PresentationSubstreamError> {
    let destination = if reader.read_flag()? {
        PresentationTopPairDestination::Front
    } else if reader.read_flag()? {
        PresentationTopPairDestination::Side
    } else {
        PresentationTopPairDestination::Back
    };
    Ok(PresentationTopPairDownmix {
        destination,
        gain_code: read_custom_gain_code(reader)?,
    })
}

fn parse_top_four_to_front_side(
    reader: &mut BitReader<'_>,
) -> Result<PresentationTopDownmix, PresentationSubstreamError> {
    Ok(PresentationTopDownmix::FourToFrontSide {
        front: parse_top_pair_to_front_side(reader)?,
        back: parse_top_pair_to_front_side(reader)?,
    })
}

fn parse_top_four_to_front_side_back(
    reader: &mut BitReader<'_>,
) -> Result<PresentationTopDownmix, PresentationSubstreamError> {
    Ok(PresentationTopDownmix::FourToFrontSideBack {
        front: parse_top_pair_to_front_side_back(reader)?,
        back: parse_top_pair_to_front_side_back(reader)?,
    })
}

fn parse_top_two_to_front_side(
    reader: &mut BitReader<'_>,
) -> Result<PresentationTopDownmix, PresentationSubstreamError> {
    Ok(PresentationTopDownmix::TwoToFrontSide {
        pair: parse_top_pair_to_front_side(reader)?,
    })
}

fn parse_top_two_to_front_side_back(
    reader: &mut BitReader<'_>,
) -> Result<PresentationTopDownmix, PresentationSubstreamError> {
    Ok(PresentationTopDownmix::TwoToFrontSideBack {
        pair: parse_top_pair_to_front_side_back(reader)?,
    })
}

fn parse_presentation_stereo_downmix_coefficients(
    reader: &mut BitReader<'_>,
    has_lfe: bool,
) -> Result<PresentationStereoDownmixCoefficients, PresentationSubstreamError> {
    let loro_centre_mixgain = read_custom_gain_code(reader)?;
    let loro_surround_mixgain =
        read_stereo_surround_mixgain(reader, PresentationStereoDownmixKind::LoRo)?;
    let (ltrt_centre_mixgain, ltrt_surround_mixgain) = if reader.read_flag()? {
        (
            Some(read_custom_gain_code(reader)?),
            Some(read_stereo_surround_mixgain(
                reader,
                PresentationStereoDownmixKind::LtRt,
            )?),
        )
    } else {
        (None, None)
    };
    let (lfe_mixinfo_present, lfe_mixgain) = if has_lfe {
        let present = reader.read_flag()?;
        let gain = if present {
            Some(u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX))
        } else {
            None
        };
        (Some(present), gain)
    } else {
        (None, None)
    };
    let preferred_downmix_method = u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX);

    Ok(PresentationStereoDownmixCoefficients {
        loro_centre_mixgain,
        loro_surround_mixgain,
        ltrt_centre_mixgain,
        ltrt_surround_mixgain,
        lfe_mixinfo_present,
        lfe_mixgain,
        preferred_downmix_method,
    })
}

fn read_stereo_surround_mixgain(
    reader: &mut BitReader<'_>,
    kind: PresentationStereoDownmixKind,
) -> Result<u8, PresentationSubstreamError> {
    let bit_position = reader.bit_position();
    let gain_code = read_custom_gain_code(reader)?;
    if gain_code < 2 {
        return Err(PresentationSubstreamError::ReservedStereoSurroundMixGain {
            kind,
            gain_code,
            bit_position,
        });
    }
    Ok(gain_code)
}

fn parse_loudness_correction(
    reader: &mut BitReader<'_>,
    context: PresentationChannelContext,
) -> Result<PresentationLoudnessCorrectionData, PresentationSubstreamError> {
    let mut data = PresentationLoudnessCorrectionData::default();
    let objects = context.presentation_channel_mode().is_none();
    if objects {
        data.object_loudness_correction = Some(reader.read_flag()?);
    }
    let object_corrections = data.object_loudness_correction == Some(true);

    let full_five_x_or_objects = context
        .presentation_channel_mode()
        .is_some_and(|mode| mode > 4)
        || object_corrections;
    if full_five_x_or_objects {
        data.corrections_for_immersive_output = Some(reader.read_flag()?);
    }

    if context
        .presentation_channel_mode()
        .is_some_and(|mode| mode > 1)
        || object_corrections
    {
        data.loro_downmix = Some(parse_loudness_correction_code(reader)?);
        data.ltrt_downmix = Some(parse_loudness_correction_code(reader)?);
    }

    if full_five_x_or_objects {
        data.five_x = Some(parse_loudness_correction_code(reader)?);
        if data.corrections_for_immersive_output == Some(true) {
            data.five_x_two = Some(parse_loudness_correction_code(reader)?);
            data.seven_x = Some(parse_loudness_correction_code(reader)?);
        }
    }

    if (context
        .presentation_channel_mode()
        .is_some_and(|mode| mode > 10)
        || object_corrections)
        && data.corrections_for_immersive_output == Some(true)
    {
        data.seven_x_four = Some(parse_loudness_correction_code(reader)?);
        data.seven_x_two = Some(parse_loudness_correction_code(reader)?);
        data.five_x_four = Some(parse_loudness_correction_code(reader)?);
    }

    if context.core_channel_mode().is_some_and(|mode| mode >= 5) {
        data.core_five_x_two = Some(parse_loudness_correction_code(reader)?);
    }
    if context.core_channel_mode().is_some_and(|mode| mode >= 3) {
        data.core_five_x = Some(parse_loudness_correction_code(reader)?);
        data.core_stereo = Some(parse_core_stereo_loudness_correction(reader)?);
    }

    if object_corrections {
        data.nine_x_four = Some(parse_loudness_correction_code(reader)?);
    }
    Ok(data)
}

fn parse_loudness_correction_code(
    reader: &mut BitReader<'_>,
) -> Result<PresentationLoudnessCorrectionCode, PresentationSubstreamError> {
    if reader.read_flag()? {
        return Ok(PresentationLoudnessCorrectionCode::Value(
            u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
        ));
    }
    Ok(PresentationLoudnessCorrectionCode::NotPresent)
}

fn parse_core_stereo_loudness_correction(
    reader: &mut BitReader<'_>,
) -> Result<PresentationCoreStereoLoudnessCorrection, PresentationSubstreamError> {
    if !reader.read_flag()? {
        return Ok(PresentationCoreStereoLoudnessCorrection::NotPresent);
    }
    Ok(PresentationCoreStereoLoudnessCorrection::Values {
        loro: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
        ltrt: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
    })
}

fn parse_drc_configuration(
    reader: &mut BitReader<'_>,
) -> Result<PresentationDrcConfiguration, PresentationSubstreamError> {
    let decoder_mode_count_minus_one = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
    let count = usize::from(decoder_mode_count_minus_one).saturating_add(1);
    let mut decoder_modes = [PresentationDrcDecoderMode::EMPTY; MAX_PRESENTATION_DRC_DECODER_MODES];
    for index in 0..count {
        let mode = parse_drc_decoder_mode(reader)?;
        let Some(slot) = decoder_modes.get_mut(index) else {
            return Err(PresentationSubstreamError::CapacityExceeded {
                what: PresentationSubstreamCapacity::DrcDecoderModes,
                declared: u32::try_from(count).unwrap_or(u32::MAX),
                limit: MAX_PRESENTATION_DRC_DECODER_MODES,
            });
        };
        *slot = mode;
    }
    let eac3_profile = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
    Ok(PresentationDrcConfiguration {
        decoder_mode_count_minus_one,
        decoder_modes,
        eac3_profile,
    })
}

fn parse_drc_decoder_mode(
    reader: &mut BitReader<'_>,
) -> Result<PresentationDrcDecoderMode, PresentationSubstreamError> {
    let mode_id = u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX);
    let output_levels = if mode_id > 3 {
        Some(PresentationDrcOutputLevelRange {
            level_from: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
            level_to: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
        })
    } else {
        None
    };

    let profile = if reader.read_flag()? {
        PresentationDrcProfile::Repeat {
            repeat_id: u8::try_from(reader.read_bits(3)?).unwrap_or(u8::MAX),
        }
    } else if reader.read_flag()? {
        PresentationDrcProfile::DefaultEac3
    } else if reader.read_flag()? {
        PresentationDrcProfile::CompressionCurve(parse_drc_compression_curve(reader)?)
    } else {
        PresentationDrcProfile::Gains {
            configuration: u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX),
        }
    };
    Ok(PresentationDrcDecoderMode {
        mode_id,
        output_levels,
        profile,
    })
}

fn parse_drc_compression_curve(
    reader: &mut BitReader<'_>,
) -> Result<PresentationDrcCompressionCurve, PresentationSubstreamError> {
    let nullband_low = u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX);
    let nullband_high = u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX);
    let gain_max_boost = u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX);
    let (level_max_boost, boost_section) = if gain_max_boost > 0 {
        let level = u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX);
        let section = if reader.read_flag()? {
            Some(PresentationDrcCurveSection {
                gain: u8::try_from(reader.read_bits(4)?).unwrap_or(u8::MAX),
                level: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
            })
        } else {
            None
        };
        (Some(level), section)
    } else {
        (None, None)
    };

    let gain_max_cut = u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX);
    let (level_max_cut, cut_section) = if gain_max_cut > 0 {
        let level = u8::try_from(reader.read_bits(6)?).unwrap_or(u8::MAX);
        let section = if reader.read_flag()? {
            Some(PresentationDrcCurveSection {
                gain: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
                level: u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
            })
        } else {
            None
        };
        (Some(level), section)
    } else {
        (None, None)
    };

    let time_constants = if reader.read_flag()? {
        None
    } else {
        let attack = u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX);
        let release = u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX);
        let attack_fast = u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX);
        let release_fast = u8::try_from(reader.read_bits(8)?).unwrap_or(u8::MAX);
        let adaptive_thresholds = if reader.read_flag()? {
            Some((
                u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
                u8::try_from(reader.read_bits(5)?).unwrap_or(u8::MAX),
            ))
        } else {
            None
        };
        Some(PresentationDrcTimeConstants {
            attack,
            release,
            attack_fast,
            release_fast,
            adaptive_thresholds,
        })
    };

    Ok(PresentationDrcCompressionCurve {
        nullband_low,
        nullband_high,
        gain_max_boost,
        level_max_boost,
        boost_section,
        gain_max_cut,
        level_max_cut,
        cut_section,
        time_constants,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentationDrcModeDataKind {
    CompressionCurve,
    Gains { configuration: u8 },
}

fn resolve_drc_mode_data_kind(
    configuration: &PresentationDrcConfiguration,
    mode_id: u8,
    visited: u8,
) -> Result<PresentationDrcModeDataKind, PresentationSubstreamError> {
    let mode_bit = 1u8.checked_shl(u32::from(mode_id)).unwrap_or(0);
    if visited & mode_bit != 0 {
        return Err(PresentationSubstreamError::CyclicDrcRepeatProfile { mode_id });
    }
    let Some(mode) = configuration
        .decoder_modes()
        .iter()
        .rev()
        .find(|mode| mode.mode_id == mode_id)
    else {
        return Err(PresentationSubstreamError::MissingDrcRepeatProfile {
            mode_id,
            repeat_id: mode_id,
        });
    };
    match mode.profile {
        PresentationDrcProfile::Repeat { repeat_id } => {
            if !configuration
                .decoder_modes()
                .iter()
                .any(|candidate| candidate.mode_id == repeat_id)
            {
                return Err(PresentationSubstreamError::MissingDrcRepeatProfile {
                    mode_id,
                    repeat_id,
                });
            }
            resolve_drc_mode_data_kind(configuration, repeat_id, visited | mode_bit)
        }
        PresentationDrcProfile::DefaultEac3 | PresentationDrcProfile::CompressionCurve(_) => {
            Ok(PresentationDrcModeDataKind::CompressionCurve)
        }
        PresentationDrcProfile::Gains { configuration } => {
            Ok(PresentationDrcModeDataKind::Gains { configuration })
        }
    }
}

fn parse_drc_data<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    configuration: &PresentationDrcConfiguration,
) -> Result<PresentationDrcData<'a>, PresentationSubstreamError> {
    let mut gain_sets = [PresentationDrcGainSet::EMPTY; MAX_PRESENTATION_DRC_DECODER_MODES];
    let mut gain_set_count = 0usize;
    let mut curve_present = false;

    for mode in configuration.decoder_modes() {
        match resolve_drc_mode_data_kind(configuration, mode.mode_id, 0)? {
            PresentationDrcModeDataKind::CompressionCurve => curve_present = true,
            PresentationDrcModeDataKind::Gains {
                configuration: gains_configuration,
            } => {
                let size_value_offset = reader.bit_position();
                let mut size = u32::try_from(reader.read_bits(6)?).unwrap_or(u32::MAX);
                if reader.read_flag()? {
                    size = reader.variable_bits_scaled_u32(2, size, 6)?;
                }
                if size < 2 {
                    return Err(PresentationSubstreamError::InvalidDrcGainSetSize {
                        declared: size,
                        minimum: 2,
                        bit_position: size_value_offset,
                    });
                }

                let payload_offset = reader.bit_position();
                if u64::from(size) > reader.remaining_bits() {
                    return Err(PresentationSubstreamError::Read(ReadError::OutOfBounds {
                        requested_bits: size,
                        bit_position: payload_offset,
                        remaining_bits: reader.remaining_bits(),
                    }));
                }
                let mut payload_reader =
                    BitReader::new_bounded(source, payload_offset, u64::from(size))?;
                let version = u8::try_from(payload_reader.read_bits(2)?).unwrap_or(u8::MAX);
                if version <= 1 && size < MIN_KNOWN_DRC_GAIN_SET_BITS {
                    return Err(PresentationSubstreamError::InvalidDrcGainSetSize {
                        declared: size,
                        minimum: MIN_KNOWN_DRC_GAIN_SET_BITS,
                        bit_position: size_value_offset,
                    });
                }
                if version == 0 && gains_configuration == 0 && size != MIN_KNOWN_DRC_GAIN_SET_BITS {
                    return Err(PresentationSubstreamError::InvalidFixedDrcGainSetSize {
                        declared: size,
                        expected: MIN_KNOWN_DRC_GAIN_SET_BITS,
                        bit_position: size_value_offset,
                    });
                }
                let gain_set = PresentationDrcGainSet {
                    decoder_mode_id: mode.mode_id,
                    gains_configuration,
                    size_value_offset,
                    version,
                    payload: PresentationAddDataBits {
                        source,
                        bit_offset: payload_offset,
                        bit_len: u64::from(size),
                    },
                };
                let Some(slot) = gain_sets.get_mut(gain_set_count) else {
                    return Err(PresentationSubstreamError::CapacityExceeded {
                        what: PresentationSubstreamCapacity::DrcGainSets,
                        declared: u32::try_from(gain_set_count)
                            .unwrap_or(u32::MAX)
                            .saturating_add(1),
                        limit: MAX_PRESENTATION_DRC_DECODER_MODES,
                    });
                };
                *slot = gain_set;
                gain_set_count = gain_set_count.saturating_add(1);
                reader.skip_bits(u64::from(size))?;
            }
        }
    }

    let curve = if curve_present {
        Some(PresentationDrcCurveData {
            reset: reader.read_flag()?,
            reserved: u8::try_from(reader.read_bits(2)?).unwrap_or(u8::MAX),
        })
    } else {
        None
    };
    if reader.remaining_bits() != 0 {
        return Err(PresentationSubstreamError::TrailingDrcFrameBits {
            bit_position: reader.bit_position(),
            remaining_bits: reader.remaining_bits(),
        });
    }
    Ok(PresentationDrcData {
        gain_set_count: u8::try_from(gain_set_count).unwrap_or(u8::MAX),
        gain_sets,
        curve,
    })
}

fn parse_drc_data_view<'a>(
    data: PresentationDrcFrameBits<'a>,
    configuration: &PresentationDrcConfiguration,
) -> Result<PresentationDrcData<'a>, PresentationSubstreamError> {
    let mut reader = BitReader::new_bounded(data.source, data.bit_offset, data.bit_len)?;
    parse_drc_data(&mut reader, data.source, configuration)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedPresentationDrcFrame<'a> {
    present: bool,
    frame: PresentationDrcFrameBits<'a>,
    configuration: Option<PresentationDrcConfiguration>,
    data: PresentationDrcFrameBits<'a>,
    data_elements: Option<PresentationDrcData<'a>>,
}

fn parse_drc_frame_envelope<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    independent: bool,
) -> Result<ParsedPresentationDrcFrame<'a>, PresentationSubstreamError> {
    let size_value_offset = reader.bit_position();
    let mut size = u32::try_from(reader.read_bits(5)?).unwrap_or(u32::MAX);
    if reader.read_flag()? {
        size = reader.variable_bits_scaled_u32(3, size, 5)?;
    }
    if size == 0 {
        return Err(PresentationSubstreamError::InvalidDrcMetadataSize {
            declared: size,
            minimum: 1,
            bit_position: size_value_offset,
        });
    }

    let bit_offset = reader.bit_position();
    let mut frame_reader = BitReader::new_bounded(source, bit_offset, u64::from(size))?;
    let present = frame_reader.read_flag()?;
    let configuration = if present && independent {
        Some(parse_drc_configuration(&mut frame_reader)?)
    } else {
        None
    };
    let data_offset = frame_reader.bit_position();
    let data = PresentationDrcFrameBits {
        source,
        bit_offset: data_offset,
        bit_len: frame_reader.remaining_bits(),
    };
    let data_elements = if let Some(configuration) = configuration.as_ref() {
        Some(parse_drc_data(&mut frame_reader, source, configuration)?)
    } else {
        None
    };
    if !present && frame_reader.remaining_bits() != 0 {
        return Err(PresentationSubstreamError::TrailingDrcFrameBits {
            bit_position: frame_reader.bit_position(),
            remaining_bits: frame_reader.remaining_bits(),
        });
    }
    reader.skip_bits(u64::from(size))?;
    let frame = PresentationDrcFrameBits {
        source,
        bit_offset,
        bit_len: u64::from(size),
    };
    Ok(ParsedPresentationDrcFrame {
        present,
        frame,
        configuration,
        data,
        data_elements,
    })
}

fn parse_additional_data<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    context: PresentationSubstreamContext,
) -> Result<Option<PresentationAdditionalData<'a>>, PresentationSubstreamError> {
    if !reader.read_flag()? {
        return Ok(None);
    }

    let mut add_data_bytes = u32::try_from(reader.read_bits(4)?)
        .unwrap_or(0)
        .saturating_add(1);
    if add_data_bytes == 16 {
        add_data_bytes = reader.variable_bits_scaled_u32(2, add_data_bytes, 0)?;
    }
    reader.byte_align()?;

    let region_bit_offset = reader.bit_position();
    let region_bits = u64::from(add_data_bytes).saturating_mul(8);
    reader.skip_bits(region_bits)?;

    let region_byte_offset = usize::try_from(region_bit_offset / 8).map_err(|_| {
        PresentationSubstreamError::Read(ReadError::ValueOverflow {
            bit_position: region_bit_offset,
        })
    })?;
    let region_len = usize::try_from(add_data_bytes).map_err(|_| {
        PresentationSubstreamError::Read(ReadError::ValueOverflow {
            bit_position: region_bit_offset,
        })
    })?;
    let region_end =
        region_byte_offset
            .checked_add(region_len)
            .ok_or(PresentationSubstreamError::Read(ReadError::ValueOverflow {
                bit_position: region_bit_offset,
            }))?;
    let region_source =
        source
            .get(region_byte_offset..region_end)
            .ok_or(PresentationSubstreamError::Read(ReadError::ValueOverflow {
                bit_position: region_bit_offset,
            }))?;

    let mut region = BitReader::new(region_source);
    let parsed = (|| -> Result<_, ReadError> {
        let immersive_audio = region.read_flag()?;
        let oamd_common_timing = if context.pres_ch_mode_undefined() {
            Some(region.read_flag()?)
        } else {
            None
        };
        let advanced_de_data = if region.read_flag()? {
            Some(AdvancedDeData::parse(&mut region)?)
        } else {
            None
        };
        Ok((immersive_audio, oamd_common_timing, advanced_de_data))
    })()
    .map_err(|error| {
        PresentationSubstreamError::Read(rebase_read_error(error, region_bit_offset))
    })?;

    let add_data = PresentationAddDataBits {
        source,
        bit_offset: region_bit_offset.saturating_add(region.bit_position()),
        bit_len: region.remaining_bits(),
    };
    Ok(Some(PresentationAdditionalData {
        add_data_bytes,
        immersive_audio: parsed.0,
        oamd_common_timing: parsed.1,
        advanced_de_data: parsed.2,
        add_data,
    }))
}

fn rebase_read_error(error: ReadError, base: u64) -> ReadError {
    match error {
        ReadError::OutOfBounds {
            requested_bits,
            bit_position,
            remaining_bits,
        } => ReadError::OutOfBounds {
            requested_bits,
            bit_position: base.saturating_add(bit_position),
            remaining_bits,
        },
        ReadError::WidthUnsupported {
            requested_bits,
            bit_position,
        } => ReadError::WidthUnsupported {
            requested_bits,
            bit_position: base.saturating_add(bit_position),
        },
        ReadError::ValueOverflow { bit_position } => ReadError::ValueOverflow {
            bit_position: base.saturating_add(bit_position),
        },
    }
}

fn parse_alternative<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    n_audio_substreams: u32,
) -> Result<AlternativePresentationSelection<'a>, PresentationSubstreamError> {
    let name = if reader.read_flag()? {
        let len = if reader.read_flag()? {
            u8::try_from(reader.read_bits(5)?).unwrap_or(0)
        } else {
            32
        };
        Some(PresentationNameBytes::read(reader, source, len)?)
    } else {
        None
    };

    let mut n_targets = u32::try_from(reader.read_bits(2)?)
        .unwrap_or(0)
        .saturating_add(1);
    if n_targets == 4 {
        n_targets = reader.variable_bits_scaled_u32(2, n_targets, 0)?;
    }
    if n_targets > u32::try_from(MAX_ALTERNATIVE_PRESENTATION_TARGETS).unwrap_or(u32::MAX) {
        return Err(PresentationSubstreamError::CapacityExceeded {
            what: PresentationSubstreamCapacity::Targets,
            declared: n_targets,
            limit: MAX_ALTERNATIVE_PRESENTATION_TARGETS,
        });
    }

    let targets_bit_offset = reader.bit_position();
    for _ in 0..n_targets {
        parse_target(reader, source, n_audio_substreams)?;
    }

    Ok(AlternativePresentationSelection {
        name,
        n_targets,
        n_audio_substreams,
        source,
        targets_bit_offset,
    })
}

fn parse_target<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
    n_audio_substreams: u32,
) -> Result<AlternativePresentationTarget<'a>, PresentationSubstreamError> {
    let target_level = u8::try_from(reader.read_bits(3)?).unwrap_or(0);
    let device_categories = AlternativeTargetDeviceCategories::parse(reader)?;
    let device_category_extension = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(4)?).unwrap_or(0))
    } else {
        None
    };
    let max_ducking_depth = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(6)?).unwrap_or(0))
    } else {
        None
    };
    let loudness_correction_target = if reader.read_flag()? {
        Some(u8::try_from(reader.read_bits(5)?).unwrap_or(0))
    } else {
        None
    };

    let activations_bit_offset = reader.bit_position();
    for index in 0..n_audio_substreams {
        parse_activation(reader, index)?;
    }

    Ok(AlternativePresentationTarget {
        target_level,
        device_categories,
        device_category_extension,
        max_ducking_depth,
        loudness_correction_target,
        source,
        activations_bit_offset,
        n_audio_substreams,
    })
}

fn parse_activation(
    reader: &mut BitReader<'_>,
    substream_index: u32,
) -> Result<AlternativeSubstreamActivation, PresentationSubstreamError> {
    let active = reader.read_flag()?;
    let alternative_data_set_index = if active {
        let first = reader.read_flag()?;
        Some(if first {
            reader.variable_bits_scaled_u32(2, 1, 0)?
        } else {
            0
        })
    } else {
        None
    };
    Ok(AlternativeSubstreamActivation {
        substream_index,
        active,
        alternative_data_set_index,
    })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn test_context(
        alternative: bool,
        n_audio_substreams: u32,
        n_substream_groups: u32,
        pres_ch_mode_undefined: bool,
    ) -> PresentationSubstreamContext {
        let channel = if pres_ch_mode_undefined {
            PresentationChannelContext::UNDEFINED
        } else {
            PresentationChannelContext::new(Some(0), None, false, 0, false)
        };
        PresentationSubstreamContext::new(
            alternative,
            false,
            n_audio_substreams,
            n_substream_groups,
            channel,
        )
    }

    #[derive(Debug, Clone)]
    struct TestBits {
        bytes: [u8; 256],
        len: usize,
    }

    impl TestBits {
        const fn new() -> Self {
            Self {
                bytes: [0; 256],
                len: 0,
            }
        }

        #[expect(
            clippy::arithmetic_side_effects,
            clippy::indexing_slicing,
            reason = "测试 bit writer 的固定缓冲和宽度由用例控制"
        )]
        fn push(&mut self, value: u64, width: u32) {
            for shift in (0..width).rev() {
                if value & (1 << shift) != 0 {
                    self.bytes[self.len / 8] |= 1 << (7 - self.len % 8);
                }
                self.len += 1;
            }
        }

        fn push_byte(&mut self, value: u8) {
            self.push(u64::from(value), 8);
        }

        fn byte_align(&mut self) {
            while !self.len.is_multiple_of(8) {
                self.push(0, 1);
            }
        }

        fn as_bytes(&self) -> &[u8] {
            self.bytes
                .get(..self.len.div_ceil(8))
                .unwrap_or(&self.bytes)
        }
    }

    fn push_minimal_target(bits: &mut TestBits, n_audio_substreams: u32) {
        bits.push(0, 3); // target_level
        bits.push(0, 4); // target_device_category[]
        bits.push(0, 1); // no extension
        bits.push(0, 1); // no ducking depth
        bits.push(0, 1); // no target loudness correction
        for _ in 0..n_audio_substreams {
            bits.push(0, 1); // inactive
        }
    }

    fn push_minimal_loudness_prefix(bits: &mut TestBits, dialnorm_bits: u8) {
        bits.push(u64::from(dialnorm_bits), 7);
        bits.push(0, 1); // no further loudness info
    }

    fn push_minimal_drc_frame(bits: &mut TestBits) {
        bits.push(1, 5); // one-bit drc_frame
        bits.push(0, 1); // no size extension
        bits.push(0, 1); // b_drc_present
    }

    fn push_minimal_common_metadata_prefix(bits: &mut TestBits, dialnorm_bits: u8) {
        push_minimal_loudness_prefix(bits, dialnorm_bits);
        push_minimal_drc_frame(bits);
    }

    fn push_minimal_complete_common_metadata(bits: &mut TestBits, dialnorm_bits: u8) {
        push_minimal_common_metadata_prefix(bits, dialnorm_bits);
        bits.push(0, 1); // no associated audio
    }

    fn drc_state_context(independent: bool) -> PresentationSubstreamContext {
        PresentationSubstreamContext::new(
            false,
            independent,
            1,
            1,
            PresentationChannelContext::new(Some(0), None, false, 0, false),
        )
    }

    fn group_gain_context(
        independent: bool,
        n_substream_groups: u32,
    ) -> PresentationSubstreamContext {
        PresentationSubstreamContext::new(
            false,
            independent,
            n_substream_groups,
            n_substream_groups,
            PresentationChannelContext::new(Some(0), None, false, 0, false),
        )
    }

    fn group_gain_codes(values: &[u8]) -> PresentationSubstreamGroupGainCodes {
        assert!(values.len() <= MAX_GROUPS_PER_PRESENTATION);
        assert!(values.iter().all(|value| *value <= 63));
        let mut codes = [0; MAX_GROUPS_PER_PRESENTATION];
        for (slot, value) in codes.iter_mut().zip(values) {
            *slot = *value;
        }
        PresentationSubstreamGroupGainCodes {
            codes,
            len: values.len(),
        }
    }

    fn push_fixed_gain_presentation(
        bits: &mut TestBits,
        include_configuration: bool,
        gainset_size: u8,
    ) -> u64 {
        bits.push(0, 1); // no additional data
        push_minimal_loudness_prefix(bits, 0);
        let frame_size = if include_configuration {
            22u8.saturating_add(gainset_size)
        } else {
            8u8.saturating_add(gainset_size)
        };
        bits.push(u64::from(frame_size), 5);
        bits.push(0, 1); // no drc_metadata_size extension
        bits.push(1, 1); // b_drc_present
        if include_configuration {
            bits.push(0, 3); // one decoder mode
            bits.push(0, 3); // Home Theatre mode ID
            bits.push(0, 1); // no repeat
            bits.push(0, 1); // no default profile
            bits.push(0, 1); // transmit gains rather than a curve
            bits.push(0, 2); // single wideband gain
            bits.push(0, 3); // E-AC-3 profile None
        }
        let data_offset = bits.len as u64;
        bits.push(u64::from(gainset_size), 6);
        bits.push(0, 1); // no gainset-size extension
        bits.push(0, 2); // drc_version 0
        bits.push(64, 7); // 0 dB₂ wideband gain
        for _ in MIN_KNOWN_DRC_GAIN_SET_BITS..u32::from(gainset_size) {
            bits.push(1, 1); // forbidden version-0 tail for negative tests
        }
        bits.push(0, 1); // no associated audio
        bits.byte_align();
        data_offset
    }

    fn push_loudness_correction_code(bits: &mut TestBits, value: Option<u8>) {
        bits.push(u64::from(value.is_some()), 1);
        if let Some(value) = value {
            bits.push(u64::from(value), 5);
        }
    }

    fn drc_configuration(modes: &[PresentationDrcDecoderMode]) -> PresentationDrcConfiguration {
        assert!(!modes.is_empty());
        assert!(modes.len() <= MAX_PRESENTATION_DRC_DECODER_MODES);
        let mut decoder_modes =
            [PresentationDrcDecoderMode::EMPTY; MAX_PRESENTATION_DRC_DECODER_MODES];
        for (slot, mode) in decoder_modes.iter_mut().zip(modes) {
            *slot = *mode;
        }
        PresentationDrcConfiguration {
            decoder_mode_count_minus_one: u8::try_from(modes.len().saturating_sub(1))
                .unwrap_or(u8::MAX),
            decoder_modes,
            eac3_profile: 0,
        }
    }

    #[test]
    fn ordinary_presentation_has_no_selection_prefix() {
        let parsed = Ac4PresentationSubstreamSelection::parse(
            &[],
            PresentationSubstreamSelectionContext::new(false, 64),
        )
        .unwrap();

        assert_eq!(parsed.alternative, None);
        assert_eq!(parsed.common_metadata_bit_offset, 0);
    }

    #[test]
    fn ordinary_full_parse_reads_dialnorm_without_additional_data() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // b_additional_data
        push_minimal_complete_common_metadata(&mut bits, 0b101_0101);

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false))
                .unwrap();

        assert_eq!(parsed.selection.alternative, None);
        assert_eq!(parsed.selection.common_metadata_bit_offset, 0);
        assert_eq!(parsed.additional_data, None);
        assert_eq!(parsed.dialnorm_bits_offset, 1);
        assert_eq!(parsed.dialnorm_bits, 0b101_0101);
        assert_eq!(parsed.further_loudness, None);
        assert_eq!(parsed.drc_metadata_size_value_offset, 9);
        assert!(!parsed.drc_present);
        assert_eq!(parsed.drc_frame.bit_offset(), 15);
        assert_eq!(parsed.drc_frame.len_bits(), 1);
        assert_eq!(parsed.drc_frame.get(0), Some(false));
        assert_eq!(parsed.drc_frame.end_bit_offset(), 16);
        assert_eq!(parsed.drc_configuration, None);
        assert!(parsed.drc_data.is_empty());
        assert_eq!(parsed.drc_data.bit_offset(), 16);
        assert_eq!(parsed.drc_data_elements, None);
        assert_eq!(
            parsed.substream_group_gain_update,
            PresentationSubstreamGroupGainUpdate::NotSignaled
        );
        assert_eq!(parsed.b_associated_offset, 16);
        assert_eq!(parsed.associated_audio, None);
        assert_eq!(parsed.custom_downmix_offset, 17);
    }

    #[test]
    fn rejects_missing_further_loudness_presence_flag() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // b_additional_data
        bits.push(0, 7); // dialnorm_bits exactly fills the only byte

        assert_eq!(
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false),)
                .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 1,
                bit_position: 8,
                remaining_bits: 0,
            })
        );
    }

    #[test]
    fn default_context_keeps_zero_group_channel_mode_undefined() {
        let context = PresentationSubstreamContext::default();
        assert_eq!(context, test_context(false, 0, 0, true));

        let mut bits = TestBits::new();
        bits.push(1, 1); // b_additional_data
        bits.push(0, 4); // one additional-data byte
        bits.byte_align();
        bits.push(0, 1); // immersive_audio_indicator
        bits.push(1, 1); // b_oamd_common_timing
        bits.push(0, 1); // no advanced DE
        bits.push(0, 5); // reserved add_data
        push_minimal_complete_common_metadata(&mut bits, 0);
        bits.push(0, 1); // b_obj_loud_corr

        let parsed = Ac4PresentationSubstream::parse(bits.as_bytes(), context).unwrap();
        let additional = parsed.additional_data.unwrap();
        assert_eq!(additional.oamd_common_timing, Some(true));
        assert_eq!(additional.advanced_de_data, None);
        assert_eq!(additional.add_data.len_bits(), 5);
        assert_eq!(parsed.dialnorm_bits_offset, 16);
        assert_eq!(parsed.drc_metadata_size_value_offset, 24);
    }

    #[test]
    fn parses_object_additional_data_and_ignores_byte_alignment_values_in_equality() {
        let mut envelope = TestBits::new();
        envelope.push(1, 1); // b_additional_data
        envelope.push(0, 4); // one additional-data byte
        envelope.byte_align();
        envelope.push(1, 1); // immersive_audio_indicator
        envelope.push(0, 1); // b_oamd_common_timing
        envelope.push(0, 1); // no advanced DE
        envelope.push(0b10101, 5); // reserved add_data
        let expected_dialnorm_offset = envelope.len as u64;

        let mut left_bits = envelope.clone();
        push_minimal_complete_common_metadata(&mut left_bits, 0b101_0101);
        left_bits.push(0, 1); // b_obj_loud_corr
        left_bits.push(0, 7); // byte_align
        let mut right_bits = envelope;
        push_minimal_complete_common_metadata(&mut right_bits, 0b101_0101);
        right_bits.push(0, 1); // same b_obj_loud_corr
        right_bits.push(0x7f, 7); // different byte_align values
        let context = test_context(false, 1, 1, true);
        let left = Ac4PresentationSubstream::parse(left_bits.as_bytes(), context).unwrap();
        let right = Ac4PresentationSubstream::parse(right_bits.as_bytes(), context).unwrap();

        let additional = left.additional_data.unwrap();
        assert_eq!(additional.add_data_bytes, 1);
        assert!(additional.immersive_audio);
        assert_eq!(additional.oamd_common_timing, Some(false));
        assert_eq!(additional.advanced_de_data, None);
        assert_eq!(additional.add_data.len_bits(), 5);
        assert_eq!(
            additional.add_data.iter().collect::<std::vec::Vec<_>>(),
            [true, false, true, false, true]
        );
        assert_eq!(additional.add_data.get(5), None);
        assert_eq!(additional.add_data.as_aligned_slice(), None);
        assert_eq!(left.dialnorm_bits_offset, expected_dialnorm_offset);
        assert_eq!(left.dialnorm_bits, 0b101_0101);
        assert_eq!(left.drc_metadata_size_value_offset, 24);
        assert_eq!(left.drc_frame.bit_offset(), 30);
        assert_eq!(left.drc_frame.end_bit_offset(), 31);
        assert_eq!(
            left.substream_group_gain_update,
            PresentationSubstreamGroupGainUpdate::NotSignaled
        );
        assert_eq!(left.b_associated_offset, 31);
        assert_eq!(left.associated_audio, None);
        assert_eq!(left.custom_downmix_offset, 32);
        assert_eq!(left.loudness_correction_offset, 32);
        assert_eq!(left.byte_alignment_offset, 33);
        assert_eq!(left.alignment_bits, 7);
        assert_eq!(left, right, "byte_align 的填充值没有语义，不进入解析视图");
    }

    #[test]
    fn defined_channel_mode_omits_oamd_common_timing() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // b_additional_data
        bits.push(0, 4); // one additional-data byte
        bits.byte_align();
        bits.push(0, 1); // immersive_audio_indicator
        bits.push(0, 1); // no advanced DE; no OAMD timing bit on this path
        bits.push(0b11_1000, 6); // reserved add_data
        push_minimal_complete_common_metadata(&mut bits, 0);

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false))
                .unwrap();
        let additional = parsed.additional_data.unwrap();
        assert_eq!(additional.oamd_common_timing, None);
        assert_eq!(additional.add_data.len_bits(), 6);
        assert_eq!(
            additional.add_data.iter().collect::<std::vec::Vec<_>>(),
            [true, true, true, false, false, false]
        );
        assert_eq!(parsed.dialnorm_bits_offset, 16);
    }

    #[test]
    fn parses_advanced_de_raw_fields_and_exact_reserved_tail() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // b_additional_data
        bits.push(3, 4); // four additional-data bytes
        bits.byte_align();
        bits.push(1, 1); // immersive_audio_indicator
        bits.push(1, 1); // b_oamd_common_timing
        bits.push(1, 1); // b_advanced_de_data_present
        bits.push(1, 1); // b_advanced_de_config_present
        bits.push(63, 6);
        bits.push(62, 6);
        bits.push(15, 4);
        bits.push(32, 6); // signed threshold -32
        bits.push(31, 5);
        bits.push(1, 1); // one reserved add_data bit
        let expected_dialnorm_offset = bits.len as u64;
        push_minimal_complete_common_metadata(&mut bits, 0);
        bits.push(0, 1); // b_obj_loud_corr

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, true))
                .unwrap();
        let additional = parsed.additional_data.unwrap();
        let advanced = additional.advanced_de_data.unwrap();
        assert_eq!(
            advanced.config,
            Some(AdvancedDeConfig {
                compressor_time_constant_attack: 63,
                compressor_time_constant_release: 62,
                compressor_ratio: 15,
            })
        );
        assert_eq!(advanced.compressor_threshold, -32);
        assert_eq!(advanced.compressor_gain, 31);
        assert_eq!(additional.add_data.len_bits(), 1);
        assert_eq!(additional.add_data.get(0), Some(true));
        assert_eq!(parsed.dialnorm_bits_offset, expected_dialnorm_offset);
    }

    #[test]
    fn parses_advanced_de_without_optional_config() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // b_additional_data
        bits.push(1, 4); // two additional-data bytes
        bits.byte_align();
        bits.push(0, 1); // immersive_audio_indicator
        bits.push(0, 1); // b_oamd_common_timing
        bits.push(1, 1); // b_advanced_de_data_present
        bits.push(0, 1); // no advanced DE config
        bits.push(31, 6); // positive threshold endpoint
        bits.push(0, 5); // gain endpoint
        bits.push(0, 1); // one reserved add_data bit
        push_minimal_complete_common_metadata(&mut bits, 0);
        bits.push(0, 1); // b_obj_loud_corr

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, true))
                .unwrap();
        let additional = parsed.additional_data.unwrap();
        assert_eq!(
            additional.advanced_de_data,
            Some(AdvancedDeData {
                config: None,
                compressor_threshold: 31,
                compressor_gain: 0,
            })
        );
        assert_eq!(additional.add_data.len_bits(), 1);
        assert_eq!(parsed.dialnorm_bits_offset, 24);
    }

    #[test]
    fn advanced_de_cannot_read_past_declared_additional_data() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // b_additional_data
        bits.push(0, 4); // one additional-data byte
        bits.byte_align();
        bits.push(0, 1); // immersive_audio_indicator
        bits.push(0, 1); // b_oamd_common_timing
        bits.push(1, 1); // b_advanced_de_data_present
        bits.push(0, 1); // no advanced DE config
        bits.push(0b1111, 4); // threshold is truncated inside the declared byte
        bits.push_byte(0xff); // dialnorm/suffix bytes must not satisfy the bounded read

        assert_eq!(
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, true),)
                .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 6,
                bit_position: 12,
                remaining_bits: 4,
            })
        );
    }

    #[test]
    fn extends_and_bounds_additional_data_byte_length() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // b_additional_data
        bits.push(15, 4); // base 16 bytes
        bits.push(2, 2); // variable_bits(2) = 2
        bits.push(0, 1);
        bits.push(1, 1); // immersive_audio_indicator
        bits.push(1, 1); // b_oamd_common_timing
        bits.push(0, 1); // no advanced DE
        for _ in 0..141 {
            bits.push(0, 1);
        }
        push_minimal_complete_common_metadata(&mut bits, 0);
        bits.push(0, 1); // b_obj_loud_corr

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, true))
                .unwrap();
        let additional = parsed.additional_data.unwrap();
        assert_eq!(additional.add_data_bytes, 18);
        assert_eq!(additional.add_data.len_bits(), 141);
        assert_eq!(parsed.dialnorm_bits_offset, 152);
    }

    #[test]
    fn parses_complete_presentation_further_loudness_prefix() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        bits.push(0b110_1101, 7); // dialnorm_bits
        bits.push(1, 1); // b_further_loudness_info
        bits.push(3, 2); // loudness_version
        bits.push(12, 4); // extended_loudness_version; effective version 15
        bits.push(1, 4); // loud_prac_type: ATSC A/85
        bits.push(1, 1); // b_loudcorr_dialgate
        bits.push(2, 3); // dialgate_prac_type
        bits.push(1, 1); // b_loudcorr_type
        bits.push(1, 1); // b_loudrelgat
        bits.push(1023, 11);
        bits.push(1, 1); // b_loudspchgat
        bits.push(1000, 11);
        bits.push(3, 3);
        bits.push(1, 1); // b_loudstrm3s
        bits.push(0, 11);
        bits.push(1, 1); // b_max_loudstrm3s
        bits.push(2047, 11);
        bits.push(1, 1); // b_truepk
        bits.push(1024, 11);
        bits.push(1, 1); // b_max_truepk
        bits.push(1500, 11);
        bits.push(1, 1); // b_prgmbndy
        bits.push(0b001, 3); // prgmbndy = 8
        bits.push(1, 1); // boundary is upcoming
        bits.push(1, 1); // b_prgmbndy_offset
        bits.push(2047, 11);
        bits.push(1, 1); // b_lra
        bits.push(1023, 10);
        bits.push(7, 3);
        bits.push(1, 1); // b_loudmntry
        bits.push(1, 11);
        bits.push(1, 1); // b_max_loudmntry
        bits.push(2, 11);
        bits.push(1, 1); // b_rtllcomp
        bits.push(0xa5, 8);
        bits.push(1, 1); // b_extension
        bits.push(5, 5);
        bits.push(0b10110, 5); // opaque extensions_bits
        let expected_drc_offset = bits.len as u64;
        push_minimal_drc_frame(&mut bits);
        bits.push(0, 1); // no associated audio

        let payload = bits.as_bytes();
        let parsed =
            Ac4PresentationSubstream::parse(payload, test_context(false, 1, 1, false)).unwrap();
        let loudness = parsed.further_loudness.unwrap();

        assert_eq!(parsed.dialnorm_bits_offset, 1);
        assert_eq!(parsed.dialnorm_bits, 0b110_1101);
        assert_eq!(parsed.drc_metadata_size_value_offset, expected_drc_offset);
        assert!(!parsed.drc_present);
        assert_eq!(parsed.drc_frame.len_bits(), 1);
        assert_eq!(loudness.loudness_version, Some(3));
        assert_eq!(loudness.extended_loudness_version, Some(12));
        assert_eq!(loudness.effective_loudness_version(), Some(15));
        assert_eq!(loudness.loud_prac_type, Some(1));
        assert_eq!(loudness.loudcorr_dialgate, Some(true));
        assert_eq!(loudness.loudcorr_dialgate_prac_type, Some(2));
        assert_eq!(loudness.loudcorr_type, Some(true));
        assert_eq!(loudness.loudrelgat, Some(1023));
        assert_eq!(loudness.loudspchgat, Some((1000, 3)));
        assert_eq!(loudness.loudstrm3s, Some(0));
        assert_eq!(loudness.max_loudstrm3s, Some(2047));
        assert_eq!(loudness.truepk, Some(1024));
        assert_eq!(loudness.max_truepk, Some(1500));
        assert_eq!(
            loudness.programme_boundary,
            Some(crate::audio_substream::LoudnessProgrammeBoundary {
                frame_distance: 8,
                upcoming: true,
                sample_offset: Some(2047),
            })
        );
        assert_eq!(loudness.lra, Some((1023, 7)));
        assert_eq!(loudness.loudmntry, Some(1));
        assert_eq!(loudness.max_loudmntry, Some(2));
        assert_eq!(loudness.rtll_comp, Some(0xa5));
        assert_eq!(loudness.extension_bits, Some(5));
        assert_eq!(
            loudness.extension_bits_offset,
            Some(expected_drc_offset.saturating_sub(5))
        );
        let extension = loudness.extension_data(payload).unwrap();
        assert_eq!(extension.len_bits(), 5);
        assert_eq!(
            extension.iter().collect::<std::vec::Vec<_>>(),
            [true, false, true, true, false]
        );
        assert_eq!(extension.get(5), None);
        assert_eq!(extension.as_aligned_slice(), None);
    }

    #[test]
    fn extends_and_bounds_presentation_drc_frame() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        push_minimal_loudness_prefix(&mut bits, 0);
        let expected_size_offset = bits.len as u64;
        bits.push(31, 5); // base drc_metadata_size
        bits.push(1, 1); // b_more_bits
        bits.push(2, 3); // variable_bits(3) = 2
        bits.push(0, 1);
        let expected_frame_offset = bits.len as u64;
        bits.push(1, 1); // b_drc_present
        for _ in 1..95 {
            bits.push(0, 1);
        }
        let expected_frame_end = bits.len as u64;
        bits.push(0, 1); // no associated audio
        let expected_custom_downmix_offset = bits.len as u64;
        bits.push(0b10101, 5); // byte_align

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false))
                .unwrap();

        assert_eq!(parsed.drc_metadata_size_value_offset, expected_size_offset);
        assert!(parsed.drc_present);
        assert_eq!(parsed.drc_frame.bit_offset(), expected_frame_offset);
        assert_eq!(parsed.drc_frame.len_bits(), 95);
        assert_eq!(parsed.drc_frame.end_bit_offset(), expected_frame_end);
        assert_eq!(parsed.drc_frame.get(0), Some(true));
        assert_eq!(parsed.drc_frame.get(94), Some(false));
        assert_eq!(parsed.drc_frame.get(95), None);
        assert_eq!(parsed.drc_frame.as_aligned_slice(), None);
        assert_eq!(
            parsed.substream_group_gain_update,
            PresentationSubstreamGroupGainUpdate::NotSignaled
        );
        assert_eq!(parsed.b_associated_offset, expected_frame_end);
        assert_eq!(parsed.associated_audio, None);
        assert_eq!(parsed.custom_downmix_offset, expected_custom_downmix_offset);
    }

    #[test]
    fn parses_complete_independent_drc_configuration_and_preserves_data() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        push_minimal_loudness_prefix(&mut bits, 0);
        bits.push(5, 5); // 5 + (5 << 5) = 165-bit drc_frame
        bits.push(1, 1); // b_more_bits
        bits.push(5, 3); // variable_bits(3) = 5
        bits.push(0, 1); // stop variable_bits
        let frame_offset = bits.len as u64;
        bits.push(1, 1); // b_drc_present
        let configuration_offset = bits.len;
        bits.push(3, 3); // four decoder modes

        bits.push(2, 3); // mode 0: portable speakers
        bits.push(0, 1); // no repeat
        bits.push(1, 1); // default E-AC-3 profile

        bits.push(5, 3); // mode 1: custom mode ID
        bits.push(1, 5); // output level from
        bits.push(31, 5); // output level to
        bits.push(1, 1); // repeat profile
        bits.push(2, 3); // repeat mode 0's ID

        bits.push(3, 3); // mode 2: portable headphones
        bits.push(0, 1); // no repeat
        bits.push(0, 1); // no default profile
        bits.push(0, 1); // transmit gains rather than a curve
        bits.push(3, 2); // drc_gains_config

        bits.push(7, 3); // mode 3: custom mode ID
        bits.push(4, 5); // output level from
        bits.push(27, 5); // output level to
        bits.push(0, 1); // no repeat
        bits.push(0, 1); // no default profile
        bits.push(1, 1); // explicit compression curve
        bits.push(1, 4); // drc_lev_nullband_low
        bits.push(2, 4); // drc_lev_nullband_high
        bits.push(3, 4); // drc_gain_max_boost
        bits.push(4, 5); // drc_lev_max_boost
        bits.push(1, 1); // one extra boost section
        bits.push(5, 4); // drc_gain_section_boost
        bits.push(6, 5); // drc_lev_section_boost
        bits.push(7, 5); // drc_gain_max_cut
        bits.push(8, 6); // drc_lev_max_cut
        bits.push(1, 1); // one extra cut section
        bits.push(9, 5); // drc_gain_section_cut
        bits.push(10, 5); // drc_lev_section_cut
        bits.push(0, 1); // custom time constants
        bits.push(11, 8); // drc_tc_attack
        bits.push(12, 8); // drc_tc_release
        bits.push(13, 8); // drc_tc_attack_fast
        bits.push(14, 8); // drc_tc_release_fast
        bits.push(1, 1); // adaptive smoothing
        bits.push(15, 5); // drc_attack_threshold
        bits.push(16, 5); // drc_release_threshold
        bits.push(5, 3); // speech E-AC-3 profile
        assert_eq!(bits.len.saturating_sub(configuration_offset), 145);

        let data_offset = bits.len as u64;
        let gainset_size_offset = bits.len as u64;
        bits.push(9, 6); // version plus seven opaque drc_gains bits
        bits.push(0, 1); // no gainset size extension
        let gainset_offset = bits.len as u64;
        bits.push(0, 2); // drc_version 0
        bits.push(0b100_0000, 7); // opaque drc_gains body
        bits.push(1, 1); // drc_reset_flag for curve modes
        bits.push(3, 2); // drc_reserved
        assert_eq!((bits.len as u64).saturating_sub(frame_offset), 165);
        let frame_end = bits.len as u64;
        bits.push(0, 1); // no associated audio
        bits.byte_align();

        let context = PresentationSubstreamContext::new(
            false,
            true,
            1,
            1,
            PresentationChannelContext::new(Some(0), None, false, 0, false),
        );
        let parsed = Ac4PresentationSubstream::parse(bits.as_bytes(), context).unwrap();

        assert!(context.presentation_is_independent());
        assert!(parsed.drc_present);
        assert_eq!(parsed.drc_frame.bit_offset(), frame_offset);
        assert_eq!(parsed.drc_frame.len_bits(), 165);
        assert_eq!(parsed.drc_frame.end_bit_offset(), frame_end);
        let configuration = parsed.drc_configuration.unwrap();
        assert_eq!(configuration.decoder_mode_count_minus_one(), 3);
        assert_eq!(configuration.eac3_profile, 5);
        assert_eq!(
            configuration.decoder_modes(),
            &[
                PresentationDrcDecoderMode {
                    mode_id: 2,
                    output_levels: None,
                    profile: PresentationDrcProfile::DefaultEac3,
                },
                PresentationDrcDecoderMode {
                    mode_id: 5,
                    output_levels: Some(PresentationDrcOutputLevelRange {
                        level_from: 1,
                        level_to: 31,
                    }),
                    profile: PresentationDrcProfile::Repeat { repeat_id: 2 },
                },
                PresentationDrcDecoderMode {
                    mode_id: 3,
                    output_levels: None,
                    profile: PresentationDrcProfile::Gains { configuration: 3 },
                },
                PresentationDrcDecoderMode {
                    mode_id: 7,
                    output_levels: Some(PresentationDrcOutputLevelRange {
                        level_from: 4,
                        level_to: 27,
                    }),
                    profile: PresentationDrcProfile::CompressionCurve(
                        PresentationDrcCompressionCurve {
                            nullband_low: 1,
                            nullband_high: 2,
                            gain_max_boost: 3,
                            level_max_boost: Some(4),
                            boost_section: Some(PresentationDrcCurveSection { gain: 5, level: 6 }),
                            gain_max_cut: 7,
                            level_max_cut: Some(8),
                            cut_section: Some(PresentationDrcCurveSection { gain: 9, level: 10 }),
                            time_constants: Some(PresentationDrcTimeConstants {
                                attack: 11,
                                release: 12,
                                attack_fast: 13,
                                release_fast: 14,
                                adaptive_thresholds: Some((15, 16)),
                            }),
                        }
                    ),
                },
            ]
        );
        assert_eq!(parsed.drc_data.bit_offset(), data_offset);
        assert_eq!(parsed.drc_data.len_bits(), 19);
        let data = parsed.drc_data_elements.unwrap();
        assert_eq!(data.gain_sets().len(), 1);
        assert_eq!(
            data.gain_sets().first(),
            Some(&PresentationDrcGainSet {
                decoder_mode_id: 3,
                gains_configuration: 3,
                size_value_offset: gainset_size_offset,
                version: 0,
                payload: PresentationAddDataBits {
                    source: bits.as_bytes(),
                    bit_offset: gainset_offset,
                    bit_len: 9,
                },
            })
        );
        let gain_set = data.gain_sets().first().copied().unwrap();
        assert_eq!(gain_set.body().bit_offset(), gainset_offset + 2);
        assert_eq!(gain_set.body().len_bits(), 7);
        assert_eq!(
            gain_set.body().iter().collect::<std::vec::Vec<_>>(),
            [true, false, false, false, false, false, false]
        );
        assert_eq!(
            data.curve,
            Some(PresentationDrcCurveData {
                reset: true,
                reserved: 3,
            })
        );
        assert_eq!(parsed.b_associated_offset, frame_end);
    }

    #[test]
    fn parses_drc_decoder_mode_count_boundaries() {
        for decoder_mode_count_minus_one in [0u8, 7] {
            let mut bits = TestBits::new();
            bits.push(u64::from(decoder_mode_count_minus_one), 3);
            for mode_id in 0..=decoder_mode_count_minus_one {
                bits.push(u64::from(mode_id), 3);
                if mode_id > 3 {
                    bits.push(u64::from(mode_id), 5); // output level from
                    bits.push(u64::from(31u8.saturating_sub(mode_id)), 5); // output level to
                }
                bits.push(0, 1); // no repeat
                bits.push(1, 1); // default E-AC-3 profile
            }
            bits.push(7, 3); // reserved E-AC-3 profile remains an original code

            let mut reader = BitReader::new(bits.as_bytes());
            let configuration = parse_drc_configuration(&mut reader).unwrap();

            assert_eq!(reader.bit_position(), bits.len as u64);
            assert_eq!(
                configuration.decoder_mode_count_minus_one(),
                decoder_mode_count_minus_one
            );
            assert_eq!(
                configuration.decoder_modes().len(),
                usize::from(decoder_mode_count_minus_one).saturating_add(1)
            );
            assert_eq!(configuration.eac3_profile, 7);
        }
    }

    #[test]
    fn parses_extended_unknown_drc_gainset_envelope() {
        let configuration = drc_configuration(&[PresentationDrcDecoderMode {
            mode_id: 0,
            output_levels: None,
            profile: PresentationDrcProfile::Gains { configuration: 0 },
        }]);
        let mut bits = TestBits::new();
        bits.push(6, 6); // 6 + (1 << 6) = 70-bit gain set
        bits.push(1, 1); // b_more_bits
        bits.push(1, 2); // variable_bits(2) = 1
        bits.push(0, 1); // stop variable_bits
        let payload_offset = bits.len as u64;
        bits.push(2, 2); // unknown drc_version
        for index in 0u64..68 {
            bits.push(index % 2, 1); // opaque drc2_bits
        }

        let mut reader = BitReader::new_bounded(bits.as_bytes(), 0, bits.len as u64).unwrap();
        let parsed = parse_drc_data(&mut reader, bits.as_bytes(), &configuration).unwrap();

        assert_eq!(reader.remaining_bits(), 0);
        assert_eq!(parsed.curve, None);
        assert_eq!(parsed.gain_sets().len(), 1);
        let gain_set = parsed.gain_sets().first().copied().unwrap();
        assert_eq!(gain_set.decoder_mode_id, 0);
        assert_eq!(gain_set.gains_configuration, 0);
        assert_eq!(gain_set.size_value_offset, 0);
        assert_eq!(gain_set.version, 2);
        assert_eq!(gain_set.payload.bit_offset(), payload_offset);
        assert_eq!(gain_set.payload.len_bits(), 70);
        assert_eq!(gain_set.body().len_bits(), 68);
        assert_eq!(gain_set.body().get(0), Some(false));
        assert_eq!(gain_set.body().get(1), Some(true));
        assert_eq!(gain_set.body().get(68), None);
    }

    #[test]
    fn resolves_forward_and_chained_drc_repeat_profiles_for_data() {
        let configuration = drc_configuration(&[
            PresentationDrcDecoderMode {
                mode_id: 0,
                output_levels: None,
                profile: PresentationDrcProfile::Repeat { repeat_id: 2 },
            },
            PresentationDrcDecoderMode {
                mode_id: 1,
                output_levels: None,
                profile: PresentationDrcProfile::Gains { configuration: 1 },
            },
            PresentationDrcDecoderMode {
                mode_id: 2,
                output_levels: None,
                profile: PresentationDrcProfile::Repeat { repeat_id: 1 },
            },
        ]);
        let mut bits = TestBits::new();
        for _ in 0..3 {
            bits.push(2, 6); // gainset contains only drc_version
            bits.push(0, 1); // no size extension
            bits.push(2, 2); // unknown version with an empty extension body
        }
        let mut reader = BitReader::new_bounded(bits.as_bytes(), 0, bits.len as u64).unwrap();
        let parsed = parse_drc_data(&mut reader, bits.as_bytes(), &configuration).unwrap();

        assert_eq!(parsed.curve, None);
        assert_eq!(
            parsed
                .gain_sets()
                .iter()
                .map(|gain_set| (gain_set.decoder_mode_id, gain_set.gains_configuration))
                .collect::<std::vec::Vec<_>>(),
            [(0, 1), (1, 1), (2, 1)]
        );
    }

    #[test]
    fn parses_drc_gainsets_at_decoder_mode_capacity() {
        let mut modes = [PresentationDrcDecoderMode::EMPTY; MAX_PRESENTATION_DRC_DECODER_MODES];
        for (mode_id, mode) in (0u8..8).zip(modes.iter_mut()) {
            *mode = PresentationDrcDecoderMode {
                mode_id,
                output_levels: (mode_id > 3).then_some(PresentationDrcOutputLevelRange {
                    level_from: mode_id,
                    level_to: mode_id,
                }),
                profile: PresentationDrcProfile::Gains {
                    configuration: mode_id % 4,
                },
            };
        }
        let configuration = drc_configuration(&modes);
        let mut bits = TestBits::new();
        for _ in 0..MAX_PRESENTATION_DRC_DECODER_MODES {
            bits.push(2, 6);
            bits.push(0, 1);
            bits.push(2, 2);
        }
        let mut reader = BitReader::new_bounded(bits.as_bytes(), 0, bits.len as u64).unwrap();
        let parsed = parse_drc_data(&mut reader, bits.as_bytes(), &configuration).unwrap();

        assert_eq!(parsed.gain_sets().len(), MAX_PRESENTATION_DRC_DECODER_MODES);
        for (mode_id, gain_set) in (0u8..8).zip(parsed.gain_sets()) {
            assert_eq!(gain_set.decoder_mode_id, mode_id);
            assert_eq!(gain_set.gains_configuration, mode_id % 4);
        }
        assert_eq!(parsed.curve, None);
    }

    #[test]
    fn rejects_missing_and_cyclic_drc_repeat_profiles() {
        let missing = drc_configuration(&[PresentationDrcDecoderMode {
            mode_id: 0,
            output_levels: None,
            profile: PresentationDrcProfile::Repeat { repeat_id: 7 },
        }]);
        assert_eq!(
            parse_drc_data(&mut BitReader::new(&[]), &[], &missing).unwrap_err(),
            PresentationSubstreamError::MissingDrcRepeatProfile {
                mode_id: 0,
                repeat_id: 7,
            }
        );

        let cyclic = drc_configuration(&[
            PresentationDrcDecoderMode {
                mode_id: 0,
                output_levels: None,
                profile: PresentationDrcProfile::Repeat { repeat_id: 1 },
            },
            PresentationDrcDecoderMode {
                mode_id: 1,
                output_levels: None,
                profile: PresentationDrcProfile::Repeat { repeat_id: 0 },
            },
        ]);
        assert_eq!(
            parse_drc_data(&mut BitReader::new(&[]), &[], &cyclic).unwrap_err(),
            PresentationSubstreamError::CyclicDrcRepeatProfile { mode_id: 0 }
        );
    }

    #[test]
    fn rejects_invalid_truncated_and_overflowing_drc_gainset_envelopes() {
        let configuration = drc_configuration(&[PresentationDrcDecoderMode {
            mode_id: 0,
            output_levels: None,
            profile: PresentationDrcProfile::Gains { configuration: 0 },
        }]);

        let mut too_small = TestBits::new();
        too_small.push(1, 6);
        too_small.push(0, 1);
        too_small.push(0, 1);
        let mut reader =
            BitReader::new_bounded(too_small.as_bytes(), 0, too_small.len as u64).unwrap();
        assert_eq!(
            parse_drc_data(&mut reader, too_small.as_bytes(), &configuration).unwrap_err(),
            PresentationSubstreamError::InvalidDrcGainSetSize {
                declared: 1,
                minimum: 2,
                bit_position: 0,
            }
        );

        let mut truncated = TestBits::new();
        truncated.push(10, 6);
        truncated.push(0, 1);
        truncated.push(0, 2); // only two gainset bits inside the parent envelope
        let parent_end = truncated.len as u64;
        truncated.push_byte(0xff); // following metadata remains readable in the source
        let mut reader = BitReader::new_bounded(truncated.as_bytes(), 0, parent_end).unwrap();
        assert_eq!(
            parse_drc_data(&mut reader, truncated.as_bytes(), &configuration).unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 10,
                bit_position: 7,
                remaining_bits: 2,
            })
        );

        let mut overflowing = TestBits::new();
        overflowing.push(63, 6);
        overflowing.push(1, 1);
        for _ in 0..16 {
            overflowing.push(3, 2);
            overflowing.push(1, 1);
        }
        overflowing.push(0, 2);
        overflowing.push(0, 1);
        assert!(matches!(
            parse_drc_data(
                &mut BitReader::new(overflowing.as_bytes()),
                overflowing.as_bytes(),
                &configuration,
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::ValueOverflow { .. }
            ))
        ));
    }

    #[test]
    fn validates_known_drc_gainset_sizes() {
        let configuration = drc_configuration(&[PresentationDrcDecoderMode {
            mode_id: 0,
            output_levels: None,
            profile: PresentationDrcProfile::Gains { configuration: 0 },
        }]);

        for version in 0u8..=1 {
            let mut too_short = TestBits::new();
            too_short.push(8, 6); // version plus only six of seven required gain bits
            too_short.push(0, 1); // no size extension
            too_short.push(u64::from(version), 2);
            too_short.push(0, 6);
            let mut reader =
                BitReader::new_bounded(too_short.as_bytes(), 0, too_short.len as u64).unwrap();
            assert_eq!(
                parse_drc_data(&mut reader, too_short.as_bytes(), &configuration).unwrap_err(),
                PresentationSubstreamError::InvalidDrcGainSetSize {
                    declared: 8,
                    minimum: MIN_KNOWN_DRC_GAIN_SET_BITS,
                    bit_position: 0,
                }
            );
        }

        let mut fixed_with_tail = TestBits::new();
        fixed_with_tail.push(10, 6); // version 0 config 0 has no extension after seven gain bits
        fixed_with_tail.push(0, 1);
        fixed_with_tail.push(0, 2);
        fixed_with_tail.push(0, 8);
        let mut reader =
            BitReader::new_bounded(fixed_with_tail.as_bytes(), 0, fixed_with_tail.len as u64)
                .unwrap();
        assert_eq!(
            parse_drc_data(&mut reader, fixed_with_tail.as_bytes(), &configuration).unwrap_err(),
            PresentationSubstreamError::InvalidFixedDrcGainSetSize {
                declared: 10,
                expected: MIN_KNOWN_DRC_GAIN_SET_BITS,
                bit_position: 0,
            }
        );

        let mut fixed = TestBits::new();
        fixed.push(u64::from(MIN_KNOWN_DRC_GAIN_SET_BITS), 6);
        fixed.push(0, 1);
        fixed.push(0, 2); // version 0
        fixed.push(64, 7); // the single wideband drc_gain_val
        let mut reader = BitReader::new_bounded(fixed.as_bytes(), 0, fixed.len as u64).unwrap();
        let parsed = parse_drc_data(&mut reader, fixed.as_bytes(), &configuration).unwrap();
        assert_eq!(reader.remaining_bits(), 0);
        assert_eq!(parsed.gain_sets().len(), 1);
        let gain_set = parsed.gain_sets().first().copied().unwrap();
        assert_eq!(gain_set.version, 0);
        assert_eq!(gain_set.body().len_bits(), 7);

        let mut version_one = TestBits::new();
        version_one.push(10, 6);
        version_one.push(0, 1);
        version_one.push(1, 2);
        version_one.push(64, 7);
        version_one.push(1, 1); // one opaque drc2 extension bit
        let mut reader =
            BitReader::new_bounded(version_one.as_bytes(), 0, version_one.len as u64).unwrap();
        let parsed = parse_drc_data(&mut reader, version_one.as_bytes(), &configuration).unwrap();
        assert_eq!(reader.remaining_bits(), 0);
        let gain_set = parsed.gain_sets().first().copied().unwrap();
        assert_eq!(gain_set.version, 1);
        assert_eq!(gain_set.body().len_bits(), 8);
    }

    #[test]
    fn rejects_trailing_bits_after_complete_curve_drc_data() {
        let configuration = drc_configuration(&[PresentationDrcDecoderMode {
            mode_id: 0,
            output_levels: None,
            profile: PresentationDrcProfile::DefaultEac3,
        }]);
        let mut bits = TestBits::new();
        bits.push(0, 1); // drc_reset_flag
        bits.push(0, 2); // drc_reserved
        bits.push(1, 1); // not part of drc_data()
        let mut reader = BitReader::new_bounded(bits.as_bytes(), 0, bits.len as u64).unwrap();

        assert_eq!(
            parse_drc_data(&mut reader, bits.as_bytes(), &configuration).unwrap_err(),
            PresentationSubstreamError::TrailingDrcFrameBits {
                bit_position: 3,
                remaining_bits: 1,
            }
        );
    }

    #[test]
    fn parses_absent_drc_compression_curve_optional_branches() {
        let mut default = TestBits::new();
        default.push(1, 4); // drc_lev_nullband_low
        default.push(2, 4); // drc_lev_nullband_high
        default.push(0, 4); // no maximum boost branch
        default.push(0, 5); // no maximum cut branch
        default.push(1, 1); // default time constants
        let mut reader = BitReader::new(default.as_bytes());
        assert_eq!(
            parse_drc_compression_curve(&mut reader).unwrap(),
            PresentationDrcCompressionCurve {
                nullband_low: 1,
                nullband_high: 2,
                gain_max_boost: 0,
                level_max_boost: None,
                boost_section: None,
                gain_max_cut: 0,
                level_max_cut: None,
                cut_section: None,
                time_constants: None,
            }
        );
        assert_eq!(reader.bit_position(), default.len as u64);

        let mut nonadaptive = TestBits::new();
        nonadaptive.push(15, 4);
        nonadaptive.push(14, 4);
        nonadaptive.push(1, 4);
        nonadaptive.push(31, 5);
        nonadaptive.push(0, 1); // no extra boost section
        nonadaptive.push(1, 5);
        nonadaptive.push(63, 6);
        nonadaptive.push(0, 1); // no extra cut section
        nonadaptive.push(0, 1); // custom time constants
        nonadaptive.push(0, 8);
        nonadaptive.push(255, 8);
        nonadaptive.push(1, 8);
        nonadaptive.push(254, 8);
        nonadaptive.push(0, 1); // no adaptive thresholds
        let mut reader = BitReader::new(nonadaptive.as_bytes());
        assert_eq!(
            parse_drc_compression_curve(&mut reader).unwrap(),
            PresentationDrcCompressionCurve {
                nullband_low: 15,
                nullband_high: 14,
                gain_max_boost: 1,
                level_max_boost: Some(31),
                boost_section: None,
                gain_max_cut: 1,
                level_max_cut: Some(63),
                cut_section: None,
                time_constants: Some(PresentationDrcTimeConstants {
                    attack: 0,
                    release: 255,
                    attack_fast: 1,
                    release_fast: 254,
                    adaptive_thresholds: None,
                }),
            }
        );
        assert_eq!(reader.bit_position(), nonadaptive.len as u64);
    }

    #[test]
    fn dependent_drc_frame_preserves_all_data_without_parsing_configuration() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        push_minimal_loudness_prefix(&mut bits, 0);
        bits.push(5, 5); // five-bit drc_frame
        bits.push(0, 1); // no size extension
        let frame_offset = bits.len as u64;
        bits.push(1, 1); // b_drc_present
        let data_offset = bits.len as u64;
        bits.push(0b1010, 4); // dependent drc_data starts immediately
        bits.push(0, 1); // no associated audio
        bits.byte_align();

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false))
                .unwrap();

        assert!(parsed.drc_present);
        assert_eq!(parsed.drc_frame.bit_offset(), frame_offset);
        assert_eq!(parsed.drc_configuration, None);
        assert_eq!(parsed.drc_data_elements, None);
        assert_eq!(parsed.drc_data.bit_offset(), data_offset);
        assert_eq!(parsed.drc_data.len_bits(), 4);
        assert_eq!(
            parsed.drc_data.iter().collect::<std::vec::Vec<_>>(),
            [true, false, true, false]
        );
    }

    #[test]
    fn drc_state_parses_dependent_data_with_the_previous_configuration() {
        let mut state = PresentationDrcState::new();
        let mut independent = TestBits::new();
        push_fixed_gain_presentation(
            &mut independent,
            true,
            u8::try_from(MIN_KNOWN_DRC_GAIN_SET_BITS).unwrap_or(u8::MAX),
        );
        let independent = Ac4PresentationSubstream::parse_with_drc_state(
            independent.as_bytes(),
            drc_state_context(true),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.configuration(), independent.drc_configuration);
        assert_eq!(independent.drc_data_elements.unwrap().gain_sets().len(), 1);

        let mut dependent = TestBits::new();
        push_fixed_gain_presentation(
            &mut dependent,
            false,
            u8::try_from(MIN_KNOWN_DRC_GAIN_SET_BITS).unwrap_or(u8::MAX),
        );
        let dependent = Ac4PresentationSubstream::parse_with_drc_state(
            dependent.as_bytes(),
            drc_state_context(false),
            &mut state,
        )
        .unwrap();

        assert_eq!(dependent.drc_configuration, None);
        let data = dependent.drc_data_elements.unwrap();
        assert_eq!(data.gain_sets().len(), 1);
        let gain_set = data.gain_sets().first().copied().unwrap();
        assert_eq!(gain_set.decoder_mode_id, 0);
        assert_eq!(gain_set.gains_configuration, 0);
        assert_eq!(gain_set.version, 0);
        assert_eq!(gain_set.body().len_bits(), 7);
        let configuration = state.configuration();
        assert!(configuration.is_some());

        let mut absent = TestBits::new();
        absent.push(0, 1); // no additional data
        push_minimal_complete_common_metadata(&mut absent, 0);
        absent.byte_align();
        let absent = Ac4PresentationSubstream::parse_with_drc_state(
            absent.as_bytes(),
            drc_state_context(false),
            &mut state,
        )
        .unwrap();
        assert!(!absent.drc_present);
        assert_eq!(state.configuration(), configuration);
    }

    #[test]
    fn drc_state_rejects_missing_history_and_keeps_history_on_failure() {
        let mut dependent = TestBits::new();
        let data_offset = push_fixed_gain_presentation(
            &mut dependent,
            false,
            u8::try_from(MIN_KNOWN_DRC_GAIN_SET_BITS).unwrap_or(u8::MAX),
        );
        let mut empty = PresentationDrcState::new();
        assert_eq!(
            Ac4PresentationSubstream::parse_with_drc_state(
                dependent.as_bytes(),
                drc_state_context(false),
                &mut empty,
            )
            .unwrap_err(),
            PresentationSubstreamError::MissingDrcConfiguration {
                bit_position: data_offset,
            }
        );
        assert_eq!(empty.configuration(), None);

        let mut independent = TestBits::new();
        push_fixed_gain_presentation(
            &mut independent,
            true,
            u8::try_from(MIN_KNOWN_DRC_GAIN_SET_BITS).unwrap_or(u8::MAX),
        );
        Ac4PresentationSubstream::parse_with_drc_state(
            independent.as_bytes(),
            drc_state_context(true),
            &mut empty,
        )
        .unwrap();
        let previous = empty.configuration();

        let mut malformed = TestBits::new();
        let size_value_offset = push_fixed_gain_presentation(
            &mut malformed,
            false,
            u8::try_from(MIN_KNOWN_DRC_GAIN_SET_BITS.saturating_add(1)).unwrap_or(u8::MAX),
        );
        assert_eq!(
            Ac4PresentationSubstream::parse_with_drc_state(
                malformed.as_bytes(),
                drc_state_context(false),
                &mut empty,
            )
            .unwrap_err(),
            PresentationSubstreamError::InvalidFixedDrcGainSetSize {
                declared: MIN_KNOWN_DRC_GAIN_SET_BITS.saturating_add(1),
                expected: MIN_KNOWN_DRC_GAIN_SET_BITS,
                bit_position: size_value_offset,
            }
        );
        assert_eq!(empty.configuration(), previous);
    }

    #[test]
    fn independent_frame_without_drc_clears_the_drc_state() {
        let mut state = PresentationDrcState::new();
        let mut configured = TestBits::new();
        push_fixed_gain_presentation(
            &mut configured,
            true,
            u8::try_from(MIN_KNOWN_DRC_GAIN_SET_BITS).unwrap_or(u8::MAX),
        );
        Ac4PresentationSubstream::parse_with_drc_state(
            configured.as_bytes(),
            drc_state_context(true),
            &mut state,
        )
        .unwrap();
        assert!(state.configuration().is_some());

        let mut absent = TestBits::new();
        absent.push(0, 1); // no additional data
        push_minimal_complete_common_metadata(&mut absent, 0);
        absent.byte_align();
        Ac4PresentationSubstream::parse_with_drc_state(
            absent.as_bytes(),
            drc_state_context(true),
            &mut state,
        )
        .unwrap();
        assert_eq!(state.configuration(), None);

        state.reset();
        assert_eq!(state, PresentationDrcState::new());
    }

    #[test]
    fn compatibility_parser_accepts_only_known_independent_object_drc_tail_bytes() {
        const STRICT: [u8; 4] = [0x00, 0x3d, 0x01, 0x00];
        let context = PresentationSubstreamContext::new(
            false,
            true,
            1,
            1,
            PresentationChannelContext::UNDEFINED,
        );

        let mut strict_state = PresentationDrcState::new();
        let strict =
            Ac4PresentationSubstream::parse_with_drc_state(&STRICT, context, &mut strict_state)
                .unwrap();
        assert!(strict.drc_present);
        assert!(strict.drc_configuration.is_some());

        let mut compat_state = PresentationDrcState::new();
        let (_, syntax_len) = Ac4PresentationSubstream::parse_with_drc_state_compat(
            &STRICT,
            context,
            &mut compat_state,
        )
        .unwrap();
        assert_eq!(syntax_len, STRICT.len());
        assert_eq!(compat_state, strict_state);

        for compatibility_byte in INDEPENDENT_OBJECT_DRC_COMPATIBILITY_BYTES {
            let payload = [
                STRICT[0],
                STRICT[1],
                STRICT[2],
                STRICT[3],
                compatibility_byte,
            ];
            let mut strict_rejected = PresentationDrcState::new();
            assert_eq!(
                Ac4PresentationSubstream::parse_with_drc_state(
                    &payload,
                    context,
                    &mut strict_rejected,
                )
                .unwrap_err(),
                PresentationSubstreamError::TrailingBits { remaining_bits: 8 }
            );
            assert_eq!(strict_rejected, PresentationDrcState::new());

            let mut accepted = PresentationDrcState::new();
            let (parsed, syntax_len) = Ac4PresentationSubstream::parse_with_drc_state_compat(
                &payload,
                context,
                &mut accepted,
            )
            .unwrap();
            assert_eq!(syntax_len, STRICT.len());
            assert!(parsed.drc_configuration.is_some());
            assert_eq!(accepted, strict_state);
        }
    }

    #[test]
    fn compatibility_parser_rejects_other_tails_and_rolls_back_state() {
        const STRICT: [u8; 4] = [0x00, 0x3d, 0x01, 0x00];
        let object_context = PresentationSubstreamContext::new(
            false,
            true,
            1,
            1,
            PresentationChannelContext::UNDEFINED,
        );
        let channel_context = PresentationSubstreamContext::new(
            false,
            true,
            1,
            1,
            PresentationChannelContext::new(Some(0), None, false, 0, false),
        );

        let mut state = PresentationDrcState::new();
        Ac4PresentationSubstream::parse_with_drc_state(&STRICT, object_context, &mut state)
            .unwrap();
        let before = state;
        let wrong_tail = [STRICT[0], STRICT[1], STRICT[2], STRICT[3], 0x81];
        assert_eq!(
            Ac4PresentationSubstream::parse_with_drc_state_compat(
                &wrong_tail,
                object_context,
                &mut state,
            )
            .unwrap_err(),
            PresentationSubstreamError::TrailingBits { remaining_bits: 8 }
        );
        assert_eq!(state, before);

        for compatibility_byte in INDEPENDENT_OBJECT_DRC_COMPATIBILITY_BYTES {
            let payload = [
                STRICT[0],
                STRICT[1],
                STRICT[2],
                STRICT[3],
                compatibility_byte,
            ];
            assert_eq!(
                Ac4PresentationSubstream::parse_with_drc_state_compat(
                    &payload,
                    channel_context,
                    &mut state,
                )
                .unwrap_err(),
                PresentationSubstreamError::TrailingBits { remaining_bits: 8 }
            );
            assert_eq!(state, before);
        }

        let no_drc_with_tail = [0x55, 0x04, 0x00, 0x80];
        assert_eq!(
            Ac4PresentationSubstream::parse_with_drc_state_compat(
                &no_drc_with_tail,
                object_context,
                &mut state,
            )
            .unwrap_err(),
            PresentationSubstreamError::TrailingBits { remaining_bits: 8 }
        );
        assert_eq!(state, before);
    }

    #[test]
    fn independent_drc_configuration_cannot_borrow_following_metadata() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        push_minimal_loudness_prefix(&mut bits, 0);
        bits.push(4, 5); // only presence and decoder-mode count fit
        bits.push(0, 1); // no size extension
        bits.push(1, 1); // b_drc_present
        bits.push(0, 3); // one decoder mode; its body is missing
        let envelope_end = bits.len as u64;
        bits.push_byte(0xff); // readable bytes after the declared envelope must not be borrowed

        let context = PresentationSubstreamContext::new(
            false,
            true,
            1,
            1,
            PresentationChannelContext::new(Some(0), None, false, 0, false),
        );
        assert_eq!(
            Ac4PresentationSubstream::parse(bits.as_bytes(), context).unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 3,
                bit_position: envelope_end,
                remaining_bits: 0,
            })
        );
    }

    #[test]
    fn distinguishes_absent_and_kept_substream_group_gains() {
        let mut prefix = TestBits::new();
        prefix.push(0, 1); // no additional data
        push_minimal_common_metadata_prefix(&mut prefix, 0);

        let mut absent = prefix.clone();
        absent.push(0, 1); // b_substream_group_gains_present
        absent.push(0, 1); // no associated audio
        absent.push(0b11_1111, 6); // byte_align
        let absent =
            Ac4PresentationSubstream::parse(absent.as_bytes(), test_context(false, 2, 2, false))
                .unwrap();
        assert_eq!(
            absent.substream_group_gain_update,
            PresentationSubstreamGroupGainUpdate::NotPresent
        );
        assert_eq!(absent.b_associated_offset, 17);
        assert_eq!(absent.associated_audio, None);
        assert_eq!(absent.custom_downmix_offset, 18);

        let mut kept = prefix;
        kept.push(1, 1); // b_substream_group_gains_present
        kept.push(1, 1); // b_keep
        kept.push(0, 1); // no associated audio
        kept.push(0b1_1111, 5); // byte_align
        let kept =
            Ac4PresentationSubstream::parse(kept.as_bytes(), test_context(false, 2, 2, false))
                .unwrap();
        assert_eq!(
            kept.substream_group_gain_update,
            PresentationSubstreamGroupGainUpdate::KeepPrevious
        );
        assert_eq!(kept.b_associated_offset, 18);
        assert_eq!(kept.associated_audio, None);
        assert_eq!(kept.custom_downmix_offset, 19);
    }

    #[test]
    fn parses_substream_group_gain_codes_at_capacity() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        push_minimal_common_metadata_prefix(&mut bits, 0);
        bits.push(1, 1); // b_substream_group_gains_present
        bits.push(0, 1); // new values, not b_keep
        for code in [0, 1, 2, 3, 60, 61, 62, 63] {
            bits.push(code, 6);
        }
        let expected_associated_offset = bits.len as u64;
        bits.push(0, 1); // no associated audio
        let expected_custom_downmix_offset = bits.len as u64;
        bits.push(0b10, 2); // byte_align

        let parsed =
            Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 8, 8, false))
                .unwrap();
        let PresentationSubstreamGroupGainUpdate::NewValues(codes) =
            parsed.substream_group_gain_update
        else {
            panic!("应保留本帧传输的 group-gain 码值");
        };
        assert_eq!(codes.as_slice(), &[0, 1, 2, 3, 60, 61, 62, 63]);
        assert_eq!(codes.len(), 8);
        assert!(!codes.is_empty());
        assert_eq!(codes.get(0), Some(0));
        assert_eq!(codes.get(7), Some(63));
        assert_eq!(codes.get(8), None);
        assert_eq!(parsed.b_associated_offset, expected_associated_offset);
        assert_eq!(parsed.associated_audio, None);
        assert_eq!(parsed.custom_downmix_offset, expected_custom_downmix_offset);
    }

    #[test]
    fn group_gain_state_defaults_to_zero_then_keeps_new_values() {
        let context = group_gain_context(false, 2);
        let mut state = PresentationSubstreamGroupGainState::new();
        assert_eq!(state.effective_codes(), None);

        let initial = state
            .apply(PresentationSubstreamGroupGainUpdate::KeepPrevious, context)
            .unwrap();
        assert_eq!(initial.as_slice(), &[0, 0]);

        let transmitted = group_gain_codes(&[1, 63]);
        let updated = state
            .apply(
                PresentationSubstreamGroupGainUpdate::NewValues(transmitted),
                context,
            )
            .unwrap();
        assert_eq!(updated.as_slice(), &[1, 63]);

        let kept = state
            .apply(PresentationSubstreamGroupGainUpdate::KeepPrevious, context)
            .unwrap();
        assert_eq!(kept, transmitted);
        assert_eq!(state.effective_codes(), Some(transmitted));
    }

    #[test]
    fn group_gain_state_resets_absent_single_group_and_independent_frames_to_zero() {
        let two_groups = group_gain_context(false, 2);
        let mut state = PresentationSubstreamGroupGainState::new();
        state
            .apply(
                PresentationSubstreamGroupGainUpdate::NewValues(group_gain_codes(&[5, 62])),
                two_groups,
            )
            .unwrap();

        let absent = state
            .apply(PresentationSubstreamGroupGainUpdate::NotPresent, two_groups)
            .unwrap();
        assert_eq!(absent.as_slice(), &[0, 0]);
        let kept_after_absent = state
            .apply(
                PresentationSubstreamGroupGainUpdate::KeepPrevious,
                two_groups,
            )
            .unwrap();
        assert_eq!(kept_after_absent.as_slice(), &[0, 0]);

        state
            .apply(
                PresentationSubstreamGroupGainUpdate::NewValues(group_gain_codes(&[5, 62])),
                two_groups,
            )
            .unwrap();
        let independent = state
            .apply(
                PresentationSubstreamGroupGainUpdate::KeepPrevious,
                group_gain_context(true, 2),
            )
            .unwrap();
        assert_eq!(independent.as_slice(), &[0, 0]);

        let mut single_group = PresentationSubstreamGroupGainState::new();
        let not_signaled = single_group
            .apply(
                PresentationSubstreamGroupGainUpdate::NotSignaled,
                group_gain_context(false, 1),
            )
            .unwrap();
        assert_eq!(not_signaled.as_slice(), &[0]);

        single_group.reset();
        assert_eq!(single_group.effective_codes(), None);
    }

    #[test]
    fn group_gain_state_rejects_dependent_topology_changes_transactionally() {
        let mut state = PresentationSubstreamGroupGainState::new();
        let transmitted = group_gain_codes(&[1, 63]);
        state
            .apply(
                PresentationSubstreamGroupGainUpdate::NewValues(transmitted),
                group_gain_context(false, 2),
            )
            .unwrap();
        let previous = state;

        assert_eq!(
            state
                .apply(
                    PresentationSubstreamGroupGainUpdate::NotSignaled,
                    group_gain_context(false, 1),
                )
                .unwrap_err(),
            PresentationSubstreamGroupGainStateError::SubstreamGroupCountChanged {
                previous: 2,
                current: 1,
            }
        );
        assert_eq!(state, previous);

        assert_eq!(
            state
                .apply(
                    PresentationSubstreamGroupGainUpdate::NotSignaled,
                    group_gain_context(false, 2),
                )
                .unwrap_err(),
            PresentationSubstreamGroupGainStateError::InconsistentUpdate {
                declared: 2,
                update: PresentationSubstreamGroupGainUpdate::NotSignaled,
            }
        );
        assert_eq!(state, previous);

        assert_eq!(
            state
                .apply(
                    PresentationSubstreamGroupGainUpdate::NewValues(transmitted),
                    group_gain_context(false, 3),
                )
                .unwrap_err(),
            PresentationSubstreamGroupGainStateError::InconsistentUpdate {
                declared: 3,
                update: PresentationSubstreamGroupGainUpdate::NewValues(transmitted),
            }
        );
        assert_eq!(state, previous);

        assert_eq!(
            state
                .apply(
                    PresentationSubstreamGroupGainUpdate::KeepPrevious,
                    group_gain_context(false, 9),
                )
                .unwrap_err(),
            PresentationSubstreamGroupGainStateError::CapacityExceeded {
                declared: 9,
                limit: MAX_GROUPS_PER_PRESENTATION,
            }
        );
        assert_eq!(state, previous);
    }

    #[test]
    fn independent_group_gain_state_accepts_the_eight_group_boundary() {
        let mut state = PresentationSubstreamGroupGainState::new();
        state
            .apply(
                PresentationSubstreamGroupGainUpdate::NewValues(group_gain_codes(&[1, 63])),
                group_gain_context(false, 2),
            )
            .unwrap();

        let boundary = group_gain_codes(&[0, 1, 2, 3, 60, 61, 62, 63]);
        let effective = state
            .apply(
                PresentationSubstreamGroupGainUpdate::NewValues(boundary),
                group_gain_context(true, 8),
            )
            .unwrap();
        assert_eq!(effective, boundary);
        assert_eq!(state.effective_codes(), Some(boundary));
    }

    #[test]
    fn parses_independent_associated_scale_presence_and_omits_non_mono_pan() {
        let scale_codes = [0x00u8, 0x7f, 0xff];
        for presence_mask in 0u8..8 {
            let mut bits = TestBits::new();
            bits.push(0, 1); // no additional data
            push_minimal_common_metadata_prefix(&mut bits, 0);
            let expected_associated_offset = bits.len as u64;
            bits.push(1, 1); // b_associated
            for (presence_bit, scale_code) in [1u8, 2, 4].into_iter().zip(scale_codes) {
                let present = presence_mask & presence_bit != 0;
                bits.push(if present { 1 } else { 0 }, 1);
                if present {
                    bits.push(u64::from(scale_code), 8);
                }
            }
            bits.push(0, 1); // associated audio is not mono
            let expected_custom_downmix_offset = bits.len as u64;

            let parsed =
                Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false))
                    .unwrap();

            assert_eq!(parsed.b_associated_offset, expected_associated_offset);
            assert_eq!(
                parsed.associated_audio,
                Some(PresentationAssociatedAudio {
                    scale_main: (presence_mask & 1 != 0).then_some(0x00),
                    scale_main_centre: (presence_mask & 2 != 0).then_some(0x7f),
                    scale_main_front: (presence_mask & 4 != 0).then_some(0xff),
                    associate_is_mono: false,
                    pan_associated: None,
                })
            );
            assert_eq!(parsed.custom_downmix_offset, expected_custom_downmix_offset);
        }
    }

    #[test]
    fn parses_associated_mono_pan_endpoints() {
        for pan_associated in [0x00u8, 0xef] {
            let mut bits = TestBits::new();
            bits.push(0, 1); // no additional data
            push_minimal_common_metadata_prefix(&mut bits, 0);
            bits.push(1, 1); // b_associated
            bits.push(0, 1); // no scale_main
            bits.push(0, 1); // no scale_main_centre
            bits.push(0, 1); // no scale_main_front
            bits.push(1, 1); // associated audio is mono
            bits.push(u64::from(pan_associated), 8);
            let expected_custom_downmix_offset = bits.len as u64;
            bits.push(0b101, 3); // byte_align

            let parsed =
                Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false))
                    .unwrap();

            assert_eq!(
                parsed.associated_audio,
                Some(PresentationAssociatedAudio {
                    scale_main: None,
                    scale_main_centre: None,
                    scale_main_front: None,
                    associate_is_mono: true,
                    pan_associated: Some(pan_associated),
                })
            );
            assert_eq!(parsed.custom_downmix_offset, expected_custom_downmix_offset);
        }
    }

    #[test]
    fn rejects_all_reserved_associated_pan_codes() {
        for pan_associated in 0xf0u8..=0xff {
            let mut bits = TestBits::new();
            bits.push(0, 1); // no additional data
            push_minimal_common_metadata_prefix(&mut bits, 0);
            bits.push(1, 1); // b_associated
            bits.push(0, 1); // no scale_main
            bits.push(0, 1); // no scale_main_centre
            bits.push(0, 1); // no scale_main_front
            bits.push(1, 1); // associated audio is mono
            let expected_pan_offset = bits.len as u64;
            bits.push(u64::from(pan_associated), 8);

            assert_eq!(
                Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false),)
                    .unwrap_err(),
                PresentationSubstreamError::ReservedAssociatedPan {
                    pan_associated,
                    bit_position: expected_pan_offset,
                }
            );
        }
    }

    #[test]
    fn derives_all_custom_downmix_bitstream_channel_configurations() {
        for (context, expected) in [
            (
                PresentationChannelContext::new(Some(13), None, true, 2, false),
                Some(0),
            ),
            (
                PresentationChannelContext::new(Some(12), None, true, 2, false),
                Some(1),
            ),
            (
                PresentationChannelContext::new(Some(11), None, false, 2, false),
                Some(2),
            ),
            (
                PresentationChannelContext::new(Some(14), None, true, 1, false),
                Some(3),
            ),
            (
                PresentationChannelContext::new(Some(12), None, true, 1, false),
                Some(4),
            ),
            (
                PresentationChannelContext::new(Some(11), None, false, 1, false),
                Some(5),
            ),
        ] {
            assert_eq!(derive_bitstream_channel_config(context), expected);
        }

        for context in [
            PresentationChannelContext::UNDEFINED,
            PresentationChannelContext::new(Some(10), None, true, 2, false),
            PresentationChannelContext::new(Some(13), None, false, 2, false),
            PresentationChannelContext::new(Some(14), None, true, 0, false),
            PresentationChannelContext::new(Some(11), None, false, 0, false),
            PresentationChannelContext::new(Some(11), None, false, 3, false),
        ] {
            assert_eq!(derive_bitstream_channel_config(context), None);
        }
    }

    #[test]
    fn custom_downmix_parameter_tools_consume_the_normative_branches() {
        let cases = [
            (0, 0, 15),
            (0, 1, 10),
            (0, 2, 7),
            (0, 3, 14),
            (0, 4, 7),
            (1, 0, 11),
            (1, 1, 6),
            (1, 2, 3),
            (1, 3, 10),
            (1, 4, 3),
            (2, 0, 8),
            (2, 1, 3),
            (3, 0, 11),
            (3, 1, 7),
            (3, 2, 7),
            (3, 3, 9),
            (3, 4, 4),
            (4, 0, 7),
            (4, 1, 3),
            (4, 2, 3),
            (4, 3, 5),
            (4, 4, 0),
            (5, 0, 4),
            (5, 1, 0),
        ];
        let source = [0u8; 2];
        for (bitstream_channel_config, output_channel_config, expected_bits) in cases {
            let mut reader = BitReader::new(&source);
            parse_custom_downmix_parameters(
                &mut reader,
                bitstream_channel_config,
                output_channel_config,
            )
            .unwrap();
            assert_eq!(
                reader.bit_position(),
                expected_bits,
                "bs_ch_config={bitstream_channel_config}, out_ch_config={output_channel_config}"
            );
        }
    }

    #[test]
    fn parses_one_and_four_custom_downmix_configurations_with_exact_output_widths() {
        let mut four = TestBits::new();
        four.push(1, 1); // b_cdmx_data_present
        four.push(3, 2); // four configurations
        for output_channel_config in [0u8, 1, 0, 1] {
            four.push(u64::from(output_channel_config), 1); // bs_ch_config 2 uses one bit
            if output_channel_config == 0 {
                four.push(1, 1); // top-front to front
                four.push(0, 3);
                four.push(0, 1); // top-back to side
                four.push(7, 3);
            } else {
                four.push(7, 3); // gain_t1_code
            }
        }
        four.push(0, 1); // no stereo coefficients
        let mut reader = BitReader::new(four.as_bytes());
        let parsed = parse_custom_downmix_data(
            &mut reader,
            PresentationChannelContext::new(Some(11), None, false, 2, false),
        )
        .unwrap();
        assert_eq!(parsed.bitstream_channel_config(), Some(2));
        assert_eq!(parsed.custom_data_present(), Some(true));
        assert_eq!(parsed.configurations().len(), 4);
        assert_eq!(
            parsed
                .configurations()
                .iter()
                .map(|configuration| configuration.output_channel_config)
                .collect::<std::vec::Vec<_>>(),
            [0, 1, 0, 1]
        );
        assert_eq!(parsed.stereo_coefficients_present(), Some(false));
        assert_eq!(reader.bit_position(), 30);

        let mut one = TestBits::new();
        one.push(1, 1); // b_cdmx_data_present
        one.push(0, 2); // one configuration
        one.push(4, 3); // bs_ch_config 1 uses three bits
        one.push(7, 3); // gain_t1_code
        one.push(0, 1); // no stereo coefficients
        let mut reader = BitReader::new(one.as_bytes());
        let parsed = parse_custom_downmix_data(
            &mut reader,
            PresentationChannelContext::new(Some(12), None, true, 2, false),
        )
        .unwrap();
        assert_eq!(parsed.bitstream_channel_config(), Some(1));
        assert_eq!(parsed.configurations().len(), 1);
        assert_eq!(
            parsed
                .configurations()
                .first()
                .map(|configuration| configuration.output_channel_config),
            Some(4)
        );
        assert_eq!(reader.bit_position(), 10);
    }

    #[test]
    fn parses_complete_nine_x_four_custom_downmix_and_minimal_loud_corr() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        push_minimal_complete_common_metadata(&mut bits, 0);
        let expected_custom_downmix_offset = bits.len as u64;
        bits.push(1, 1); // b_cdmx_data_present
        bits.push(0, 2); // one configuration
        bits.push(0, 3); // out_ch_config = 5.X.0
        bits.push(1, 1); // put screen channels into centre
        bits.push(7, 3); // gain_f1_code; mute is legal
        bits.push(1, 1); // top-front pair to front
        bits.push(6, 3); // gain_t2a_code
        bits.push(0, 1); // top-back pair to side
        bits.push(5, 3); // gain_t2e_code
        bits.push(4, 3); // gain_b_code
        bits.push(1, 1); // b_stereo_dmx_coeff
        bits.push(7, 3); // loro_centre_mixgain; mute is legal
        bits.push(2, 3); // loro_surround_mixgain
        bits.push(1, 1); // b_ltrt_mixinfo
        bits.push(0, 3); // ltrt_centre_mixgain
        bits.push(6, 3); // ltrt_surround_mixgain
        bits.push(1, 1); // b_lfe_mixinfo
        bits.push(31, 5); // lfe_mixgain
        bits.push(3, 2); // preferred_dmx_method
        let expected_loudness_correction_offset = bits.len as u64;
        bits.push(0, 1); // no immersive-output corrections
        bits.push(0, 1); // no LoRo correction
        bits.push(0, 1); // no LtRt correction
        bits.push(0, 1); // no 5.X correction
        bits.push(0, 1); // no core 5.X.2 correction
        bits.push(0, 1); // no core 5.X correction
        bits.push(0, 1); // no core LoRo/LtRt correction
        let expected_alignment_offset = bits.len as u64;

        let mut right_bits = bits.clone();
        bits.push(0, 5); // byte_align
        right_bits.push(0b1_1111, 5); // different byte_align values
        let context = PresentationSubstreamContext::new(
            false,
            false,
            1,
            1,
            PresentationChannelContext::new(Some(14), Some(6), true, 2, true),
        );
        let parsed = Ac4PresentationSubstream::parse(bits.as_bytes(), context).unwrap();
        let right = Ac4PresentationSubstream::parse(right_bits.as_bytes(), context).unwrap();

        assert_eq!(parsed, right);
        assert_eq!(parsed.custom_downmix_offset, expected_custom_downmix_offset);
        assert_eq!(
            parsed.loudness_correction_offset,
            expected_loudness_correction_offset
        );
        assert_eq!(parsed.byte_alignment_offset, expected_alignment_offset);
        assert_eq!(parsed.alignment_bits, 5);
        let custom = parsed.custom_downmix;
        assert_eq!(custom.bitstream_channel_config(), Some(0));
        assert_eq!(custom.custom_data_present(), Some(true));
        assert_eq!(custom.stereo_coefficients_present(), Some(true));
        assert_eq!(
            custom.configurations(),
            &[PresentationCustomDownmixConfiguration {
                output_channel_config: 0,
                parameters: PresentationCustomDownmixParameters {
                    screen: Some(PresentationScreenDownmix::ToCentre { gain_f1_code: 7 }),
                    top: Some(PresentationTopDownmix::FourToFrontSide {
                        front: PresentationTopPairDownmix {
                            destination: PresentationTopPairDestination::Front,
                            gain_code: 6,
                        },
                        back: PresentationTopPairDownmix {
                            destination: PresentationTopPairDestination::Side,
                            gain_code: 5,
                        },
                    }),
                    back_four_to_two_gain_code: Some(4),
                },
            }]
        );
        assert_eq!(
            custom.stereo_coefficients(),
            Some(PresentationStereoDownmixCoefficients {
                loro_centre_mixgain: 7,
                loro_surround_mixgain: 2,
                ltrt_centre_mixgain: Some(0),
                ltrt_surround_mixgain: Some(6),
                lfe_mixinfo_present: Some(true),
                lfe_mixgain: Some(31),
                preferred_downmix_method: 3,
            })
        );
    }

    #[test]
    fn parses_complete_channel_and_core_loudness_corrections() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // b_corr_for_immersive_out
        push_loudness_correction_code(&mut bits, Some(0)); // LoRo
        push_loudness_correction_code(&mut bits, None); // LtRt
        push_loudness_correction_code(&mut bits, Some(31)); // 5.X; reserved means 0 dB
        push_loudness_correction_code(&mut bits, Some(1)); // 5.X.2
        push_loudness_correction_code(&mut bits, Some(2)); // 7.X
        push_loudness_correction_code(&mut bits, Some(3)); // 7.X.4
        push_loudness_correction_code(&mut bits, None); // 7.X.2
        push_loudness_correction_code(&mut bits, Some(4)); // 5.X.4
        push_loudness_correction_code(&mut bits, Some(5)); // core 5.X.2
        push_loudness_correction_code(&mut bits, Some(6)); // core 5.X
        bits.push(1, 1); // shared core LoRo/LtRt presence
        bits.push(31, 5); // core LoRo; reserved means 0 dB
        bits.push(7, 5); // core LtRt

        let mut reader = BitReader::new(bits.as_bytes());
        let parsed = parse_loudness_correction(
            &mut reader,
            PresentationChannelContext::new(Some(14), Some(6), true, 2, true),
        )
        .unwrap();

        assert_eq!(reader.bit_position(), bits.len as u64);
        assert_eq!(
            parsed,
            PresentationLoudnessCorrectionData {
                object_loudness_correction: None,
                corrections_for_immersive_output: Some(true),
                loro_downmix: Some(PresentationLoudnessCorrectionCode::Value(0)),
                ltrt_downmix: Some(PresentationLoudnessCorrectionCode::NotPresent),
                five_x: Some(PresentationLoudnessCorrectionCode::Value(31)),
                five_x_two: Some(PresentationLoudnessCorrectionCode::Value(1)),
                seven_x: Some(PresentationLoudnessCorrectionCode::Value(2)),
                seven_x_four: Some(PresentationLoudnessCorrectionCode::Value(3)),
                seven_x_two: Some(PresentationLoudnessCorrectionCode::NotPresent),
                five_x_four: Some(PresentationLoudnessCorrectionCode::Value(4)),
                core_five_x_two: Some(PresentationLoudnessCorrectionCode::Value(5)),
                core_five_x: Some(PresentationLoudnessCorrectionCode::Value(6)),
                core_stereo: Some(PresentationCoreStereoLoudnessCorrection::Values {
                    loro: 31,
                    ltrt: 7,
                }),
                nine_x_four: None,
            }
        );
    }

    #[test]
    fn parses_complete_object_loudness_corrections_and_nine_x_four() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // b_obj_loud_corr
        bits.push(1, 1); // b_corr_for_immersive_out
        for value in 8u8..=15 {
            push_loudness_correction_code(&mut bits, Some(value));
        }
        push_loudness_correction_code(&mut bits, Some(31)); // 9.X.4

        let mut reader = BitReader::new(bits.as_bytes());
        let parsed =
            parse_loudness_correction(&mut reader, PresentationChannelContext::UNDEFINED).unwrap();

        assert_eq!(reader.bit_position(), bits.len as u64);
        assert_eq!(
            parsed,
            PresentationLoudnessCorrectionData {
                object_loudness_correction: Some(true),
                corrections_for_immersive_output: Some(true),
                loro_downmix: Some(PresentationLoudnessCorrectionCode::Value(8)),
                ltrt_downmix: Some(PresentationLoudnessCorrectionCode::Value(9)),
                five_x: Some(PresentationLoudnessCorrectionCode::Value(10)),
                five_x_two: Some(PresentationLoudnessCorrectionCode::Value(11)),
                seven_x: Some(PresentationLoudnessCorrectionCode::Value(12)),
                seven_x_four: Some(PresentationLoudnessCorrectionCode::Value(13)),
                seven_x_two: Some(PresentationLoudnessCorrectionCode::Value(14)),
                five_x_four: Some(PresentationLoudnessCorrectionCode::Value(15)),
                core_five_x_two: None,
                core_five_x: None,
                core_stereo: None,
                nine_x_four: Some(PresentationLoudnessCorrectionCode::Value(31)),
            }
        );
    }

    #[test]
    fn loudness_correction_gates_consume_only_applicable_fields() {
        let zeroes = [0u8; 2];
        for (presentation_mode, core_mode, expected_bits) in [
            (Some(1), None, 0),
            (Some(2), None, 2),
            (Some(4), None, 2),
            (Some(5), None, 4),
            (Some(10), None, 4),
            (Some(11), None, 4),
            (Some(0), Some(2), 0),
            (Some(0), Some(3), 2),
            (Some(0), Some(4), 2),
            (Some(0), Some(5), 3),
            (Some(0), Some(6), 3),
        ] {
            let mut reader = BitReader::new(&zeroes);
            parse_loudness_correction(
                &mut reader,
                PresentationChannelContext::new(presentation_mode, core_mode, false, 0, false),
            )
            .unwrap();
            assert_eq!(
                reader.bit_position(),
                expected_bits,
                "pres_ch_mode={presentation_mode:?}, pres_ch_mode_core={core_mode:?}"
            );
        }

        let immersive = [0x80u8, 0];
        for (presentation_mode, expected_bits) in [(5, 6), (10, 6), (11, 9), (14, 9)] {
            let mut reader = BitReader::new(&immersive);
            parse_loudness_correction(
                &mut reader,
                PresentationChannelContext::new(Some(presentation_mode), None, false, 0, false),
            )
            .unwrap();
            assert_eq!(
                reader.bit_position(),
                expected_bits,
                "pres_ch_mode={presentation_mode}, immersive corrections enabled"
            );
        }

        let mut reader = BitReader::new(&[0]);
        let object_disabled =
            parse_loudness_correction(&mut reader, PresentationChannelContext::UNDEFINED).unwrap();
        assert_eq!(reader.bit_position(), 1);
        assert_eq!(
            object_disabled,
            PresentationLoudnessCorrectionData {
                object_loudness_correction: Some(false),
                ..PresentationLoudnessCorrectionData::default()
            }
        );

        let mut reader = BitReader::new(&[0x80]);
        let object_enabled =
            parse_loudness_correction(&mut reader, PresentationChannelContext::UNDEFINED).unwrap();
        assert_eq!(reader.bit_position(), 6);
        assert_eq!(object_enabled.object_loudness_correction, Some(true));
        assert_eq!(object_enabled.corrections_for_immersive_output, Some(false));
        assert_eq!(
            object_enabled.nine_x_four,
            Some(PresentationLoudnessCorrectionCode::NotPresent)
        );
    }

    #[test]
    fn rejects_truncated_loudness_correction_values_and_core_stereo_pair() {
        let mut single = BitReader::new(&[0xff]);
        single.skip_bits(3).unwrap();
        assert_eq!(
            parse_loudness_correction_code(&mut single).unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 5,
                bit_position: 4,
                remaining_bits: 4,
            })
        );

        let mut core_stereo = BitReader::new(&[0xff, 0xff]);
        core_stereo.skip_bits(6).unwrap();
        assert_eq!(
            parse_core_stereo_loudness_correction(&mut core_stereo).unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 5,
                bit_position: 12,
                remaining_bits: 4,
            })
        );
    }

    #[test]
    fn accepts_nonzero_byte_alignment_and_rejects_trailing_bytes() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no additional data
        push_minimal_complete_common_metadata(&mut bits, 0);
        bits.push(0, 1); // b_obj_loud_corr
        let expected_alignment_offset = bits.len as u64;
        bits.push(0b10_1011, 6); // nonzero byte_align bits are opaque

        let context = PresentationSubstreamContext::new(
            false,
            false,
            1,
            1,
            PresentationChannelContext::UNDEFINED,
        );
        let parsed = Ac4PresentationSubstream::parse(bits.as_bytes(), context).unwrap();
        assert_eq!(parsed.byte_alignment_offset, expected_alignment_offset);
        assert_eq!(parsed.alignment_bits, 6);

        bits.push_byte(0xa5);
        assert_eq!(
            Ac4PresentationSubstream::parse(bits.as_bytes(), context).unwrap_err(),
            PresentationSubstreamError::TrailingBits { remaining_bits: 8 }
        );
    }

    #[test]
    fn custom_downmix_preserves_absent_and_not_signaled_gates() {
        let mut absent = TestBits::new();
        absent.push(0, 1); // no custom configurations
        absent.push(0, 1); // no stereo coefficients
        let mut reader = BitReader::new(absent.as_bytes());
        let parsed = parse_custom_downmix_data(
            &mut reader,
            PresentationChannelContext::new(Some(14), Some(6), true, 2, true),
        )
        .unwrap();
        assert_eq!(parsed.bitstream_channel_config(), Some(0));
        assert_eq!(parsed.custom_data_present(), Some(false));
        assert!(parsed.configurations().is_empty());
        assert_eq!(parsed.stereo_coefficients_present(), Some(false));
        assert_eq!(parsed.stereo_coefficients(), None);
        assert_eq!(reader.bit_position(), 2);

        for core_channel_mode in [3, 4] {
            let mut static_ajoc = TestBits::new();
            static_ajoc.push(0, 1); // core mode still carries the stereo gate
            let mut reader = BitReader::new(static_ajoc.as_bytes());
            let parsed = parse_custom_downmix_data(
                &mut reader,
                PresentationChannelContext::new(None, Some(core_channel_mode), false, 0, false),
            )
            .unwrap();
            assert_eq!(parsed.bitstream_channel_config(), None);
            assert_eq!(parsed.custom_data_present(), None);
            assert_eq!(parsed.stereo_coefficients_present(), Some(false));
            assert_eq!(reader.bit_position(), 1);
        }

        // direct-object 与 adaptive A-JOC 都令完整/core mode 未定义，不应消费 custom syntax。
        let source = [0xff];
        let mut reader = BitReader::new(&source);
        let parsed =
            parse_custom_downmix_data(&mut reader, PresentationChannelContext::UNDEFINED).unwrap();
        assert_eq!(parsed.bitstream_channel_config(), None);
        assert_eq!(parsed.custom_data_present(), None);
        assert_eq!(parsed.stereo_coefficients_present(), None);
        assert_eq!(reader.bit_position(), 0);
    }

    #[test]
    fn alternative_object_context_reaches_loud_corr_without_custom_bits() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no alternative presentation name
        bits.push(0, 2); // one target
        push_minimal_target(&mut bits, 1);
        bits.push(0, 1); // no additional data
        push_minimal_complete_common_metadata(&mut bits, 0);
        let expected_offset = bits.len as u64;
        bits.push(0, 1); // b_obj_loud_corr

        let parsed = Ac4PresentationSubstream::parse(
            bits.as_bytes(),
            PresentationSubstreamContext::new(
                true,
                false,
                1,
                1,
                PresentationChannelContext::UNDEFINED,
            ),
        )
        .unwrap();
        assert!(parsed.selection.alternative.is_some());
        assert_eq!(parsed.custom_downmix_offset, expected_offset);
        assert_eq!(parsed.loudness_correction_offset, expected_offset);
        assert_eq!(parsed.byte_alignment_offset, expected_offset + 1);
        assert_eq!(parsed.alignment_bits, 0);
        assert_eq!(parsed.custom_downmix.custom_data_present(), None);
        assert_eq!(parsed.custom_downmix.stereo_coefficients_present(), None);
    }

    #[test]
    fn stereo_custom_downmix_preserves_nested_presence_gates() {
        let mut no_lfe = TestBits::new();
        no_lfe.push(1, 1); // b_stereo_dmx_coeff
        no_lfe.push(0, 3); // loro centre; every code is valid
        no_lfe.push(2, 3); // loro surround
        no_lfe.push(0, 1); // no LtRt mix info
        no_lfe.push(3, 2); // preferred method
        let mut reader = BitReader::new(no_lfe.as_bytes());
        let parsed = parse_custom_downmix_data(
            &mut reader,
            PresentationChannelContext::new(Some(3), None, false, 0, false),
        )
        .unwrap();
        assert_eq!(
            parsed.stereo_coefficients(),
            Some(PresentationStereoDownmixCoefficients {
                loro_centre_mixgain: 0,
                loro_surround_mixgain: 2,
                ltrt_centre_mixgain: None,
                ltrt_surround_mixgain: None,
                lfe_mixinfo_present: None,
                lfe_mixgain: None,
                preferred_downmix_method: 3,
            })
        );
        assert_eq!(reader.bit_position(), 10);

        let mut lfe_absent = TestBits::new();
        lfe_absent.push(1, 1); // b_stereo_dmx_coeff
        lfe_absent.push(7, 3);
        lfe_absent.push(7, 3);
        lfe_absent.push(0, 1); // no LtRt mix info
        lfe_absent.push(0, 1); // b_lfe_mixinfo = 0
        lfe_absent.push(0, 2); // preferred method
        let mut reader = BitReader::new(lfe_absent.as_bytes());
        let parsed = parse_custom_downmix_data(
            &mut reader,
            PresentationChannelContext::new(Some(3), None, false, 0, true),
        )
        .unwrap();
        let coefficients = parsed.stereo_coefficients().unwrap();
        assert_eq!(coefficients.lfe_mixinfo_present, Some(false));
        assert_eq!(coefficients.lfe_mixgain, None);
        assert_eq!(reader.bit_position(), 11);
    }

    #[test]
    fn rejects_unused_custom_outputs_and_reserved_stereo_surround_codes() {
        for output_channel_config in 5u8..=7 {
            let mut bits = TestBits::new();
            bits.push(1, 1); // b_cdmx_data_present
            bits.push(0, 2); // one configuration
            bits.push(u64::from(output_channel_config), 3);
            let mut reader = BitReader::new(bits.as_bytes());
            assert_eq!(
                parse_custom_downmix_data(
                    &mut reader,
                    PresentationChannelContext::new(Some(13), None, true, 2, false),
                )
                .unwrap_err(),
                PresentationSubstreamError::UnusedCustomDownmixOutputChannelConfig {
                    output_channel_config,
                    bit_position: 3,
                }
            );
        }

        for gain_code in 0u8..=1 {
            let mut loro = TestBits::new();
            loro.push(1, 1); // b_stereo_dmx_coeff
            loro.push(0, 3); // centre
            loro.push(u64::from(gain_code), 3); // reserved surround
            let mut reader = BitReader::new(loro.as_bytes());
            assert_eq!(
                parse_custom_downmix_data(
                    &mut reader,
                    PresentationChannelContext::new(Some(3), None, false, 0, false),
                )
                .unwrap_err(),
                PresentationSubstreamError::ReservedStereoSurroundMixGain {
                    kind: PresentationStereoDownmixKind::LoRo,
                    gain_code,
                    bit_position: 4,
                }
            );

            let mut ltrt = TestBits::new();
            ltrt.push(1, 1); // b_stereo_dmx_coeff
            ltrt.push(0, 3); // LoRo centre
            ltrt.push(2, 3); // LoRo surround
            ltrt.push(1, 1); // b_ltrt_mixinfo
            ltrt.push(0, 3); // LtRt centre
            ltrt.push(u64::from(gain_code), 3); // reserved LtRt surround
            let mut reader = BitReader::new(ltrt.as_bytes());
            assert_eq!(
                parse_custom_downmix_data(
                    &mut reader,
                    PresentationChannelContext::new(Some(3), None, false, 0, false),
                )
                .unwrap_err(),
                PresentationSubstreamError::ReservedStereoSurroundMixGain {
                    kind: PresentationStereoDownmixKind::LtRt,
                    gain_code,
                    bit_position: 11,
                }
            );
        }
    }

    #[test]
    fn rejects_truncated_custom_downmix_tools_and_optional_coefficients() {
        let mut screen_gain = TestBits::new();
        screen_gain.push(1, 1); // b_cdmx_data_present
        screen_gain.push(0, 2); // one configuration
        screen_gain.push(0, 3); // out_ch_config
        screen_gain.push(1, 1); // b_put_screen_to_c
        screen_gain.push(0, 1); // only one of three gain bits remains
        let mut reader = BitReader::new(screen_gain.as_bytes());
        assert_eq!(
            parse_custom_downmix_data(
                &mut reader,
                PresentationChannelContext::new(Some(13), None, true, 2, false),
            )
            .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 3,
                bit_position: 7,
                remaining_bits: 1,
            })
        );

        let mut ltrt = TestBits::new();
        ltrt.push(1, 1); // b_stereo_dmx_coeff
        ltrt.push(0, 3); // LoRo centre
        ltrt.push(2, 3); // LoRo surround
        ltrt.push(1, 1); // b_ltrt_mixinfo; LtRt fields are absent
        let mut reader = BitReader::new(ltrt.as_bytes());
        assert_eq!(
            parse_custom_downmix_data(
                &mut reader,
                PresentationChannelContext::new(Some(3), None, false, 0, false),
            )
            .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 3,
                bit_position: 8,
                remaining_bits: 0,
            })
        );

        let mut lfe = TestBits::new();
        lfe.push(1, 1); // b_stereo_dmx_coeff
        lfe.push(0, 3); // LoRo centre
        lfe.push(2, 3); // LoRo surround
        lfe.push(1, 1); // b_ltrt_mixinfo
        lfe.push(0, 3); // LtRt centre
        lfe.push(2, 3); // LtRt surround
        lfe.push(1, 1); // b_lfe_mixinfo; only one padding bit remains
        let mut reader = BitReader::new(lfe.as_bytes());
        assert_eq!(
            parse_custom_downmix_data(
                &mut reader,
                PresentationChannelContext::new(Some(3), None, false, 0, true),
            )
            .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 5,
                bit_position: 15,
                remaining_bits: 1,
            })
        );
    }

    #[test]
    fn rejects_missing_and_truncated_associated_audio_fields() {
        let mut missing_presence = TestBits::new();
        missing_presence.push(0, 1); // no additional data
        push_minimal_common_metadata_prefix(&mut missing_presence, 0);
        assert_eq!(
            Ac4PresentationSubstream::parse(
                missing_presence.as_bytes(),
                test_context(false, 1, 1, false),
            )
            .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 1,
                bit_position: 16,
                remaining_bits: 0,
            })
        );

        for (truncated_field, bit_position, remaining_bits) in
            [(0, 18, 6), (1, 19, 5), (2, 20, 4), (3, 21, 3)]
        {
            let mut bits = TestBits::new();
            bits.push(0, 1); // no additional data
            push_minimal_common_metadata_prefix(&mut bits, 0);
            bits.push(1, 1); // b_associated
            match truncated_field {
                0 => bits.push(1, 1), // scale_main is truncated
                1 => {
                    bits.push(0, 1);
                    bits.push(1, 1); // scale_main_centre is truncated
                }
                2 => {
                    bits.push(0, 1);
                    bits.push(0, 1);
                    bits.push(1, 1); // scale_main_front is truncated
                }
                _ => {
                    bits.push(0, 1);
                    bits.push(0, 1);
                    bits.push(0, 1);
                    bits.push(1, 1); // pan_associated is truncated
                }
            }

            assert_eq!(
                Ac4PresentationSubstream::parse(bits.as_bytes(), test_context(false, 1, 1, false),)
                    .unwrap_err(),
                PresentationSubstreamError::Read(ReadError::OutOfBounds {
                    requested_bits: 8,
                    bit_position,
                    remaining_bits,
                })
            );
        }
    }

    #[test]
    fn rejects_truncated_group_gain_codes_and_group_count_capacity() {
        let mut truncated = TestBits::new();
        truncated.push(0, 1); // no additional data
        push_minimal_common_metadata_prefix(&mut truncated, 0);
        truncated.push(1, 1); // b_substream_group_gains_present
        truncated.push(0, 1); // new values, not b_keep
        truncated.push(42, 6); // only the first of two group gains
        assert_eq!(
            Ac4PresentationSubstream::parse(
                truncated.as_bytes(),
                test_context(false, 2, 2, false),
            )
            .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 6,
                bit_position: 24,
                remaining_bits: 0,
            })
        );

        assert_eq!(
            Ac4PresentationSubstream::parse(&[], test_context(false, 0, 9, true),).unwrap_err(),
            PresentationSubstreamError::CapacityExceeded {
                what: PresentationSubstreamCapacity::SubstreamGroups,
                declared: 9,
                limit: MAX_GROUPS_PER_PRESENTATION,
            }
        );
    }

    #[test]
    fn rejects_empty_truncated_and_overflowing_drc_envelopes() {
        let mut empty = TestBits::new();
        empty.push(0, 1); // no additional data
        push_minimal_loudness_prefix(&mut empty, 0);
        empty.push(0, 5); // empty drc_frame cannot carry b_drc_present
        empty.push(0, 1); // no size extension
        assert_eq!(
            Ac4PresentationSubstream::parse(empty.as_bytes(), test_context(false, 1, 1, false),)
                .unwrap_err(),
            PresentationSubstreamError::InvalidDrcMetadataSize {
                declared: 0,
                minimum: 1,
                bit_position: 9,
            }
        );

        let mut absent_with_tail = TestBits::new();
        absent_with_tail.push(0, 1); // no additional data
        push_minimal_loudness_prefix(&mut absent_with_tail, 0);
        absent_with_tail.push(2, 5); // presence plus one invalid trailing bit
        absent_with_tail.push(0, 1); // no size extension
        absent_with_tail.push(0, 1); // b_drc_present
        absent_with_tail.push(1, 1); // no syntax is permitted after absent DRC
        assert_eq!(
            Ac4PresentationSubstream::parse(
                absent_with_tail.as_bytes(),
                test_context(false, 1, 1, false),
            )
            .unwrap_err(),
            PresentationSubstreamError::TrailingDrcFrameBits {
                bit_position: 16,
                remaining_bits: 1,
            }
        );

        let mut truncated = TestBits::new();
        truncated.push(0, 1);
        push_minimal_loudness_prefix(&mut truncated, 0);
        truncated.push(16, 5); // declares sixteen drc_frame bits
        truncated.push(0, 1);
        truncated.push_byte(0xff); // only nine bits remain including byte padding
        assert_eq!(
            Ac4PresentationSubstream::parse(
                truncated.as_bytes(),
                test_context(false, 1, 1, false),
            )
            .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 16,
                bit_position: 15,
                remaining_bits: 9,
            })
        );

        let mut overflowing = TestBits::new();
        overflowing.push(0, 1);
        push_minimal_loudness_prefix(&mut overflowing, 0);
        overflowing.push(31, 5);
        overflowing.push(1, 1);
        for _ in 0..12 {
            overflowing.push(7, 3);
            overflowing.push(1, 1);
        }
        overflowing.push(0, 3);
        overflowing.push(0, 1);
        assert!(matches!(
            Ac4PresentationSubstream::parse(
                overflowing.as_bytes(),
                test_context(false, 1, 1, false),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::ValueOverflow { .. }
            ))
        ));
    }

    #[test]
    fn rejects_truncated_and_overflowing_additional_data_lengths() {
        let mut truncated = TestBits::new();
        truncated.push(1, 1); // b_additional_data
        truncated.push(1, 4); // two additional-data bytes
        truncated.byte_align();
        truncated.push_byte(0); // only one byte remains
        assert_eq!(
            Ac4PresentationSubstream::parse(
                truncated.as_bytes(),
                test_context(false, 1, 1, false),
            )
            .unwrap_err(),
            PresentationSubstreamError::Read(ReadError::OutOfBounds {
                requested_bits: 16,
                bit_position: 8,
                remaining_bits: 8,
            })
        );

        let mut overflowing = TestBits::new();
        overflowing.push(1, 1);
        overflowing.push(15, 4);
        for _ in 0..40 {
            overflowing.push(3, 2);
            overflowing.push(1, 1);
        }
        assert!(matches!(
            Ac4PresentationSubstream::parse(
                overflowing.as_bytes(),
                test_context(false, 1, 1, false),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::ValueOverflow { .. }
            ))
        ));
    }

    #[test]
    fn equality_ignores_unparsed_common_metadata_suffix() {
        let mut prefix = TestBits::new();
        prefix.push(1, 1); // name present
        prefix.push(1, 1); // explicit length
        prefix.push(1, 5);
        prefix.push_byte(b'A');
        prefix.push(0, 2); // one target
        push_minimal_target(&mut prefix, 1);
        let expected_offset = prefix.len as u64;

        let mut left_bits = prefix.clone();
        left_bits.push(0, 4);
        let mut right_bits = prefix;
        right_bits.push(0b1111, 4);

        let context = PresentationSubstreamSelectionContext::new(true, 1);
        let left = Ac4PresentationSubstreamSelection::parse(left_bits.as_bytes(), context).unwrap();
        let right =
            Ac4PresentationSubstreamSelection::parse(right_bits.as_bytes(), context).unwrap();
        assert_eq!(left.common_metadata_bit_offset, expected_offset);
        assert_eq!(right.common_metadata_bit_offset, expected_offset);

        let left_alternative = left.alternative.unwrap();
        let right_alternative = right.alternative.unwrap();
        assert_eq!(left_alternative.name, right_alternative.name);
        assert_eq!(left_alternative, right_alternative);
        assert_eq!(
            left_alternative.targets().next().unwrap().unwrap(),
            right_alternative.targets().next().unwrap().unwrap()
        );
        assert_eq!(left, right);
    }

    #[test]
    fn parses_target_fields_and_dataset_map() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(0, 2); // one target
        bits.push(5, 3); // target_level
        bits.push(0b1010, 4); // device categories 0 and 2
        bits.push(1, 1); // extension present
        bits.push(0b1100, 4);
        bits.push(1, 1); // ducking present
        bits.push(33, 6);
        bits.push(1, 1); // loudness correction present
        bits.push(31, 5);
        bits.push(0, 1); // substream 0 inactive
        bits.push(1, 1); // substream 1 active
        bits.push(0, 1); // data set index 0
        bits.push(1, 1); // substream 2 active
        bits.push(1, 1); // extended data set index
        // variable_bits(2) = 6: (00, more), (10, stop); final index = 1 + 6 = 7
        bits.push(0, 2);
        bits.push(1, 1);
        bits.push(2, 2);
        bits.push(0, 1);
        let expected_offset = bits.len as u64;
        bits.push(0b10101, 5); // 尚未解析的公共 metadata 后缀

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 3),
        )
        .unwrap();
        assert_eq!(parsed.common_metadata_bit_offset, expected_offset);

        let alternative = parsed.alternative.unwrap();
        assert_eq!(alternative.n_targets, 1);
        assert_eq!(alternative.n_audio_substreams, 3);
        assert_eq!(alternative.name, None);

        let target = alternative.targets().next().unwrap().unwrap();
        assert_eq!(target.target_level, 5);
        assert_eq!(target.device_categories.raw(), 0b1010);
        assert!(target.device_categories.contains(0));
        assert!(!target.device_categories.contains(1));
        assert!(target.device_categories.contains(2));
        assert!(!target.device_categories.contains(4));
        assert_eq!(target.device_category_extension, Some(0b1100));
        assert_eq!(target.max_ducking_depth, Some(33));
        assert_eq!(target.loudness_correction_target, Some(31));

        let mut activations = target.substream_activations();
        assert_eq!(
            activations.next().unwrap().unwrap(),
            AlternativeSubstreamActivation {
                substream_index: 0,
                active: false,
                alternative_data_set_index: None,
            }
        );
        assert_eq!(
            activations.next().unwrap().unwrap(),
            AlternativeSubstreamActivation {
                substream_index: 1,
                active: true,
                alternative_data_set_index: Some(0),
            }
        );
        assert_eq!(
            activations.next().unwrap().unwrap(),
            AlternativeSubstreamActivation {
                substream_index: 2,
                active: true,
                alternative_data_set_index: Some(7),
            }
        );
        assert!(activations.next().is_none());
    }

    #[test]
    fn preserves_unaligned_serialized_name_chunk() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // name present
        bits.push(1, 1); // explicit length
        bits.push(3, 5);
        bits.push_byte(b'A');
        bits.push_byte(0);
        bits.push_byte(2); // final chunk, two chunks total
        bits.push(0, 2); // one target
        push_minimal_target(&mut bits, 1);

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let name = parsed.alternative.unwrap().name.unwrap();

        assert_eq!(name.len(), 3);
        assert_eq!(name.iter().collect::<std::vec::Vec<_>>(), [b'A', 0, 2]);
        assert_eq!(name.as_aligned_slice(), None, "名称从 bit 7 开始");
        assert_eq!(
            name.chunk_kind(),
            PresentationNameChunkKind::Final { total_chunks: 2 }
        );
    }

    #[test]
    fn parses_extended_target_count() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(3, 2); // four targets before extension
        bits.push(2, 2); // variable_bits(2) = 2
        bits.push(0, 1);
        for _ in 0..6 {
            push_minimal_target(&mut bits, 1);
        }

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let alternative = parsed.alternative.unwrap();
        assert_eq!(alternative.n_targets, 6);
        assert_eq!(alternative.targets().count(), 6);
    }

    #[test]
    fn accepts_fixed_name_and_target_capacity_boundaries() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // name present
        bits.push(0, 1); // fixed 32-byte form
        for _ in 0..31 {
            bits.push_byte(b'N');
        }
        bits.push_byte(0); // complete name terminator
        bits.push(3, 2); // four targets before extension
        // variable_bits(2) = 28: 00/more, 10/more, 00/stop; total = 32
        bits.push(0, 2);
        bits.push(1, 1);
        bits.push(2, 2);
        bits.push(1, 1);
        bits.push(0, 2);
        bits.push(0, 1);
        for _ in 0..MAX_ALTERNATIVE_PRESENTATION_TARGETS {
            push_minimal_target(&mut bits, 1);
        }

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let alternative = parsed.alternative.unwrap();
        let name = alternative.name.unwrap();
        assert_eq!(name.len(), 32);
        assert_eq!(name.get(0), Some(b'N'));
        assert_eq!(name.get(31), Some(0));
        assert_eq!(name.chunk_kind(), PresentationNameChunkKind::Complete);
        assert_eq!(alternative.n_targets, 32);
        assert_eq!(alternative.targets().count(), 32);
    }

    #[test]
    fn preserves_zero_length_name_chunk() {
        let mut bits = TestBits::new();
        bits.push(1, 1); // name present
        bits.push(1, 1); // explicit length
        bits.push(0, 5);
        bits.push(0, 2); // one target
        push_minimal_target(&mut bits, 1);

        let parsed = Ac4PresentationSubstreamSelection::parse(
            bits.as_bytes(),
            PresentationSubstreamSelectionContext::new(true, 1),
        )
        .unwrap();
        let name = parsed.alternative.unwrap().name.unwrap();
        assert!(name.is_empty());
        assert_eq!(name.chunk_kind(), PresentationNameChunkKind::Empty);
    }

    #[test]
    fn rejects_target_count_above_capacity_before_target_loop() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(3, 2); // base 4
        // variable_bits(2) = 29: 00/more, 10/more, 01/stop; total = 33
        bits.push(0, 2);
        bits.push(1, 1);
        bits.push(2, 2);
        bits.push(1, 1);
        bits.push(1, 2);
        bits.push(0, 1);

        assert_eq!(
            Ac4PresentationSubstreamSelection::parse(
                bits.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 1),
            )
            .unwrap_err(),
            PresentationSubstreamError::CapacityExceeded {
                what: PresentationSubstreamCapacity::Targets,
                declared: 33,
                limit: MAX_ALTERNATIVE_PRESENTATION_TARGETS,
            }
        );
    }

    #[test]
    fn rejects_target_count_extension_overflow() {
        let mut bits = TestBits::new();
        bits.push(0, 1); // no name
        bits.push(3, 2); // extended target count
        for _ in 0..40 {
            bits.push(3, 2);
            bits.push(1, 1);
        }

        assert!(matches!(
            Ac4PresentationSubstreamSelection::parse(
                bits.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 1),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::ValueOverflow { .. }
            ))
        ));
    }

    #[test]
    fn rejects_context_beyond_topology_capacity_without_reading_payload() {
        assert_eq!(
            Ac4PresentationSubstreamSelection::parse(
                &[],
                PresentationSubstreamSelectionContext::new(true, 65),
            )
            .unwrap_err(),
            PresentationSubstreamError::CapacityExceeded {
                what: PresentationSubstreamCapacity::AudioSubstreams,
                declared: 65,
                limit: MAX_AUDIO_SUBSTREAMS_PER_PRESENTATION,
            }
        );
    }

    #[test]
    fn refuses_to_treat_unknown_audio_substream_count_as_zero() {
        assert_eq!(
            Ac4PresentationSubstreamSelection::parse(
                &[],
                PresentationSubstreamSelectionContext::new(true, 0),
            )
            .unwrap_err(),
            PresentationSubstreamError::MissingAudioSubstreams
        );
    }

    #[test]
    fn rejects_truncated_name_and_activation_map() {
        let mut name = TestBits::new();
        name.push(1, 1);
        name.push(1, 1);
        name.push(2, 5); // declares two bytes
        name.push_byte(b'A'); // only one byte is present
        assert!(matches!(
            Ac4PresentationSubstreamSelection::parse(
                name.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 1),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::OutOfBounds { .. }
            ))
        ));

        let mut activation = TestBits::new();
        activation.push(0, 1); // no name
        activation.push(0, 2); // one target
        push_minimal_target(&mut activation, 0); // no activation bits available
        assert!(matches!(
            Ac4PresentationSubstreamSelection::parse(
                activation.as_bytes(),
                PresentationSubstreamSelectionContext::new(true, 7),
            ),
            Err(PresentationSubstreamError::Read(
                ReadError::OutOfBounds { .. }
            ))
        ));
    }
}
