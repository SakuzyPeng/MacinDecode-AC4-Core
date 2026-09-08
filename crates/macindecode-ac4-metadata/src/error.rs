//! 有界元数据错误与定位。局部分区错误由 AU observation 保存。
use core::fmt;
use macindecode_ac4_bitstream::{
    AudioSubstreamError, PresentationSubstreamError, PresentationSubstreamGroupGainStateError,
    oamd::{OamdError, OamdStateError},
    topology::TopologyError,
};

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataErrorKind {
    FeatureUnavailable,
    NeedMoreData,
    Toc(macindecode_ac4_bitstream::TocError),
    Topology(TopologyError),
    Presentation(PresentationSubstreamError),
    GroupGain(PresentationSubstreamGroupGainStateError),
    Audio(AudioSubstreamError),
    #[cfg(feature = "metadata-decode")]
    AudioSyntax(macindecode_ac4_decode::audio_syntax::FullAjocSyntaxError),
    #[cfg(feature = "metadata-decode")]
    AjocContext(macindecode_ac4_decode::SubstreamAudioError),
    #[cfg(feature = "metadata-decode")]
    DrcGains(macindecode_ac4_decode::PresentationDrcGainsError),
    #[cfg(feature = "metadata-decode")]
    DialogueEnhancement(macindecode_ac4_decode::DialogEnhancementStateError),
    Oamd(OamdError),
    OamdState(OamdStateError),
    OamdTimingConflict {
        expected: u8,
        actual: u8,
    },
    OamdCommonConflict,
    Unsupported(&'static str),
    PresentationSelection(crate::selection::PresentationSelectionError),
    ContextConflict,
    HistoryUnavailable,
    Capacity,
    TimelineOverflow,
    Invariant,
    ResetRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataErrorContext {
    pub access_unit_index: u64,
    pub presentation_index: Option<u32>,
    pub presentation_id: Option<u32>,
    pub group_index: Option<u32>,
    pub substream_index: Option<u32>,
    pub syntax_path: Option<&'static str>,
}

impl MetadataErrorContext {
    pub const fn for_access_unit(index: u64) -> Self {
        Self {
            access_unit_index: index,
            presentation_index: None,
            presentation_id: None,
            group_index: None,
            substream_index: None,
            syntax_path: None,
        }
    }
    pub const fn with_presentation(mut self, index: u32, id: Option<u32>) -> Self {
        self.presentation_index = Some(index);
        self.presentation_id = id;
        self
    }
    pub const fn with_group(mut self, index: u32) -> Self {
        self.group_index = Some(index);
        self
    }
    pub const fn with_substream(mut self, index: u32) -> Self {
        self.substream_index = Some(index);
        self
    }
    pub const fn with_syntax_path(mut self, path: &'static str) -> Self {
        self.syntax_path = Some(path);
        self
    }
    pub const fn presentation_index(self) -> Option<u32> {
        self.presentation_index
    }
    pub const fn group_index(self) -> Option<u32> {
        self.group_index
    }
    pub const fn substream_index(self) -> Option<u32> {
        self.substream_index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataError {
    kind: MetadataErrorKind,
    context: MetadataErrorContext,
}
impl MetadataError {
    pub const fn new(kind: MetadataErrorKind, context: MetadataErrorContext) -> Self {
        Self { kind, context }
    }
    pub const fn kind(self) -> MetadataErrorKind {
        self.kind
    }
    pub const fn context(self) -> MetadataErrorContext {
        self.context
    }
}
impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AU {} metadata {:?}: {:?}",
            self.context.access_unit_index, self.context, self.kind
        )
    }
}
impl core::error::Error for MetadataError {}
