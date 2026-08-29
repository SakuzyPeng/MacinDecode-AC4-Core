//! AC-4 inspection, DAMF/ADM audition probes, and full ADM export tooling.

mod adm;
#[cfg(feature = "audio-decode")]
mod caf;
mod container;
mod corecaf;
mod corepcm;
mod damf;
#[cfg(feature = "audio-decode")]
mod metadata_batch;
#[cfg(feature = "audio-decode")]
mod pcm_batch;
#[cfg(test)]
#[path = "../tests/common/result_schema.rs"]
mod result_schema;
#[cfg(feature = "audio-decode")]
mod scene_batch;
#[cfg(feature = "audio-decode")]
mod scene_export;
mod trace;
mod wire;

use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use macindecode_ac4_inspect::inspect_path;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "macinac4", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Emit an AC-4 container, topology, and syntax trace as JSON.
    Trace {
        /// MP4/M4A or raw AC-4 input.
        input: PathBuf,
    },
    /// Print a human-readable AC-4 bitstream metadata report.
    Inspect {
        /// MP4/M4A or raw AC-4 input.
        input: PathBuf,
        /// Success-output format; diagnostics remain JSON Lines on standard error.
        #[arg(long, value_enum, default_value_t = InspectFormat::Text)]
        format: InspectFormat,
    },
    /// Generate a DAMF audition probe from synthetic pink noise and OAMD metadata.
    ExportDamf(ExportDamfArgs),
    /// Export full A-JOC objects, full OAMD, and optional LFE as a DAMF package.
    ExportFullDamf(ExportFullDamfArgs),
    /// Generate an ADM BWF in a forced 64-bit container from pink noise and OAMD metadata.
    ExportAdmBwf(ExportAdmBwfArgs),
    /// Export full A-JOC objects, full OAMD, and LFE as ADM BW64/RF64.
    ExportFullAdmBwf(ExportFullAdmBwfArgs),
    /// Write a verified fixed core-object grid as Float32 CAF with an Apple speaker layout.
    ExportCoreCaf(ExportCoreCafArgs),
    /// Export core-band A-JOC downmix PCM as EXTENSIBLE 32-bit float WAVE.
    ExportCorePcm(ExportCorePcmArgs),
    /// Export the same PCM after A-SPX bandwidth extension and final QMF synthesis.
    ExportAspxPcm(ExportAspxPcmArgs),
    /// Export final PCM for full A-JOC objects with LFE reinserted.
    ExportObjectsPcm(ExportObjectsPcmArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum InspectFormat {
    Text,
    Json,
}

/// Arguments for `export-objects-pcm`.
#[derive(Debug, Args)]
pub(crate) struct ExportObjectsPcmArgs {
    /// MP4/M4A or raw AC-4 input.
    pub input: PathBuf,

    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    pub presentation: Option<usize>,

    /// New WAVE_FORMAT_EXTENSIBLE 32-bit float file; existing paths are never overwritten.
    ///
    /// Samples retain the internal ±32,768 scale and the MP4 edit list is applied. Channel
    /// order follows the full A-JOC object sequence, with LFE reinserted at the position from
    /// `Pseudocode 15`. `output_channel` in the response matches WAVE interleave order.
    #[arg(short, long)]
    pub output: PathBuf,
}

/// Arguments for `export-aspx-pcm`.
#[derive(Debug, Args)]
pub(crate) struct ExportAspxPcmArgs {
    /// MP4/M4A or raw AC-4 input.
    pub input: PathBuf,

    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    pub presentation: Option<usize>,

    /// New WAVE_FORMAT_EXTENSIBLE 32-bit float file; existing paths are never overwritten.
    ///
    /// Scale, gain, and edit-list handling match `export-core-pcm`. The output additionally
    /// includes A-SPX bandwidth extension, and channel order follows A-JOC input order (see
    /// `Pseudocode 14a`). LFE that bypasses A-JOC is appended last. The response `role`
    /// distinguishes the two; each export has its own regression baseline.
    ///
    /// This is not the final scene: processing stops before A-JOC upmix, so every channel is
    /// still a downmix signal.
    #[arg(short, long)]
    pub output: PathBuf,
}

/// Arguments for `export-core-pcm`.
#[derive(Debug, Args)]
pub(crate) struct ExportCorePcmArgs {
    /// MP4/M4A or raw AC-4 input.
    pub input: PathBuf,

    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    pub presentation: Option<usize>,

    /// New WAVE_FORMAT_EXTENSIBLE 32-bit float file; existing paths are never overwritten.
    ///
    /// Samples retain the decoder's internal ±32,768 scale with no gain adjustment. Each sample
    /// is written to the `data` chunk using `f32::to_bits()`, so the file SHA-256 can serve as a
    /// bit-exact regression baseline. Direct playback is roughly 90 dB too loud; lower the gain
    /// before listening.
    ///
    /// The MP4 edit list is applied, excluding priming and trailing padding from presentation
    /// PCM. This is a core-band reconstruction of the A-JOC downmix, not the final scene:
    /// processing stops before A-SPX bandwidth extension and A-JOC upmix.
    #[arg(short, long)]
    pub output: PathBuf,
}

/// Arguments for `export-core-caf`.
#[derive(Debug, Args)]
pub(crate) struct ExportCoreCafArgs {
    /// MP4/M4A or raw AC-4 input.
    pub input: PathBuf,

    /// New CoreAudio Float32 PCM CAF; existing paths are never overwritten.
    ///
    /// Accepts only verified fixed 5/7/9/11-point core grids with independent LFE and writes the
    /// corresponding 5.1/5.1.2/5.1.4/7.1.4 channel-layout tag. Samples are multiplied by `2^-15`
    /// without normalization or clipping; values outside ±1 are preserved. Handle true peak and
    /// loudness externally.
    #[arg(short, long)]
    pub output: PathBuf,

    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    pub presentation: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DecodeMode {
    Full,
    Core,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DamfPresentationType {
    Home,
    #[value(name = "3dof")]
    ThreeDof,
}

#[cfg(feature = "audio-decode")]
impl DamfPresentationType {
    const fn version(self) -> &'static str {
        match self {
            Self::Home => "0.5.1",
            Self::ThreeDof => "0.6.0",
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::ThreeDof => "3dof",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AdmCompatibility {
    /// Standard ADM BW64 with a nine-digit time reference and no vendor-private metadata.
    Standard,
    /// Logic Pro-compatible RF64 with a five-digit time reference and Dolby `dbmd`.
    Logic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MasterFrameRate {
    #[value(name = "23.976")]
    Fps23976,
    #[value(name = "24")]
    Fps24,
    #[value(name = "25")]
    Fps25,
    #[value(name = "29.97")]
    Fps2997,
    #[value(name = "29.97df")]
    Fps2997Drop,
    #[value(name = "30")]
    Fps30,
}

#[cfg(feature = "audio-decode")]
impl MasterFrameRate {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Fps23976 => "23.976",
            Self::Fps24 => "24",
            Self::Fps25 => "25",
            Self::Fps2997 => "29.97",
            Self::Fps2997Drop => "29.97df",
            Self::Fps30 => "30",
        }
    }

    const fn dbmd_code(self) -> u8 {
        match self {
            Self::Fps23976 => 0x21,
            Self::Fps24 => 0x22,
            Self::Fps25 => 0x23,
            Self::Fps2997 => 0x25,
            Self::Fps2997Drop => 0x24,
            Self::Fps30 => 0x26,
        }
    }
}

#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("selection")
        .required(true)
        .multiple(false)
        .args(["object", "all_objects"])
))]
struct ExportDamfArgs {
    /// MP4/M4A or raw AC-4 input.
    input: PathBuf,
    /// New DAMF package directory; the path must not already exist.
    #[arg(short, long)]
    output: PathBuf,
    /// Object selector; repeat or comma-separate OBJECT or SUBSTREAM:OBJECT values.
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    object: Vec<String>,
    /// Select every dynamic full-range object in the presentation.
    #[arg(long)]
    all_objects: bool,
    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    presentation: Option<usize>,
    /// Select the full or core object set.
    #[arg(long, value_enum, default_value_t = DecodeMode::Full)]
    mode: DecodeMode,
    /// DAMF frame rate.
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// Theoretical peak level of each pink-noise channel, in dBFS.
    #[arg(long, default_value_t = -18.0, allow_hyphen_values = true)]
    probe_level_dbfs: f64,
    /// Base name for the three package files; defaults to the input file name.
    #[arg(long)]
    stem: Option<String>,
    /// Fail when AC-4 metadata cannot be mapped exactly.
    #[arg(long)]
    strict_mapping: bool,
}

/// Arguments for `export-full-damf`.
#[derive(Debug, Args)]
struct ExportFullDamfArgs {
    /// MP4/M4A or raw AC-4 input.
    input: PathBuf,
    /// New DAMF package directory; existing paths are never overwritten.
    #[arg(short, long)]
    output: PathBuf,
    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    presentation: Option<usize>,
    /// DAMF presentation type; 3DoF changes only the manifest declaration.
    #[arg(long, value_enum, default_value_t = DamfPresentationType::Home)]
    presentation_type: DamfPresentationType,
    /// DAMF frame rate.
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// Base name for the three package files; defaults to the input file name.
    #[arg(long)]
    stem: Option<String>,
    /// Fail when AC-4 metadata cannot be mapped exactly.
    #[arg(long)]
    strict_mapping: bool,
}

#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("adm_selection")
        .required(true)
        .multiple(false)
        .args(["object", "all_objects"])
))]
struct ExportAdmBwfArgs {
    /// MP4/M4A or raw AC-4 input.
    input: PathBuf,
    /// New ADM BWF file in a 64-bit container with ds64; usually uses a .wav extension.
    #[arg(short, long)]
    output: PathBuf,
    /// Object selector; repeat or comma-separate OBJECT or SUBSTREAM:OBJECT values.
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    object: Vec<String>,
    /// Select every dynamic full-range object in the presentation.
    #[arg(long)]
    all_objects: bool,
    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    presentation: Option<usize>,
    /// Select the full or core object set.
    #[arg(long, value_enum, default_value_t = DecodeMode::Full)]
    mode: DecodeMode,
    /// Logic DBMD frame rate; ignored for standard BW64 output.
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// Theoretical peak level of each pink-noise channel, in dBFS.
    #[arg(long, default_value_t = -18.0, allow_hyphen_values = true)]
    probe_level_dbfs: f64,
    /// Select standard BW64 or Logic Pro-compatible RF64/dbmd output.
    #[arg(long, value_enum, default_value_t = AdmCompatibility::Standard)]
    compatibility: AdmCompatibility,
    /// Fail when AC-4 metadata cannot be mapped exactly.
    #[arg(long)]
    strict_mapping: bool,
}

