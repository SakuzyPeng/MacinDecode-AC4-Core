//! A-JOC trace 的帧级与 A-SPX 可达性统计。

#[cfg(feature = "audio-decode")]
use super::{
    Ac4SubstreamAjoc, AjocTrace, AjocTraceContext, AspxReach, FullAjocAsfError,
    FullAjocAsfErrorKind, FullAjocAsfFrameObservation, FullAjocAsfStage, FullAjocAudioFrameError,
    FullAjocAudioFrameInput, FullAjocBlocker, FullAjocDecodeError, FullAjocDecodeErrorKind,
    FullAjocDecodeMode, FullAjocFrameProvenance, FullAjocObservation, FullAjocSyntaxFrameInput,
    FullAjocSyntaxObservation, ReconstructionInvariant, ScaledStats, companding_is_active,
};
#[cfg(feature = "audio-decode")]
use macindecode_ac4_bitstream::substream_audio::AjocSubstreamContext;

/// 一条物理 A-JOC substream 的成功观察结果。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AjocObservation {
    pub(in crate::trace) intra_frame_updates: bool,
    pub(in crate::trace) companding_present: bool,
    pub(in crate::trace) companding_active: bool,
    pub(in crate::trace) aspx: AspxReach,
}

/// 一帧内跨 substream 的累积量。
///
/// 帧级计数与 substream 级计数是两套单位：一帧可含多条物理 A-JOC
/// substream，`frames`、`parsed`、`failures`、`intra_frame_update_frames` 与
/// 两个 `companding_*_frames` 每帧最多变动一次，`substreams` 与
/// `parsed_substreams` 则逐条累加。把帧级字段写在逐条的循环里会按 substream
/// 数重复计数；实测码流每帧恰好一条 substream，两套单位恒相等，落点门禁
/// 抓不到这类错误。
#[cfg(feature = "audio-decode")]
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FrameTally {
    /// 本帧声明存在的物理 substream 条数。
    pub(in crate::trace) declared: usize,
    /// 其中解析成功的条数。
    pub(in crate::trace) parsed: usize,
    /// 任一条带帧内多块。
    pub(in crate::trace) intra_frame_updates: bool,
    /// 任一条传输了 `companding_control()`。
    pub(in crate::trace) companding_present: bool,
    /// 任一条启用了逐声道或平均压扩。
    pub(in crate::trace) companding_active: bool,
    /// 任一条触发了对应的 A-SPX 分支。
    pub(in crate::trace) aspx: AspxReach,
    /// 任一环节失败，含未进入解析的上下文冲突。
    pub(in crate::trace) failed: bool,
}

#[cfg(feature = "audio-decode")]
impl FrameTally {
    pub(in crate::trace) fn observe(&mut self, observation: Option<AjocObservation>) {
        match observation {
            Some(observation) => {
                self.parsed = self.parsed.saturating_add(1);
                self.intra_frame_updates |= observation.intra_frame_updates;
                self.companding_present |= observation.companding_present;
                self.companding_active |= observation.companding_active;
                self.aspx.merge(observation.aspx);
            }
            None => self.failed = true,
        }
    }
}

