//! A-JOC 统一 engine observation 的 trace 统计。

#[cfg(feature = "audio-decode")]
use super::{
    Ac4SubstreamAjoc, Ajoc, AjocObjectControl, AjocObjectMatrix, AjocSubstreamContext, DecodeMode,
    MAX_OAMD_OBJECTS, MAX_POSITION_TIMELINE, MAX_SUBSTREAM_GROUPS, MAX_SUBSTREAMS,
    OamdMetadataBlock, OamdState, PositionChange, ReconstructionInvariant, ScaledStats,
    SubstreamInfo, SubstreamInfoAjoc, group_is_alternative, resolve_oamd_blocks_timed,
};
use super::{Ac4Topology, OamdCommonData, OamdTimingData};
#[cfg(feature = "audio-decode")]
use macindecode_ac4_decode::full_ajoc::{
    AspxBlocker, AspxReach, FullAjocAsfError, FullAjocAsfErrorKind, FullAjocAsfFrameObservation,
    FullAjocAsfStage, FullAjocAudioFrameError, FullAjocAudioFrameInput, FullAjocBlocker,
    FullAjocDecodeError, FullAjocDecodeErrorKind, FullAjocDecodeMode, FullAjocDecoder,
    FullAjocFrameProvenance, FullAjocObservation, FullAjocSyntaxFrameInput,
    FullAjocSyntaxObservation, SupportedAspxFrame, companding_is_active,
};

#[cfg(feature = "audio-decode")]
mod aspx;
#[cfg(feature = "audio-decode")]
mod census;
mod context;
mod decode;
mod objects;
mod stats;

#[cfg(feature = "audio-decode")]
use census::AjocMatrixCensus;
#[cfg(feature = "audio-decode")]
use context::{
    AjocContextSlot, AjocTraceContext, EffectiveSceneContext, group_frame_rate_fraction,
    shared_scene_timing,
};
pub(crate) use context::{GroupCommonState, GroupOamdState};
#[cfg(feature = "audio-decode")]
use stats::{AjocObservation, FrameTally};

/// A-JOC substream 音频数据的统计。
///
/// 未启用 `audio-decode` 时是空壳，`to_json` 输出 `null`——trace 是核对接口，
/// 字段集合不应随编译选项变化。
#[cfg(not(feature = "audio-decode"))]
#[derive(Debug, Default)]
pub(crate) struct AjocTrace;

#[cfg(not(feature = "audio-decode"))]
impl AjocTrace {
    pub(super) const fn new() -> Self {
        Self
    }

    #[allow(dead_code)]
    pub(super) const fn observe(
        &mut self,
        _frame: &[u8],
        _topology: &Ac4Topology,
        _index: u32,
        _group_oamd: &[GroupOamdState],
    ) {
    }

    pub(super) const fn observe_at(
        &mut self,
        _frame: &[u8],
        _topology: &Ac4Topology,
        _index: u32,
        _group_oamd: &[GroupOamdState],
        _frame_start_samples: i64,
    ) {
    }

    pub(super) const fn reset_history(&mut self) {}

    pub(super) fn to_json(&self) -> String {
        "null".to_owned()
    }
}

