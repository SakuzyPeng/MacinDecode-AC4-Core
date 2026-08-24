//! 拓扑 trace 与 TOC 描述。
//!
//! 逐帧核对 presentation、substream group 与 substream 的引用关系，并把 TOC
//! 渲染成 JSON。

use super::{
    Ac4Topology, AjocTrace, AudioTrace, ConfigFingerprint, DecoderAction, DecodingDelay, OamdTrace,
    RandomAccess, ResetReason, SampleOffsetSource, ScenePath, SequenceTransition, SubstreamInfo,
    TopologyStateMachine, presentation_config_label, validate_group_references,
    validate_substream_references,
};

/// 逐帧的拓扑核对结果。
///
/// 拓扑不像 TOC 前置字段那样有容器侧的独立声明可比，因此这里用码流自身
/// 的冗余做校验：`payload_base` 给出 substream 0 相对字节对齐 TOC 末尾的
/// 偏移，`substream_index_table()` 给出各 substream 的尺寸。二者与帧长必须
/// 自洽，且所有显式引用必须精确覆盖索引表。逐位解析只要错
/// 一位，这两类门禁通常至少有一类失败。
pub(crate) struct TopologyTrace {
    pub(super) parsed: u32,
    pub(super) failures: u32,
    pub(super) first_error: Option<String>,
    /// substream 尺寸之和超出帧长的帧数。
    pub(super) size_overruns: u32,
    /// presentation 引用了不存在的 group 的帧数。
    pub(super) dangling_group_refs: u32,
    /// substream 引用与索引表不精确匹配的帧数。
    pub(super) substream_reference_failures: u32,
    /// 容器 `stss` 与完整场景随机访问判定不一致的帧数。
    pub(super) stss_random_access_mismatches: u32,
    /// 与首帧完整规范化配置不一致的帧数。
    pub(super) path_changes: u32,
    pub(super) path: Option<ScenePath>,
    pub(super) n_presentations: Option<usize>,
    pub(super) n_groups: Option<usize>,
    pub(super) total_objects: Option<u32>,
    /// 完整随机访问点数，即全部 ndot 标志为真的帧。
    pub(super) full_random_access: u32,
    /// 仅音频可起解的帧数：b_iframe_global 为真但有 substream 依赖前序帧。
    pub(super) audio_only_random_access: u32,
    /// 配置代次；每次指纹变化加一。
    pub(super) config_generations: u32,
    pub(super) first_fingerprint: Option<ConfigFingerprint>,
    /// 规范计数规则判定的来源变化次数。
    pub(super) source_changes: u32,
    /// 在完整随机访问点实际执行重置的次数。
    pub(super) reset_events: u32,
    /// 已需重置但当前帧不是完整随机访问点的帧数。
    pub(super) waiting_for_random_access_frames: u32,
    pub(super) state: TopologyStateMachine,
    /// 首帧声明的解码延迟。
    pub(super) delay: Option<DecodingDelay>,
    pub(super) detail: String,
    pub(super) oamd: OamdTrace,
    pub(super) audio: AudioTrace,
    pub(super) ajoc_audio: AjocTrace,
}

pub(crate) fn timing_json(
    timing: &macindecode_ac4_bitstream::oamd::OamdTimingData,
    index: u32,
) -> String {
    let source = match timing.offset_source {
        SampleOffsetSource::Implicit => "implicit",
        SampleOffsetSource::Code => "code",
        SampleOffsetSource::Explicit => "explicit",
    };
    let mut blocks = String::new();
    for (position, block) in timing.blocks().iter().enumerate() {
        if position > 0 {
            blocks.push_str(", ");
        }
        blocks.push_str(&format!(
            "{{\"block_offset_factor\": {}, \"offset_samples\": {}, \"ramp_duration_code\": {}, \"ramp_duration\": {}}}",
            block.block_offset_factor,
            block.offset_samples(),
            block.ramp_duration_code,
            block.ramp_duration
        ));
    }
    format!(
        "{{\"frame\": {index}, \"offset_source\": \"{source}\", \"sample_offset\": {}, \"num_obj_info_blocks\": {}, \"blocks\": [{blocks}]}}",
        timing.sample_offset, timing.num_obj_info_blocks
    )
}

