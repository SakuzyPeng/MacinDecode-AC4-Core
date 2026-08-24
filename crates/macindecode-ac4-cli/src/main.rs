//! AC-4 检视、DAMF/ADM 试听探针与真实 full ADM 导出工具。

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
    /// 输出 AC-4 容器、拓扑与语法 trace JSON。
    Trace {
        /// MP4/M4A 或裸 AC-4 输入。
        input: PathBuf,
    },
    /// 用合成粉红噪声和 OAMD 元数据生成 DAMF 试听探针。
    ExportDamf(ExportDamfArgs),
    /// 把 full A-JOC 重建对象、full OAMD 与可选 LFE 导出为真实 DAMF。
    ExportFullDamf(ExportFullDamfArgs),
    /// 用合成粉红噪声和 OAMD 元数据直接生成强制 64 位容器的 ADM BWF。
    ExportAdmBwf(ExportAdmBwfArgs),
    /// 把 full A-JOC 重建对象、full OAMD 与 LFE 导出为 ADM BW64/RF64。
    ExportFullAdmBwf(ExportFullAdmBwfArgs),
    /// 把已验证的固定 core 对象网格直接写成带 Apple 扬声器布局的 Float32 CAF。
    ExportCoreCaf(ExportCoreCafArgs),
    /// 导出 A-JOC 下混信号的核心带 PCM，EXTENSIBLE 32 位浮点 WAVE。
    ExportCorePcm(ExportCorePcmArgs),
    /// 同上，但再走一段 A-SPX 带宽扩展与终端 QMF 合成。
    ExportAspxPcm(ExportAspxPcmArgs),
    /// 导出 full A-JOC 重建对象及插回 LFE 的最终 PCM。
    ExportObjectsPcm(ExportObjectsPcmArgs),
}

/// `export-objects-pcm` 的参数。
#[derive(Debug, Args)]
pub(crate) struct ExportObjectsPcmArgs {
    /// MP4/M4A 或裸 AC-4 输入。
    pub input: PathBuf,

    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
    #[arg(long)]
    pub presentation: Option<usize>,

    /// 新建的 WAVE_FORMAT_EXTENSIBLE 32 位浮点文件；不会覆盖已有路径。
    ///
    /// 样本保持内部 ±32 768 量级并应用 MP4 edit list。逐路顺序是 full A-JOC
    /// 对象序列，LFE 按 `Pseudocode 15` 的位置插回；响应中的 `output_channel`
    /// 与 WAVE 交织顺序相同。
    #[arg(short, long)]
    pub output: PathBuf,
}

/// `export-aspx-pcm` 的参数。
#[derive(Debug, Args)]
pub(crate) struct ExportAspxPcmArgs {
    /// MP4/M4A 或裸 AC-4 输入。
    pub input: PathBuf,

    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
    #[arg(long)]
    pub presentation: Option<usize>,

    /// 新建的 WAVE_FORMAT_EXTENSIBLE 32 位浮点文件；不会覆盖已有路径。
    ///
    /// 与 `export-core-pcm` 的量级、缩放与 edit list 处理完全相同，差别只有两处：
    /// 内容多了 A-SPX 带宽扩展，**逐路顺序是 A-JOC 的输入顺序**（见
    /// `Pseudocode 14a`），不进入 A-JOC 的 LFE 单独排在最后。响应中的
    /// `role` 会把两者区分开；两条导出各自冻结基线，不可混比。
    ///
    /// 仍**不是最终场景**：本命令停在 A-JOC 上混之前，这里的每一路都还是下混信号。
    #[arg(short, long)]
    pub output: PathBuf,
}

/// `export-core-pcm` 的参数。
#[derive(Debug, Args)]
pub(crate) struct ExportCorePcmArgs {
    /// MP4/M4A 或裸 AC-4 输入。
    pub input: PathBuf,

    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
    #[arg(long)]
    pub presentation: Option<usize>,

