//! 有表语法后端。源顺序的 OAMD 继承独立于 Scene 表 188 到期队列。
use crate::oamd::resolve_side;
use crate::{
    MetadataError, MetadataErrorContext, MetadataErrorKind, MetadataPartition,
    ObjectMetadataDomain, PresentationSubstreamMetadata, session::MetadataOutput,
};
use alloc::{collections::BTreeMap, vec::Vec};
use macindecode_ac4_bitstream::{
    oamd::{OamdState, OamdTimingData},
    substream::SubstreamInfo,
    topology::{Ac4Topology, MAX_SUBSTREAMS},
};
use macindecode_ac4_decode::{
    Ac4SubstreamAjoc, AjocSubstreamContext, DialogEnhancementState, PresentationDrcGainSetExt,
    PresentationDrcGains, PresentationDrcGainsContext,
    audio_syntax::{FullAjocSyntaxDecoder, FullAjocSyntaxFrameInput},
};

#[derive(Debug, Clone, Copy)]
pub struct AjocMetadataRecord {
    pub substream_index: u32,
    pub parsed: Ac4SubstreamAjoc,
    pub context: AjocSubstreamContext,
}
#[derive(Debug, Clone, Copy)]
pub struct DrcMetadataRecord {
    pub presentation_index: u32,
    pub gain_set_index: usize,
    pub gains: Option<PresentationDrcGains>,
    pub extension_bit_range: Option<(u64, u64)>,
}
#[derive(Debug, Clone, Copy)]
pub struct DeMetadataRecord {
    pub substream_index: u32,
    pub data: Option<macindecode_ac4_decode::DialogEnhancementEffectiveData>,
}
#[derive(Debug, Clone, Copy)]
struct ObjectHistory {
    dmx: OamdState,
    umx: OamdState,
    dmx_timing: Option<OamdTimingData>,
    umx_timing: Option<OamdTimingData>,
}
impl Default for ObjectHistory {
    fn default() -> Self {
        Self {
            dmx: OamdState::new(),
            umx: OamdState::new(),
            dmx_timing: None,
            umx_timing: None,
        }
    }
}
#[derive(Debug, Default)]
pub(crate) struct ExtendedMetadataDecoder {
    syntax: FullAjocSyntaxDecoder,
    objects: BTreeMap<u32, ObjectHistory>,
    de: BTreeMap<u32, DialogEnhancementState>,
    inputs: Vec<SyntaxInput>,
}
type SyntaxInput = (
    u32,
    Result<(AjocSubstreamContext, Option<OamdTimingData>), MetadataError>,
);
impl ExtendedMetadataDecoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        self.syntax.reset();
        self.objects
            .values_mut()
            .for_each(|s| *s = ObjectHistory::default());
        self.de.values_mut().for_each(DialogEnhancementState::reset);
    }
    pub fn observe(
        &mut self,
        raw: &[u8],
        topology: &Ac4Topology,
        mask: u8,
        output: &mut MetadataOutput,
    ) -> Result<(), MetadataError> {
        let scope = MetadataErrorContext::for_access_unit(output.context.index());
        output
            .ajoc
            .try_reserve(MAX_SUBSTREAMS)
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        output
            .de
            .try_reserve(MAX_SUBSTREAMS)
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        self.inputs.clear();
        self.inputs
            .try_reserve(MAX_SUBSTREAMS)
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        for (group_index, group) in topology.groups().iter().enumerate() {
            let group_index = u32::try_from(group_index).unwrap_or(u32::MAX);
            if mask & 1u8.checked_shl(group_index).unwrap_or(0) == 0 {
                continue;
            }
            let common = output.groups.iter().find(|g| g.group_index == group_index);
            let timing = common.and_then(|g| g.effective_timing);
            let references = topology
                .presentations()
                .iter()
                .filter(|p| p.group_indices().contains(&group_index));
            let alternative = references
                .clone()
                .any(|p| p.substream.is_some_and(|s| s.alternative));
            let fraction = references.map(|p| p.frame_rate_fraction).max().unwrap_or(1);
            for info in group.substreams() {
                let SubstreamInfo::Ajoc(info) = info else {
                    continue;
                };
                let Some(index) = info.substream_index() else {
                    continue;
                };
                let context = AjocSubstreamContext::derive(
                    &topology.toc,
                    info,
                    group.frame_rate_factor,
                    fraction,
                    alternative,
                    timing.map(|t| t.num_obj_info_blocks),
                )
                .map(|c| (c, timing))
                .map_err(|e| {
                    MetadataError::new(
                        MetadataErrorKind::AjocContext(e),
                        scope.with_group(group_index).with_substream(index),
                    )
                });
                if let Some((_, previous)) = self.inputs.iter_mut().find(|(i, _)| *i == index) {
                    if *previous != context {
                        *previous = Err(MetadataError::new(
                            MetadataErrorKind::ContextConflict,
                            scope.with_substream(index),
                        ));
                    }
                } else {
                    self.inputs.push((index, context));
                }
            }
        }
        self.inputs.sort_unstable_by_key(|(index, _)| *index);
        for (index, input) in self.inputs.iter().copied() {
            let source = scope.with_substream(index);
            let (context, group_timing) = match input {
                Ok(c) => c,
                Err(e) => {
                    self.syntax.reset_substream(index);
                    self.objects.remove(&index);
                    output.issue(MetadataPartition::CoreOamd, e);
                    continue;
                }
            };
            let payload = topology
                .substream_payload(raw, index)
                .map_err(|e| MetadataError::new(MetadataErrorKind::Topology(e), source))?;
            let state = self.objects.entry(index).or_default();
            let previous = *state;
            let frame = match self.syntax.decode_complete_frame(FullAjocSyntaxFrameInput {
                payload,
                context,
                substream_index: index,
                physical_substreams: 1,
            }) {
                Ok(frame) => frame,
                Err(e) => {
                    *state = ObjectHistory::default();
                    if matches!(e,macindecode_ac4_decode::audio_syntax::FullAjocSyntaxError::AllocationFailure {..}){return Err(MetadataError::new(MetadataErrorKind::Capacity,source));}
                    output.issue(
                        MetadataPartition::CoreOamd,
                        MetadataError::new(MetadataErrorKind::AudioSyntax(e), source),
                    );
                    continue;
                }
            };
            let parsed = frame.parsed();
            output.ajoc.push(AjocMetadataRecord {
                substream_index: index,
                parsed,
                context,
            });
            let dmx_timing = parsed
                .audio
                .dmx_timing
                .or(group_timing)
                .or(previous.dmx_timing);
            let umx_timing =
                parsed
                    .audio
                    .umx_timing
                    .or(if parsed.audio.derive_timing_from_dmx == Some(true) {
                        dmx_timing
                    } else {
                        group_timing.or(previous.umx_timing)
                    });
            let update_start = output.updates.len();
            let object_start = output.objects.len();
            let result = (|| {
                let dmx = resolve_side(
                    previous.dmx,
                    frame.dmx_blocks(),
                    parsed.audio.dmx_num_obj_info_blocks,
                    dmx_timing,
                    context.dmx_objects.as_slice(),
                    index,
                    ObjectMetadataDomain::Core,
                    output,
                )?;
                let umx = resolve_side(
                    previous.umx,
                    frame.umx_blocks(),
                    parsed.audio.umx_num_obj_info_blocks,
                    umx_timing,
                    context.umx_objects.as_slice(),
                    index,
                    ObjectMetadataDomain::Full,
                    output,
                )?;
                Ok::<_, MetadataError>(ObjectHistory {
                    dmx,
                    umx,
                    dmx_timing,
                    umx_timing,
                })
            })();
            match result {
                Ok(next) => *state = next,
                Err(e) => {
                    *state = ObjectHistory::default();
                    output.updates.truncate(update_start);
                    output.objects.truncate(object_start);
                    output.issue(MetadataPartition::CoreOamd, e);
                }
            }
        }
        for position in 0..output.audio.len() {
            let Some(record) = output.audio.get(position).copied() else {
                continue;
            };
            let index = record.substream_index;
            let state = self.de.entry(index).or_default();
            let Ok((_, parsed)) = record.result else {
                state.reset();
                continue;
            };
            let payload = topology.substream_payload(raw, index).map_err(|e| {
                MetadataError::new(MetadataErrorKind::Topology(e), scope.with_substream(index))
            })?;
            match state.decode_frame(parsed.tools_metadata.dialog_enhancement, payload) {
                Ok(de) => output.de.push(DeMetadataRecord {
                    substream_index: index,
                    data: de,
                }),
                Err(e) => {
                    state.reset();
                    output.issue(
                        MetadataPartition::DialogueEnhancement,
                        MetadataError::new(
                            MetadataErrorKind::DialogueEnhancement(e),
                            scope.with_substream(index),
                        ),
                    );
                }
            }
        }
        for position in 0..output.presentations.len() {
            let Some(envelope) = output.presentations.get(position).copied() else {
                continue;
            };
            let Ok(record) = envelope.result else {
                continue;
            };
            let payload = topology
                .substream_payload(raw, record.substream_index)
                .map_err(|e| MetadataError::new(MetadataErrorKind::Topology(e), scope))?;
            let view = PresentationSubstreamMetadata::from_record(payload, record);
            let parsed = view
                .parsed_substream()
                .map_err(|e| MetadataError::new(MetadataErrorKind::Presentation(e), scope))?;
            let Some(data) = parsed.drc_data_elements else {
                continue;
            };
            output
                .drc
                .try_reserve(data.gain_sets().len())
                .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
            for (gain_set_index, set) in data.gain_sets().iter().copied().enumerate() {
                let factor = topology
                    .presentations()
                    .get(usize::try_from(record.presentation_index).unwrap_or(usize::MAX))
                    .map(|p| p.frame_rate_factor);
                let shape = drc_context(
                    record.context,
                    factor.and_then(|f| topology.toc.codec_frame_len_base(f)),
                );
                match set.decode_gains(shape) {
                    Ok(decoded) => output.drc.push(DrcMetadataRecord {
                        presentation_index: record.presentation_index,
                        gain_set_index,
                        gains: decoded.gains,
                        extension_bit_range: decoded
                            .extension
                            .map(|v| (v.bit_offset(), v.len_bits())),
                    }),
                    Err(e) => output.issue(
                        MetadataPartition::DrcGains,
                        MetadataError::new(
                            MetadataErrorKind::DrcGains(e),
                            scope
                                .with_presentation(
                                    record.presentation_index,
                                    record.presentation_id,
                                )
                                .with_substream(record.substream_index),
                        ),
                    ),
                }
            }
        }
        Ok(())
    }
}

fn drc_context(
    context: macindecode_ac4_bitstream::PresentationSubstreamContext,
    frame_len: Option<u16>,
) -> Option<PresentationDrcGainsContext> {
    PresentationDrcGainsContext::derive(
        context.channel_context().presentation_channel_mode(),
        frame_len?,
    )
}
