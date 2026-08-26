//! 普通音频子流框架与 metadata trace。
//!
use super::{Ac4AudioSubstream, Ac4Topology, MAX_SUBSTREAMS, SubstreamContext, SubstreamInfo};

/// `ac4_substream()` 框架与 metadata 的统计。
///
/// 按 `TS103190-1:v1.4.1:4.3.4.1` 用 `audio_size` 跳过音频数据直达 metadata，
/// 不解码音频。判定条件是解析后必须恰好落在 substream 末尾。
#[derive(Debug, Default)]
pub(crate) struct AudioTrace {
    /// 成功定位音频 substream 载荷的帧数。
    pub(super) located: u32,
    /// 成功解析框架与 metadata 的帧数。
    pub(super) parsed: u32,
    /// 解析失败的帧数。
    pub(super) failures: u32,
    pub(super) first_error: Option<String>,
    /// `audio_size` 的最小值与最大值。
    pub(super) min_audio_size: Option<u32>,
    pub(super) max_audio_size: Option<u32>,
    /// metadata 区段字节数的最小值与最大值。
    pub(super) min_metadata_bytes: Option<u32>,
    pub(super) max_metadata_bytes: Option<u32>,
    /// `tools_metadata_size` 的最大值，单位为比特。
    pub(super) max_tools_bits: u32,
    /// 传输了 `dialnorm_bits` 的帧数；sus_ver = 1 下应为 0。
    pub(super) dialnorm_frames: u32,
    /// 传输了 `substream_loudness_bits` 的帧数。
    pub(super) substream_loudness_frames: u32,
    /// 首帧的 metadata 摘要。
    pub(super) first_detail: Option<String>,
    /// 已由非歧义帧确认的物理 substream metadata 解析上下文。
    ///
    /// DEE IMS 允许 7.1/stereo 两个候选，但同一配置代次内不能逐帧切换语法。
    selected_contexts: [Option<SubstreamContext>; MAX_SUBSTREAMS],
}

/// 该 group 是否被某个 alternative presentation 引用。
///
/// alternative 属性只沿实际引用关系传播：一个物理 substream 可被多个
/// group/presentation 共享，未引用它的 presentation 不应影响它的解析上下文。
pub(crate) fn group_is_alternative(topology: &Ac4Topology, group_index: usize) -> bool {
    topology.presentations().iter().any(|presentation| {
        presentation
            .substream
            .is_some_and(|substream| substream.alternative)
            && presentation
                .group_indices()
                .iter()
                .any(|&referenced| usize::try_from(referenced) == Ok(group_index))
    })
}

/// DEE 的 IMS presentation v2 以 `ch_mode = 6` 描述沉浸式呈现，但其物理
/// audio substream 的 metadata 仍使用 stereo 分支。
///
/// 这里只增加一个兼容候选，不覆盖 TOC 声明；由
/// [`Ac4AudioSubstream::parse`] 的严格子流末尾校验筛选，并在首个非歧义帧后
/// 固定选择。DEE legacy IMS 同时引用 7.1 与 stereo group，共享同一物理载荷，
/// 因此也为该兼容关系提供了可观测的对照。
fn group_needs_dee_ims_stereo_candidate(
    topology: &Ac4Topology,
    group_index: usize,
    channel_mode: u32,
) -> bool {
    channel_mode == 6
        && topology.presentations().iter().any(|presentation| {
            presentation.presentation_version == 2
                && presentation
                    .group_indices()
                    .iter()
                    .any(|&referenced| usize::try_from(referenced) == Ok(group_index))
        })
}

fn is_known_dee_ims_pair(current: SubstreamContext, candidate: SubstreamContext) -> bool {
    current.sus_ver == candidate.sus_ver
        && !current.ajoc
        && !candidate.ajoc
        && matches!(
            (current.channel_mode, candidate.channel_mode),
            (Some(1), Some(6)) | (Some(6), Some(1))
        )
}

