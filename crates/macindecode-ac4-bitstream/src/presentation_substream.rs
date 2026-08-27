//! `ac4_presentation_substream()` 的选择、additional-data、响度、DRC、group gain 与关联音频。
//!
//! 对应 `TS103190-2:v1.3.1:6.2.2.3`、`6.2.2.5`；选择字段语义见
//! `6.3.3.1.1` 至 `6.3.3.1.15`，additional-data 字段见
//! `6.3.3.1.16` 至 `6.3.3.1.18`；响度语法见 `6.2.7.3`，共享字段语义见
//! `TS103190-1:v1.4.1:4.3.12.3`；DRC envelope 见 `6.3.3.1.19` 至
//! `6.3.3.1.21`，substream-group gain 见 `6.3.3.1.22` 至 `6.3.3.1.24`，
//! associated-audio 字段见 `6.3.3.1.25` 至 `6.3.3.1.26`，其码值语义见
//! `TS103190-1:v1.4.1:4.3.12.4.3` 至 `4.3.12.4.9`；custom downmix 语法与语义见
//! `TS103190-2:v1.3.1:6.2.9.2` 至 `6.2.9.10`、`6.3.10.2` 至 `6.3.10.3`，共享的
//! stereo downmix 码值见 `TS103190-1:v1.4.1:4.3.12.2.8` 至 `4.3.12.2.19`。
//!
//! 本模块解析 presentation 名称分片、播放目标、逐音频 substream 的
//! activation/dataset map，以及有界 additional-data 区域中的 immersive/OAMD timing 与
//! advanced dialogue-enhancement 原始码值，并保留 dialnorm、further loudness 和严格定界的
//! `drc_frame()` 原始比特、逐帧 substream-group gain 更新、associated-audio scale/pan 码值及
//! custom downmix 的配置、路由与 gain 码值。DRC 内部语法、group gain 跨帧生效状态与
//! loudness correction 仍不解释；本模块也不执行任何处理。

use crate::audio_substream::FurtherLoudnessInfo;
use crate::presentation::MAX_GROUPS_PER_PRESENTATION;
use crate::reader::{BitReader, ReadError};
use crate::substream::MAX_LF_SUBSTREAMS;
use core::fmt;

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

/// presentation substream 前缀中超出固定容量的结构。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSubstreamCapacity {
    /// alternative target 数。
    Targets,
    /// 一个 presentation 中按语法顺序出现的音频 substream 数。
    AudioSubstreams,
    /// 一个 presentation 声明的 substream group 数。
    SubstreamGroups,
}

impl fmt::Display for PresentationSubstreamCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match *self {
            PresentationSubstreamCapacity::Targets => "alternative presentation targets",
            PresentationSubstreamCapacity::AudioSubstreams => "audio substreams in a presentation",
            PresentationSubstreamCapacity::SubstreamGroups => "substream groups in a presentation",
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
/// [`PresentationChannelContext::presentation_channel_mode`] 为 `None` 时，
/// additional-data envelope 内会多传一个 `b_oamd_common_timing`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationSubstreamContext {
    selection: PresentationSubstreamSelectionContext,
    n_substream_groups: u32,
    channel: PresentationChannelContext,
}

impl Default for PresentationSubstreamContext {
    fn default() -> Self {
        Self::new(false, 0, 0, PresentationChannelContext::UNDEFINED)
    }
}

impl PresentationSubstreamContext {
    /// 构造完整解析上下文。
    #[must_use]
    pub const fn new(
        alternative: bool,
        n_audio_substreams: u32,
        n_substream_groups: u32,
        channel: PresentationChannelContext,
    ) -> Self {
        Self {
            selection: PresentationSubstreamSelectionContext::new(alternative, n_audio_substreams),
            n_substream_groups,
            channel,
        }
    }

    /// selection 前缀所需的上下文子集。
    #[must_use]
    pub const fn selection_context(self) -> PresentationSubstreamSelectionContext {
        self.selection
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
/// 当前增量只读取首个 `b_drc_present` 并保留整个 frame，不解释或执行 DRC。
pub type PresentationDrcFrameBits<'a> = PresentationAddDataBits<'a>;

/// 当前帧传输的逐 substream-group `sg_gain` 六比特码值。
///
/// 最多保存 [`MAX_GROUPS_PER_PRESENTATION`] 个值，不做 dB 换算或增益应用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationSubstreamGroupGainCodes {
    codes: [u8; MAX_GROUPS_PER_PRESENTATION],
    len: usize,
}

impl PresentationSubstreamGroupGainCodes {
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
/// 本枚举保留 `b_substream_group_gains_present`/`b_keep` 的码流语义，不解析上一帧的有效值。
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
/// associated audio 与 custom downmix。
///
/// [`drc_metadata_size_value_offset`](Self::drc_metadata_size_value_offset) 指向响度字段之后的
/// `drc_metadata_size_value`；[`b_associated_offset`](Self::b_associated_offset) 指向 group gain
/// 之后的 associated-audio metadata；[`custom_downmix_offset`](Self::custom_downmix_offset) 指向
/// `custom_dmx_data()`，[`loudness_correction_offset`](Self::loudness_correction_offset) 指向随后的
/// `loud_corr()`。DRC 内部语法、group gain 跨帧有效状态和 loudness correction 尚未解析。
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
            && self.substream_group_gain_update == other.substream_group_gain_update
            && self.b_associated_offset == other.b_associated_offset
            && self.associated_audio == other.associated_audio
            && self.custom_downmix_offset == other.custom_downmix_offset
            && self.custom_downmix == other.custom_downmix
            && self.loudness_correction_offset == other.loudness_correction_offset
    }
}