impl TopologyTrace {
    pub(super) fn new() -> Self {
        Self {
            parsed: 0,
            failures: 0,
            first_error: None,
            size_overruns: 0,
            dangling_group_refs: 0,
            substream_reference_failures: 0,
            stss_random_access_mismatches: 0,
            path_changes: 0,
            path: None,
            n_presentations: None,
            n_groups: None,
            total_objects: None,
            full_random_access: 0,
            audio_only_random_access: 0,
            config_generations: 0,
            first_fingerprint: None,
            source_changes: 0,
            reset_events: 0,
            waiting_for_random_access_frames: 0,
            state: TopologyStateMachine::new(),
            delay: None,
            detail: String::new(),
            oamd: OamdTrace::new(),
            audio: AudioTrace::new(),
            ajoc_audio: AjocTrace::new(),
        }
    }

    #[cfg(feature = "audio-decode")]
    pub(super) fn new_tracing() -> Self {
        let mut out = Self::new();
        out.ajoc_audio = AjocTrace::new_tracing();
        out
    }

    /// 解码延迟的 JSON 表示。
    ///
    /// 码字 0 与 7 不是帧数，必须与真正的帧数区分开。
    /// 序列化为 trace 中的 `topology` 对象。
    ///
    /// 容器路径与裸流路径共用；两者唯一的差别是裸流没有 `stss`，相关比对
    /// 在 `observe` 中按 `None` 跳过。
    pub(super) fn to_json(&self) -> String {
        format!(
            concat!(
                "{{\n",
                "    \"frames_parsed\": {},\n",
                "    \"parse_failures\": {},\n",
                "    \"first_error\": {},\n",
                "    \"substream_size_overruns\": {},\n",
                "    \"dangling_group_references\": {},\n",
                "    \"substream_reference_failures\": {},\n",
                "    \"stss_random_access_mismatches\": {},\n",
                "    \"frames_differing_from_first\": {},\n",
                "    \"full_random_access_frames\": {},\n",
                "    \"audio_only_random_access_frames\": {},\n",
                "    \"config_generations\": {},\n",
                "    \"source_changes\": {},\n",
                "    \"reset_events\": {},\n",
                "    \"waiting_for_random_access_frames\": {},\n",
                "    \"awaiting_random_access\": {},\n",
                "    \"decoding_delay\": {},\n",
                "    \"scene_path\": {},\n",
                "    \"presentations\": {},\n",
                "    \"substream_groups\": {},\n",
                "    \"total_objects\": {},\n",
                "    \"oamd\": {},\n",
                "    \"audio_substream\": {},\n",
                "    \"ajoc_audio\": {},\n",
                "    \"first_frame\": {}\n",
                "  }}"
            ),
            self.parsed,
            self.failures,
            self.first_error
                .as_ref()
                .map_or_else(|| "null".to_owned(), |text| format!("{text:?}")),
            self.size_overruns,
            self.dangling_group_refs,
            self.substream_reference_failures,
            self.stss_random_access_mismatches,
            self.path_changes,
            self.full_random_access,
            self.audio_only_random_access,
            self.config_generations,
            self.source_changes,
            self.reset_events,
            self.waiting_for_random_access_frames,
            self.state.is_waiting_for_random_access(),
            self.decoding_delay_json(),
            self.path
                .map_or_else(|| "null".to_owned(), |value| format!("\"{value}\"")),
            self.n_presentations
                .map_or_else(|| "null".to_owned(), |value| value.to_string()),
            self.n_groups
                .map_or_else(|| "null".to_owned(), |value| value.to_string()),
            self.total_objects
                .map_or_else(|| "null".to_owned(), |value| value.to_string()),
            self.oamd.to_json(),
            self.audio.to_json(),
            self.ajoc_audio.to_json(),
            if self.detail.is_empty() {
                "null"
            } else {
                &self.detail
            },
        )
    }

    pub(super) fn decoding_delay_json(&self) -> String {
        match self.delay {
            None => "null".to_owned(),
            Some(DecodingDelay::ConstantBitRate) => "{\"kind\": \"cbr\", \"frames\": 0}".to_owned(),
            Some(DecodingDelay::VariableBitRate) => {
                "{\"kind\": \"vbr\", \"frames\": null}".to_owned()
            }
            Some(DecodingDelay::Frames(count)) => {
                format!("{{\"kind\": \"frames\", \"frames\": {count}}}")
            }
        }
    }

    /// 丢弃所有依赖前帧的解码历史。
    pub(super) fn reset_decoder_history(&mut self) {
        self.oamd.reset_history();
        self.audio.reset_history();
        self.ajoc_audio.reset_history();
    }

    pub(super) fn observe(&mut self, frame: &[u8], index: u32, is_sync: Option<bool>) {
        self.observe_at(frame, index, is_sync, 0);
    }