    /// 新建的 WAVE_FORMAT_EXTENSIBLE 32 位浮点文件；不会覆盖已有路径。
    ///
    /// 样本保持解码器内部的 ±32 768 量级，**不做缩放**；`data` 块逐样本直接写
    /// f32::to_bits()，整个文件的 SHA-256 可直接当逐位回归基线。代价是直接播放
    /// 会响约 90 dB，试听时请先降增益。
    ///
    /// MP4 edit list 会应用到输出，因而 priming 与尾部 padding 不进入呈现 PCM。
    /// 内容是 A-JOC 下混信号的核心带重建，**不是最终场景**：本命令停在 A-SPX
    /// 带宽扩展与 A-JOC 上混之前。
    #[arg(short, long)]
    pub output: PathBuf,
}

/// `export-core-caf` 的参数。
#[derive(Debug, Args)]
pub(crate) struct ExportCoreCafArgs {
    /// MP4/M4A 或裸 AC-4 输入。
    pub input: PathBuf,

    /// 新建的 CoreAudio Float32 PCM CAF；不会覆盖已有路径。
    ///
    /// 仅接受已验证的固定 5/7/9/11 点 core 网格与独立 LFE，自动写入
    /// 5.1/5.1.2/5.1.4/7.1.4 channel layout tag。样本固定乘 `2^-15`，
    /// 不归一化、不限幅；超过 ±1 的浮点样本原样保留，真峰值与响度请在外部处理。
    #[arg(short, long)]
    pub output: PathBuf,

    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
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
    /// 标准 ADM BW64、九位时钟，不加入厂商私有元数据。
    Standard,
    /// Logic Pro 兼容 RF64、五位时钟及 Dolby `dbmd`。
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
    /// MP4/M4A 或裸 AC-4 输入。
    input: PathBuf,
    /// 新建的 DAMF 包目录；该路径必须不存在。
    #[arg(short, long)]
    output: PathBuf,
    /// 对象选择器，可重复或逗号分隔：OBJECT 或 SUBSTREAM:OBJECT。
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    object: Vec<String>,
    /// 选择 presentation 内全部动态全频对象。
    #[arg(long)]
    all_objects: bool,
    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
    #[arg(long)]
    presentation: Option<usize>,
    /// 选择 full 或 core 对象集合。
    #[arg(long, value_enum, default_value_t = DecodeMode::Full)]
    mode: DecodeMode,
    /// DAMF 帧率。
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// 每路粉红噪声的理论峰值，单位 dBFS。
    #[arg(long, default_value_t = -18.0, allow_hyphen_values = true)]
    probe_level_dbfs: f64,
    /// 三件套文件名主干；默认使用输入文件名。
    #[arg(long)]
    stem: Option<String>,
    /// 发现无法精确映射的 AC-4 元数据时失败。
    #[arg(long)]
    strict_mapping: bool,
}

/// `export-full-damf` 的参数。
#[derive(Debug, Args)]
struct ExportFullDamfArgs {
    /// MP4/M4A 或裸 AC-4 输入。
    input: PathBuf,
    /// 新建的 DAMF package 目录；不会覆盖已有路径。
    #[arg(short, long)]
    output: PathBuf,
    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
    #[arg(long)]
    presentation: Option<usize>,
    /// DAMF presentation 类型；3DoF 只改变 manifest 声明。
    #[arg(long, value_enum, default_value_t = DamfPresentationType::Home)]
    presentation_type: DamfPresentationType,
    /// DAMF 帧率。
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// 三件套文件名主干；默认使用输入文件名。
    #[arg(long)]
    stem: Option<String>,
    /// 发现无法精确映射的 AC-4 元数据时失败。
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
    /// MP4/M4A 或裸 AC-4 输入。
    input: PathBuf,
    /// 新建的 ADM BWF 文件；始终使用带 ds64 的 64 位容器，通常使用 .wav 扩展名。
    #[arg(short, long)]
    output: PathBuf,
    /// 对象选择器，可重复或逗号分隔：OBJECT 或 SUBSTREAM:OBJECT。
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    object: Vec<String>,
    /// 选择 presentation 内全部动态全频对象。
    #[arg(long)]
    all_objects: bool,
    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
    #[arg(long)]
    presentation: Option<usize>,
    /// 选择 full 或 core 对象集合。
    #[arg(long, value_enum, default_value_t = DecodeMode::Full)]
    mode: DecodeMode,
    /// Logic DBMD 帧率；标准 BW64 不使用该值。
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// 每路粉红噪声的理论峰值，单位 dBFS。
    #[arg(long, default_value_t = -18.0, allow_hyphen_values = true)]
    probe_level_dbfs: f64,
    /// ADM programme/content 名称；默认使用输入文件名。
    #[arg(long)]
    name: Option<String>,
    /// 选择标准 BW64 或 Logic Pro 兼容 RF64/dbmd 输出。
    #[arg(long, value_enum, default_value_t = AdmCompatibility::Standard)]
    compatibility: AdmCompatibility,
    /// 发现无法精确映射的 AC-4 元数据时失败。
    #[arg(long)]
    strict_mapping: bool,
}

