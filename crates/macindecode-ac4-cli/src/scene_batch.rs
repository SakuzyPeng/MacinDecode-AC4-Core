//! Scene Session 到整文件 artifact 批次的适配层。
//!
//! Scene crate 只接收已经定界的 raw AU，并输出 normalized planar PCM；这里负责
//! 剥离裸流 sync wrapper、读取 MP4 sample table、在 Scene 外应用 edit list，并把
//! normalized 样本乘精确的 `2^15`，恢复既有出口使用的 `±32768` 量级。A-SPX
//! 诊断基线、CoreCAF 与 artifact adapter 共用这一入口；后两者还把已选择
//! presentation 的借用对象描述与对应 downmix/upmix OAMD 更新投影到 writer-owned
//! 元数据批次。合成诊断渲染只保留场景元数据，不累积已由 Session 解码并完成控制
//! 对齐的 PCM。

use crate::container::{
    AUDIO_SAMPLE_ENTRY_LEN, find_ac4_track, presentation_media_span, presentation_sample_shift,
    project_pcm_batch_to_presentation, scale_i64_round, scale_u64_round,
};
use crate::metadata_batch::{
    MediaSpan, MetadataBatch, MetadataElement, MetadataElementId, MetadataElementKind,
    MetadataEvent,
};
use crate::pcm_batch::{PcmBatch, PcmTrack, PcmTrackSource};
use macindecode_ac4_bitstream::oamd::OamdCommonData;
use macindecode_ac4_bitstream::{Ac4Toc, SyncFrameIter};
use macindecode_ac4_mp4::{
    Ac4Dsi, EditListEntry, SampleTable, find_box, find_path, parse_edit_list, parse_header_timing,
    presentation_timing,
};
use macindecode_ac4_scene::{
    Ac4DecoderConfig, Ac4DecoderSession, Ac4SceneFrame, AccessUnit, AccessUnitContext,
    CoreBandPcmFrame, DecodeError, DecodeErrorKind, DecodeMode, DecodeStage, DecodeStatus,
    PcmLayout, PcmSampleFormat, PresentationSelection, ResetKind, SceneElementId,
    SceneElementSource, ScenePath, SpeakerLabel, UnsupportedReason,
};
use std::collections::{BTreeMap, BTreeSet};

/// CLI 经 Scene Session 批量采集时的稳定失败分类。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SceneBatchError {
    /// presentation 无法唯一解析或显式下标无效。
    Selection(String),
    /// 输入合法，但超出当前 Scene 子集。
    Unsupported {
        message: String,
        scene_path: Option<SceneBatchPath>,
    },
    /// Scene PCM 或元素所有权违反公开不变量。
    Invariant(String),
    /// 容器、码流或 DSP 解码失败。
    Failed(String),
}

impl SceneBatchError {
    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
            scene_path: None,
        }
    }

    fn unsupported_decode(message: String, reason: UnsupportedReason) -> Self {
        Self::Unsupported {
            message,
            scene_path: SceneBatchPath::from_unsupported_reason(reason),
        }
    }
}

/// Scene Session 已明确识别出的输入编码路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SceneBatchPath {
    ChannelBased,
    DirectObject,
    Ajoc,
    Mixed,
    Empty,
}

impl SceneBatchPath {
    const fn from_unsupported_reason(reason: UnsupportedReason) -> Option<Self> {
        match reason {
            UnsupportedReason::ChannelBased => Some(Self::ChannelBased),
            UnsupportedReason::DirectObject => Some(Self::DirectObject),
            UnsupportedReason::Mixed => Some(Self::Mixed),
            UnsupportedReason::EmptyScene => Some(Self::Empty),
            UnsupportedReason::AjocSubstreamIndexAbsent
            | UnsupportedReason::OamdSubstreamIndexAbsent
            | UnsupportedReason::AjocSubstreamContextConflict
            | UnsupportedReason::MultipleFullSubstreams { .. }
            | UnsupportedReason::MultipleCoreSubstreams { .. }
            | UnsupportedReason::StaticDownmix
            | UnsupportedReason::SamplingFrequency { .. }
            | UnsupportedReason::FullbandDownmixSignalsExceeded { .. }
            | UnsupportedReason::AjocObjectsExceeded { .. }
            | UnsupportedReason::FullbandObjectAssignment { .. }
            | UnsupportedReason::CoreObjectAssignment { .. }
            | UnsupportedReason::CoreDialogueEnhancement { .. }
            | UnsupportedReason::AudioMetadataBranch
            | UnsupportedReason::AlternativeObjectMetadata
            | UnsupportedReason::FullAjocBranch
            | UnsupportedReason::FullAjoc(_) => Some(Self::Ajoc),
            _ => None,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ChannelBased => "channel_based",
            Self::DirectObject => "direct_object",
            Self::Ajoc => "ajoc",
            Self::Mixed => "mixed",
            Self::Empty => "empty",
        }
    }
}

/// 已选择 presentation 的 full 场景元数据与对象 PCM。
#[derive(Debug)]
pub(crate) struct FullSceneBatch {
    pub(crate) metadata: MetadataBatch,
    pub(crate) pcm: PcmBatch,
}

/// 已选择 presentation 的 Core 场景元数据与 A-SPX 诊断 PCM。
#[derive(Debug)]
pub(crate) struct CoreSceneBatch {
    pub(crate) metadata: MetadataBatch,
    pub(crate) pcm: PcmBatch,
}

/// 已选择 presentation 的诊断渲染场景元数据。
#[derive(Debug)]
pub(crate) struct DiagnosticSceneBatch {
    pub(crate) metadata: MetadataBatch,
}

/// 通过流式 Core Scene Session 累积 pre-A-SPX 核心带诊断基线 PCM。
pub(crate) fn collect_core_pcm(
    data: &[u8],
    selection: PresentationSelection,
) -> Result<PcmBatch, SceneBatchError> {
    collect_batch(data, selection, DecodeMode::Core, false, false, true)?
        .core_band_pcm
        .ok_or_else(|| {
            SceneBatchError::Invariant("Core-band scene batch produced no PCM".to_owned())
        })
}

/// 通过流式 Scene Session 累积既有对象 WAVE 所需的整文件 PCM。
pub(crate) fn collect_objects_pcm(
    data: &[u8],
    selection: PresentationSelection,
) -> Result<PcmBatch, SceneBatchError> {
    collect_batch(data, selection, DecodeMode::Full, true, false, false)?
        .pcm
        .ok_or_else(|| SceneBatchError::Invariant("Object scene batch produced no PCM".to_owned()))
}

/// 通过流式 Core Scene Session 累积 A-SPX 诊断基线所需的整文件 PCM。
pub(crate) fn collect_aspx_pcm(
    data: &[u8],
    selection: PresentationSelection,
) -> Result<PcmBatch, SceneBatchError> {
    collect_batch(data, selection, DecodeMode::Core, true, false, false)?
        .pcm
        .ok_or_else(|| SceneBatchError::Invariant("A-SPX scene batch produced no PCM".to_owned()))
}

