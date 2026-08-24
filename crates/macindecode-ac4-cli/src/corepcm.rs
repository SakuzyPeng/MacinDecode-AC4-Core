//! A-JOC 音频链三层 PCM 的导出。
//!
//! **这不是最终场景。** 核心带命令交出 `var_channel_element()` 解出的
//! 下混信号；A-SPX 命令另为已确认子集补齐带宽；objects 命令继续执行 full
//! A-JOC 矩阵、LFE 插回与对象终端合成。三层各自冻结数值回归基线。
//!
//! 写 WAVE_FORMAT_EXTENSIBLE 32 位浮点而非整数：Scene batch 从按需启用的
//! normalized 核心带侧车取样，再精确乘 `2^15` 恢复解码器内部的 `±32 768` 量级。
//! writer 不再缩放或取整，因此 `data` 块是恢复后 `f32::to_bits()` 的直接转写，
//! 其 SHA-256 就是整条链路的逐位基线。声道掩码使用 DIRECTOUT（0），
//! 因为这些是带来源描述的诊断信号或对象，不是预先分配给扬声器的位置。代价是直接播放
//! 会很响，见 `--help` 的说明。

#[cfg(feature = "audio-decode")]
use crate::pcm_batch::{PcmBatch, PcmTrackSource};
use crate::wire::{CliError, DiagnosticCode};
use crate::{ExportAspxPcmArgs, ExportCorePcmArgs, ExportObjectsPcmArgs};

const COMMAND: &str = "export-core-pcm";
const ASPX_COMMAND: &str = "export-aspx-pcm";
const OBJECTS_COMMAND: &str = "export-objects-pcm";

fn cli_error(code: DiagnosticCode, message: impl Into<String>) -> CliError {
    CliError::new(COMMAND, code, message)
}

fn aspx_error(code: DiagnosticCode, message: impl Into<String>) -> CliError {
    CliError::new(ASPX_COMMAND, code, message)
}

fn objects_error(code: DiagnosticCode, message: impl Into<String>) -> CliError {
    CliError::new(OBJECTS_COMMAND, code, message)
}

#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run(_args: ExportCorePcmArgs) -> Result<String, CliError> {
    Err(cli_error(
        DiagnosticCode::FeatureRequired,
        "export-core-pcm 需要以 --features audio-decode 重新构建 macinac4",
    ))
}

