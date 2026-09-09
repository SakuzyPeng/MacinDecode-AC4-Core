//! 源 AU 观察。边界先验证，分区单独失效，所有借用视图在本次调用结束时发布。
use crate::{
    AccessUnit, AccessUnitContext, MetadataError, MetadataErrorContext, MetadataErrorKind,
    MetadataStateMachine, PresentationSelection, PresentationSubstreamMetadata,
    group_oamd::{ErrorScope, GroupOamdDecoder, PreparedGroupOamdState},
    presentation::{PresentationMetadataState, PresentationSubstreamMetadataRecord},
};
use alloc::{collections::BTreeMap, vec::Vec};
use macindecode_ac4_bitstream::{
    Ac4AudioSubstream, Ac4Toc, DialogEnhancementConfiguration, PresentationChannelContext,
    PresentationSubstreamContext, SubstreamContext,
    oamd::{
        AdditionalObjectMetadata, OamdMetadataBlock, OamdTimingData, ObjectDescriptor,
        ObjectMetadataState,
    },
    reader::ReadError,
    substream::SubstreamInfo,
    topology::{
        Ac4Topology, DecoderAction, MAX_PRESENTATIONS, MAX_SUBSTREAM_GROUPS, MAX_SUBSTREAMS,
        ResetReason, TopologyError,
    },
};

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataDetail {
    #[default]
    Basic,
    Full,
}
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresentationScope {
    #[default]
    All,
    Selected(PresentationSelection),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ac4MetadataConfig {
    detail: MetadataDetail,
    scope: PresentationScope,
}
impl Ac4MetadataConfig {
    pub const fn new() -> Self {
        Self {
            detail: MetadataDetail::Basic,
            scope: PresentationScope::All,
        }
    }
    pub const fn with_detail(mut self, detail: MetadataDetail) -> Self {
        self.detail = detail;
        self
    }
    pub const fn with_presentations(mut self, scope: PresentationScope) -> Self {
        self.scope = scope;
        self
    }
    pub const fn detail(self) -> MetadataDetail {
        self.detail
    }
    pub const fn presentations(self) -> PresentationScope {
        self.scope
    }
}
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataStatus {
    Ready,
    Partial,
    WaitingForRandomAccess,
}
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataPartition {
    Topology,
    Presentation,
    GroupGain,
    Audio,
    GroupOamd,
    CoreOamd,
    FullOamd,
    DrcGains,
    DialogueEnhancement,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataDiagnostic {
    pub partition: MetadataPartition,
    pub error: MetadataError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectMetadataDomain {
    Core,
    Full,
    Standalone,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataObjectState {
    pub substream_index: u32,
    pub domain: ObjectMetadataDomain,
    pub object_index: u8,
    pub descriptor: ObjectDescriptor,
    pub state: ObjectMetadataState,
    pub additional: AdditionalObjectMetadata,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataObjectUpdate {
    pub target: MetadataObjectState,
    pub raw: OamdMetadataBlock,
    pub timing: Option<OamdTimingData>,
    pub offset_samples: Option<u32>,
    pub ramp_duration_samples: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioMetadataRecord {
    pub substream_index: u32,
    pub result: Result<(SubstreamContext, Ac4AudioSubstream), MetadataError>,
    pub effective_de_configuration: Option<DialogEnhancementConfiguration>,
    /// 全部已验证候选，仅用于保留 IMS 诊断与兼容显示。
    pub context_ambiguous: bool,
}
#[derive(Debug, Clone, Copy)]
pub struct PresentationMetadataRecord {
    pub presentation_index: u32,
    pub substream_index: u32,
    pub result: Result<PresentationSubstreamMetadataRecord, MetadataError>,
}

#[derive(Debug)]
pub(crate) struct MetadataOutput {
    pub(crate) raw: Vec<u8>,
    pub(crate) context: AccessUnitContext,
    pub(crate) toc: Option<Ac4Toc>,
    pub(crate) topology: Option<Ac4Topology>,
    pub(crate) transition: Option<macindecode_ac4_bitstream::topology::TopologyTransition>,
    pub(crate) generation: u32,
    pub(crate) epoch: u64,
    pub(crate) sample_start: Option<i64>,
    pub(crate) status: MetadataStatus,
    pub(crate) presentations: Vec<PresentationMetadataRecord>,
    pub(crate) audio: Vec<AudioMetadataRecord>,
    pub(crate) groups: Vec<PreparedGroupOamdState>,
    pub(crate) objects: Vec<MetadataObjectState>,
    pub(crate) updates: Vec<MetadataObjectUpdate>,
    pub(crate) diagnostics: Vec<MetadataDiagnostic>,
    #[cfg(feature = "metadata-decode")]
    pub(crate) ajoc: Vec<crate::AjocMetadataRecord>,
    #[cfg(feature = "metadata-decode")]
    pub(crate) drc: Vec<crate::DrcMetadataRecord>,
    #[cfg(feature = "metadata-decode")]
    pub(crate) de: Vec<crate::DeMetadataRecord>,
}
impl Default for MetadataOutput {
    fn default() -> Self {
        Self {
            raw: Vec::new(),
            context: AccessUnitContext::default(),
            toc: None,
            topology: None,
            transition: None,
            generation: 0,
            epoch: 0,
            sample_start: None,
            status: MetadataStatus::WaitingForRandomAccess,
            presentations: Vec::new(),
            audio: Vec::new(),
            groups: Vec::new(),
            objects: Vec::new(),
            updates: Vec::new(),
            diagnostics: Vec::new(),
            #[cfg(feature = "metadata-decode")]
            ajoc: Vec::new(),
            #[cfg(feature = "metadata-decode")]
            drc: Vec::new(),
            #[cfg(feature = "metadata-decode")]
            de: Vec::new(),
        }
    }
}
impl MetadataOutput {
    fn clear(&mut self) {
        self.generation = 0;
        self.epoch = 0;
        self.sample_start = None;
        self.status = MetadataStatus::WaitingForRandomAccess;
        self.raw.clear();
        self.toc = None;
        self.topology = None;
        self.transition = None;
        self.presentations.clear();
        self.audio.clear();
        self.groups.clear();
        self.objects.clear();
        self.updates.clear();
        self.diagnostics.clear();
        #[cfg(feature = "metadata-decode")]
        {
            self.ajoc.clear();
            self.drc.clear();
            self.de.clear();
        }
    }
    pub(crate) fn issue(&mut self, partition: MetadataPartition, mut error: MetadataError) {
        // All scope 的 group 可以由多个 presentation 引用，不能把内部占位 index 0
        // 发布成已知的 presentation 身份；保留 group/substream 供调用方关联。
        if partition == MetadataPartition::GroupOamd {
            let mut context = error.context();
            context.presentation_index = None;
            context.presentation_id = None;
            error = MetadataError::new(error.kind(), context);
        }
        self.diagnostics
            .push(MetadataDiagnostic { partition, error });
        if self.status != MetadataStatus::WaitingForRandomAccess {
            self.status = MetadataStatus::Partial;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MetadataAccessUnit<'a> {
    output: &'a MetadataOutput,
}
impl<'a> MetadataAccessUnit<'a> {
    pub const fn context(self) -> AccessUnitContext {
        self.output.context
    }
    pub const fn status(self) -> MetadataStatus {
        self.output.status
    }
    pub const fn configuration_generation(self) -> u32 {
        self.output.generation
    }
    /// reset/不连续建立新的观察区间；不能跨区间声称完整覆盖。
    pub const fn observation_epoch(self) -> u64 {
        self.output.epoch
    }
    pub const fn source_sample_start(self) -> Option<i64> {
        self.output.sample_start
    }
    pub const fn toc(self) -> Option<&'a Ac4Toc> {
        self.output.toc.as_ref()
    }
    pub const fn topology(self) -> Option<&'a Ac4Topology> {
        self.output.topology.as_ref()
    }
    pub const fn transition(
        self,
    ) -> Option<macindecode_ac4_bitstream::topology::TopologyTransition> {
        self.output.transition
    }
    pub fn raw_frame(self) -> &'a [u8] {
        &self.output.raw
    }
    pub fn presentations(self) -> &'a [PresentationMetadataRecord] {
        &self.output.presentations
    }
    pub fn presentation_metadata(self, index: u32) -> Option<PresentationSubstreamMetadata<'a>> {
        let record = self
            .output
            .presentations
            .iter()
            .find(|p| p.presentation_index == index)?
            .result
            .ok()?;
        let payload = self
            .output
            .topology
            .as_ref()?
            .substream_payload(&self.output.raw, record.substream_index)
            .ok()?;
        Some(PresentationSubstreamMetadata::from_record(payload, record))
    }
    pub fn audio_metadata(self) -> &'a [AudioMetadataRecord] {
        &self.output.audio
    }
    pub fn audio_payload(self, index: u32) -> Option<&'a [u8]> {
        self.topology()?
            .substream_payload(&self.output.raw, index)
            .ok()
    }
    pub fn group_oamd(self) -> &'a [PreparedGroupOamdState] {
        &self.output.groups
    }
    /// 按源 AU 的码流顺序还原的末尾目标状态，不是播放起点状态。
    pub fn objects(self) -> &'a [MetadataObjectState] {
        &self.output.objects
    }
    pub fn object_updates(self) -> &'a [MetadataObjectUpdate] {
        &self.output.updates
    }
    pub fn diagnostics(self) -> &'a [MetadataDiagnostic] {
        &self.output.diagnostics
    }
    #[cfg(feature = "metadata-decode")]
    pub fn ajoc_metadata(self) -> &'a [crate::AjocMetadataRecord] {
        &self.output.ajoc
    }
    #[cfg(feature = "metadata-decode")]
    pub fn drc_gains(self) -> &'a [crate::DrcMetadataRecord] {
        &self.output.drc
    }
    #[cfg(feature = "metadata-decode")]
    pub fn dialogue_enhancement(self) -> &'a [crate::DeMetadataRecord] {
        &self.output.de
    }
}

