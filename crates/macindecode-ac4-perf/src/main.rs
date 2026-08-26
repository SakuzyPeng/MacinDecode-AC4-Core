use clap::{Args, Parser, Subcommand, ValueEnum};
use macindecode_ac4_perf::{
    BenchmarkMode, Consumption, ConsumptionReport, PerfResult, PreparedCase, ProfileReportSettings,
    ProfileResult, REPORT_SCHEMA, REPORT_SCHEMA_VERSION, TimingReport, TimingReportSettings,
    TimingSettings, WORST_ACCESS_UNIT_LIMIT, environment_report, load_manifest, run_timing_case,
    summarize_qmf_sample,
};
use macindecode_ac4_scene::PresentationSelection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::hint::black_box;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[cfg(feature = "allocation-stats")]
use macindecode_ac4_perf::{
    AllocationCaseReport, AllocationReport, AllocationReportSettings, AllocationStatsReport,
};
#[cfg(feature = "allocation-stats")]
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
#[cfg(feature = "allocation-stats")]
use std::alloc::System;

#[cfg(feature = "allocation-stats")]
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[derive(Debug, Parser)]
#[command(name = "macinac4-perf", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Measure cold first-AU and steady-state Session decode latency.
    Timing(TimingArgs),
    /// Count steady-state heap operations after one complete warm-up pass.
    Allocations(AllocationArgs),
    /// Run one prepared input repeatedly so an external sampling profiler can attach.
    Profile(ProfileArgs),
    /// Summarize QMF split symbols from a macOS sample call graph.
    QmfSampleSummary(QmfSampleSummaryArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModeArg {
    Core,
    Full,
}

impl From<ModeArg> for BenchmarkMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Core => Self::Core,
            ModeArg::Full => Self::Full,
        }
    }
}

#[derive(Debug, Args)]
struct BatchArgs {
    /// Baseline manifest whose entries define the exact benchmark corpus.
    #[arg(long, default_value = "vectors/objects_baseline.json")]
    manifest: PathBuf,

    /// Root containing `<case>/encoded/<file>` for manifest keys.
    #[arg(long, default_value = "vectors")]
    vectors_root: PathBuf,

    /// Scene reconstruction modes to measure.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "core,full")]
    modes: Vec<ModeArg>,
}

#[derive(Debug, Args)]
struct TimingArgs {
    #[command(flatten)]
    batch: BatchArgs,

    /// Fresh Session repetitions used for first-AU latency.
    #[arg(long, default_value_t = 20)]
    cold_runs: u32,

    /// Minimum number of complete steady-state passes per case/mode.
    #[arg(long, default_value_t = 5)]
    min_passes: u32,

    /// Minimum accumulated decode time per case/mode.
    #[arg(long, default_value_t = 2.0)]
    min_time_seconds: f64,

    /// Hard cap on complete steady-state passes per case/mode.
    #[arg(long, default_value_t = 30)]
    max_passes: u32,

    /// Pretty-printed JSON destination.
    #[arg(long, default_value = "target/perf/m4-pro-timing.json")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct AllocationArgs {
    #[command(flatten)]
    batch: BatchArgs,

    /// Pretty-printed JSON destination.
    #[arg(long, default_value = "target/perf/m4-pro-allocations.json")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Representative MP4/M4A input, preloaded before sampling begins.
    #[arg(
        long,
        default_value = "vectors/probe_axes_single_object/encoded/master_ac4_1500K.m4a"
    )]
    input: PathBuf,

    /// Relative, non-sensitive input identifier stored in the final JSON.
    #[arg(long, default_value = "probe_axes_single_object/master_ac4_1500K.m4a")]
    label: String,

    /// Scene reconstruction mode to sample.
    #[arg(long, value_enum, default_value = "full")]
    mode: ModeArg,

    /// Explicit zero-based presentation index; omitted means AutoUnique.
    #[arg(long)]
    presentation: Option<u32>,

    /// Time available for an external profiler to attach after PROFILE_READY.
    #[arg(long, default_value_t = 3.0)]
    startup_delay_seconds: f64,

    /// Duration of the repeated decode loop, excluding warm-up and startup delay.
    #[arg(long, default_value_t = 30.0)]
    duration_seconds: f64,

    /// Optional pretty-printed JSON destination; stdout is used when omitted.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct QmfSampleSummaryArgs {
    /// Raw text emitted by macOS `sample` for a qmf-split-profile build.
    #[arg(long)]
    sample: PathBuf,

    /// Profile JSON emitted by the sampled process.
    #[arg(long)]
    profile: PathBuf,

    /// Sampling interval used by macOS `sample`, in microseconds.
    #[arg(long, default_value_t = 1_000)]
    sample_interval_us: u64,

    /// Pretty-printed, path-sanitized JSON destination.
    #[arg(long, default_value = "target/perf/m4-pro-qmf-sample-summary.json")]
    output: PathBuf,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("macinac4-perf: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> PerfResult<()> {
    match cli.command {
        Command::Timing(args) => run_timing(args),
        Command::Allocations(args) => run_allocations(args),
        Command::Profile(args) => run_profile(args),
        Command::QmfSampleSummary(args) => run_qmf_sample_summary(args),
    }
}