/// 把统一 Full engine 的逐路 ASF observation 合并进既有 trace JSON 计数。
///
/// engine 已在交出成功帧前拒绝任一非有限值或局部失败，因此这里不重复扫描
/// PCM，也不自持 overlap。`pcm_samples` 仍按借用 plane 的实际长度计数，使既有
/// `pcm_sample_conservation` 判据继续独立核对 observation 的解组长度。
#[cfg(feature = "audio-decode")]
fn append_engine_asf_observation(
    frame: FullAjocAsfFrameObservation<'_>,
    scale_factor_bands: &mut u64,
    scale_factor_min: &mut Option<u8>,
    scale_factor_max: &mut Option<u8>,
    stats: &mut ScaledStats,
) -> Result<(), String> {
    for index in 0..frame.channels() {
        let channel = frame.channel(index).ok_or_else(|| {
            format!(
                "Full engine 声明 {} 路 ASF observation，但第 {index} 路不可读取",
                frame.channels()
            )
        })?;
        let observation = channel.observation();

        *scale_factor_bands = scale_factor_bands
            .saturating_add(u64::try_from(observation.scale_factor_bands()).unwrap_or(u64::MAX));
        if let Some(value) = observation.scale_factor_min() {
            *scale_factor_min = Some(scale_factor_min.map_or(value, |low| low.min(value)));
        }
        if let Some(value) = observation.scale_factor_max() {
            *scale_factor_max = Some(scale_factor_max.map_or(value, |high| high.max(value)));
        }

        stats.lines = stats
            .lines
            .saturating_add(u64::try_from(observation.scaled_lines()).unwrap_or(u64::MAX));
        stats.peak = stats.peak.max(observation.scaled_peak());
        stats.ungrouped_lines = stats
            .ungrouped_lines
            .saturating_add(u64::try_from(observation.ungrouped_lines()).unwrap_or(u64::MAX));
        if observation.scaled_nonzero() != observation.ungrouped_nonzero() {
            stats.ungroup_count_mismatch = stats.ungroup_count_mismatch.saturating_add(1);
        }
        stats.ungroup_energy_drift = stats
            .ungroup_energy_drift
            .max(observation.ungroup_energy_drift());
        stats.pcm_frames = stats.pcm_frames.saturating_add(1);
        stats.pcm_samples = stats
            .pcm_samples
            .saturating_add(u64::try_from(channel.samples().len()).unwrap_or(u64::MAX));
        stats.pcm_peak = stats.pcm_peak.max(observation.pcm_peak());
        if observation.input_silent() {
            stats.silent_input_frames = stats.silent_input_frames.saturating_add(1);
        } else if observation.output_silent() {
            stats.zero_output_with_nonzero_input_frames = stats
                .zero_output_with_nonzero_input_frames
                .saturating_add(1);
        }
    }
    Ok(())
}

#[cfg(feature = "audio-decode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineAsfFailureClass {
    ScaleFactor,
    Scale,
    Ungroup,
    ScaledNonFinite,
    PcmNonFinite,
    Synthesis,
}

#[cfg(feature = "audio-decode")]
const fn engine_asf_failure_class(kind: FullAjocAsfErrorKind) -> EngineAsfFailureClass {
    match kind {
        FullAjocAsfErrorKind::ScaleFactors(_) => EngineAsfFailureClass::ScaleFactor,
        FullAjocAsfErrorKind::ScaleSpectrum(_) => EngineAsfFailureClass::Scale,
        FullAjocAsfErrorKind::UngroupSpectrum(_) => EngineAsfFailureClass::Ungroup,
        FullAjocAsfErrorKind::NonFinite {
            stage: FullAjocAsfStage::Pcm,
            ..
        } => EngineAsfFailureClass::PcmNonFinite,
        FullAjocAsfErrorKind::NonFinite {
            stage: FullAjocAsfStage::ScaledSpectrum | FullAjocAsfStage::UngroupedSpectrum,
            ..
        } => EngineAsfFailureClass::ScaledNonFinite,
        _ => EngineAsfFailureClass::Synthesis,
    }
}

#[cfg(feature = "audio-decode")]
impl AjocTrace {
    /// 把统一 engine 的结构化 ASF 失败折回既有 trace 不变量字段。
    fn record_engine_asf_failure(&mut self, frame_index: u32, error: FullAjocAsfError) {
        let detail = format!("帧 {frame_index} {error}");
        let occurrences = u64::try_from(error.nonfinite_samples().unwrap_or(1)).unwrap_or(u64::MAX);
        match engine_asf_failure_class(error.kind()) {
            EngineAsfFailureClass::ScaleFactor => {
                self.scale_factor_failures = self.scale_factor_failures.saturating_add(1);
                self.scale_factor_first_error.get_or_insert(detail);
            }
            EngineAsfFailureClass::Scale => {
                self.scaled_stats.scale_failures =
                    self.scaled_stats.scale_failures.saturating_add(1);
                self.scaled_stats.scale_first_error.get_or_insert(detail);
            }
            EngineAsfFailureClass::Ungroup => {
                self.scaled_stats.ungroup_failures =
                    self.scaled_stats.ungroup_failures.saturating_add(1);
                self.scaled_stats.ungroup_first_error.get_or_insert(detail);
            }
            EngineAsfFailureClass::ScaledNonFinite => {
                self.scaled_stats.nonfinite =
                    self.scaled_stats.nonfinite.saturating_add(occurrences);
            }
            EngineAsfFailureClass::PcmNonFinite => {
                self.scaled_stats.pcm_nonfinite =
                    self.scaled_stats.pcm_nonfinite.saturating_add(occurrences);
            }
            EngineAsfFailureClass::Synthesis => {
                self.scaled_stats.synthesis_failures =
                    self.scaled_stats.synthesis_failures.saturating_add(1);
                self.scaled_stats
                    .synthesis_first_error
                    .get_or_insert(detail);
            }
        }
    }