/// 通过流式 Scene Session 一趟采集 full artifact writer 所需的 PCM 与 OAMD 场景。
pub(crate) fn collect_full_scene_batch(
    data: &[u8],
    selection: PresentationSelection,
) -> Result<FullSceneBatch, SceneBatchError> {
    let batch = collect_batch(data, selection, DecodeMode::Full, true, true, false)?;
    let metadata = batch.metadata.ok_or_else(|| {
        SceneBatchError::Invariant("Full scene batch produced no scene metadata".to_owned())
    })?;
    let pcm = batch
        .pcm
        .ok_or_else(|| SceneBatchError::Invariant("Full scene batch produced no PCM".to_owned()))?;
    Ok(FullSceneBatch { metadata, pcm })
}

/// 通过流式 Core Scene Session 一趟采集 CoreCAF 所需的 A-SPX PCM 与 OAMD。
pub(crate) fn collect_core_scene_batch(
    data: &[u8],
    selection: PresentationSelection,
) -> Result<CoreSceneBatch, SceneBatchError> {
    let batch = collect_batch(data, selection, DecodeMode::Core, true, true, false)?;
    let metadata = batch.metadata.ok_or_else(|| {
        SceneBatchError::Invariant("Core scene batch produced no scene metadata".to_owned())
    })?;
    let pcm = batch
        .pcm
        .ok_or_else(|| SceneBatchError::Invariant("Core scene batch produced no PCM".to_owned()))?;
    Ok(CoreSceneBatch { metadata, pcm })
}

/// 通过流式 Scene Session 采集合成诊断渲染所需的 OAMD 场景。
///
/// Session 仍完整解码 PCM 以维持表 188 的控制所有权与时间对齐；本适配器只是不把
/// PCM 累积为整文件副本，避免合成探针同时占用一份无用的对象音频内存。
pub(crate) fn collect_diagnostic_scene_batch(
    data: &[u8],
    selection: PresentationSelection,
    mode: DecodeMode,
) -> Result<DiagnosticSceneBatch, SceneBatchError> {
    let batch = collect_batch(data, selection, mode, false, true, false)?;
    let metadata = batch.metadata.ok_or_else(|| {
        SceneBatchError::Invariant("Diagnostic scene batch produced no scene metadata".to_owned())
    })?;
    Ok(DiagnosticSceneBatch { metadata })
}

fn collect_batch(
    data: &[u8],
    selection: PresentationSelection,
    mode: DecodeMode,
    collect_pcm: bool,
    collect_metadata: bool,
    collect_core_band_pcm: bool,
) -> Result<CollectedBatch, SceneBatchError> {
    if matches!(data.get(0..2), Some([0xAC, 0x40] | [0xAC, 0x41])) {
        collect_raw(
            data,
            selection,
            mode,
            collect_pcm,
            collect_metadata,
            collect_core_band_pcm,
        )
    } else {
        collect_mp4(
            data,
            selection,
            mode,
            collect_pcm,
            collect_metadata,
            collect_core_band_pcm,
        )
    }
}

fn collect_raw(
    data: &[u8],
    selection: PresentationSelection,
    mode: DecodeMode,
    collect_pcm: bool,
    collect_metadata: bool,
    collect_core_band_pcm: bool,
) -> Result<CollectedBatch, SceneBatchError> {
    let mut session = Ac4DecoderSession::new(
        Ac4DecoderConfig::new(selection)
            .with_decode_mode(mode)
            .with_core_band_diagnostics(collect_core_band_pcm),
    );
    let mut accumulator = SceneBatchAccumulator::new(
        None,
        mode,
        collect_pcm,
        collect_metadata,
        collect_core_band_pcm,
    );
    let mut frame_start = 0i64;
    let mut sample_rate = None;
    let mut access_units = 0u64;

    for item in SyncFrameIter::new(data) {
        let sync_frame = item.map_err(|error| SceneBatchError::Failed(error.to_string()))?;
        let toc = Ac4Toc::parse(sync_frame.raw_frame)
            .map_err(|error| SceneBatchError::Failed(error.to_string()))?;
        let rate = toc.base_sampling_frequency_hz().ok_or_else(|| {
            SceneBatchError::Failed("Raw AC-4 declares no supported sample rate".to_owned())
        })?;
        if sample_rate.is_some_and(|current| current != rate) {
            return Err(SceneBatchError::Failed(
                "Raw AC-4 changes sample rate midstream".to_owned(),
            ));
        }
        sample_rate = Some(rate);

        let context = AccessUnitContext::new(access_units)
            .with_source_sample_start(frame_start)
            .with_presentation_sample_start(frame_start);
        decode_into(
            &mut session,
            &mut accumulator,
            sync_frame.raw_frame,
            context,
        )?;

        let frame_len = toc
            .codec_frame_len_base(1)
            .ok_or_else(|| SceneBatchError::unsupported("Cannot derive raw AC-4 frame length"))?;
        frame_start = frame_start
            .checked_add(i64::from(frame_len))
            .ok_or_else(|| SceneBatchError::Failed("Raw AC-4 timeline overflow".to_owned()))?;
        access_units = access_units.checked_add(1).ok_or_else(|| {
            SceneBatchError::Failed("Raw AC-4 access-unit index overflow".to_owned())
        })?;
    }

    if access_units == 0 {
        return Err(SceneBatchError::Failed(
            "Input contains no AC-4 sync frame".to_owned(),
        ));
    }
    let duration_samples = u64::try_from(frame_start)
        .map_err(|_| SceneBatchError::Failed("Raw AC-4 duration is negative".to_owned()))?;
    accumulator.finish(
        sample_rate.ok_or_else(|| {
            SceneBatchError::Failed("Cannot determine raw AC-4 sample rate".to_owned())
        })?,
        duration_samples,
        Some(MediaSpan {
            start_sample: 0,
            end_sample: duration_samples,
        }),
        Some(0),
    )
}

