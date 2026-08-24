//! OAMD trace 与逐对象位置统计。
//!
//! 按 OAMD substream 隔离状态机，统计块数、时间数据与逐帧位置变化。

use super::{
    Ac4Topology, GroupCommonState, GroupOamdState, MAX_SUBSTREAM_GROUPS, MAX_SUBSTREAMS,
    OamdContext, OamdState, OamdSubstreamPayload, ObjectDescriptors, SubstreamInfo, timing_json,
};
#[cfg(feature = "audio-decode")]
use super::{OamdMetadataBlock, OamdStateError, OamdTimingData};

/// `position_timeline` 的容量。
///
/// 原值 64 是按合成探针向量定的——那种素材每段边界加编码器的数帧过渡，总共
/// 只有几十次变化。真实素材推翻了这个前提：157 秒、19 个动态对象的音乐母版
/// 产生 596 次变化，64 只留下 11 %，而同一份 JSON 里 `position_changes` 报的
/// 是全量，两个数并排容易被读成同一回事。
///
/// 取 4 096，覆盖实测值约七倍。超出仍置位 `truncated` 而不是静默截断——上限
/// 是有的，只是不再由最小的那类素材决定。
#[cfg(feature = "audio-decode")]
pub(crate) const MAX_POSITION_TIMELINE: usize = 4096;

/// 一个块应用后的位置变化。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PositionChange {
    pub(super) object_index: u8,
    pub(super) block_index: u8,
    pub(super) x: u8,
    pub(super) y: u8,
    pub(super) z: i8,
}

/// 整批 OAMD 块在提交前产生的统计。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Default)]
pub(crate) struct OamdBlockStats {
    pub(super) differential_positions: u64,
    pub(super) position_changes: Vec<PositionChange>,
}

/// 在临时状态上逐块解算，整批成功后由调用方提交。
#[cfg(feature = "audio-decode")]
pub(crate) fn resolve_oamd_blocks(
    initial: OamdState,
    blocks: &[OamdMetadataBlock],
    num_obj_info_blocks: u8,
) -> Result<(OamdState, OamdBlockStats), OamdStateError> {
    let mut next = initial;
    let mut stats = OamdBlockStats::default();
    for block in blocks {
        let object = usize::from(block.object_index);
        let old = next.object(object).and_then(|item| item.render);
        next.apply_blocks(core::slice::from_ref(block), None)?;
        let new = next.object(object).and_then(|item| item.render);

        if block.info.diff_pos_coding {
            stats.differential_positions = stats.differential_positions.saturating_add(1);
        }
        if let (Some(old), Some(new)) = (old, new) {
            if old.position.x != new.position.x
                || old.position.y != new.position.y
                || old.position.z != new.position.z
            {
                stats.position_changes.push(PositionChange {
                    object_index: block.object_index,
                    block_index: block.block_index,
                    x: new.position.x,
                    y: new.position.y,
                    z: new.position.z,
                });
            }
        }
    }
    next.apply_blocks(&[], Some(num_obj_info_blocks))?;
    Ok((next, stats))
}

#[cfg(feature = "audio-decode")]
pub(crate) fn resolve_oamd_blocks_timed(
    initial: OamdState,
    blocks: &[OamdMetadataBlock],
    num_obj_info_blocks: u8,
    timing: Option<OamdTimingData>,
) -> Result<(OamdState, OamdBlockStats), OamdStateError> {
    let (mut next, stats) = resolve_oamd_blocks(initial, blocks, num_obj_info_blocks)?;
    if let Some(timing) = timing {
        next.apply_frame(&[], None, Some(timing))?;
    }
    Ok((next, stats))
}

