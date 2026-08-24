//! Scene 解码错误及其稳定上下文。

use core::fmt;
#[cfg(feature = "audio-decode")]
use macindecode_ac4_bitstream::full_ajoc::FullAjocUnsupported;
use macindecode_ac4_bitstream::{
    oamd::{OamdError, OamdStateError},
    reader::ReadError,
    topology::{Capacity, TopologyError, Unsupported as TopologyUnsupported},
};

/// 解码流水线中的结构化阶段。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStage {
    Topology,
    AudioSyntax,
    CoreAudio,
    Aspx,
    Ajoc,
    Qmf,
    Oamd,
    SceneAssembly,
}

impl DecodeStage {
    /// 用于日志和诊断序列化的稳定名称。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Topology => "topology",
            Self::AudioSyntax => "audio_syntax",
            Self::CoreAudio => "core_audio",
            Self::Aspx => "aspx",
            Self::Ajoc => "ajoc",
            Self::Qmf => "qmf",
            Self::Oamd => "oamd",
            Self::SceneAssembly => "scene_assembly",
        }
    }
}

impl fmt::Display for DecodeStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// 截断输入还缺少的读取信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeedMoreData {
    required_bits: u64,
    available_bits: u64,
}

impl NeedMoreData {
    /// 完成当前 AU 解析至少需要的总比特数。
    #[must_use]
    pub const fn required_bits(self) -> u64 {
        self.required_bits
    }

    /// 当前 AU 实际提供的总比特数。
    #[must_use]
    pub const fn available_bits(self) -> u64 {
        self.available_bits
    }

    /// 为完成当前读取至少还需追加的比特数。
    #[must_use]
    pub const fn minimum_additional_bits(self) -> u64 {
        self.required_bits.saturating_sub(self.available_bits)
    }
}

impl fmt::Display for NeedMoreData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "AU 至少需要 {} 比特，但当前只有 {} 比特",
            self.required_bits, self.available_bits
        )
    }
}

impl core::error::Error for NeedMoreData {}

/// presentation 选择失败。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationSelectionError {
    /// 没有携带音频 group 的 eligible presentation。
    NoEligiblePresentation { declared: u32 },
    /// `AutoUnique` 遇到多个 eligible presentation。
    Ambiguous { eligible: u32 },
    /// 零基下标超出码流声明范围。
    IndexOutOfRange { requested: u32, declared: u32 },
    /// `presentation_id` 在当前配置中不存在。
    IdNotFound { requested: u32 },
    /// 同一 `presentation_id` 在当前配置中出现多次。
    IdNotUnique { requested: u32, matches: u32 },
    /// 显式选择到了只携带数据或不引用音频 group 的 presentation。
    NotEligible { index: u32 },
}

impl fmt::Display for PresentationSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NoEligiblePresentation { declared } => write!(
                formatter,
                "输入声明 {declared} 个 presentation，但没有 eligible 音频 presentation"
            ),
            Self::Ambiguous { eligible } => write!(
                formatter,
                "输入有 {eligible} 个 eligible presentation，AutoUnique 无法唯一选择"
            ),
            Self::IndexOutOfRange {
                requested,
                declared,
            } => write!(
                formatter,
                "presentation 下标 {requested} 超出范围；输入声明 {declared} 个"
            ),
            Self::IdNotFound { requested } => {
                write!(formatter, "presentation_id {requested} 不存在")
            }
            Self::IdNotUnique { requested, matches } => write!(
                formatter,
                "presentation_id {requested} 出现 {matches} 次，无法唯一选择"
            ),
            Self::NotEligible { index } => {
                write!(formatter, "presentation {index} 不携带可选择的音频 group")
            }
        }
    }
}

impl core::error::Error for PresentationSelectionError {}