fn push_context(
    slot: &mut Vec<SubstreamContext>,
    candidate: SubstreamContext,
) -> Result<(), &'static str> {
    if let Some(current) = slot.iter_mut().find(|current| {
        current.sus_ver == candidate.sus_ver
            && current.ajoc == candidate.ajoc
            && current.channel_mode == candidate.channel_mode
    }) {
        // 只要任一实际引用它的 presentation 是 alternative，该语法上下文就
        // 携带 alternative metadata。
        current.alternative |= candidate.alternative;
        return Ok(());
    }

    if slot
        .iter()
        .copied()
        .all(|current| is_known_dee_ims_pair(current, candidate))
    {
        slot.push(candidate);
        Ok(())
    } else {
        Err("Conflicting parse contexts for the same audio substream")
    }
}

fn select_parsed_candidate(
    selected: &mut Option<SubstreamContext>,
    successful: &[(SubstreamContext, Ac4AudioSubstream)],
) -> Result<Option<Ac4AudioSubstream>, &'static str> {
    if successful.is_empty() {
        return Ok(None);
    }

    if let Some(expected) = *selected {
        return successful
            .iter()
            .find_map(|(context, parsed)| (*context == expected).then_some(*parsed))
            .map(Some)
            .ok_or("Audio-substream parse context changed between frames");
    }

    if let [(context, parsed)] = successful {
        *selected = Some(*context);
        return Ok(Some(*parsed));
    }

    // 多个候选同时落在 substream 末尾时尚不能判定真实语法；保留未选择状态，
    // 等后续首个非歧义帧确认，而不是依赖候选顺序永久锁定。
    Ok(successful.first().map(|(_, parsed)| *parsed))
}