/// 载荷侧 `oamd_substream()` 的统计。
///
/// A-JOC 路径下逐对象动态数据不在此处，见 `TS103190-2:v1.3.1` 表 7。
#[derive(Debug, Default)]
pub(crate) struct OamdTrace {
    /// 存在 OAMD substream 且成功定位载荷的帧数。
    pub(super) located: u32,
    /// 成功解析的帧数。
    pub(super) parsed: u32,
    /// 解析失败的帧数。
    pub(super) failures: u32,
    pub(super) first_error: Option<String>,
    /// 携带 `oamd_common_data()` 的帧数。
    pub(super) common_data_frames: u32,
    /// OAMD 适用帧中，`oamd_common_data()` 出现集合与容器 `stss` 的逐帧失配数。
    pub(super) common_data_sync_mismatches: u32,
    /// 携带 `oamd_timing_data()` 的帧数。
    pub(super) timing_frames: u32,
    /// 未携带时间数据、沿用前序 `num_obj_info_blocks` 的帧数。
    pub(super) timing_carryover_frames: u32,
    /// `byte_align` 残余比特的最大值；必须始终小于 8。
    pub(super) max_align_bits: u32,
    /// `oamd_dyndata_multi` 中出现的 `object_info_block` 总数。
    pub(super) dyndata_blocks: u32,
    /// 其中依赖前序状态的块数。
    pub(super) history_dependent_blocks: u32,
    /// 观察到的 `num_obj_info_blocks` 最小值与最大值。
    pub(super) min_blocks: Option<u8>,
    pub(super) max_blocks: Option<u8>,
    /// 观察到的最大 `block_offset_factor` 对应的采样偏移。
    pub(super) max_block_offset_samples: u32,
    /// 观察到的非零 `ramp_duration` 集合上界。
    pub(super) max_ramp_duration: u16,
    /// 按 OAMD substream 下标隔离的跨帧状态。
    pub(super) states: [OamdState; MAX_SUBSTREAMS],
    /// OAMD substream 与 A-JOC info 两个来源合并后的逐 group common 历史。
    pub(super) group_common: [GroupCommonState; MAX_SUBSTREAM_GROUPS],
    /// 首帧时间数据的可读摘要。
    pub(super) first_timing: Option<String>,
}

impl OamdTrace {
    pub(super) const fn new() -> Self {
        Self {
            located: 0,
            parsed: 0,
            failures: 0,
            first_error: None,
            common_data_frames: 0,
            common_data_sync_mismatches: 0,
            timing_frames: 0,
            timing_carryover_frames: 0,
            max_align_bits: 0,
            dyndata_blocks: 0,
            history_dependent_blocks: 0,
            min_blocks: None,
            max_blocks: None,
            max_block_offset_samples: 0,
            max_ramp_duration: 0,
            states: [OamdState::new(); MAX_SUBSTREAMS],
            group_common: [GroupCommonState {
                common: None,
                conflict: false,
            }; MAX_SUBSTREAM_GROUPS],
            first_timing: None,
        }
    }