/// 当前 Scene 解码器明确拒绝的边界。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    LegacyBitstreamVersion {
        bitstream_version: u32,
    },
    FutureBitstreamVersion {
        bitstream_version: u32,
    },
    ReservedChannelMode {
        channel_mode: u32,
    },
    TopologyCapacityExceeded {
        what: Capacity,
        declared: u32,
        limit: usize,
    },
    ChannelBased,
    DirectObject,
    Mixed,
    EmptyScene,
    AjocSubstreamIndexAbsent,
    OamdSubstreamIndexAbsent,
    AjocSubstreamContextConflict,
    MultipleFullSubstreams {
        count: u32,
    },
    MultipleCoreSubstreams {
        count: u32,
    },
    MultiSubstreamFrameRate {
        frame_rate_factor: u32,
    },
    FragmentedFrameRate {
        frame_rate_fraction: u32,
    },
    StaticDownmix,
    SamplingFrequency {
        sampling_frequency_hz: u32,
    },
    FullbandDownmixSignalsExceeded {
        declared: u32,
        limit: u8,
    },
    AjocObjectsExceeded {
        declared: u32,
        limit: usize,
    },
    FullbandObjectAssignment {
        bed_signals: u32,
        isf_signals: u32,
    },
    CoreObjectAssignment {
        bed_signals: u32,
        isf_signals: u32,
    },
    CoreDialogueEnhancement {
        dialogue_objects: u8,
    },
    AudioMetadataBranch,
    AlternativeObjectMetadata,
    FullAjocBranch,
    #[cfg(feature = "audio-decode")]
    FullAjoc(FullAjocUnsupported),
    AudioDecodeFeatureDisabled,
}

impl fmt::Display for UnsupportedReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::LegacyBitstreamVersion { bitstream_version } => write!(
                formatter,
                "bitstream_version {bitstream_version} 的旧 presentation 语法未覆盖"
            ),
            Self::FutureBitstreamVersion { bitstream_version } => write!(
                formatter,
                "bitstream_version {bitstream_version} 超出当前规范支持范围"
            ),
            Self::ReservedChannelMode { channel_mode } => {
                write!(formatter, "保留的 channel_mode {channel_mode}")
            }
            Self::TopologyCapacityExceeded {
                what,
                declared,
                limit,
            } => write!(
                formatter,
                "{what}数为 {declared}，超过当前 Scene 实现上限 {limit}"
            ),
            Self::ChannelBased => formatter.write_str("channel-based 场景尚未进入本 API"),
            Self::DirectObject => {
                formatter.write_str("direct-object 尚无真实 PCM 验证，当前明确拒绝")
            }
            Self::Mixed => formatter.write_str("mixed 编码路径尚未覆盖"),
            Self::EmptyScene => formatter.write_str("所选 presentation 没有音频场景元素"),
            Self::AjocSubstreamIndexAbsent => {
                formatter.write_str("A-JOC substream 未携带可定位的显式下标")
            }
            Self::OamdSubstreamIndexAbsent => {
                formatter.write_str("OAMD substream 未携带可定位的显式下标")
            }
            Self::AjocSubstreamContextConflict => {
                formatter.write_str("同一物理 A-JOC substream 的音频解析上下文互相冲突")
            }
            Self::MultipleFullSubstreams { count } => write!(
                formatter,
                "所选 presentation 引用 {count} 条 A-JOC Full substream；当前只支持一条"
            ),
            Self::MultipleCoreSubstreams { count } => write!(
                formatter,
                "所选 presentation 引用 {count} 条 A-JOC Core substream；当前只支持一条"
            ),
            Self::MultiSubstreamFrameRate { frame_rate_factor } => write!(
                formatter,
                "frame_rate_factor 为 {frame_rate_factor}，一个 info 覆盖多条连续 substream"
            ),
            Self::FragmentedFrameRate {
                frame_rate_fraction,
            } => write!(
                formatter,
                "frame_rate_fraction 为 {frame_rate_fraction}，codec frame 跨多个传输帧"
            ),
            Self::StaticDownmix => formatter
                .write_str("b_static_dmx 需要 channel-based core downmix，当前 Scene 子集未覆盖"),
            Self::SamplingFrequency {
                sampling_frequency_hz,
            } => write!(
                formatter,
                "A-JOC substream 采样率为 {sampling_frequency_hz} Hz，当前只支持 44100/48000 Hz"
            ),
            Self::FullbandDownmixSignalsExceeded { declared, limit } => write!(
                formatter,
                "A-JOC fullband downmix 信号数为 {declared}，超过当前上限 {limit}"
            ),
            Self::AjocObjectsExceeded { declared, limit } => write!(
                formatter,
                "A-JOC 对象数为 {declared}，超过当前 OAMD/Scene 上限 {limit}"
            ),
            Self::FullbandObjectAssignment {
                bed_signals,
                isf_signals,
            } => write!(
                formatter,
                "A-JOC Full 上混分配包含 {bed_signals} 路 BED、{isf_signals} 路 ISF；当前 Scene 只支持动态对象"
            ),
            Self::CoreObjectAssignment {
                bed_signals,
                isf_signals,
            } => write!(
                formatter,
                "A-JOC Core 下混分配包含 {bed_signals} 路 BED、{isf_signals} 路 ISF；当前 Scene 只支持动态对象"
            ),
            Self::CoreDialogueEnhancement { dialogue_objects } => write!(
                formatter,
                "A-JOC Core 启用了 {dialogue_objects} 个 dialogue enhancement 对象；当前 Scene 不应用其系数"
            ),
            Self::AudioMetadataBranch => formatter
                .write_str("ac4_substream metadata 中存在当前 A-JOC Scene 子集未覆盖的分支"),
            Self::AlternativeObjectMetadata => {
                formatter.write_str("b_alternative 对象动态数据分支尚未覆盖")
            }
            Self::FullAjocBranch => {
                formatter.write_str("Full A-JOC engine 返回了尚未分类的未覆盖分支")
            }
            #[cfg(feature = "audio-decode")]
            Self::FullAjoc(reason) => write!(formatter, "{reason}"),
            Self::AudioDecodeFeatureDisabled => {
                formatter.write_str("构建时未启用 audio-decode feature")
            }
        }
    }
}