/// Arguments for `export-full-adm-bwf`.
#[derive(Debug, Args)]
struct ExportFullAdmBwfArgs {
    /// MP4/M4A or raw AC-4 input.
    input: PathBuf,
    /// New ADM BWF file; existing paths are never overwritten.
    #[arg(short, long)]
    output: PathBuf,
    /// Zero-based presentation index; omitted means auto-select only if exactly one is eligible.
    #[arg(long)]
    presentation: Option<usize>,
    /// Select standard BW64 or Logic Pro-compatible RF64/dbmd output.
    #[arg(long, value_enum, default_value_t = AdmCompatibility::Standard)]
    compatibility: AdmCompatibility,
    /// Logic DBMD frame rate; ignored for standard BW64 output.
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// Fail when AC-4 metadata cannot be mapped exactly.
    #[arg(long)]
    strict_mapping: bool,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            let command = std::env::args().nth(1).unwrap_or_else(|| "cli".to_owned());
            let diagnostic = wire::CliError::new(
                command,
                wire::DiagnosticCode::CliInvalidArguments,
                "Invalid command-line arguments",
            )
            .with_context("detail", error.to_string());
            wire::write_error(&diagnostic);
            return ExitCode::from(2);
        }
    };

    if let Command::Inspect { input, format } = cli.command {
        let result = inspect_path(&input)
            .map_err(wire::inspect_error)
            .and_then(|report| match format {
                InspectFormat::Text => wire::write_inspect_text(&report.render_text()),
                InspectFormat::Json => {
                    wire::prepare_inspect(report).and_then(|success| success.write())
                }
            });
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                wire::write_error(&error);
                ExitCode::FAILURE
            }
        };
    }

    let (command, result) = match cli.command {
        Command::Trace { input } => ("trace", run_trace(&input)),
        Command::Inspect { .. } => unreachable!("inspect is handled before legacy JSON commands"),
        Command::ExportDamf(args) => ("export-damf", damf::run(args)),
        Command::ExportFullDamf(args) => ("export-full-damf", damf::run_full(args)),
        Command::ExportAdmBwf(args) => ("export-adm-bwf", adm::run(args)),
        Command::ExportFullAdmBwf(args) => ("export-full-adm-bwf", adm::run_full(args)),
        Command::ExportCoreCaf(args) => ("export-core-caf", corecaf::run(args)),
        Command::ExportCorePcm(args) => ("export-core-pcm", corepcm::run(args)),
        Command::ExportAspxPcm(args) => ("export-aspx-pcm", corepcm::run_aspx(args)),
        Command::ExportObjectsPcm(args) => ("export-objects-pcm", corepcm::run_objects(args)),
    };
    match result.and_then(|legacy| wire::prepare(command, &legacy)) {
        Ok(success) => match success.write() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                wire::write_error(&error);
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            wire::write_error(&error);
            ExitCode::FAILURE
        }
    }
}