    /// 解析本帧全部 OAMD substream，并返回逐 group 生效的公共数据与完整
    /// 时间数据。公共数据同时合并 A-JOC info 内嵌位置，并按 group 保留历史。
    ///
    /// A-JOC 的 `audio_data_ajoc()` 在自身未携带 timing 时要沿用 group 级的
    /// 这一份（`6.2.3.4`），故它不只是统计量，而是下游解析的输入。
    pub(super) fn observe(
        &mut self,
        frame: &[u8],
        topology: &Ac4Topology,
        index: u32,
        is_sync: Option<bool>,
    ) -> [GroupOamdState; MAX_SUBSTREAMS] {
        let mut substream_effective = [GroupOamdState::default(); MAX_SUBSTREAMS];
        let mut substream_current_common = [None; MAX_SUBSTREAMS];
        let mut seen_substreams = 0u32;
        let mut found = false;
        let mut frame_located = false;
        let mut frame_parsed = false;
        let mut frame_failed = false;
        let mut frame_common = false;
        let mut frame_timing = false;
        let mut frame_carryover = false;
        // channel-coded group 按 6.2.1.6 不携带 OAMD；只有显式 OAMD
        // substream 或可内嵌 common data 的 A-JOC group 才做 stss 成员比对。
        let oamd_applicable = topology
            .groups()
            .iter()
            .any(|group| group.oamd_substream.is_some() || group.has_ajoc());

        // `b_alternative` 表示码流中是否存在 alternative presentation。
        let alternative = topology.presentations().iter().any(|item| {
            item.substream
                .is_some_and(|substream| substream.alternative)
        });

        for group in topology.groups() {
            let Some(reference) = group.oamd_substream else {
                continue;
            };
            let Some(substream_index) = reference.substream_index else {
                continue;
            };
            found = true;
            let Some(mask) = 1u32.checked_shl(substream_index) else {
                frame_failed = true;
                self.remember_failure(index, "OAMD substream 下标超出状态容量");
                continue;
            };
            // 同一物理 OAMD substream 可以被多个 group 引用，只解析和统计一次。
            if seen_substreams & mask != 0 {
                continue;
            }
            seen_substreams |= mask;

            let payload = match topology.substream_payload(frame, substream_index) {
                Ok(payload) => payload,
                Err(error) => {
                    frame_failed = true;
                    self.reset_substream(substream_index);
                    self.remember_failure(index, &format!("定位失败：{error}"));
                    continue;
                }
            };
            frame_located = true;

            let descriptors = match ObjectDescriptors::from_group(group) {
                Ok(descriptors) => descriptors,
                Err(error) => {
                    frame_failed = true;
                    self.reset_substream(substream_index);
                    self.remember_failure(index, &format!("对象描述失败：{error}"));
                    continue;
                }
            };

            let state_index = usize::try_from(substream_index).unwrap_or(usize::MAX);
            let previous_num_obj_info_blocks = self
                .states
                .get(state_index)
                .and_then(OamdState::previous_num_obj_info_blocks);

            let context = OamdContext {
                objects: descriptors.as_slice(),
                b_alternative: alternative,
                b_oamd_ndot: reference.ndot,
                previous_num_obj_info_blocks,
            };
            let parsed = match OamdSubstreamPayload::parse(payload, context) {
                Ok(parsed) => parsed,
                Err(error) => {
                    frame_failed = true;
                    self.reset_substream(substream_index);
                    self.remember_failure(index, &format!("{error}"));
                    continue;
                }
            };
            let Some(state) = self.states.get_mut(state_index) else {
                frame_failed = true;
                self.remember_failure(index, "OAMD substream 下标超出状态容量");
                continue;
            };
            if let Err(error) = state.apply(&parsed) {
                state.reset();
                frame_failed = true;
                self.remember_failure(index, &format!("状态延续失败：{error}"));
                continue;
            }

            if let Some(slot) = substream_effective.get_mut(state_index) {
                *slot = GroupOamdState {
                    common: state.effective_common(),
                    timing: state.effective_timing(),
                    common_conflict: false,
                };
            }
            if let Some(slot) = substream_current_common.get_mut(state_index) {
                *slot = parsed.common;
            }

            frame_parsed = true;
            frame_common |= parsed.common.is_some();
            frame_timing |= parsed.timing.is_some();
            frame_carryover |= parsed.timing.is_none();
            self.record_payload(&parsed, index);
        }

        let mut group_effective = [GroupOamdState::default(); MAX_SUBSTREAMS];
        for (group_index, group) in topology.groups().iter().enumerate() {
            let external = group
                .oamd_substream
                .and_then(|reference| reference.substream_index)
                .and_then(|substream| {
                    substream_effective
                        .get(usize::try_from(substream).unwrap_or(usize::MAX))
                        .copied()
                })
                .unwrap_or_default();
            let external_current = group
                .oamd_substream
                .and_then(|reference| reference.substream_index)
                .and_then(|substream| {
                    substream_current_common
                        .get(usize::try_from(substream).unwrap_or(usize::MAX))
                        .copied()
                        .flatten()
                });

            let mut inline_current = None;
            let mut common_conflict = false;
            for info in group.substreams() {
                let SubstreamInfo::Ajoc(ajoc) = *info else {
                    continue;
                };
                let Some(common) = ajoc.oamd_common_data else {
                    continue;
                };
                frame_common = true;
                match inline_current {
                    Some(current) if current != common => common_conflict = true,
                    None => inline_current = Some(common),
                    _ => {}
                }
            }
            if matches!((inline_current, external_current), (Some(a), Some(b)) if a != b) {
                common_conflict = true;
            }
            if let Some(current) = inline_current.or(external_current) {
                if let Some(slot) = self.group_common.get_mut(group_index) {
                    *slot = GroupCommonState {
                        common: Some(current),
                        conflict: common_conflict,
                    };
                }
            }
            let common = self
                .group_common
                .get(group_index)
                .copied()
                .unwrap_or_default();
            if let Some(slot) = group_effective.get_mut(group_index) {
                *slot = GroupOamdState {
                    common: common.common,
                    timing: external.timing,
                    common_conflict: common.conflict,
                };
            }
        }

        if frame_located {
            self.located = self.located.saturating_add(1);
        }
        if found && frame_parsed && !frame_failed {
            self.parsed = self.parsed.saturating_add(1);
        }
        if frame_failed {
            self.failures = self.failures.saturating_add(1);
        }
        if frame_common {
            self.common_data_frames = self.common_data_frames.saturating_add(1);
        }
        if frame_timing {
            self.timing_frames = self.timing_frames.saturating_add(1);
        }
        if frame_carryover {
            self.timing_carryover_frames = self.timing_carryover_frames.saturating_add(1);
        }
        // 裸流没有 stss，此时不做成员关系比对而非默认为「一致」。
        if oamd_applicable && is_sync.is_some_and(|sync| frame_common != sync) {
            self.common_data_sync_mismatches = self.common_data_sync_mismatches.saturating_add(1);
        }
        group_effective
    }