#[derive(Debug, Default)]
struct PresentationHistory {
    state: PresentationMetadataState,
    channel: Option<PresentationChannelContext>,
}

#[derive(Debug)]
pub struct Ac4MetadataSession {
    config: Ac4MetadataConfig,
    control: MetadataStateMachine,
    presentations: BTreeMap<u32, PresentationHistory>,
    audio: BTreeMap<u32, crate::AudioMetadataState>,
    audio_candidates: BTreeMap<u32, Vec<SubstreamContext>>,
    groups: GroupOamdDecoder,
    output: MetadataOutput,
    epoch: u64,
    cursor: Option<i64>,
    reset_required: bool,
    #[cfg(feature = "metadata-decode")]
    extended: crate::extended::ExtendedMetadataDecoder,
}
impl Ac4MetadataSession {
    pub fn new(config: Ac4MetadataConfig) -> Result<Self, MetadataError> {
        if config.detail == MetadataDetail::Full && !cfg!(feature = "metadata-decode") {
            return Err(MetadataError::new(
                MetadataErrorKind::FeatureUnavailable,
                MetadataErrorContext::for_access_unit(0),
            ));
        }
        Ok(Self {
            config,
            control: MetadataStateMachine::new(),
            presentations: BTreeMap::new(),
            audio: BTreeMap::new(),
            audio_candidates: BTreeMap::new(),
            groups: GroupOamdDecoder::new(),
            output: MetadataOutput::default(),
            epoch: 0,
            cursor: Some(0),
            reset_required: false,
            #[cfg(feature = "metadata-decode")]
            extended: crate::extended::ExtendedMetadataDecoder::new(),
        })
    }
    pub const fn config(&self) -> Ac4MetadataConfig {
        self.config
    }
    fn clear_history(&mut self) {
        for history in self.presentations.values_mut() {
            history.state.reset();
            history.channel = None;
        }
        for history in self.audio.values_mut() {
            history.reset();
        }
        self.groups.reset();
        #[cfg(feature = "metadata-decode")]
        self.extended.reset();
    }
    pub fn mark_discontinuity(&mut self) {
        self.output.clear();
        self.clear_history();
        self.control
            .mark_discontinuity(ResetReason::ExternalDiscontinuity);
        self.cursor = None;
        if let Some(next) = self.epoch.checked_add(1) {
            self.epoch = next;
        } else {
            self.reset_required = true;
        }
    }
    pub fn reset(&mut self) {
        self.mark_discontinuity();
        self.control = MetadataStateMachine::new();
        self.cursor = Some(0);
        self.reset_required = self.epoch == u64::MAX;
    }
    pub fn observe_access_unit(
        &mut self,
        au: AccessUnit<'_>,
    ) -> Result<MetadataAccessUnit<'_>, MetadataError> {
        let context = au.context();
        let scope = MetadataErrorContext::for_access_unit(context.index());
        if self.reset_required {
            return Err(MetadataError::new(MetadataErrorKind::ResetRequired, scope));
        }
        self.output.clear();
        let toc = Ac4Toc::parse(au.raw_frame()).map_err(|e| {
            let kind = if matches!(
                e,
                macindecode_ac4_bitstream::TocError::Read(ReadError::OutOfBounds { .. })
            ) {
                MetadataErrorKind::NeedMoreData
            } else {
                self.clear_history();
                self.control.mark_discontinuity(ResetReason::ParseFailure);
                self.cursor = None;
                MetadataErrorKind::Toc(e)
            };
            MetadataError::new(kind, scope)
        })?;
        let topology = match Ac4Topology::parse(au.raw_frame()) {
            Ok(t) => t,
            Err(e) => {
                if matches!(e, TopologyError::Read(ReadError::OutOfBounds { .. })) {
                    return Err(MetadataError::new(MetadataErrorKind::NeedMoreData, scope));
                }
                self.clear_history();
                self.control.mark_discontinuity(ResetReason::ParseFailure);
                self.cursor = None;
                if !matches!(e, TopologyError::Unsupported { .. }) {
                    return Err(MetadataError::new(MetadataErrorKind::Topology(e), scope));
                }
                self.output
                    .raw
                    .try_reserve(au.raw_frame().len())
                    .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
                self.output.raw.extend_from_slice(au.raw_frame());
                self.output.context = context;
                self.output.epoch = self.epoch;
                self.output.sample_start = context.source_sample_start();
                self.output.toc = Some(toc);
                self.output.status = MetadataStatus::Partial;
                self.output.issue(
                    MetadataPartition::Topology,
                    MetadataError::new(MetadataErrorKind::Topology(e), scope),
                );
                return Ok(MetadataAccessUnit {
                    output: &self.output,
                });
            }
        };
        if let Err(e) = macindecode_ac4_bitstream::topology::validate_group_references(&topology)
            .and_then(|()| {
                macindecode_ac4_bitstream::topology::validate_substream_references(&topology)
            })
        {
            self.clear_history();
            self.control.mark_discontinuity(ResetReason::ParseFailure);
            self.cursor = None;
            return Err(MetadataError::new(MetadataErrorKind::Topology(e), scope));
        }
        if let Err(e) = topology.substream_payload(
            au.raw_frame(),
            topology.index_table.n_substreams.saturating_sub(1),
        ) {
            self.clear_history();
            self.control.mark_discontinuity(ResetReason::ParseFailure);
            self.cursor = None;
            return Err(MetadataError::new(MetadataErrorKind::Topology(e), scope));
        }
        let selected = selected_presentations(&topology, self.config.scope)
            .map_err(|k| MetadataError::new(k, scope))?;
        let prepared = self.control.prepare(&topology, context)?;
        let transition = prepared.transition();
        let reset = matches!(
            transition.action,
            DecoderAction::Reset { .. } | DecoderAction::WaitForRandomAccess { .. }
        );
        let sample_start = context
            .source_sample_start()
            .or(if context.discontinuity() {
                None
            } else {
                self.cursor
            });
        let next_cursor = match (sample_start, toc.frame_len_base()) {
            (Some(s), Some(n)) => Some(s.checked_add(i64::from(n)).ok_or(MetadataError::new(
                MetadataErrorKind::TimelineOverflow,
                scope,
            ))?),
            _ => None,
        };
        self.output
            .raw
            .try_reserve(au.raw_frame().len())
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        self.output
            .presentations
            .try_reserve(MAX_PRESENTATIONS)
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        self.output
            .audio
            .try_reserve(MAX_SUBSTREAMS)
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        self.output
            .groups
            .try_reserve(MAX_SUBSTREAM_GROUPS)
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        self.output
            .diagnostics
            .try_reserve(MAX_SUBSTREAMS.saturating_mul(8))
            .map_err(|_| MetadataError::new(MetadataErrorKind::Capacity, scope))?;
        if reset {
            self.clear_history();
        }
        if transition.config_changed
            || context.discontinuity()
            || matches!(transition.action, DecoderAction::Reset { .. })
            || transition.sequence == macindecode_ac4_bitstream::SequenceTransition::SourceChange
        {
            self.epoch = self.epoch.checked_add(1).ok_or(MetadataError::new(
                MetadataErrorKind::TimelineOverflow,
                scope,
            ))?;
        }
        self.output.context = context;
        self.output.toc = Some(toc);
        self.output.generation = transition.generation;
        self.output.transition = Some(transition);
        self.output.epoch = self.epoch;
        self.output.sample_start = sample_start;
        self.output.status =
            if matches!(transition.action, DecoderAction::WaitForRandomAccess { .. }) {
                MetadataStatus::WaitingForRandomAccess
            } else {
                MetadataStatus::Ready
            };
        if self.output.status != MetadataStatus::WaitingForRandomAccess {
            self.observe_presentations(au.raw_frame(), &topology, selected.as_slice(), scope);
            let group_mask = selected
                .iter()
                .filter_map(|i| topology.presentations().get(*i))
                .flat_map(|p| p.group_indices())
                .fold(0u8, |mask, &g| mask | 1u8.checked_shl(g).unwrap_or(0));
            self.observe_groups(au.raw_frame(), &topology, group_mask, scope);
            self.observe_audio(au.raw_frame(), &topology, group_mask, scope);
            #[cfg(feature = "metadata-decode")]
            if self.config.detail == MetadataDetail::Full
                && let Err(error) =
                    self.extended
                        .observe(au.raw_frame(), &topology, group_mask, &mut self.output)
            {
                self.clear_history();
                self.control.mark_discontinuity(ResetReason::ParseFailure);
                self.reset_required = true;
                return Err(error);
            }
        }
        if let Some(error) = self
            .output
            .diagnostics
            .iter()
            .find(|d| {
                matches!(
                    d.error.kind(),
                    MetadataErrorKind::Capacity | MetadataErrorKind::Invariant
                )
            })
            .map(|d| d.error)
        {
            self.clear_history();
            self.control.mark_discontinuity(ResetReason::ParseFailure);
            self.reset_required = true;
            return Err(error);
        }
        self.output.topology = Some(topology);
        self.output.raw.extend_from_slice(au.raw_frame());
        self.control.commit(prepared);
        self.cursor = next_cursor;
        Ok(MetadataAccessUnit {
            output: &self.output,
        })
    }
    fn observe_presentations(
        &mut self,
        raw: &[u8],
        topology: &Ac4Topology,
        selected: &[usize],
        scope: MetadataErrorContext,
    ) {
        for &index in selected {
            let Some(p) = topology.presentations().get(index) else {
                continue;
            };
            let Some(reference) = p.substream else {
                continue;
            };
            let Some(context) = topology.presentation_substream_context(index) else {
                continue;
            };
            let Ok(payload) = topology.substream_payload(raw, reference.substream_index) else {
                continue;
            };
            let key = u32::try_from(index).unwrap_or(u32::MAX);
            let source = scope
                .with_presentation(key, p.presentation_id)
                .with_substream(reference.substream_index);
            let history = self.presentations.entry(key).or_default();
            let locked = history.channel.map(|channel| {
                PresentationSubstreamContext::new(
                    context.selection_context().alternative(),
                    context.presentation_is_independent(),
                    context.selection_context().n_audio_substreams(),
                    context.n_substream_groups(),
                    channel,
                )
            });
            let primary = locked.unwrap_or(context);
            let mut candidate = history.state.prepare(payload, primary, source, false);
            if candidate.is_err() && locked.is_none() && p.presentation_version == 2 {
                let ims = PresentationSubstreamContext::new(
                    context.selection_context().alternative(),
                    context.presentation_is_independent(),
                    context.selection_context().n_audio_substreams(),
                    context.n_substream_groups(),
                    PresentationChannelContext::new(Some(1), None, false, 0, false),
                );
                candidate = history.state.prepare(payload, ims, source, false);
            }
            let result = match candidate {
                Ok(candidate) => {
                    if history
                        .state
                        .storage
                        .try_reserve_payload(payload.len())
                        .is_err()
                    {
                        Err(MetadataError::new(MetadataErrorKind::Capacity, source))
                    } else {
                        let record = candidate.record;
                        history.channel = Some(record.context.channel_context());
                        history.state.commit(candidate);
                        Ok(record)
                    }
                }
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                history.state.reset();
                self.output.issue(MetadataPartition::Presentation, e);
            }
            self.output.presentations.push(PresentationMetadataRecord {
                presentation_index: key,
                substream_index: reference.substream_index,
                result,
            });
        }
    }
    fn observe_groups(
        &mut self,
        raw: &[u8],
        topology: &Ac4Topology,
        mask: u8,
        scope: MetadataErrorContext,
    ) {
        let error_scope = ErrorScope {
            access_unit_index: scope.access_unit_index,
            presentation_index: scope.presentation_index.unwrap_or(0),
            presentation_id: scope.presentation_id,
        };
        let object_start = self.output.objects.len();
        let update_start = self.output.updates.len();
        let mut remaining = mask;
        let mut invalidated = 0u8;
        loop {
            let mut observe =
                |index,
                 parsed: &macindecode_ac4_bitstream::oamd::OamdSubstreamPayload,
                 initial: macindecode_ac4_bitstream::oamd::OamdState,
                 descriptors: &macindecode_ac4_bitstream::oamd::ObjectDescriptors| {
                    if descriptors.as_slice().iter().all(|d| d.b_ajoc_coded) {
                        return Ok(());
                    }
                    let timing = parsed.timing.or(initial.effective_timing());
                    let count = timing
                        .map(|t| t.num_obj_info_blocks)
                        .or(initial.previous_num_obj_info_blocks())
                        .unwrap_or(0);
                    crate::oamd::resolve_side(
                        initial,
                        parsed.metadata_blocks(),
                        count,
                        timing,
                        descriptors.as_slice(),
                        index,
                        ObjectMetadataDomain::Standalone,
                        &mut self.output,
                    )
                    .map(|_| ())
                };
            match self.groups.prepare_with_observer(
                raw,
                topology,
                remaining,
                false,
                error_scope,
                false,
                &mut observe,
            ) {
                Ok(candidate) => {
                    // 失败组的 common 单独失效；与成功组共享的物理状态由候选恢复。
                    for index in 0..topology.groups().len() {
                        let group = u32::try_from(index).unwrap_or(u32::MAX);
                        if invalidated & 1u8.checked_shl(group).unwrap_or(0) != 0 {
                            self.groups.invalidate_group(topology, index);
                        }
                    }
                    self.output.groups.extend_from_slice(candidate.groups());
                    self.groups.commit(&candidate);
                    break;
                }
                Err(error) => {
                    self.output.objects.truncate(object_start);
                    self.output.updates.truncate(update_start);
                    let failed = failed_oamd_groups(topology, remaining, error);
                    invalidated |= failed;
                    remaining &= !failed;
                    for index in 0..topology.groups().len() {
                        let group = u32::try_from(index).unwrap_or(u32::MAX);
                        if failed & 1u8.checked_shl(group).unwrap_or(0) != 0 {
                            self.output.issue(
                                MetadataPartition::GroupOamd,
                                MetadataError::new(error.kind(), error.context().with_group(group)),
                            );
                        }
                    }
                    // 每次至少排除一组；余下组始终从本 AU 之前的历史重新准备。
                    // TS103190-2:v1.3.1:6.2.2.4、TS103190-2:v1.3.1:6.3.9：
                    // 共享物理载荷的差分只能提交一次，剩余组间的上下文仍须一致。
                }
            }
        }
    }
    fn observe_audio(
        &mut self,
        raw: &[u8],
        topology: &Ac4Topology,
        group_mask: u8,
        scope: MetadataErrorContext,
    ) {
        fill_audio_contexts(
            topology,
            group_mask,
            &self.output.groups,
            &mut self.audio_candidates,
        );
        for (&index, candidates) in &self.audio_candidates {
            if candidates.is_empty() {
                continue;
            }
            let Ok(payload) = topology.substream_payload(raw, index) else {
                continue;
            };
            let history = self.audio.entry(index).or_default();
            let candidate = history.prepare(payload, candidates, scope.with_substream(index));
            let record = candidate.record();
            if self.config.detail == MetadataDetail::Basic
                && let Ok((_, parsed)) = record.result
                && parsed.tools_metadata.dialog_enhancement.configuration
                    == macindecode_ac4_bitstream::DialogEnhancementConfigurationUpdate::KeepPrevious
                && record.effective_de_configuration.is_none()
            {
                self.output.issue(
                    MetadataPartition::DialogueEnhancement,
                    MetadataError::new(
                        MetadataErrorKind::HistoryUnavailable,
                        scope.with_substream(index),
                    ),
                );
            }
            if let Err(e) = record.result {
                self.output.issue(MetadataPartition::Audio, e);
            }
            history.commit(candidate);
            self.output.audio.push(record);
        }
    }
}