impl core::error::Error for UnsupportedReason {}

/// 保留底层量化或结构错误的码流失败原因。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitstreamFailure {
    Topology(TopologyError),
    Oamd(OamdError),
    OamdState(OamdStateError),
    OamdCommonConflict,
    OamdTimingConflict {
        expected: u8,
        actual: u8,
    },
    OamdUpdateOrder {
        object_index: u8,
        previous_sample: i64,
        current_sample: i64,
    },
    FrameLengthUnavailable {
        fs_index: u8,
        frame_rate_index: u8,
        frame_rate_factor: u32,
    },
}

impl fmt::Display for BitstreamFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Topology(error) => write!(formatter, "{error}"),
            Self::Oamd(error) => write!(formatter, "{error}"),
            Self::OamdState(error) => write!(formatter, "{error}"),
            Self::OamdCommonConflict => {
                formatter.write_str("同一 group 的 OAMD common 当前更新互相冲突")
            }
            Self::OamdTimingConflict { expected, actual } => write!(
                formatter,
                "所选 Full A-JOC group 的 OAMD 块数冲突：{expected} 与 {actual}"
            ),
            Self::OamdUpdateOrder {
                object_index,
                previous_sample,
                current_sample,
            } => write!(
                formatter,
                "OAMD 对象 {object_index} 的更新样本从 {previous_sample} 回退到 {current_sample}"
            ),
            Self::FrameLengthUnavailable {
                fs_index,
                frame_rate_index,
                frame_rate_factor,
            } => write!(
                formatter,
                "fs_index {fs_index}、frame_rate_index {frame_rate_index} 与因子 \
                 {frame_rate_factor} 没有已定义的 codec frame length"
            ),
        }
    }
}

impl core::error::Error for BitstreamFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Topology(error) => Some(error),
            Self::Oamd(error) => Some(error),
            Self::OamdState(error) => Some(error),
            Self::OamdCommonConflict
            | Self::OamdTimingConflict { .. }
            | Self::OamdUpdateOrder { .. }
            | Self::FrameLengthUnavailable { .. } => None,
        }
    }
}

/// Scene API 的顶层错误类别。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeErrorKind {
    NeedMoreData(NeedMoreData),
    Selection(PresentationSelectionError),
    Unsupported(UnsupportedReason),
    InvalidBitstream(BitstreamFailure),
    DecodeFailure { stage: DecodeStage },
    InternalInvariant { stage: DecodeStage },
    ResetRequired,
}