/// `export-full-adm-bwf` 的参数。
#[derive(Debug, Args)]
struct ExportFullAdmBwfArgs {
    /// MP4/M4A 或裸 AC-4 输入。
    input: PathBuf,
    /// 新建的 ADM BWF 文件；不会覆盖已有路径。
    #[arg(short, long)]
    output: PathBuf,
    /// 零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择。
    #[arg(long)]
    presentation: Option<usize>,
    /// ADM programme/content 名称；默认使用输入文件名。
    #[arg(long)]
    name: Option<String>,
    /// 选择标准 BW64 或 Logic Pro 兼容 RF64/dbmd 输出。
    #[arg(long, value_enum, default_value_t = AdmCompatibility::Standard)]
    compatibility: AdmCompatibility,
    /// Logic DBMD 帧率；标准 BW64 不使用该值。
    #[arg(long, value_enum, default_value = "24")]
    fps: MasterFrameRate,
    /// 发现无法精确映射的 AC-4 元数据时失败。
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
                "命令行参数无效",
            )
            .with_context("detail", error.to_string());
            wire::write_error(&diagnostic);
            return ExitCode::from(2);
        }
    };

    let (command, result) = match cli.command {
        Command::Trace { input } => ("trace", run_trace(&input)),
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
            "无法读取输入文件",
        )
        .with_context("path", path.display().to_string())
        .with_context("cause", error.to_string())
    })?;
    if data.is_empty() {
        return Err(wire::CliError::new(
            "trace",
            wire::DiagnosticCode::InputInvalid,
            "输入文件为空",
        )
        .with_context("path", path.display().to_string()));
    }
    trace::trace_input(&data).map_err(|message| {
        wire::CliError::new(
            "trace",
            wire::DiagnosticCode::ParseFailed,
            "解析 AC-4 输入失败",
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
                    "零基 presentation 下标；省略时仅在 eligible presentation 唯一时自动选择"
                ),
                "{command} 必须公开 AutoUnique 的 eligible 选择语义：{help}"
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
            "--name",
            "OAMD probe",
        ])
        .expect("合法 ADM BWF 参数应能解析");
        let Command::ExportAdmBwf(args) = parsed.command else {
            panic!("应解析为 export-adm-bwf");
        };
        assert_eq!(args.object, ["2:1", "3:2"]);
        assert_eq!(args.name.as_deref(), Some("OAMD probe"));
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
            "--name",
            "Full scene",
            "--strict-mapping",
        ])
        .expect("合法 full ADM 参数应能解析");
        let Command::ExportFullAdmBwf(args) = standard.command else {
            panic!("应解析为 export-full-adm-bwf");
        };
        assert_eq!(args.presentation, Some(1));
        assert_eq!(args.name.as_deref(), Some("Full scene"));
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
            name: None,
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
            name: None,
            compatibility: AdmCompatibility::Standard,
            fps: MasterFrameRate::Fps24,
            strict_mapping: false,
        };
        let error = adm::run_full(args).expect_err("未启用 feature 时必须失败");
        assert!(matches!(error.code, wire::DiagnosticCode::FeatureRequired));
        assert!(error.message.contains("--features audio-decode"));
    }
}
