//! AC-4 码流 trace。
//!
//! 走一遍码流，把能观测到的都收集起来并渲染为 trace JSON。生产 Scene 制品由
//! crate 顶层的 `scene_batch` 把 bounded AU 交给 Scene Session；本模块只维护
//! `trace` 的统计契约。
//!
//! CLI 的 `trace` 子命令名不变；`invariants` 只形成它的稳定诊断 JSON，制品导出
//! 的失败策略由 Scene Session 与 batch adapter 自己负责。
//!
//! 外部依赖仍在门面统一引入；生产子模块显式列出所需名称，让拆分后的职责边界和
//! 依赖方向可以直接从文件头审阅。测试模块保留就近的 `use super::*`。

#[cfg(test)]
pub(crate) mod testutil;

mod ajoc;
mod audio_substream;
mod invariants;
mod oamd;
mod report;
mod spectrum;
mod topology;

use crate::container::{AUDIO_SAMPLE_ENTRY_LEN, find_ac4_track, parse_stsz};
use ajoc::{AjocTrace, GroupCommonState, GroupOamdState};
use audio_substream::AudioTrace;
#[cfg(feature = "audio-decode")]
use audio_substream::group_is_alternative;
#[cfg(feature = "audio-decode")]
use invariants::ReconstructionInvariant;
use oamd::OamdTrace;
#[cfg(feature = "audio-decode")]
use oamd::{MAX_POSITION_TIMELINE, PositionChange, resolve_oamd_blocks_timed};
#[cfg(feature = "audio-decode")]
use spectrum::ScaledStats;
use topology::{TopologyTrace, timing_json};

#[cfg(feature = "audio-decode")]
use macindecode_ac4_bitstream::{
    Ac4SubstreamAjoc, AjocSubstreamContext,
    ajoc::{Ajoc, AjocObjectControl, AjocObjectMatrix},
    oamd::{MAX_OAMD_OBJECTS, OamdMetadataBlock, OamdStateError},
    substream::SubstreamInfoAjoc,
};
use macindecode_ac4_bitstream::{
    Ac4Toc, DecodingDelay, SequenceTransition, SyncFrameIter,
    audio_substream::{Ac4AudioSubstream, SubstreamContext},
    oamd::{
        OamdCommonData, OamdContext, OamdState, OamdSubstreamPayload, OamdTimingData,
        ObjectDescriptors, SampleOffsetSource,
    },
    presentation::presentation_config_label,
    substream::SubstreamInfo,
    topology::{
        Ac4Topology, ConfigFingerprint, DecoderAction, MAX_SUBSTREAM_GROUPS, MAX_SUBSTREAMS,
        RandomAccess, ResetReason, ScenePath, TopologyStateMachine, validate_group_references,
        validate_substream_references,
    },
};
use macindecode_ac4_mp4::{
    Ac4Dsi, BoxIter, EditListEntry, SampleDelta, SampleTable, find_box, find_path,
    media_time_to_presentation, parse_edit_list, parse_header_timing, presentation_timing,
};
#[cfg(feature = "audio-decode")]
use macindecode_ac4_scene::DecodeMode;

pub(crate) fn trace_input(data: &[u8]) -> Result<String, String> {
    // 裸流以 sync_word 起始；MP4 以 box 头起始。二者不会混淆。
    if matches!(data.get(0..2), Some([0xAC, 0x40] | [0xAC, 0x41])) {
        report::trace_raw(data)
    } else {
        report::trace(data)
    }
}