    pub(super) fn observe_at(
        &mut self,
        frame: &[u8],
        index: u32,
        is_sync: Option<bool>,
        frame_start_samples: i64,
    ) {
        let topology = match Ac4Topology::parse(frame) {
            Ok(topology) => topology,
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                if self.first_error.is_none() {
                    self.first_error = Some(format!("帧 {index}：{error}"));
                }
                self.state.mark_discontinuity(ResetReason::ParseFailure);
                self.reset_decoder_history();
                return;
            }
        };
        self.parsed = self.parsed.saturating_add(1);

        if validate_group_references(&topology).is_err() {
            self.dangling_group_refs = self.dangling_group_refs.saturating_add(1);
        }
        if validate_substream_references(&topology).is_err() {
            self.substream_reference_failures = self.substream_reference_failures.saturating_add(1);
        }

        // 4.3.3.2.11：payload_base 相对字节对齐的 ac4_toc 末尾计
        let toc_bytes = topology.bits_consumed.div_ceil(8);
        let payload_start = toc_bytes.saturating_add(u64::from(topology.toc.payload_base));
        let declared: u64 = topology
            .index_table
            .sizes()
            .iter()
            .fold(0u64, |acc, size| acc.saturating_add(u64::from(size.bytes)));
        if payload_start.saturating_add(declared) > frame.len() as u64 {
            self.size_overruns = self.size_overruns.saturating_add(1);
        }

        let random_access = topology.random_access();
        match random_access {
            RandomAccess::Full => {
                self.full_random_access = self.full_random_access.saturating_add(1);
            }
            RandomAccess::AudioOnly => {
                self.audio_only_random_access = self.audio_only_random_access.saturating_add(1);
            }
            RandomAccess::None => {}
        }
        if is_sync.is_some_and(|sync| sync != (random_access == RandomAccess::Full)) {
            self.stss_random_access_mismatches =
                self.stss_random_access_mismatches.saturating_add(1);
        }

        // 状态机同时处理配置代次、来源变化与随机访问门禁。
        let transition = self.state.observe(&topology);
        self.config_generations = transition.generation;
        if transition.sequence == SequenceTransition::SourceChange {
            self.source_changes = self.source_changes.saturating_add(1);
        }
        match transition.action {
            DecoderAction::Reset { .. } => {
                self.reset_events = self.reset_events.saturating_add(1);
                self.reset_decoder_history();
            }
            DecoderAction::WaitForRandomAccess { .. } => {
                self.waiting_for_random_access_frames =
                    self.waiting_for_random_access_frames.saturating_add(1);
                self.reset_decoder_history();
            }
            DecoderAction::Continue => {}
        }

        // `frames_differing_from_first` 按完整规范化指纹计数，不再只比较
        // 路径和结构数量。
        let fingerprint = topology.config_fingerprint();
        if let Some(first) = self.first_fingerprint {
            if fingerprint != first {
                self.path_changes = self.path_changes.saturating_add(1);
            }
        } else {
            self.first_fingerprint = Some(fingerprint);
        }

        let group_oamd = self.oamd.observe(frame, &topology, index, is_sync);
        self.audio.observe(frame, &topology, index);
        self.ajoc_audio
            .observe_at(frame, &topology, index, &group_oamd, frame_start_samples);

        if self.delay.is_none() {
            self.delay = topology.toc.decoding_delay();
        }

        let path = topology.scene_path();
        let presentations = topology.presentations().len();
        let groups = topology.groups().len();
        let objects = topology.total_objects();

        if self.path.is_none() {
            self.path = Some(path);
            self.n_presentations = Some(presentations);
            self.n_groups = Some(groups);
            self.total_objects = Some(objects);
            self.detail = describe_topology(&topology);
        }
    }

    pub(super) fn record_parse_failure(&mut self, index: u32, message: &str) {
        self.failures = self.failures.saturating_add(1);
        if self.first_error.is_none() {
            self.first_error = Some(format!("帧 {index}：{message}"));
        }
        self.state.mark_discontinuity(ResetReason::ParseFailure);
        self.reset_decoder_history();
    }
}

