//! 可从有界 payload 或已有 engine observation 准备的 audio metadata 状态。
use crate::{
    AudioMetadataRecord, MetadataError, MetadataErrorContext, MetadataErrorKind,
    same_audio_context_family,
};
use macindecode_ac4_bitstream::{
    Ac4AudioSubstream, AudioSubstreamError, DialogEnhancementConfiguration,
    DialogEnhancementConfigurationUpdate, SubstreamContext,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct AudioMetadataState {
    context: Option<SubstreamContext>,
    de: Option<DialogEnhancementConfiguration>,
}
#[derive(Debug, Clone, Copy)]
pub struct PreparedAudioMetadata {
    next: AudioMetadataState,
    record: AudioMetadataRecord,
}
impl PreparedAudioMetadata {
    pub const fn record(&self) -> AudioMetadataRecord {
        self.record
    }
}
impl AudioMetadataState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn commit(&mut self, candidate: PreparedAudioMetadata) {
        *self = candidate.next;
    }
    pub const fn effective_de_configuration(&self) -> Option<DialogEnhancementConfiguration> {
        self.de
    }
    pub fn prepare(
        &self,
        payload: &[u8],
        candidates: &[SubstreamContext],
        scope: MetadataErrorContext,
    ) -> PreparedAudioMetadata {
        let mut first = None;
        let mut stereo = None;
        let mut locked = None;
        let mut successes = 0usize;
        let mut failure = None;
        for &context in candidates {
            match Ac4AudioSubstream::parse(payload, context) {
                Ok(parsed) => {
                    let value = (context, parsed);
                    successes = successes.saturating_add(1);
                    first.get_or_insert(value);
                    if context.channel_mode == Some(1) {
                        stereo = Some(value);
                    }
                    if self
                        .context
                        .is_some_and(|c| same_audio_context_family(c, context))
                    {
                        locked = Some(value);
                    }
                }
                Err(e) => {
                    if failure.is_none() || matches!(e, AudioSubstreamError::Unsupported { .. }) {
                        failure = Some(e);
                    }
                }
            }
        }
        let chosen = if self.context.is_some() {
            locked
        } else {
            stereo.or(first)
        };
        let result = chosen.ok_or_else(|| {
            MetadataError::new(
                if successes > 0 {
                    MetadataErrorKind::ContextConflict
                } else {
                    failure.map_or(
                        MetadataErrorKind::Unsupported("audio metadata context"),
                        MetadataErrorKind::Audio,
                    )
                },
                scope,
            )
        });
        self.prepare_result(result, successes > 1, scope)
    }
    /// engine 已验证 payload 时直接交接摘要，不再次解析音频语法。
    pub fn prepare_parsed(
        &self,
        context: SubstreamContext,
        parsed: Ac4AudioSubstream,
        scope: MetadataErrorContext,
    ) -> PreparedAudioMetadata {
        let result = if self
            .context
            .is_some_and(|c| !same_audio_context_family(c, context))
        {
            Err(MetadataError::new(
                MetadataErrorKind::ContextConflict,
                scope,
            ))
        } else {
            Ok((context, parsed))
        };
        self.prepare_result(result, false, scope)
    }
    fn prepare_result(
        &self,
        result: Result<(SubstreamContext, Ac4AudioSubstream), MetadataError>,
        ambiguous: bool,
        scope: MetadataErrorContext,
    ) -> PreparedAudioMetadata {
        let mut next = *self;
        match result {
            Ok((context, parsed)) => {
                if !ambiguous {
                    next.context = Some(context);
                }
                match parsed.tools_metadata.dialog_enhancement.configuration {
                    DialogEnhancementConfigurationUpdate::NotPresent => {
                        if context.b_iframe == Some(true) {
                            next.de = None;
                        }
                    }
                    DialogEnhancementConfigurationUpdate::KeepPrevious => {}
                    DialogEnhancementConfigurationUpdate::New(c) => next.de = Some(c),
                }
            }
            Err(_) => next.de = None,
        }
        PreparedAudioMetadata {
            next,
            record: AudioMetadataRecord {
                substream_index: scope.substream_index.unwrap_or(0),
                result,
                effective_de_configuration: next.de,
                context_ambiguous: ambiguous,
            },
        }
    }
}