    pub(super) fn record_payload(&mut self, parsed: &OamdSubstreamPayload, index: u32) {
        self.max_align_bits = self.max_align_bits.max(parsed.align_bits);
        self.dyndata_blocks = self.dyndata_blocks.saturating_add(parsed.dyndata_blocks);
        self.history_dependent_blocks = self
            .history_dependent_blocks
            .saturating_add(parsed.history_dependent_blocks);

        if let Some(timing) = parsed.timing {
            let count = timing.num_obj_info_blocks;
            self.min_blocks = Some(self.min_blocks.map_or(count, |value| value.min(count)));
            self.max_blocks = Some(self.max_blocks.map_or(count, |value| value.max(count)));
            for block in timing.blocks() {
                self.max_block_offset_samples =
                    self.max_block_offset_samples.max(block.offset_samples());
                self.max_ramp_duration = self.max_ramp_duration.max(block.ramp_duration);
            }
            if self.first_timing.is_none() {
                self.first_timing = Some(timing_json(&timing, index));
            }
        }
    }

    pub(super) fn remember_failure(&mut self, index: u32, message: &str) {
        if self.first_error.is_none() {
            self.first_error = Some(format!("帧 {index}：{message}"));
        }
    }

    pub(super) fn reset_substream(&mut self, substream_index: u32) {
        let index = usize::try_from(substream_index).unwrap_or(usize::MAX);
        if let Some(state) = self.states.get_mut(index) {
            state.reset();
        }
    }

    pub(super) fn reset_history(&mut self) {
        for state in &mut self.states {
            state.reset();
        }
        self.group_common.fill(GroupCommonState::default());
    }