fn failed_oamd_groups(topology: &Ac4Topology, selected: u8, error: MetadataError) -> u8 {
    let context = error.context();
    let affected = if error.kind() != MetadataErrorKind::ContextConflict
        && let Some(group) = context.group_index
    {
        1u8.checked_shl(group).unwrap_or(0)
    } else if let Some(physical) = context.substream_index {
        topology
            .groups()
            .iter()
            .enumerate()
            .fold(0u8, |mask, (index, group)| {
                if group.oamd_substream.and_then(|s| s.substream_index) == Some(physical) {
                    mask | 1u8
                        .checked_shl(u32::try_from(index).unwrap_or(u32::MAX))
                        .unwrap_or(0)
                } else {
                    mask
                }
            })
    } else {
        0
    };
    let affected = affected & selected;
    if affected == 0 { selected } else { affected }
}

#[derive(Debug)]
struct SelectedPresentations {
    indices: [usize; MAX_PRESENTATIONS],
    len: usize,
}
impl SelectedPresentations {
    fn as_slice(&self) -> &[usize] {
        self.indices.get(..self.len).unwrap_or(&[])
    }
    fn iter(&self) -> core::slice::Iter<'_, usize> {
        self.as_slice().iter()
    }
}
fn selected_presentations(
    topology: &Ac4Topology,
    scope: PresentationScope,
) -> Result<SelectedPresentations, MetadataErrorKind> {
    let mut selected = SelectedPresentations {
        indices: [0; MAX_PRESENTATIONS],
        len: 0,
    };
    match scope {
        PresentationScope::All => {
            for (index, slot) in selected
                .indices
                .iter_mut()
                .take(topology.presentations().len())
                .enumerate()
            {
                *slot = index;
                selected.len = selected.len.saturating_add(1);
            }
        }
        PresentationScope::Selected(selection) => {
            let mut candidates =
                [crate::selection::PresentationCandidate::EMPTY; MAX_PRESENTATIONS];
            for (index, p) in topology.presentations().iter().enumerate() {
                let slot = candidates
                    .get_mut(index)
                    .ok_or(MetadataErrorKind::Capacity)?;
                *slot = crate::selection::PresentationCandidate {
                    index: u32::try_from(index).map_err(|_| MetadataErrorKind::Capacity)?,
                    id: p.presentation_id,
                    eligible: p.presentation_config != Some(6) && !p.group_indices().is_empty(),
                };
            }
            let candidate = crate::selection::select_candidate(
                candidates
                    .get(..topology.presentations().len())
                    .ok_or(MetadataErrorKind::Capacity)?,
                selection,
            )
            .map_err(MetadataErrorKind::PresentationSelection)?;
            *selected
                .indices
                .first_mut()
                .ok_or(MetadataErrorKind::Capacity)? =
                usize::try_from(candidate.index).map_err(|_| MetadataErrorKind::Capacity)?;
            selected.len = 1;
        }
    }
    Ok(selected)
}

