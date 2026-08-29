//! 面向人的 AC-4 只读检视报告。
//!
//! 本模块直接消费 MP4 sample 或 Annex G sync frame 的 typed 解析结果，不经过 `trace`
//! JSON。所有换算只用于展示；不应用响度、DRC、dialogue enhancement、downmix 或 PCM。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;
use std::io::Write as IoWrite;
use std::path::Path;

use macindecode_ac4_bitstream::{
    Ac4PresentationSubstream, Ac4Toc, PresentationChannelContext, PresentationDrcConfiguration,
    PresentationDrcDecoderMode, PresentationDrcProfile, PresentationDrcState,
    PresentationSubstreamContext, PresentationSubstreamError, PresentationSubstreamGroupGainState,
    SequenceTransition, SyncFrameIter,
    audio_substream::{
        Ac4AudioSubstream, AudioSubstreamError, DialogEnhancementConfiguration,
        DialogEnhancementConfigurationUpdate, FurtherLoudnessInfo, PreprocessingMetadata,
        SubstreamContext,
    },
    presentation::presentation_config_label,
    substream::{ChannelMode, SubstreamInfo, SubstreamInfoChan},
    topology::{Ac4Topology, ConfigFingerprint, RandomAccess, TopologyError},
};
use macindecode_ac4_mp4::{
    Ac4BitrateDsi, Ac4Dsi, Ac4DsiPresentationIndicators, BaseSamplingFrequency, SampleTable,
    dsi::frame_rate, find_box, parse_header_timing,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::container::{AUDIO_SAMPLE_ENTRY_LEN, find_ac4_track};
use crate::wire::{CliError, DiagnosticCode};

/// 可选字段的稳定可用性状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FieldStatus {
    Present,
    NotPresent,
    NotApplicable,
    Unknown,
    Unsupported,
}

/// 解析未能形成 typed metadata 时，报告层可承诺的可用性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataFailure {
    Unknown,
    Unsupported,
}

/// 同时承载语义值、单位与原始码值的 wire 字段。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ReportedField {
    status: FieldStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unit: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_code: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl ReportedField {
    fn present(value: impl Into<Value>) -> Self {
        Self {
            status: FieldStatus::Present,
            value: Some(value.into()),
            unit: None,
            raw_code: None,
            reason: None,
        }
    }

    fn present_unit(value: impl Into<Value>, unit: &'static str) -> Self {
        Self {
            unit: Some(unit),
            ..Self::present(value)
        }
    }

    fn present_raw(
        value: impl Into<Value>,
        unit: Option<&'static str>,
        raw: impl Into<Value>,
    ) -> Self {
        Self {
            status: FieldStatus::Present,
            value: Some(value.into()),
            unit,
            raw_code: Some(raw.into()),
            reason: None,
        }
    }

    fn not_present() -> Self {
        Self::unavailable(FieldStatus::NotPresent, None)
    }

    fn not_applicable() -> Self {
        Self::unavailable(FieldStatus::NotApplicable, None)
    }

    fn unknown(reason: impl Into<String>) -> Self {
        Self::unavailable(FieldStatus::Unknown, Some(reason.into()))
    }

    fn unknown_raw(raw: impl Into<Value>, reason: impl Into<String>) -> Self {
        Self {
            raw_code: Some(raw.into()),
            ..Self::unknown(reason)
        }
    }

    fn unsupported(reason: impl Into<String>) -> Self {
        Self::unavailable(FieldStatus::Unsupported, Some(reason.into()))
    }

    fn unavailable(status: FieldStatus, reason: Option<String>) -> Self {
        Self {
            status,
            value: None,
            unit: None,
            raw_code: None,
            reason,
        }
    }

    fn text(&self) -> String {
        match self.status {
            FieldStatus::Present => {
                let mut value = self
                    .value
                    .as_ref()
                    .map(value_text)
                    .unwrap_or_else(|| "Present".to_owned());
                if let Some(unit) = self.unit {
                    value.push(' ');
                    value.push_str(unit);
                }
                if let Some(raw) = self.raw_code.as_ref() {
                    value.push_str(" (raw: ");
                    value.push_str(&value_text(raw));
                    value.push(')');
                }
                value
            }
            FieldStatus::NotPresent => "Not present".to_owned(),
            FieldStatus::NotApplicable => "Not applicable".to_owned(),
            FieldStatus::Unknown => reason_text("Unknown", self.reason.as_deref()),
            FieldStatus::Unsupported => reason_text("Unsupported", self.reason.as_deref()),
        }
    }
}

fn reason_text(label: &str, reason: Option<&str>) -> String {
    reason.map_or_else(|| label.to_owned(), |reason| format!("{label}: {reason}"))
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
        Value::Number(number) => number.to_string(),
        Value::Object(object) => {
            if let Some(display) = object.get("display").and_then(Value::as_str) {
                return display.to_owned();
            }
            serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_owned())
        }
        Value::Array(_) => serde_json::to_string(value).unwrap_or_else(|_| "[]".to_owned()),
        Value::Null => "null".to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InspectIssue {
    code: String,
    severity: &'static str,
    message: String,
    frame_index: Option<u64>,
    presentation_id: Option<u32>,
    substream_index: Option<u32>,
}

impl InspectIssue {
    fn warning(code: &str, message: impl Into<String>, frame_index: Option<u64>) -> Self {
        Self {
            code: code.to_owned(),
            severity: "warning",
            message: message.into(),
            frame_index,
            presentation_id: None,
            substream_index: None,
        }
    }

    fn presentation(mut self, presentation_id: Option<u32>) -> Self {
        self.presentation_id = presentation_id;
        self
    }