    fn record_syntax_fields(&mut self, parsed: &Ac4SubstreamAjoc, frame_index: u32) {
        self.min_fill_bits = Some(
            self.min_fill_bits
                .map_or(parsed.fill_bits, |value| value.min(parsed.fill_bits)),
        );
        self.max_fill_bits = Some(
            self.max_fill_bits
                .map_or(parsed.fill_bits, |value| value.max(parsed.fill_bits)),
        );
        self.max_dmx_blocks = self
            .max_dmx_blocks
            .max(parsed.audio.dmx_num_obj_info_blocks);
        self.max_umx_blocks = self
            .max_umx_blocks
            .max(parsed.audio.umx_num_obj_info_blocks);
        self.dmx_blocks = self
            .dmx_blocks
            .saturating_add(parsed.audio.dmx_blocks_written() as u64);
        self.umx_blocks = self
            .umx_blocks
            .saturating_add(parsed.audio.umx_blocks_written() as u64);
        if parsed.audio.derive_timing_from_dmx == Some(true) {
            self.derive_timing = self.derive_timing.saturating_add(1);
        }
        if parsed.audio.some_signals_inactive {
            self.inactive_signals = self.inactive_signals.saturating_add(1);
        }
        if parsed.audio.bed_info.is_some() {
            self.bed_info = self.bed_info.saturating_add(1);
        }
        if self.first_detail.is_none() {
            self.first_detail = Some(format!(
                "{{\"frame\": {frame_index}, \"audio_size\": {}, \"audio_data_bits\": {}, \
                 \"fill_bits\": {}, \"dmx_blocks\": {}, \"umx_blocks\": {}, \
                 \"codec_mode_aspx\": {}, \"channel_elements\": {}, \"aspx_elements\": {}}}",
                parsed.substream.audio_size,
                parsed.audio_data_bits,
                parsed.fill_bits,
                parsed.audio.dmx_num_obj_info_blocks,
                parsed.audio.umx_num_obj_info_blocks,
                parsed.audio.var_element.codec_mode_aspx,
                parsed.audio.var_element.channel_elements(),
                parsed.audio.var_element.aspx_elements(),
            ));
        }
    }

    /// 从统一 engine 的同一解析快照记录 trace 语法与 A-JOC 矩阵 census。
    fn record_engine_syntax(&mut self, syntax: FullAjocSyntaxObservation<'_>, frame_index: u32) {
        let parsed = syntax.parsed();
        self.record_syntax_fields(&parsed, frame_index);
        self.ajoc_matrix.observe(
            &parsed.audio.ajoc,
            syntax.object_controls(),
            syntax.matrices(),
            syntax
                .full_support()
                .as_ref()
                .err()
                .map(FullAjocBlocker::detail),
        );
    }