#[cfg(feature = "audio-decode")]
pub(crate) fn run(args: ExportCorePcmArgs) -> Result<String, CliError> {
    use crate::scene_batch::collect_core_pcm;
    use macindecode_ac4_scene::PresentationSelection;
    use std::fs;

    ensure_output_absent_for(COMMAND, &args.output)?;
    let data = fs::read(&args.input).map_err(|error| {
        cli_error(
            DiagnosticCode::InputReadFailed,
            format!("读取输入失败：{error}"),
        )
    })?;
    let selection = match args.presentation {
        None => PresentationSelection::AutoUnique,
        Some(index) => PresentationSelection::Index(u32::try_from(index).map_err(|_| {
            cli_error(
                DiagnosticCode::SelectionInvalid,
                "presentation 下标超出 u32",
            )
        })?),
    };
    let pcm = collect_core_pcm(&data, selection).map_err(core_batch_error)?;
    let (frames, channels) = layout(&pcm)
        .map_err(|message| cli_error(DiagnosticCode::InternalInvariantFailed, message))?;
    write_atomic_wave_for(COMMAND, &args.output, &pcm, frames, channels)?;

    let descriptors = pcm
        .tracks
        .iter()
        .map(|item| match item.source {
            PcmTrackSource::TransportChannel {
                element_index,
                channel_index,
            } => Ok(format!(
                "{{\"substream\": {}, \"element\": {element_index}, \"channel\": {channel_index}}}",
                item.substream_index
            )),
            _ => Err(cli_error(
                DiagnosticCode::InternalInvariantFailed,
                format!(
                    "核心带导出的 substream {} 输出 {} 缺少传输声道来源",
                    item.substream_index, item.output_index
                ),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let file = json_quote(&args.output.display().to_string());
    Ok(format!(
        "{{\"file\": {file}, \"format\": \"wave_extensible_ieee_float32\", \"sample_rate\": {}, \
         \"channels\": {channels}, \"frames\": {frames}, \"scale\": \"±32768\", \
         \"bandwidth\": \"core_only\", \"tracks\": [{descriptors}]}}",
        pcm.sample_rate,
    ))
}

/// 未启用 `audio-decode` 时的 `export-aspx-pcm`。
#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run_aspx(_args: ExportAspxPcmArgs) -> Result<String, CliError> {
    Err(aspx_error(
        DiagnosticCode::FeatureRequired,
        "export-aspx-pcm 需要以 --features audio-decode 重新构建 macinac4",
    ))
}

/// 未启用 `audio-decode` 时的 `export-objects-pcm`。
#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run_objects(_args: ExportObjectsPcmArgs) -> Result<String, CliError> {
    Err(objects_error(
        DiagnosticCode::FeatureRequired,
        "export-objects-pcm 需要以 --features audio-decode 重新构建 macinac4",
    ))
}

/// 带宽扩展后的下混信号 PCM。
///
/// 与 [`run`] 共用形状校验与 WAVE 写出；差别只有采集阶段与两个自述字段——
/// `bandwidth` 记为 `aspx`，`channel_order` 记为 `ajoc_input_then_lfe`。逐路
/// `role` 再区分 A-JOC 输入与不进入 A-JOC 的 LFE，好让基线不会把两者
/// 混为同一种下标语义。
#[cfg(feature = "audio-decode")]
pub(crate) fn run_aspx(args: ExportAspxPcmArgs) -> Result<String, CliError> {
    use crate::scene_batch::collect_aspx_pcm;
    use macindecode_ac4_scene::PresentationSelection;
    use std::fs;

    ensure_output_absent_for(ASPX_COMMAND, &args.output)?;
    let data = fs::read(&args.input).map_err(|error| {
        aspx_error(
            DiagnosticCode::InputReadFailed,
            format!("读取输入失败：{error}"),
        )
    })?;
    let selection = match args.presentation {
        None => PresentationSelection::AutoUnique,
        Some(index) => PresentationSelection::Index(u32::try_from(index).map_err(|_| {
            aspx_error(
                DiagnosticCode::SelectionInvalid,
                "presentation 下标超出 u32",
            )
        })?),
    };
    let pcm = collect_aspx_pcm(&data, selection).map_err(aspx_batch_error)?;
    let (frames, channels) = layout(&pcm)
        .map_err(|message| aspx_error(DiagnosticCode::InternalInvariantFailed, message))?;
    write_atomic_wave_for(ASPX_COMMAND, &args.output, &pcm, frames, channels)?;

    let descriptors = aspx_descriptors(&pcm)?;
    let file = json_quote(&args.output.display().to_string());
    Ok(format!(
        "{{\"file\": {file}, \"format\": \"wave_extensible_ieee_float32\", \"sample_rate\": {}, \
         \"channels\": {channels}, \"frames\": {frames}, \"scale\": \"±32768\", \
         \"bandwidth\": \"aspx\", \"channel_order\": \"ajoc_input_then_lfe\", \
         \"tracks\": [{descriptors}]}}",
        pcm.sample_rate,
    ))
}

/// full A-JOC 对象 PCM，LFE 已按码流声明的位置插回。
#[cfg(feature = "audio-decode")]
pub(crate) fn run_objects(args: ExportObjectsPcmArgs) -> Result<String, CliError> {
    use crate::scene_batch::collect_objects_pcm;
    use macindecode_ac4_scene::PresentationSelection;
    use std::fs;

    ensure_output_absent_for(OBJECTS_COMMAND, &args.output)?;
    let data = fs::read(&args.input).map_err(|error| {
        objects_error(
            DiagnosticCode::InputReadFailed,
            format!("读取输入失败：{error}"),
        )
    })?;
    let selection = match args.presentation {
        None => PresentationSelection::AutoUnique,
        Some(index) => PresentationSelection::Index(u32::try_from(index).map_err(|_| {
            objects_error(
                DiagnosticCode::SelectionInvalid,
                "presentation 下标超出 u32",
            )
        })?),
    };
    let pcm = collect_objects_pcm(&data, selection).map_err(objects_batch_error)?;
    let (frames, channels) = layout(&pcm)
        .map_err(|message| objects_error(DiagnosticCode::InternalInvariantFailed, message))?;
    let descriptors = objects_descriptors(&pcm)?;
    write_atomic_wave_for(OBJECTS_COMMAND, &args.output, &pcm, frames, channels)?;

    let file = json_quote(&args.output.display().to_string());
    Ok(format!(
        "{{\"file\": {file}, \"format\": \"wave_extensible_ieee_float32\", \"sample_rate\": {}, \
         \"channels\": {channels}, \"frames\": {frames}, \"scale\": \"±32768\", \
         \"bandwidth\": \"aspx\", \"channel_order\": \"ajoc_objects_with_lfe_reinserted\", \
         \"tracks\": [{descriptors}]}}",
        pcm.sample_rate,
    ))
}

/// A-SPX 出口的选择、合法未支持分支与内部不变量使用各自的稳定诊断码。
#[cfg(feature = "audio-decode")]
fn aspx_batch_error(error: crate::scene_batch::SceneBatchError) -> CliError {
    scene_batch_error(ASPX_COMMAND, error)
}

/// 核心带出口同样消费 Scene Session 的结构化失败分类。
#[cfg(feature = "audio-decode")]
fn core_batch_error(error: crate::scene_batch::SceneBatchError) -> CliError {
    scene_batch_error(COMMAND, error)
}

#[cfg(feature = "audio-decode")]
fn scene_batch_error(
    command: &'static str,
    error: crate::scene_batch::SceneBatchError,
) -> CliError {
    use crate::scene_batch::SceneBatchError;

    match error {
        SceneBatchError::Selection(message) => {
            CliError::new(command, DiagnosticCode::SelectionInvalid, message)
        }
        SceneBatchError::Unsupported {
            message,
            scene_path,
        } => {
            let error = CliError::new(command, DiagnosticCode::UnsupportedCodingPath, message);
            match scene_path {
                Some(path) => error.with_context("scene_path", path.label()),
                None => error,
            }
        }
        SceneBatchError::Invariant(message) => {
            CliError::new(command, DiagnosticCode::InternalInvariantFailed, message)
        }
        SceneBatchError::Failed(message) => {
            CliError::new(command, DiagnosticCode::ParseFailed, message)
        }
    }
}

/// 对象出口的合法未支持分支与内部不变量必须使用不同稳定诊断码。
#[cfg(feature = "audio-decode")]
fn objects_batch_error(error: crate::scene_batch::SceneBatchError) -> CliError {
    scene_batch_error(OBJECTS_COMMAND, error)
}

/// 给带宽扩展导出的混合顺序写出无歧义的逐路来源。
#[cfg(feature = "audio-decode")]
fn aspx_descriptors(pcm: &PcmBatch) -> Result<String, CliError> {
    pcm.tracks
        .iter()
        .map(|item| match item.source {
            PcmTrackSource::AjocInput { input_index } => Ok(format!(
                "{{\"substream\": {}, \"role\": \"ajoc_input\", \"ajoc_input\": {}}}",
                item.substream_index, input_index
            )),
            PcmTrackSource::Lfe => Ok(format!(
                "{{\"substream\": {}, \"role\": \"lfe\"}}",
                item.substream_index
            )),
            PcmTrackSource::TransportChannel { .. } | PcmTrackSource::AjocObject { .. } => {
                Err(aspx_error(
                    DiagnosticCode::InternalInvariantFailed,
                    format!(
                        "带宽扩展导出的 substream {} 第 {} 路带有错误的来源语义",
                        item.substream_index, item.output_index
                    ),
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| items.join(", "))
}

/// 给最终对象序列写出对象下标与交织输出位置；LFE 只占输出位置。
#[cfg(feature = "audio-decode")]
fn objects_descriptors(pcm: &PcmBatch) -> Result<String, CliError> {
    pcm.tracks
        .iter()
        .map(|item| match item.source {
            PcmTrackSource::AjocObject { object_index } => Ok(format!(
                "{{\"substream\": {}, \"role\": \"ajoc_object\", \"ajoc_object\": {object}, \
                 \"output_channel\": {}}}",
                item.substream_index,
                item.output_index,
                object = object_index,
            )),
            PcmTrackSource::Lfe => Ok(format!(
                "{{\"substream\": {}, \"role\": \"lfe\", \"output_channel\": {}}}",
                item.substream_index, item.output_index
            )),
            PcmTrackSource::TransportChannel { .. } | PcmTrackSource::AjocInput { .. } => {
                Err(objects_error(
                    DiagnosticCode::InternalInvariantFailed,
                    format!(
                        "对象导出的 substream {} 输出 {} 仍带下混侧来源语义",
                        item.substream_index, item.output_index
                    ),
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| items.join(", "))
}

/// 目标必须不存在；这既防止覆盖已有导出，也挡住 `input == output` 的源文件截断。
#[cfg(feature = "audio-decode")]
fn ensure_output_absent_for(command: &str, output: &std::path::Path) -> Result<(), CliError> {
    match std::fs::symlink_metadata(output) {
        Ok(_) => Err(CliError::new(
            command,
            DiagnosticCode::OutputExists,
            format!("输出路径已存在：{}", output.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CliError::new(
            command,
            DiagnosticCode::OutputCreateFailed,
            format!("检查输出路径失败：{error}"),
        )),
    }
}

/// 先在目标目录完整写入临时文件，再发布最终路径；失败时不留下半成品。
#[cfg(feature = "audio-decode")]
fn write_atomic_wave_for(
    command: &str,
    output: &std::path::Path,
    pcm: &PcmBatch,
    frames: usize,
    channels: usize,
) -> Result<(), CliError> {
    use std::fs::{self, OpenOptions};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};

    fn output_parent(output: &Path) -> &Path {
        output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    fn create_temp_file(output: &Path) -> Result<(PathBuf, std::fs::File), String> {
        let parent = output_parent(output);
        let base = output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("core.wav");
        for attempt in 0..100u32 {
            let candidate = parent.join(format!(".{base}.tmp-{}-{attempt}", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => return Ok((candidate, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("创建临时 PCM 失败：{error}")),
            }
        }
        Err("无法分配临时 PCM 文件".to_owned())
    }

    let (temp, file) = create_temp_file(output)
        .map_err(|message| CliError::new(command, DiagnosticCode::OutputCreateFailed, message))?;
    let result = (|| {
        let mut writer = BufWriter::new(file);
        write_wave(&mut writer, pcm, frames, channels).map_err(|message| {
            CliError::new(command, DiagnosticCode::OutputWriteFailed, message)
        })?;
        writer.flush().map_err(|error| {
            CliError::new(
                command,
                DiagnosticCode::OutputWriteFailed,
                format!("写出失败：{error}"),
            )
        })?;
        drop(writer);
        ensure_output_absent_for(command, output)?;
        fs::rename(&temp, output).map_err(|error| {
            CliError::new(
                command,
                DiagnosticCode::OutputCommitFailed,
                format!("提交 PCM 失败：{error}"),
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(feature = "audio-decode")]
fn json_quote(value: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            value if value.is_control() => {
                write!(&mut out, "\\u{:04x}", u32::from(value)).expect("写入 String 不会失败");
            }
            value => out.push(value),
        }
    }
    out.push('\"');
    out
}

/// 交织前的形状校验：所有声道必须等长，且至少有一个声道。
///
/// 声道等长不是自然成立的——某个声道帧合成失败就会短一帧。Session 的逐 AU
/// 事务已拒绝半帧，这里仍再核一次，因为交织本身没有对齐信息可依。
#[cfg(feature = "audio-decode")]
fn layout(pcm: &PcmBatch) -> Result<(usize, usize), String> {
    let Some(first) = pcm.tracks.first() else {
        return Err("解码结果不含任何声道".to_owned());
    };
    let frames = first.samples.len();
    if frames == 0 {
        return Err("解码结果不含任何样本".to_owned());
    }
    for item in &pcm.tracks {
        if item.samples.len() != frames {
            return Err(format!(
                "声道长度不一致：substream {} 输出 {} 有 {} 个样本，首个声道有 {frames} 个",
                item.substream_index,
                item.output_index,
                item.samples.len()
            ));
        }
        if let Some(sample) = item.samples.iter().position(|value| !value.is_finite()) {
            return Err(format!(
                "PCM 含非有限样本：substream {} 输出 {} 样本 {sample}",
                item.substream_index, item.output_index
            ));
        }
    }
    Ok((frames, pcm.tracks.len()))
}

/// 写出 32 位浮点 WAVE。
///
/// 使用 40 字节 WAVEFORMATEXTENSIBLE `fmt `、`fact` 与 `data`。DIRECTOUT 声道
/// 掩码保留 `(substream, element, channel)` 的原始顺序，不把解码器内部信号伪装
/// 成扬声器布局。本项目的向量远低于 4 GiB，不需要 RF64/BW64；超限时显式失败。
#[cfg(feature = "audio-decode")]
fn write_wave<W: std::io::Write>(
    writer: &mut W,
    pcm: &PcmBatch,
    frames: usize,
    channels: usize,
) -> Result<(), String> {
    const BYTES_PER_SAMPLE: usize = 4;
    const FORMAT_EXTENSIBLE: u16 = 0xfffe;
    const SUBFORMAT_IEEE_FLOAT: [u8; 16] = [
        0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];

    let channels_u16 = u16::try_from(channels).map_err(|_| "声道数超出 u16")?;
    let block_align = channels
        .checked_mul(BYTES_PER_SAMPLE)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or("blockAlign 超出 u16")?;
    let data_size = frames
        .checked_mul(usize::from(block_align))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("data 块超出 32 位 WAVE 的上限，需要 RF64")?;
    let frames_u32 = u32::try_from(frames).map_err(|_| "fact 样本帧数超出 u32")?;
    let riff_size = data_size
        .checked_add(72)
        .ok_or("RIFF 大小超出 32 位 WAVE 的上限")?;
    let bytes_per_second = pcm
        .sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or("bytesPerSecond 溢出")?;

    let io = |error: std::io::Error| format!("写出失败：{error}");
    writer.write_all(b"RIFF").map_err(io)?;
    writer.write_all(&riff_size.to_le_bytes()).map_err(io)?;
    writer.write_all(b"WAVEfmt ").map_err(io)?;
    writer.write_all(&40u32.to_le_bytes()).map_err(io)?;
    writer
        .write_all(&FORMAT_EXTENSIBLE.to_le_bytes())
        .map_err(io)?;
    writer.write_all(&channels_u16.to_le_bytes()).map_err(io)?;
    writer
        .write_all(&pcm.sample_rate.to_le_bytes())
        .map_err(io)?;
    writer
        .write_all(&bytes_per_second.to_le_bytes())
        .map_err(io)?;
    writer.write_all(&block_align.to_le_bytes()).map_err(io)?;
    writer.write_all(&32u16.to_le_bytes()).map_err(io)?;
    writer.write_all(&22u16.to_le_bytes()).map_err(io)?;
    writer.write_all(&32u16.to_le_bytes()).map_err(io)?;
    writer.write_all(&0u32.to_le_bytes()).map_err(io)?; // SPEAKER_DIRECTOUT
    writer.write_all(&SUBFORMAT_IEEE_FLOAT).map_err(io)?;
    writer.write_all(b"fact").map_err(io)?;
    writer.write_all(&4u32.to_le_bytes()).map_err(io)?;
    writer.write_all(&frames_u32.to_le_bytes()).map_err(io)?;
    writer.write_all(b"data").map_err(io)?;
    writer.write_all(&data_size.to_le_bytes()).map_err(io)?;

    // 逐帧交织。分块攒够再写，避免每个样本一次 write_all。
    let flush_at = channels
        .saturating_mul(BYTES_PER_SAMPLE)
        .saturating_mul(1024);
    let mut chunk = Vec::with_capacity(flush_at);
    for frame in 0..frames {
        for track in &pcm.tracks {
            let sample = track.samples.get(frame).copied().unwrap_or(0.0);
            chunk.extend_from_slice(&sample.to_le_bytes());
        }
        if chunk.len() >= flush_at {
            writer.write_all(&chunk).map_err(io)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        writer.write_all(&chunk).map_err(io)?;
    }
    Ok(())
}

#[cfg(all(test, feature = "audio-decode"))]
#[expect(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "下标与长度都由同一用例构造的固定 WAVE 头派生；越界即是该用例要报告的失败"
)]
mod tests {
    use super::*;
    use crate::pcm_batch::{PcmBatch, PcmTrack, PcmTrackSource};

    fn track(output_index: usize, samples: &[f32]) -> PcmTrack {
        PcmTrack {
            substream_index: 2,
            output_index,
            scene_element_id: None,
            source: PcmTrackSource::TransportChannel {
                element_index: 0,
                channel_index: output_index,
            },
            samples: samples.to_vec(),
        }
    }

    #[test]
    fn aspx_descriptors_keep_lfe_outside_the_ajoc_input_indices() {
        let pcm = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![
                PcmTrack {
                    source: PcmTrackSource::AjocInput { input_index: 0 },
                    ..track(0, &[1.0])
                },
                PcmTrack {
                    source: PcmTrackSource::Lfe,
                    ..track(1, &[2.0])
                },
            ],
        };
        assert_eq!(
            aspx_descriptors(&pcm).expect("来源完整时应可自述"),
            "{\"substream\": 2, \"role\": \"ajoc_input\", \"ajoc_input\": 0}, \
             {\"substream\": 2, \"role\": \"lfe\"}"
        );

        let invalid = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![track(0, &[1.0])],
        };
        let error = aspx_descriptors(&invalid).expect_err("传输侧编号不得冒充 A-JOC 输入");
        assert!(matches!(
            error.code,
            DiagnosticCode::InternalInvariantFailed
        ));
    }

    #[test]
    fn object_descriptors_keep_object_indices_separate_from_output_channels() {
        let pcm = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![
                PcmTrack {
                    source: PcmTrackSource::Lfe,
                    ..track(0, &[1.0])
                },
                PcmTrack {
                    source: PcmTrackSource::AjocObject { object_index: 0 },
                    ..track(1, &[2.0])
                },
                PcmTrack {
                    source: PcmTrackSource::AjocObject { object_index: 1 },
                    ..track(2, &[3.0])
                },
            ],
        };
        assert_eq!(
            objects_descriptors(&pcm).expect("对象来源完整时应可自述"),
            "{\"substream\": 2, \"role\": \"lfe\", \"output_channel\": 0}, \
             {\"substream\": 2, \"role\": \"ajoc_object\", \"ajoc_object\": 0, \"output_channel\": 1}, \
             {\"substream\": 2, \"role\": \"ajoc_object\", \"ajoc_object\": 1, \"output_channel\": 2}"
        );

        let invalid = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![PcmTrack {
                source: PcmTrackSource::AjocInput { input_index: 0 },
                ..track(0, &[1.0])
            }],
        };
        assert_eq!(
            objects_descriptors(&invalid)
                .expect_err("下混输入不得冒充对象")
                .code,
            DiagnosticCode::InternalInvariantFailed
        );
    }

    #[test]
    fn aspx_batch_distinguishes_selection_unsupported_invariant_and_parse_failures() {
        use crate::scene_batch::SceneBatchError;

        assert_eq!(
            aspx_batch_error(SceneBatchError::Selection("presentation 歧义".to_owned())).code,
            DiagnosticCode::SelectionInvalid
        );
        assert_eq!(
            aspx_batch_error(SceneBatchError::unsupported("SIMPLE 时间轴未裁决")).code,
            DiagnosticCode::UnsupportedCodingPath
        );
        assert_eq!(
            aspx_batch_error(SceneBatchError::Invariant("PCM 非有限".to_owned())).code,
            DiagnosticCode::InternalInvariantFailed
        );
        assert_eq!(
            aspx_batch_error(SceneBatchError::Failed("帧解析失败".to_owned())).code,
            DiagnosticCode::ParseFailed
        );
    }

    #[test]
    fn core_batch_distinguishes_selection_unsupported_invariant_and_parse_failures() {
        use crate::scene_batch::SceneBatchError;

        assert_eq!(
            core_batch_error(SceneBatchError::Selection("presentation 歧义".to_owned())).code,
            DiagnosticCode::SelectionInvalid
        );
        assert_eq!(
            core_batch_error(SceneBatchError::unsupported("核心路径未覆盖")).code,
            DiagnosticCode::UnsupportedCodingPath
        );
        assert_eq!(
            core_batch_error(SceneBatchError::Invariant("核心带非有限".to_owned())).code,
            DiagnosticCode::InternalInvariantFailed
        );
        assert_eq!(
            core_batch_error(SceneBatchError::Failed("帧解析失败".to_owned())).code,
            DiagnosticCode::ParseFailed
        );
    }

    #[test]
    fn objects_batch_distinguishes_unsupported_invariant_and_parse_failures() {
        use crate::scene_batch::SceneBatchError;

        assert_eq!(
            objects_batch_error(SceneBatchError::Selection("presentation 歧义".to_owned())).code,
            DiagnosticCode::SelectionInvalid
        );
        assert_eq!(
            objects_batch_error(SceneBatchError::unsupported("活动 DE")).code,
            DiagnosticCode::UnsupportedCodingPath
        );
        assert_eq!(
            objects_batch_error(SceneBatchError::Invariant("对象非有限".to_owned())).code,
            DiagnosticCode::InternalInvariantFailed
        );
        assert_eq!(
            objects_batch_error(SceneBatchError::Failed("帧解析失败".to_owned())).code,
            DiagnosticCode::ParseFailed
        );
    }

    /// 头部字段与交织顺序逐字节固定。
    ///
    /// 基线是这些字节的 SHA-256，因此头部里任何一个派生量算错都会让全部向量
    /// 的基线一起漂移，却不会有别的判据报警——这里把它钉死。
    #[test]
    fn writes_a_bit_exact_float_wave() {
        let pcm = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![track(0, &[1.0, 3.0]), track(1, &[2.0, 4.0])],
        };
        let (frames, channels) = layout(&pcm).expect("等长两声道");
        assert_eq!((frames, channels), (2, 2));

        let mut out = Vec::new();
        write_wave(&mut out, &pcm, frames, channels).expect("应可写出");

        assert_eq!(&out[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 72 + 16);
        assert_eq!(&out[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(out[16..20].try_into().unwrap()), 40);
        assert_eq!(u16::from_le_bytes(out[20..22].try_into().unwrap()), 0xfffe);
        assert_eq!(u16::from_le_bytes(out[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(out[24..28].try_into().unwrap()), 48_000);
        assert_eq!(
            u32::from_le_bytes(out[28..32].try_into().unwrap()),
            48_000 * 8,
            "bytesPerSecond = 采样率 × blockAlign"
        );
        assert_eq!(u16::from_le_bytes(out[32..34].try_into().unwrap()), 8);
        assert_eq!(u16::from_le_bytes(out[34..36].try_into().unwrap()), 32);
        assert_eq!(u16::from_le_bytes(out[36..38].try_into().unwrap()), 22);
        assert_eq!(u16::from_le_bytes(out[38..40].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(out[40..44].try_into().unwrap()), 0);
        assert_eq!(
            &out[44..60],
            &[
                0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
                0x9b, 0x71,
            ]
        );
        assert_eq!(&out[60..64], b"fact");
        assert_eq!(u32::from_le_bytes(out[64..68].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(out[68..72].try_into().unwrap()), 2);
        assert_eq!(&out[72..76], b"data");
        assert_eq!(u32::from_le_bytes(out[76..80].try_into().unwrap()), 16);

        // 交织必须是「帧内按声道升序」，不是按声道拼接。
        let samples: Vec<f32> = out[80..]
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(samples, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(out.len(), 80 + 16);
    }

    #[test]
    fn json_paths_escape_every_control_character() {
        assert_eq!(json_quote("core-\u{7f}.wav"), "\"core-\\u007f.wav\"");
        assert_eq!(json_quote("a\n\"b\\c"), "\"a\\n\\\"b\\\\c\"");
    }

    #[test]
    fn existing_output_is_never_truncated() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "macinac4-core-pcm-existing-{}-{serial}.m4a",
            std::process::id()
        ));
        std::fs::write(&path, b"keep me").expect("应能创建输入 fixture");
        let error = run(ExportCorePcmArgs {
            input: path.clone(),
            presentation: None,
            output: path.clone(),
        })
        .expect_err("输入与输出相同必须在读取前失败");
        assert!(matches!(error.code, DiagnosticCode::OutputExists));
        assert!(error.message.contains("输出路径已存在"), "{:?}", error);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
        std::fs::remove_file(path).expect("应能清理 fixture");
    }

    /// 声道不等长必须显式失败：交织本身没有对齐信息可依。
    #[test]
    fn rejects_ragged_channels() {
        let pcm = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![track(0, &[1.0, 2.0]), track(1, &[3.0])],
        };
        let error = layout(&pcm).expect_err("不等长应被拒绝");
        assert!(error.contains("声道长度不一致"), "{error}");

        let empty = PcmBatch {
            sample_rate: 48_000,
            tracks: Vec::new(),
        };
        assert!(
            layout(&empty)
                .expect_err("空声道应被拒绝")
                .contains("不含任何声道")
        );

        let silent = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![track(0, &[])],
        };
        assert!(
            layout(&silent)
                .expect_err("零样本应被拒绝")
                .contains("不含任何样本")
        );

        let nonfinite = PcmBatch {
            sample_rate: 48_000,
            tracks: vec![track(0, &[f32::NAN])],
        };
        assert!(
            layout(&nonfinite)
                .expect_err("非有限对象样本应被拒绝")
                .contains("非有限样本")
        );
    }
}