impl fmt::Display for DecodeErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NeedMoreData(error) => write!(formatter, "输入被截断：{error}"),
            Self::Selection(error) => write!(formatter, "presentation 选择失败：{error}"),
            Self::Unsupported(reason) => write!(formatter, "不支持的解码边界：{reason}"),
            Self::InvalidBitstream(error) => write!(formatter, "无效码流：{error}"),
            Self::DecodeFailure { stage } => write!(formatter, "{stage} 解码失败"),
            Self::InternalInvariant { stage } => write!(formatter, "{stage} 内部不变量失败"),
            Self::ResetRequired => formatter.write_str("Session 已失效，必须显式 reset"),
        }
    }
}

/// 错误发生位置；所有下标均使用码流中的零基值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeErrorContext {
    access_unit_index: u64,
    presentation_index: Option<u32>,
    presentation_id: Option<u32>,
    group_index: Option<u32>,
    substream_index: Option<u32>,
    syntax_path: Option<&'static str>,
    bit_offset: Option<u64>,
}

impl DecodeErrorContext {
    #[must_use]
    pub const fn access_unit_index(self) -> u64 {
        self.access_unit_index
    }

    #[must_use]
    pub const fn presentation_index(self) -> Option<u32> {
        self.presentation_index
    }

    #[must_use]
    pub const fn presentation_id(self) -> Option<u32> {
        self.presentation_id
    }

    #[must_use]
    pub const fn group_index(self) -> Option<u32> {
        self.group_index
    }

    #[must_use]
    pub const fn substream_index(self) -> Option<u32> {
        self.substream_index
    }

    #[must_use]
    pub const fn syntax_path(self) -> Option<&'static str> {
        self.syntax_path
    }

    #[must_use]
    pub const fn bit_offset(self) -> Option<u64> {
        self.bit_offset
    }

    pub(crate) const fn for_access_unit(access_unit_index: u64) -> Self {
        Self {
            access_unit_index,
            presentation_index: None,
            presentation_id: None,
            group_index: None,
            substream_index: None,
            syntax_path: None,
            bit_offset: None,
        }
    }

    pub(crate) const fn with_presentation(mut self, index: u32, id: Option<u32>) -> Self {
        self.presentation_index = Some(index);
        self.presentation_id = id;
        self
    }

    pub(crate) const fn with_group(mut self, index: u32) -> Self {
        self.group_index = Some(index);
        self
    }

    pub(crate) const fn with_substream(mut self, index: u32) -> Self {
        self.substream_index = Some(index);
        self
    }

    pub(crate) const fn with_syntax_path(mut self, path: &'static str) -> Self {
        self.syntax_path = Some(path);
        self
    }

    pub(crate) const fn with_bit_offset(mut self, bit_offset: u64) -> Self {
        self.bit_offset = Some(bit_offset);
        self
    }
}

/// 带 AU、presentation、group、substream 与语法位置的解码错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    kind: DecodeErrorKind,
    context: DecodeErrorContext,
}

impl DecodeError {
    #[must_use]
    pub const fn kind(self) -> DecodeErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn context(self) -> DecodeErrorContext {
        self.context
    }

    pub(crate) const fn new(kind: DecodeErrorKind, context: DecodeErrorContext) -> Self {
        Self { kind, context }
    }