    /// feature trace 的单次统一 engine 入口。
    ///
    /// 语法、矩阵、raw OAMD、ASF 与 Full DSP 全部来自同一调用。engine 在
    /// A-SPX/Full 下游失败后保留最近一次成功前端 observation，因此 blocker
    /// 不会抹掉旧 trace 已经记录的语法或核心带统计，也无需第二次解析载荷。
    #[expect(
        clippy::too_many_arguments,
        reason = "trace 帧必须显式绑定载荷、拓扑引用、AU 时间与 LFE 位置"
    )]
    pub(in crate::trace) fn observe_traced_audio(
        &mut self,
        payload: &[u8],
        context: AjocSubstreamContext,
        candidate: AjocTraceContext,
        substream_index: u32,
        frame_index: u32,
        frame_start_samples: i64,
        physical_substreams: usize,
        lfe_position: Option<u32>,
    ) -> Option<AjocObservation> {
        let mut decoder = self
            .full_decoder
            .take()
            .expect("ObserveFull 构造器必须建立统一 engine");
        let input = FullAjocAudioFrameInput {
            syntax: FullAjocSyntaxFrameInput {
                payload,
                context,
                substream_index,
                physical_substreams,
            },
            provenance: FullAjocFrameProvenance::new(u64::from(frame_index))
                .with_source_sample_start(frame_start_samples),
            lfe_position,
            mode: FullAjocDecodeMode::ObserveFull,
        };

        let mut reset_after_commit = false;
        let observation = match decoder.decode_audio_frame(input) {
            Ok(decoded) => {
                let syntax = decoded.frontend().syntax().observation();
                match self.commit_engine_syntax_observation(
                    syntax,
                    candidate,
                    substream_index,
                    frame_index,
                ) {
                    Ok(observation) => {
                        let asf = append_engine_asf_observation(
                            decoded.frontend().asf().observation(),
                            &mut self.scale_factor_bands,
                            &mut self.scale_factor_min,
                            &mut self.scale_factor_max,
                            &mut self.scaled_stats,
                        );
                        let result = match asf {
                            Ok(()) => Ok(decoded.output().observation()),
                            Err(error) => {
                                reset_after_commit = true;
                                Err(FullAjocAudioFrameError::Decode(
                                    FullAjocDecodeError::object_shape(error),
                                ))
                            }
                        };
                        self.commit_full_audio_frame(substream_index, frame_index, result);
                        Some(observation)
                    }
                    Err(error) => {
                        reset_after_commit = true;
                        self.state_failures = self.state_failures.saturating_add(1);
                        self.remember_failure(frame_index, &error);
                        None
                    }
                }
            }
            Err(error) => {
                let Some(syntax) = decoder.last_syntax_observation() else {
                    self.remember_failure(frame_index, &error.to_string());
                    self.full_decoder = Some(decoder);
                    self.reset_substream(substream_index);
                    return None;
                };
                let aspx_support = syntax.aspx_support();
                match self.commit_engine_syntax_observation(
                    syntax,
                    candidate,
                    substream_index,
                    frame_index,
                ) {
                    Ok(observation) => {
                        if let Some(asf) = decoder.last_asf_observation() {
                            if let Err(shape) = append_engine_asf_observation(
                                asf,
                                &mut self.scale_factor_bands,
                                &mut self.scale_factor_min,
                                &mut self.scale_factor_max,
                                &mut self.scaled_stats,
                            ) {
                                self.object_shape_mismatches =
                                    self.object_shape_mismatches.saturating_add(1);
                                self.object_shape_first_error
                                    .get_or_insert_with(|| format!("帧 {frame_index}：{shape}"));
                            }
                        }
                        if let Err(blocker) = aspx_support {
                            if let FullAjocAudioFrameError::Asf(asf) = error {
                                self.record_engine_asf_failure(frame_index, asf);
                            }
                            self.commit_aspx_support(Err(blocker), substream_index, frame_index);
                        } else {
                            self.commit_full_audio_frame(substream_index, frame_index, Err(error));
                        }
                        Some(observation)
                    }
                    Err(error) => {
                        reset_after_commit = true;
                        self.state_failures = self.state_failures.saturating_add(1);
                        self.remember_failure(frame_index, &error);
                        None
                    }
                }
            }
        };

        self.full_decoder = Some(decoder);
        if reset_after_commit {
            self.reset_substream(substream_index);
        }
        observation
    }

    fn commit_engine_syntax_observation(
        &mut self,
        syntax: FullAjocSyntaxObservation<'_>,
        candidate: AjocTraceContext,
        substream_index: u32,
        frame_index: u32,
    ) -> Result<AjocObservation, String> {
        let parsed = syntax.parsed();
        self.apply_object_states_from_blocks(
            &parsed,
            syntax.dmx_blocks(),
            syntax.umx_blocks(),
            candidate,
            substream_index,
            frame_index,
        )?;
        self.record_engine_syntax(syntax, frame_index);

        let companding = parsed.audio.var_element.companding.as_ref();
        Ok(AjocObservation {
            intra_frame_updates: parsed.audio.dmx_num_obj_info_blocks > 1
                || parsed.audio.umx_num_obj_info_blocks > 1,
            companding_present: companding.is_some(),
            companding_active: companding.is_some_and(companding_is_active),
            aspx: parsed.audio.var_element.aspx_reach(),
        })
    }

    /// 把完整 engine 的前端或 DSP 失败折回既有 trace 诊断契约。
    fn commit_full_audio_frame(
        &mut self,
        substream_index: u32,
        frame_index: u32,
        result: Result<FullAjocObservation, FullAjocAudioFrameError>,
    ) {
        match result {
            Ok(observation) => {
                self.commit_aspx_frame(substream_index, frame_index, Ok(observation));
            }
            Err(FullAjocAudioFrameError::Decode(error)) => {
                self.commit_aspx_frame(substream_index, frame_index, Err(error));
            }
            Err(FullAjocAudioFrameError::Asf(error)) => {
                self.record_engine_asf_failure(frame_index, error);
                self.fail_aspx(substream_index, frame_index, format!("ASF 前端：{error}"));
            }
            Err(error) => {
                self.fail_aspx(substream_index, frame_index, error.to_string());
            }
        }
    }

    /// 提交一帧的驱动结果：失败按 substream 整条失效并计入不变量。
    ///
    /// 与拦截共用 [`AjocTrace::fail_aspx`]，两条失败路径因此不会分叉。
    /// 单独成一个方法是为了让完整入口的 `Decode` 分支与测试都只使用一套计数
    /// 映射。Syntax 前端失败由上一层归入公共驱动门禁；ASF 失败会先恢复既有
    /// 细分不变量，再进入同一公共门禁。
    pub(in crate::trace) fn commit_aspx_frame(
        &mut self,
        substream_index: u32,
        frame_index: u32,
        result: Result<FullAjocObservation, FullAjocDecodeError>,
    ) {
        match result {
            Ok(observation) => {
                self.ajoc_full_warmup_frames = self
                    .ajoc_full_warmup_frames
                    .saturating_add(u32::from(observation.warmup()));
                self.ajoc_full_reconstructed_frames = self
                    .ajoc_full_reconstructed_frames
                    .saturating_add(u32::from(observation.reconstructed()));
                self.ajoc_full_wet_frames = self
                    .ajoc_full_wet_frames
                    .saturating_add(u32::from(observation.wet()));
            }
            Err(error) => {
                let detail = error.detail().to_owned();
                let remembered = format!("帧 {frame_index}：{detail}");
                match error.kind() {
                    FullAjocDecodeErrorKind::Other => {}
                    FullAjocDecodeErrorKind::Unsupported => {
                        if self.full_unsupported_first_error.is_none() {
                            self.full_unsupported_first_error = Some(remembered);
                        }
                    }
                    FullAjocDecodeErrorKind::Reconstruction => {
                        self.ajoc_reconstruction_failures =
                            self.ajoc_reconstruction_failures.saturating_add(1);
                        if self.ajoc_reconstruction_first_error.is_none() {
                            self.ajoc_reconstruction_first_error = Some(remembered);
                        }
                    }
                    FullAjocDecodeErrorKind::ObjectsNonFinite => {
                        self.objects_nonfinite = self.objects_nonfinite.saturating_add(1);
                        if self.objects_nonfinite_first_error.is_none() {
                            self.objects_nonfinite_first_error = Some(remembered);
                        }
                    }
                    FullAjocDecodeErrorKind::ObjectShapeMismatch => {
                        self.object_shape_mismatches =
                            self.object_shape_mismatches.saturating_add(1);
                        if self.object_shape_first_error.is_none() {
                            self.object_shape_first_error = Some(remembered);
                        }
                    }
                    _ => {}
                }
                self.fail_aspx(substream_index, frame_index, detail);
            }
        }
    }

    pub(in crate::trace) fn remember_failure(&mut self, index: u32, message: &str) {
        if self.first_error.is_none() {
            self.first_error = Some(format!("帧 {index}：{message}"));
        }
    }

    pub(in crate::trace) fn to_json(&self) -> String {
        let error = self
            .first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let sf_error = self
            .scale_factor_first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        // shell 侧只读这一段，不再逐字段抄重建清单。
        let invariants = self
            .invariant_violations()
            .map(|(kind, detail)| {
                format!("{{\"name\": {:?}, \"detail\": {detail:?}}}", kind.name())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let scale_error = self
            .scaled_stats
            .scale_first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let ungroup_error = self
            .scaled_stats
            .ungroup_first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let synthesis_error = self
            .scaled_stats
            .synthesis_first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let ajoc_reconstruction_error = self
            .ajoc_reconstruction_first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let objects_nonfinite_error = self
            .objects_nonfinite_first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let object_shape_error = self
            .object_shape_first_error
            .as_ref()
            .map_or_else(|| "null".to_owned(), |text| format!("{text:?}"));
        let detail = self
            .first_detail
            .as_ref()
            .map_or_else(|| "null".to_owned(), Clone::clone);
        let positions = self
            .first_positions
            .iter()
            .filter_map(Option::as_deref)
            .collect::<Vec<_>>()
            .join(", ");
        let timeline = self.position_timeline.join(", ");
        let opt = |value: Option<u64>| value.map_or_else(|| "null".to_owned(), |v| v.to_string());
        format!(
            "{{\"frames\": {}, \"parsed\": {}, \"substreams\": {}, \
             \"parsed_substreams\": {}, \"failures\": {}, \"first_error\": {error}, \
             \"min_fill_bits\": {}, \"max_fill_bits\": {}, \
             \"max_dmx_obj_info_blocks\": {}, \"max_umx_obj_info_blocks\": {}, \
             \"dmx_object_info_blocks\": {}, \"umx_object_info_blocks\": {}, \
             \"derive_timing_from_dmx\": {}, \"some_signals_inactive\": {}, \
             \"oamd_extension_present\": {}, \"intra_frame_update_frames\": {}, \
             \"companding_frames\": {}, \"companding_active_frames\": {}, \
             \"aspx_add_harmonic_frames\": {}, \"aspx_interleaved_frames\": {}, \
             \"aspx_variable_framing_frames\": {}, \"aspx_balance_frames\": {}, \
             \"ajoc_full_warmup_frames\": {}, \"ajoc_full_reconstructed_frames\": {}, \
             \"ajoc_full_wet_frames\": {}, \
             \"ajoc_reconstruction_failures\": {}, \
             \"ajoc_reconstruction_first_error\": {ajoc_reconstruction_error}, \
             \"objects_nonfinite\": {}, \
             \"objects_nonfinite_first_error\": {objects_nonfinite_error}, \
             \"object_shape_mismatches\": {}, \
             \"object_shape_first_error\": {object_shape_error}, \
             \"position_changes\": {}, \"differential_positions\": {}, \
             \"state_failures\": {}, \"scale_factor_bands\": {}, \
             \"scale_factor_min\": {}, \"scale_factor_max\": {}, \
             \"scale_factor_failures\": {}, \"scale_factor_first_error\": {sf_error}, \
             \"scaled_lines\": {}, \"scaled_peak\": {}, \"scaled_nonfinite\": {}, \
             \"scale_failures\": {}, \"scale_first_error\": {scale_error}, \
             \"ungrouped_lines\": {}, \"ungroup_failures\": {}, \
             \"ungroup_first_error\": {ungroup_error}, \"ungroup_count_mismatch\": {}, \
             \"ungroup_energy_drift\": {:e}, \
             \"pcm_frames\": {}, \"pcm_samples\": {}, \"pcm_peak\": {}, \
             \"pcm_nonfinite\": {}, \"synthesis_failures\": {}, \
             \"synthesis_first_error\": {synthesis_error}, \"pcm_silent_input_frames\": {}, \
             \"pcm_zero_output_with_nonzero_input_frames\": {}, \
             \"reconstruction_invariants\": {{\"checked\": {}, \"violations\": [{invariants}]}}, \
             \"ajoc_matrix\": {}, \
             \"first_detail\": {detail}, \
             \"first_positions\": [{positions}], \"position_timeline\": [{timeline}], \
             \"position_timeline_truncated\": {}}}",
            self.frames,
            self.parsed,
            self.substreams,
            self.parsed_substreams,
            self.failures,
            opt(self.min_fill_bits),
            opt(self.max_fill_bits),
            self.max_dmx_blocks,
            self.max_umx_blocks,
            self.dmx_blocks,
            self.umx_blocks,
            self.derive_timing,
            self.inactive_signals,
            self.bed_info,
            self.intra_frame_update_frames,
            self.companding_frames,
            self.companding_active_frames,
            self.aspx_add_harmonic_frames,
            self.aspx_interleaved_frames,
            self.aspx_variable_framing_frames,
            self.aspx_balance_frames,
            self.ajoc_full_warmup_frames,
            self.ajoc_full_reconstructed_frames,
            self.ajoc_full_wet_frames,
            self.ajoc_reconstruction_failures,
            self.objects_nonfinite,
            self.object_shape_mismatches,
            self.position_changes,
            self.differential_positions,
            self.state_failures,
            self.scale_factor_bands,
            opt(self.scale_factor_min.map(u64::from)),
            opt(self.scale_factor_max.map(u64::from)),
            self.scale_factor_failures,
            self.scaled_stats.lines,
            self.scaled_stats.peak,
            self.scaled_stats.nonfinite,
            self.scaled_stats.scale_failures,
            self.scaled_stats.ungrouped_lines,
            self.scaled_stats.ungroup_failures,
            self.scaled_stats.ungroup_count_mismatch,
            self.scaled_stats.ungroup_energy_drift,
            self.scaled_stats.pcm_frames,
            self.scaled_stats.pcm_samples,
            self.scaled_stats.pcm_peak,
            self.scaled_stats.pcm_nonfinite,
            self.scaled_stats.synthesis_failures,
            self.scaled_stats.silent_input_frames,
            self.scaled_stats.zero_output_with_nonzero_input_frames,
            ReconstructionInvariant::ALL.len(),
            self.ajoc_matrix.to_json(),
            self.timeline_truncated,
        )
    }
}

#[cfg(all(test, feature = "audio-decode"))]
mod tests {
    use super::super::{AspxBlocker, GroupOamdState, MAX_SUBSTREAM_GROUPS};
    use super::*;
    use crate::trace::testutil::{
        minimal_full_audio_topology, minimal_full_audio_topology_with_active_companding,
    };
    use macindecode_ac4_bitstream::asf::reconstruct::ReconstructError;
    use macindecode_ac4_bitstream::full_ajoc::FullAjocSyntaxError;

    #[test]
    fn tracing_collects_syntax_and_asf_from_the_unified_engine() {
        let (frame, topology) = minimal_full_audio_topology(0);
        let group_oamd = [GroupOamdState::default(); MAX_SUBSTREAM_GROUPS];
        let mut trace = AjocTrace::new_tracing();

        trace.observe_at(&frame, &topology, 0, &group_oamd, 0);

        assert_eq!(trace.parsed_substreams, 1);
        assert_eq!(trace.parsed, 1);
        assert!(trace.min_fill_bits.is_some());
        assert!(trace.scaled_stats.pcm_frames > 0);
        assert!(trace.scaled_stats.pcm_samples > 0);
        assert_eq!(
            trace.scaled_stats.pcm_samples, trace.scaled_stats.ungrouped_lines,
            "engine observation 必须保留既有样本守恒判据"
        );
        assert!(trace.full_decoder.is_some());
    }

    #[test]
    fn tracing_keeps_asf_observation_before_aspx_blockers() {
        let (frame, topology) = minimal_full_audio_topology_with_active_companding(0);
        let group_oamd = [GroupOamdState::default(); MAX_SUBSTREAM_GROUPS];
        let mut trace = AjocTrace::new_tracing();

        trace.observe_at(&frame, &topology, 0, &group_oamd, 0);

        assert_eq!(trace.parsed_substreams, 1);
        assert_eq!(trace.aspx_failures, 1);
        assert!(trace.scaled_stats.pcm_frames > 0);
        assert!(trace.scaled_stats.pcm_samples > 0);
        assert!(
            trace
                .aspx_unsupported_first_error
                .as_deref()
                .is_some_and(|error| error.contains("companding"))
        );
    }

    #[test]
    fn engine_asf_errors_keep_the_existing_trace_invariant_classes() {
        let reconstruct = ReconstructError::InvalidBandRange { group: 1, sfb: 2 };
        assert_eq!(
            engine_asf_failure_class(FullAjocAsfErrorKind::ScaleFactors(reconstruct)),
            EngineAsfFailureClass::ScaleFactor
        );
        assert_eq!(
            engine_asf_failure_class(FullAjocAsfErrorKind::ScaleSpectrum(reconstruct)),
            EngineAsfFailureClass::Scale
        );
        assert_eq!(
            engine_asf_failure_class(FullAjocAsfErrorKind::UngroupSpectrum(reconstruct)),
            EngineAsfFailureClass::Ungroup
        );
        for stage in [
            FullAjocAsfStage::ScaledSpectrum,
            FullAjocAsfStage::UngroupedSpectrum,
        ] {
            assert_eq!(
                engine_asf_failure_class(FullAjocAsfErrorKind::NonFinite { stage, sample: 3 }),
                EngineAsfFailureClass::ScaledNonFinite
            );
        }
        assert_eq!(
            engine_asf_failure_class(FullAjocAsfErrorKind::NonFinite {
                stage: FullAjocAsfStage::Pcm,
                sample: 3,
            }),
            EngineAsfFailureClass::PcmNonFinite
        );
        assert_eq!(
            engine_asf_failure_class(FullAjocAsfErrorKind::MissingLayout),
            EngineAsfFailureClass::Synthesis
        );
    }

    #[test]
    fn the_first_unsupported_branch_survives_later_ones() {
        let mut trace = AjocTrace::new_tracing();
        assert!(
            trace
                .commit_aspx_support(Err(AspxBlocker::SimpleTimeline), 1, 4)
                .is_none()
        );
        assert!(
            trace
                .commit_aspx_support(
                    Err(AspxBlocker::ShortFrameTimeline {
                        frame_length: 1_024,
                    }),
                    2,
                    9,
                )
                .is_none()
        );

        assert!(
            trace
                .aspx_unsupported_first_error
                .as_deref()
                .is_some_and(|error| error.starts_with("帧 4：substream 1：SIMPLE"))
        );
        assert_eq!(trace.aspx_failures, 2);
    }

    #[test]
    fn full_frontend_failure_uses_the_common_drive_gate() {
        let mut trace = AjocTrace::new_tracing();
        trace.commit_full_audio_frame(
            2,
            9,
            Err(FullAjocAudioFrameError::Syntax(
                FullAjocSyntaxError::SubstreamIndexOutOfRange { index: 2, limit: 1 },
            )),
        );

        assert_eq!(trace.aspx_failures, 1);
        assert!(
            trace
                .aspx_first_error
                .as_deref()
                .is_some_and(|detail| detail.contains("帧 9：音频语法"))
        );
        assert_eq!(trace.ajoc_reconstruction_failures, 0);
        assert_eq!(trace.objects_nonfinite, 0);
        assert_eq!(trace.object_shape_mismatches, 0);
    }

    #[test]
    fn full_failures_update_specific_invariants_before_the_common_gate() {
        let cases = [
            FullAjocDecodeError::reconstruction("矩阵失败"),
            FullAjocDecodeError::objects_nonfinite("对象非有限"),
            FullAjocDecodeError::object_shape("对象形状错误"),
            FullAjocDecodeError::unsupported("活动 DE 尚未支持"),
        ];
        let mut trace = AjocTrace::new_tracing();
        for (slot, error) in cases.into_iter().enumerate() {
            trace.commit_aspx_frame(slot as u32, slot as u32, Err(error));
        }

        assert_eq!(trace.ajoc_reconstruction_failures, 1);
        assert_eq!(trace.objects_nonfinite, 1);
        assert_eq!(trace.object_shape_mismatches, 1);
        assert_eq!(trace.aspx_failures, 4);
        assert!(
            trace
                .full_unsupported_first_error
                .as_deref()
                .is_some_and(|detail| detail.contains("活动 DE"))
        );
    }
}