fn run_trace(path: &PathBuf) -> Result<String, wire::CliError> {
    let data = std::fs::read(path).map_err(|error| {
        wire::CliError::new(
            "trace",
            wire::DiagnosticCode::InputReadFailed,
            "Failed to read input file",
        )
        .with_context("path", path.display().to_string())
        .with_context("cause", error.to_string())
    })?;
    if data.is_empty() {
        return Err(wire::CliError::new(
            "trace",
            wire::DiagnosticCode::InputInvalid,
            "Input file is empty",
        )
        .with_context("path", path.display().to_string()));
    }
    trace::trace_input(&data).map_err(|message| {
        wire::CliError::new(
            "trace",
            wire::DiagnosticCode::ParseFailed,
            "Failed to parse AC-4 input",
        )
        .with_context("path", path.display().to_string())
        .with_context("cause", message)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_requires_exactly_one_object_selection_form() {
        assert!(Cli::try_parse_from(["macinac4", "export-damf", "in.m4a", "-o", "out"]).is_err());
        assert!(
            Cli::try_parse_from([
                "macinac4",
                "export-damf",
                "in.m4a",
                "-o",
                "out",
                "--object",
                "2:1",
                "--all-objects",
            ])
            .is_err()
        );
    }

    #[test]
    fn clap_splits_comma_delimited_object_selectors() {
        let parsed = Cli::try_parse_from([
            "macinac4",
            "export-damf",
            "in.m4a",
            "-o",
            "out",
            "--object",
            "2:1,3:2",
        ])
        .expect("合法参数应能解析");
        let Command::ExportDamf(args) = parsed.command else {
            panic!("应解析为 export-damf");
        };
        assert_eq!(args.object, ["2:1", "3:2"]);
    }

    #[test]
    fn clap_parses_core_caf_with_optional_presentation() {
        let parsed = Cli::try_parse_from([
            "macinac4",
            "export-core-caf",
            "in.m4a",
            "-o",
            "out.caf",
            "--presentation",
            "1",
        ])
        .expect("合法 core CAF 参数应能解析");
        let Command::ExportCoreCaf(args) = parsed.command else {
            panic!("应解析为 export-core-caf");
        };
        assert_eq!(args.input, PathBuf::from("in.m4a"));
        assert_eq!(args.output, PathBuf::from("out.caf"));
        assert_eq!(args.presentation, Some(1));
    }

    #[test]
    fn clap_parses_objects_pcm_with_optional_presentation() {
        let automatic =
            Cli::try_parse_from(["macinac4", "export-objects-pcm", "in.m4a", "-o", "out.wav"])
                .expect("省略 presentation 应交给 AutoUnique");
        let Command::ExportObjectsPcm(args) = automatic.command else {
            panic!("应解析为 export-objects-pcm");
        };
        assert_eq!(args.presentation, None);

        let explicit = Cli::try_parse_from([
            "macinac4",
            "export-objects-pcm",
            "in.m4a",
            "-o",
            "out.wav",
            "--presentation",
            "1",
        ])
        .expect("显式 presentation 下标应可解析");
        let Command::ExportObjectsPcm(args) = explicit.command else {
            panic!("应解析为 export-objects-pcm");
        };
        assert_eq!(args.presentation, Some(1));
    }

    #[test]
    fn clap_parses_aspx_pcm_with_optional_presentation() {
        let automatic =
            Cli::try_parse_from(["macinac4", "export-aspx-pcm", "in.m4a", "-o", "out.wav"])
                .expect("省略 presentation 应交给 AutoUnique");
        let Command::ExportAspxPcm(args) = automatic.command else {
            panic!("应解析为 export-aspx-pcm");
        };
        assert_eq!(args.presentation, None);

        let explicit = Cli::try_parse_from([
            "macinac4",
            "export-aspx-pcm",
            "in.m4a",
            "-o",
            "out.wav",
            "--presentation",
            "1",
        ])
        .expect("显式 presentation 下标应可解析");
        let Command::ExportAspxPcm(args) = explicit.command else {
            panic!("应解析为 export-aspx-pcm");
        };
        assert_eq!(args.presentation, Some(1));
    }

    #[test]
    fn clap_parses_core_pcm_with_optional_presentation() {
        let automatic =
            Cli::try_parse_from(["macinac4", "export-core-pcm", "in.m4a", "-o", "out.wav"])
                .expect("省略 presentation 应交给 AutoUnique");
        let Command::ExportCorePcm(args) = automatic.command else {
            panic!("应解析为 export-core-pcm");
        };
        assert_eq!(args.presentation, None);

        let explicit = Cli::try_parse_from([
            "macinac4",
            "export-core-pcm",
            "in.m4a",
            "-o",
            "out.wav",
            "--presentation",
            "1",
        ])
        .expect("显式 presentation 下标应可解析");
        let Command::ExportCorePcm(args) = explicit.command else {
            panic!("应解析为 export-core-pcm");
        };
        assert_eq!(args.presentation, Some(1));
    }

    #[test]
    fn clap_restricts_damf_frame_rates() {
        for fps in ["23.976", "24", "25", "29.97", "29.97df", "30"] {
            assert!(
                Cli::try_parse_from([
                    "macinac4",
                    "export-damf",
                    "in.m4a",
                    "-o",
                    "out",
                    "--object",
                    "2:1",
                    "--fps",
                    fps,
                ])
                .is_ok(),
                "应接受 {fps}"
            );
        }
        assert!(
            Cli::try_parse_from([
                "macinac4",
                "export-damf",
                "in.m4a",
                "-o",
                "out",
                "--object",
                "2:1",
                "--fps",
                "60",
            ])
            .is_err()
        );
    }

    #[test]
    fn clap_parses_full_damf_home_and_3dof_without_object_selection() {
        let home = Cli::try_parse_from([
            "macinac4",
            "export-full-damf",
            "in.m4a",
            "-o",
            "out",
            "--presentation",
            "1",
            "--stem",
            "Full scene",
            "--strict-mapping",
        ])
        .expect("合法 full DAMF 参数应能解析");
        let Command::ExportFullDamf(args) = home.command else {
            panic!("应解析为 export-full-damf");
        };
        assert_eq!(args.presentation, Some(1));
        assert_eq!(args.presentation_type, DamfPresentationType::Home);
        assert_eq!(args.fps, MasterFrameRate::Fps24);
        assert_eq!(args.stem.as_deref(), Some("Full scene"));
        assert!(args.strict_mapping);

        let three_dof = Cli::try_parse_from([
            "macinac4",
            "export-full-damf",
            "in.m4a",
            "-o",
            "out",
            "--presentation-type",
            "3dof",
            "--fps",
            "29.97df",
        ])
        .expect("合法 3DoF full DAMF 参数应能解析");
        let Command::ExportFullDamf(args) = three_dof.command else {
            panic!("应解析为 export-full-damf");
        };
        assert_eq!(args.presentation_type, DamfPresentationType::ThreeDof);
        assert_eq!(args.fps, MasterFrameRate::Fps2997Drop);
        assert!(
            Cli::try_parse_from([
                "macinac4",
                "export-full-damf",
                "in.m4a",
                "-o",
                "out",
                "--object",
                "2:1",
            ])
            .is_err(),
            "full DAMF 固定导出全部对象"
        );
    }

    #[test]
    fn scene_export_help_describes_auto_unique_eligible_selection() {
        for command in [
            "export-core-pcm",
            "export-aspx-pcm",
            "export-objects-pcm",
            "export-damf",
            "export-full-damf",
            "export-adm-bwf",
            "export-full-adm-bwf",
        ] {
            let help = Cli::try_parse_from(["macinac4", command, "--help"])
                .expect_err("--help 应提前退出参数解析")
                .to_string();
            assert!(
                    help.contains(
                        "Zero-based presentation index; omitted means auto-select only if exactly one is eligible"
                    ),
                    "{command} must document AutoUnique eligible-presentation selection: {help}"
                );
        }
    }

    #[test]
    fn clap_parses_adm_bwf_and_requires_one_selection_form() {
        assert!(
            Cli::try_parse_from(["macinac4", "export-adm-bwf", "in.m4a", "-o", "out.wav"]).is_err()
        );
        assert!(
            Cli::try_parse_from([
                "macinac4",
                "export-adm-bwf",
                "in.m4a",
                "-o",
                "out.wav",
                "--object",
                "2:1",
                "--all-objects",
            ])
            .is_err()
        );

        let parsed = Cli::try_parse_from([
            "macinac4",
            "export-adm-bwf",
            "in.m4a",
            "-o",
            "out.wav",
            "--object",
            "2:1,3:2",
        ])
        .expect("合法 ADM BWF 参数应能解析");
        let Command::ExportAdmBwf(args) = parsed.command else {
            panic!("应解析为 export-adm-bwf");
        };
        assert_eq!(args.object, ["2:1", "3:2"]);
        assert_eq!(args.compatibility, AdmCompatibility::Standard);

        let parsed = Cli::try_parse_from([
            "macinac4",
            "export-adm-bwf",
            "in.m4a",
            "-o",
            "out.wav",
            "--all-objects",
            "--compatibility",
            "logic",
            "--fps",
            "29.97df",
        ])
        .expect("Logic 兼容配置应能解析");
        let Command::ExportAdmBwf(args) = parsed.command else {
            panic!("应解析为 export-adm-bwf");
        };
        assert_eq!(args.compatibility, AdmCompatibility::Logic);
        assert_eq!(args.fps, MasterFrameRate::Fps2997Drop);
    }

    #[test]
    fn clap_rejects_removed_core_adm_command() {
        let error =
            Cli::try_parse_from(["macinac4", "export-core-adm-bwf", "in.m4a", "-o", "out.wav"])
                .expect_err("已移除的 core ADM 命令必须被拒绝");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn clap_parses_full_adm_standard_and_logic_without_an_object_selector() {
        let standard = Cli::try_parse_from([
            "macinac4",
            "export-full-adm-bwf",
            "in.m4a",
            "-o",
            "out.wav",
            "--presentation",
            "1",
            "--strict-mapping",
        ])
        .expect("合法 full ADM 参数应能解析");
        let Command::ExportFullAdmBwf(args) = standard.command else {
            panic!("应解析为 export-full-adm-bwf");
        };
        assert_eq!(args.presentation, Some(1));
        assert_eq!(args.compatibility, AdmCompatibility::Standard);
        assert_eq!(args.fps, MasterFrameRate::Fps24);
        assert!(args.strict_mapping);

        let logic = Cli::try_parse_from([
            "macinac4",
            "export-full-adm-bwf",
            "in.m4a",
            "-o",
            "out.wav",
            "--compatibility",
            "logic",
            "--fps",
            "29.97df",
        ])
        .expect("Logic full ADM 参数应能解析");
        let Command::ExportFullAdmBwf(args) = logic.command else {
            panic!("应解析为 export-full-adm-bwf");
        };
        assert_eq!(args.compatibility, AdmCompatibility::Logic);
        assert_eq!(args.fps, MasterFrameRate::Fps2997Drop);
        assert!(
            Cli::try_parse_from([
                "macinac4",
                "export-full-adm-bwf",
                "in.m4a",
                "-o",
                "out.wav",
                "--object",
                "2:1",
            ])
            .is_err(),
            "full ADM v1 固定导出全部对象，不接受对象子集"
        );
    }

    #[cfg(not(feature = "audio-decode"))]
    #[test]
    fn export_entry_explains_required_feature() {
        let args = ExportDamfArgs {
            input: PathBuf::from("unused.ac4"),
            output: PathBuf::from("unused-output"),
            object: vec!["0".to_owned()],
            all_objects: false,
            presentation: None,
            mode: DecodeMode::Full,
            fps: MasterFrameRate::Fps24,
            probe_level_dbfs: -18.0,
            stem: None,
            strict_mapping: false,
        };
        let error = damf::run(args).expect_err("未启用 feature 时必须失败");
        assert!(matches!(error.code, wire::DiagnosticCode::FeatureRequired));
        assert!(error.message.contains("--features audio-decode"));
    }

    #[cfg(not(feature = "audio-decode"))]
    #[test]
    fn full_damf_entry_explains_required_feature() {
        let args = ExportFullDamfArgs {
            input: PathBuf::from("unused.ac4"),
            output: PathBuf::from("unused-output"),
            presentation: None,
            presentation_type: DamfPresentationType::Home,
            fps: MasterFrameRate::Fps24,
            stem: None,
            strict_mapping: false,
        };
        let error = damf::run_full(args).expect_err("未启用 feature 时必须失败");
        assert!(matches!(error.code, wire::DiagnosticCode::FeatureRequired));
        assert!(error.message.contains("--features audio-decode"));
    }

    #[cfg(not(feature = "audio-decode"))]
    #[test]
    fn adm_bwf_entry_explains_required_feature() {
        let args = ExportAdmBwfArgs {
            input: PathBuf::from("unused.ac4"),
            output: PathBuf::from("unused.wav"),
            object: vec!["0".to_owned()],
            all_objects: false,
            presentation: None,
            mode: DecodeMode::Full,
            fps: MasterFrameRate::Fps24,
            probe_level_dbfs: -18.0,
            compatibility: AdmCompatibility::Standard,
            strict_mapping: false,
        };
        let error = adm::run(args).expect_err("未启用 feature 时必须失败");
        assert!(matches!(error.code, wire::DiagnosticCode::FeatureRequired));
        assert!(error.message.contains("--features audio-decode"));
    }

    #[cfg(not(feature = "audio-decode"))]
    #[test]
    fn full_adm_entry_explains_required_feature() {
        let args = ExportFullAdmBwfArgs {
            input: PathBuf::from("unused.ac4"),
            output: PathBuf::from("unused.wav"),
            presentation: None,
            compatibility: AdmCompatibility::Standard,
            fps: MasterFrameRate::Fps24,
            strict_mapping: false,
        };
        let error = adm::run_full(args).expect_err("未启用 feature 时必须失败");
        assert!(matches!(error.code, wire::DiagnosticCode::FeatureRequired));
        assert!(error.message.contains("--features audio-decode"));
    }
}
