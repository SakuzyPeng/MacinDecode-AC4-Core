//! A-JOC trace 的逐帧上下文路由和统一 engine 调用。

#[cfg(feature = "audio-decode")]
use super::{
    Ac4Topology, AjocContextSlot, AjocObservation, AjocSubstreamContext, AjocTrace,
    AjocTraceContext, FrameTally, FullAjocDecoder, GroupOamdState, MAX_SUBSTREAMS,
    ReconstructionInvariant, ScaledStats, SubstreamInfo, group_frame_rate_fraction,
    group_is_alternative,
};

/// 一次物理 substream 观察所处的帧级上下文。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy)]
struct FrameObservationContext {
    index: u32,
    start_samples: i64,
    physical_substreams: usize,
}

#[cfg(feature = "audio-decode")]
impl AjocTrace {
    /// 全部被违反的重建不变量，按 [`ReconstructionInvariant::ALL`] 的顺序。
    pub(in crate::trace) fn invariant_violations(
        &self,
    ) -> impl Iterator<Item = (ReconstructionInvariant, String)> {
        ReconstructionInvariant::ALL
            .iter()
            .copied()
            .filter_map(|kind| kind.violation(self).map(|detail| (kind, detail)))
    }

    pub(in crate::trace) fn new() -> Self {
        Self {
            frames: 0,
            parsed: 0,
            substreams: 0,
            parsed_substreams: 0,
            failures: 0,
            first_error: None,
            min_fill_bits: None,
            max_fill_bits: None,
            max_dmx_blocks: 0,
            max_umx_blocks: 0,
            dmx_blocks: 0,
            umx_blocks: 0,
            derive_timing: 0,
            inactive_signals: 0,
            bed_info: 0,
            companding_frames: 0,
            companding_active_frames: 0,
            aspx_add_harmonic_frames: 0,
            aspx_interleaved_frames: 0,
            aspx_variable_framing_frames: 0,
            aspx_balance_frames: 0,
            ajoc_matrix: Default::default(),
            intra_frame_update_frames: 0,
            position_changes: 0,
            differential_positions: 0,
            state_failures: 0,
            scale_factor_bands: 0,
            scale_factor_min: None,
            scale_factor_max: None,
            scale_factor_failures: 0,
            scale_factor_first_error: None,
            scaled_stats: ScaledStats::default(),
            dmx_objects: Vec::new(),
            umx_objects: Vec::new(),
            dmx_audio_timing: Vec::new(),
            umx_audio_timing: Vec::new(),
            full_decoder: Some(FullAjocDecoder::new()),
            aspx_failures: 0,
            aspx_first_error: None,
            ajoc_full_warmup_frames: 0,
            ajoc_full_reconstructed_frames: 0,
            ajoc_full_wet_frames: 0,
            ajoc_reconstruction_failures: 0,
            ajoc_reconstruction_first_error: None,
            objects_nonfinite: 0,
            objects_nonfinite_first_error: None,
            object_shape_mismatches: 0,
            object_shape_first_error: None,
            aspx_unsupported_first_error: None,
            full_unsupported_first_error: None,
            first_detail: None,
            first_positions: Vec::new(),
            position_timeline: Vec::new(),
            timeline_truncated: false,
        }
    }

    /// feature trace 执行 full/wet DSP，但只累计 observation，不留存 PCM。
    pub(in crate::trace) fn new_tracing() -> Self {
        Self::new()
    }

    /// `group_oamd` 是 `OamdTrace::observe` 得到的逐 group 公共数据与 timing；
    /// A-JOC 元素自身未携带 timing 时沿用对应 group 的块数。
    pub(in crate::trace) fn observe_at(
        &mut self,
        frame: &[u8],
        topology: &Ac4Topology,
        index: u32,
        group_oamd: &[GroupOamdState],
        frame_start_samples: i64,
    ) {
        let mut contexts = [AjocContextSlot::Empty; MAX_SUBSTREAMS];
        let mut seen = false;
        let mut frame_failed = false;

        for (group_index, group) in topology.groups().iter().enumerate() {
            let alternative = group_is_alternative(topology, group_index);
            let frame_rate_fraction =
                group_frame_rate_fraction(topology, group_index, group.frame_rate_factor);
            let group_oamd = group_oamd.get(group_index).copied().unwrap_or_default();
            for info in group.substreams() {
                let SubstreamInfo::Ajoc(ref ajoc) = *info else {
                    continue;
                };
                let Some(substream_index) = ajoc.substream_index() else {
                    continue;
                };
                seen = true;

                let slot_index = usize::try_from(substream_index).unwrap_or(usize::MAX);
                let Some(slot) = contexts.get_mut(slot_index) else {
                    frame_failed = true;
                    self.remember_failure(index, "A-JOC substream 下标超出统计容量");
                    continue;
                };
                let frame_rate_fraction = match frame_rate_fraction {
                    Ok(value) => value,
                    Err(error) => {
                        *slot = AjocContextSlot::Conflict;
                        self.reset_substream(substream_index);
                        frame_failed = true;
                        self.remember_failure(index, error);
                        continue;
                    }
                };
                let candidate = AjocTraceContext::new(
                    *ajoc,
                    group.frame_rate_factor,
                    frame_rate_fraction,
                    alternative,
                    group_oamd,
                );
                let conflict = match *slot {
                    AjocContextSlot::Empty => {
                        *slot = AjocContextSlot::Ready(candidate);
                        false
                    }
                    AjocContextSlot::Ready(mut current) => {
                        if current.merge(candidate) {
                            *slot = AjocContextSlot::Ready(current);
                            false
                        } else {
                            *slot = AjocContextSlot::Conflict;
                            true
                        }
                    }
                    AjocContextSlot::Conflict => true,
                };
                if conflict {
                    self.reset_substream(substream_index);
                    frame_failed = true;
                    self.remember_failure(index, "同一 A-JOC substream 的解析上下文冲突");
                }
            }
        }

        if !seen {
            return;
        }

        let mut tally = FrameTally {
            declared: contexts
                .iter()
                .filter(|slot| !matches!(slot, AjocContextSlot::Empty))
                .count(),
            failed: frame_failed,
            ..FrameTally::default()
        };
        let location = FrameObservationContext {
            index,
            start_samples: frame_start_samples,
            physical_substreams: tally.declared,
        };
        for (substream_index, slot) in contexts.iter().copied().enumerate() {
            let AjocContextSlot::Ready(context) = slot else {
                continue;
            };
            let substream_index = u32::try_from(substream_index).unwrap_or(u32::MAX);
            tally.observe(self.observe_one(frame, topology, context, substream_index, location));
        }
        self.commit_frame(tally);
    }