fn collect_mp4(
    data: &[u8],
    selection: PresentationSelection,
    mode: DecodeMode,
    collect_pcm: bool,
    collect_metadata: bool,
    collect_core_band_pcm: bool,
) -> Result<CollectedBatch, SceneBatchError> {
    let moov = find_box(data, b"moov")
        .ok_or_else(|| SceneBatchError::Failed("moov box not found".to_owned()))?;
    let mvhd = find_box(moov.payload, b"mvhd")
        .ok_or_else(|| SceneBatchError::Failed("mvhd box not found".to_owned()))?;
    let movie = parse_header_timing(*b"mvhd", mvhd.payload)
        .map_err(|error| SceneBatchError::Failed(error.to_string()))?;
    let track = find_ac4_track(moov.payload).ok_or_else(|| {
        SceneBatchError::Failed("No track with an ac-4 sample entry was found".to_owned())
    })?;
    let mdhd = find_box(track.mdia.payload, b"mdhd")
        .ok_or_else(|| SceneBatchError::Failed("mdhd box not found".to_owned()))?;
    let media = parse_header_timing(*b"mdhd", mdhd.payload)
        .map_err(|error| SceneBatchError::Failed(error.to_string()))?;

    let mut edit_storage = [EditListEntry {
        segment_duration: 0,
        media_time: 0,
        media_rate: (0, 0),
    }; 8];
    let edit_count = find_path(track.trak.payload, &[*b"edts", *b"elst"])
        .map(|elst| parse_edit_list(elst.payload, &mut edit_storage))
        .transpose()
        .map_err(|error| SceneBatchError::Failed(error.to_string()))?
        .unwrap_or(0);
    let edits = edit_storage.get(..edit_count).unwrap_or(&[]);
    if edits.iter().filter(|entry| !entry.is_empty_edit()).count() > 1 {
        return Err(SceneBatchError::unsupported(
            "This scene-export version does not support multiple discontiguous media edits",
        ));
    }
    let presentation = presentation_timing(media, movie.timescale, edits)
        .map_err(|error| SceneBatchError::Failed(error.to_string()))?;

    let specific = track
        .sample_entry
        .payload
        .get(AUDIO_SAMPLE_ENTRY_LEN..)
        .and_then(|tail| find_box(tail, b"dac4"))
        .ok_or_else(|| SceneBatchError::Failed("ac-4 sample entry has no dac4 box".to_owned()))?;
    let dsi = Ac4Dsi::parse(specific.payload)
        .map_err(|error| SceneBatchError::Failed(error.to_string()))?;
    let sample_rate = dsi.base_sampling_frequency.hz();
    let table = SampleTable::parse(track.stbl.payload)
        .map_err(|error| SceneBatchError::Failed(error.to_string()))?;
    let output_duration = scale_u64_round(
        presentation.presented_duration,
        u64::from(sample_rate),
        u64::from(media.timescale),
    )
    .map_err(SceneBatchError::Failed)?;
    let priming = scale_u64_round(
        presentation.priming,
        u64::from(sample_rate),
        u64::from(media.timescale),
    )
    .map_err(SceneBatchError::Failed)?;
    let capacity_hint = output_duration
        .checked_add(priming)
        .and_then(|samples| usize::try_from(samples).ok());
    let presentation_shift =
        presentation_sample_shift(sample_rate, media.timescale, movie.timescale, edits)
            .map_err(SceneBatchError::Failed)?;

    let mut session = Ac4DecoderSession::new(
        Ac4DecoderConfig::new(selection)
            .with_decode_mode(mode)
            .with_core_band_diagnostics(collect_core_band_pcm),
    );
    let mut accumulator = SceneBatchAccumulator::new(
        capacity_hint,
        mode,
        collect_pcm,
        collect_metadata,
        collect_core_band_pcm,
    );
    for item in table.iter() {
        let info = item.map_err(|error| SceneBatchError::Failed(error.to_string()))?;
        let start = usize::try_from(info.offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(usize::try_from(info.size).unwrap_or(0));
        let frame = data.get(start..end).ok_or_else(|| {
            SceneBatchError::Failed("AC-4 sample range exceeds the file size".to_owned())
        })?;
        let source_start = scale_i64_round(
            info.composition_time,
            i64::from(sample_rate),
            i64::from(media.timescale),
        )
        .map_err(SceneBatchError::Failed)?;
        let mut context = AccessUnitContext::new(u64::from(info.index))
            .with_source_sample_start(source_start)
            .with_priming_samples(priming)
            .with_random_access_hint(info.is_sync);
        if let Some(shift) = presentation_shift {
            let presentation_start = source_start.checked_add(shift).ok_or_else(|| {
                SceneBatchError::Failed(
                    "Access-unit presentation-position overflow after applying MP4 edits"
                        .to_owned(),
                )
            })?;
            context = context.with_presentation_sample_start(presentation_start);
        }
        decode_into(&mut session, &mut accumulator, frame, context)?;
    }

    let media_span = if edits.is_empty() {
        Some(MediaSpan {
            start_sample: 0,
            end_sample: output_duration,
        })
    } else {
        presentation_media_span(sample_rate, media.timescale, movie.timescale, edits)
            .map_err(SceneBatchError::Failed)?
    };
    let mut batch =
        accumulator.finish(sample_rate, output_duration, media_span, presentation_shift)?;
    batch.pcm = project_batch_pcm(
        batch.pcm,
        sample_rate,
        priming,
        output_duration,
        media_span,
        "Scene object PCM",
    )?;
    batch.core_band_pcm = project_batch_pcm(
        batch.core_band_pcm,
        sample_rate,
        priming,
        output_duration,
        media_span,
        "Core-band diagnostic PCM",
    )?;
    Ok(batch)
}

fn project_batch_pcm(
    pcm: Option<PcmBatch>,
    sample_rate: u32,
    priming: u64,
    output_duration: u64,
    media_span: Option<MediaSpan>,
    label: &'static str,
) -> Result<Option<PcmBatch>, SceneBatchError> {
    let Some(pcm) = pcm else {
        return Ok(None);
    };
    if pcm.sample_rate != sample_rate {
        return Err(SceneBatchError::Invariant(format!(
            "{label} sample rate {} does not match MP4 dac4 sample rate {sample_rate}",
            pcm.sample_rate
        )));
    }
    project_pcm_batch_to_presentation(Some(pcm), priming, output_duration, media_span)
        .map_err(SceneBatchError::Failed)?
        .map(Some)
        .ok_or_else(|| SceneBatchError::Invariant(format!("{label} was lost after projection")))
}

fn decode_into(
    session: &mut Ac4DecoderSession,
    accumulator: &mut SceneBatchAccumulator,
    raw_frame: &[u8],
    context: AccessUnitContext,
) -> Result<(), SceneBatchError> {
    let decoded = session
        .decode_access_unit(AccessUnit::new(raw_frame, context))
        .map_err(classify_decode_error)?;
    match decoded.status() {
        DecodeStatus::Decoded => {}
        DecodeStatus::WaitingForRandomAccess { .. } => {
            return Err(SceneBatchError::Failed(
                "Whole-file scene-batch input does not begin at a complete random-access point"
                    .to_owned(),
            ));
        }
        _ => {
            return Err(SceneBatchError::unsupported(
                "Scene session returned a state not supported by the batch adapter",
            ));
        }
    }
    if let Some(core_band_pcm) = accumulator.core_band_pcm.as_mut() {
        let frame = decoded.core_band_pcm().ok_or_else(|| {
            SceneBatchError::Invariant(
                "Successful access unit lacks the pre-A-SPX core-band diagnostic sidecar"
                    .to_owned(),
            )
        })?;
        core_band_pcm.append(frame)?;
    }
    for frame in decoded.frames() {
        accumulator.append(frame)?;
    }
    Ok(())
}

fn classify_decode_error(error: DecodeError) -> SceneBatchError {
    classify_decode_error_kind(error.kind(), error.to_string())
}

fn classify_decode_error_kind(kind: DecodeErrorKind, message: String) -> SceneBatchError {
    match kind {
        DecodeErrorKind::Selection(_) => SceneBatchError::Selection(message),
        DecodeErrorKind::Unsupported(reason) => {
            SceneBatchError::unsupported_decode(message, reason)
        }
        DecodeErrorKind::InternalInvariant { .. }
        | DecodeErrorKind::ResetRequired
        | DecodeErrorKind::DecodeFailure {
            stage: DecodeStage::Ajoc,
        } => SceneBatchError::Invariant(message),
        DecodeErrorKind::NeedMoreData(_)
        | DecodeErrorKind::InvalidBitstream(_)
        | DecodeErrorKind::DecodeFailure { .. } => SceneBatchError::Failed(message),
        _ => SceneBatchError::Failed(message),
    }
}

#[derive(Debug)]
struct CollectedBatch {
    pcm: Option<PcmBatch>,
    core_band_pcm: Option<PcmBatch>,
    metadata: Option<MetadataBatch>,
}

#[derive(Debug)]
struct SceneBatchAccumulator {
    mode: DecodeMode,
    pcm: Option<ScenePcmAccumulator>,
    core_band_pcm: Option<CoreBandPcmAccumulator>,
    metadata: Option<MetadataAccumulator>,
    sample_rate: Option<u32>,
    generation: Option<u32>,
    presentation: Option<(u32, Option<u32>)>,
}

impl SceneBatchAccumulator {
    fn new(
        capacity_hint: Option<usize>,
        mode: DecodeMode,
        collect_pcm: bool,
        collect_metadata: bool,
        collect_core_band_pcm: bool,
    ) -> Self {
        Self {
            mode,
            pcm: collect_pcm.then(|| ScenePcmAccumulator::new(capacity_hint, mode)),
            core_band_pcm: collect_core_band_pcm
                .then(|| CoreBandPcmAccumulator::new(capacity_hint)),
            metadata: collect_metadata.then(|| MetadataAccumulator::new(mode)),
            sample_rate: None,
            generation: None,
            presentation: None,
        }
    }

    fn append(&mut self, frame: Ac4SceneFrame<'_>) -> Result<(), SceneBatchError> {
        self.validate_frame(&frame)?;
        if let Some(pcm) = self.pcm.as_mut() {
            pcm.append(frame)?;
        }
        if let Some(metadata) = self.metadata.as_mut() {
            metadata.append(frame)?;
        }
        Ok(())
    }

    fn validate_frame(&mut self, frame: &Ac4SceneFrame<'_>) -> Result<(), SceneBatchError> {
        let timeline = frame.timeline();
        if timeline.duration_samples() == 0 {
            return Err(SceneBatchError::Invariant(
                "SceneFrame PCM duration is zero".to_owned(),
            ));
        }
        if !matches!(frame.presentation().path(), ScenePath::Ajoc)
            || frame.presentation().mode() != self.mode
        {
            return Err(SceneBatchError::unsupported(
                "Scene batch adapter received an A-JOC output mode inconsistent with the configuration",
            ));
        }
        if frame
            .diagnostics()
            .reset()
            .is_some_and(|reset| reset != ResetKind::Initial)
        {
            return Err(SceneBatchError::Failed(
                "Source, configuration, or continuity reset occurred midstream; whole-file scene batches require one continuous configuration"
                    .to_owned(),
            ));
        }

        let sample_rate = timeline.sample_rate();
        if self
            .sample_rate
            .is_some_and(|current| current != sample_rate)
        {
            return Err(SceneBatchError::Invariant(
                "SceneFrame changes PCM sample rate midstream".to_owned(),
            ));
        }
        self.sample_rate = Some(sample_rate);

        let generation = timeline.configuration_generation();
        if self.generation.is_some_and(|current| current != generation) {
            return Err(SceneBatchError::Failed(
                "Configuration generation changes midstream; whole-file scene batches do not support dynamic topology".to_owned(),
            ));
        }
        self.generation = Some(generation);

        let presentation = (frame.presentation().index(), frame.presentation().id());
        if self
            .presentation
            .is_some_and(|current| current != presentation)
        {
            return Err(SceneBatchError::Failed(
                "Selected presentation changes midstream".to_owned(),
            ));
        }
        self.presentation = Some(presentation);
        Ok(())
    }

    fn finish(
        self,
        sample_rate: u32,
        duration_samples: u64,
        media_span: Option<MediaSpan>,
        presentation_shift: Option<i64>,
    ) -> Result<CollectedBatch, SceneBatchError> {
        if self.sample_rate != Some(sample_rate) {
            return Err(SceneBatchError::Invariant(format!(
                "SceneFrame sample rate {:?} does not match declared input rate {sample_rate}",
                self.sample_rate
            )));
        }
        self.presentation.ok_or_else(|| {
            SceneBatchError::Failed("Decoder did not resolve a selected presentation".to_owned())
        })?;
        let pcm = self.pcm.map(ScenePcmAccumulator::finish).transpose()?;
        let core_band_pcm = self
            .core_band_pcm
            .map(CoreBandPcmAccumulator::finish)
            .transpose()?;
        let metadata = self
            .metadata
            .map(|metadata| {
                metadata.finish(
                    sample_rate,
                    duration_samples,
                    media_span,
                    presentation_shift,
                )
            })
            .transpose()?;
        Ok(CollectedBatch {
            pcm,
            core_band_pcm,
            metadata,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SceneElementBinding {
    element_id: SceneElementId,
    substream: u32,
    object: u8,
}

#[derive(Debug)]
struct MetadataAccumulator {
    mode: DecodeMode,
    elements: Vec<MetadataElement>,
    events: Vec<MetadataEvent>,
    bindings: Vec<SceneElementBinding>,
    source_starts: BTreeMap<u64, i64>,
    baseline_control_sources: BTreeMap<u32, u64>,
    emitted_baselines: BTreeSet<SceneElementId>,
    next_stream_order: u64,
}

impl MetadataAccumulator {
    fn new(mode: DecodeMode) -> Self {
        Self {
            mode,
            elements: Vec::new(),
            events: Vec::new(),
            bindings: Vec::new(),
            source_starts: BTreeMap::new(),
            baseline_control_sources: BTreeMap::new(),
            emitted_baselines: BTreeSet::new(),
            next_stream_order: 0,
        }
    }

    fn append(&mut self, frame: Ac4SceneFrame<'_>) -> Result<(), SceneBatchError> {
        let timeline = frame.timeline();
        let source_start = timeline.source_sample_start().ok_or_else(|| {
            SceneBatchError::Invariant("SceneFrame lacks a source-sample start position".to_owned())
        })?;
        match self
            .source_starts
            .insert(timeline.access_unit_index(), source_start)
        {
            Some(previous) if previous != source_start => {
                return Err(SceneBatchError::Invariant(format!(
                    "Access unit {} source-sample start changed from {previous} to {source_start}",
                    timeline.access_unit_index()
                )));
            }
            _ => {}
        }

        let (common, common_conflict) = frame_common(&frame);
        let mut frame_substreams = BTreeSet::new();

        for object in frame.objects() {
            let (substream_index, object_index) = match (self.mode, object.source()) {
                (
                    DecodeMode::Core,
                    SceneElementSource::AjocCoreObject {
                        substream_index,
                        object_index,
                        ..
                    },
                )
                | (
                    DecodeMode::Full,
                    SceneElementSource::AjocObject {
                        substream_index,
                        object_index,
                        ..
                    },
                ) => (substream_index, object_index),
                _ => {
                    return Err(SceneBatchError::unsupported(
                        "Scene object source is inconsistent with the selected A-JOC decode mode",
                    ));
                }
            };
            frame_substreams.insert(substream_index);
            self.remember_element(
                object.element_id(),
                substream_index,
                object_index,
                MetadataElementKind::DynamicObject,
                common,
                common_conflict,
            )?;
        }
        for bed in frame.beds() {
            let (substream_index, object_index) = match (self.mode, bed.source()) {
                (
                    DecodeMode::Core,
                    SceneElementSource::AjocCoreLfe {
                        substream_index,
                        object_index,
                        ..
                    },
                )
                | (
                    DecodeMode::Full,
                    SceneElementSource::AjocLfe {
                        substream_index,
                        object_index,
                        ..
                    },
                ) => (substream_index, object_index),
                _ => {
                    return Err(SceneBatchError::unsupported(
                        "Scene bed source is inconsistent with the selected A-JOC decode mode",
                    ));
                }
            };
            frame_substreams.insert(substream_index);
            self.remember_element(
                bed.element_id(),
                substream_index,
                object_index,
                MetadataElementKind::LfeBed,
                common,
                common_conflict,
            )?;
        }
        if frame_substreams.len() != 1 {
            return Err(SceneBatchError::unsupported(format!(
                "Scene batch accepts one physical A-JOC substream; got {}",
                frame_substreams.len()
            )));
        }
        let substream = frame_substreams
            .first()
            .copied()
            .ok_or_else(|| SceneBatchError::Invariant("Scene element set is empty".to_owned()))?;
        if frame.presentation().substream_indices() != [substream] {
            return Err(SceneBatchError::Invariant(
                "Scene presentation substream is inconsistent with element sources".to_owned(),
            ));
        }
        if let Some(control_source) = timeline.control_source_access_unit_index() {
            self.baseline_control_sources
                .entry(substream)
                .or_insert(control_source);
        }

        for update in frame.metadata_updates() {
            let binding = self
                .bindings
                .iter()
                .copied()
                .find(|binding| binding.element_id == update.element_id())
                .ok_or_else(|| {
                    SceneBatchError::Invariant(format!(
                        "OAMD update references unknown scene element {}",
                        update.element_id().get()
                    ))
                })?;
            let raw = update.raw();
            if raw.block().object_index != binding.object || binding.substream != substream {
                return Err(SceneBatchError::Invariant(
                    "OAMD update raw object index is inconsistent with the scene-element source"
                        .to_owned(),
                ));
            }
            let state = update.state().raw();
            let sample_position = source_start
                .checked_add(i64::from(update.offset_samples()))
                .ok_or_else(|| {
                    SceneBatchError::Invariant(
                        "OAMD update absolute-sample-position overflow".to_owned(),
                    )
                })?;
            let control_source = update.control_source_access_unit_index();
            if raw.block().block_index == 0
                && self.baseline_control_sources.get(&substream) == Some(&control_source)
                && self.emitted_baselines.insert(update.element_id())
            {
                let baseline_start = self
                    .source_starts
                    .get(&control_source)
                    .copied()
                    .ok_or_else(|| {
                        SceneBatchError::Invariant(format!(
                            "First due OAMD update has no timing for control-source access unit {control_source}"
                        ))
                    })?;
                let stream_order = self.take_stream_order()?;
                self.events.push(MetadataEvent {
                    sample_position: baseline_start,
                    element_id: MetadataElementId::from(update.element_id()),
                    stream_order,
                    ramp_samples: 0,
                    state: state.effective(),
                    additional: state.additional(),
                });
            }
            let stream_order = self.take_stream_order()?;
            self.events.push(MetadataEvent {
                sample_position,
                element_id: MetadataElementId::from(update.element_id()),
                stream_order,
                ramp_samples: update.ramp_duration_samples(),
                state: state.effective(),
                additional: state.additional(),
            });
        }
        Ok(())
    }

    fn take_stream_order(&mut self) -> Result<u64, SceneBatchError> {
        let current = self.next_stream_order;
        self.next_stream_order = current
            .checked_add(1)
            .ok_or_else(|| SceneBatchError::Invariant("OAMD event order exceeds u64".to_owned()))?;
        Ok(current)
    }

    fn remember_element(
        &mut self,
        element_id: SceneElementId,
        substream: u32,
        object: u8,
        kind: MetadataElementKind,
        common: Option<OamdCommonData>,
        common_conflict: bool,
    ) -> Result<(), SceneBatchError> {
        let binding = SceneElementBinding {
            element_id,
            substream,
            object,
        };
        if let Some(existing) = self
            .bindings
            .iter()
            .find(|existing| existing.element_id == element_id)
        {
            if *existing != binding {
                return Err(SceneBatchError::Invariant(format!(
                    "Encoded source for scene element {} changed",
                    element_id.get()
                )));
            }
        } else {
            if self
                .bindings
                .iter()
                .any(|existing| existing.substream == substream && existing.object == object)
            {
                return Err(SceneBatchError::Invariant(format!(
                    "Scene object {substream}:{object} changed element ID within one configuration generation"
                )));
            }
            self.bindings.push(binding);
        }

        if let Some(existing) = self
            .elements
            .iter_mut()
            .find(|existing| existing.element_id == MetadataElementId::from(element_id))
        {
            if existing.kind != kind
                || existing.substream_index != substream
                || existing.object_index != object
            {
                return Err(SceneBatchError::Invariant(format!(
                    "Scene object {substream}:{object} changed type"
                )));
            }
            existing.common_conflict |= common_conflict;
            match (existing.common, common) {
                (Some(current), Some(next)) if current != next => {
                    existing.common_conflict = true;
                }
                (None, Some(next)) => existing.common = Some(next),
                _ => {}
            }
        } else {
            self.elements.push(MetadataElement {
                element_id: element_id.into(),
                substream_index: substream,
                object_index: object,
                kind,
                common,
                common_conflict,
            });
        }
        Ok(())
    }

    fn finish(
        mut self,
        sample_rate: u32,
        duration_samples: u64,
        media_span: Option<MediaSpan>,
        presentation_shift: Option<i64>,
    ) -> Result<MetadataBatch, SceneBatchError> {
        if let Some(shift) = presentation_shift {
            for event in &mut self.events {
                event.sample_position =
                    event.sample_position.checked_add(shift).ok_or_else(|| {
                        SceneBatchError::Failed(
                            "Object-event position overflow after applying MP4 edits".to_owned(),
                        )
                    })?;
            }
        } else {
            self.events.clear();
        }
        self.elements
            .sort_by_key(|item| (item.substream_index, item.object_index));
        self.events
            .sort_by_key(|item| (item.sample_position, item.stream_order));
        Ok(MetadataBatch {
            sample_rate,
            duration_samples,
            media_span,
            decode_mode: self.mode,
            elements: self.elements,
            events: self.events,
        })
    }
}

fn frame_common(frame: &Ac4SceneFrame<'_>) -> (Option<OamdCommonData>, bool) {
    let mut common = None;
    let mut conflict = false;
    for next in frame
        .oamd_common_states()
        .iter()
        .filter_map(|state| state.effective())
    {
        match common {
            Some(current) if current != next => conflict = true,
            None => common = Some(next),
            _ => {}
        }
    }
    (common, conflict)
}

#[derive(Debug, Clone, Copy)]
struct FrameTrack<'a> {
    element_id: SceneElementId,
    substream: u32,
    source: PcmTrackSource,
    normalized_samples: &'a [f32],
}

#[derive(Debug)]
struct CoreBandPcmAccumulator {
    pcm: Option<PcmBatch>,
    capacity_hint: Option<usize>,
}

impl CoreBandPcmAccumulator {
    fn new(capacity_hint: Option<usize>) -> Self {
        Self {
            pcm: None,
            capacity_hint,
        }
    }

    fn append(&mut self, frame: CoreBandPcmFrame<'_>) -> Result<(), SceneBatchError> {
        let channel_count = frame.channel_count();
        let samples_per_channel = frame.samples_per_channel();
        if frame.sample_format() != PcmSampleFormat::F32
            || frame.layout() != PcmLayout::Planar
            || frame.nominal_full_scale() != 1.0
            || channel_count == 0
            || samples_per_channel == 0
        {
            return Err(SceneBatchError::Invariant(
                "Core-band diagnostic sidecar is not valid non-empty planar f32".to_owned(),
            ));
        }

        if let Some(pcm) = self.pcm.as_mut() {
            if pcm.sample_rate != frame.sample_rate() || pcm.tracks.len() != channel_count {
                return Err(SceneBatchError::Invariant(
                    "Core-band diagnostic sidecar changes sample rate or channel count midstream"
                        .to_owned(),
                ));
            }
            for (index, output) in pcm.tracks.iter_mut().enumerate() {
                let input = validated_core_band_channel(frame, index, samples_per_channel)?;
                let source = PcmTrackSource::TransportChannel {
                    element_index: input.element_index(),
                    channel_index: input.channel_index(),
                };
                if output.substream_index != frame.substream_index()
                    || output.output_index != index
                    || output.scene_element_id.is_some()
                    || output.source != source
                {
                    return Err(SceneBatchError::Invariant(format!(
                        "Transport identity of core-band diagnostic sidecar channel {index} changed"
                    )));
                }
                append_restored_samples(&mut output.samples, input.samples())?;
            }
        } else {
            let mut tracks = Vec::new();
            tracks.try_reserve_exact(channel_count).map_err(|error| {
                SceneBatchError::Failed(format!(
                    "Failed to reserve {channel_count} channels for core-band diagnostic PCM: {error}"
                ))
            })?;
            for index in 0..channel_count {
                let input = validated_core_band_channel(frame, index, samples_per_channel)?;
                let mut samples = Vec::new();
                if let Some(capacity) = self.capacity_hint {
                    samples.try_reserve_exact(capacity).map_err(|error| {
                        SceneBatchError::Failed(format!(
                            "Failed to reserve {capacity} samples for core-band diagnostic PCM: {error}"
                        ))
                    })?;
                }
                append_restored_samples(&mut samples, input.samples())?;
                tracks.push(PcmTrack {
                    substream_index: frame.substream_index(),
                    output_index: index,
                    scene_element_id: None,
                    source: PcmTrackSource::TransportChannel {
                        element_index: input.element_index(),
                        channel_index: input.channel_index(),
                    },
                    samples,
                });
            }
            self.pcm = Some(PcmBatch {
                sample_rate: frame.sample_rate(),
                tracks,
            });
        }
        Ok(())
    }

    fn finish(self) -> Result<PcmBatch, SceneBatchError> {
        self.pcm.ok_or_else(|| {
            SceneBatchError::Failed("Decoder retained no core-band diagnostic PCM".to_owned())
        })
    }
}

fn validated_core_band_channel<'a>(
    frame: CoreBandPcmFrame<'a>,
    index: usize,
    expected_samples: usize,
) -> Result<macindecode_ac4_scene::CoreBandPcmChannel<'a>, SceneBatchError> {
    let channel = frame.channel(index).ok_or_else(|| {
        SceneBatchError::Invariant(format!(
            "Core-band diagnostic sidecar lacks channel {index}"
        ))
    })?;
    if channel.stride() != 1
        || channel.samples().len() != expected_samples
        || channel.samples().iter().any(|sample| !sample.is_finite())
    {
        return Err(SceneBatchError::Invariant(format!(
            "Core-band diagnostic sidecar channel {index} has an invalid stride, length, or finite-value state"
        )));
    }
    Ok(channel)
}

#[derive(Debug)]
struct ScenePcmAccumulator {
    mode: DecodeMode,
    pcm: Option<PcmBatch>,
    capacity_hint: Option<usize>,
}

impl ScenePcmAccumulator {
    fn new(capacity_hint: Option<usize>, mode: DecodeMode) -> Self {
        Self {
            mode,
            pcm: None,
            capacity_hint,
        }
    }

    fn append(&mut self, frame: Ac4SceneFrame<'_>) -> Result<(), SceneBatchError> {
        let timeline = frame.timeline();
        let tracks = frame_tracks(&frame, self.mode)?;
        if let Some(pcm) = self.pcm.as_mut() {
            if pcm.sample_rate != timeline.sample_rate() {
                return Err(SceneBatchError::Invariant(
                    "SceneFrame changes PCM sample rate midstream".to_owned(),
                ));
            }
            if pcm.tracks.len() != tracks.len() {
                return Err(SceneBatchError::Invariant(
                    "SceneFrame changes the object-PCM track count midstream".to_owned(),
                ));
            }
            for (output_index, (channel, track)) in
                pcm.tracks.iter_mut().zip(tracks.iter()).enumerate()
            {
                if channel.substream_index != track.substream
                    || channel.output_index != output_index
                    || channel.source != track.source
                    || channel.scene_element_id != Some(track.element_id)
                {
                    return Err(SceneBatchError::Invariant(format!(
                        "Object identity or source of SceneFrame channel {output_index} changed"
                    )));
                }
                append_restored_samples(&mut channel.samples, track.normalized_samples)?;
            }
        } else {
            let mut owned_tracks = Vec::with_capacity(tracks.len());
            for (output_index, track) in tracks.iter().enumerate() {
                let mut samples = Vec::new();
                if let Some(capacity) = self.capacity_hint {
                    samples.try_reserve_exact(capacity).map_err(|error| {
                        SceneBatchError::Failed(format!(
                            "Failed to reserve {capacity} samples for object PCM: {error}"
                        ))
                    })?;
                }
                append_restored_samples(&mut samples, track.normalized_samples)?;
                owned_tracks.push(PcmTrack {
                    substream_index: track.substream,
                    output_index,
                    scene_element_id: Some(track.element_id),
                    source: track.source,
                    samples,
                });
            }
            self.pcm = Some(PcmBatch {
                sample_rate: timeline.sample_rate(),
                tracks: owned_tracks,
            });
        }
        Ok(())
    }

    fn finish(self) -> Result<PcmBatch, SceneBatchError> {
        self.pcm.ok_or_else(|| {
            SceneBatchError::Failed("Decoder retained no scene-object PCM".to_owned())
        })
    }
}

fn frame_tracks<'a>(
    frame: &'a Ac4SceneFrame<'_>,
    mode: DecodeMode,
) -> Result<Vec<FrameTrack<'a>>, SceneBatchError> {
    let component_count = frame
        .beds()
        .iter()
        .try_fold(0usize, |count, bed| {
            count.checked_add(bed.components().len())
        })
        .ok_or_else(|| {
            SceneBatchError::Invariant("Scene bed-component count overflow".to_owned())
        })?;
    let track_count = frame
        .objects()
        .len()
        .checked_add(component_count)
        .ok_or_else(|| SceneBatchError::Invariant("Scene PCM track-count overflow".to_owned()))?;
    if track_count == 0 {
        return Err(SceneBatchError::Invariant(
            "A-JOC SceneFrame has no PCM elements".to_owned(),
        ));
    }
    if frame.beds().len() > 1 || component_count > 1 {
        return Err(SceneBatchError::unsupported(
            "This object-WAVE version accepts at most one native LFE bed",
        ));
    }

    let expected_samples = usize::try_from(frame.timeline().duration_samples()).map_err(|_| {
        SceneBatchError::Invariant("SceneFrame PCM duration exceeds usize".to_owned())
    })?;
    let has_lfe = component_count == 1;
    let mut slots = vec![None; track_count];

    for object in frame.objects() {
        let (substream_index, object_index, output_index) = match (mode, object.source()) {
            (
                DecodeMode::Core,
                SceneElementSource::AjocCoreObject {
                    substream_index,
                    object_index,
                    input_index,
                },
            ) => (substream_index, object_index, input_index),
            (
                DecodeMode::Full,
                SceneElementSource::AjocObject {
                    substream_index,
                    object_index,
                    output_index,
                },
            ) => (substream_index, object_index, output_index),
            _ => {
                return Err(SceneBatchError::unsupported(
                    "Scene object source is inconsistent with the selected A-JOC decode mode",
                ));
            }
        };
        let pcm = object.pcm();
        if pcm.sample_format() != PcmSampleFormat::F32
            || pcm.layout() != PcmLayout::Planar
            || pcm.nominal_full_scale() != 1.0
            || pcm.planes().len() != 1
            || pcm.samples_per_plane() != expected_samples
        {
            return Err(SceneBatchError::Invariant(
                "Scene-object PCM is not valid normalized mono planar f32".to_owned(),
            ));
        }
        let plane = pcm.planes().first().ok_or_else(|| {
            SceneBatchError::Invariant("Scene-object PCM lacks a plane".to_owned())
        })?;
        if plane.stride() != 1 || plane.samples().len() != expected_samples {
            return Err(SceneBatchError::Invariant(
                "Scene-object PCM has an invalid stride or normalized length".to_owned(),
            ));
        }
        let output_index = usize::try_from(output_index).map_err(|_| {
            SceneBatchError::Invariant("Object output index exceeds usize".to_owned())
        })?;
        let slot = slots.get_mut(output_index).ok_or_else(|| {
            SceneBatchError::Invariant(
                "Object output index exceeds the scene PCM track range".to_owned(),
            )
        })?;
        if slot.is_some() {
            return Err(SceneBatchError::Invariant(
                "Scene PCM output index is duplicated".to_owned(),
            ));
        }
        *slot = Some((
            object.element_id(),
            substream_index,
            Some(object_index),
            plane.samples(),
        ));
    }

    for bed in frame.beds() {
        let (substream_index, object_index, output_index) = match (mode, bed.source()) {
            (
                DecodeMode::Core,
                SceneElementSource::AjocCoreLfe {
                    substream_index,
                    object_index,
                    output_index,
                },
            ) => (substream_index, object_index, output_index),
            (
                DecodeMode::Full,
                SceneElementSource::AjocLfe {
                    substream_index,
                    object_index,
                    reinsertion_index,
                },
            ) => (substream_index, object_index, reinsertion_index),
            _ => {
                return Err(SceneBatchError::unsupported(
                    "Scene bed source is inconsistent with the selected A-JOC decode mode",
                ));
            }
        };
        if object_index != 0 {
            return Err(SceneBatchError::Invariant(
                "Scene LFE source object index is not 0".to_owned(),
            ));
        }
        let component = bed.components().first().ok_or_else(|| {
            SceneBatchError::Invariant("Scene LFE bed lacks a component".to_owned())
        })?;
        if !matches!(component.speaker(), SpeakerLabel::Lfe)
            || component.plane().stride() != 1
            || component.plane().samples().len() != expected_samples
        {
            return Err(SceneBatchError::Invariant(
                "Scene LFE component has an invalid label, stride, or normalized length".to_owned(),
            ));
        }
        let output_index = usize::try_from(output_index)
            .map_err(|_| SceneBatchError::Invariant("LFE output index exceeds usize".to_owned()))?;
        let slot = slots.get_mut(output_index).ok_or_else(|| {
            SceneBatchError::Invariant(
                "LFE output index exceeds the scene PCM track range".to_owned(),
            )
        })?;
        if slot.is_some() {
            return Err(SceneBatchError::Invariant(
                "Scene PCM output index is duplicated".to_owned(),
            ));
        }
        *slot = Some((
            bed.element_id(),
            substream_index,
            None,
            component.plane().samples(),
        ));
    }

    let mut object_ordinal = 0usize;
    slots
        .into_iter()
        .enumerate()
        .map(|(output_index, slot)| {
            let (element_id, substream, object_index, normalized_samples) = slot.ok_or_else(|| {
                    SceneBatchError::Invariant(format!(
                        "Scene PCM lacks Pseudocode 15 output position {output_index}"
                    ))
                })?;
            let source = if let Some(object_index) = object_index {
                let expected_oamd_index = object_ordinal.checked_add(usize::from(has_lfe)).ok_or_else(
                    || SceneBatchError::Invariant("A-JOC object-index overflow".to_owned()),
                )?;
                if usize::from(object_index) != expected_oamd_index {
                    return Err(SceneBatchError::Invariant(format!(
                        "OAMD index for scene object {object_ordinal} should be {expected_oamd_index}, got {object_index}"
                    )));
                }
                let source = match mode {
                    DecodeMode::Core => PcmTrackSource::AjocInput {
                        input_index: output_index,
                    },
                    DecodeMode::Full => PcmTrackSource::AjocObject {
                        object_index: object_ordinal,
                    },
                    _ => {
                        return Err(SceneBatchError::unsupported(
                            "Scene batch adapter does not support this decode mode",
                        ));
                    }
                };
                object_ordinal = object_ordinal.checked_add(1).ok_or_else(|| {
                    SceneBatchError::Invariant("A-JOC object-ordinal overflow".to_owned())
                })?;
                source
            } else {
                PcmTrackSource::Lfe
            };
            Ok(FrameTrack {
                element_id,
                substream,
                source,
                normalized_samples,
            })
        })
        .collect()
}