fn fill_audio_contexts(
    topology: &Ac4Topology,
    group_mask: u8,
    groups: &[PreparedGroupOamdState],
    contexts: &mut BTreeMap<u32, Vec<SubstreamContext>>,
) {
    for candidates in contexts.values_mut() {
        candidates.clear();
    }
    for (group_index, group) in topology.groups().iter().enumerate() {
        if group_mask
            & 1u8
                .checked_shl(u32::try_from(group_index).unwrap_or(u32::MAX))
                .unwrap_or(0)
            == 0
        {
            continue;
        }
        let associated = |p: &&macindecode_ac4_bitstream::Ac4PresentationV1Info| {
            p.group_indices()
                .iter()
                .any(|&i| usize::try_from(i) == Ok(group_index))
        };
        let alternative = topology
            .presentations()
            .iter()
            .filter(associated)
            .any(|p| p.substream.is_some_and(|s| s.alternative));
        let ims = topology
            .presentations()
            .iter()
            .filter(associated)
            .any(|p| p.presentation_version == 2);
        for info in group.substreams() {
            let Some(first) = info.substream_index() else {
                continue;
            };
            let (ajoc, channel_mode) = match info {
                SubstreamInfo::Chan(c) => (false, Some(c.channel_mode.ch_mode)),
                SubstreamInfo::Ajoc(_) => (true, None),
                SubstreamInfo::Obj(_) => (false, None),
            };
            for offset in 0..group.frame_rate_factor {
                let Some(index) = first.checked_add(offset) else {
                    continue;
                };
                let candidate = SubstreamContext {
                    sus_ver: 1,
                    alternative,
                    ajoc,
                    channel_mode,
                    b_iframe: if group.frame_rate_factor == 1 || info.audio_ndot() {
                        Some(info.audio_ndot())
                    } else {
                        None
                    },
                    alternative_oamd: match info {
                        SubstreamInfo::Obj(info) if alternative => groups.iter().find(|g|usize::try_from(g.group_index)==Ok(group_index)).and_then(|g|g.effective_timing).and_then(|t|macindecode_ac4_bitstream::AlternativeOamdContext::from_object_substream(info,t.num_obj_info_blocks).ok()),
                        _=>None,
                    },
                };
                let slot = contexts.entry(index).or_default();
                if !slot.contains(&candidate) {
                    slot.push(candidate);
                }
                if ims && matches!(channel_mode, Some(5 | 6)) {
                    let stereo = SubstreamContext {
                        channel_mode: Some(1),
                        ..candidate
                    };
                    if !slot.contains(&stereo) {
                        slot.push(stereo);
                    }
                }
            }
        }
    }
}
pub fn same_audio_context_family(left: SubstreamContext, right: SubstreamContext) -> bool {
    left.sus_ver == right.sus_ver
        && left.alternative == right.alternative
        && left.ajoc == right.ajoc
        && left.channel_mode == right.channel_mode
        && match (left.alternative_oamd, right.alternative_oamd) {
            (Some(a), Some(b)) => a.objects() == b.objects(),
            (None, None) => true,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, vec};

    #[allow(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "构造已知有界测试位串"
    )]
    fn pack(bits: &str) -> Vec<u8> {
        let bits: Vec<_> = bits.bytes().filter(|b| matches!(b, b'0' | b'1')).collect();
        let mut bytes = vec![0; bits.len().div_ceil(8)];
        for (i, b) in bits.into_iter().enumerate() {
            if b == b'1' {
                bytes[i / 8] |= 1 << (7 - i % 8);
            }
        }
        bytes
    }
    fn frame(sequence: u16, independent: bool, presentation: &[u8], audio: &[u8]) -> Vec<u8> {
        let header = format!(
            "10 {sequence:010b} 0 1 1101 {} 1 0 0 1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00 1 0 1 1 10 0 0 1 01 0 10 0 {:010b} 0 {:010b}",
            u8::from(independent),
            presentation.len(),
            audio.len()
        );
        let mut raw = pack(&header);
        raw.extend_from_slice(presentation);
        raw.extend_from_slice(audio);
        raw
    }
    fn valid(sequence: u16, independent: bool) -> Vec<u8> {
        frame(sequence, independent, &[0x55, 0x04, 0], &[0, 0, 0, 0x20])
    }
    fn session() -> Ac4MetadataSession {
        Ac4MetadataSession::new(Ac4MetadataConfig::new()).unwrap()
    }

    fn shared_oamd_frame(
        ndot: [bool; 3],
        separate_group: usize,
        shared: &[u8],
        separate: &[u8],
    ) -> (Vec<u8>, Ac4Topology) {
        // config 3 引用三个 direct-object group；两组共享 OAMD 2，另一组使用 OAMD 3。
        // 三组引用同一 audio 1，presentation 0/audio 1 留空，仅测试 group OAMD 事务。
        let presentation = "0 011 0 000 0 00 000 0 00 00 0 0 000 001 010 0 0 0 1 00";
        let groups = ndot
            .into_iter()
            .enumerate()
            .map(|(index, independent)| {
                let physical = if index == separate_group {
                    "11 00 0"
                } else {
                    "10"
                };
                format!(
                    "1 0 1 0 1 {} {physical} 0 001 1 0 0 0 1 01 0",
                    u8::from(independent)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut raw = pack(&format!(
            "10 0000000000 0 1 1101 1 1 0 0 {presentation} {groups} 00 00 0 0 0000000000 0 0000000000 0 {:010b} 0 {:010b}",
            shared.len(),
            separate.len()
        ));
        raw.extend_from_slice(shared);
        raw.extend_from_slice(separate);
        let topology = Ac4Topology::parse(&raw).unwrap();
        assert_eq!(topology.groups().len(), 3);
        macindecode_ac4_bitstream::topology::validate_substream_references(&topology).unwrap();
        (raw, topology)
    }

    fn observe_group_frame(
        session: &mut Ac4MetadataSession,
        frame: (Vec<u8>, Ac4Topology),
        index: u64,
    ) {
        session.output.clear();
        session.output.context = AccessUnitContext::new(index);
        session.observe_groups(
            &frame.0,
            &frame.1,
            7,
            MetadataErrorContext::for_access_unit(index),
        );
    }

    fn observed_object_x(session: &Ac4MetadataSession, substream_index: u32) -> u8 {
        session
            .output
            .objects
            .iter()
            .find(|object| object.substream_index == substream_index)
            .unwrap()
            .state
            .render
            .unwrap()
            .position
            .x
    }

    #[test]
    fn shared_oamd_delta_commits_once_when_an_independent_group_fails() {
        let absolute = pack("0 1 0 001 000000 00 0 1 001010 011111 1 0000 1 1 0");
        let delta = pack("0 0 0 0 1 0 0 1 001 000 000 1 1 0");
        let reuse = pack("0 0 0 1 1 0");
        for separate_group in 0..3 {
            let mut session = session();
            observe_group_frame(
                &mut session,
                shared_oamd_frame([true; 3], separate_group, &absolute, &absolute),
                0,
            );
            assert!(session.output.diagnostics.is_empty());
            assert_eq!(observed_object_x(&session, 2), 10);

            observe_group_frame(
                &mut session,
                shared_oamd_frame([false; 3], separate_group, &delta, &[0xff]),
                1,
            );
            assert_eq!(session.output.groups.len(), 2);
            assert_eq!(session.output.objects.len(), 1);
            assert_eq!(session.output.updates.len(), 1);
            assert_eq!(session.output.diagnostics.len(), 1);
            assert_eq!(
                session
                    .output
                    .diagnostics
                    .first()
                    .unwrap()
                    .error
                    .context()
                    .group_index,
                u32::try_from(separate_group).ok()
            );
            assert_eq!(observed_object_x(&session, 2), 11);

            observe_group_frame(
                &mut session,
                shared_oamd_frame([false; 3], separate_group, &reuse, &[0xff]),
                2,
            );
            assert_eq!(
                observed_object_x(&session, 2),
                11,
                "下一 AU 必须继承只应用一次差分的物理 OAMD 状态"
            );
        }
    }

    #[test]
    fn failed_shared_oamd_invalidates_both_groups_and_preserves_the_other_source() {
        let absolute = pack("0 1 0 001 000000 00 0 1 001010 011111 1 0000 1 1 0");
        let delta = pack("0 0 0 0 1 0 0 1 001 000 000 1 1 0");
        let reuse = pack("0 0 0 1 1 0");
        let mut session = session();
        observe_group_frame(
            &mut session,
            shared_oamd_frame([true; 3], 2, &absolute, &absolute),
            0,
        );
        observe_group_frame(
            &mut session,
            shared_oamd_frame([false; 3], 2, &[0xff], &delta),
            1,
        );
        assert_eq!(session.output.groups.len(), 1);
        assert_eq!(session.output.objects.len(), 1);
        assert_eq!(session.output.diagnostics.len(), 2);
        assert_eq!(observed_object_x(&session, 3), 11);

        observe_group_frame(
            &mut session,
            shared_oamd_frame([false; 3], 2, &reuse, &reuse),
            2,
        );
        assert_eq!(session.output.objects.len(), 1);
        assert_eq!(session.output.diagnostics.len(), 2);
        assert_eq!(observed_object_x(&session, 3), 11);
    }

    #[test]
    fn shared_oamd_context_conflict_is_detected_after_an_unrelated_failure() {
        let absolute = pack("0 1 0 001 000000 00 0 1 001010 011111 1 0000 1 1 0");
        let delta = pack("0 0 0 0 1 0 0 1 001 000 000 1 1 0");
        let mut session = session();
        observe_group_frame(
            &mut session,
            shared_oamd_frame([true; 3], 0, &absolute, &absolute),
            0,
        );
        // group 0 先失败；group 1/2 随后暴露同一物理 OAMD 的 ndot 冲突。
        observe_group_frame(
            &mut session,
            shared_oamd_frame([false, false, true], 0, &delta, &[0xff]),
            1,
        );
        assert!(session.output.groups.is_empty());
        assert!(session.output.objects.is_empty());
        assert!(session.output.updates.is_empty());
        assert_eq!(
            session
                .output
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.error.kind() == MetadataErrorKind::ContextConflict)
                .count(),
            2
        );
    }

    #[test]
    fn basic_observes_presentation_audio_and_integer_source_time() {
        let mut session = session();
        let raw = valid(0, true);
        let au = session
            .observe_access_unit(AccessUnit::new(
                &raw,
                AccessUnitContext::new(10).with_source_sample_start(400),
            ))
            .unwrap();
        assert_eq!(au.status(), MetadataStatus::Ready);
        assert_eq!(au.source_sample_start(), Some(400));
        assert_eq!(au.configuration_generation(), 1);
        assert_eq!(au.presentations().len(), 1);
        assert_eq!(au.audio_metadata().len(), 1);
        assert_eq!(
            au.presentation_metadata(0)
                .unwrap()
                .parsed_substream()
                .unwrap()
                .dialnorm_bits,
            85
        );
        assert!(au.audio_metadata().first().unwrap().result.is_ok());
        assert!(au.objects().is_empty());
        let raw = valid(1, false);
        let next = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(11)))
            .unwrap();
        assert_eq!(next.source_sample_start(), Some(2448));
        assert_eq!(next.status(), MetadataStatus::Ready);
    }
    #[test]
    fn malformed_partition_preserves_other_verified_partitions() {
        let mut session = session();
        let raw = frame(0, true, &[0x55, 4, 0], &[0xff; 4]);
        let au = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(0)))
            .unwrap();
        assert_eq!(au.status(), MetadataStatus::Partial);
        assert!(au.presentation_metadata(0).is_some());
        assert!(au.audio_metadata().first().unwrap().result.is_err());
        assert!(
            au.diagnostics()
                .iter()
                .any(|d| d.partition == MetadataPartition::Audio)
        );
        let raw = frame(1, true, &[0xff; 3], &[0, 0, 0, 0x20]);
        let au = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(1)))
            .unwrap();
        assert!(au.presentation_metadata(0).is_none());
        assert!(au.audio_metadata().first().unwrap().result.is_ok());
        let raw = valid(2, true);
        assert_eq!(
            session
                .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(2)))
                .unwrap()
                .status(),
            MetadataStatus::Ready
        );
    }
    #[test]
    fn prefix_retry_does_not_advance_committed_control_or_time() {
        let mut session = session();
        let raw = valid(0, true);
        session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(0)))
            .unwrap();
        let before = (session.control, session.cursor);
        let error = session
            .observe_access_unit(AccessUnit::new(&[0], AccessUnitContext::new(1)))
            .unwrap_err();
        assert_eq!(error.kind(), MetadataErrorKind::NeedMoreData);
        assert_eq!((session.control, session.cursor), before);
        let raw = valid(1, false);
        assert_eq!(
            session
                .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(1)))
                .unwrap()
                .source_sample_start(),
            Some(2048)
        );
    }
    #[test]
    fn bounded_payload_overrun_invalidates_history_and_waits() {
        let mut session = session();
        let mut raw = valid(0, true);
        session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(0)))
            .unwrap();
        raw.pop();
        let error = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(1)))
            .unwrap_err();
        assert!(matches!(error.kind(), MetadataErrorKind::Topology(_)));
        assert!(session.control.is_waiting_for_random_access());
        let raw = valid(1, false);
        let au = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(2)))
            .unwrap();
        assert_eq!(au.status(), MetadataStatus::WaitingForRandomAccess);
        assert!(au.topology().is_some());
        assert!(au.presentation_metadata(0).is_none());
    }
    #[test]
    fn discontinuity_and_reset_create_explicit_observation_domains() {
        let mut session = session();
        let raw = valid(0, true);
        let first = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(0)))
            .unwrap()
            .observation_epoch();
        session.mark_discontinuity();
        let raw = valid(1, false);
        let au = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(1)))
            .unwrap();
        assert_eq!(au.status(), MetadataStatus::WaitingForRandomAccess);
        assert_eq!(au.source_sample_start(), None);
        let waiting = au.observation_epoch();
        let raw = valid(2, false);
        assert_eq!(
            session
                .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(2)))
                .unwrap()
                .observation_epoch(),
            waiting
        );
        session.reset();
        let raw = valid(0, true);
        let au = session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(3)))
            .unwrap();
        assert!(au.observation_epoch() > first);
        assert_eq!(au.source_sample_start(), Some(0));
    }
    #[test]
    fn source_time_overflow_does_not_commit_a_candidate() {
        let mut session = session();
        let raw = valid(0, true);
        let error = session
            .observe_access_unit(AccessUnit::new(
                &raw,
                AccessUnitContext::new(0).with_source_sample_start(i64::MAX),
            ))
            .unwrap_err();
        assert_eq!(error.kind(), MetadataErrorKind::TimelineOverflow);
        assert_eq!(session.control.generation(), 0);
    }
    #[test]
    fn selection_failure_keeps_configuration_uncommitted() {
        let mut session = Ac4MetadataSession::new(
            Ac4MetadataConfig::new()
                .with_presentations(PresentationScope::Selected(PresentationSelection::Id(999))),
        )
        .unwrap();
        let raw = valid(0, true);
        assert!(matches!(
            session
                .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(0)))
                .unwrap_err()
                .kind(),
            MetadataErrorKind::PresentationSelection(
                crate::selection::PresentationSelectionError::IdNotFound { requested: 999 }
            )
        ));
        assert_eq!(session.control.generation(), 0);
    }
    #[test]
    fn stable_configuration_reuses_payload_and_context_storage() {
        let mut session = session();
        let raw = valid(0, true);
        session
            .observe_access_unit(AccessUnit::new(&raw, AccessUnitContext::new(0)))
            .unwrap();
        let raw_address = session.output.raw.as_ptr();
        let context_address = session.audio_candidates.get(&1).unwrap().as_ptr();
        let view_address = session
            .presentations
            .get(&0)
            .unwrap()
            .state
            .storage
            .view()
            .unwrap()
            .payload()
            .as_ptr();
        for sequence in 1..32 {
            let raw = valid(sequence, true);
            session
                .observe_access_unit(AccessUnit::new(
                    &raw,
                    AccessUnitContext::new(u64::from(sequence)),
                ))
                .unwrap();
            assert_eq!(session.output.raw.as_ptr(), raw_address);
            assert_eq!(
                session.audio_candidates.get(&1).unwrap().as_ptr(),
                context_address
            );
            assert_eq!(
                session
                    .presentations
                    .get(&0)
                    .unwrap()
                    .state
                    .storage
                    .view()
                    .unwrap()
                    .payload()
                    .as_ptr(),
                view_address
            );
        }
    }
    #[cfg(not(feature = "metadata-decode"))]
    #[test]
    fn full_detail_requires_the_independent_backend_feature() {
        assert_eq!(
            Ac4MetadataSession::new(Ac4MetadataConfig::new().with_detail(MetadataDetail::Full))
                .unwrap_err()
                .kind(),
            MetadataErrorKind::FeatureUnavailable
        );
    }
}