/// 把首帧拓扑展开为 JSON，供人工核对与回归比对。
pub(crate) fn describe_topology(topology: &Ac4Topology) -> String {
    let mut out = String::from("{\n      \"presentations\": [");
    for (index, presentation) in topology.presentations().iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let config = presentation.presentation_config.map_or_else(
            || "null".to_owned(),
            |value| {
                format!(
                    "{{\"value\": {value}, \"role\": \"{}\"}}",
                    presentation_config_label(value)
                )
            },
        );
        let groups: String = presentation
            .group_indices()
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            concat!(
                "{{\"index\": {}, \"single_group\": {}, \"config\": {}, ",
                "\"version\": {}, \"md_compat\": {}, \"frame_rate_factor\": {}, ",
                "\"frame_rate_fraction\": {}, \"group_indices\": [{}], ",
                "\"presentation_substream\": {}}}"
            ),
            index,
            presentation.single_substream_group,
            config,
            presentation.presentation_version,
            presentation
                .md_compat
                .map_or_else(|| "null".to_owned(), |value| value.to_string()),
            presentation.frame_rate_factor,
            presentation.frame_rate_fraction,
            groups,
            presentation.substream.map_or_else(
                || "null".to_owned(),
                |substream| format!(
                    "{{\"index\": {}, \"ndot\": {}}}",
                    substream.substream_index, substream.ndot
                )
            ),
        ));
    }
    out.push_str("],\n      \"substream_groups\": [");

    for (index, group) in topology.groups().iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let mut substreams = String::from("[");
        for (position, info) in group.substreams().iter().enumerate() {
            if position > 0 {
                substreams.push_str(", ");
            }
            let extra = match *info {
                SubstreamInfo::Chan(ref chan) => format!(
                    ", \"channel_mode\": \"{}\", \"ch_mode\": {}",
                    chan.channel_mode.label().unwrap_or("reserved"),
                    chan.channel_mode.ch_mode
                ),
                SubstreamInfo::Ajoc(ref ajoc) => {
                    // core 与 full 两种解码模式各自独立声明对象构成，
                    // 见 6.3.2.10.8.2 NOTE 1
                    let dmx = ajoc.dmx_assignment.map_or_else(
                        || "null".to_owned(),
                        |assignment| {
                            format!(
                                "{{\"bed\": {}, \"isf\": {}, \"dynamic\": {}}}",
                                assignment.n_bed,
                                assignment.n_isf,
                                assignment.n_dynamic()
                            )
                        },
                    );
                    format!(
                        concat!(
                            ", \"lfe\": {}, \"static_dmx\": {}, \"dmx_signals\": {}, ",
                            "\"dmx_assignment\": {}, \"upmix_signals\": {}, ",
                            "\"bed\": {}, \"isf\": {}, \"dynamic\": {}"
                        ),
                        ajoc.b_lfe,
                        ajoc.static_dmx,
                        ajoc.n_dmx_signals,
                        dmx,
                        ajoc.n_upmix_signals,
                        ajoc.upmix_assignment.n_bed,
                        ajoc.upmix_assignment.n_isf,
                        ajoc.upmix_assignment.n_dynamic(),
                    )
                }
                SubstreamInfo::Obj(ref obj) => format!(
                    ", \"lfe\": {}, \"dynamic_objects\": {}, \"bed\": {}, \"isf\": {}",
                    obj.b_lfe, obj.dynamic_objects, obj.n_bed, obj.n_isf
                ),
            };
            substreams.push_str(&format!(
                "{{\"kind\": \"{}\", \"substream_index\": {}, \"ndot\": {}, \"objects\": {}{}}}",
                info.kind(),
                info.substream_index()
                    .map_or_else(|| "null".to_owned(), |value| value.to_string()),
                info.audio_ndot(),
                info.n_objects(),
                extra,
            ));
        }
        substreams.push(']');

        out.push_str(&format!(
            concat!(
                "{{\"index\": {}, \"channel_coded\": {}, \"n_lf_substreams\": {}, ",
                "\"oamd_substream\": {}, \"objects\": {}, \"substreams\": {}}}"
            ),
            index,
            group.channel_coded,
            group.n_lf_substreams,
            group.oamd_substream.map_or_else(
                || "null".to_owned(),
                |oamd| format!(
                    "{{\"index\": {}, \"ndot\": {}}}",
                    oamd.substream_index
                        .map_or_else(|| "null".to_owned(), |value| value.to_string()),
                    oamd.ndot
                )
            ),
            group.n_objects(),
            substreams,
        ));
    }

    let sizes: String = topology
        .index_table
        .sizes()
        .iter()
        .map(|size| size.bytes.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        concat!(
            "],\n      \"substream_index_table\": {{\"n_substreams\": {}, ",
            "\"size_present\": {}, \"sizes\": [{}]}},\n",
            "      \"toc_bits\": {}\n    }}"
        ),
        topology.index_table.n_substreams,
        topology.index_table.size_present,
        sizes,
        topology.bits_consumed,
    ));
    out
}