/// A-JOC substream 音频数据的统计。
///
/// 这是 `audio_size` 第一次被当作判据使用：`ac4_substream()` 声明音频区段
/// 长度，`audio_data_ajoc()` 必须在该区段内走完。区段外还有长度不受约束的
/// `fill_bits`，因此判据是单向的——越界必然报错，少读只体现为偏大的
/// `fill_bits`，故这里把它的极值一并统计出来。
#[cfg(feature = "audio-decode")]
#[derive(Debug)]
pub(crate) struct AjocTrace {
    /// 含至少一个 A-JOC substream 的帧数。
    pub(super) frames: u32,
    /// 全部 A-JOC substream 均成功解析的帧数。
    pub(super) parsed: u32,
    /// 观察到的唯一物理 A-JOC substream 次数。
    pub(super) substreams: u32,
    /// 成功解析的物理 A-JOC substream 次数。
    pub(super) parsed_substreams: u32,
    /// 至少一条 A-JOC substream 未完整落地的帧数，包含上下文、解析与状态失败。
    pub(super) failures: u32,
    pub(super) first_error: Option<String>,
    /// `fill_bits` 的极值。合法码流可以有任意多填充，但持续偏大意味着
    /// 本实现少读了某个字段。
    pub(super) min_fill_bits: Option<u64>,
    pub(super) max_fill_bits: Option<u64>,
    /// 两种解码模式的 `num_obj_info_blocks` 上界。
    pub(super) max_dmx_blocks: u8,
    pub(super) max_umx_blocks: u8,
    /// 写入的 `object_info_block` 总数。
    pub(super) dmx_blocks: u64,
    pub(super) umx_blocks: u64,
    /// `b_derive_timing_from_dmx` 为真的次数。
    pub(super) derive_timing: u32,
    /// `b_some_signals_inactive` 为真的次数。
    pub(super) inactive_signals: u32,
    /// `b_oamd_extension_present` 为真的次数。
    pub(super) bed_info: u32,
    /// 至少一条 substream 传输了 `companding_control()` 的帧数。
    ///
    /// `4.2.11` 只在 A-SPX 且下混信号数不超过 5 时传输该元素，因此「未传输」
    /// 与「传输了但全关」是两回事，分开计数。
    pub(super) companding_frames: u32,
    /// 其中任一 `b_compand_on` 或 `b_compand_avg` 为真的帧数。
    pub(super) companding_active_frames: u32,
    /// A-SPX 各分支被触发的帧数，见 `AspxReach`。
    pub(super) aspx_add_harmonic_frames: u32,
    pub(super) aspx_interleaved_frames: u32,
    pub(super) aspx_variable_framing_frames: u32,
    pub(super) aspx_balance_frames: u32,
    /// `ajoc()` 量化矩阵与 full-path 支持边界的逐 substream census。
    pub(super) ajoc_matrix: AjocMatrixCensus,
    /// `num_obj_info_blocks` 大于 1 的帧数，即帧内多次更新。
    pub(super) intra_frame_update_frames: u32,
    /// 应用逐对象更新后位置发生变化的块数。
    pub(super) position_changes: u64,
    /// 差分编码的位置更新块数。
    pub(super) differential_positions: u64,
    /// 状态延续失败的次数。
    pub(super) state_failures: u32,
    /// 还原出绝对标度因子的频带总数（`5.1.3.2`）。
    pub(super) scale_factor_bands: u64,
    /// 还原出的标度因子取值范围；`5.1.3.2` NOTE 规定合法区间是 `0…255`。
    pub(super) scale_factor_min: Option<u8>,
    pub(super) scale_factor_max: Option<u8>,
    /// 标度因子走出合法区间的声道次数。
    pub(super) scale_factor_failures: u32,
    pub(super) scale_factor_first_error: Option<String>,
    /// `5.1.3.2` 后两步的实测统计。
    pub(super) scaled_stats: ScaledStats,
    /// 两种解码模式各自的 OAMD 对象状态，按物理 substream 下标隔离。
    ///
    /// core 与 full 的对象集合不同（下混信号数对上混对象数），因此状态不能
    /// 共用一份。
    pub(super) dmx_objects: Vec<OamdState>,
    pub(super) umx_objects: Vec<OamdState>,
    /// 物理 A-JOC substream 的共享完整 timing 历史。
    ///
    /// 自身 timing 总是共享；group timing 只在全部引用一致时写入，否则一个
    /// group 的偏移会泄漏到引用同一音频载荷的另一个 group。
    pub(super) dmx_audio_timing: Vec<Option<OamdTimingData>>,
    pub(super) umx_audio_timing: Vec<Option<OamdTimingData>>,
    /// 无整文件 sink 的 bitstream Full decoder。`Option` 只用于一次调用期间临时
    /// 移出，以同时更新 CLI observation；稳定状态始终为 `Some`。
    pub(super) full_decoder: Option<FullAjocDecoder>,
    /// A-SPX 驱动失败数与首个原因，与解析失败分开计。
    pub(super) aspx_failures: u32,
    pub(super) aspx_first_error: Option<String>,
    /// 表 188 预热、full 重建及实际启用 wet 路径的帧数。
    ///
    /// 三项是路径覆盖观测，不是正确性 oracle；`audio_check.sh` 用后两项确保真实
    /// 256/448 kbps 媒体确实越过 A-SPX 诊断出口进入对象矩阵和去相关分支。
    pub(super) ajoc_full_warmup_frames: u32,
    pub(super) ajoc_full_reconstructed_frames: u32,
    pub(super) ajoc_full_wet_frames: u32,
    /// full 矩阵、终端有限值与对象输出形状的三条独立不变量。
    pub(super) ajoc_reconstruction_failures: u32,
    pub(super) ajoc_reconstruction_first_error: Option<String>,
    pub(super) objects_nonfinite: u64,
    pub(super) objects_nonfinite_first_error: Option<String>,
    pub(super) object_shape_mismatches: u32,
    pub(super) object_shape_first_error: Option<String>,
    /// 首个合法但未支持的 A-SPX 分支，供 CLI 选择稳定诊断码。
    pub(super) aspx_unsupported_first_error: Option<String>,
    /// 首个合法但未支持的 full A-JOC 分支，只在对象导出要求 full 时产生。
    pub(super) full_unsupported_first_error: Option<String>,
    pub(super) first_detail: Option<String>,
    /// 每条物理 substream 首次成功解析时的上混侧位置。
    pub(super) first_positions: Vec<Option<String>>,
    /// 上混侧逐次位置变化，用于与母版意图比对；超过容量后停止记录。
    pub(super) position_timeline: Vec<String>,
    pub(super) timeline_truncated: bool,
}