impl AudioTrace {
    pub(super) fn observe(&mut self, frame: &[u8], topology: &Ac4Topology, index: u32) {
        let mut contexts: [Vec<SubstreamContext>; MAX_SUBSTREAMS] =
            core::array::from_fn(|_| Vec::new());
        let mut frame_failed = false;

        // 先按物理 substream 汇总上下文，再解析载荷。DEE legacy IMS 会让同一
        // 物理载荷由 7.1 与 stereo group 共同引用；只对这组已知兼容关系保留
        // 两个候选，其他语法族或声道模式冲突仍然失败关闭。
        for (group_index, group) in topology.groups().iter().enumerate() {
            let alternative = group_is_alternative(topology, group_index);

            for info in group.substreams() {
                let (Some(first_substream_index), ajoc, channel_mode) = (match *info {
                    SubstreamInfo::Ajoc(ref ajoc) => (ajoc.substream_index(), true, None),
                    SubstreamInfo::Obj(ref obj) => (obj.substream_index(), false, None),
                    // 声道编码的 substream 由 channel_mode 决定 metadata 分支。
                    SubstreamInfo::Chan(ref chan) => (
                        chan.substream_index(),
                        false,
                        Some(chan.channel_mode.ch_mode),
                    ),
                }) else {
                    continue;
                };

                for offset in 0..group.frame_rate_factor {
                    let Some(substream_index) = first_substream_index.checked_add(offset) else {
                        frame_failed = true;
                        self.remember_failure(
                            index,
                            "Audio-substream index overflow after applying the frame-rate offset",
                        );
                        continue;
                    };

                    let slot_index = usize::try_from(substream_index).unwrap_or(usize::MAX);
                    let Some(slot) = contexts.get_mut(slot_index) else {
                        frame_failed = true;
                        self.remember_failure(
                            index,
                            "Audio-substream index exceeds statistics capacity",
                        );
                        continue;
                    };
                    let candidate = SubstreamContext {
                        // bitstream_version = 2 下 sus_ver 恒为 1，见 6.3.2.5.4。
                        sus_ver: 1,
                        alternative,
                        ajoc,
                        channel_mode,
                    };
                    if let Err(error) = push_context(slot, candidate) {
                        frame_failed = true;
                        self.remember_failure(index, error);
                        continue;
                    }

                    if channel_mode.is_some_and(|mode| {
                        group_needs_dee_ims_stereo_candidate(topology, group_index, mode)
                    }) && let Err(error) = push_context(
                        slot,
                        SubstreamContext {
                            channel_mode: Some(1),
                            ..candidate
                        },
                    ) {
                        frame_failed = true;
                        self.remember_failure(index, error);
                    }
                }
            }
        }

        let has_audio = contexts.iter().any(|slot| !slot.is_empty());
        let mut frame_located = has_audio && !frame_failed;
        let mut frame_parsed = has_audio && !frame_failed;
        let mut frame_dialnorm = false;
        let mut frame_substream_loudness = false;

        for (substream_index, slot) in contexts.iter().enumerate() {
            if slot.is_empty() {
                continue;
            }
            let index_u32 = u32::try_from(substream_index).unwrap_or(u32::MAX);
            let payload = match topology.substream_payload(frame, index_u32) {
                Ok(payload) => payload,
                Err(error) => {
                    frame_located = false;
                    frame_parsed = false;
                    frame_failed = true;
                    self.remember_failure(index, &format!("Location failed: {error}"));
                    continue;
                }
            };

            let mut successful = Vec::new();
            let mut candidate_errors = Vec::new();
            for context in slot.iter().copied() {
                match Ac4AudioSubstream::parse(payload, context) {
                    Ok(parsed) => {
                        successful.push((context, parsed));
                    }
                    Err(error) => candidate_errors.push(format!("{context:?}: {error}")),
                }
            }

            let selection = self
                .selected_contexts
                .get_mut(substream_index)
                .map_or(Ok(None), |selected| {
                    select_parsed_candidate(selected, &successful)
                });

            if let Ok(Some(parsed)) = selection {
                frame_dialnorm |= parsed.basic.dialnorm_bits.is_some();
                frame_substream_loudness |= parsed.basic.substream_loudness_bits.is_some();
                // 一个物理 substream 只统计一次；slot 中的多个 context 是互斥
                // 候选，不是需要重复解析和累计的多份载荷。
                self.record_payload(&parsed, index);
            } else {
                frame_parsed = false;
                frame_failed = true;
                let message = match selection {
                    Err(error) => format!("Audio substream {substream_index}: {error}"),
                    Ok(_) => format!(
                        "Every parse context failed for audio substream {substream_index}: {}",
                        candidate_errors.join("; ")
                    ),
                };
                self.remember_failure(index, &message);
            }
        }

        if frame_located {
            self.located = self.located.saturating_add(1);
        }
        if frame_parsed {
            self.parsed = self.parsed.saturating_add(1);
        }
        if frame_failed {
            self.failures = self.failures.saturating_add(1);
        }
        if frame_dialnorm {
            self.dialnorm_frames = self.dialnorm_frames.saturating_add(1);
        }
        if frame_substream_loudness {
            self.substream_loudness_frames = self.substream_loudness_frames.saturating_add(1);
        }
    }

    pub(super) fn record_payload(&mut self, parsed: &Ac4AudioSubstream, index: u32) {
        self.min_audio_size = Some(
            self.min_audio_size
                .map_or(parsed.audio_size, |value| value.min(parsed.audio_size)),
        );
        self.max_audio_size = Some(
            self.max_audio_size
                .map_or(parsed.audio_size, |value| value.max(parsed.audio_size)),
        );
        self.min_metadata_bytes = Some(
            self.min_metadata_bytes
                .map_or(parsed.metadata_bytes, |v| v.min(parsed.metadata_bytes)),
        );
        self.max_metadata_bytes = Some(
            self.max_metadata_bytes
                .map_or(parsed.metadata_bytes, |v| v.max(parsed.metadata_bytes)),
        );
        self.max_tools_bits = self.max_tools_bits.max(parsed.tools_metadata_bits);
        if self.first_detail.is_none() {
            self.first_detail = Some(format!(
                "{{\"frame\": {index}, \"audio_size\": {}, \"metadata_bytes\": {}, \
                 \"tools_metadata_bits\": {}, \"substream_loudness_bits\": {}, \
                 \"dialog\": {}, \"channels_classifier\": {}, \"dc_block_on\": {}}}",
                parsed.audio_size,
                parsed.metadata_bytes,
                parsed.tools_metadata_bits,
                parsed
                    .basic
                    .substream_loudness_bits
                    .map_or_else(|| "null".to_owned(), |v| v.to_string()),
                parsed.extended.dialog,
                parsed.extended.channels_classifier,
                parsed
                    .basic
                    .dc_block_on
                    .map_or_else(|| "null".to_owned(), |v| v.to_string()),
            ));
        }
    }

