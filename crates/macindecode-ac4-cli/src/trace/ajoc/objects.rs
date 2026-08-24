//! 统一 engine 的 raw OAMD trace observation、跨帧状态与轨迹投影。

#[cfg(feature = "audio-decode")]
use super::{
    Ac4SubstreamAjoc, AjocTrace, AjocTraceContext, DecodeMode, EffectiveSceneContext,
    MAX_OAMD_OBJECTS, MAX_POSITION_TIMELINE, MAX_SUBSTREAMS, OamdMetadataBlock, OamdState,
    OamdTimingData, PositionChange, resolve_oamd_blocks_timed, shared_scene_timing,
};

#[cfg(feature = "audio-decode")]
impl AjocTrace {
    /// 应用统一 engine 同一语法 observation 借出的 core/full OAMD 块。
    ///
    /// 两侧状态先在局部值中完整解析，任一侧失败均不提交半帧。
    pub(in crate::trace) fn apply_object_states_from_blocks(
        &mut self,
        parsed: &Ac4SubstreamAjoc,
        dmx_blocks: &[OamdMetadataBlock],
        umx_blocks: &[OamdMetadataBlock],
        candidate: AjocTraceContext,
        substream_index: u32,
        frame_index: u32,
    ) -> Result<(), String> {
        let index = usize::try_from(substream_index).unwrap_or(usize::MAX);
        if self.dmx_objects.len() < MAX_SUBSTREAMS || self.umx_objects.len() < MAX_SUBSTREAMS {
            self.dmx_objects.resize(MAX_SUBSTREAMS, OamdState::new());
            self.umx_objects.resize(MAX_SUBSTREAMS, OamdState::new());
        }
        if self.dmx_audio_timing.len() < MAX_SUBSTREAMS
            || self.umx_audio_timing.len() < MAX_SUBSTREAMS
        {
            self.dmx_audio_timing.resize(MAX_SUBSTREAMS, None);
            self.umx_audio_timing.resize(MAX_SUBSTREAMS, None);
        }
        let previous_dmx = self.dmx_audio_timing.get(index).copied().flatten();
        let previous_umx = self.umx_audio_timing.get(index).copied().flatten();
        let mut scene_contexts = Vec::with_capacity(candidate.scene_contexts().len());
        for scene in candidate.scene_contexts() {
            let dmx_timing = parsed
                .audio
                .dmx_timing
                .or(scene.group_oamd.timing)
                .or(previous_dmx);
            let umx_timing = parsed.audio.umx_timing.or_else(|| {
                if parsed.audio.derive_timing_from_dmx == Some(true) {
                    dmx_timing
                } else {
                    scene.group_oamd.timing.or(previous_umx)
                }
            });
            if dmx_timing.is_some_and(|timing| {
                timing.num_obj_info_blocks != parsed.audio.dmx_num_obj_info_blocks
            }) || umx_timing.is_some_and(|timing| {
                timing.num_obj_info_blocks != parsed.audio.umx_num_obj_info_blocks
            }) {
                return Err(
                    "A-JOC effective-timing block count does not match dynamic data".to_owned(),
                );
            }
            scene_contexts.push(EffectiveSceneContext {
                dmx_timing,
                umx_timing,
            });
        }
        let dmx_shared_timing = shared_scene_timing(&scene_contexts, DecodeMode::Core);
        let umx_shared_timing = shared_scene_timing(&scene_contexts, DecodeMode::Full);

        self.apply_object_block_batches(
            substream_index,
            frame_index,
            dmx_blocks,
            parsed.audio.dmx_num_obj_info_blocks,
            dmx_shared_timing,
            umx_blocks,
            parsed.audio.umx_num_obj_info_blocks,
            umx_shared_timing,
        )?;
        if let Some(slot) = self.dmx_audio_timing.get_mut(index) {
            *slot = dmx_shared_timing;
        }
        if let Some(slot) = self.umx_audio_timing.get_mut(index) {
            *slot = umx_shared_timing;
        }
        Ok(())
    }