#[cfg(all(test, feature = "audio-decode"))]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use macindecode_ac4_decode::{
        aspx::{IntervalClass, collect_aspx_reach},
        channel::CompandingControl,
    };

    #[test]
    fn frame_level_counters_advance_once_per_frame() {
        let mut trace = AjocTrace::new();
        let mut tally = FrameTally {
            declared: 2,
            ..FrameTally::default()
        };
        let observation = AjocObservation {
            intra_frame_updates: true,
            companding_present: true,
            companding_active: true,
            aspx: AspxReach::default(),
        };
        tally.observe(Some(observation));
        tally.observe(Some(observation));
        trace.commit_frame(tally);

        assert_eq!(trace.frames, 1);
        assert_eq!(trace.parsed, 1);
        assert_eq!(trace.failures, 0);
        assert_eq!(trace.companding_frames, 1);
        assert_eq!(trace.companding_active_frames, 1);
        assert_eq!(trace.substreams, 2);
        assert_eq!(trace.parsed_substreams, 2);
    }

    /// 同帧部分 substream 失败时，帧级与 substream 级计数不能混用。
    #[test]
    fn partly_failed_frame_keeps_frame_and_substream_units_apart() {
        let mut trace = AjocTrace::new();
        let mut tally = FrameTally {
            declared: 3,
            ..FrameTally::default()
        };
        tally.observe(Some(AjocObservation {
            intra_frame_updates: true,
            companding_present: false,
            companding_active: false,
            aspx: AspxReach::default(),
        }));
        tally.observe(None);
        tally.observe(None);
        trace.commit_frame(tally);

        assert_eq!(trace.frames, 1);
        assert_eq!(trace.parsed, 0);
        assert_eq!(trace.failures, 1);
        assert_eq!(trace.substreams, 3);
        assert_eq!(trace.parsed_substreams, 1);
        assert_eq!(trace.intra_frame_update_frames, 1);
    }

    #[test]
    fn aspx_reach_reads_branches_used_only_by_right_channel() {
        let used = parse_two_channel_aspx(true);
        let unused = parse_two_channel_aspx(false);

        assert_eq!(
            used.framing(0).map(|framing| framing.params.int_class),
            Some(IntervalClass::FixFix)
        );
        assert_eq!(
            used.framing(1).map(|framing| framing.params.int_class),
            Some(IntervalClass::FixVar)
        );
        assert_eq!(used.hfgen(0).and_then(|h| h.add_harmonic(0)), Some(false));
        assert_eq!(used.hfgen(1).and_then(|h| h.add_harmonic(0)), Some(true));

        let reach = collect_aspx_reach(core::slice::from_ref(&used));
        assert!(reach.add_harmonic());
        assert!(reach.variable_framing());
        let quiet = collect_aspx_reach(core::slice::from_ref(&unused));
        assert!(!quiet.add_harmonic());
        assert!(!quiet.variable_framing());
    }

    /// A-SPX 可达性按帧合并，同帧多条 substream 只让各计数增加一次。
    #[test]
    fn aspx_reach_merges_once_per_frame() {
        let mut tally = FrameTally {
            declared: 2,
            ..FrameTally::default()
        };
        tally.observe(Some(AjocObservation {
            intra_frame_updates: false,
            companding_present: false,
            companding_active: false,
            aspx: AspxReach::new(true, false, false, false),
        }));
        tally.observe(Some(AjocObservation {
            intra_frame_updates: false,
            companding_present: false,
            companding_active: false,
            aspx: AspxReach::new(false, false, true, false),
        }));

        let mut trace = AjocTrace::new();
        trace.commit_frame(tally);
        assert_eq!(trace.aspx_add_harmonic_frames, 1);
        assert_eq!(trace.aspx_variable_framing_frames, 1);
        assert_eq!(trace.aspx_interleaved_frames, 0);
        assert_eq!(trace.aspx_balance_frames, 0);
    }

    #[test]
    fn companding_average_alone_counts_as_active() {
        let average = CompandingControl {
            sync: false,
            compand_on: [false; 8],
            channels: 5,
            compand_avg: Some(true),
        };
        assert!(companding_is_active(&average));
        assert!(!companding_is_active(&CompandingControl {
            compand_avg: Some(false),
            ..average
        }));
    }

    #[test]
    fn every_reconstruction_invariant_is_exposed_in_trace_json() {
        fn seed(trace: &mut AjocTrace, kind: ReconstructionInvariant) {
            match kind {
                ReconstructionInvariant::State => trace.state_failures = 1,
                ReconstructionInvariant::Frame => trace.failures = 1,
                ReconstructionInvariant::ScaleFactor => trace.scale_factor_failures = 1,
                ReconstructionInvariant::Scale => trace.scaled_stats.scale_failures = 1,
                ReconstructionInvariant::Ungroup => trace.scaled_stats.ungroup_failures = 1,
                ReconstructionInvariant::UngroupCountMismatch => {
                    trace.scaled_stats.ungroup_count_mismatch = 1;
                }
                ReconstructionInvariant::UngroupEnergyDrift => {
                    trace.scaled_stats.ungroup_energy_drift = 1e-12;
                }
                ReconstructionInvariant::ScaledNonFinite => trace.scaled_stats.nonfinite = 1,
                ReconstructionInvariant::Synthesis => {
                    trace.scaled_stats.synthesis_failures = 1;
                }
                ReconstructionInvariant::PcmNonFinite => {
                    trace.scaled_stats.pcm_nonfinite = 1;
                }
                ReconstructionInvariant::PcmSampleConservation => {
                    trace.scaled_stats.ungrouped_lines = 1;
                }
                ReconstructionInvariant::AjocReconstruction => {
                    trace.ajoc_reconstruction_failures = 1;
                }
                ReconstructionInvariant::ObjectsNonFinite => trace.objects_nonfinite = 1,
                ReconstructionInvariant::ObjectShapeMismatch => {
                    trace.object_shape_mismatches = 1;
                }
                ReconstructionInvariant::AspxDrive => trace.aspx_failures = 1,
            }
        }

        let clean = AjocTrace::new();
        assert_eq!(clean.invariant_violations().count(), 0);
        for kind in ReconstructionInvariant::ALL.iter().copied() {
            let mut trace = AjocTrace::new();
            seed(&mut trace, kind);
            assert_eq!(
                trace.invariant_violations().next().map(|item| item.0),
                Some(kind)
            );
            let json = trace.to_json();
            assert!(
                json.contains(&format!("{{\"name\": {:?}, \"detail\":", kind.name())),
                "{kind:?} 未出现在 trace JSON：{json}"
            );
        }
    }

    #[test]
    fn position_timeline_reports_capacity_truncation() {
        let change = PositionChange {
            object_index: 0,
            block_index: 0,
            x: 1,
            y: 2,
            z: 3,
        };
        let mut trace = AjocTrace::new();
        trace.record_position_timeline(
            2,
            7,
            &vec![change; MAX_POSITION_TIMELINE.saturating_add(1)],
        );
        assert_eq!(trace.position_timeline.len(), MAX_POSITION_TIMELINE);
        assert!(trace.timeline_truncated);

        let mut intact = AjocTrace::new();
        intact.record_position_timeline(2, 7, &[change]);
        assert_eq!(intact.position_timeline.len(), 1);
        assert!(!intact.timeline_truncated);
    }

    /// 超容量下标在建立任何 slot 之前失败，仍必须把整帧记为失败。
    #[test]
    fn out_of_range_substream_index_fails_the_frame() {
        let (frame, topology) = topology_with_out_of_range_ajoc_index();
        assert!(validate_substream_references(&topology).is_err());

        let mut trace = AjocTrace::new_tracing();
        trace.observe_at(
            &frame,
            &topology,
            0,
            &[GroupOamdState::default(); MAX_SUBSTREAM_GROUPS],
            0,
        );

        assert_eq!(trace.frames, 1);
        assert_eq!(trace.substreams, 0);
        assert_eq!(trace.parsed_substreams, 0);
        assert_eq!(trace.parsed, 0);
        assert_eq!(trace.failures, 1);
        assert!(trace.first_error.is_some());
    }

    /// 多个 group 引用同一物理载荷时，只能解析并计数一次。
    #[test]
    fn shared_ajoc_deduplicates_the_physical_substream() {
        let (frame, topology) = topology_with_shared_audio_substream();
        let mut trace = AjocTrace::new_tracing();

        trace.observe_at(
            &frame,
            &topology,
            0,
            &[GroupOamdState::default(); MAX_SUBSTREAM_GROUPS],
            0,
        );

        assert_eq!(trace.frames, 1);
        assert_eq!(trace.substreams, 1);
        assert_eq!(trace.parsed_substreams, 0);
        assert_eq!(trace.failures, 1);
    }

    /// 同一物理载荷的两个引用声明不同块数时，冲突必须在解码前失败关闭。
    #[test]
    fn shared_ajoc_rejects_conflicting_group_contexts() {
        let (frame, topology) = topology_with_shared_audio_substream();
        let mut group_oamd = [GroupOamdState::default(); MAX_SUBSTREAM_GROUPS];
        group_oamd[0].timing = Some(implicit_timing(1));
        group_oamd[1].timing = Some(implicit_timing(2));
        let mut trace = AjocTrace::new_tracing();

        trace.observe_at(&frame, &topology, 0, &group_oamd, 0);

        assert_eq!(trace.frames, 1);
        assert_eq!(trace.substreams, 1);
        assert_eq!(trace.parsed_substreams, 0);
        assert_eq!(trace.parsed, 0);
        assert_eq!(trace.failures, 1);
        assert!(
            trace
                .first_error
                .as_deref()
                .is_some_and(|error| error.contains("Conflicting parse contexts")),
            "{:?}",
            trace.first_error
        );
    }

    #[test]
    fn shared_ajoc_keeps_distinct_group_timings() {
        let (_, topology) = topology_with_shared_audio_substream();
        let Some(SubstreamInfo::Ajoc(info)) = topology
            .groups()
            .first()
            .and_then(|group| group.substreams().first())
            .copied()
        else {
            panic!("fixture 应包含 A-JOC info");
        };
        let timing = implicit_timing(1);
        let shifted_timing = explicit_timing(1, 1);
        let mut first = AjocTraceContext::new(
            info,
            1,
            1,
            false,
            GroupOamdState {
                timing: Some(timing),
                ..GroupOamdState::default()
            },
        );
        let second = AjocTraceContext::new(
            info,
            1,
            1,
            false,
            GroupOamdState {
                timing: Some(shifted_timing),
                ..GroupOamdState::default()
            },
        );

        assert!(first.merge(second));
        assert_eq!(first.group_num_obj_info_blocks, Some(1));
        let [first_scene, second_scene] = first.scene_contexts() else {
            panic!("两条不同 timing 的 group 上下文都应保留");
        };
        assert_eq!(first_scene.group_oamd.timing, Some(timing));
        assert_eq!(second_scene.group_oamd.timing, Some(shifted_timing));
    }
}
