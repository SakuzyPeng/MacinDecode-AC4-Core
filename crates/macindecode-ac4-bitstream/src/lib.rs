//! AC-4 比特流语法、拓扑与元数据解析。
//!
//! 规范基线 `TS103190:2025-07`，见 `docs/SPEC_TRACEABILITY.md`。
//!
//! 本 crate 只负责解析，不依赖容器或平台。所有解析入口都接受有限切片，不会越过
//! 传入数据的边界寻找内容。
//! OAMD、substream 与 topology 领域类型分别只以 [`oamd`]、[`substream`]、
//! [`topology`] 为规范公共入口；crate 根不重复批量重导出这些类型。
//!
//! 数值重建（ASF、A-SPX、A-JOC、QMF）与一切需要 ETSI 表的处理都在
//! `macindecode-ac4-decode`，见 ADR-0013。本 crate 因此没有 feature、没有构建
//! 脚本，也不含任何规范表。

#![no_std]

pub mod audio_substream;
pub mod emdf;
pub mod math;
pub mod oamd;
pub mod presentation;
pub mod presentation_substream;
pub mod reader;
pub mod substream;
pub mod syncframe;
pub mod toc;
pub mod topology;

pub use audio_substream::{
    Ac4AudioSubstream, AlternativeOamdContext, AudioSubstreamError, AudioToolsMetadata,
    AudioToolsMetadataBits, BasicMetadata, DialogEnhancementConfiguration,
    DialogEnhancementConfigurationUpdate, DialogEnhancementMetadata, ExtendedMetadata,
    FurtherLoudnessInfo, LoudnessExtensionBits, LoudnessProgrammeBoundary, PreprocessingMetadata,
    StereoDownmixPreprocessingMetadata, SubstreamContext,
};
pub use emdf::{
    EmdfError, EmdfInfo, EmdfPayload, EmdfPayloadByteIter, EmdfPayloadBytes, EmdfPayloadConfig,
    EmdfPayloadsSubstream, MAX_EMDF_PAYLOAD_BYTES, MAX_EMDF_PAYLOADS,
};
pub use presentation::{Ac4PresentationV1Info, PresentationSubstreamInfo};
pub use presentation_substream::{
    Ac4PresentationSubstream, Ac4PresentationSubstreamSelection, AdvancedDeConfig, AdvancedDeData,
    AlternativePresentationSelection, PresentationAddDataBits, PresentationAdditionalData,
    PresentationAssociatedAudio, PresentationChannelContext,
    PresentationCoreStereoLoudnessCorrection, PresentationCustomDownmixConfiguration,
    PresentationCustomDownmixData, PresentationCustomDownmixParameters,
    PresentationDrcCompressionCurve, PresentationDrcConfiguration, PresentationDrcCurveData,
    PresentationDrcCurveSection, PresentationDrcData, PresentationDrcDecoderMode,
    PresentationDrcFrameBits, PresentationDrcGainSet, PresentationDrcOutputLevelRange,
    PresentationDrcProfile, PresentationDrcState, PresentationDrcTimeConstants,
    PresentationLoudnessCorrectionCode, PresentationLoudnessCorrectionData,
    PresentationScreenDownmix, PresentationStereoDownmixCoefficients,
    PresentationStereoDownmixKind, PresentationSubstreamCapacity, PresentationSubstreamContext,
    PresentationSubstreamError, PresentationSubstreamGroupGainCodes,
    PresentationSubstreamGroupGainState, PresentationSubstreamGroupGainStateError,
    PresentationSubstreamGroupGainUpdate, PresentationSubstreamSelectionContext,
    PresentationTopDownmix, PresentationTopPairDestination, PresentationTopPairDownmix,
};
pub use reader::{BitReader, ReadError};
pub use syncframe::{SyncFrame, SyncFrameError, SyncFrameIter, SyncWord};
pub use toc::{Ac4Toc, DecodingDelay, SequenceTransition, TocError};
