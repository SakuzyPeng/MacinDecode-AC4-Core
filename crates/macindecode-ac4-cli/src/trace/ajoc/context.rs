//! A-JOC trace 的物理子流与场景引用上下文合并。

#[cfg(feature = "audio-decode")]
use super::{Ac4Topology, DecodeMode, MAX_SUBSTREAM_GROUPS, SubstreamInfoAjoc};
use super::{OamdCommonData, OamdTimingData};

/// 一条 group 引用附带的场景上下文；同一物理音频 substream 可有多条。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AjocSceneContext {
    pub(in crate::trace) group_oamd: GroupOamdState,
}

/// 同一物理 A-JOC substream 在一帧内只有一套音频解析上下文，但可以被多组
/// 场景元数据引用。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AjocTraceContext {
    pub(in crate::trace) info: SubstreamInfoAjoc,
    pub(in crate::trace) frame_rate_factor: u32,
    pub(in crate::trace) frame_rate_fraction: u32,
    pub(in crate::trace) alternative: bool,
    pub(in crate::trace) group_num_obj_info_blocks: Option<u8>,
    pub(in crate::trace) scene_contexts: [AjocSceneContext; MAX_SUBSTREAM_GROUPS],
    pub(in crate::trace) scene_contexts_written: usize,
}

#[cfg(feature = "audio-decode")]
impl AjocTraceContext {
    pub(in crate::trace) fn new(
        info: SubstreamInfoAjoc,
        frame_rate_factor: u32,
        frame_rate_fraction: u32,
        alternative: bool,
        group_oamd: GroupOamdState,
    ) -> Self {
        let mut scene_contexts = [AjocSceneContext::default(); MAX_SUBSTREAM_GROUPS];
        scene_contexts[0] = AjocSceneContext { group_oamd };
        Self {
            info,
            frame_rate_factor,
            frame_rate_fraction,
            alternative,
            group_num_obj_info_blocks: group_oamd.timing.map(|timing| timing.num_obj_info_blocks),
            scene_contexts,
            scene_contexts_written: 1,
        }
    }

    pub(in crate::trace) fn scene_contexts(&self) -> &[AjocSceneContext] {
        self.scene_contexts
            .get(..self.scene_contexts_written)
            .unwrap_or(&[])
    }

    /// 合并另一个引用。只有实际改变音频语法或循环次数的字段才构成冲突；
    /// common 与 timing 的偏移属于各 group 的场景上下文，分别保留。
    pub(in crate::trace) fn merge(&mut self, candidate: Self) -> bool {
        if !self.info.has_same_audio_context(&candidate.info)
            || self.frame_rate_factor != candidate.frame_rate_factor
            || self.frame_rate_fraction != candidate.frame_rate_fraction
        {
            return false;
        }
        match (
            self.group_num_obj_info_blocks,
            candidate.group_num_obj_info_blocks,
        ) {
            (Some(current), Some(next)) if current != next => return false,
            (None, Some(next)) => self.group_num_obj_info_blocks = Some(next),
            _ => {}
        }
        self.alternative |= candidate.alternative;
        for next in candidate.scene_contexts() {
            if self
                .scene_contexts
                .iter()
                .take(self.scene_contexts_written)
                .any(|current| current.group_oamd == next.group_oamd)
            {
                continue;
            }
            let Some(slot) = self.scene_contexts.get_mut(self.scene_contexts_written) else {
                return false;
            };
            *slot = *next;
            self.scene_contexts_written = self.scene_contexts_written.saturating_add(1);
        }
        true
    }
}

/// group 级 OAMD 跨帧合并后的公共数据与时间数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GroupOamdState {
    pub(in crate::trace) common: Option<OamdCommonData>,
    pub(in crate::trace) timing: Option<OamdTimingData>,
    pub(in crate::trace) common_conflict: bool,
}

/// 两个规范允许的位置共同刷新的一份 group common 历史。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GroupCommonState {
    pub(in crate::trace) common: Option<OamdCommonData>,
    pub(in crate::trace) conflict: bool,
}

/// 一条 group 引用在当前帧最终生效的 core/full 场景时间。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct EffectiveSceneContext {
    pub(in crate::trace) dmx_timing: Option<OamdTimingData>,
    pub(in crate::trace) umx_timing: Option<OamdTimingData>,
}

#[cfg(feature = "audio-decode")]
pub(crate) fn shared_scene_timing(
    contexts: &[EffectiveSceneContext],
    mode: DecodeMode,
) -> Option<OamdTimingData> {
    let first = contexts
        .first()
        .and_then(|context| scene_timing(*context, mode))?;
    contexts
        .iter()
        .all(|context| scene_timing(*context, mode) == Some(first))
        .then_some(first)
}

#[cfg(feature = "audio-decode")]
fn scene_timing(context: EffectiveSceneContext, mode: DecodeMode) -> Option<OamdTimingData> {
    match mode {
        DecodeMode::Core => context.dmx_timing,
        DecodeMode::Full => context.umx_timing,
        _ => None,
    }
}

#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::large_enum_variant,
    reason = "fixed-capacity per-frame contexts remain Copy and avoid per-frame heap allocation"
)]
pub(crate) enum AjocContextSlot {
    Empty,
    Ready(AjocTraceContext),
    Conflict,
}

/// 一个 group 可被多个 presentation 共享，但其帧率上下文必须一致。
#[cfg(feature = "audio-decode")]
pub(crate) fn group_frame_rate_fraction(
    topology: &Ac4Topology,
    group_index: usize,
    parsed_factor: u32,
) -> Result<u32, &'static str> {
    let mut fraction = None;
    for presentation in topology.presentations() {
        if !presentation
            .group_indices()
            .iter()
            .any(|&referenced| usize::try_from(referenced) == Ok(group_index))
        {
            continue;
        }
        if presentation.frame_rate_factor != parsed_factor {
            return Err("Conflicting frame_rate_factor declarations for the same group");
        }
        match fraction {
            None => fraction = Some(presentation.frame_rate_fraction),
            Some(current) if current == presentation.frame_rate_fraction => {}
            Some(_) => {
                return Err("Conflicting frame_rate_fraction declarations for the same group");
            }
        }
    }
    Ok(fraction.unwrap_or(1))
}