    pub(crate) fn from_topology(error: TopologyError, access_unit_index: u64) -> Self {
        let mut context = DecodeErrorContext::for_access_unit(access_unit_index)
            .with_syntax_path(topology_syntax_path(&error));
        if let Some(bit_offset) = topology_bit_offset(&error) {
            context = context.with_bit_offset(bit_offset);
        }
        context = match error {
            TopologyError::GroupIndexOutOfRange { group_index, .. } => {
                context.with_group(group_index)
            }
            TopologyError::SubstreamIndexOutOfRange { index, .. }
            | TopologyError::UnreferencedSubstream { index }
            | TopologyError::SubstreamPayloadOutOfFrame { index, .. } => {
                context.with_substream(index)
            }
            _ => context,
        };

        let kind = match error {
            TopologyError::Read(ReadError::OutOfBounds {
                requested_bits,
                bit_position,
                remaining_bits,
            })
            | TopologyError::OamdCommon(OamdError::Read(ReadError::OutOfBounds {
                requested_bits,
                bit_position,
                remaining_bits,
            })) => DecodeErrorKind::NeedMoreData(need_more_from_read(
                requested_bits,
                bit_position,
                remaining_bits,
            )),
            payload @ TopologyError::SubstreamPayloadOutOfFrame { .. } => {
                DecodeErrorKind::InvalidBitstream(BitstreamFailure::Topology(payload))
            }
            TopologyError::Unsupported { what, .. } => {
                DecodeErrorKind::Unsupported(topology_unsupported(what))
            }
            TopologyError::CapacityExceeded {
                what,
                declared,
                limit,
            } => DecodeErrorKind::Unsupported(UnsupportedReason::TopologyCapacityExceeded {
                what,
                declared,
                limit,
            }),
            other => DecodeErrorKind::InvalidBitstream(BitstreamFailure::Topology(other)),
        };
        Self::new(kind, context)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "AU {}：{}",
            self.context.access_unit_index, self.kind
        )?;
        if let Some(path) = self.context.syntax_path {
            write!(formatter, "（{path}")?;
            if let Some(bit_offset) = self.context.bit_offset {
                write!(formatter, "，bit {bit_offset}")?;
            }
            formatter.write_str("）")?;
        }
        Ok(())
    }
}

impl core::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match &self.kind {
            DecodeErrorKind::NeedMoreData(error) => Some(error),
            DecodeErrorKind::Selection(error) => Some(error),
            DecodeErrorKind::Unsupported(error) => Some(error),
            DecodeErrorKind::InvalidBitstream(error) => Some(error),
            DecodeErrorKind::DecodeFailure { .. }
            | DecodeErrorKind::InternalInvariant { .. }
            | DecodeErrorKind::ResetRequired => None,
        }
    }
}

const fn topology_unsupported(error: TopologyUnsupported) -> UnsupportedReason {
    match error {
        TopologyUnsupported::LegacyPresentationInfo { bitstream_version } => {
            UnsupportedReason::LegacyBitstreamVersion { bitstream_version }
        }
        TopologyUnsupported::FutureBitstreamVersion { bitstream_version } => {
            UnsupportedReason::FutureBitstreamVersion { bitstream_version }
        }
        TopologyUnsupported::ReservedChannelMode { ch_mode } => {
            UnsupportedReason::ReservedChannelMode {
                channel_mode: ch_mode,
            }
        }
    }
}

const fn topology_syntax_path(error: &TopologyError) -> &'static str {
    match error {
        TopologyError::OamdCommon(_) => {
            "raw_ac4_frame/ac4_toc/ac4_substream_group_info/oamd_common_data"
        }
        TopologyError::Unsupported {
            what:
                TopologyUnsupported::LegacyPresentationInfo { .. }
                | TopologyUnsupported::FutureBitstreamVersion { .. },
            ..
        }
        | TopologyError::PresentationVersionTooLong { .. } => {
            "raw_ac4_frame/ac4_toc/ac4_presentation_info"
        }
        TopologyError::Unsupported {
            what: TopologyUnsupported::ReservedChannelMode { .. },
            ..
        } => "raw_ac4_frame/ac4_toc/ac4_substream_group_info/channel_mode",
        TopologyError::SubstreamIndexOutOfRange { .. }
        | TopologyError::UnreferencedSubstream { .. }
        | TopologyError::SubstreamSizesAbsent
        | TopologyError::SubstreamPayloadOutOfFrame { .. } => {
            "raw_ac4_frame/ac4_toc/substream_index_table"
        }
        TopologyError::GroupIndexOutOfRange { .. } => {
            "raw_ac4_frame/ac4_toc/ac4_presentation_info/group_index"
        }
        TopologyError::Read(_) | TopologyError::CapacityExceeded { .. } => "raw_ac4_frame/ac4_toc",
    }
}