fn append_restored_samples(
    target: &mut Vec<f32>,
    normalized: &[f32],
) -> Result<(), SceneBatchError> {
    const NORMALIZED_TO_INTERNAL: f32 = 32_768.0;
    for (sample_index, normalized_sample) in normalized.iter().copied().enumerate() {
        let restored = normalized_sample * NORMALIZED_TO_INTERNAL;
        if !normalized_sample.is_finite() || !restored.is_finite() {
            return Err(SceneBatchError::Invariant(format!(
                "Scene PCM sample {sample_index} cannot be restored to a finite internal-scale value"
            )));
        }
    }
    target.try_reserve(normalized.len()).map_err(|error| {
        SceneBatchError::Failed(format!(
            "Failed to accumulate {} PCM samples for the current SceneFrame: {error}",
            normalized.len()
        ))
    })?;
    target.extend(
        normalized
            .iter()
            .map(|sample| *sample * NORMALIZED_TO_INTERNAL),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_AUDIO_TOC_SUFFIX: &str = "0 1 0001 1 1 0 0";
    const FULL_AUDIO_TOC_SUFFIX_NOT_IFRAME: &str = "0 1 0001 0 1 0 0";
    const FULL_AUDIO_PRESENTATION: &str = "1 0 000 0 0 00 000 0 00 00 0 000 0 0 0 1 00";
    const FULL_AUDIO_GROUP: &str = "1 0 1 0 0 1 0 0 0000 1 0 0000 1 0 0 1 01 0";
    const MINIMAL_FULL_AUDIO_PAYLOAD: [u8; 24] = [
        0x00, 0x28, 0x40, 0x85, 0x88, 0x40, 0x10, 0x00, 0x00, 0x0f, 0x80, 0x00, 0x00, 0x00, 0x00,
        0x0e, 0xfe, 0x44, 0x02, 0x00, 0xc3, 0x00, 0x00, 0x20,
    ];

    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "测试位串按已计数长度写入新分配的字节数组"
    )]
    fn pack_bits(parts: &[&str]) -> Vec<u8> {
        let bits = parts
            .iter()
            .flat_map(|part| part.chars())
            .filter(|ch| *ch == '0' || *ch == '1')
            .collect::<Vec<_>>();
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (index, bit) in bits.into_iter().enumerate() {
            if bit == '1' {
                bytes[index / 8] |= 1 << (7 - index % 8);
            }
        }
        bytes
    }

    fn raw_full_frame_with_toc(sequence_counter: u16, toc_suffix: &str) -> Vec<u8> {
        let size = format!("{size:010b}", size = MINIMAL_FULL_AUDIO_PAYLOAD.len());
        let table = ["10 0 0000000000 0 ", &size].concat();
        let toc = format!("10 {sequence_counter:010b} {toc_suffix}");
        let mut frame = pack_bits(&[&toc, FULL_AUDIO_PRESENTATION, FULL_AUDIO_GROUP, &table]);
        frame.extend_from_slice(&MINIMAL_FULL_AUDIO_PAYLOAD);
        frame
    }

    fn raw_full_frame_with_sequence(sequence_counter: u16) -> Vec<u8> {
        raw_full_frame_with_toc(sequence_counter, FULL_AUDIO_TOC_SUFFIX)
    }

    fn sync_stream_frames(frames: usize) -> Vec<u8> {
        let mut stream = Vec::new();
        for frame in 0..frames {
            let sequence_counter = u16::try_from(frame).expect("测试帧数应适合序列计数器");
            let raw = raw_full_frame_with_sequence(sequence_counter);
            let size = u16::try_from(raw.len()).expect("最小夹具应使用短 frame_size");
            stream.extend_from_slice(&[0xac, 0x40]);
            stream.extend_from_slice(&size.to_be_bytes());
            stream.extend_from_slice(&raw);
        }
        stream
    }

    fn sync_stream() -> Vec<u8> {
        sync_stream_frames(1)
    }

    #[test]
    fn scene_batch_reports_an_invalid_explicit_presentation_as_selection() {
        let error = collect_objects_pcm(&sync_stream(), PresentationSelection::Index(1))
            .expect_err("单 presentation 输入不存在下标 1");
        assert!(matches!(error, SceneBatchError::Selection(_)));
    }

    #[test]
    fn scene_batch_waiting_error_uses_shared_scene_wording() {
        let raw = raw_full_frame_with_toc(0, FULL_AUDIO_TOC_SUFFIX_NOT_IFRAME);
        let size = u16::try_from(raw.len()).expect("最小夹具应使用短 frame_size");
        let mut input = vec![0xac, 0x40];
        input.extend_from_slice(&size.to_be_bytes());
        input.extend_from_slice(&raw);

        assert_eq!(
            collect_core_pcm(&input, PresentationSelection::AutoUnique)
                .expect_err("依赖帧开头的 core batch 必须等待完整随机访问点"),
            SceneBatchError::Failed(
                "Whole-file scene-batch input does not begin at a complete random-access point"
                    .to_owned()
            )
        );
    }

    #[test]
    fn scene_batch_preserves_decode_error_classes_and_scene_paths() {
        assert_eq!(
            classify_decode_error_kind(
                DecodeErrorKind::DecodeFailure {
                    stage: DecodeStage::Ajoc,
                },
                "A-JOC 矩阵重建失败".to_owned(),
            ),
            SceneBatchError::Invariant("A-JOC 矩阵重建失败".to_owned())
        );
        assert_eq!(
            classify_decode_error_kind(
                DecodeErrorKind::DecodeFailure {
                    stage: DecodeStage::Oamd,
                },
                "OAMD 状态失败".to_owned(),
            ),
            SceneBatchError::Failed("OAMD 状态失败".to_owned())
        );
        for (reason, scene_path) in [
            (
                UnsupportedReason::ChannelBased,
                SceneBatchPath::ChannelBased,
            ),
            (
                UnsupportedReason::DirectObject,
                SceneBatchPath::DirectObject,
            ),
            (UnsupportedReason::Mixed, SceneBatchPath::Mixed),
            (UnsupportedReason::EmptyScene, SceneBatchPath::Empty),
            (
                UnsupportedReason::CoreDialogueEnhancement {
                    dialogue_objects: 1,
                },
                SceneBatchPath::Ajoc,
            ),
        ] {
            assert_eq!(
                classify_decode_error_kind(
                    DecodeErrorKind::Unsupported(reason),
                    "不支持的场景路径".to_owned(),
                ),
                SceneBatchError::Unsupported {
                    message: "不支持的场景路径".to_owned(),
                    scene_path: Some(scene_path),
                }
            );
        }
        assert_eq!(
            classify_decode_error_kind(
                DecodeErrorKind::Unsupported(UnsupportedReason::FutureBitstreamVersion {
                    bitstream_version: 3,
                }),
                "尚未识别输入场景路径".to_owned(),
            ),
            SceneBatchError::Unsupported {
                message: "尚未识别输入场景路径".to_owned(),
                scene_path: None,
            }
        );
    }

    #[test]
    fn normalized_pcm_restores_internal_scale_without_clipping() {
        let normalized = [0.0, 1.0, -2.0, -0.0];
        let mut restored = Vec::new();
        append_restored_samples(&mut restored, &normalized)
            .expect("normalized PCM 应恢复旧出口尺度");
        assert_eq!(restored, [0.0, 32_768.0, -65_536.0, -0.0]);
        assert_eq!(
            restored.last().copied().unwrap_or_default().to_bits(),
            (-0.0f32).to_bits()
        );
    }

    #[test]
    fn normalized_pcm_restore_rejects_non_finite_without_partial_output() {
        let mut restored = vec![4.0];
        assert!(matches!(
            append_restored_samples(&mut restored, &[0.0, f32::NAN]),
            Err(SceneBatchError::Invariant(_))
        ));
        assert_eq!(restored, [4.0]);
    }
}