fn run_timing(args: TimingArgs) -> PerfResult<()> {
    if cfg!(feature = "allocation-stats") {
        return Err(
            "timing must be built without allocation-stats so allocator instrumentation cannot skew latency"
                .to_owned(),
        );
    }
    if cfg!(any(
        feature = "qmf-split-profile",
        feature = "ajoc-reconstruction-split-profile"
    )) {
        return Err(
            "timing must be built without split-profile features so no-inline markers cannot skew latency"
                .to_owned(),
        );
    }
    let min_time = checked_duration(args.min_time_seconds, "min-time-seconds")?;
    let settings = TimingSettings {
        cold_runs: args.cold_runs,
        min_passes: args.min_passes,
        min_time,
        max_passes: args.max_passes,
    }
    .validate()?;
    let manifest = load_manifest(&args.batch.manifest, &args.batch.vectors_root)?;
    let modes = checked_modes(&args.batch.modes)?;
    let total = manifest
        .len()
        .checked_mul(modes.len())
        .ok_or_else(|| "timing work-item count overflows usize".to_owned())?;
    let mut cases = Vec::with_capacity(total);
    let mut completed = 0usize;
    for item in &manifest {
        let prepared = PreparedCase::load(item)?;
        for &mode in &modes {
            completed = completed.saturating_add(1);
            eprintln!(
                "timing {completed}/{total}: {} {}",
                prepared.key(),
                mode.label()
            );
            cases.push(run_timing_case(&prepared, mode, settings)?);
        }
    }
    let report = TimingReport::new(
        TimingReportSettings {
            manifest: manifest_label(&args.batch.manifest),
            modes,
            cold_runs: settings.cold_runs,
            warmup_passes: 1,
            min_passes: settings.min_passes,
            min_time_ns: u64::try_from(settings.min_time.as_nanos())
                .map_err(|_| "minimum timing duration exceeds u64 nanoseconds".to_owned())?,
            max_passes: settings.max_passes,
            worst_access_unit_limit: WORST_ACCESS_UNIT_LIMIT,
            timed_boundary: "Ac4DecoderSession::decode_access_unit",
        },
        cases,
    );
    write_json(&report, Some(&args.output))
}

fn run_allocations(args: AllocationArgs) -> PerfResult<()> {
    if cfg!(any(
        feature = "qmf-split-profile",
        feature = "ajoc-reconstruction-split-profile"
    )) {
        return Err(
            "allocations must be built without split-profile features so profiling markers stay isolated"
                .to_owned(),
        );
    }
    #[cfg(not(feature = "allocation-stats"))]
    {
        let _ = args;
        Err(
            "allocations requires rebuilding with --features audio-decode,allocation-stats"
                .to_owned(),
        )
    }

    #[cfg(feature = "allocation-stats")]
    {
        let manifest = load_manifest(&args.batch.manifest, &args.batch.vectors_root)?;
        let modes = checked_modes(&args.batch.modes)?;
        let total = manifest
            .len()
            .checked_mul(modes.len())
            .ok_or_else(|| "allocation work-item count overflows usize".to_owned())?;
        let mut cases = Vec::with_capacity(total);
        let mut completed = 0usize;
        for item in &manifest {
            let prepared = PreparedCase::load(item)?;
            for &mode in &modes {
                completed = completed.saturating_add(1);
                eprintln!(
                    "allocations {completed}/{total}: {} {}",
                    prepared.key(),
                    mode.label()
                );
                cases.push(measure_allocations(&prepared, mode)?);
            }
        }
        let report = AllocationReport::new(
            AllocationReportSettings {
                manifest: manifest_label(&args.batch.manifest),
                modes,
                warmup_passes: 1,
                measured_passes: 1,
                measured_boundary: "Ac4DecoderSession::decode_access_unit",
            },
            cases,
        );
        write_json(&report, Some(&args.output))
    }
}