    fn substream(mut self, substream_index: u32) -> Self {
        self.substream_index = Some(substream_index);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectSource {
    kind: &'static str,
    input: String,
    track_index: ReportedField,
    frame_count: u64,
    duration: ReportedField,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectStream {
    codec: String,
    bit_rate: ReportedField,
    estimated_average_bit_rate: ReportedField,
    bitstream_version: ReportedField,
    frame_rate: ReportedField,
    sample_rate: ReportedField,
    i_frame: ReportedField,
    i_frame_interval: ReportedField,
    sync_word: ReportedField,
    crc_errors: ReportedField,
    number_of_presentations: ReportedField,
    number_of_audio_substreams: ReportedField,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectLoudness {
    loudness: ReportedField,
    version: ReportedField,
    regulation_type: ReportedField,
    correction_type: ReportedField,
    dialogue_intelligence: ReportedField,
    integrated_speech_gated: ReportedField,
    integrated_level_gated: ReportedField,
    maximum_true_peak: ReportedField,
    maximum_momentary_loudness: ReportedField,
    loudness_range: ReportedField,
}

impl Default for InspectLoudness {
    fn default() -> Self {
        Self {
            loudness: ReportedField::not_present(),
            version: ReportedField::not_present(),
            regulation_type: ReportedField::not_present(),
            correction_type: ReportedField::not_present(),
            dialogue_intelligence: ReportedField::not_present(),
            integrated_speech_gated: ReportedField::not_present(),
            integrated_level_gated: ReportedField::not_present(),
            maximum_true_peak: ReportedField::not_present(),
            maximum_momentary_loudness: ReportedField::not_present(),
            loudness_range: ReportedField::not_present(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectDrc {
    enhanced_ac3_profile: ReportedField,
    home_theater_avr: ReportedField,
    flat_panel_tv: ReportedField,
    portable_speakers: ReportedField,
    portable_headphones: ReportedField,
}

impl Default for InspectDrc {
    fn default() -> Self {
        Self {
            enhanced_ac3_profile: ReportedField::not_present(),
            home_theater_avr: ReportedField::not_present(),
            flat_panel_tv: ReportedField::not_present(),
            portable_speakers: ReportedField::not_present(),
            portable_headphones: ReportedField::not_present(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectMixing {
    main_audio_ducking_level: ReportedField,
    main_audio_ducking_level_center: ReportedField,
    main_audio_ducking_level_front: ReportedField,
}

impl Default for InspectMixing {
    fn default() -> Self {
        Self {
            main_audio_ducking_level: ReportedField::not_present(),
            main_audio_ducking_level_center: ReportedField::not_present(),
            main_audio_ducking_level_front: ReportedField::not_present(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectDownmix {
    loro_center_mix_gain: ReportedField,
    loro_surround_mix_gain: ReportedField,
    ltrt_center_mix_gain: ReportedField,
    ltrt_surround_mix_gain: ReportedField,
    lfe_mix_info: ReportedField,
    lfe_mix_gain: ReportedField,
    preferred_downmix: ReportedField,
}

impl Default for InspectDownmix {
    fn default() -> Self {
        Self {
            loro_center_mix_gain: ReportedField::not_present(),
            loro_surround_mix_gain: ReportedField::not_present(),
            ltrt_center_mix_gain: ReportedField::not_present(),
            ltrt_surround_mix_gain: ReportedField::not_present(),
            lfe_mix_info: ReportedField::not_present(),
            lfe_mix_gain: ReportedField::not_present(),
            preferred_downmix: ReportedField::not_present(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectPresentation {
    index: usize,
    presentation_id: ReportedField,
    summary: ReportedField,
    presentation_type: ReportedField,
    minimal_compatibility_level: ReportedField,
    dialogue_normalization: ReportedField,
    language: ReportedField,
    multi_pid: ReportedField,
    bit_rate: ReportedField,
    audio_substreams: ReportedField,
    metadata_authentication_id: ReportedField,
    loudness: InspectLoudness,
    dynamic_range_control: InspectDrc,
    mixing_metadata: InspectMixing,
    downmix: InspectDownmix,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectPreprocessing {
    previous_mix_type_2channel: ReportedField,
    phase90_filter_info_2channel: ReportedField,
    loro_center_mix_gain: ReportedField,
    loro_surround_mix_gain: ReportedField,
    loro_downmix_loudness_correction: ReportedField,
    ltrt_center_mix_gain: ReportedField,
    ltrt_surround_mix_gain: ReportedField,
    ltrt_downmix_loudness_correction: ReportedField,
    lfe_mix_gain: ReportedField,
    preferred_downmix: ReportedField,
    previous_downmix_type_5channel: ReportedField,
    previous_upmix_type_5channel: ReportedField,
    previous_upmix_type_3_4: ReportedField,
    previous_upmix_type_3_2_2: ReportedField,
    phase90_filter_info: ReportedField,
    surround_attenuation_known: ReportedField,
    lfe_attenuation_known: ReportedField,
}

impl Default for InspectPreprocessing {
    fn default() -> Self {
        Self {
            previous_mix_type_2channel: ReportedField::not_present(),
            phase90_filter_info_2channel: ReportedField::not_present(),
            loro_center_mix_gain: ReportedField::not_present(),
            loro_surround_mix_gain: ReportedField::not_present(),
            loro_downmix_loudness_correction: ReportedField::not_present(),
            ltrt_center_mix_gain: ReportedField::not_present(),
            ltrt_surround_mix_gain: ReportedField::not_present(),
            ltrt_downmix_loudness_correction: ReportedField::not_present(),
            lfe_mix_gain: ReportedField::not_present(),
            preferred_downmix: ReportedField::not_present(),
            previous_downmix_type_5channel: ReportedField::not_present(),
            previous_upmix_type_5channel: ReportedField::not_present(),
            previous_upmix_type_3_4: ReportedField::not_present(),
            previous_upmix_type_3_2_2: ReportedField::not_present(),
            phase90_filter_info: ReportedField::not_present(),
            surround_attenuation_known: ReportedField::not_present(),
            lfe_attenuation_known: ReportedField::not_present(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectDialogueEnhancement {
    enabled: ReportedField,
    method: ReportedField,
    max_gain: ReportedField,
    channel_configuration: ReportedField,
}

impl Default for InspectDialogueEnhancement {
    fn default() -> Self {
        Self {
            enabled: ReportedField::present_raw(false, None, false),
            method: ReportedField::not_present(),
            max_gain: ReportedField::not_present(),
            channel_configuration: ReportedField::not_present(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectAudioSubstream {
    index: u32,
    summary: ReportedField,
    channel_configuration: ReportedField,
    channel_layout: ReportedField,
    object_coded: ReportedField,
    bit_rate: ReportedField,
    preprocessing: InspectPreprocessing,
    dialogue_enhancement: InspectDialogueEnhancement,
}

/// `inspectResult` 的固定 domain/wire 骨架。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct InspectReport {
    pub(crate) source: InspectSource,
    pub(crate) stream: InspectStream,
    pub(crate) presentations: Vec<InspectPresentation>,
    pub(crate) audio_substreams: Vec<InspectAudioSubstream>,
    pub(crate) issues: Vec<InspectIssue>,
}

#[derive(Debug, Clone, Copy)]
struct DurationRatio {
    numerator: u128,
    denominator: u128,
}

impl DurationRatio {
    fn seconds(self) -> Option<f64> {
        (self.denominator != 0).then(|| self.numerator as f64 / self.denominator as f64)
    }

    fn bit_rate(self, bytes: u128) -> Option<u64> {
        if self.numerator == 0 || self.denominator == 0 {
            return None;
        }
        let bits = bytes.checked_mul(8)?.checked_mul(self.denominator)?;
        let half = self.numerator.checked_div(2)?;
        let rounded = bits.checked_add(half)?.checked_div(self.numerator)?;
        u64::try_from(rounded).ok()
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        if self.denominator == 0 || other.denominator == 0 {
            return None;
        }
        let common = gcd_u128(self.denominator, other.denominator)?;
        let left_scale = other.denominator.checked_div(common)?;
        let right_scale = self.denominator.checked_div(common)?;
        let numerator = self
            .numerator
            .checked_mul(left_scale)?
            .checked_add(other.numerator.checked_mul(right_scale)?)?;
        let denominator = self.denominator.checked_mul(left_scale)?;
        let reduction = gcd_u128(numerator, denominator)?;
        Some(Self {
            numerator: numerator.checked_div(reduction)?,
            denominator: denominator.checked_div(reduction)?,
        })
    }
}

fn gcd_u128(mut left: u128, mut right: u128) -> Option<u128> {
    while right != 0 {
        let remainder = left.checked_rem(right)?;
        left = right;
        right = remainder;
    }
    (left != 0).then_some(left)
}

#[derive(Debug, Clone)]
struct DsiPresentationSummary {
    index: usize,
    effective_id: Option<u32>,
    presentation_config: u8,
    md_compat: Option<u8>,
    multi_pid: Option<bool>,
    channel_mode: Option<u8>,
    bitrate: Option<Ac4BitrateDsi>,
    alternative: bool,
    indicators: Option<Ac4DsiPresentationIndicators>,
    languages: Vec<Vec<u8>>,
    group_classifiers: Vec<Option<u8>>,
}

#[derive(Debug, Clone)]
struct DsiSummary {
    bitstream_version: u8,
    sample_rate: u32,
    frame_rate_numerator: u32,
    frame_rate_denominator: u32,
    bitrate: Option<Ac4BitrateDsi>,
    presentations: Vec<DsiPresentationSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PresentationKey {
    Id(u32),
    DuplicateId(u32, usize),
    Anonymous(usize),
}

#[derive(Debug, Default)]
struct PresentationParseState {
    drc: PresentationDrcState,
    group_gain: PresentationSubstreamGroupGainState,
}

#[derive(Debug, Clone)]
struct PresentationAccumulator {
    index: usize,
    id: Option<u32>,
    presentation_config: Option<u32>,
    md_compat: Option<u8>,
    multi_pid: Option<bool>,
    alternative: bool,
    scene_path: &'static str,
    channel_label: Option<String>,
    audio_substreams: BTreeSet<u32>,
    role: &'static str,
    language: Option<Vec<u8>>,
    language_conflict: bool,
    dsi_bitrate: Option<Ac4BitrateDsi>,
    measured_audio_bytes: u128,
    parsed_metadata: Option<PresentationMetadataOwned>,
    metadata_failure: Option<MetadataFailure>,
    metadata_conflicts: PresentationMetadataConflicts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PresentationMetadataOwned {
    dialnorm_bits: u8,
    further_loudness: Option<FurtherLoudnessInfo>,
    drc_configuration: Option<PresentationDrcConfiguration>,
    associated_scale: Option<(Option<u8>, Option<u8>, Option<u8>)>,
    stereo_downmix: Option<macindecode_ac4_bitstream::PresentationStereoDownmixCoefficients>,
}

#[derive(Debug, Clone, Copy, Default)]
struct PresentationMetadataConflicts {
    dialnorm: bool,
    further_loudness: bool,
    drc_configuration: bool,
    associated_scale: bool,
    stereo_downmix: bool,
}

#[derive(Debug, Clone)]
struct SubstreamAccumulator {
    index: u32,
    role: &'static str,
    channel_info: Option<SubstreamInfoChan>,
    ims: bool,
    object_coded: bool,
    measured_bytes: u128,
    preprocessing: Option<PreprocessingMetadata>,
    preprocessing_sampled: bool,
    preprocessing_conflict: bool,
    metadata_failure: Option<MetadataFailure>,
    de_configuration: Option<DialogEnhancementConfiguration>,
    reported_de_configuration: Option<DialogEnhancementConfiguration>,
    de_configuration_sampled: bool,
    de_configuration_conflict: bool,
    de_seen: bool,
}

#[derive(Debug, Default)]
struct Aggregator {
    canonical_fingerprint: Option<ConfigFingerprint>,
    last_fingerprint: Option<ConfigFingerprint>,
    canonical_changed: bool,
    frames_before_baseline: u64,
    previous_sequence: Option<u16>,
    bitstream_versions: BTreeSet<u32>,
    fs_indices: BTreeSet<u8>,
    frame_rate_indices: BTreeSet<u8>,
    first_iframe: Option<bool>,
    last_iframe_index: Option<u64>,
    iframe_intervals: BTreeMap<u64, u64>,
    frame_count: u64,
    observed_duration: Option<DurationRatio>,
    duration_unavailable: bool,
    presentations: BTreeMap<PresentationKey, PresentationAccumulator>,
    presentation_states: BTreeMap<PresentationKey, PresentationParseState>,
    presentation_channels: BTreeMap<PresentationKey, PresentationChannelContext>,
    substreams: BTreeMap<u32, SubstreamAccumulator>,
    audio_contexts: BTreeMap<u32, SubstreamContext>,
    issues: Vec<InspectIssue>,
}

/// 读取并聚合一个 AC-4 输入。
pub(crate) fn run(path: &Path) -> Result<InspectReport, CliError> {
    let data = std::fs::read(path).map_err(|error| {
        CliError::new(
            "inspect",
            DiagnosticCode::InputReadFailed,
            "Failed to read input file",
        )
        .with_context("path", path.display().to_string())
        .with_context("cause", error.to_string())
    })?;
    if data.is_empty() {
        return Err(CliError::new(
            "inspect",
            DiagnosticCode::InputInvalid,
            "Input file is empty",
        )
        .with_context("path", path.display().to_string()));
    }

    let result = if matches!(data.get(..2), Some([0xac, 0x40] | [0xac, 0x41])) {
        inspect_raw(&data, path)
    } else {
        inspect_mp4(&data, path)
    };
    result.map_err(|cause| {
        CliError::new(
            "inspect",
            DiagnosticCode::ParseFailed,
            "Failed to parse AC-4 input",
        )
        .with_context("path", path.display().to_string())
        .with_context("cause", cause)
    })
}

fn inspect_raw(data: &[u8], path: &Path) -> Result<InspectReport, String> {
    let mut aggregate = Aggregator::default();
    let mut total_transport_bytes = 0u128;
    let mut sync_words = BTreeSet::new();
    let mut crc_protected = 0u64;
    let mut crc_failures = 0u64;

    for item in SyncFrameIter::new(data) {
        let frame = item.map_err(|error| error.to_string())?;
        let index = aggregate.frame_count;
        total_transport_bytes = total_transport_bytes.saturating_add(frame.total_size as u128);
        sync_words.insert(frame.sync_word.as_u16());
        if frame.crc_word.is_some() {
            crc_protected = crc_protected.saturating_add(1);
            if frame.verify_crc(data) != Some(true) {
                crc_failures = crc_failures.saturating_add(1);
                aggregate.issues.push(InspectIssue::warning(
                    "crc_mismatch",
                    "Annex G CRC verification failed",
                    Some(index),
                ));
            }
        }
        aggregate.observe(frame.raw_frame, index)?;
    }
    if aggregate.frame_count == 0 {
        return Err("Input contains no AC-4 sync frames".to_owned());
    }

    let duration = aggregate.observed_duration;
    let sync_word = match sync_words.len() {
        0 => ReportedField::not_present(),
        1 => {
            let raw = sync_words.iter().next().copied().unwrap_or_default();
            ReportedField::present_raw(format!("0x{raw:04X}"), None, raw)
        }
        _ => {
            aggregate.issues.push(InspectIssue::warning(
                "sync_word_changed",
                "Annex G sync word changes between frames",
                None,
            ));
            ReportedField::unknown("multiple sync words observed")
        }
    };
    let crc_errors = if crc_protected == 0 {
        ReportedField::not_present()
    } else {
        ReportedField::present_raw(
            crc_failures != 0,
            None,
            json!({"protected_frames": crc_protected, "failures": crc_failures}),
        )
    };
    aggregate.finish(
        path,
        "annex_g",
        ReportedField::not_applicable(),
        duration,
        total_transport_bytes,
        None,
        sync_word,
        crc_errors,
    )
}

fn inspect_mp4(data: &[u8], path: &Path) -> Result<InspectReport, String> {
    let moov = find_box(data, b"moov").ok_or("moov box not found")?;
    let track =
        find_ac4_track(moov.payload).ok_or("No track with an ac-4 sample entry was found")?;
    let mdhd = find_box(track.mdia.payload, b"mdhd").ok_or("mdhd box not found")?;
    let media = parse_header_timing(*b"mdhd", mdhd.payload).map_err(|error| error.to_string())?;
    let specific = track
        .sample_entry
        .payload
        .get(AUDIO_SAMPLE_ENTRY_LEN..)
        .and_then(|tail| find_box(tail, b"dac4"))
        .ok_or("ac-4 sample entry has no dac4 box")?;
    let dsi = Ac4Dsi::parse(specific.payload).map_err(|error| error.to_string())?;
    let dsi_summary = collect_dsi_summary(&dsi)?;
    let table = SampleTable::parse(track.stbl.payload).map_err(|error| error.to_string())?;

    let mut aggregate = Aggregator::default();
    let mut total_sample_bytes = 0u128;
    let mut duration_ticks = 0u128;
    for item in table.iter() {
        let info = item.map_err(|error| error.to_string())?;
        let start = usize::try_from(info.offset).unwrap_or(usize::MAX);
        let len = usize::try_from(info.size).unwrap_or(usize::MAX);
        let end = start.checked_add(len).ok_or("MP4 sample range overflow")?;
        let sample = data
            .get(start..end)
            .ok_or("MP4 AC-4 sample range exceeds the file size")?;
        total_sample_bytes = total_sample_bytes.saturating_add(u128::from(info.size));
        duration_ticks = duration_ticks.saturating_add(u128::from(info.duration));
        aggregate.observe(sample, u64::from(info.index))?;
    }
    if aggregate.frame_count == 0 {
        return Err("AC-4 sample table contains no samples".to_owned());
    }
    let duration = DurationRatio {
        numerator: duration_ticks,
        denominator: u128::from(media.timescale),
    };
    aggregate.finish(
        path,
        "mp4",
        ReportedField::present(track.index),
        Some(duration),
        total_sample_bytes,
        Some(dsi_summary),
        ReportedField::not_applicable(),
        ReportedField::not_applicable(),
    )
}

fn collect_dsi_summary(dsi: &Ac4Dsi<'_>) -> Result<DsiSummary, String> {
    let rate = dsi.frame_rate().ok_or_else(|| {
        format!(
            "dac4 frame_rate_index {} is undefined",
            dsi.frame_rate_index
        )
    })?;
    let mut summary = DsiSummary {
        bitstream_version: dsi.bitstream_version,
        sample_rate: dsi.base_sampling_frequency.hz(),
        frame_rate_numerator: rate.numerator,
        frame_rate_denominator: rate.denominator,
        bitrate: None,
        presentations: Vec::new(),
    };
    let Some(v1) = dsi.v1().map_err(|error| error.to_string())? else {
        return Ok(summary);
    };
    summary.bitrate = Some(v1.bitrate);
    for item in v1.presentations() {
        let envelope = item.map_err(|error| error.to_string())?;
        let Some(presentation) = envelope.v1().map_err(|error| error.to_string())? else {
            continue;
        };
        let mut languages = Vec::new();
        let mut group_classifiers = Vec::new();
        for group in presentation.substream_groups() {
            let group = group.map_err(|error| error.to_string())?;
            group_classifiers.push(group.content_type.map(|content| content.classifier));
            if let Some(language) = group
                .content_type
                .and_then(|content| content.language_tag)
                .map(|bytes| bytes.iter().collect::<Vec<_>>())
                && !languages.contains(&language)
            {
                languages.push(language);
            }
        }
        summary.presentations.push(DsiPresentationSummary {
            index: usize::from(presentation.index),
            effective_id: presentation.effective_presentation_id().map(u32::from),
            presentation_config: presentation.presentation_config,
            md_compat: presentation.md_compat,
            multi_pid: presentation.multi_pid,
            channel_mode: presentation
                .channel_layout
                .map(|layout| layout.channel_mode),
            bitrate: presentation.bitrate,
            alternative: presentation.alternative.is_some(),
            indicators: presentation.indicators,
            languages,
            group_classifiers,
        });
    }
    Ok(summary)
}

impl Aggregator {
    fn observe(&mut self, frame: &[u8], index: u64) -> Result<(), String> {
        let toc = Ac4Toc::parse(frame).map_err(|error| format!("Frame {index}: {error}"))?;
        self.frame_count = self.frame_count.saturating_add(1);
        self.bitstream_versions.insert(toc.bitstream_version);
        self.fs_indices.insert(toc.fs_index);
        self.frame_rate_indices.insert(toc.frame_rate_index);
        if !self.duration_unavailable {
            let next_duration = frame_duration_ratio(toc.fs_index, toc.frame_rate_index).and_then(
                |frame_duration| {
                    self.observed_duration
                        .map_or(Some(frame_duration), |duration| {
                            duration.checked_add(frame_duration)
                        })
                },
            );
            if let Some(duration) = next_duration {
                self.observed_duration = Some(duration);
            } else {
                self.duration_unavailable = true;
                self.observed_duration = None;
                self.issues.push(InspectIssue::warning(
                    "frame_timing_unknown",
                    format!(
                        "Frame duration is undefined or overflows for fs_index={} frame_rate_index={}",
                        toc.fs_index, toc.frame_rate_index
                    ),
                    Some(index),
                ));
            }
        }
        if self.first_iframe.is_none() {
            self.first_iframe = Some(toc.iframe_global);
        }
        if toc.iframe_global {
            if let Some(previous) = self.last_iframe_index {
                let interval = index.saturating_sub(previous);
                let count = self.iframe_intervals.entry(interval).or_default();
                *count = count.saturating_add(1);
            }
            self.last_iframe_index = Some(index);
        }

        let sequence_change =
            toc.sequence_transition(self.previous_sequence) == SequenceTransition::SourceChange;
        if sequence_change {
            self.reset_parser_history();
        }
        let topology = match Ac4Topology::parse(frame) {
            Ok(topology) => topology,
            Err(error @ TopologyError::Unsupported { .. }) => {
                self.reset_parser_history();
                self.last_fingerprint = None;
                self.previous_sequence = Some(toc.sequence_counter);
                if self.canonical_fingerprint.is_none() {
                    self.frames_before_baseline = self.frames_before_baseline.saturating_add(1);
                }
                self.canonical_changed |= self.canonical_fingerprint.is_some();
                self.issues.push(InspectIssue::warning(
                    "topology_unsupported",
                    error.to_string(),
                    Some(index),
                ));
                return Ok(());
            }
            Err(error) => return Err(format!("Frame {index}: {error}")),
        };
        let fingerprint = topology.config_fingerprint();
        let generation_change = self
            .last_fingerprint
            .is_some_and(|last| last != fingerprint);
        if generation_change {
            self.reset_parser_history();
        }
        if self.canonical_fingerprint.is_none() && topology.random_access() != RandomAccess::Full {
            self.frames_before_baseline = self.frames_before_baseline.saturating_add(1);
            self.previous_sequence = Some(topology.toc.sequence_counter);
            self.last_fingerprint = Some(fingerprint);
            return Ok(());
        }
        if self.canonical_fingerprint.is_none() {
            self.canonical_fingerprint = Some(fingerprint);
            self.initialize_configuration(&topology, index)?;
            if self.frames_before_baseline != 0 {
                self.issues.push(InspectIssue::warning(
                    "frames_before_complete_configuration",
                    format!(
                        "Skipped {} frame(s) before the first complete independent configuration",
                        self.frames_before_baseline
                    ),
                    Some(index),
                ));
            }
        } else if self.canonical_fingerprint != Some(fingerprint) {
            self.canonical_changed = true;
            if generation_change {
                let canonical = self.canonical_fingerprint.unwrap_or(fingerprint);
                self.issues.push(InspectIssue::warning(
                    "configuration_changed",
                    describe_configuration_change(canonical, fingerprint),
                    Some(index),
                ));
            }
            self.previous_sequence = Some(topology.toc.sequence_counter);
            self.last_fingerprint = Some(fingerprint);
            return Ok(());
        }

        self.observe_presentation_payloads(frame, &topology, index)?;
        self.observe_audio_payloads(frame, &topology, index)?;
        self.previous_sequence = Some(topology.toc.sequence_counter);
        self.last_fingerprint = Some(fingerprint);
        Ok(())
    }

    fn reset_parser_history(&mut self) {
        self.presentation_states.clear();
        self.presentation_channels.clear();
        self.audio_contexts.clear();
        for substream in self.substreams.values_mut() {
            substream.de_configuration = None;
        }
    }

    fn initialize_configuration(
        &mut self,
        topology: &Ac4Topology,
        frame_index: u64,
    ) -> Result<(), String> {
        let mut id_counts = BTreeMap::<u32, usize>::new();
        for id in topology
            .presentations()
            .iter()
            .filter_map(|presentation| presentation.presentation_id)
        {
            let count = id_counts.entry(id).or_default();
            *count = count.saturating_add(1);
        }
        for (&id, &count) in &id_counts {
            if count > 1 {
                self.issues.push(
                    InspectIssue::warning(
                        "duplicate_presentation_id",
                        format!(
                            "Effective presentation ID {id} occurs {count} times; state is isolated by occurrence"
                        ),
                        Some(frame_index),
                    )
                    .presentation(Some(id)),
                );
            }
        }
        for (presentation_index, presentation) in topology.presentations().iter().enumerate() {
            let key = presentation_key(topology, presentation_index, presentation.presentation_id);
            let alternative = presentation
                .substream
                .is_some_and(|substream| substream.alternative);
            let ims = presentation.presentation_version == 2;
            let mut audio_substreams = BTreeSet::new();
            let mut channel_info = None;
            let mut channel_info_conflict = false;
            let mut has_object = false;
            let mut has_channel = false;

            for (position, &group_index) in presentation.group_indices().iter().enumerate() {
                let group_index = usize::try_from(group_index)
                    .map_err(|_| "Presentation group index exceeds usize".to_owned())?;
                let group = topology.groups().get(group_index).ok_or_else(|| {
                    "Presentation references a missing substream group".to_owned()
                })?;
                let role = group_role(
                    presentation.presentation_config,
                    presentation.single_substream_group,
                    position,
                    group.content_type.map(|content| content.content_classifier),
                );
                for info in group.substreams() {
                    let Some(first) = info.substream_index() else {
                        continue;
                    };
                    match *info {
                        SubstreamInfo::Chan(ref channel) => {
                            has_channel = true;
                            if let Some(previous) = channel_info {
                                channel_info_conflict |=
                                    !same_channel_layout_info(previous, *channel);
                            } else {
                                channel_info = Some(*channel);
                            }
                        }
                        SubstreamInfo::Ajoc(_) | SubstreamInfo::Obj(_) => has_object = true,
                    }
                    for offset in 0..group.frame_rate_factor {
                        let index = first
                            .checked_add(offset)
                            .ok_or_else(|| "Audio substream index overflow".to_owned())?;
                        audio_substreams.insert(index);
                        let accumulator = self.substreams.entry(index).or_insert_with(|| {
                            let (channel_info, object_coded) = match *info {
                                SubstreamInfo::Chan(ref channel) => (Some(*channel), false),
                                SubstreamInfo::Ajoc(_) | SubstreamInfo::Obj(_) => (None, true),
                            };
                            SubstreamAccumulator {
                                index,
                                role,
                                channel_info,
                                ims,
                                object_coded,
                                measured_bytes: 0,
                                preprocessing: None,
                                preprocessing_sampled: false,
                                preprocessing_conflict: false,
                                metadata_failure: None,
                                de_configuration: None,
                                reported_de_configuration: None,
                                de_configuration_sampled: false,
                                de_configuration_conflict: false,
                                de_seen: false,
                            }
                        });
                        accumulator.ims |= ims;
                        if let SubstreamInfo::Chan(channel) = *info
                            && accumulator.channel_info.is_some_and(|previous| {
                                !same_channel_layout_info(previous, channel)
                            })
                        {
                            accumulator.channel_info = None;
                        }
                    }
                }
            }

            let scene_path = if ims {
                "IMS"
            } else {
                match (has_channel, has_object) {
                    (true, true) => "Mixed",
                    (false, true) => "Object-Based",
                    (true, false) => "Channel-Based",
                    (false, false) => "Data-Only",
                }
            };
            let role = if alternative { "Alternative" } else { "Main" };
            self.presentations.insert(
                key,
                PresentationAccumulator {
                    index: presentation_index,
                    id: presentation.presentation_id,
                    presentation_config: presentation.presentation_config,
                    md_compat: presentation.md_compat,
                    multi_pid: presentation.multi_pid,
                    alternative,
                    scene_path,
                    channel_label: (!channel_info_conflict)
                        .then(|| channel_info.and_then(effective_channel_layout_label))
                        .flatten(),
                    audio_substreams,
                    role,
                    language: None,
                    language_conflict: false,
                    dsi_bitrate: None,
                    measured_audio_bytes: 0,
                    parsed_metadata: None,
                    metadata_failure: None,
                    metadata_conflicts: PresentationMetadataConflicts::default(),
                },
            );
        }
        Ok(())
    }

    fn observe_presentation_payloads(
        &mut self,
        frame: &[u8],
        topology: &Ac4Topology,
        frame_index: u64,
    ) -> Result<(), String> {
        for (presentation_index, presentation) in topology.presentations().iter().enumerate() {
            let Some(substream) = presentation.substream else {
                continue;
            };
            let key = presentation_key(topology, presentation_index, presentation.presentation_id);
            let Some(context) = topology.presentation_substream_context(presentation_index) else {
                if let Some(accumulator) = self.presentations.get_mut(&key) {
                    record_metadata_failure(
                        &mut accumulator.metadata_failure,
                        MetadataFailure::Unknown,
                    );
                }
                self.issues.push(
                    InspectIssue::warning(
                        "presentation_context_unavailable",
                        "Presentation metadata context could not be derived",
                        Some(frame_index),
                    )
                    .presentation(presentation.presentation_id),
                );
                continue;
            };
            let payload = match topology.substream_payload(frame, substream.substream_index) {
                Ok(payload) => payload,
                Err(error) => {
                    return Err(format!(
                        "Frame {frame_index}, presentation {:?}: {error}",
                        presentation.presentation_id
                    ));
                }
            };
            let state = self.presentation_states.entry(key).or_default();
            let locked_context = self
                .presentation_channels
                .get(&key)
                .copied()
                .map(|channel| {
                    PresentationSubstreamContext::new(
                        context.selection_context().alternative(),
                        context.presentation_is_independent(),
                        context.selection_context().n_audio_substreams(),
                        context.n_substream_groups(),
                        channel,
                    )
                });
            let ims_context = (presentation.presentation_version == 2).then(|| {
                PresentationSubstreamContext::new(
                    context.selection_context().alternative(),
                    context.presentation_is_independent(),
                    context.selection_context().n_audio_substreams(),
                    context.n_substream_groups(),
                    PresentationChannelContext::new(Some(1), None, false, 0, false),
                )
            });
            let parsed = match if let Some(locked_context) = locked_context {
                Ac4PresentationSubstream::parse_with_drc_state_compat(
                    payload,
                    locked_context,
                    &mut state.drc,
                )
                .map(|(parsed, _)| (parsed, locked_context))
            } else {
                Ac4PresentationSubstream::parse_with_drc_state_compat(
                    payload,
                    context,
                    &mut state.drc,
                )
                .map(|(parsed, _)| (parsed, context))
                .or_else(|primary_error| {
                    let Some(ims_context) = ims_context else {
                        return Err(primary_error);
                    };
                    Ac4PresentationSubstream::parse_with_drc_state_compat(
                        payload,
                        ims_context,
                        &mut state.drc,
                    )
                    .map(|(parsed, _)| (parsed, ims_context))
                })
            } {
                Ok(parsed) => parsed,
                Err(error) => {
                    if !presentation_metadata_error_is_reportable(error) {
                        return Err(format!(
                            "Frame {frame_index}, presentation {:?}: {error}",
                            presentation.presentation_id
                        ));
                    }
                    let failure = presentation_metadata_failure(error);
                    if let Some(accumulator) = self.presentations.get_mut(&key) {
                        record_metadata_failure(&mut accumulator.metadata_failure, failure);
                    }
                    let code = if presentation_metadata_error_is_reserved(error) {
                        "reserved_code"
                    } else if failure == MetadataFailure::Unknown {
                        "presentation_metadata_unavailable"
                    } else {
                        "presentation_metadata_unsupported"
                    };
                    self.issues.push(
                        InspectIssue::warning(
                            code,
                            format!(
                                "{error}; payload_bytes={}; context={context:?}",
                                payload.len()
                            ),
                            Some(frame_index),
                        )
                        .presentation(presentation.presentation_id),
                    );
                    continue;
                }
            };
            let (parsed, effective_context) = parsed;
            self.presentation_channels
                .entry(key)
                .or_insert(effective_context.channel_context());
            if let Err(error) = state
                .group_gain
                .apply(parsed.substream_group_gain_update, effective_context)
            {
                self.issues.push(
                    InspectIssue::warning(
                        "presentation_group_gain_state",
                        error.to_string(),
                        Some(frame_index),
                    )
                    .presentation(presentation.presentation_id),
                );
            }
            let drc_configuration = state.drc.configuration();
            let associated_scale = parsed.associated_audio.map(|associated| {
                (
                    associated.scale_main,
                    associated.scale_main_centre,
                    associated.scale_main_front,
                )
            });
            let metadata = PresentationMetadataOwned {
                dialnorm_bits: parsed.dialnorm_bits,
                further_loudness: parsed.further_loudness,
                drc_configuration,
                associated_scale,
                stereo_downmix: parsed.custom_downmix.stereo_coefficients(),
            };
            // Stable report metadata is sampled at complete presentation random-access points.
            // Dependent frames may omit optional blocks while retaining the prior effective
            // configuration; omission is not itself a configuration change.
            if !effective_context.presentation_is_independent() {
                continue;
            }
            let mut conflict_message = None;
            if let Some(accumulator) = self.presentations.get_mut(&key) {
                match accumulator.parsed_metadata.as_ref() {
                    None => accumulator.parsed_metadata = Some(metadata),
                    Some(previous) if *previous == metadata => {}
                    Some(previous) => {
                        conflict_message = record_presentation_metadata_conflicts(
                            &mut accumulator.metadata_conflicts,
                            previous,
                            &metadata,
                        );
                    }
                }
            }
            if let Some(message) = conflict_message {
                self.issues.push(
                    InspectIssue::warning(
                        "presentation_metadata_changed",
                        message,
                        Some(frame_index),
                    )
                    .presentation(presentation.presentation_id),
                );
            }
        }
        Ok(())
    }

    fn observe_audio_payloads(
        &mut self,
        frame: &[u8],
        topology: &Ac4Topology,
        frame_index: u64,
    ) -> Result<(), String> {
        let contexts = audio_contexts(topology);
        for (&substream_index, candidates) in &contexts {
            let payload = match topology.substream_payload(frame, substream_index) {
                Ok(payload) => payload,
                Err(error) => {
                    return Err(format!(
                        "Frame {frame_index}, audio substream {substream_index}: {error}"
                    ));
                }
            };
            if let Some(accumulator) = self.substreams.get_mut(&substream_index) {
                accumulator.measured_bytes = accumulator
                    .measured_bytes
                    .saturating_add(payload.len() as u128);
            }
            let mut successful = Vec::new();
            let mut failures = Vec::new();
            for &context in candidates {
                match Ac4AudioSubstream::parse(payload, context) {
                    Ok(parsed) => successful.push((context, parsed)),
                    Err(error) => failures.push((context, error)),
                }
            }
            let selected = if let Some(locked) = self.audio_contexts.get(&substream_index).copied()
            {
                successful
                    .iter()
                    .copied()
                    .find(|(context, _)| same_audio_context_family(*context, locked))
            } else if successful.len() == 1 {
                let selected = successful.pop();
                if let Some((context, _)) = selected {
                    self.audio_contexts.insert(substream_index, context);
                }
                selected
            } else {
                // 多个语法候选同时成功时暂不锁定；IMS 优先展示物理 stereo metadata。
                successful
                    .iter()
                    .copied()
                    .find(|(context, _)| context.channel_mode == Some(1))
                    .or_else(|| successful.first().copied())
            };
            let Some((context, parsed)) = selected else {
                if !successful.is_empty() {
                    self.issues.push(
                        InspectIssue::warning(
                            "audio_metadata_context_changed",
                            "Only an audio metadata context outside the locked configuration parsed successfully",
                            Some(frame_index),
                        )
                        .substream(substream_index),
                    );
                    if let Some(accumulator) = self.substreams.get_mut(&substream_index) {
                        accumulator.preprocessing_conflict = true;
                        accumulator.de_configuration_conflict = true;
                    }
                    continue;
                }
                let reportable = failures
                    .iter()
                    .find(|(_, error)| audio_metadata_error_is_reportable(*error));
                if let Some((context, error)) = reportable {
                    if let Some(accumulator) = self.substreams.get_mut(&substream_index) {
                        record_metadata_failure(
                            &mut accumulator.metadata_failure,
                            MetadataFailure::Unsupported,
                        );
                    }
                    self.issues.push(
                        InspectIssue::warning(
                            "audio_metadata_unsupported",
                            format!(
                                "{error}; payload_bytes={}; context={context:?}",
                                payload.len()
                            ),
                            Some(frame_index),
                        )
                        .substream(substream_index),
                    );
                    continue;
                }
                let detail = failures.first().map_or_else(
                    || "no metadata context was derived".to_owned(),
                    |(context, error)| format!("{error}; context={context:?}"),
                );
                return Err(format!(
                    "Frame {frame_index}, audio substream {substream_index}: {detail}"
                ));
            };
            let mut preprocessing_changed = false;
            let mut de_configuration_changed = false;
            if let Some(accumulator) = self.substreams.get_mut(&substream_index) {
                let independent = context.b_iframe == Some(true);
                if independent {
                    let preprocessing = parsed.basic.preprocessing;
                    if accumulator.preprocessing_sampled {
                        if accumulator.preprocessing != preprocessing
                            && !accumulator.preprocessing_conflict
                        {
                            accumulator.preprocessing_conflict = true;
                            preprocessing_changed = true;
                        }
                    } else {
                        accumulator.preprocessing = preprocessing;
                        accumulator.preprocessing_sampled = true;
                    }
                }
                if accumulator.ims
                    && context.channel_mode == Some(1)
                    && let Some(mut info) = accumulator.channel_info
                {
                    info.channel_mode = ChannelMode {
                        codeword: 0b10,
                        ch_mode: 1,
                    };
                    info.four_back_channels_present = None;
                    info.centre_present = None;
                    info.top_channels_present = None;
                    accumulator.channel_info = Some(info);
                }
                let de = parsed.tools_metadata.dialog_enhancement;
                accumulator.de_seen |= de.data_present;
                let de_configuration_is_new = matches!(
                    de.configuration,
                    DialogEnhancementConfigurationUpdate::New(_)
                );
                match de.configuration {
                    DialogEnhancementConfigurationUpdate::NotPresent => {
                        if independent {
                            accumulator.de_configuration = None;
                        }
                    }
                    DialogEnhancementConfigurationUpdate::KeepPrevious => {}
                    DialogEnhancementConfigurationUpdate::New(configuration) => {
                        accumulator.de_configuration = Some(configuration);
                    }
                }
                if independent {
                    if accumulator.de_configuration_sampled {
                        if accumulator.reported_de_configuration != accumulator.de_configuration
                            && !accumulator.de_configuration_conflict
                        {
                            accumulator.de_configuration_conflict = true;
                            de_configuration_changed = true;
                        }
                    } else {
                        accumulator.reported_de_configuration = accumulator.de_configuration;
                        accumulator.de_configuration_sampled = true;
                    }
                } else if de_configuration_is_new
                    && accumulator.de_configuration_sampled
                    && accumulator.reported_de_configuration != accumulator.de_configuration
                    && !accumulator.de_configuration_conflict
                {
                    accumulator.de_configuration_conflict = true;
                    de_configuration_changed = true;
                }
            }
            if preprocessing_changed {
                self.issues.push(
                    InspectIssue::warning(
                        "preprocessing_metadata_changed",
                        "Stable preprocessing metadata changes within the configuration",
                        Some(frame_index),
                    )
                    .substream(substream_index),
                );
            }
            if de_configuration_changed {
                self.issues.push(
                    InspectIssue::warning(
                        "dialogue_enhancement_configuration_changed",
                        "Dialogue Enhancement configuration changes within the configuration",
                        Some(frame_index),
                    )
                    .substream(substream_index),
                );
            }
        }

        for presentation in self.presentations.values_mut() {
            for &substream_index in &presentation.audio_substreams {
                if let Ok(payload) = topology.substream_payload(frame, substream_index) {
                    presentation.measured_audio_bytes = presentation
                        .measured_audio_bytes
                        .saturating_add(payload.len() as u128);
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        mut self,
        path: &Path,
        source_kind: &'static str,
        track_index: ReportedField,
        duration: Option<DurationRatio>,
        source_bytes: u128,
        dsi: Option<DsiSummary>,
        sync_word: ReportedField,
        crc_errors: ReportedField,
    ) -> Result<InspectReport, String> {
        if let Some(dsi) = dsi.as_ref() {
            self.apply_dsi(dsi);
        }

        let duration_field = duration
            .and_then(DurationRatio::seconds)
            .map(|seconds| ReportedField::present_unit(round_decimal(seconds, 3), "seconds"))
            .unwrap_or_else(|| ReportedField::unknown("duration is unavailable"));
        let bit_rate = duration
            .and_then(|duration| duration.bit_rate(source_bytes))
            .map(bit_rate_field)
            .unwrap_or_else(|| ReportedField::unknown("duration is unavailable"));
        let estimated_average_bit_rate = dsi
            .as_ref()
            .and_then(|summary| summary.bitrate)
            .map_or_else(ReportedField::not_present, bitrate_range_field);

        let observed_version = single_raw_value_field(&self.bitstream_versions);
        let observed_fs = self.fs_indices.iter().next().copied();
        let observed_rate_index = self.frame_rate_indices.iter().next().copied();
        let (sample_rate, frame_rate_report) =
            if self.fs_indices.len() == 1 && self.frame_rate_indices.len() == 1 {
                let fs_index = observed_fs.unwrap_or_default();
                let base = match fs_index {
                    0 => Some(BaseSamplingFrequency::Hz44100),
                    1 => Some(BaseSamplingFrequency::Hz48000),
                    _ => None,
                };
                let sample_rate = base.map_or_else(
                    || ReportedField::unknown_raw(fs_index, "undefined sampling-frequency index"),
                    |base| ReportedField::present_raw(base.hz(), Some("Hz"), fs_index),
                );
                let frame_rate_report = base
                    .and_then(|base| frame_rate(base, observed_rate_index.unwrap_or_default()))
                    .map(|rate| frame_rate_field(rate, observed_rate_index.unwrap_or_default()))
                    .unwrap_or_else(|| {
                        ReportedField::unknown_raw(
                            observed_rate_index.unwrap_or_default(),
                            "undefined frame-rate index",
                        )
                    });
                (sample_rate, frame_rate_report)
            } else {
                (
                    ReportedField::unknown("sampling frequency changes within the stream"),
                    ReportedField::unknown("frame rate changes within the stream"),
                )
            };

        if let Some(dsi) = dsi.as_ref() {
            if self.bitstream_versions.len() == 1
                && self.bitstream_versions.iter().next().copied()
                    != Some(u32::from(dsi.bitstream_version))
            {
                self.issues.push(InspectIssue::warning(
                    "dsi_toc_mismatch",
                    "dac4 bitstream_version differs from the observed TOC",
                    Some(0),
                ));
            }
            if sample_rate.status == FieldStatus::Present
                && sample_rate.value != Some(json!(dsi.sample_rate))
            {
                self.issues.push(InspectIssue::warning(
                    "dsi_toc_mismatch",
                    "dac4 sampling frequency differs from the observed TOC",
                    Some(0),
                ));
            }
            if frame_rate_report.status == FieldStatus::Present
                && frame_rate_report.value.as_ref().is_some_and(|value| {
                    value.get("numerator") != Some(&json!(dsi.frame_rate_numerator))
                        || value.get("denominator") != Some(&json!(dsi.frame_rate_denominator))
                })
            {
                self.issues.push(InspectIssue::warning(
                    "dsi_toc_mismatch",
                    "dac4 frame rate differs from the observed TOC",
                    Some(0),
                ));
            }
        }

        if self.canonical_fingerprint.is_none() {
            self.issues.push(InspectIssue::warning(
                "complete_configuration_unavailable",
                "No complete independent configuration was observed; topology fields are unavailable",
                None,
            ));
        }

        let i_frame = self
            .first_iframe
            .map(|value| ReportedField::present_raw(value, None, value))
            .unwrap_or_else(ReportedField::not_present);
        let i_frame_interval = iframe_interval_field(&self.iframe_intervals);
        let measurement_incomplete = self.frames_before_baseline != 0;
        let mut presentations = self
            .presentations
            .values()
            .map(|presentation| {
                build_presentation_report(
                    presentation,
                    duration,
                    self.canonical_changed,
                    measurement_incomplete,
                )
            })
            .collect::<Vec<_>>();
        presentations.sort_by_key(|presentation| presentation.index);
        let audio_substreams = self
            .substreams
            .values()
            .map(|substream| {
                build_substream_report(
                    substream,
                    duration,
                    self.canonical_changed,
                    measurement_incomplete,
                )
            })
            .collect::<Vec<_>>();
        let topology_count = |count: usize| {
            if self.canonical_fingerprint.is_none() {
                ReportedField::unknown("complete configuration is unavailable")
            } else if self.canonical_changed {
                ReportedField::unknown("topology changes after the first configuration")
            } else {
                ReportedField::present(u32::try_from(count).unwrap_or(u32::MAX))
            }
        };
        let number_of_presentations = topology_count(presentations.len());
        let number_of_audio_substreams = topology_count(audio_substreams.len());
        self.issues
            .extend(reserved_code_issues(&presentations, &audio_substreams));

        Ok(InspectReport {
            source: InspectSource {
                kind: source_kind,
                input: path.display().to_string(),
                track_index,
                frame_count: self.frame_count,
                duration: duration_field,
            },
            stream: InspectStream {
                codec: "Dolby AC-4".to_owned(),
                bit_rate,
                estimated_average_bit_rate,
                bitstream_version: observed_version,
                frame_rate: frame_rate_report,
                sample_rate,
                i_frame,
                i_frame_interval,
                sync_word,
                crc_errors,
                number_of_presentations,
                number_of_audio_substreams,
            },
            presentations,
            audio_substreams,
            issues: self.issues,
        })
    }

    fn apply_dsi(&mut self, dsi: &DsiSummary) {
        let mut toc_id_counts = BTreeMap::<u32, usize>::new();
        for id in self
            .presentations
            .values()
            .filter_map(|presentation| presentation.id)
        {
            let count = toc_id_counts.entry(id).or_default();
            *count = count.saturating_add(1);
        }
        let mut dsi_id_counts = BTreeMap::<u32, usize>::new();
        for id in dsi
            .presentations
            .iter()
            .filter_map(|presentation| presentation.effective_id)
        {
            let count = dsi_id_counts.entry(id).or_default();
            *count = count.saturating_add(1);
        }
        for (&id, &count) in &dsi_id_counts {
            if count > 1 {
                self.issues.push(
                    InspectIssue::warning(
                        "duplicate_dsi_presentation_id",
                        format!(
                            "dac4 effective presentation ID {id} occurs {count} times; DSI metadata is not associated by array position"
                        ),
                        None,
                    )
                    .presentation(Some(id)),
                );
            }
        }

        let mut unmatched = Vec::new();
        for presentation in &dsi.presentations {
            if let Some(indicators) = presentation.indicators {
                if indicators.reserved != 0 {
                    self.issues.push(
                        InspectIssue::warning(
                            "reserved_code",
                            format!(
                                "dac4 presentation {} has nonzero reserved indicator code {}",
                                presentation.index, indicators.reserved
                            ),
                            None,
                        )
                        .presentation(presentation.effective_id),
                    );
                }
                if indicators.reserved_id_bit == Some(true) {
                    self.issues.push(
                        InspectIssue::warning(
                            "reserved_code",
                            format!(
                                "dac4 presentation {} has a nonzero reserved presentation-ID bit",
                                presentation.index
                            ),
                            None,
                        )
                        .presentation(presentation.effective_id),
                    );
                }
            }
            let key = match presentation.effective_id {
                Some(id)
                    if dsi_id_counts.get(&id) == Some(&1) && toc_id_counts.get(&id) == Some(&1) =>
                {
                    Some(PresentationKey::Id(id)).filter(|key| self.presentations.contains_key(key))
                }
                Some(_) => None,
                None => (presentation.effective_id.is_none()
                    && dsi.presentations.len() == 1
                    && self.presentations.len() == 1
                    && self
                        .presentations
                        .values()
                        .next()
                        .is_some_and(|target| target.id.is_none()))
                .then(|| self.presentations.keys().next().copied())
                .flatten(),
            };
            let Some(key) = key else {
                unmatched.push((presentation.index, presentation.effective_id));
                continue;
            };
            let Some(target) = self.presentations.get_mut(&key) else {
                continue;
            };
            target.md_compat = target.md_compat.or(presentation.md_compat);
            target.multi_pid = target.multi_pid.or(presentation.multi_pid);
            target.alternative |= presentation.alternative;
            target.dsi_bitrate = presentation.bitrate;
            if target.channel_label.is_none() {
                target.channel_label = presentation
                    .channel_mode
                    .and_then(channel_mode_label)
                    .map(str::to_owned);
            }
            if presentation.languages.len() == 1 {
                target.language = presentation.languages.first().cloned();
            } else if presentation.languages.len() > 1 {
                target.language_conflict = true;
            }
            if target.presentation_config.is_none() && presentation.presentation_config != 31 {
                target.presentation_config = Some(u32::from(presentation.presentation_config));
            }
            if target.role == "Main"
                && presentation
                    .group_classifiers
                    .iter()
                    .flatten()
                    .any(|classifier| matches!(*classifier, 2 | 3 | 5))
            {
                target.role = "Associated";
            }
        }
        for (index, effective_id) in unmatched {
            let identity = effective_id.map_or_else(
                || "without an effective presentation ID".to_owned(),
                |id| format!("with effective presentation ID {id}"),
            );
            self.issues.push(InspectIssue::warning(
                "dsi_presentation_unmatched",
                format!("dac4 presentation {index} {identity} could not be associated uniquely"),
                None,
            ));
        }
    }
}

fn reserved_code_issues(
    presentations: &[InspectPresentation],
    audio_substreams: &[InspectAudioSubstream],
) -> Vec<InspectIssue> {
    let value = json!({
        "presentations": presentations,
        "audio_substreams": audio_substreams,
    });
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    collect_reserved_code_issues(&value, "$", &mut seen, &mut issues);
    issues
}

fn collect_reserved_code_issues(
    value: &Value,
    path: &str,
    seen: &mut BTreeSet<String>,
    issues: &mut Vec<InspectIssue>,
) {
    match value {
        Value::Object(object) => {
            let reason = object.get("reason").and_then(Value::as_str);
            let raw = object.get("raw_code");
            if object.get("status") == Some(&json!("unknown"))
                && reason.is_some_and(|reason| reason.contains("reserved"))
                && let (Some(reason), Some(raw)) = (reason, raw)
            {
                let signature = format!("{reason}:{raw}");
                if seen.insert(signature) {
                    issues.push(InspectIssue::warning(
                        "reserved_code",
                        format!("{path}: {reason}; raw_code={raw}"),
                        None,
                    ));
                }
            }
            for (key, child) in object {
                collect_reserved_code_issues(child, &format!("{path}.{key}"), seen, issues);
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                collect_reserved_code_issues(child, &format!("{path}[{index}]"), seen, issues);
            }
        }
        _ => {}
    }
}

fn presentation_key(topology: &Ac4Topology, index: usize, id: Option<u32>) -> PresentationKey {
    let Some(id) = id else {
        return PresentationKey::Anonymous(index);
    };
    let occurrences = topology
        .presentations()
        .iter()
        .filter(|presentation| presentation.presentation_id == Some(id))
        .take(2)
        .count();
    if occurrences == 1 {
        PresentationKey::Id(id)
    } else {
        PresentationKey::DuplicateId(id, index)
    }
}

fn frame_duration_ratio(fs_index: u8, frame_rate_index: u8) -> Option<DurationRatio> {
    let base = match fs_index {
        0 => BaseSamplingFrequency::Hz44100,
        1 => BaseSamplingFrequency::Hz48000,
        _ => return None,
    };
    let rate = frame_rate(base, frame_rate_index)?;
    Some(DurationRatio {
        numerator: u128::from(rate.denominator),
        denominator: u128::from(rate.numerator),
    })
}

fn presentation_metadata_error_is_reportable(error: PresentationSubstreamError) -> bool {
    matches!(
        error,
        PresentationSubstreamError::MissingAudioSubstreams
            | PresentationSubstreamError::MissingDrcConfiguration { .. }
            | PresentationSubstreamError::ReservedAssociatedPan { .. }
            | PresentationSubstreamError::UnusedCustomDownmixOutputChannelConfig { .. }
            | PresentationSubstreamError::ReservedStereoSurroundMixGain { .. }
            | PresentationSubstreamError::CapacityExceeded { .. }
    )
}

fn presentation_metadata_error_is_reserved(error: PresentationSubstreamError) -> bool {
    matches!(
        error,
        PresentationSubstreamError::ReservedAssociatedPan { .. }
            | PresentationSubstreamError::UnusedCustomDownmixOutputChannelConfig { .. }
            | PresentationSubstreamError::ReservedStereoSurroundMixGain { .. }
    )
}

fn presentation_metadata_failure(error: PresentationSubstreamError) -> MetadataFailure {
    if matches!(error, PresentationSubstreamError::CapacityExceeded { .. }) {
        MetadataFailure::Unsupported
    } else {
        MetadataFailure::Unknown
    }
}

fn record_metadata_failure(slot: &mut Option<MetadataFailure>, failure: MetadataFailure) {
    if slot.is_none() || failure == MetadataFailure::Unknown {
        *slot = Some(failure);
    }
}

fn audio_metadata_error_is_reportable(error: AudioSubstreamError) -> bool {
    matches!(error, AudioSubstreamError::Unsupported { .. })
}

fn describe_configuration_change(
    canonical: ConfigFingerprint,
    observed: ConfigFingerprint,
) -> String {
    let mut changes = Vec::new();
    if canonical.bitstream_version != observed.bitstream_version {
        changes.push("bitstream_version");
    }
    if canonical.fs_index != observed.fs_index {
        changes.push("sampling_frequency");
    }
    if canonical.frame_rate_index != observed.frame_rate_index {
        changes.push("frame_rate");
    }
    if canonical.n_presentations != observed.n_presentations {
        changes.push("presentations");
    }
    if canonical.n_groups != observed.n_groups {
        changes.push("substream_groups");
    }
    if canonical.scene_path != observed.scene_path {
        changes.push("scene_path");
    }
    if canonical.total_objects != observed.total_objects {
        changes.push("objects");
    }
    if canonical.n_substreams != observed.n_substreams {
        changes.push("substreams");
    }
    let detail = if changes.is_empty() {
        "presentation or group configuration".to_owned()
    } else {
        changes.join(", ")
    };
    format!("The normalized AC-4 topology differs from the report baseline: {detail}")
}

fn record_presentation_metadata_conflicts(
    conflicts: &mut PresentationMetadataConflicts,
    previous: &PresentationMetadataOwned,
    current: &PresentationMetadataOwned,
) -> Option<String> {
    let mut changes = Vec::new();
    if previous.dialnorm_bits != current.dialnorm_bits && !conflicts.dialnorm {
        conflicts.dialnorm = true;
        changes.push(format!(
            "dialnorm {} -> {}",
            previous.dialnorm_bits, current.dialnorm_bits
        ));
    }
    if previous.further_loudness != current.further_loudness && !conflicts.further_loudness {
        conflicts.further_loudness = true;
        changes.push("further loudness".to_owned());
    }
    if previous.drc_configuration != current.drc_configuration && !conflicts.drc_configuration {
        conflicts.drc_configuration = true;
        changes.push("DRC configuration".to_owned());
    }
    if previous.associated_scale != current.associated_scale && !conflicts.associated_scale {
        conflicts.associated_scale = true;
        changes.push("associated-audio scale".to_owned());
    }
    if previous.stereo_downmix != current.stereo_downmix && !conflicts.stereo_downmix {
        conflicts.stereo_downmix = true;
        changes.push("stereo downmix".to_owned());
    }
    (!changes.is_empty()).then(|| {
        format!(
            "Stable presentation metadata changes within the configuration: {}",
            changes.join(", ")
        )
    })
}

fn group_role(
    config: Option<u32>,
    single: bool,
    position: usize,
    classifier: Option<u8>,
) -> &'static str {
    if single {
        return "Main";
    }
    match (config, position) {
        (Some(0), 0) => "Music and effects",
        (Some(0), 1) => "Dialogue",
        (Some(1), 0) | (Some(2), 0) | (Some(4), 0) => "Main",
        (Some(1), 1) | (Some(4), 1) => "Dialogue enhancement",
        (Some(2), 1) | (Some(3), 2) | (Some(4), 2) => "Associated",
        (Some(3), 0) => "Music and effects",
        (Some(3), 1) => "Dialogue",
        (Some(5), _) => match classifier {
            Some(2 | 3 | 5) => "Associated",
            Some(4) => "Dialogue",
            Some(1) => "Music and effects",
            _ => "Main",
        },
        (Some(6), _) => "Data",
        _ => "Main",
    }
}

fn audio_contexts(topology: &Ac4Topology) -> BTreeMap<u32, Vec<SubstreamContext>> {
    let mut contexts = BTreeMap::<u32, Vec<SubstreamContext>>::new();
    for (group_index, group) in topology.groups().iter().enumerate() {
        let alternative = topology.presentations().iter().any(|presentation| {
            presentation
                .substream
                .is_some_and(|substream| substream.alternative)
                && presentation
                    .group_indices()
                    .iter()
                    .any(|&index| usize::try_from(index) == Ok(group_index))
        });
        let ims = topology.presentations().iter().any(|presentation| {
            presentation.presentation_version == 2
                && presentation
                    .group_indices()
                    .iter()
                    .any(|&index| usize::try_from(index) == Ok(group_index))
        });
        for info in group.substreams() {
            let Some(first) = info.substream_index() else {
                continue;
            };
            let (ajoc, channel_mode) = match *info {
                SubstreamInfo::Chan(ref channel) => (false, Some(channel.channel_mode.ch_mode)),
                SubstreamInfo::Ajoc(_) => (true, None),
                SubstreamInfo::Obj(_) => (false, None),
            };
            for offset in 0..group.frame_rate_factor {
                let Some(index) = first.checked_add(offset) else {
                    continue;
                };
                let b_iframe = if group.frame_rate_factor == 1 || info.audio_ndot() {
                    Some(info.audio_ndot())
                } else {
                    None
                };
                let candidate = SubstreamContext {
                    sus_ver: 1,
                    alternative,
                    ajoc,
                    channel_mode,
                    b_iframe,
                    alternative_oamd: None,
                };
                let slot = contexts.entry(index).or_default();
                if !slot.contains(&candidate) {
                    slot.push(candidate);
                }
                if ims && matches!(channel_mode, Some(5 | 6)) {
                    let stereo = SubstreamContext {
                        channel_mode: Some(1),
                        ..candidate
                    };
                    if !slot.contains(&stereo) {
                        slot.push(stereo);
                    }
                }
            }
        }
    }
    contexts
}

fn same_audio_context_family(left: SubstreamContext, right: SubstreamContext) -> bool {
    left.sus_ver == right.sus_ver
        && left.alternative == right.alternative
        && left.ajoc == right.ajoc
        && left.channel_mode == right.channel_mode
        && left.alternative_oamd == right.alternative_oamd
}

fn same_channel_layout_info(left: SubstreamInfoChan, right: SubstreamInfoChan) -> bool {
    left.channel_mode == right.channel_mode
        && left.four_back_channels_present == right.four_back_channels_present
        && left.centre_present == right.centre_present
        && left.top_channels_present == right.top_channels_present
}

fn build_presentation_report(
    presentation: &PresentationAccumulator,
    duration: Option<DurationRatio>,
    configuration_changed: bool,
    measurement_incomplete: bool,
) -> InspectPresentation {
    const TOPOLOGY_CHANGED: &str = "topology changes after the first configuration";

    let presentation_id = if configuration_changed {
        ReportedField::unknown(TOPOLOGY_CHANGED)
    } else {
        presentation
            .id
            .map(|value| ReportedField::present_raw(value, None, value))
            .unwrap_or_else(ReportedField::not_present)
    };
    let config_label = presentation
        .presentation_config
        .map_or("single_group", presentation_config_label);
    let layout = match presentation.scene_path {
        "IMS" => "Stereo / IMS",
        "Channel-Based" => presentation
            .channel_label
            .as_deref()
            .unwrap_or(presentation.scene_path),
        _ => presentation.scene_path,
    };
    let summary_value = format!(
        "{layout} {} ({config_label})",
        presentation.role.to_lowercase()
    );
    let summary = if configuration_changed {
        ReportedField::unknown(TOPOLOGY_CHANGED)
    } else if let Some(raw) = presentation.presentation_config {
        ReportedField::present_raw(summary_value, None, raw)
    } else {
        ReportedField::present(summary_value)
    };
    let presentation_type = if configuration_changed {
        ReportedField::unknown(TOPOLOGY_CHANGED)
    } else {
        ReportedField::present(presentation.role)
    };
    let minimal_compatibility_level = if configuration_changed {
        ReportedField::unknown(TOPOLOGY_CHANGED)
    } else {
        presentation
            .md_compat
            .map(|value| ReportedField::present_raw(value, None, value))
            .unwrap_or_else(ReportedField::not_present)
    };
    let language = if configuration_changed {
        ReportedField::unknown(TOPOLOGY_CHANGED)
    } else if presentation.language_conflict {
        ReportedField::unknown("multiple language tags contribute to the presentation")
    } else {
        presentation
            .language
            .as_ref()
            .map_or_else(
                ReportedField::not_present,
                |bytes| match std::str::from_utf8(bytes) {
                    Ok(language) => {
                        ReportedField::present_raw(language.to_owned(), None, json!(bytes))
                    }
                    Err(_) => {
                        ReportedField::unknown_raw(json!(bytes), "language tag is not valid UTF-8")
                    }
                },
            )
    };
    let multi_pid = if configuration_changed {
        ReportedField::unknown(TOPOLOGY_CHANGED)
    } else {
        presentation
            .multi_pid
            .map(|value| ReportedField::present_raw(value, None, value))
            .unwrap_or_else(ReportedField::not_present)
    };
    let measured_bitrate = if configuration_changed {
        ReportedField::unknown("topology changes prevent a stable presentation bitrate")
    } else if measurement_incomplete {
        ReportedField::unknown("frames before the report baseline were not attributable")
    } else {
        duration
            .and_then(|duration| duration.bit_rate(presentation.measured_audio_bytes))
            .map(bit_rate_field)
            .unwrap_or_else(|| ReportedField::unknown("duration is unavailable"))
    };
    let bit_rate = if let Some(declared) = presentation.dsi_bitrate {
        let mut field = measured_bitrate;
        field.raw_code = Some(json!({
            "mode": declared.mode.code(),
            "bit_rate_bps": declared.bit_rate,
            "precision_bps": if declared.precision_unknown() {
                Value::Null
            } else {
                json!(declared.precision)
            },
        }));
        field
    } else {
        measured_bitrate
    };
    let audio_substreams = if configuration_changed {
        ReportedField::unknown(TOPOLOGY_CHANGED)
    } else {
        ReportedField::present(json!(
            presentation
                .audio_substreams
                .iter()
                .copied()
                .collect::<Vec<_>>()
        ))
    };

    let (mut dialogue_normalization, mut loudness, mut drc, mut mixing, mut downmix) =
        presentation.parsed_metadata.as_ref().map_or_else(
            || {
                (
                    ReportedField::not_present(),
                    InspectLoudness::default(),
                    InspectDrc::default(),
                    InspectMixing::default(),
                    InspectDownmix::default(),
                )
            },
            |metadata| {
                let dialnorm = ReportedField::present_raw(
                    -f64::from(metadata.dialnorm_bits) / 4.0,
                    Some("dBFS"),
                    metadata.dialnorm_bits,
                );
                (
                    dialnorm,
                    loudness_report(metadata.dialnorm_bits, metadata.further_loudness),
                    drc_report(metadata.drc_configuration),
                    mixing_report(metadata.associated_scale),
                    downmix_report(metadata.stereo_downmix),
                )
            },
        );
    let conflicts = presentation.metadata_conflicts;
    if conflicts.dialnorm {
        dialogue_normalization = metadata_changed_field();
        if loudness.integrated_level_gated.status != FieldStatus::Present {
            loudness.loudness = metadata_changed_field();
        }
    }
    if conflicts.further_loudness {
        loudness = conflicting_loudness_report();
    }
    if conflicts.drc_configuration {
        drc = conflicting_drc_report();
    }
    if conflicts.associated_scale {
        mixing = conflicting_mixing_report();
    }
    if conflicts.stereo_downmix {
        downmix = conflicting_downmix_report();
    }
    let metadata_unavailable = if configuration_changed {
        Some((MetadataFailure::Unknown, TOPOLOGY_CHANGED))
    } else {
        presentation.metadata_failure.map(|failure| {
            (
                failure,
                "presentation metadata could not be interpreted completely",
            )
        })
    };
    if let Some((failure, reason)) = metadata_unavailable {
        dialogue_normalization = unavailable_field(failure, reason);
        loudness = unavailable_loudness_report(failure, reason);
        drc = unavailable_drc_report(failure, reason);
        mixing = unavailable_mixing_report(failure, reason);
        downmix = unavailable_downmix_report(failure, reason);
    }

    InspectPresentation {
        index: presentation.index,
        presentation_id,
        summary,
        presentation_type,
        minimal_compatibility_level,
        dialogue_normalization,
        language,
        multi_pid,
        bit_rate,
        audio_substreams,
        metadata_authentication_id: ReportedField::unsupported(
            "no confirmed ETSI field maps to the proprietary label",
        ),
        loudness,
        dynamic_range_control: drc,
        mixing_metadata: mixing,
        downmix,
    }
}

fn metadata_changed_field() -> ReportedField {
    ReportedField::unknown("value changes within the configuration")
}

fn unavailable_field(failure: MetadataFailure, reason: &str) -> ReportedField {
    match failure {
        MetadataFailure::Unknown => ReportedField::unknown(reason),
        MetadataFailure::Unsupported => ReportedField::unsupported(reason),
    }
}

fn unavailable_loudness_report(failure: MetadataFailure, reason: &str) -> InspectLoudness {
    let field = || unavailable_field(failure, reason);
    InspectLoudness {
        loudness: field(),
        version: field(),
        regulation_type: field(),
        correction_type: field(),
        dialogue_intelligence: field(),
        integrated_speech_gated: field(),
        integrated_level_gated: field(),
        maximum_true_peak: field(),
        maximum_momentary_loudness: field(),
        loudness_range: field(),
    }
}

fn unavailable_drc_report(failure: MetadataFailure, reason: &str) -> InspectDrc {
    let field = || unavailable_field(failure, reason);
    InspectDrc {
        enhanced_ac3_profile: field(),
        home_theater_avr: field(),
        flat_panel_tv: field(),
        portable_speakers: field(),
        portable_headphones: field(),
    }
}

fn unavailable_mixing_report(failure: MetadataFailure, reason: &str) -> InspectMixing {
    let field = || unavailable_field(failure, reason);
    InspectMixing {
        main_audio_ducking_level: field(),
        main_audio_ducking_level_center: field(),
        main_audio_ducking_level_front: field(),
    }
}

fn unavailable_downmix_report(failure: MetadataFailure, reason: &str) -> InspectDownmix {
    let field = || unavailable_field(failure, reason);
    InspectDownmix {
        loro_center_mix_gain: field(),
        loro_surround_mix_gain: field(),
        ltrt_center_mix_gain: field(),
        ltrt_surround_mix_gain: field(),
        lfe_mix_info: field(),
        lfe_mix_gain: field(),
        preferred_downmix: field(),
    }
}

fn conflicting_loudness_report() -> InspectLoudness {
    unavailable_loudness_report(
        MetadataFailure::Unknown,
        "value changes within the configuration",
    )
}

fn conflicting_drc_report() -> InspectDrc {
    unavailable_drc_report(
        MetadataFailure::Unknown,
        "value changes within the configuration",
    )
}

fn conflicting_mixing_report() -> InspectMixing {
    unavailable_mixing_report(
        MetadataFailure::Unknown,
        "value changes within the configuration",
    )
}

fn conflicting_downmix_report() -> InspectDownmix {
    unavailable_downmix_report(
        MetadataFailure::Unknown,
        "value changes within the configuration",
    )
}

fn loudness_report(dialnorm_bits: u8, further: Option<FurtherLoudnessInfo>) -> InspectLoudness {
    let mut report = InspectLoudness::default();
    let dialnorm = -f64::from(dialnorm_bits) / 4.0;
    report.loudness = ReportedField::present_raw(dialnorm, Some("LKFS"), dialnorm_bits);
    let Some(further) = further else {
        return report;
    };

    report.version = match further.effective_loudness_version() {
        Some(version) => ReportedField::present_raw(
            version,
            None,
            json!({
                "loudness_version": further.loudness_version,
                "extended_loudness_version": further.extended_loudness_version,
            }),
        ),
        None => ReportedField::unknown("loudness version extension is incomplete"),
    };
    report.regulation_type = further
        .loud_prac_type
        .map_or_else(ReportedField::not_present, loudness_practice_field);
    report.correction_type =
        further
            .loudcorr_type
            .map_or_else(ReportedField::not_present, |realtime| {
                ReportedField::present_raw(
                    if realtime {
                        "Real-time loudness measurement"
                    } else {
                        "File-based lookahead"
                    },
                    None,
                    realtime,
                )
            });
    report.dialogue_intelligence = further
        .loudcorr_dialgate
        .map_or_else(ReportedField::not_present, |value| {
            ReportedField::present_raw(value, None, value)
        });
    report.integrated_speech_gated =
        further
            .loudspchgat
            .map_or_else(ReportedField::not_present, |(raw, practice)| {
                ReportedField::present_raw(
                    loudness_code(raw),
                    Some("LUFS"),
                    json!({"value": raw, "dialgate_practice": practice}),
                )
            });
    report.integrated_level_gated = further
        .loudrelgat
        .map_or_else(ReportedField::not_present, |raw| {
            ReportedField::present_raw(loudness_code(raw), Some("LKFS"), raw)
        });
    if further.loudrelgat.is_some() {
        report.loudness = report.integrated_level_gated.clone();
    }
    report.maximum_true_peak = further
        .max_truepk
        .map_or_else(ReportedField::not_present, |raw| {
            ReportedField::present_raw(loudness_code(raw), Some("dBTP"), raw)
        });
    report.maximum_momentary_loudness = further
        .max_loudmntry
        .map_or_else(ReportedField::not_present, |raw| {
            ReportedField::present_raw(loudness_code(raw), Some("LUFS"), raw)
        });
    report.loudness_range =
        further
            .lra
            .map_or_else(ReportedField::not_present, |(raw, practice)| {
                let practice_name = match practice {
                    0 => "EBU Tech 3342 v1",
                    1 => "EBU Tech 3342 v2",
                    _ => "Reserved",
                };
                if practice <= 1 {
                    ReportedField::present_raw(
                        f64::from(raw) / 10.0,
                        Some("LU"),
                        json!({"value": raw, "practice": practice, "practice_name": practice_name}),
                    )
                } else {
                    ReportedField::unknown_raw(
                        json!({"value": raw, "practice": practice}),
                        "reserved loudness-range practice code",
                    )
                }
            });
    report
}

fn loudness_practice_field(raw: u8) -> ReportedField {
    let name = match raw {
        0 => "Not indicated",
        1 => "ATSC A/85",
        2 => "EBU R128",
        3 => "ARIB TR-B32",
        4 => "Free TV Australia OP-59",
        14 => "Manual",
        15 => "Consumer leveller",
        _ => {
            return ReportedField::unknown_raw(raw, "reserved loudness practice code");
        }
    };
    ReportedField::present_raw(name, None, raw)
}

fn drc_report(configuration: Option<PresentationDrcConfiguration>) -> InspectDrc {
    let Some(configuration) = configuration else {
        return InspectDrc::default();
    };
    InspectDrc {
        enhanced_ac3_profile: drc_profile_field(configuration.eac3_profile),
        home_theater_avr: drc_decoder_mode_field(&configuration, 0),
        flat_panel_tv: drc_decoder_mode_field(&configuration, 1),
        portable_speakers: drc_decoder_mode_field(&configuration, 2),
        portable_headphones: drc_decoder_mode_field(&configuration, 3),
    }
}

fn drc_decoder_mode_field(
    configuration: &PresentationDrcConfiguration,
    mode_id: u8,
) -> ReportedField {
    let mut modes = configuration
        .decoder_modes()
        .iter()
        .filter(|mode| mode.mode_id == mode_id);
    let Some(mode) = modes.next().copied() else {
        return ReportedField::not_present();
    };
    if modes.next().is_some() {
        return ReportedField::unknown_raw(
            json!({"mode_id": mode_id}),
            "decoder mode ID is declared more than once",
        );
    }
    drc_decoder_mode_profile_field(mode, configuration.eac3_profile)
}

fn drc_decoder_mode_profile_field(
    mode: PresentationDrcDecoderMode,
    eac3_profile: u8,
) -> ReportedField {
    match mode.profile {
        PresentationDrcProfile::DefaultEac3 => {
            let mut field = drc_profile_field(eac3_profile);
            field.raw_code = Some(json!({
                "mode_id": mode.mode_id,
                "profile": "default_eac3",
                "eac3_profile": eac3_profile,
            }));
            field
        }
        PresentationDrcProfile::Repeat { repeat_id } => ReportedField::present_raw(
            format!("Repeat decoder mode {repeat_id}"),
            None,
            json!({
                "mode_id": mode.mode_id,
                "profile": "repeat",
                "repeat_id": repeat_id,
            }),
        ),
        PresentationDrcProfile::CompressionCurve(_) => ReportedField::present_raw(
            "Custom compression curve",
            None,
            json!({"mode_id": mode.mode_id, "profile": "compression_curve"}),
        ),
        PresentationDrcProfile::Gains { configuration } => ReportedField::present_raw(
            "Per-frame DRC gains",
            None,
            json!({
                "mode_id": mode.mode_id,
                "profile": "gains",
                "gains_configuration": configuration,
            }),
        ),
    }
}

fn drc_profile_field(raw: u8) -> ReportedField {
    let name = match raw {
        0 => "None",
        1 => "Film standard",
        2 => "Film light",
        3 => "Music standard",
        4 => "Music light",
        5 => "Speech",
        _ => return ReportedField::unknown_raw(raw, "reserved (E-)AC-3 DRC profile"),
    };
    ReportedField::present_raw(name, None, raw)
}

fn mixing_report(scales: Option<(Option<u8>, Option<u8>, Option<u8>)>) -> InspectMixing {
    let Some((main, center, front)) = scales else {
        return InspectMixing::default();
    };
    InspectMixing {
        main_audio_ducking_level: scale_field(main),
        main_audio_ducking_level_center: scale_field(center),
        main_audio_ducking_level_front: scale_field(front),
    }
}

fn scale_field(raw: Option<u8>) -> ReportedField {
    raw.map_or_else(ReportedField::not_present, |raw| {
        if raw == u8::MAX {
            ReportedField::present_raw("negative_infinity", Some("dB"), raw)
        } else {
            ReportedField::present_raw(-0.3 * f64::from(raw), Some("dB"), raw)
        }
    })
}

fn downmix_report(
    coefficients: Option<macindecode_ac4_bitstream::PresentationStereoDownmixCoefficients>,
) -> InspectDownmix {
    let Some(coefficients) = coefficients else {
        return InspectDownmix::default();
    };
    let ltrt_center = coefficients
        .ltrt_centre_mixgain
        .unwrap_or(coefficients.loro_centre_mixgain);
    let ltrt_surround = coefficients
        .ltrt_surround_mixgain
        .unwrap_or(coefficients.loro_surround_mixgain);
    InspectDownmix {
        loro_center_mix_gain: center_mix_gain_field(coefficients.loro_centre_mixgain),
        loro_surround_mix_gain: surround_mix_gain_field(coefficients.loro_surround_mixgain),
        ltrt_center_mix_gain: center_mix_gain_field(ltrt_center),
        ltrt_surround_mix_gain: surround_mix_gain_field(ltrt_surround),
        lfe_mix_info: coefficients
            .lfe_mixinfo_present
            .map_or_else(ReportedField::not_applicable, |present| {
                ReportedField::present_raw(present, None, present)
            }),
        lfe_mix_gain: coefficients
            .lfe_mixgain
            .map_or_else(ReportedField::not_present, |raw| {
                ReportedField::present_raw(5.5 - f64::from(raw), Some("dB"), raw)
            }),
        preferred_downmix: preferred_downmix_field(coefficients.preferred_downmix_method),
    }
}

fn center_mix_gain_field(raw: u8) -> ReportedField {
    if raw == 7 {
        ReportedField::present_raw("negative_infinity", Some("dB"), raw)
    } else {
        ReportedField::present_raw(3.0 - 1.5 * f64::from(raw), Some("dB"), raw)
    }
}

fn surround_mix_gain_field(raw: u8) -> ReportedField {
    match raw {
        0 | 1 => ReportedField::unknown_raw(raw, "reserved surround mix-gain code"),
        7 => ReportedField::present_raw("negative_infinity", Some("dB"), raw),
        _ => ReportedField::present_raw(3.0 - 1.5 * f64::from(raw), Some("dB"), raw),
    }
}

fn preferred_downmix_field(raw: u8) -> ReportedField {
    let value = match raw {
        0 => "Not indicated",
        1 => "Lo/Ro",
        2 => "Lt/Rt",
        3 => "Lt/Rt with phase scaling",
        _ => return ReportedField::unknown_raw(raw, "invalid preferred downmix code"),
    };
    ReportedField::present_raw(value, None, raw)
}

fn build_substream_report(
    substream: &SubstreamAccumulator,
    duration: Option<DurationRatio>,
    configuration_changed: bool,
    measurement_incomplete: bool,
) -> InspectAudioSubstream {
    let (channel_configuration, channel_layout) = if configuration_changed {
        let field = ReportedField::unknown("topology changes after the first configuration");
        (field.clone(), field)
    } else if substream.object_coded {
        let field = ReportedField::present("Object-Based");
        (field.clone(), field)
    } else if substream.ims
        && substream
            .channel_info
            .is_some_and(|info| info.channel_mode.ch_mode == 1)
    {
        let field = ReportedField::present_raw("Stereo / IMS", None, 1);
        (field.clone(), field)
    } else {
        substream.channel_info.map_or_else(
            || {
                let field = ReportedField::unknown("channel mode is unavailable");
                (field.clone(), field)
            },
            |info| {
                let mode = info.channel_mode;
                let configuration = mode.label().map_or_else(
                    || ReportedField::unknown_raw(mode.ch_mode, "reserved channel mode"),
                    |label| ReportedField::present_raw(label, None, mode.ch_mode),
                );
                let layout = effective_channel_layout_label(info).map_or_else(
                    || {
                        ReportedField::unknown_raw(
                            json!({
                                "channel_mode": mode.ch_mode,
                                "four_back_channels_present": info.four_back_channels_present,
                                "centre_present": info.centre_present,
                                "top_channels_present": info.top_channels_present,
                            }),
                            "effective channel layout is unavailable",
                        )
                    },
                    |label| {
                        ReportedField::present_raw(
                            label,
                            None,
                            json!({
                                "channel_mode": mode.ch_mode,
                                "four_back_channels_present": info.four_back_channels_present,
                                "centre_present": info.centre_present,
                                "top_channels_present": info.top_channels_present,
                            }),
                        )
                    },
                );
                (configuration, layout)
            },
        )
    };
    let bit_rate = if configuration_changed {
        ReportedField::unknown("topology changes prevent a stable substream bitrate")
    } else if measurement_incomplete {
        ReportedField::unknown("frames before the report baseline were not attributable")
    } else {
        duration
            .and_then(|duration| duration.bit_rate(substream.measured_bytes))
            .map(bit_rate_field)
            .unwrap_or_else(|| ReportedField::unknown("duration is unavailable"))
    };
    let preprocessing = if configuration_changed {
        unknown_preprocessing("topology changes after the first configuration")
    } else if substream.preprocessing_conflict {
        unknown_preprocessing("value changes within the configuration")
    } else if let Some(failure) = substream.metadata_failure {
        unavailable_preprocessing(
            failure,
            "audio metadata could not be interpreted completely",
        )
    } else {
        substream
            .preprocessing
            .map_or_else(InspectPreprocessing::default, preprocessing_report)
    };
    let dialogue_enhancement = if configuration_changed {
        unknown_dialogue_enhancement("topology changes after the first configuration")
    } else if substream.de_configuration_conflict {
        unknown_dialogue_enhancement("value changes within the configuration")
    } else if let Some(failure) = substream.metadata_failure {
        unavailable_dialogue_enhancement(
            failure,
            "audio metadata could not be interpreted completely",
        )
    } else {
        dialogue_enhancement_report(substream.de_seen, substream.reported_de_configuration)
    };

    InspectAudioSubstream {
        index: substream.index,
        summary: if configuration_changed {
            ReportedField::unknown("topology changes after the first configuration")
        } else {
            ReportedField::present(substream.role)
        },
        channel_configuration,
        channel_layout,
        object_coded: if configuration_changed {
            ReportedField::unknown("topology changes after the first configuration")
        } else {
            ReportedField::present_raw(substream.object_coded, None, substream.object_coded)
        },
        bit_rate,
        preprocessing,
        dialogue_enhancement,
    }
}

fn unknown_preprocessing(reason: &str) -> InspectPreprocessing {
    unavailable_preprocessing(MetadataFailure::Unknown, reason)
}

fn unavailable_preprocessing(failure: MetadataFailure, reason: &str) -> InspectPreprocessing {
    let field = || unavailable_field(failure, reason);
    InspectPreprocessing {
        previous_mix_type_2channel: field(),
        phase90_filter_info_2channel: field(),
        loro_center_mix_gain: field(),
        loro_surround_mix_gain: field(),
        loro_downmix_loudness_correction: field(),
        ltrt_center_mix_gain: field(),
        ltrt_surround_mix_gain: field(),
        ltrt_downmix_loudness_correction: field(),
        lfe_mix_gain: field(),
        preferred_downmix: field(),
        previous_downmix_type_5channel: field(),
        previous_upmix_type_5channel: field(),
        previous_upmix_type_3_4: field(),
        previous_upmix_type_3_2_2: field(),
        phase90_filter_info: field(),
        surround_attenuation_known: field(),
        lfe_attenuation_known: field(),
    }
}

fn unknown_dialogue_enhancement(reason: &str) -> InspectDialogueEnhancement {
    unavailable_dialogue_enhancement(MetadataFailure::Unknown, reason)
}

fn unavailable_dialogue_enhancement(
    failure: MetadataFailure,
    reason: &str,
) -> InspectDialogueEnhancement {
    let field = || unavailable_field(failure, reason);
    InspectDialogueEnhancement {
        enabled: field(),
        method: field(),
        max_gain: field(),
        channel_configuration: field(),
    }
}

fn preprocessing_report(metadata: PreprocessingMetadata) -> InspectPreprocessing {
    let stereo = metadata.stereo_downmix;
    InspectPreprocessing {
        previous_mix_type_2channel: metadata
            .previous_downmix_type_2ch
            .map_or_else(ReportedField::not_present, previous_downmix_2ch_field),
        phase90_filter_info_2channel: metadata
            .phase90_info_2ch
            .map_or_else(ReportedField::not_present, phase90_2ch_field),
        loro_center_mix_gain: stereo.map_or_else(ReportedField::not_present, |stereo| {
            center_mix_gain_field(stereo.loro_centre_mixgain)
        }),
        loro_surround_mix_gain: stereo.map_or_else(ReportedField::not_present, |stereo| {
            surround_mix_gain_field(stereo.loro_surround_mixgain)
        }),
        loro_downmix_loudness_correction: stereo
            .and_then(|stereo| stereo.loro_dmx_loud_corr)
            .map_or_else(ReportedField::not_present, raw_value_field),
        ltrt_center_mix_gain: stereo
            .and_then(|stereo| stereo.ltrt_centre_mixgain)
            .map_or_else(ReportedField::not_present, center_mix_gain_field),
        ltrt_surround_mix_gain: stereo
            .and_then(|stereo| stereo.ltrt_surround_mixgain)
            .map_or_else(ReportedField::not_present, surround_mix_gain_field),
        ltrt_downmix_loudness_correction: stereo
            .and_then(|stereo| stereo.ltrt_dmx_loud_corr)
            .map_or_else(ReportedField::not_present, raw_value_field),
        lfe_mix_gain: stereo
            .and_then(|stereo| stereo.lfe_mixgain)
            .map_or_else(ReportedField::not_present, |raw| {
                ReportedField::present_raw(5.5 - f64::from(raw), Some("dB"), raw)
            }),
        preferred_downmix: stereo.map_or_else(ReportedField::not_present, |stereo| {
            preferred_downmix_field(stereo.preferred_dmx_method)
        }),
        previous_downmix_type_5channel: metadata
            .previous_downmix_type_5ch
            .map_or_else(ReportedField::not_present, previous_downmix_5ch_field),
        previous_upmix_type_5channel: metadata
            .previous_upmix_type_5ch
            .map_or_else(ReportedField::not_present, previous_upmix_5ch_field),
        previous_upmix_type_3_4: metadata
            .previous_upmix_type_3_4
            .map_or_else(ReportedField::not_present, previous_upmix_3_4_field),
        previous_upmix_type_3_2_2: metadata.previous_upmix_type_3_2_2.map_or_else(
            ReportedField::not_present,
            |raw| {
                if raw {
                    ReportedField::unknown_raw(raw, "reserved 3/2/2 upmix code")
                } else {
                    ReportedField::present_raw("Dolby Pro Logic IIz Height", None, raw)
                }
            },
        ),
        phase90_filter_info: metadata
            .phase90_info_multichannel
            .map_or_else(ReportedField::not_present, phase90_multichannel_field),
        surround_attenuation_known: metadata
            .surround_attenuation_known
            .map_or_else(ReportedField::not_present, |raw| {
                ReportedField::present_raw(raw, None, raw)
            }),
        lfe_attenuation_known: metadata
            .lfe_attenuation_known
            .map_or_else(ReportedField::not_present, |raw| {
                ReportedField::present_raw(raw, None, raw)
            }),
    }
}

fn raw_value_field(raw: u8) -> ReportedField {
    ReportedField::present_raw(raw, None, raw)
}

fn previous_downmix_2ch_field(raw: u8) -> ReportedField {
    let value = match raw {
        0 => "Unknown",
        1 => "Lo/Ro",
        2 => "Lt/Rt",
        3 => "Lt/Rt asymmetric surround",
        _ => return ReportedField::unknown_raw(raw, "reserved 2-channel downmix code"),
    };
    ReportedField::present_raw(value, None, raw)
}

fn phase90_2ch_field(raw: u8) -> ReportedField {
    let value = match raw {
        0 => "Not indicated",
        1 => return ReportedField::unknown_raw(raw, "reserved 2-channel phase-90 code"),
        2 => "Surrounds phase-shifted before downmix",
        3 => "Surrounds not phase-shifted before downmix",
        _ => return ReportedField::unknown_raw(raw, "invalid 2-channel phase-90 code"),
    };
    ReportedField::present_raw(value, None, raw)
}

fn previous_downmix_5ch_field(raw: u8) -> ReportedField {
    let value = match raw {
        0 => "Five-channel identity with center-surround fold",
        1 => "Back-surround fold at -3 dB",
        2 => "Asymmetric back-surround fold",
        3 => "Asymmetric vertical-height fold",
        _ => return ReportedField::unknown_raw(raw, "reserved 5-channel downmix code"),
    };
    ReportedField::present_raw(value, None, raw)
}

fn previous_upmix_5ch_field(raw: u8) -> ReportedField {
    let value = match raw {
        0 => "Dolby Pro Logic",
        1 => "Dolby Pro Logic II Movie",
        2 => "Dolby Pro Logic II Music",
        3 => "Dolby Professional Upmixer",
        _ => return ReportedField::unknown_raw(raw, "reserved 5-channel upmix code"),
    };
    ReportedField::present_raw(value, None, raw)
}

fn previous_upmix_3_4_field(raw: u8) -> ReportedField {
    let value = match raw {
        0 => "Dolby Pro Logic IIx Movie",
        1 => "Dolby Pro Logic IIx Music",
        _ => return ReportedField::unknown_raw(raw, "reserved 3/4 upmix code"),
    };
    ReportedField::present_raw(value, None, raw)
}

fn phase90_multichannel_field(raw: u8) -> ReportedField {
    let value = match raw {
        0 => "Not indicated",
        1 => "Surrounds phase-shifted before encoding",
        2 => "Surrounds not phase-shifted before encoding",
        3 => return ReportedField::unknown_raw(raw, "reserved multichannel phase-90 code"),
        _ => return ReportedField::unknown_raw(raw, "invalid multichannel phase-90 code"),
    };
    ReportedField::present_raw(value, None, raw)
}

fn dialogue_enhancement_report(
    seen: bool,
    configuration: Option<DialogEnhancementConfiguration>,
) -> InspectDialogueEnhancement {
    let Some(configuration) = configuration else {
        return InspectDialogueEnhancement {
            enabled: ReportedField::present_raw(seen, None, seen),
            ..InspectDialogueEnhancement::default()
        };
    };
    let method = match configuration.method {
        0 => "Channel-independent",
        1 => "Cross-channel",
        2 => "Waveform-parametric hybrid, channel-independent",
        3 => "Waveform-parametric hybrid, cross-channel",
        _ => "Unknown",
    };
    InspectDialogueEnhancement {
        enabled: ReportedField::present_raw(seen, None, seen),
        method: ReportedField::present_raw(method, None, configuration.method),
        max_gain: ReportedField::present_raw(
            3u32.saturating_mul(u32::from(configuration.max_gain).saturating_add(1)),
            Some("dB"),
            configuration.max_gain,
        ),
        channel_configuration: ReportedField::present_raw(
            format!(
                "code {} ({} parameter channel(s))",
                configuration.channel_config,
                configuration.channel_count()
            ),
            None,
            configuration.channel_config,
        ),
    }
}

fn channel_mode_label(raw: u8) -> Option<&'static str> {
    ChannelMode {
        codeword: 0,
        ch_mode: u32::from(raw),
    }
    .label()
}

/// 把 extended channel mode 的 presence flags 投影为实际承载布局。
///
/// 表 56 的 7.x.4/9.x.4 是配置 family；`four_back_channels_present`、
/// `centre_present` 与 `top_channels_present` 才决定该 substream 实际携带的声道。
fn effective_channel_layout_label(info: SubstreamInfoChan) -> Option<String> {
    let mode = info.channel_mode;
    if !matches!(mode.ch_mode, 11..=14) {
        return mode.label().map(str::to_owned);
    }

    effective_extended_channel_layout_label(
        mode.ch_mode,
        info.four_back_channels_present?,
        info.centre_present?,
        info.top_channels_present?,
    )
}

fn effective_extended_channel_layout_label(
    mode: u32,
    four_back_channels_present: bool,
    centre_present: bool,
    top_channels_present: u8,
) -> Option<String> {
    let mut fullband = if matches!(mode, 11 | 12) {
        7u8
    } else if matches!(mode, 13 | 14) {
        9u8
    } else {
        return None;
    };
    if !four_back_channels_present {
        fullband = fullband.checked_sub(2)?;
    }
    if !centre_present {
        fullband = fullband.checked_sub(1)?;
    }
    let top_channels = match top_channels_present {
        0 => 0,
        1 | 2 => 2,
        3 => 4,
        _ => return None,
    };
    let lfe = u8::from(matches!(mode, 12 | 14));
    Some(format!("{fullband}.{lfe}.{top_channels}"))
}

fn loudness_code(raw: u16) -> f64 {
    (f64::from(raw) - 1_024.0) / 10.0
}

fn bit_rate_field(bit_rate_bps: u64) -> ReportedField {
    let kbps = bit_rate_bps.saturating_add(500) / 1_000;
    ReportedField::present_unit(kbps, "kbps")
}

fn bitrate_range_field(bitrate: Ac4BitrateDsi) -> ReportedField {
    if bitrate.bit_rate == 0 || bitrate.precision_unknown() {
        return ReportedField::unknown_raw(
            json!({
                "mode": bitrate.mode.code(),
                "bit_rate_bps": bitrate.bit_rate,
                "precision_bps": bitrate.precision,
            }),
            "dac4 bitrate or precision is unspecified",
        );
    }
    let minimum = bitrate.bit_rate.saturating_sub(bitrate.precision);
    let maximum = bitrate.bit_rate.saturating_add(bitrate.precision);
    let minimum_kbps = minimum.saturating_add(500) / 1_000;
    let maximum_kbps = maximum.saturating_add(500) / 1_000;
    ReportedField::present_raw(
        json!({
            "minimum": minimum_kbps,
            "maximum": maximum_kbps,
            "display": format!("{minimum_kbps} - {maximum_kbps}"),
        }),
        Some("kbps"),
        json!({
            "mode": bitrate.mode.code(),
            "bit_rate_bps": bitrate.bit_rate,
            "precision_bps": bitrate.precision,
        }),
    )
}

fn frame_rate_field(rate: macindecode_ac4_mp4::FrameRate, frame_rate_index: u8) -> ReportedField {
    let decimal = f64::from(rate.numerator) / f64::from(rate.denominator);
    ReportedField::present_raw(
        json!({
            "numerator": rate.numerator,
            "denominator": rate.denominator,
            "decimal": round_decimal(decimal, 3),
            "display": format_decimal(decimal, 3),
        }),
        Some("fps"),
        json!({
            "frame_rate_index": frame_rate_index,
            "frame_length_base": rate.frame_length_base,
        }),
    )
}

fn iframe_interval_field(intervals: &BTreeMap<u64, u64>) -> ReportedField {
    if intervals.is_empty() {
        return ReportedField::not_present();
    }
    if intervals.len() == 1 {
        let interval = intervals.keys().next().copied().unwrap_or_default();
        return ReportedField::present_raw(
            json!({"kind": "fixed", "frames": interval, "display": interval.to_string()}),
            Some("frames"),
            json!({"distribution": intervals}),
        );
    }
    let minimum = intervals.keys().next().copied().unwrap_or_default();
    let maximum = intervals.keys().next_back().copied().unwrap_or_default();
    ReportedField::present_raw(
        json!({
            "kind": "variable",
            "minimum": minimum,
            "maximum": maximum,
            "display": format!("Variable {minimum} - {maximum}"),
        }),
        Some("frames"),
        json!({"distribution": intervals}),
    )
}

fn single_raw_value_field<T>(values: &BTreeSet<T>) -> ReportedField
where
    T: Copy + Into<Value> + Ord,
{
    match values.len() {
        0 => ReportedField::not_present(),
        1 => {
            let value = values
                .iter()
                .next()
                .copied()
                .map(Into::into)
                .unwrap_or(Value::Null);
            ReportedField::present_raw(value.clone(), None, value)
        }
        _ => ReportedField::unknown("value changes within the stream"),
    }
}

fn round_decimal(value: f64, digits: u32) -> f64 {
    let factor = 10f64.powi(i32::try_from(digits).unwrap_or(0));
    (value * factor).round() / factor
}

fn format_decimal(value: f64, digits: usize) -> String {
    let mut output = format!("{value:.digits$}");
    while output.contains('.') && output.ends_with('0') {
        output.pop();
    }
    if output.ends_with('.') {
        output.pop();
    }
    output
}

impl InspectReport {
    /// 渲染稳定的英文纯文本成功输出。
    pub(crate) fn write_text(self) -> Result<(), CliError> {
        let text = self.render_text();
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        writer.write_all(text.as_bytes()).map_err(|error| {
            CliError::new(
                "inspect",
                DiagnosticCode::OutputWriteFailed,
                "Failed to write to standard output",
            )
            .with_context("cause", error.to_string())
        })
    }

    pub(crate) fn render_text(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(output, "Audio:");
        text_field(
            &mut output,
            1,
            "Codec",
            &ReportedField::present(self.stream.codec.clone()),
        );
        text_field(
            &mut output,
            1,
            "Source",
            &ReportedField::present(self.source.kind),
        );
        text_field(&mut output, 1, "Track index", &self.source.track_index);
        let _ = writeln!(output, "    Frames: {}", self.source.frame_count);
        text_field(&mut output, 1, "Duration", &self.source.duration);
        text_field(&mut output, 1, "Bit rate", &self.stream.bit_rate);
        text_field(
            &mut output,
            1,
            "Estimated average bit rate",
            &self.stream.estimated_average_bit_rate,
        );
        text_field(
            &mut output,
            1,
            "Bitstream version",
            &self.stream.bitstream_version,
        );
        text_field(&mut output, 1, "Frame rate", &self.stream.frame_rate);
        text_field(&mut output, 1, "Sample rate", &self.stream.sample_rate);
        text_field(&mut output, 1, "I-frame", &self.stream.i_frame);
        text_field(
            &mut output,
            1,
            "I-frame interval",
            &self.stream.i_frame_interval,
        );
        text_field(&mut output, 1, "Sync word", &self.stream.sync_word);
        text_field(&mut output, 1, "CRC errors", &self.stream.crc_errors);
        text_field(
            &mut output,
            1,
            "Number of presentations",
            &self.stream.number_of_presentations,
        );
        text_field(
            &mut output,
            1,
            "Number of audio substreams",
            &self.stream.number_of_audio_substreams,
        );

        for presentation in &self.presentations {
            let _ = writeln!(output, "Presentation {}:", presentation.index);
            text_field(
                &mut output,
                1,
                "Presentation ID",
                &presentation.presentation_id,
            );
            text_field(&mut output, 1, "Summary", &presentation.summary);
            text_field(&mut output, 1, "Type", &presentation.presentation_type);
            text_field(
                &mut output,
                1,
                "Minimal compatibility level",
                &presentation.minimal_compatibility_level,
            );
            text_field(
                &mut output,
                1,
                "Dialogue normalization",
                &presentation.dialogue_normalization,
            );
            text_field(&mut output, 1, "Language", &presentation.language);
            text_field(&mut output, 1, "Multi-PID", &presentation.multi_pid);
            text_field(&mut output, 1, "Bit rate", &presentation.bit_rate);
            text_audio_substreams(&mut output, &presentation.audio_substreams);
            text_field(
                &mut output,
                1,
                "Metadata authentication ID",
                &presentation.metadata_authentication_id,
            );
            let _ = writeln!(output, "    Loudness:");
            text_field(&mut output, 2, "Loudness", &presentation.loudness.loudness);
            text_field(&mut output, 2, "Version", &presentation.loudness.version);
            text_field(
                &mut output,
                2,
                "Regulation type",
                &presentation.loudness.regulation_type,
            );
            text_field(
                &mut output,
                2,
                "Correction type",
                &presentation.loudness.correction_type,
            );
            text_field(
                &mut output,
                2,
                "Dialogue Intelligence",
                &presentation.loudness.dialogue_intelligence,
            );
            text_field(
                &mut output,
                2,
                "Integrated loudness (speech-gated)",
                &presentation.loudness.integrated_speech_gated,
            );
            text_field(
                &mut output,
                2,
                "Integrated loudness (level-gated)",
                &presentation.loudness.integrated_level_gated,
            );
            text_field(
                &mut output,
                2,
                "Maximum true peak",
                &presentation.loudness.maximum_true_peak,
            );
            text_field(
                &mut output,
                2,
                "Maximum momentary loudness",
                &presentation.loudness.maximum_momentary_loudness,
            );
            text_field(
                &mut output,
                2,
                "Loudness range",
                &presentation.loudness.loudness_range,
            );
            let _ = writeln!(output, "    Dynamic range control:");
            text_field(
                &mut output,
                2,
                "Enhanced AC-3 profile",
                &presentation.dynamic_range_control.enhanced_ac3_profile,
            );
            text_field(
                &mut output,
                2,
                "Home theater AVR",
                &presentation.dynamic_range_control.home_theater_avr,
            );
            text_field(
                &mut output,
                2,
                "Flat-panel TV",
                &presentation.dynamic_range_control.flat_panel_tv,
            );
            text_field(
                &mut output,
                2,
                "Portable speakers",
                &presentation.dynamic_range_control.portable_speakers,
            );
            text_field(
                &mut output,
                2,
                "Portable headphones",
                &presentation.dynamic_range_control.portable_headphones,
            );
            let _ = writeln!(output, "    Mixing metadata:");
            text_field(
                &mut output,
                2,
                "Main audio ducking level",
                &presentation.mixing_metadata.main_audio_ducking_level,
            );
            text_field(
                &mut output,
                2,
                "Main audio ducking level, Center",
                &presentation.mixing_metadata.main_audio_ducking_level_center,
            );
            text_field(
                &mut output,
                2,
                "Main audio ducking level, Front",
                &presentation.mixing_metadata.main_audio_ducking_level_front,
            );
            let _ = writeln!(output, "    Downmix:");
            text_field(
                &mut output,
                2,
                "Lo/Ro Center mix gain",
                &presentation.downmix.loro_center_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Lo/Ro Surround mix gain",
                &presentation.downmix.loro_surround_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Lt/Rt Center mix gain",
                &presentation.downmix.ltrt_center_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Lt/Rt Surround mix gain",
                &presentation.downmix.ltrt_surround_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "LFE mix info",
                &presentation.downmix.lfe_mix_info,
            );
            text_field(
                &mut output,
                2,
                "LFE mix gain",
                &presentation.downmix.lfe_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Preferred downmix",
                &presentation.downmix.preferred_downmix,
            );
        }

        for substream in &self.audio_substreams {
            let _ = writeln!(output, "Substream {}:", substream.index);
            text_field(&mut output, 1, "Summary", &substream.summary);
            text_field(
                &mut output,
                1,
                "Channel configuration",
                &substream.channel_configuration,
            );
            text_field(&mut output, 1, "Channel layout", &substream.channel_layout);
            text_field(&mut output, 1, "Object coded", &substream.object_coded);
            text_field(&mut output, 1, "Bit rate", &substream.bit_rate);
            let _ = writeln!(output, "    Preprocessing:");
            text_field(
                &mut output,
                2,
                "Previous mix type 2-channel",
                &substream.preprocessing.previous_mix_type_2channel,
            );
            text_field(
                &mut output,
                2,
                "Phase 90 filter info 2-channel",
                &substream.preprocessing.phase90_filter_info_2channel,
            );
            text_field(
                &mut output,
                2,
                "Lo/Ro Center mix gain",
                &substream.preprocessing.loro_center_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Lo/Ro Surround mix gain",
                &substream.preprocessing.loro_surround_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Lo/Ro downmix loudness correction",
                &substream.preprocessing.loro_downmix_loudness_correction,
            );
            text_field(
                &mut output,
                2,
                "Lt/Rt Center mix gain",
                &substream.preprocessing.ltrt_center_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Lt/Rt Surround mix gain",
                &substream.preprocessing.ltrt_surround_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Lt/Rt downmix loudness correction",
                &substream.preprocessing.ltrt_downmix_loudness_correction,
            );
            text_field(
                &mut output,
                2,
                "LFE mix gain",
                &substream.preprocessing.lfe_mix_gain,
            );
            text_field(
                &mut output,
                2,
                "Preferred downmix",
                &substream.preprocessing.preferred_downmix,
            );
            text_field(
                &mut output,
                2,
                "Previous downmix type 5-channel",
                &substream.preprocessing.previous_downmix_type_5channel,
            );
            text_field(
                &mut output,
                2,
                "Previous upmix type 5-channel",
                &substream.preprocessing.previous_upmix_type_5channel,
            );
            text_field(
                &mut output,
                2,
                "Previous upmix type 3/4",
                &substream.preprocessing.previous_upmix_type_3_4,
            );
            text_field(
                &mut output,
                2,
                "Previous upmix type 3/2/2",
                &substream.preprocessing.previous_upmix_type_3_2_2,
            );
            text_field(
                &mut output,
                2,
                "Phase 90 filter info",
                &substream.preprocessing.phase90_filter_info,
            );
            text_field(
                &mut output,
                2,
                "Surround attenuation known",
                &substream.preprocessing.surround_attenuation_known,
            );
            text_field(
                &mut output,
                2,
                "LFE attenuation known",
                &substream.preprocessing.lfe_attenuation_known,
            );
            let _ = writeln!(output, "    Dialogue Enhancement:");
            text_field(
                &mut output,
                2,
                "Dialogue Enhancement enabled",
                &substream.dialogue_enhancement.enabled,
            );
            text_field(
                &mut output,
                2,
                "Method",
                &substream.dialogue_enhancement.method,
            );
            text_field(
                &mut output,
                2,
                "Max Dialogue Enhancement gain",
                &substream.dialogue_enhancement.max_gain,
            );
            text_field(
                &mut output,
                2,
                "Dialogue Enhancement channel configuration",
                &substream.dialogue_enhancement.channel_configuration,
            );
        }

        let _ = writeln!(output, "Issues:");
        if self.issues.is_empty() {
            let _ = writeln!(output, "    None");
        } else {
            for issue in &self.issues {
                let mut context = String::new();
                if let Some(frame) = issue.frame_index {
                    let _ = write!(context, " frame={frame}");
                }
                if let Some(presentation) = issue.presentation_id {
                    let _ = write!(context, " presentation={presentation}");
                }
                if let Some(substream) = issue.substream_index {
                    let _ = write!(context, " substream={substream}");
                }
                let _ = writeln!(
                    output,
                    "    [{}] {}: {}{}",
                    issue.severity, issue.code, issue.message, context
                );
            }
        }
        output
    }
}

fn text_field(output: &mut String, indent: usize, label: &str, field: &ReportedField) {
    let _ = writeln!(
        output,
        "{}{}: {}",
        "    ".repeat(indent),
        label,
        field.text()
    );
}

fn text_audio_substreams(output: &mut String, field: &ReportedField) {
    if field.status != FieldStatus::Present {
        text_field(output, 1, "Audio substreams", field);
        return;
    }
    let Some(references) = field.value.as_ref().and_then(Value::as_array) else {
        text_field(output, 1, "Audio substreams", field);
        return;
    };
    let references = references
        .iter()
        .filter_map(Value::as_u64)
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        output,
        "    Audio substreams: {}",
        if references.is_empty() {
            "None"
        } else {
            &references
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_states_remain_distinct_in_text_and_json() {
        let fields = [
            ReportedField::present_raw(-18.0, Some("LKFS"), 844),
            ReportedField::not_present(),
            ReportedField::not_applicable(),
            ReportedField::unknown_raw(7, "reserved code"),
            ReportedField::unsupported("no ETSI mapping"),
        ];
        assert_eq!(fields[0].text(), "-18.0 LKFS (raw: 844)");
        assert_eq!(fields[1].text(), "Not present");
        assert_eq!(fields[2].text(), "Not applicable");
        assert_eq!(fields[3].text(), "Unknown: reserved code");
        assert_eq!(fields[4].text(), "Unsupported: no ETSI mapping");
        let value = serde_json::to_value(fields).unwrap();
        let statuses = value
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field.get("status").and_then(Value::as_str).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                "present",
                "not_present",
                "not_applicable",
                "unknown",
                "unsupported"
            ]
        );
    }

    #[test]
    fn spec_mappings_cover_loudness_drc_downmix_and_de() {
        assert_eq!(loudness_code(844), -18.0);
        assert_eq!(loudness_practice_field(1).text(), "ATSC A/85 (raw: 1)");
        assert_eq!(drc_profile_field(4).text(), "Music light (raw: 4)");
        assert_eq!(center_mix_gain_field(4).text(), "-3.0 dB (raw: 4)");
        let de = dialogue_enhancement_report(
            true,
            Some(DialogEnhancementConfiguration {
                method: 0,
                max_gain: 2,
                channel_config: 0,
            }),
        );
        assert_eq!(de.max_gain.text(), "9 dB (raw: 2)");
    }

    #[test]
    fn drc_device_fields_only_report_their_declared_decoder_modes() {
        const STRICT: [u8; 4] = [0x00, 0x3d, 0x01, 0x00];
        let context = PresentationSubstreamContext::new(
            false,
            true,
            1,
            1,
            PresentationChannelContext::UNDEFINED,
        );
        let mut state = PresentationDrcState::new();
        Ac4PresentationSubstream::parse_with_drc_state(&STRICT, context, &mut state)
            .expect("严格 presentation payload 应能解析");
        let configuration = state.configuration().expect("payload 应声明 DRC 配置");
        let modes = configuration.decoder_modes();
        assert_eq!(modes.len(), 1);
        let declared_mode = usize::from(modes.first().expect("应有一个 decoder mode").mode_id);
        assert!(declared_mode < 4);

        let report = drc_report(Some(configuration));
        assert_eq!(report.enhanced_ac3_profile.status, FieldStatus::Present);
        for (mode, field) in [
            &report.home_theater_avr,
            &report.flat_panel_tv,
            &report.portable_speakers,
            &report.portable_headphones,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                field.status,
                if mode == declared_mode {
                    FieldStatus::Present
                } else {
                    FieldStatus::NotPresent
                }
            );
        }
    }

    #[test]
    fn unsupported_audio_metadata_does_not_fall_back_to_absent_or_disabled() {
        let report = build_substream_report(
            &SubstreamAccumulator {
                index: 1,
                role: "Main",
                channel_info: None,
                ims: false,
                object_coded: false,
                measured_bytes: 0,
                preprocessing: None,
                preprocessing_sampled: false,
                preprocessing_conflict: false,
                metadata_failure: Some(MetadataFailure::Unsupported),
                de_configuration: None,
                reported_de_configuration: None,
                de_configuration_sampled: false,
                de_configuration_conflict: false,
                de_seen: false,
            },
            None,
            false,
            false,
        );
        let preprocessing = serde_json::to_value(report.preprocessing).unwrap();
        assert!(
            preprocessing
                .as_object()
                .unwrap()
                .values()
                .all(|field| field["status"] == "unsupported")
        );
        let dialogue_enhancement = serde_json::to_value(report.dialogue_enhancement).unwrap();
        assert!(
            dialogue_enhancement
                .as_object()
                .unwrap()
                .values()
                .all(|field| field["status"] == "unsupported")
        );
    }

    #[test]
    fn unsupported_presentation_metadata_does_not_fall_back_to_not_present() {
        let report = build_presentation_report(
            &PresentationAccumulator {
                index: 0,
                id: Some(1),
                presentation_config: None,
                md_compat: None,
                multi_pid: None,
                alternative: false,
                scene_path: "Object-Based",
                channel_label: None,
                audio_substreams: BTreeSet::from([1]),
                role: "Main",
                language: None,
                language_conflict: false,
                dsi_bitrate: None,
                measured_audio_bytes: 0,
                parsed_metadata: None,
                metadata_failure: Some(MetadataFailure::Unsupported),
                metadata_conflicts: PresentationMetadataConflicts::default(),
            },
            None,
            false,
            false,
        );
        assert_eq!(
            report.dialogue_normalization.status,
            FieldStatus::Unsupported
        );
        for section in [
            serde_json::to_value(report.loudness).unwrap(),
            serde_json::to_value(report.dynamic_range_control).unwrap(),
            serde_json::to_value(report.mixing_metadata).unwrap(),
            serde_json::to_value(report.downmix).unwrap(),
        ] {
            assert!(
                section
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|field| field["status"] == "unsupported")
            );
        }
    }

    #[test]
    fn mixed_presentation_summary_is_not_replaced_by_a_channel_label() {
        let report = build_presentation_report(
            &PresentationAccumulator {
                index: 0,
                id: Some(1),
                presentation_config: None,
                md_compat: None,
                multi_pid: None,
                alternative: false,
                scene_path: "Mixed",
                channel_label: Some("5.1".to_owned()),
                audio_substreams: BTreeSet::new(),
                role: "Main",
                language: None,
                language_conflict: false,
                dsi_bitrate: None,
                measured_audio_bytes: 0,
                parsed_metadata: None,
                metadata_failure: None,
                metadata_conflicts: PresentationMetadataConflicts::default(),
            },
            None,
            false,
            false,
        );
        assert_eq!(
            report.summary.value,
            Some(json!("Mixed main (single_group)"))
        );
    }

    #[test]
    fn iframe_interval_reports_fixed_and_variable_distributions() {
        let fixed = BTreeMap::from([(24, 3)]);
        assert_eq!(
            iframe_interval_field(&fixed).text(),
            "24 frames (raw: {\"distribution\":{\"24\":3}})"
        );
        let variable = BTreeMap::from([(24, 2), (48, 1)]);
        assert!(
            iframe_interval_field(&variable)
                .text()
                .starts_with("Variable 24 - 48 frames")
        );
    }

    #[test]
    fn raw_duration_accumulates_mixed_frame_rates_as_an_exact_ratio() {
        let fps_24 = frame_duration_ratio(1, 1).expect("24 fps timing should be defined");
        let fps_23_438 =
            frame_duration_ratio(1, 13).expect("48 kHz / 2048 timing should be defined");
        let duration = fps_24
            .checked_add(fps_23_438)
            .expect("small frame-duration ratios should not overflow");

        assert_eq!((duration.numerator, duration.denominator), (253, 3_000));
        assert_eq!(
            round_decimal(duration.seconds().unwrap_or_default(), 6),
            0.084333
        );
        assert_eq!(duration.bit_rate(1_000), Some(94_862));
        assert!(frame_duration_ratio(1, 14).is_none());
        assert!(frame_duration_ratio(0, 1).is_none());
    }

    #[test]
    fn preprocessing_projects_legacy_downmix_fields_without_applying_them() {
        let report = preprocessing_report(PreprocessingMetadata {
            stereo_downmix: Some(
                macindecode_ac4_bitstream::audio_substream::StereoDownmixPreprocessingMetadata {
                    loro_centre_mixgain: 4,
                    loro_surround_mixgain: 4,
                    loro_dmx_loud_corr: Some(17),
                    ltrt_centre_mixgain: Some(3),
                    ltrt_surround_mixgain: Some(5),
                    ltrt_dmx_loud_corr: Some(9),
                    lfe_mixgain: Some(12),
                    preferred_dmx_method: 2,
                },
            ),
            ..PreprocessingMetadata::default()
        });
        assert_eq!(report.loro_center_mix_gain.text(), "-3.0 dB (raw: 4)");
        assert_eq!(report.loro_surround_mix_gain.text(), "-3.0 dB (raw: 4)");
        assert_eq!(
            report.loro_downmix_loudness_correction.text(),
            "17 (raw: 17)"
        );
        assert_eq!(report.ltrt_center_mix_gain.text(), "-1.5 dB (raw: 3)");
        assert_eq!(report.ltrt_surround_mix_gain.text(), "-4.5 dB (raw: 5)");
        assert_eq!(report.lfe_mix_gain.text(), "-6.5 dB (raw: 12)");
        assert_eq!(report.preferred_downmix.text(), "Lt/Rt (raw: 2)");
    }

    #[test]
    fn extended_channel_presence_flags_produce_the_effective_layout() {
        assert_eq!(
            effective_extended_channel_layout_label(12, false, true, 3).as_deref(),
            Some("5.1.4")
        );
        assert_eq!(
            effective_extended_channel_layout_label(14, true, true, 3).as_deref(),
            Some("9.1.4")
        );
        assert_eq!(
            effective_extended_channel_layout_label(11, false, false, 1).as_deref(),
            Some("4.0.2")
        );
        assert_eq!(
            effective_extended_channel_layout_label(10, true, true, 3),
            None
        );
    }

    #[test]
    fn reserved_semantic_codes_create_one_deduplicated_issue() {
        let value = json!({
            "first": drc_profile_field(7),
            "same_projection": drc_profile_field(7),
        });
        let mut seen = BTreeSet::new();
        let mut issues = Vec::new();
        collect_reserved_code_issues(&value, "$", &mut seen, &mut issues);
        assert_eq!(issues.len(), 1);
        let issue = issues.first().unwrap();
        assert_eq!(issue.code, "reserved_code");
        assert!(issue.message.contains("raw_code=7"));
    }

    #[test]
    fn configuration_generation_reset_drops_only_parser_history() {
        let key = PresentationKey::Anonymous(0);
        let mut aggregate = Aggregator::default();
        aggregate
            .presentation_states
            .insert(key, PresentationParseState::default());
        aggregate
            .presentation_channels
            .insert(key, PresentationChannelContext::UNDEFINED);
        aggregate.audio_contexts.insert(
            1,
            SubstreamContext {
                sus_ver: 1,
                alternative: false,
                ajoc: false,
                channel_mode: Some(1),
                b_iframe: Some(true),
                alternative_oamd: None,
            },
        );
        let configuration = DialogEnhancementConfiguration {
            method: 0,
            max_gain: 2,
            channel_config: 0,
        };
        aggregate.substreams.insert(
            1,
            SubstreamAccumulator {
                index: 1,
                role: "Main",
                channel_info: None,
                ims: false,
                object_coded: false,
                measured_bytes: 0,
                preprocessing: None,
                preprocessing_sampled: true,
                preprocessing_conflict: false,
                metadata_failure: None,
                de_configuration: Some(configuration),
                reported_de_configuration: Some(configuration),
                de_configuration_sampled: true,
                de_configuration_conflict: false,
                de_seen: true,
            },
        );

        aggregate.reset_parser_history();

        assert!(aggregate.presentation_states.is_empty());
        assert!(aggregate.presentation_channels.is_empty());
        assert!(aggregate.audio_contexts.is_empty());
        let substream = aggregate.substreams.get(&1).unwrap();
        assert_eq!(substream.de_configuration, None);
        assert_eq!(substream.reported_de_configuration, Some(configuration));
        assert!(substream.preprocessing_sampled);
    }
}