    /// 提交一帧的跨 substream 统计，见 [`FrameTally`] 对两套单位的说明。
    pub(in crate::trace) fn commit_frame(&mut self, tally: FrameTally) {
        self.frames = self.frames.saturating_add(1);
        self.substreams = self
            .substreams
            .saturating_add(u32::try_from(tally.declared).unwrap_or(u32::MAX));
        self.parsed_substreams = self
            .parsed_substreams
            .saturating_add(u32::try_from(tally.parsed).unwrap_or(u32::MAX));
        if tally.intra_frame_updates {
            self.intra_frame_update_frames = self.intra_frame_update_frames.saturating_add(1);
        }
        if tally.companding_present {
            self.companding_frames = self.companding_frames.saturating_add(1);
        }
        if tally.companding_active {
            self.companding_active_frames = self.companding_active_frames.saturating_add(1);
        }
        for (flag, counter) in [
            (
                tally.aspx.add_harmonic(),
                &mut self.aspx_add_harmonic_frames,
            ),
            (tally.aspx.interleaved(), &mut self.aspx_interleaved_frames),
            (
                tally.aspx.variable_framing(),
                &mut self.aspx_variable_framing_frames,
            ),
            (tally.aspx.balance(), &mut self.aspx_balance_frames),
        ] {
            if flag {
                *counter = counter.saturating_add(1);
            }
        }
        if !tally.failed && tally.parsed == tally.declared {
            self.parsed = self.parsed.saturating_add(1);
        } else {
            self.failures = self.failures.saturating_add(1);
        }
    }

    fn observe_one(
        &mut self,
        raw_frame: &[u8],
        topology: &Ac4Topology,
        candidate: AjocTraceContext,
        substream_index: u32,
        location: FrameObservationContext,
    ) -> Option<AjocObservation> {
        let context = match AjocSubstreamContext::derive(
            &topology.toc,
            &candidate.info,
            candidate.frame_rate_factor,
            candidate.frame_rate_fraction,
            candidate.alternative,
            candidate.group_num_obj_info_blocks,
        ) {
            Ok(context) => context,
            Err(error) => {
                self.reset_substream(substream_index);
                self.remember_failure(location.index, &format!("{error}"));
                return None;
            }
        };
        let payload = match topology.substream_payload(raw_frame, substream_index) {
            Ok(payload) => payload,
            Err(error) => {
                self.reset_substream(substream_index);
                self.remember_failure(location.index, &format!("定位失败：{error}"));
                return None;
            }
        };

        self.observe_traced_audio(
            payload,
            context,
            candidate,
            substream_index,
            location.index,
            location.start_samples,
            location.physical_substreams,
            candidate.info.lfe_reinsertion_position(),
        )
    }

    pub(in crate::trace) fn reset_substream(&mut self, substream_index: u32) {
        if let Some(decoder) = self.full_decoder.as_mut() {
            decoder.reset_substream(substream_index);
        }
        let index = usize::try_from(substream_index).unwrap_or(usize::MAX);
        if let Some(state) = self.dmx_objects.get_mut(index) {
            state.reset();
        }
        if let Some(state) = self.umx_objects.get_mut(index) {
            state.reset();
        }
        if let Some(timing) = self.dmx_audio_timing.get_mut(index) {
            *timing = None;
        }
        if let Some(timing) = self.umx_audio_timing.get_mut(index) {
            *timing = None;
        }
    }

    pub(in crate::trace) fn reset_history(&mut self) {
        if let Some(decoder) = self.full_decoder.as_mut() {
            decoder.reset();
        }
        for state in self
            .dmx_objects
            .iter_mut()
            .chain(self.umx_objects.iter_mut())
        {
            state.reset();
        }
        self.dmx_audio_timing.fill(None);
        self.umx_audio_timing.fill(None);
    }
}