#[cfg(feature = "allocation-stats")]
fn measure_allocations(
    prepared: &PreparedCase,
    mode: BenchmarkMode,
) -> PerfResult<AllocationCaseReport> {
    let mut session = prepared.new_session(mode);
    black_box(prepared.decode_pass(&mut session)?);
    session.reset();

    let region = Region::new(GLOBAL);
    let consumed = prepared.decode_pass(&mut session)?;
    black_box(consumed);
    let measured = region.change();
    let stats = AllocationStatsReport {
        allocations: u64::try_from(measured.allocations)
            .map_err(|_| "allocation count exceeds u64".to_owned())?,
        deallocations: u64::try_from(measured.deallocations)
            .map_err(|_| "deallocation count exceeds u64".to_owned())?,
        reallocations: u64::try_from(measured.reallocations)
            .map_err(|_| "reallocation count exceeds u64".to_owned())?,
        bytes_allocated: u64::try_from(measured.bytes_allocated)
            .map_err(|_| "allocated byte count exceeds u64".to_owned())?,
        bytes_deallocated: u64::try_from(measured.bytes_deallocated)
            .map_err(|_| "deallocated byte count exceeds u64".to_owned())?,
        bytes_reallocated: i64::try_from(measured.bytes_reallocated)
            .map_err(|_| "reallocated byte delta exceeds i64".to_owned())?,
    };
    Ok(AllocationCaseReport {
        input: prepared.key().to_owned(),
        mode,
        sample_rate_hz: prepared.sample_rate(),
        access_units: u64::try_from(prepared.access_unit_count())
            .map_err(|_| "access-unit count exceeds u64".to_owned())?,
        codec_samples: prepared.codec_samples(),
        allocation_free: stats.allocation_free(),
        heap_operation_free: stats.heap_operation_free(),
        stats,
    })
}

fn run_profile(args: ProfileArgs) -> PerfResult<()> {
    if cfg!(feature = "allocation-stats") {
        return Err(
            "profile must be built without allocation-stats so stack samples represent the production allocator"
                .to_owned(),
        );
    }
    let startup_delay = checked_duration(args.startup_delay_seconds, "startup-delay-seconds")?;
    let duration = checked_duration(args.duration_seconds, "duration-seconds")?;
    let mode = BenchmarkMode::from(args.mode);
    let selection = args
        .presentation
        .map(PresentationSelection::Index)
        .unwrap_or(PresentationSelection::AutoUnique);
    let prepared = PreparedCase::load_path(args.label.clone(), &args.input, selection)?;
    let mut session = prepared.new_session(mode);
    black_box(prepared.decode_pass(&mut session)?);
    session.reset();

    eprintln!(
        "PROFILE_READY pid={} input={} mode={} delay_seconds={}",
        std::process::id(),
        prepared.key(),
        mode.label(),
        startup_delay.as_secs_f64()
    );
    io::stderr()
        .flush()
        .map_err(|error| format!("cannot flush profile readiness marker: {error}"))?;
    std::thread::sleep(startup_delay);

    let started = Instant::now();
    let mut passes = 0u64;
    let mut consumption = Consumption::default();
    while started.elapsed() < duration {
        session.reset();
        consumption.merge(prepared.decode_pass(&mut session)?);
        passes = passes
            .checked_add(1)
            .ok_or_else(|| "profile pass count overflows u64".to_owned())?;
    }
    let elapsed = started.elapsed();
    black_box(consumption);
    let access_unit_calls = passes
        .checked_mul(
            u64::try_from(prepared.access_unit_count())
                .map_err(|_| "profile access-unit count exceeds u64".to_owned())?,
        )
        .ok_or_else(|| "profile access-unit call count overflows u64".to_owned())?;
    let result = ProfileResult {
        schema: REPORT_SCHEMA,
        schema_version: REPORT_SCHEMA_VERSION,
        kind: "profile",
        environment: environment_report(false),
        settings: ProfileReportSettings {
            warmup_passes: 1,
            startup_delay_ns: duration_ns(startup_delay)?,
            requested_duration_ns: duration_ns(duration)?,
            qmf_split_symbols: cfg!(feature = "qmf-split-profile"),
            ajoc_reconstruction_split_symbols: cfg!(feature = "ajoc-reconstruction-split-profile"),
            loop_boundary: "PreparedCase::decode_pass",
        },
        input: prepared.key().to_owned(),
        mode,
        sample_rate_hz: prepared.sample_rate(),
        access_units_per_pass: u64::try_from(prepared.access_unit_count())
            .map_err(|_| "profile access-unit count exceeds u64".to_owned())?,
        codec_samples_per_pass: prepared.codec_samples(),
        duration_ns: u64::try_from(elapsed.as_nanos())
            .map_err(|_| "profile duration exceeds u64 nanoseconds".to_owned())?,
        passes,
        access_unit_calls,
        consumption: ConsumptionReport::from(consumption),
    };
    write_json(&result, args.output.as_deref())
}