    pub(super) fn remember_failure(&mut self, index: u32, message: &str) {
        if self.first_error.is_none() {
            self.first_error = Some(format!("Frame {index}: {message}"));
        }
    }

    pub(super) fn reset_history(&mut self) {
        self.selected_contexts.fill(None);
    }

    pub(super) fn to_json(&self) -> String {
        let error = self
            .first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let detail = self
            .first_detail
            .as_ref()
            .map_or_else(|| "null".to_owned(), Clone::clone);
        let opt = |value: Option<u32>| value.map_or_else(|| "null".to_owned(), |v| v.to_string());
        format!(
            "{{\"located\": {}, \"parsed\": {}, \"failures\": {}, \"first_error\": {error}, \
             \"min_audio_size\": {}, \"max_audio_size\": {}, \
             \"min_metadata_bytes\": {}, \"max_metadata_bytes\": {}, \
             \"max_tools_metadata_bits\": {}, \"dialnorm_frames\": {}, \
             \"substream_loudness_frames\": {}, \"first_detail\": {detail}}}",
            self.located,
            self.parsed,
            self.failures,
            opt(self.min_audio_size),
            opt(self.max_audio_size),
            opt(self.min_metadata_bytes),
            opt(self.max_metadata_bytes),
            self.max_tools_bits,
            self.dialnorm_frames,
            self.substream_loudness_frames,
        )
    }
}

