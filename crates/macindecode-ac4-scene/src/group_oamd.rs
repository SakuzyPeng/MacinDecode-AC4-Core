//! 共享 group OAMD 状态到 Scene 错误契约的适配。
use crate::{
    BitstreamFailure, DecodeError, DecodeErrorContext, DecodeErrorKind, DecodeStage,
    UnsupportedReason,
};
use macindecode_ac4_bitstream::topology::Ac4Topology;
pub(crate) use macindecode_ac4_metadata::group_oamd::{ErrorScope, PreparedGroupOamd};

#[derive(Debug, Default)]
pub(crate) struct GroupOamdDecoder(macindecode_ac4_metadata::group_oamd::GroupOamdDecoder);
impl GroupOamdDecoder {
    pub(crate) const fn new() -> Self {
        Self(macindecode_ac4_metadata::group_oamd::GroupOamdDecoder::new())
    }
    pub(crate) fn reset(&mut self) {
        self.0.reset();
    }
    pub(crate) fn commit(&mut self, candidate: &PreparedGroupOamd) {
        self.0.commit(candidate);
    }
    pub(crate) fn prepare(
        &self,
        raw: &[u8],
        topology: &Ac4Topology,
        mask: u8,
        reset: bool,
        scope: ErrorScope,
    ) -> Result<PreparedGroupOamd, DecodeError> {
        self.0
            .prepare(raw, topology, mask, reset, scope)
            .map_err(map_error)
    }
}
fn map_error(error: macindecode_ac4_metadata::MetadataError) -> DecodeError {
    use macindecode_ac4_metadata::MetadataErrorKind as K;
    let kind = match error.kind() {
        K::Topology(e) => DecodeErrorKind::InvalidBitstream(BitstreamFailure::Topology(e)),
        K::Oamd(e) => DecodeErrorKind::InvalidBitstream(BitstreamFailure::Oamd(e)),
        K::OamdState(e) => DecodeErrorKind::InvalidBitstream(BitstreamFailure::OamdState(e)),
        K::OamdCommonConflict => {
            DecodeErrorKind::InvalidBitstream(BitstreamFailure::OamdCommonConflict)
        }
        K::OamdTimingConflict { expected, actual } => {
            DecodeErrorKind::InvalidBitstream(BitstreamFailure::OamdTimingConflict {
                expected,
                actual,
            })
        }
        K::Unsupported(_) => {
            DecodeErrorKind::Unsupported(UnsupportedReason::OamdSubstreamIndexAbsent)
        }
        _ => DecodeErrorKind::InternalInvariant {
            stage: DecodeStage::Oamd,
        },
    };
    let source = error.context();
    let mut context = DecodeErrorContext::for_access_unit(source.access_unit_index);
    if let Some(index) = source.presentation_index {
        context = context.with_presentation(index, source.presentation_id);
    }
    if let Some(index) = source.group_index {
        context = context.with_group(index);
    }
    if let Some(index) = source.substream_index {
        context = context.with_substream(index);
    }
    if let Some(path) = source.syntax_path {
        context = context.with_syntax_path(path);
    }
    DecodeError::new(kind, context)
}
