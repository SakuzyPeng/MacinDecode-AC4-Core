//! MacinDecode AC-4 的内部性能测量支撑。
//!
//! 本 crate 只负责准备已定界的 MP4 access unit、驱动公开 Scene Session，
//! 以及生成可复核的性能 JSON。容器读取、JSON 和报告整理都不进入计时区。

use macindecode_ac4_bitstream::Ac4Toc;
use macindecode_ac4_mp4::{
    Ac4Dsi, EditListEntry, SampleTable, find_ac4_track, find_box, find_path,
    media_time_to_presentation, parse_edit_list, parse_header_timing, presentation_timing,
};
use macindecode_ac4_scene::{
    Ac4DecoderConfig, Ac4DecoderSession, AccessUnit, AccessUnitContext, DecodeMode, DecodeStatus,
    DecodedAccessUnit, PresentationSelection,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::hint::black_box;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const REPORT_SCHEMA: &str = "macindecode-ac4.performance";
pub const REPORT_SCHEMA_VERSION: u32 = 1;
pub const WORST_ACCESS_UNIT_LIMIT: usize = 8;
const MAX_EDIT_ENTRIES: usize = 8;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

pub type PerfResult<T> = Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BenchmarkMode {
    Core,
    Full,
}

impl BenchmarkMode {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Full => "full",
        }
    }

    const fn decode_mode(self) -> DecodeMode {
        match self {
            Self::Core => DecodeMode::Core,
            Self::Full => DecodeMode::Full,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BaselineManifest {
    #[serde(default)]
    presentation_overrides: BTreeMap<String, u32>,
    entries: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestCase {
    pub key: String,
    pub path: PathBuf,
    pub presentation: PresentationSelection,
}

pub fn load_manifest(path: &Path, vectors_root: &Path) -> PerfResult<Vec<ManifestCase>> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read benchmark manifest {}: {error}", path.display()))?;
    let manifest: BaselineManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid benchmark manifest {}: {error}", path.display()))?;
    manifest_cases(manifest, vectors_root)
}

fn manifest_cases(
    manifest: BaselineManifest,
    vectors_root: &Path,
) -> PerfResult<Vec<ManifestCase>> {
    if manifest.entries.is_empty() {
        return Err("benchmark manifest has no entries".to_owned());
    }
    let mut cases = Vec::with_capacity(manifest.entries.len());
    for (key, entry) in manifest.entries {
        if !entry.is_object() {
            return Err(format!("benchmark entry {key:?} must be an object"));
        }
        let path = path_for_key(vectors_root, &key)?;
        let presentation = manifest
            .presentation_overrides
            .get(&key)
            .copied()
            .map(PresentationSelection::Index)
            .unwrap_or(PresentationSelection::AutoUnique);
        cases.push(ManifestCase {
            key,
            path,
            presentation,
        });
    }
    Ok(cases)
}

fn path_for_key(vectors_root: &Path, key: &str) -> PerfResult<PathBuf> {
    if key.contains('\\') {
        return Err(format!("invalid benchmark key {key:?}"));
    }
    let mut parts = key.split('/');
    let case = parts.next().unwrap_or_default();
    let file = parts.next().unwrap_or_default();
    if case.is_empty()
        || file.is_empty()
        || parts.next().is_some()
        || matches!(case, "." | "..")
        || matches!(file, "." | "..")
    {
        return Err(format!("invalid benchmark key {key:?}"));
    }
    Ok(vectors_root.join(case).join("encoded").join(file))
}

#[derive(Debug, Clone)]
struct AccessUnitDescriptor {
    range: Range<usize>,
    context: AccessUnitContext,
    codec_samples: u32,
}

#[derive(Debug)]
pub struct PreparedCase {
    key: String,
    data: Vec<u8>,
    access_units: Vec<AccessUnitDescriptor>,
    sample_rate: u32,
    codec_samples: u64,
    presentation: PresentationSelection,
}

impl PreparedCase {
    pub fn load(manifest: &ManifestCase) -> PerfResult<Self> {
        let data = fs::read(&manifest.path).map_err(|error| {
            format!(
                "cannot read benchmark input {}: {error}",
                manifest.path.display()
            )
        })?;
        Self::parse(manifest.key.clone(), data, manifest.presentation)
    }

    pub fn load_path(
        key: impl Into<String>,
        path: &Path,
        presentation: PresentationSelection,
    ) -> PerfResult<Self> {
        let data = fs::read(path)
            .map_err(|error| format!("cannot read profile input {}: {error}", path.display()))?;
        Self::parse(key.into(), data, presentation)
    }

    fn parse(key: String, data: Vec<u8>, presentation: PresentationSelection) -> PerfResult<Self> {
        let (sample_rate, access_units) = parse_mp4_access_units(&data)?;
        if access_units.is_empty() {
            return Err(format!("benchmark input {key:?} has no AC-4 access units"));
        }
        let codec_samples = access_units.iter().try_fold(0u64, |total, access_unit| {
            total
                .checked_add(u64::from(access_unit.codec_samples))
                .ok_or_else(|| format!("codec sample count overflows for {key:?}"))
        })?;
        Ok(Self {
            key,
            data,
            access_units,
            sample_rate,
            codec_samples,
            presentation,
        })
    }

    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[must_use]
    pub fn access_unit_count(&self) -> usize {
        self.access_units.len()
    }

    #[must_use]
    pub const fn codec_samples(&self) -> u64 {
        self.codec_samples
    }

    #[must_use]
    pub fn new_session(&self, mode: BenchmarkMode) -> Ac4DecoderSession {
        Ac4DecoderSession::new(
            Ac4DecoderConfig::new(self.presentation)
                .with_decode_mode(mode.decode_mode())
                .with_core_band_diagnostics(false),
        )
    }

    pub fn decode_pass(&self, session: &mut Ac4DecoderSession) -> PerfResult<Consumption> {
        let mut consumption = Consumption::default();
        for descriptor in &self.access_units {
            let decoded = self.decode_descriptor(session, descriptor)?;
            consumption.merge(consume_decoded(&decoded));
        }
        black_box(consumption);
        Ok(consumption)
    }

    fn decode_descriptor<'session>(
        &self,
        session: &'session mut Ac4DecoderSession,
        descriptor: &AccessUnitDescriptor,
    ) -> PerfResult<DecodedAccessUnit<'session>> {
        let raw_frame = self.data.get(descriptor.range.clone()).ok_or_else(|| {
            format!(
                "prepared AU range {:?} escaped benchmark input {:?}",
                descriptor.range, self.key
            )
        })?;
        let decoded = session
            .decode_access_unit(AccessUnit::new(raw_frame, descriptor.context))
            .map_err(|error| {
                format!(
                    "{} {} decode failed: {error}",
                    self.key,
                    descriptor.context.index()
                )
            })?;
        match decoded.status() {
            DecodeStatus::Decoded => Ok(decoded),
            DecodeStatus::WaitingForRandomAccess { reason } => Err(format!(
                "{} AU {} waited for random access: {reason:?}",
                self.key,
                descriptor.context.index()
            )),
            _ => Err(format!(
                "{} AU {} returned an unknown decode status",
                self.key,
                descriptor.context.index()
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Consumption {
    pub frames: u64,
    pub planes: u64,
    pub samples: u64,
    pub token: u64,
}

impl Consumption {
    pub fn merge(&mut self, other: Self) {
        self.frames = self.frames.saturating_add(other.frames);
        self.planes = self.planes.saturating_add(other.planes);
        self.samples = self.samples.saturating_add(other.samples);
        self.token = self
            .token
            .wrapping_mul(0x9e37_79b9)
            .wrapping_add(other.token);
    }
}

fn consume_decoded(decoded: &DecodedAccessUnit<'_>) -> Consumption {
    let mut consumed = Consumption::default();
    for frame in decoded.frames() {
        consumed.frames = consumed.frames.saturating_add(1);
        consumed.token = consumed
            .token
            .wrapping_mul(16_777_619)
            .wrapping_add(u64::from(frame.timeline().duration_samples()));
        consumed.token ^= u64::try_from(frame.objects().len()).unwrap_or(u64::MAX);
        consumed.token ^= u64::try_from(frame.beds().len())
            .unwrap_or(u64::MAX)
            .rotate_left(17);
        for object in frame.objects() {
            let pcm = object.pcm();
            for plane in pcm.planes() {
                consume_plane(plane.samples(), &mut consumed);
            }
        }
        for bed in frame.beds() {
            for component in bed.components() {
                consume_plane(component.plane().samples(), &mut consumed);
            }
        }
    }
    black_box(consumed)
}

fn consume_plane(samples: &[f32], consumed: &mut Consumption) {
    consumed.planes = consumed.planes.saturating_add(1);
    consumed.samples = consumed
        .samples
        .saturating_add(u64::try_from(samples.len()).unwrap_or(u64::MAX));
    let first = samples.first().copied().unwrap_or_default().to_bits();
    let last = samples.last().copied().unwrap_or_default().to_bits();
    consumed.token = consumed
        .token
        .wrapping_mul(16_777_619)
        .wrapping_add(u64::from(first))
        .wrapping_add(u64::from(last).rotate_left(23));
}

#[derive(Debug, Clone, Copy)]
pub struct TimingSettings {
    pub cold_runs: u32,
    pub min_passes: u32,
    pub min_time: Duration,
    pub max_passes: u32,
}

impl TimingSettings {
    pub fn validate(self) -> PerfResult<Self> {
        if self.cold_runs == 0 {
            return Err("cold-runs must be greater than zero".to_owned());
        }
        if self.min_passes == 0 {
            return Err("min-passes must be greater than zero".to_owned());
        }
        if self.max_passes < self.min_passes {
            return Err("max-passes must be at least min-passes".to_owned());
        }
        if self.min_time.is_zero() {
            return Err("min-time must be greater than zero".to_owned());
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencySummary {
    pub samples: u64,
    pub mean_ns: f64,
    pub p50_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorstAccessUnitTiming {
    /// 本次案例/模式测量中从零起的完整 pass 索引。
    pub pass_index: u32,
    /// 预解析 MP4 sample table 中从零起的 access-unit 索引。
    pub access_unit_index: u64,
    pub codec_samples: u32,
    pub latency_ns: u64,
    pub budget_ns: f64,
    pub deadline_ratio: f64,
    pub missed_deadline: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SteadyTiming {
    pub passes: u32,
    pub calls: u64,
    pub total_decode_ns: u64,
    pub realtime_factor: f64,
    pub ns_per_audio_sample: f64,
    pub latency: LatencySummary,
    pub worst_deadline_ratio: f64,
    pub deadline_misses: u64,
    pub worst_access_units: Vec<WorstAccessUnitTiming>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimingCaseReport {
    pub input: String,
    pub mode: BenchmarkMode,
    pub sample_rate_hz: u32,
    pub access_units: u64,
    pub codec_samples_per_pass: u64,
    pub cold_first_au: LatencySummary,
    pub steady: SteadyTiming,
}

#[derive(Debug, Clone, Copy)]
struct TimedAccessUnit {
    pass_index: u32,
    access_unit_index: u64,
    latency_ns: u64,
    codec_samples: u32,
}

#[derive(Debug)]
struct DeadlineSummary {
    misses: u64,
    worst_ratio: f64,
    worst_access_units: Vec<WorstAccessUnitTiming>,
}

pub fn run_timing_case(
    prepared: &PreparedCase,
    mode: BenchmarkMode,
    settings: TimingSettings,
) -> PerfResult<TimingCaseReport> {
    let settings = settings.validate()?;
    let cold_capacity =
        usize::try_from(settings.cold_runs).map_err(|_| "cold-runs exceeds usize".to_owned())?;
    let mut cold = Vec::with_capacity(cold_capacity);
    for _ in 0..settings.cold_runs {
        let mut session = prepared.new_session(mode);
        let descriptor = prepared
            .access_units
            .first()
            .ok_or_else(|| format!("benchmark input {:?} has no first AU", prepared.key))?;
        let started = Instant::now();
        let decoded = prepared.decode_descriptor(&mut session, descriptor)?;
        let elapsed = duration_ns(started.elapsed())?;
        let consumed = consume_decoded(&decoded);
        black_box(consumed);
        cold.push(elapsed);
    }

    let mut session = prepared.new_session(mode);
    black_box(prepared.decode_pass(&mut session)?);
    session.reset();

    let max_calls = prepared
        .access_unit_count()
        .checked_mul(
            usize::try_from(settings.max_passes)
                .map_err(|_| "max-passes exceeds usize".to_owned())?,
        )
        .ok_or_else(|| "timing sample capacity overflows usize".to_owned())?;
    let mut measured = Vec::with_capacity(max_calls);
    let mut total_decode_ns = 0u128;
    let mut passes = 0u32;
    while should_continue(passes, total_decode_ns, settings) {
        session.reset();
        for descriptor in &prepared.access_units {
            let started = Instant::now();
            let decoded = prepared.decode_descriptor(&mut session, descriptor)?;
            let elapsed = duration_ns(started.elapsed())?;
            total_decode_ns = total_decode_ns
                .checked_add(u128::from(elapsed))
                .ok_or_else(|| "total timing duration overflows u128".to_owned())?;
            let consumed = consume_decoded(&decoded);
            black_box(consumed);
            measured.push(TimedAccessUnit {
                pass_index: passes,
                access_unit_index: descriptor.context.index(),
                latency_ns: elapsed,
                codec_samples: descriptor.codec_samples,
            });
        }
        passes = passes
            .checked_add(1)
            .ok_or_else(|| "timing pass count overflows u32".to_owned())?;
    }
    if measured.is_empty() {
        return Err(format!(
            "{} {} produced no timing samples",
            prepared.key,
            mode.label()
        ));
    }

    let mut latencies: Vec<u64> = measured.iter().map(|item| item.latency_ns).collect();
    let deadline = deadline_summary(&measured, prepared.sample_rate)?;

    let calls =
        u64::try_from(measured.len()).map_err(|_| "timing call count exceeds u64".to_owned())?;
    let total_decode_ns_u64 = u64::try_from(total_decode_ns)
        .map_err(|_| "total timing duration exceeds u64 nanoseconds".to_owned())?;
    let decoded_samples = u128::from(prepared.codec_samples)
        .checked_mul(u128::from(passes))
        .ok_or_else(|| "decoded sample count overflows".to_owned())?;
    let realtime_factor = decoded_samples as f64 * NANOS_PER_SECOND as f64
        / (f64::from(prepared.sample_rate) * total_decode_ns as f64);
    let ns_per_audio_sample = total_decode_ns as f64 / decoded_samples as f64;
    validate_finite("realtime_factor", realtime_factor)?;
    validate_finite("ns_per_audio_sample", ns_per_audio_sample)?;
    validate_finite("worst_deadline_ratio", deadline.worst_ratio)?;

    Ok(TimingCaseReport {
        input: prepared.key.clone(),
        mode,
        sample_rate_hz: prepared.sample_rate,
        access_units: u64::try_from(prepared.access_unit_count())
            .map_err(|_| "access-unit count exceeds u64".to_owned())?,
        codec_samples_per_pass: prepared.codec_samples,
        cold_first_au: latency_summary(&mut cold)?,
        steady: SteadyTiming {
            passes,
            calls,
            total_decode_ns: total_decode_ns_u64,
            realtime_factor,
            ns_per_audio_sample,
            latency: latency_summary(&mut latencies)?,
            worst_deadline_ratio: deadline.worst_ratio,
            deadline_misses: deadline.misses,
            worst_access_units: deadline.worst_access_units,
        },
    })
}

fn deadline_summary(measured: &[TimedAccessUnit], sample_rate: u32) -> PerfResult<DeadlineSummary> {
    if measured.is_empty() {
        return Err("cannot summarize deadlines without access units".to_owned());
    }
    if sample_rate == 0 {
        return Err("cannot summarize deadlines at a zero sample rate".to_owned());
    }

    let mut misses = 0u64;
    let mut worst_ratio = 0.0f64;
    let mut worst_access_units = Vec::with_capacity(measured.len());
    for item in measured {
        if item.codec_samples == 0 {
            return Err("cannot summarize an AU with zero codec samples".to_owned());
        }
        let budget_numerator = u128::from(item.codec_samples)
            .checked_mul(NANOS_PER_SECOND)
            .ok_or_else(|| "AU deadline numerator overflows".to_owned())?;
        let elapsed_scaled = u128::from(item.latency_ns)
            .checked_mul(u128::from(sample_rate))
            .ok_or_else(|| "AU elapsed duration overflows".to_owned())?;
        let missed_deadline = elapsed_scaled > budget_numerator;
        if missed_deadline {
            misses = misses
                .checked_add(1)
                .ok_or_else(|| "deadline miss count overflows u64".to_owned())?;
        }
        let budget_ns =
            item.codec_samples as f64 * NANOS_PER_SECOND as f64 / f64::from(sample_rate);
        let deadline_ratio = item.latency_ns as f64 / budget_ns;
        validate_finite("AU budget_ns", budget_ns)?;
        validate_finite("AU deadline_ratio", deadline_ratio)?;
        worst_ratio = worst_ratio.max(deadline_ratio);
        worst_access_units.push(WorstAccessUnitTiming {
            pass_index: item.pass_index,
            access_unit_index: item.access_unit_index,
            codec_samples: item.codec_samples,
            latency_ns: item.latency_ns,
            budget_ns,
            deadline_ratio,
            missed_deadline,
        });
    }
    worst_access_units.sort_unstable_by(|left, right| {
        right
            .deadline_ratio
            .total_cmp(&left.deadline_ratio)
            .then_with(|| right.latency_ns.cmp(&left.latency_ns))
            .then_with(|| left.pass_index.cmp(&right.pass_index))
            .then_with(|| left.access_unit_index.cmp(&right.access_unit_index))
    });
    worst_access_units.truncate(WORST_ACCESS_UNIT_LIMIT);
    Ok(DeadlineSummary {
        misses,
        worst_ratio,
        worst_access_units,
    })
}

fn duration_ns(duration: Duration) -> PerfResult<u64> {
    u64::try_from(duration.as_nanos()).map_err(|_| "duration exceeds u64 nanoseconds".to_owned())
}

fn validate_finite(label: &str, value: f64) -> PerfResult<()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(format!("{label} is not a finite non-negative value"))
    }
}

#[must_use]
pub fn should_continue(passes: u32, total_decode_ns: u128, settings: TimingSettings) -> bool {
    passes < settings.max_passes
        && (passes < settings.min_passes || total_decode_ns < settings.min_time.as_nanos())
}

fn latency_summary(values: &mut [u64]) -> PerfResult<LatencySummary> {
    if values.is_empty() {
        return Err("cannot summarize an empty latency sample".to_owned());
    }
    values.sort_unstable();
    let sum = values.iter().try_fold(0u128, |total, value| {
        total
            .checked_add(u128::from(*value))
            .ok_or_else(|| "latency sum overflows u128".to_owned())
    })?;
    let count = u64::try_from(values.len()).map_err(|_| "latency count exceeds u64".to_owned())?;
    let mean_ns = sum as f64 / count as f64;
    validate_finite("mean_ns", mean_ns)?;
    Ok(LatencySummary {
        samples: count,
        mean_ns,
        p50_ns: nearest_rank(values, 50)?,
        p95_ns: nearest_rank(values, 95)?,
        p99_ns: nearest_rank(values, 99)?,
        max_ns: *values
            .last()
            .ok_or_else(|| "latency sample disappeared".to_owned())?,
    })
}

pub fn nearest_rank(sorted: &[u64], percentile: u32) -> PerfResult<u64> {
    if sorted.is_empty() {
        return Err("nearest-rank requires at least one sample".to_owned());
    }
    if !(1..=100).contains(&percentile) {
        return Err("nearest-rank percentile must be in 1..=100".to_owned());
    }
    let numerator = sorted
        .len()
        .checked_mul(usize::try_from(percentile).map_err(|_| "percentile exceeds usize")?)
        .ok_or_else(|| "nearest-rank numerator overflows usize".to_owned())?;
    let rank = numerator
        .checked_add(99)
        .ok_or_else(|| "nearest-rank rounding overflows usize".to_owned())?
        / 100;
    sorted
        .get(rank.saturating_sub(1))
        .copied()
        .ok_or_else(|| "nearest-rank result escaped sample bounds".to_owned())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnvironmentReport {
    pub generated_at_utc: Option<String>,
    pub architecture: String,
    pub operating_system: String,
    pub cpu: Option<String>,
    pub memory_bytes: Option<u64>,
    pub rustc: Option<String>,
    pub cargo: Option<String>,
    pub git_commit: Option<String>,
    pub git_dirty: Option<bool>,
    pub build_profile: String,
    pub target_cpu: String,
    pub instrumented_allocator: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimingReportSettings {
    pub manifest: String,
    pub modes: Vec<BenchmarkMode>,
    pub cold_runs: u32,
    pub warmup_passes: u32,
    pub min_passes: u32,
    pub min_time_ns: u64,
    pub max_passes: u32,
    pub worst_access_unit_limit: usize,
    pub timed_boundary: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimingReport {
    pub schema: &'static str,
    pub schema_version: u32,
    pub kind: &'static str,
    pub environment: EnvironmentReport,
    pub settings: TimingReportSettings,
    pub cases: Vec<TimingCaseReport>,
}

impl TimingReport {
    #[must_use]
    pub fn new(settings: TimingReportSettings, cases: Vec<TimingCaseReport>) -> Self {
        Self {
            schema: REPORT_SCHEMA,
            schema_version: REPORT_SCHEMA_VERSION,
            kind: "timing",
            environment: environment_report(false),
            settings,
            cases,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AllocationStatsReport {
    pub allocations: u64,
    pub deallocations: u64,
    pub reallocations: u64,
    pub bytes_allocated: u64,
    pub bytes_deallocated: u64,
    pub bytes_reallocated: i64,
}

impl AllocationStatsReport {
    #[must_use]
    pub const fn allocation_free(&self) -> bool {
        self.allocations == 0 && self.reallocations == 0
    }

    #[must_use]
    pub const fn heap_operation_free(&self) -> bool {
        self.allocation_free() && self.deallocations == 0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AllocationCaseReport {
    pub input: String,
    pub mode: BenchmarkMode,
    pub sample_rate_hz: u32,
    pub access_units: u64,
    pub codec_samples: u64,
    pub stats: AllocationStatsReport,
    pub allocation_free: bool,
    pub heap_operation_free: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AllocationReportSettings {
    pub manifest: String,
    pub modes: Vec<BenchmarkMode>,
    pub warmup_passes: u32,
    pub measured_passes: u32,
    pub measured_boundary: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AllocationReport {
    pub schema: &'static str,
    pub schema_version: u32,
    pub kind: &'static str,
    pub environment: EnvironmentReport,
    pub settings: AllocationReportSettings,
    pub cases: Vec<AllocationCaseReport>,
}

impl AllocationReport {
    #[must_use]
    pub fn new(settings: AllocationReportSettings, cases: Vec<AllocationCaseReport>) -> Self {
        Self {
            schema: REPORT_SCHEMA,
            schema_version: REPORT_SCHEMA_VERSION,
            kind: "allocations",
            environment: environment_report(true),
            settings,
            cases,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileReportSettings {
    pub warmup_passes: u32,
    pub startup_delay_ns: u64,
    pub requested_duration_ns: u64,
    pub qmf_split_symbols: bool,
    pub ajoc_reconstruction_split_symbols: bool,
    pub loop_boundary: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileResult {
    pub schema: &'static str,
    pub schema_version: u32,
    pub kind: &'static str,
    pub environment: EnvironmentReport,
    pub settings: ProfileReportSettings,
    pub input: String,
    pub mode: BenchmarkMode,
    pub sample_rate_hz: u32,
    pub access_units_per_pass: u64,
    pub codec_samples_per_pass: u64,
    pub duration_ns: u64,
    pub passes: u64,
    pub access_unit_calls: u64,
    pub consumption: ConsumptionReport,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct QmfPhaseSampleCount {
    pub inclusive_samples: u64,
    pub share_of_qmf_synthesis_percent: f64,
    pub share_of_profile_percent: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct QmfSynthesisPhaseSamples {
    pub state_advance: QmfPhaseSampleCount,
    pub modulation: QmfPhaseSampleCount,
    pub polyphase_tail: QmfPhaseSampleCount,
    pub unclassified: QmfPhaseSampleCount,
}

#[derive(Debug, Clone, Serialize)]
pub struct QmfSampleSummarySettings {
    pub sample_interval_us: u64,
    pub attribution: &'static str,
    pub required_feature: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct QmfSampleSummary {
    pub schema: &'static str,
    pub schema_version: u32,
    pub kind: &'static str,
    pub environment: EnvironmentReport,
    pub settings: QmfSampleSummarySettings,
    pub input: String,
    pub mode: BenchmarkMode,
    pub profile_duration_ns: u64,
    pub access_unit_calls: u64,
    pub profile_thread_samples: u64,
    pub qmf_synthesis_inclusive_samples: u64,
    pub qmf_synthesis_share_of_profile_percent: f64,
    pub phases: QmfSynthesisPhaseSamples,
}

const QMF_SYNTHESIS_SAMPLE_SYMBOL: &str = "macindecode_ac4_decode::aspx::qmf::synthesise::h";
const QMF_STATE_ADVANCE_SAMPLE_SYMBOL: &str =
    "macindecode_ac4_decode::aspx::qmf::advance_synthesis_state::h";
const QMF_MODULATION_SAMPLE_SYMBOL: &str =
    "macindecode_ac4_decode::aspx::qmf::modulate_synthesis_slot::h";
const QMF_POLYPHASE_SAMPLE_SYMBOL: &str =
    "macindecode_ac4_decode::aspx::qmf::accumulate_synthesis_polyphase::h";

/// 汇总 macOS `sample` 调用树中的 QMF 合成分段 inclusive 样本。
///
/// 只读取 `Call graph:` 到 `Total number in stack` 之间的调用树，不读取文件路径、
/// Binary Images 或 profiler 元数据。三个阶段在同一时刻互斥，因此可从父级
/// `synthesise` inclusive 样本中扣出未归类的循环/调用开销。
pub fn summarize_qmf_sample(
    sample: &str,
    environment: EnvironmentReport,
    input: String,
    mode: BenchmarkMode,
    profile_duration_ns: u64,
    access_unit_calls: u64,
    sample_interval_us: u64,
) -> PerfResult<QmfSampleSummary> {
    if input.is_empty() {
        return Err("QMF sample summary input label must not be empty".to_owned());
    }
    if profile_duration_ns == 0 || access_unit_calls == 0 || sample_interval_us == 0 {
        return Err("QMF sample summary counts and durations must be non-zero".to_owned());
    }
    let call_graph = sample_call_graph(sample)?;
    let profile_thread_samples = call_graph.thread_samples;
    let qmf_synthesis = call_graph.symbol_samples(QMF_SYNTHESIS_SAMPLE_SYMBOL)?;
    let state_advance = call_graph.symbol_samples(QMF_STATE_ADVANCE_SAMPLE_SYMBOL)?;
    let modulation = call_graph.symbol_samples(QMF_MODULATION_SAMPLE_SYMBOL)?;
    let polyphase_tail = call_graph.symbol_samples(QMF_POLYPHASE_SAMPLE_SYMBOL)?;
    let classified = state_advance
        .checked_add(modulation)
        .and_then(|value| value.checked_add(polyphase_tail))
        .ok_or_else(|| "QMF phase sample count overflows u64".to_owned())?;
    let unclassified = qmf_synthesis.checked_sub(classified).ok_or_else(|| {
        format!("QMF phase samples ({classified}) exceed synthesis samples ({qmf_synthesis})")
    })?;
    let phase = |samples| qmf_phase_sample_count(samples, qmf_synthesis, profile_thread_samples);
    let qmf_synthesis_share_of_profile_percent =
        sample_percent(qmf_synthesis, profile_thread_samples)?;

    Ok(QmfSampleSummary {
        schema: REPORT_SCHEMA,
        schema_version: REPORT_SCHEMA_VERSION,
        kind: "qmf_sample_summary",
        environment,
        settings: QmfSampleSummarySettings {
            sample_interval_us,
            attribution: "macOS sample Call graph inclusive symbols",
            required_feature: "qmf-split-profile",
        },
        input,
        mode,
        profile_duration_ns,
        access_unit_calls,
        profile_thread_samples,
        qmf_synthesis_inclusive_samples: qmf_synthesis,
        qmf_synthesis_share_of_profile_percent,
        phases: QmfSynthesisPhaseSamples {
            state_advance: phase(state_advance)?,
            modulation: phase(modulation)?,
            polyphase_tail: phase(polyphase_tail)?,
            unclassified: phase(unclassified)?,
        },
    })
}

fn qmf_phase_sample_count(
    samples: u64,
    qmf_synthesis_samples: u64,
    profile_samples: u64,
) -> PerfResult<QmfPhaseSampleCount> {
    Ok(QmfPhaseSampleCount {
        inclusive_samples: samples,
        share_of_qmf_synthesis_percent: sample_percent(samples, qmf_synthesis_samples)?,
        share_of_profile_percent: sample_percent(samples, profile_samples)?,
    })
}

fn sample_percent(numerator: u64, denominator: u64) -> PerfResult<f64> {
    if denominator == 0 {
        return Err("sample percentage denominator must be non-zero".to_owned());
    }
    let percent = numerator as f64 * 100.0 / denominator as f64;
    validate_finite("sample percentage", percent)?;
    Ok(percent)
}

#[derive(Debug)]
struct SampleCallGraph<'a> {
    thread_samples: u64,
    lines: Vec<&'a str>,
}

impl SampleCallGraph<'_> {
    fn symbol_samples(&self, symbol: &str) -> PerfResult<u64> {
        let mut total = 0u64;
        let mut found = false;
        for line in &self.lines {
            if let Some((prefix, _)) = line.split_once(symbol) {
                let samples = prefix
                    .split_whitespace()
                    .rev()
                    .find_map(|token| token.parse::<u64>().ok())
                    .ok_or_else(|| format!("sample call-graph symbol {symbol:?} has no count"))?;
                total = total
                    .checked_add(samples)
                    .ok_or_else(|| format!("sample call-graph count for {symbol:?} overflows"))?;
                found = true;
            }
        }
        if !found {
            return Err(format!("sample call graph is missing symbol {symbol:?}"));
        }
        Ok(total)
    }
}

fn sample_call_graph(sample: &str) -> PerfResult<SampleCallGraph<'_>> {
    let mut in_call_graph = false;
    let mut thread_samples = None;
    let mut lines = Vec::new();
    for line in sample.lines() {
        if line.trim() == "Call graph:" {
            in_call_graph = true;
            continue;
        }
        if !in_call_graph {
            continue;
        }
        if line.starts_with("Total number in stack") || line.starts_with("Sort by top of stack") {
            break;
        }
        if thread_samples.is_none() && line.contains(" Thread_") {
            thread_samples = line
                .split_whitespace()
                .find_map(|token| token.parse::<u64>().ok());
        }
        lines.push(line);
    }
    if !in_call_graph {
        return Err("sample text has no Call graph section".to_owned());
    }
    if lines.is_empty() {
        return Err("sample Call graph section is empty".to_owned());
    }
    let thread_samples = thread_samples
        .ok_or_else(|| "sample Call graph has no main Thread_ sample count".to_owned())?;
    if thread_samples == 0 {
        return Err("sample Call graph main thread has zero samples".to_owned());
    }
    Ok(SampleCallGraph {
        thread_samples,
        lines,
    })
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ConsumptionReport {
    pub frames: u64,
    pub planes: u64,
    pub samples: u64,
    pub token: u64,
}

impl From<Consumption> for ConsumptionReport {
    fn from(value: Consumption) -> Self {
        Self {
            frames: value.frames,
            planes: value.planes,
            samples: value.samples,
            token: value.token,
        }
    }
}

pub fn environment_report(instrumented_allocator: bool) -> EnvironmentReport {
    let operating_system = command_output("sw_vers", &["-productVersion"])
        .map(|version| format!("macOS {version}"))
        .unwrap_or_else(|| std::env::consts::OS.to_owned());
    let cpu = command_output("sysctl", &["-n", "machdep.cpu.brand_string"]);
    let memory_bytes =
        command_output("sysctl", &["-n", "hw.memsize"]).and_then(|value| value.parse::<u64>().ok());
    let git_commit = command_output("git", &["rev-parse", "HEAD"]);
    let git_dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty());
    EnvironmentReport {
        generated_at_utc: command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        architecture: std::env::consts::ARCH.to_owned(),
        operating_system,
        cpu,
        memory_bytes,
        rustc: command_output("rustc", &["-vV"]),
        cargo: command_output("cargo", &["-V"]),
        git_commit,
        git_dirty,
        build_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
        .to_owned(),
        target_cpu: "portable-default".to_owned(),
        instrumented_allocator,
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_mp4_access_units(data: &[u8]) -> PerfResult<(u32, Vec<AccessUnitDescriptor>)> {
    let moov = find_box(data, b"moov").ok_or_else(|| "MP4 has no moov box".to_owned())?;
    let mvhd = find_box(moov.payload, b"mvhd").ok_or_else(|| "MP4 has no mvhd box".to_owned())?;
    let movie = parse_header_timing(*b"mvhd", mvhd.payload)
        .map_err(|error| format!("invalid MP4 mvhd: {error}"))?;
    let track = find_ac4_track(moov.payload)
        .map_err(|error| format!("invalid MP4 sample description: {error}"))?
        .ok_or_else(|| "MP4 has no AC-4 track".to_owned())?;
    let mdhd = find_box(track.mdia.payload, b"mdhd")
        .ok_or_else(|| "AC-4 track has no mdhd box".to_owned())?;
    let media = parse_header_timing(*b"mdhd", mdhd.payload)
        .map_err(|error| format!("invalid MP4 mdhd: {error}"))?;

    let mut edit_storage = [EditListEntry {
        segment_duration: 0,
        media_time: 0,
        media_rate: (0, 0),
    }; MAX_EDIT_ENTRIES];
    let edit_count = find_path(track.trak.payload, &[*b"edts", *b"elst"])
        .map(|item| parse_edit_list(item.payload, &mut edit_storage))
        .transpose()
        .map_err(|error| format!("invalid MP4 edit list: {error}"))?
        .unwrap_or(0);
    let edits = edit_storage
        .get(..edit_count)
        .ok_or_else(|| "MP4 edit count escaped fixed storage".to_owned())?;
    if edits.iter().filter(|entry| !entry.is_empty_edit()).count() > 1 {
        return Err("benchmark input has multiple discontiguous media edits".to_owned());
    }
    let presentation = presentation_timing(media, movie.timescale, edits)
        .map_err(|error| format!("invalid MP4 presentation timing: {error}"))?;

    let specific = track
        .dac4()
        .ok_or_else(|| "AC-4 sample entry has no dac4 box".to_owned())?;
    let dsi =
        Ac4Dsi::parse(specific.payload).map_err(|error| format!("invalid MP4 dac4: {error}"))?;
    let sample_rate = dsi.base_sampling_frequency.hz();
    let table = SampleTable::parse(track.stbl.payload)
        .map_err(|error| format!("invalid MP4 sample table: {error}"))?;
    let priming = scale_u64_round(
        presentation.priming,
        u64::from(sample_rate),
        u64::from(media.timescale),
    )?;
    let presentation_shift =
        presentation_sample_shift(sample_rate, media.timescale, movie.timescale, edits)?;

    let capacity = usize::try_from(table.sample_count())
        .map_err(|_| "MP4 sample count exceeds usize".to_owned())?;
    let mut access_units = Vec::with_capacity(capacity);
    for item in table.iter() {
        let info = item.map_err(|error| format!("invalid MP4 sample: {error}"))?;
        track
            .sample_entry
            .validate_sample(&info)
            .map_err(|error| format!("invalid MP4 sample: {error}"))?;
        let range = checked_sample_range(info.offset, info.size, data.len())?;
        let raw_frame = data
            .get(range.clone())
            .ok_or_else(|| "checked MP4 sample range became invalid".to_owned())?;
        let toc = Ac4Toc::parse(raw_frame)
            .map_err(|error| format!("cannot parse AC-4 TOC for sample {}: {error}", info.index))?;
        let codec_samples = toc.codec_frame_len_base(1).ok_or_else(|| {
            format!(
                "cannot derive codec frame length for MP4 sample {}",
                info.index
            )
        })?;
        let source_start = scale_i64_round(
            info.composition_time,
            i64::from(sample_rate),
            i64::from(media.timescale),
        )?;
        let mut context = AccessUnitContext::new(u64::from(info.index))
            .with_source_sample_start(source_start)
            .with_priming_samples(priming)
            .with_random_access_hint(info.is_sync);
        if let Some(shift) = presentation_shift {
            let presentation_start = source_start
                .checked_add(shift)
                .ok_or_else(|| "presentation sample start overflows i64".to_owned())?;
            context = context.with_presentation_sample_start(presentation_start);
        }
        access_units.push(AccessUnitDescriptor {
            range,
            context,
            codec_samples: u32::from(codec_samples),
        });
    }
    Ok((sample_rate, access_units))
}

pub fn checked_sample_range(offset: u64, size: u32, input_len: usize) -> PerfResult<Range<usize>> {
    let start =
        usize::try_from(offset).map_err(|_| "MP4 sample offset exceeds usize".to_owned())?;
    let size = usize::try_from(size).map_err(|_| "MP4 sample size exceeds usize".to_owned())?;
    let end = start
        .checked_add(size)
        .ok_or_else(|| "MP4 sample range overflows usize".to_owned())?;
    if end > input_len {
        return Err(format!(
            "MP4 sample range {start}..{end} exceeds input length {input_len}"
        ));
    }
    Ok(start..end)
}

fn presentation_sample_shift(
    sample_rate: u32,
    media_timescale: u32,
    movie_timescale: u32,
    edits: &[EditListEntry],
) -> PerfResult<Option<i64>> {
    let Some(presentation_zero) =
        media_time_to_presentation(0, media_timescale, movie_timescale, edits)
            .map_err(|error| format!("cannot map MP4 presentation zero: {error}"))?
    else {
        return Ok(None);
    };
    scale_i64_round(
        presentation_zero,
        i64::from(sample_rate),
        i64::from(media_timescale),
    )
    .map(Some)
}

fn scale_i64_round(value: i64, numerator: i64, denominator: i64) -> PerfResult<i64> {
    if numerator <= 0 || denominator <= 0 {
        return Err("timeline ratio must be positive".to_owned());
    }
    let scaled = i128::from(value)
        .checked_mul(i128::from(numerator))
        .ok_or_else(|| "timeline multiplication overflows".to_owned())?;
    let half = i128::from(denominator) / 2;
    let rounded_numerator = if scaled >= 0 {
        scaled
            .checked_add(half)
            .ok_or_else(|| "timeline rounding overflows".to_owned())?
    } else {
        scaled
            .checked_sub(half)
            .ok_or_else(|| "timeline rounding overflows".to_owned())?
    };
    let rounded = rounded_numerator
        .checked_div(i128::from(denominator))
        .ok_or_else(|| "timeline divisor is zero".to_owned())?;
    i64::try_from(rounded).map_err(|_| "timeline value exceeds i64".to_owned())
}

fn scale_u64_round(value: u64, numerator: u64, denominator: u64) -> PerfResult<u64> {
    if numerator == 0 || denominator == 0 {
        return Err("timeline ratio must be positive".to_owned());
    }
    let scaled = u128::from(value)
        .checked_mul(u128::from(numerator))
        .ok_or_else(|| "timeline multiplication overflows".to_owned())?;
    let rounded = scaled
        .checked_add(u128::from(denominator / 2))
        .ok_or_else(|| "timeline rounding overflows".to_owned())?
        .checked_div(u128::from(denominator))
        .ok_or_else(|| "timeline divisor is zero".to_owned())?;
    u64::try_from(rounded).map_err(|_| "timeline value exceeds u64".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(json: &str) -> BaselineManifest {
        serde_json::from_str(json).expect("test manifest must parse")
    }

    #[test]
    fn manifest_paths_insert_encoded_directory_and_reject_escape() {
        let cases = manifest_cases(
            manifest(r#"{"entries":{"case/input.m4a":{}},"presentation_overrides":{}}"#),
            Path::new("vectors"),
        )
        .expect("valid manifest");
        assert_eq!(
            cases.first().expect("one case").path,
            Path::new("vectors/case/encoded/input.m4a")
        );
        assert!(path_for_key(Path::new("vectors"), "../input.m4a").is_err());
        assert!(path_for_key(Path::new("vectors"), "case/sub/input.m4a").is_err());
        assert!(path_for_key(Path::new("vectors"), "case\\input.m4a").is_err());
    }

    #[test]
    fn presentation_override_is_preserved() {
        let cases = manifest_cases(
            manifest(
                r#"{"entries":{"case/input.m4a":{}},"presentation_overrides":{"case/input.m4a":3}}"#,
            ),
            Path::new("vectors"),
        )
        .expect("valid override");
        assert_eq!(
            cases.first().expect("one case").presentation,
            PresentationSelection::Index(3)
        );
    }

    #[test]
    fn checked_mp4_sample_range_rejects_overflow_and_truncation() {
        assert_eq!(checked_sample_range(4, 3, 8).unwrap(), 4..7);
        assert!(checked_sample_range(7, 2, 8).is_err());
        assert!(checked_sample_range(u64::MAX, 1, 8).is_err());
    }

    #[test]
    fn nearest_rank_uses_the_standard_ceiling_rule() {
        let values: Vec<u64> = (1..=100).collect();
        assert_eq!(nearest_rank(&values, 50).unwrap(), 50);
        assert_eq!(nearest_rank(&values, 95).unwrap(), 95);
        assert_eq!(nearest_rank(&values, 99).unwrap(), 99);
        assert_eq!(nearest_rank(&[7], 99).unwrap(), 7);
        assert!(nearest_rank(&[], 50).is_err());
    }

    #[test]
    fn convergence_requires_both_minimums_but_obeys_the_cap() {
        let settings = TimingSettings {
            cold_runs: 1,
            min_passes: 5,
            min_time: Duration::from_secs(2),
            max_passes: 30,
        };
        assert!(should_continue(4, 3_000_000_000, settings));
        assert!(should_continue(8, 1_000_000_000, settings));
        assert!(!should_continue(8, 3_000_000_000, settings));
        assert!(!should_continue(30, 1, settings));
    }

    #[test]
    fn worst_access_units_are_bounded_sorted_and_keep_source_indices() {
        let measured: Vec<TimedAccessUnit> = (0u32..10)
            .map(|index| TimedAccessUnit {
                pass_index: index / 3,
                access_unit_index: u64::from(100 + index),
                latency_ns: u64::from(index + 1) * 1_000_000,
                codec_samples: 1,
            })
            .collect();
        let summary = deadline_summary(&measured, 1_000).expect("valid deadline samples");

        assert_eq!(summary.misses, 9, "one sample is exactly on its deadline");
        assert_eq!(summary.worst_ratio, 10.0);
        assert_eq!(summary.worst_access_units.len(), WORST_ACCESS_UNIT_LIMIT);
        let first = summary
            .worst_access_units
            .first()
            .expect("bounded summary is non-empty");
        assert_eq!(first.pass_index, 3);
        assert_eq!(first.access_unit_index, 109);
        assert_eq!(first.budget_ns, 1_000_000.0);
        assert!(first.missed_deadline);
        assert_eq!(
            summary
                .worst_access_units
                .last()
                .expect("bounded summary has eight entries")
                .access_unit_index,
            102
        );
        assert!(summary.worst_access_units.windows(2).all(
            |pair| matches!(pair, [left, right] if left.deadline_ratio >= right.deadline_ratio)
        ));

        assert!(deadline_summary(&[], 1_000).is_err());
        assert!(deadline_summary(&measured, 0).is_err());
        let zero_samples = [TimedAccessUnit {
            pass_index: 0,
            access_unit_index: 0,
            latency_ns: 1,
            codec_samples: 0,
        }];
        assert!(deadline_summary(&zero_samples, 48_000).is_err());
    }

    const QMF_SAMPLE_FIXTURE: &str = r#"
Analysis of sampling macinac4-perf every 1 millisecond
Call graph:
    1000 Thread_123 DispatchQueue_1: com.apple.main-thread
      + 800 macindecode_ac4_decode::aspx::qmf::synthesise::hparent
      + ! 20 macindecode_ac4_decode::aspx::qmf::advance_synthesis_state::hstate
      + ! : 500 macindecode_ac4_decode::aspx::qmf::modulate_synthesis_slot::hmod
      + ! : 230 macindecode_ac4_decode::aspx::qmf::accumulate_synthesis_polyphase::htail
      + 100 macindecode_ac4_decode::aspx::qmf::synthesise::hsecond
      + ! 80 macindecode_ac4_decode::aspx::qmf::modulate_synthesis_slot::hsecond
      + ! 20 macindecode_ac4_decode::aspx::qmf::accumulate_synthesis_polyphase::hsecond

Total number in stack (recursive counted multiple, when >=5):
        999 macindecode_ac4_decode::aspx::qmf::modulate_synthesis_slot::hignored
"#;

    #[test]
    fn qmf_sample_summary_uses_inclusive_call_graph_counts_only() {
        let summary = summarize_qmf_sample(
            QMF_SAMPLE_FIXTURE,
            environment_report(false),
            "case/input.m4a".to_owned(),
            BenchmarkMode::Full,
            20_000_000_000,
            3_432,
            1_000,
        )
        .expect("synthetic macOS sample call graph should summarize");

        assert_eq!(summary.profile_thread_samples, 1_000);
        assert_eq!(summary.qmf_synthesis_inclusive_samples, 900);
        assert_eq!(summary.phases.state_advance.inclusive_samples, 20);
        assert_eq!(summary.phases.modulation.inclusive_samples, 580);
        assert_eq!(summary.phases.polyphase_tail.inclusive_samples, 250);
        assert_eq!(summary.phases.unclassified.inclusive_samples, 50);
        assert_eq!(summary.qmf_synthesis_share_of_profile_percent, 90.0);
        assert_eq!(
            summary.phases.modulation.share_of_qmf_synthesis_percent,
            580.0 * 100.0 / 900.0
        );

        assert!(
            summarize_qmf_sample(
                "no call graph",
                environment_report(false),
                "case/input.m4a".to_owned(),
                BenchmarkMode::Core,
                1,
                1,
                1,
            )
            .is_err()
        );
        let impossible = QMF_SAMPLE_FIXTURE.replacen("+ 800", "+ 10", 1);
        assert!(
            summarize_qmf_sample(
                &impossible,
                environment_report(false),
                "case/input.m4a".to_owned(),
                BenchmarkMode::Core,
                1,
                1,
                1,
            )
            .is_err(),
            "三个阶段的 inclusive 样本不得超过父级 synthesis"
        );
    }

    #[test]
    fn reports_have_versioned_required_fields() {
        let timing = TimingReport::new(
            TimingReportSettings {
                manifest: "objects_baseline.json".to_owned(),
                modes: vec![BenchmarkMode::Core, BenchmarkMode::Full],
                cold_runs: 20,
                warmup_passes: 1,
                min_passes: 5,
                min_time_ns: 2_000_000_000,
                max_passes: 30,
                worst_access_unit_limit: WORST_ACCESS_UNIT_LIMIT,
                timed_boundary: "Ac4DecoderSession::decode_access_unit",
            },
            Vec::new(),
        );
        let allocations = AllocationReport::new(
            AllocationReportSettings {
                manifest: "objects_baseline.json".to_owned(),
                modes: vec![BenchmarkMode::Core, BenchmarkMode::Full],
                warmup_passes: 1,
                measured_passes: 1,
                measured_boundary: "Ac4DecoderSession::decode_access_unit",
            },
            Vec::new(),
        );
        let profile = ProfileResult {
            schema: REPORT_SCHEMA,
            schema_version: REPORT_SCHEMA_VERSION,
            kind: "profile",
            environment: environment_report(false),
            settings: ProfileReportSettings {
                warmup_passes: 1,
                startup_delay_ns: 3_000_000_000,
                requested_duration_ns: 30_000_000_000,
                qmf_split_symbols: false,
                ajoc_reconstruction_split_symbols: false,
                loop_boundary: "PreparedCase::decode_pass",
            },
            input: "case/input.m4a".to_owned(),
            mode: BenchmarkMode::Full,
            sample_rate_hz: 48_000,
            access_units_per_pass: 143,
            codec_samples_per_pass: 292_864,
            duration_ns: 30_000_000_000,
            passes: 24,
            access_unit_calls: 3_432,
            consumption: ConsumptionReport {
                frames: 3_432,
                planes: 0,
                samples: 0,
                token: 0,
            },
        };
        let qmf_summary = summarize_qmf_sample(
            QMF_SAMPLE_FIXTURE,
            environment_report(false),
            "case/input.m4a".to_owned(),
            BenchmarkMode::Full,
            20_000_000_000,
            3_432,
            1_000,
        )
        .expect("QMF summary fixture must parse");

        for (report, kind, result_field) in [
            (
                serde_json::to_value(timing).expect("timing report must serialize"),
                "timing",
                "cases",
            ),
            (
                serde_json::to_value(allocations).expect("allocation report must serialize"),
                "allocations",
                "cases",
            ),
            (
                serde_json::to_value(profile).expect("profile report must serialize"),
                "profile",
                "access_unit_calls",
            ),
            (
                serde_json::to_value(qmf_summary).expect("QMF summary must serialize"),
                "qmf_sample_summary",
                "phases",
            ),
        ] {
            assert_eq!(
                report.get("schema"),
                Some(&serde_json::json!(REPORT_SCHEMA))
            );
            assert_eq!(
                report.get("schema_version"),
                Some(&serde_json::json!(REPORT_SCHEMA_VERSION))
            );
            assert_eq!(report.get("kind"), Some(&serde_json::json!(kind)));
            assert!(report.get("environment").is_some());
            assert!(report.get("settings").is_some());
            assert!(report.get(result_field).is_some());
        }

        let steady = serde_json::to_value(SteadyTiming {
            passes: 1,
            calls: 1,
            total_decode_ns: 1,
            realtime_factor: 1.0,
            ns_per_audio_sample: 1.0,
            latency: LatencySummary {
                samples: 1,
                mean_ns: 1.0,
                p50_ns: 1,
                p95_ns: 1,
                p99_ns: 1,
                max_ns: 1,
            },
            worst_deadline_ratio: 0.5,
            deadline_misses: 0,
            worst_access_units: vec![WorstAccessUnitTiming {
                pass_index: 0,
                access_unit_index: 7,
                codec_samples: 2_048,
                latency_ns: 1,
                budget_ns: 42_666_666.666_666_664,
                deadline_ratio: 0.5,
                missed_deadline: false,
            }],
        })
        .expect("steady timing must serialize");
        let worst = steady
            .get("worst_access_units")
            .and_then(serde_json::Value::as_array)
            .expect("steady timing must expose worst access units");
        assert_eq!(worst.len(), 1);
        assert_eq!(
            worst.first().and_then(|item| item.get("access_unit_index")),
            Some(&serde_json::json!(7))
        );
    }
}