const fn topology_bit_offset(error: &TopologyError) -> Option<u64> {
    match *error {
        TopologyError::Read(read) | TopologyError::OamdCommon(OamdError::Read(read)) => {
            Some(read_bit_offset(read))
        }
        TopologyError::Unsupported { bit_position, .. }
        | TopologyError::PresentationVersionTooLong { bit_position } => Some(bit_position),
        TopologyError::SubstreamPayloadOutOfFrame { frame_len, .. } => {
            Some(frame_len.saturating_mul(8))
        }
        TopologyError::OamdCommon(_)
        | TopologyError::CapacityExceeded { .. }
        | TopologyError::GroupIndexOutOfRange { .. }
        | TopologyError::SubstreamIndexOutOfRange { .. }
        | TopologyError::UnreferencedSubstream { .. }
        | TopologyError::SubstreamSizesAbsent => None,
    }
}

const fn read_bit_offset(error: ReadError) -> u64 {
    match error {
        ReadError::OutOfBounds { bit_position, .. }
        | ReadError::WidthUnsupported { bit_position, .. }
        | ReadError::ValueOverflow { bit_position } => bit_position,
    }
}

const fn need_more_from_read(
    requested_bits: u32,
    bit_position: u64,
    remaining_bits: u64,
) -> NeedMoreData {
    NeedMoreData {
        required_bits: bit_position.saturating_add(requested_bits as u64),
        available_bits: bit_position.saturating_add(remaining_bits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macindecode_ac4_bitstream::topology::Capacity;

    #[test]
    fn truncated_topology_maps_to_retryable_need_more_data() {
        let error = DecodeError::from_topology(
            TopologyError::Read(ReadError::OutOfBounds {
                requested_bits: 7,
                bit_position: 19,
                remaining_bits: 2,
            }),
            41,
        );

        let DecodeErrorKind::NeedMoreData(needed) = error.kind() else {
            panic!("截断输入必须是 NeedMoreData")
        };
        assert_eq!(needed.minimum_additional_bits(), 5);
        assert_eq!(needed.required_bits(), 26);
        assert_eq!(needed.available_bits(), 21);
        assert_eq!(error.context().access_unit_index(), 41);
        assert_eq!(error.context().bit_offset(), Some(19));
        assert_eq!(error.context().syntax_path(), Some("raw_ac4_frame/ac4_toc"));
    }

    #[test]
    fn payload_past_the_bounded_au_is_invalid_bitstream() {
        let topology = TopologyError::SubstreamPayloadOutOfFrame {
            index: 2,
            start: 80,
            end: 112,
            frame_len: 96,
        };
        let error = DecodeError::from_topology(topology, 5);

        assert_eq!(
            error.kind(),
            DecodeErrorKind::InvalidBitstream(BitstreamFailure::Topology(topology))
        );
        assert_eq!(error.context().bit_offset(), Some(96 * 8));
        assert_eq!(error.context().substream_index(), Some(2));
    }

    #[test]
    fn topology_capacity_is_unsupported_instead_of_invalid() {
        let topology = TopologyError::CapacityExceeded {
            what: Capacity::Presentations,
            declared: 9,
            limit: 8,
        };
        let error = DecodeError::from_topology(topology, 3);

        assert_eq!(
            error.kind(),
            DecodeErrorKind::Unsupported(UnsupportedReason::TopologyCapacityExceeded {
                what: Capacity::Presentations,
                declared: 9,
                limit: 8,
            })
        );
    }

    #[test]
    fn topology_reference_errors_keep_their_structured_indices() {
        let group = DecodeError::from_topology(
            TopologyError::GroupIndexOutOfRange {
                group_index: 7,
                total: 2,
            },
            3,
        );
        assert_eq!(group.context().group_index(), Some(7));

        let substream = DecodeError::from_topology(
            TopologyError::SubstreamIndexOutOfRange {
                index: 11,
                total: 4,
            },
            3,
        );
        assert_eq!(substream.context().substream_index(), Some(11));
    }

    #[test]
    fn topology_unsupported_keeps_structured_reason_and_bit_offset() {
        let error = DecodeError::from_topology(
            TopologyError::Unsupported {
                what: TopologyUnsupported::FutureBitstreamVersion {
                    bitstream_version: 5,
                },
                bit_position: 6,
            },
            9,
        );

        assert_eq!(
            error.kind(),
            DecodeErrorKind::Unsupported(UnsupportedReason::FutureBitstreamVersion {
                bitstream_version: 5,
            })
        );
        assert_eq!(error.context().bit_offset(), Some(6));
    }
}