impl AudioTrace {
    pub(super) const fn new() -> Self {
        Self {
            located: 0,
            parsed: 0,
            failures: 0,
            first_error: None,
            min_audio_size: None,
            max_audio_size: None,
            min_metadata_bytes: None,
            max_metadata_bytes: None,
            max_tools_bits: 0,
            dialnorm_frames: 0,
            substream_loudness_frames: 0,
            first_detail: None,
            selected_contexts: [None; MAX_SUBSTREAMS],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    const SURROUND: SubstreamContext = SubstreamContext {
        sus_ver: 1,
        alternative: false,
        ajoc: false,
        channel_mode: Some(6),
    };
    const STEREO: SubstreamContext = SubstreamContext {
        channel_mode: Some(1),
        ..SURROUND
    };

    fn minimal(context: SubstreamContext) -> Ac4AudioSubstream {
        Ac4AudioSubstream::parse(&[0, 0, 0, 0], context).expect("最小 metadata 应可解析")
    }

    #[test]
    fn context_candidates_only_allow_the_known_dee_ims_pair() {
        let mut slot = Vec::new();
        push_context(&mut slot, SURROUND).unwrap();
        push_context(&mut slot, STEREO).unwrap();
        assert_eq!(slot.len(), 2);

        let ajoc = SubstreamContext {
            ajoc: true,
            channel_mode: None,
            ..SURROUND
        };
        assert!(push_context(&mut slot, ajoc).is_err());

        let mut unrelated_modes = Vec::new();
        push_context(&mut unrelated_modes, STEREO).unwrap();
        assert!(
            push_context(
                &mut unrelated_modes,
                SubstreamContext {
                    channel_mode: Some(3),
                    ..SURROUND
                }
            )
            .is_err()
        );
    }

    #[test]
    fn ambiguous_context_waits_for_evidence_and_then_cannot_switch() {
        let surround = minimal(SURROUND);
        let stereo = minimal(STEREO);
        let mut selected = None;

        assert!(
            select_parsed_candidate(&mut selected, &[(SURROUND, surround), (STEREO, stereo)])
                .unwrap()
                .is_some()
        );
        assert_eq!(selected, None, "歧义帧不应按候选顺序锁定上下文");

        assert!(
            select_parsed_candidate(&mut selected, &[(STEREO, stereo)])
                .unwrap()
                .is_some()
        );
        assert_eq!(selected, Some(STEREO));
        assert!(select_parsed_candidate(&mut selected, &[(SURROUND, surround)]).is_err());
    }

    /// 兼容候选的适用范围由 presentation v2 界定，不是对所有 7.1 group 生效。
    ///
    /// 放宽这个条件不会让任何既有判据变红——两个夹具的 group 0 都是 ch_mode
    /// 6，差别只在 presentation version（v2 与 v1）。少了这条，`>= 1` 之类的
    /// 放宽会给任意 presentation 引用的 7.1 group 平白多出一个 stereo 候选，
    /// 把本该报冲突的码流悄悄放过去。
    #[test]
    fn the_stereo_candidate_is_scoped_to_presentation_v2() {
        let (_, ims_v2) = topology_with_ims_v2_stereo_metadata();
        assert_eq!(
            ims_v2
                .presentations()
                .first()
                .map(|p| p.presentation_version),
            Some(2)
        );
        assert!(
            group_needs_dee_ims_stereo_candidate(&ims_v2, 0, 6),
            "v2 presentation 的 7.1 group 需要 stereo 兼容候选"
        );

        // 只差一个版本比特的同形码流：v1 不得获得候选。用 v1 而不是 v0 做反例，
        // 是因为把条件放宽成 `>= 1` 时 v0 仍然为假，反例会一起沉默。
        let (v1_frame, ims_v1) = topology_with_ims_v1_stereo_metadata();
        assert_eq!(
            ims_v1
                .presentations()
                .first()
                .map(|p| p.presentation_version),
            Some(1)
        );
        assert!(
            !group_needs_dee_ims_stereo_candidate(&ims_v1, 0, 6),
            "非 v2 presentation 不得凭空获得 stereo 候选"
        );

        // 后果要可观察：该载荷只有 stereo 读法成立，没有候选就必须失败关闭。
        let mut trace = AudioTrace::new();
        trace.observe(&v1_frame, &ims_v1, 0);
        assert_eq!(trace.parsed, 0);
        assert_eq!(trace.failures, 1, "{:?}", trace.first_error);

        // 声道模式那一维同样要收窄：v2 也只对 ch_mode 6 补候选。
        assert!(!group_needs_dee_ims_stereo_candidate(&ims_v2, 0, 12));
    }

    /// 解码不连续必须丢弃已固定的解析上下文。
    ///
    /// [`AudioTrace::selected_contexts`] 是跨帧状态：seek 或解析失败之后若不
    /// 清，上一段选定的读法会继续压制新段的候选筛选。这条判据挂在
    /// [`TopologyTrace`] 上而不是直接调 `reset_history`，因为要验的正是那一
    /// 行调用有没有接上——只测 `reset_history` 本身，把调用删掉照样全绿。
    #[test]
    fn a_decoder_reset_drops_the_locked_audio_context() {
        let (frame, _) = topology_with_ims_v2_stereo_metadata();
        let mut trace = TopologyTrace::new();

        trace.observe(&frame, 0, Some(true));
        let locked = trace
            .audio
            .selected_contexts
            .get(1)
            .copied()
            .flatten()
            .expect("首帧不歧义，应固定读法");
        assert_eq!(locked.channel_mode, Some(1), "该载荷只有 stereo 读法成立");

        // 空帧解析失败即不连续，走 reset_decoder_history。
        trace.observe(&[], 1, Some(false));
        assert_eq!(
            trace.audio.selected_contexts.get(1).copied().flatten(),
            None,
            "重置后不得保留上一段的上下文选择"
        );
    }
}