impl Eq for Ac4PresentationSubstream<'_> {}

impl<'a> Ac4PresentationSubstream<'a> {
    /// 解析 selection、common additional-data、presentation 响度、DRC envelope、group gain、
    /// associated audio 与 custom downmix。
    ///
    /// `payload` 必须恰好是 TOC 中 presentation substream index 对应的有界 payload。
    /// additional-data 声明的完整字节区域会先验界；`advanced_de_data()` 仅保留原始码值，
    /// 不执行 dialogue enhancement。进一步响度字段同样只保留原值，不执行归一化；
    /// `drc_metadata_size` 只严格定界并保留完整 `drc_frame()`，不解释或执行 DRC；group gain
    /// 只保留逐帧传输形态，不解析跨帧有效值或应用增益；associated-audio scale/pan 同样只
    /// 保留原值，不执行 gain、pan 或 renderer 处理；custom downmix 同样只保留配置、路由与
    /// gain 码值，不执行矩阵运算。
    ///
    /// # Errors
    ///
    /// selection 字段、additional-data/DRC/group gain/associated audio/custom downmix 或其已知
    /// 字段截断，变长字段溢出，或计数超过固定容量时返回错误。零长度 DRC envelope 不能容纳
    /// 必需的 presence bit，以及 associated pan、custom output config 或 stereo surround gain
    /// 使用禁止码值时，同样返回错误。
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
        let (drc_present, drc_frame) = parse_drc_frame_envelope(&mut reader, payload)?;
        let substream_group_gain_update =
            parse_substream_group_gain_update(&mut reader, n_substream_groups)?;
        let b_associated_offset = reader.bit_position();
        let associated_audio = parse_associated_audio(&mut reader)?;
        let custom_downmix_offset = reader.bit_position();
        let custom_downmix = parse_custom_downmix_data(&mut reader, context.channel_context())?;
        let loudness_correction_offset = reader.bit_position();
        Ok(Self {
            selection,
            additional_data,
            dialnorm_bits_offset,
            dialnorm_bits,
            further_loudness,
            drc_metadata_size_value_offset,
            drc_present,
            drc_frame,
            substream_group_gain_update,
            b_associated_offset,
            associated_audio,
            custom_downmix_offset,
            custom_downmix,
            loudness_correction_offset,
        })
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

fn parse_drc_frame_envelope<'a>(
    reader: &mut BitReader<'a>,
    source: &'a [u8],
) -> Result<(bool, PresentationDrcFrameBits<'a>), PresentationSubstreamError> {
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
    reader.skip_bits(u64::from(size))?;
    let bits = PresentationDrcFrameBits {
        source,
        bit_offset,
        bit_len: u64::from(size),
    };
    let mut frame_reader = BitReader::new(source);
    frame_reader.skip_bits(bit_offset)?;
    let present = frame_reader.read_flag()?;
    Ok((present, bits))
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

        let parsed = Ac4PresentationSubstream::parse(bits.as_bytes(), context).unwrap();
        let additional = parsed.additional_data.unwrap();
        assert_eq!(additional.oamd_common_timing, Some(true));
        assert_eq!(additional.advanced_de_data, None);
        assert_eq!(additional.add_data.len_bits(), 5);
        assert_eq!(parsed.dialnorm_bits_offset, 16);
        assert_eq!(parsed.drc_metadata_size_value_offset, 24);
    }

    #[test]
    fn parses_object_additional_data_and_ignores_loudness_correction_suffix_in_equality() {
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
        left_bits.push(0, 4); // unparsed loud_corr()
        let mut right_bits = envelope;
        push_minimal_complete_common_metadata(&mut right_bits, 0b101_0101);
        right_bits.push(0b1111, 4);
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
        assert_eq!(left, right, "loudness correction 尚未进入已解析视图");
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
        bits.push(0b10101, 5); // loud_corr() must remain untouched

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
    fn distinguishes_absent_and_kept_substream_group_gains() {
        let mut prefix = TestBits::new();
        prefix.push(0, 1); // no additional data
        push_minimal_common_metadata_prefix(&mut prefix, 0);

        let mut absent = prefix.clone();
        absent.push(0, 1); // b_substream_group_gains_present
        absent.push(0, 1); // no associated audio
        absent.push(0b11_1111, 6); // loud_corr() must remain untouched
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
        kept.push(0b1_1111, 5); // loud_corr() must remain untouched
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
        bits.push(0b10, 2); // loud_corr() must remain untouched

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
            bits.push_byte(0xff); // loud_corr(), not pan_associated

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
            bits.push(0b101, 3); // loud_corr() must remain untouched

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
    fn parses_complete_nine_x_four_custom_downmix_and_stops_at_loud_corr() {
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

        let mut right_bits = bits.clone();
        bits.push(0, 4); // unparsed loud_corr() suffix
        right_bits.push(0b1111, 4); // different unparsed loud_corr() suffix
        let context = PresentationSubstreamContext::new(
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
        bits.push(1, 1); // unparsed loud_corr()

        let parsed = Ac4PresentationSubstream::parse(
            bits.as_bytes(),
            PresentationSubstreamContext::new(true, 1, 1, PresentationChannelContext::UNDEFINED),
        )
        .unwrap();
        assert!(parsed.selection.alternative.is_some());
        assert_eq!(parsed.custom_downmix_offset, expected_offset);
        assert_eq!(parsed.loudness_correction_offset, expected_offset);
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