    /// 应用同一帧的 core/full 对象块；任一侧失败时两侧状态与统计均不提交。
    #[expect(
        clippy::too_many_arguments,
        reason = "core and full transactional inputs must be supplied together"
    )]
    fn apply_object_block_batches(
        &mut self,
        substream_index: u32,
        frame_index: u32,
        dmx: &[OamdMetadataBlock],
        dmx_num_obj_info_blocks: u8,
        dmx_timing: Option<OamdTimingData>,
        umx: &[OamdMetadataBlock],
        umx_num_obj_info_blocks: u8,
        umx_timing: Option<OamdTimingData>,
    ) -> Result<(), String> {
        let index = usize::try_from(substream_index).unwrap_or(usize::MAX);
        let dmx_initial =
            self.dmx_objects.get(index).copied().ok_or_else(|| {
                "A-JOC substream index exceeds core-object state capacity".to_owned()
            })?;
        let umx_initial =
            self.umx_objects.get(index).copied().ok_or_else(|| {
                "A-JOC substream index exceeds full-object state capacity".to_owned()
            })?;
        let (next_dmx, dmx_stats) =
            resolve_oamd_blocks_timed(dmx_initial, dmx, dmx_num_obj_info_blocks, dmx_timing)
                .map_err(|error| format!("Core state continuation failed: {error}"))?;
        let (next_umx, umx_stats) =
            resolve_oamd_blocks_timed(umx_initial, umx, umx_num_obj_info_blocks, umx_timing)
                .map_err(|error| format!("Full state continuation failed: {error}"))?;

        let (Some(dmx_state), Some(umx_state)) = (
            self.dmx_objects.get_mut(index),
            self.umx_objects.get_mut(index),
        ) else {
            return Err("A-JOC substream index exceeds object-state capacity".to_owned());
        };
        *dmx_state = next_dmx;
        *umx_state = next_umx;

        self.differential_positions = self
            .differential_positions
            .saturating_add(dmx_stats.differential_positions)
            .saturating_add(umx_stats.differential_positions);
        self.position_changes = self
            .position_changes
            .saturating_add(u64::try_from(dmx_stats.position_changes.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(umx_stats.position_changes.len()).unwrap_or(u64::MAX));
        self.record_position_timeline(substream_index, frame_index, &umx_stats.position_changes);
        self.remember_first_positions(index, substream_index, frame_index);
        Ok(())
    }

    pub(in crate::trace) fn record_position_timeline(
        &mut self,
        substream_index: u32,
        frame_index: u32,
        changes: &[PositionChange],
    ) {
        for change in changes {
            if self.position_timeline.len() >= MAX_POSITION_TIMELINE {
                self.timeline_truncated = true;
                break;
            }
            self.position_timeline.push(format!(
                "{{\"frame\": {frame_index}, \"substream\": {substream_index}, \
                 \"object\": {}, \"block\": {}, \"x\": {}, \"y\": {}, \"z\": {}}}",
                change.object_index, change.block_index, change.x, change.y, change.z
            ));
        }
    }

    pub(in crate::trace) fn remember_first_positions(
        &mut self,
        index: usize,
        substream_index: u32,
        frame_index: u32,
    ) {
        if self.first_positions.len() < MAX_SUBSTREAMS {
            self.first_positions.resize_with(MAX_SUBSTREAMS, || None);
        }
        if self.first_positions.get(index).is_some_and(Option::is_none) {
            let positions = self.upmix_positions_json(index, substream_index, frame_index);
            if let Some(slot) = self.first_positions.get_mut(index) {
                *slot = Some(positions);
            }
        }
    }

    /// 一条物理 substream 首次成功解析时的上混侧位置。
    pub(in crate::trace) fn upmix_positions_json(
        &self,
        index: usize,
        substream_index: u32,
        frame_index: u32,
    ) -> String {
        let Some(state) = self.umx_objects.get(index) else {
            return "null".to_owned();
        };
        let mut items = Vec::new();
        for object in 0..MAX_OAMD_OBJECTS {
            let Some(item) = state.object(object) else {
                break;
            };
            let Some(render) = item.render else {
                continue;
            };
            items.push(format!(
                "{{\"object\": {object}, \"active\": {}, \"x\": {}, \"y\": {}, \"z\": {}}}",
                item.active, render.position.x, render.position.y, render.position.z
            ));
        }
        format!(
            "{{\"frame\": {frame_index}, \"substream\": {substream_index}, \
             \"objects\": [{}]}}",
            items.join(", ")
        )
    }
}