#[derive(Debug, Deserialize)]
struct ProfileSummaryInput {
    schema: String,
    schema_version: u32,
    kind: String,
    environment: macindecode_ac4_perf::EnvironmentReport,
    settings: ProfileSummarySettingsInput,
    input: String,
    mode: BenchmarkMode,
    duration_ns: u64,
    access_unit_calls: u64,
}

#[derive(Debug, Deserialize)]
struct ProfileSummarySettingsInput {
    qmf_split_symbols: bool,
}

fn run_qmf_sample_summary(args: QmfSampleSummaryArgs) -> PerfResult<()> {
    let profile_bytes = fs::read(&args.profile).map_err(|error| {
        format!(
            "cannot read QMF profile metadata {}: {error}",
            args.profile.display()
        )
    })?;
    let profile: ProfileSummaryInput = serde_json::from_slice(&profile_bytes).map_err(|error| {
        format!(
            "invalid QMF profile metadata {}: {error}",
            args.profile.display()
        )
    })?;
    if profile.schema != REPORT_SCHEMA
        || profile.schema_version != REPORT_SCHEMA_VERSION
        || profile.kind != "profile"
    {
        return Err("QMF sample summary requires a compatible profile JSON".to_owned());
    }
    if !profile.settings.qmf_split_symbols {
        return Err(
            "QMF sample summary requires a profile built with qmf-split-profile".to_owned(),
        );
    }
    let sample = fs::read_to_string(&args.sample).map_err(|error| {
        format!(
            "cannot read macOS sample text {}: {error}",
            args.sample.display()
        )
    })?;
    let summary = summarize_qmf_sample(
        &sample,
        profile.environment,
        profile.input,
        profile.mode,
        profile.duration_ns,
        profile.access_unit_calls,
        args.sample_interval_us,
    )?;
    write_json(&summary, Some(&args.output))
}

fn duration_ns(duration: Duration) -> PerfResult<u64> {
    u64::try_from(duration.as_nanos()).map_err(|_| "duration exceeds u64 nanoseconds".to_owned())
}

fn checked_duration(seconds: f64, label: &str) -> PerfResult<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(format!("{label} must be a finite positive number"));
    }
    Duration::try_from_secs_f64(seconds)
        .map_err(|error| format!("{label} cannot be represented: {error}"))
}

fn checked_modes(values: &[ModeArg]) -> PerfResult<Vec<BenchmarkMode>> {
    if values.is_empty() {
        return Err("at least one mode is required".to_owned());
    }
    let mut modes = Vec::with_capacity(values.len());
    for &value in values {
        let mode = BenchmarkMode::from(value);
        if !modes.contains(&mode) {
            modes.push(mode);
        }
    }
    Ok(modes)
}

fn manifest_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("objects_baseline.json")
        .to_owned()
}

fn write_json<T: Serialize>(value: &T, output: Option<&Path>) -> PerfResult<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("cannot serialize performance JSON: {error}"))?;
    bytes.push(b'\n');
    if let Some(path) = output {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "cannot create performance output {}: {error}",
                    parent.display()
                )
            })?;
        }
        fs::write(path, bytes)
            .map_err(|error| format!("cannot write performance JSON {}: {error}", path.display()))
    } else {
        io::stdout()
            .write_all(&bytes)
            .map_err(|error| format!("cannot write performance JSON to stdout: {error}"))
    }
}