    pub(super) fn to_json(&self) -> String {
        let error = self
            .first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let timing = self
            .first_timing
            .as_ref()
            .map_or_else(|| "null".to_owned(), Clone::clone);
        format!(
            "{{\"located\": {}, \"parsed\": {}, \"failures\": {}, \"first_error\": {error}, \
             \"common_data_frames\": {}, \"common_data_sync_mismatches\": {}, \
             \"timing_frames\": {}, \"timing_carryover_frames\": {}, \
             \"max_align_bits\": {}, \"dyndata_blocks\": {}, \"history_dependent_blocks\": {}, \
             \"min_obj_info_blocks\": {}, \"max_obj_info_blocks\": {}, \
             \"max_block_offset_samples\": {}, \"max_ramp_duration\": {}, \
             \"first_timing\": {timing}}}",
            self.located,
            self.parsed,
            self.failures,
            self.common_data_frames,
            self.common_data_sync_mismatches,
            self.timing_frames,
            self.timing_carryover_frames,
            self.max_align_bits,
            self.dyndata_blocks,
            self.history_dependent_blocks,
            self.min_blocks
                .map_or_else(|| "null".to_owned(), |value| value.to_string()),
            self.max_blocks
                .map_or_else(|| "null".to_owned(), |value| value.to_string()),
            self.max_block_offset_samples,
            self.max_ramp_duration,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    #[test]
    fn oamd_trace_counts_frames_with_multiple_groups_once() {
        // 两个 OAMD 均携带最简 common data。
        let (frame, topology) = topology_with_two_oamd_substreams([0xC0, 0x00, 0xC0, 0x00]);
        assert_eq!(topology.groups().len(), 2);

        let mut trace = OamdTrace::new();
        trace.observe(&frame, &topology, 0, Some(true));

        assert_eq!(trace.located, 1, "{:?}", trace.first_error);
        assert_eq!(trace.parsed, 1);
        assert_eq!(trace.failures, 0);
        assert_eq!(trace.common_data_frames, 1);
        assert_eq!(trace.common_data_sync_mismatches, 0);
        assert_eq!(trace.timing_carryover_frames, 1);

        // common 与 stss 数量同为 1，但位于不同帧时必须报告两次成员失配。
        let (without_common, without_common_topology) =
            topology_with_two_oamd_substreams([0x00; 4]);
        let mut mismatch = OamdTrace::new();
        mismatch.observe(&frame, &topology, 0, Some(false));
        mismatch.observe(&without_common, &without_common_topology, 1, Some(true));
        assert_eq!(mismatch.common_data_frames, 1);
        assert_eq!(mismatch.common_data_sync_mismatches, 2);
    }
    /// 适用性判定的第二支：group 没有独立 OAMD substream，但 A-JOC 元素本身
    /// 可以内嵌 `oamd_common_data()`，所以仍要做 stss 成员比对。
    ///
    /// 现有素材里带 A-JOC 的 group 同时也带 OAMD substream，第一支永远先成立
    /// ——删掉 `|| has_ajoc()` 不会有任何判据变红。本夹具专门只留 A-JOC 那一支。
    #[test]
    fn an_ajoc_group_without_an_oamd_substream_still_expects_common_data() {
        let (frame, topology) = topology_with_scoped_alternative();
        assert!(
            topology
                .groups()
                .iter()
                .all(|group| group.oamd_substream.is_none()),
            "夹具前提：没有独立 OAMD substream"
        );
        assert!(
            topology.groups().iter().any(|group| group.has_ajoc()),
            "夹具前提：存在 A-JOC group"
        );

        let mut trace = OamdTrace::new();
        trace.observe(&frame, &topology, 0, Some(true));

        assert_eq!(trace.located, 0);
        assert_eq!(trace.common_data_frames, 0);
        assert_eq!(
            trace.common_data_sync_mismatches, 1,
            "A-JOC group 可内嵌 common data，同步帧缺失时必须报失配"
        );
    }

    #[test]
    fn channel_based_sync_frame_does_not_require_oamd_common_data() {
        let (frame, topology) = topology_with_shared_channel_audio_substream();
        let mut trace = OamdTrace::new();

        trace.observe(&frame, &topology, 0, Some(true));

        assert_eq!(trace.located, 0);
        assert_eq!(trace.parsed, 0);
        assert_eq!(trace.failures, 0);
        assert_eq!(trace.common_data_frames, 0);
        assert_eq!(trace.common_data_sync_mismatches, 0);
    }
    #[test]
    fn oamd_trace_prefers_and_carries_inline_group_common() {
        // group 0 同时携带内嵌 common（ratio=17）与 OAMD substream common；
        // 两者冲突时按 group 内嵌值解释，并显式标出冲突。
        let inline_group0 = "1 0 1 0 1 1 00 1 0 1 1 0 10001 0 0 0000 1 0 0 1 01 0";
        let (frame, topology) = topology_with_group0_oamd([0xC0, 0x00, 0xC0, 0x00], inline_group0);
        let mut trace = OamdTrace::new();
        let first = trace.observe(&frame, &topology, 0, Some(true));

        assert_eq!(
            first[0]
                .common
                .and_then(|common| common.master_screen_size_ratio_code),
            Some(17)
        );
        assert!(first[0].common_conflict);

        let (next_frame, next_topology) = topology_with_two_oamd_substreams([0x00; 4]);
        let carried = trace.observe(&next_frame, &next_topology, 1, Some(false));
        assert_eq!(
            carried[0]
                .common
                .and_then(|common| common.master_screen_size_ratio_code),
            Some(17)
        );
        assert!(carried[0].common_conflict);

        // 下一次只由 OAMD substream 刷新时，旧的内嵌来源不得继续压住新值。
        let (refresh_frame, refresh_topology) =
            topology_with_two_oamd_substreams([0xC0, 0x00, 0xC0, 0x00]);
        let refreshed = trace.observe(&refresh_frame, &refresh_topology, 2, Some(true));
        assert!(
            refreshed[0]
                .common
                .is_some_and(|common| common.default_screen_size_ratio)
        );
        assert!(!refreshed[0].common_conflict);
    }
    #[cfg(feature = "audio-decode")]
    #[test]
    fn position_stats_preserve_every_intra_frame_update() {
        let initial = position_block(
            0,
            PositionUpdate::Absolute(AbsolutePosition {
                x: 10,
                y: 31,
                z_sign: true,
                z: 0,
            }),
        );
        let (state, initial_stats) =
            resolve_oamd_blocks(OamdState::new(), &[initial], 1).expect("初始位置应能建立");
        assert!(initial_stats.position_changes.is_empty());

        // 先 +2 再 -2，帧末回到原点。若只比帧前与批次终态，
        // 这两次变化会被错记为 0 次。
        let updates = [
            position_block(
                0,
                PositionUpdate::Differential(DifferentialPosition { x: 2, y: 0, z: 0 }),
            ),
            position_block(
                1,
                PositionUpdate::Differential(DifferentialPosition { x: 6, y: 0, z: 0 }),
            ),
        ];
        let (final_state, stats) =
            resolve_oamd_blocks(state, &updates, 2).expect("帧内两块应能顺序应用");

        assert_eq!(stats.differential_positions, 2);
        assert_eq!(
            stats.position_changes,
            vec![
                PositionChange {
                    object_index: 0,
                    block_index: 0,
                    x: 12,
                    y: 31,
                    z: 0,
                },
                PositionChange {
                    object_index: 0,
                    block_index: 1,
                    x: 10,
                    y: 31,
                    z: 0,
                },
            ]
        );
        assert_eq!(
            final_state
                .object(0)
                .and_then(|object| object.render)
                .map(|render| render.position.x),
            Some(10)
        );
    }
}
