//! 真实 full A-JOC 重建对象音频到 ADM BW64/RF64 的导出。
//!
//! full 模式把矩阵重建后的对象 PCM 与第二份 OAMD 配对。输出仍保留十轨
//! 7.1.2 compatibility bed：LFE 只写入第 4 轨，其余九轨静音；全部 full
//! 对象从第 11 轨开始。标准模式写 BW64，Logic 模式改写 RF64 并携带 dbmd。

use super::*;
use crate::ExportFullAdmBwfArgs;
#[cfg(test)]
use crate::pcm_batch::{PcmTrack, PcmTrackSource};
use crate::scene_batch::{FullSceneBatch, SceneBatchError, collect_full_scene_batch};
#[cfg(test)]
use crate::scene_export::full_s24_sample;
use crate::scene_export::{
    FULL_LFE_BED_CHANNEL, FullSceneError, FullSceneErrorKind, FullSourceSelection, PreparedFullPcm,
    map_full_pcm, prepare_full_pcm, select_full_sources, write_full_s24le,
};
use macindecode_ac4_scene::PresentationSelection;

#[derive(Debug)]
struct FullSelection {
    source: FullSourceSelection,
    objects: Vec<SelectedObject>,
}

fn full_error(code: DiagnosticCode, message: impl Into<String>) -> CliError {
    CliError::new(super::super::FULL_COMMAND, code, message)
}

pub(super) fn run(args: ExportFullAdmBwfArgs) -> Result<String, CliError> {
    ensure_output(&args.output)?;
    let data = fs::read(&args.input).map_err(|error| {
        full_error(
            DiagnosticCode::InputReadFailed,
            format!("Failed to read {}: {error}", args.input.display()),
        )
    })?;
    let presentation_selection = match args.presentation {
        Some(index) => PresentationSelection::Index(u32::try_from(index).map_err(|_| {
            full_error(
                DiagnosticCode::SelectionInvalid,
                "Presentation index exceeds u32",
            )
        })?),
        None => PresentationSelection::AutoUnique,
    };
    let FullSceneBatch { metadata, pcm } =
        collect_full_scene_batch(&data, presentation_selection).map_err(full_batch_error)?;
    let selection = select_full_objects(&metadata)?;
    validate_selected_common(&selection.objects)
        .map_err(|message| full_error(DiagnosticCode::SelectionInvalid, message))?;
    let mapped = map_full_pcm(&metadata, &pcm, &selection.source).map_err(full_scene_error)?;
    let audio = prepare_full_pcm(mapped);
    let duration = u64::try_from(audio.frames).map_err(|_| {
        full_error(
            DiagnosticCode::InternalInvariantFailed,
            "Full ADM PCM frame count exceeds u64",
        )
    })?;
    if duration == 0 {
        return Err(full_error(
            DiagnosticCode::InputInvalid,
            "Presentation duration is zero after applying edits",
        ));
    }
    format_sample_time_at(duration, pcm.sample_rate, args.compatibility)
        .map_err(|message| full_error(DiagnosticCode::MappingUnsupported, message))?;

    let mut warnings = WarningSet::default();
    append_common_warnings(&selection.objects, &mut warnings);
    let axml = build_full_axml(
        ADM_PROGRAMME_NAME,
        &metadata,
        &selection.objects,
        duration,
        pcm.sample_rate,
        args.compatibility,
        &mut warnings,
    )
    .map_err(|message| full_error(DiagnosticCode::MappingUnsupported, message))?;
    let chna = build_chna(&selection.objects)
        .map_err(|message| full_error(DiagnosticCode::MappingUnsupported, message))?;
    let channels = BED_CHANNELS
        .len()
        .checked_add(selection.objects.len())
        .ok_or_else(|| {
            full_error(
                DiagnosticCode::InternalInvariantFailed,
                "Full ADM channel-count overflow",
            )
        })?;
    let dbmd = match args.compatibility {
        AdmCompatibility::Standard => None,
        AdmCompatibility::Logic => Some(
            build_logic_dbmd(channels, args.fps)
                .map_err(|message| full_error(DiagnosticCode::MappingUnsupported, message))?,
        ),
    };
    if args.strict_mapping && !warnings.items.is_empty() {
        return Err(full_error(
            DiagnosticCode::MappingUnsupported,
            format!(
                "Strict mapping rejected {} class(es) of metadata that cannot be represented exactly: {}",
                warnings.items.len(),
                warnings
                    .items
                    .iter()
                    .map(|item| format!("{}:{}", item.selector, item.field))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }

    write_atomic_full_adm(
        &args.output,
        pcm.sample_rate,
        &audio,
        channels,
        axml.as_bytes(),
        &chna,
        dbmd.as_deref(),
        args.compatibility,
    )?;
    summary_json(
        &args.output,
        pcm.sample_rate,
        duration,
        &selection,
        &audio,
        axml.len(),
        dbmd.as_ref().map(Vec::len),
        args.compatibility,
        &warnings.items,
    )
    .map_err(|message| full_error(DiagnosticCode::SerializationFailed, message))
}

fn ensure_output(output: &Path) -> Result<(), CliError> {
    if fs::symlink_metadata(output).is_ok() {
        return Err(full_error(
            DiagnosticCode::OutputExists,
            format!("Output path already exists: {}", output.display()),
        ));
    }
    let parent = output_parent(output);
    if !parent.is_dir() {
        return Err(full_error(
            DiagnosticCode::OutputCreateFailed,
            format!(
                "Output parent directory does not exist: {}",
                parent.display()
            ),
        ));
    }
    Ok(())
}

fn full_batch_error(error: SceneBatchError) -> CliError {
    match error {
        SceneBatchError::Selection(message) => {
            full_error(DiagnosticCode::SelectionInvalid, message)
        }
        SceneBatchError::Unsupported {
            message,
            scene_path,
        } => {
            let error = full_error(DiagnosticCode::UnsupportedCodingPath, message);
            match scene_path {
                Some(path) => error.with_context("scene_path", path.label()),
                None => error,
            }
        }
        SceneBatchError::Invariant(message) => {
            full_error(DiagnosticCode::InternalInvariantFailed, message)
        }
        SceneBatchError::Failed(message) => full_error(DiagnosticCode::ParseFailed, message),
    }
}

fn full_scene_error(error: FullSceneError) -> CliError {
    let code = match error.kind {
        FullSceneErrorKind::SelectionInvalid => DiagnosticCode::SelectionInvalid,
        FullSceneErrorKind::UnsupportedCodingPath => DiagnosticCode::UnsupportedCodingPath,
        FullSceneErrorKind::InternalInvariant => DiagnosticCode::InternalInvariantFailed,
    };
    error
        .context
        .into_iter()
        .fold(full_error(code, error.message), |out, (key, value)| {
            out.with_context(key, value)
        })
}

fn select_full_objects(metadata: &MetadataBatch) -> Result<FullSelection, CliError> {
    let source = select_full_sources(metadata).map_err(full_scene_error)?;
    let objects = source
        .objects
        .iter()
        .copied()
        .enumerate()
        .map(|(index, scene)| {
            let ordinal = u16::try_from(index.saturating_add(1)).map_err(|_| {
                full_error(
                    DiagnosticCode::InternalInvariantFailed,
                    "ADM object ordinal exceeds u16",
                )
            })?;
            let track_index = u16::try_from(
                BED_CHANNELS.len().saturating_add(index.saturating_add(1)),
            )
            .map_err(|_| {
                full_error(
                    DiagnosticCode::InternalInvariantFailed,
                    "ADM track index exceeds u16",
                )
            })?;
            Ok(SelectedObject {
                scene,
                ordinal,
                track_index,
            })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    Ok(FullSelection { source, objects })
}

#[allow(clippy::too_many_arguments)]
fn write_atomic_full_adm(
    output: &Path,
    sample_rate: u32,
    audio: &PreparedFullPcm<'_>,
    channels: usize,
    axml: &[u8],
    chna: &[u8],
    dbmd: Option<&[u8]>,
    compatibility: AdmCompatibility,
) -> Result<(), CliError> {
    let temp = create_temp_file(output)
        .map_err(|message| full_error(DiagnosticCode::OutputCreateFailed, message))?;
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&temp)
            .map_err(|error| {
                full_error(
                    DiagnosticCode::OutputCreateFailed,
                    format!("Failed to open temporary full ADM BWF: {error}"),
                )
            })?;
        let frames = u64::try_from(audio.frames).map_err(|_| {
            full_error(
                DiagnosticCode::InternalInvariantFailed,
                "Full ADM PCM frame count exceeds u64",
            )
        })?;
        write_adm_wave_payload(
            file,
            AdmWaveSpec {
                frames,
                channels,
                sample_rate,
                axml,
                chna,
                dbmd,
                compatibility,
            },
            |writer| write_full_s24le(writer, audio),
        )
        .map_err(|message| full_error(DiagnosticCode::OutputWriteFailed, message))?;
        publish_temp_noclobber(&temp, output)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn publish_temp_noclobber(temp: &Path, output: &Path) -> Result<(), CliError> {
    match fs::hard_link(temp, output) {
        Ok(()) => {
            // 临时文件与目标同目录；hard link 原子发布完整 inode，且不会替换
            // 写入期间出现的目标。目标已提交后，清理临时链接失败不回滚结果。
            drop(fs::remove_file(temp));
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(full_error(
            DiagnosticCode::OutputExists,
            format!(
                "Output path was created while writing: {}",
                output.display()
            ),
        )),
        Err(error) => Err(full_error(
            DiagnosticCode::OutputCommitFailed,
            format!("Failed to commit full ADM BWF atomically without clobbering: {error}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn summary_json(
    output: &Path,
    sample_rate: u32,
    duration: u64,
    selection: &FullSelection,
    audio: &PreparedFullPcm<'_>,
    axml_bytes: usize,
    dbmd_bytes: Option<usize>,
    compatibility: AdmCompatibility,
    warnings: &[MappingWarning],
) -> Result<String, String> {
    let output = fs::canonicalize(output)
        .map_err(|error| format!("Failed to canonicalize output path: {error}"))?;
    let objects = selection
        .objects
        .iter()
        .zip(&audio.objects)
        .map(|(object, channel)| {
            format!(
                "{{\"selector\":{},\"role\":\"object\",\"ajoc_object\":{},\"source_output_channel\":{},\"track_index\":{},\"audio_object_id\":{}}}",
                json_quote(&scene_selector(&object.scene)),
                object.ordinal.saturating_sub(1),
                channel.output_index,
                object.track_index,
                json_quote(&object_id(object.track_index))
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut tracks = BED_CHANNELS
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            let track = index.saturating_add(1);
            let essence = if index == FULL_LFE_BED_CHANNEL && selection.source.lfe.is_some() {
                "lfe"
            } else {
                "silence"
            };
            let source = if index == FULL_LFE_BED_CHANNEL {
                audio
                    .lfe
                    .map(|item| format!(",\"source_output_channel\":{}", item.output_index))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            format!(
                "{{\"track_index\":{track},\"role\":\"bed\",\"speaker\":{},\"essence\":{}{source}}}",
                json_quote(channel.label),
                json_quote(essence)
            )
        })
        .collect::<Vec<_>>();
    tracks.extend(
        selection
            .objects
            .iter()
            .zip(&audio.objects)
            .map(|(object, channel)| {
                format!(
                    "{{\"track_index\":{},\"role\":\"object\",\"selector\":{},\"ajoc_object\":{},\"source_output_channel\":{}}}",
                    object.track_index,
                    json_quote(&scene_selector(&object.scene)),
                    object.ordinal.saturating_sub(1),
                    channel.output_index
                )
            }),
    );
    let warning_json = warnings
        .iter()
        .map(|warning| {
            format!(
                "{{\"selector\":{},\"sample\":{},\"field\":{},\"detail\":{}}}",
                json_quote(&warning.selector),
                warning
                    .sample
                    .map_or_else(|| "null".to_owned(), |value| value.to_string()),
                json_quote(warning.field),
                json_quote(&warning.detail)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let scale = format!(
        "global_linear_gain={:.12};source_peak={:.6};internal_±32768_to_pcm_s24le",
        audio.linear_gain, audio.source_peak
    );
    let dbmd_bytes = dbmd_bytes.map_or_else(|| "null".to_owned(), |value| value.to_string());
    Ok(format!(
        "{{\"file\":{},\"container\":{},\"compatibility\":{},\"adm_version\":\"ITU-R_BS.2076-2\",\"sample_rate\":{sample_rate},\"bit_depth\":24,\"channels\":{},\"duration_samples\":{duration},\"axml_bytes\":{axml_bytes},\"dbmd_bytes\":{dbmd_bytes},\"objects\":[{objects}],\"tracks\":[{}],\"scale\":{},\"bandwidth\":\"aspx\",\"channel_order\":\"7.1.2_bed_then_ajoc_full_objects\",\"unmapped\":[{warning_json}]}}",
        json_quote(&output.display().to_string()),
        json_quote(compatibility.container_name()),
        json_quote(match compatibility {
            AdmCompatibility::Standard => "standard",
            AdmCompatibility::Logic => "logic",
        }),
        BED_CHANNELS.len().saturating_add(selection.objects.len()),
        tracks.join(","),
        json_quote(&scale),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_scene_batch_failure_classes() {
        assert_eq!(
            full_batch_error(SceneBatchError::Selection("presentation 歧义".to_owned())).code,
            DiagnosticCode::SelectionInvalid
        );
        assert_eq!(
            full_batch_error(SceneBatchError::unsupported("活动 DE")).code,
            DiagnosticCode::UnsupportedCodingPath
        );
        assert_eq!(
            full_batch_error(SceneBatchError::Invariant("对象非有限".to_owned())).code,
            DiagnosticCode::InternalInvariantFailed
        );
        assert_eq!(
            full_batch_error(SceneBatchError::Failed("帧解析失败".to_owned())).code,
            DiagnosticCode::ParseFailed
        );
    }

    #[test]
    fn normalizes_and_globally_attenuates_internal_pcm() {
        assert_eq!(full_s24_sample(0.0, 1.0).unwrap(), [0, 0, 0]);
        assert_eq!(full_s24_sample(32_768.0, 1.0).unwrap(), [0xff, 0xff, 0x7f]);
        assert_eq!(full_s24_sample(-32_768.0, 1.0).unwrap(), [0x01, 0x00, 0x80]);
        assert!(full_s24_sample(f32::NAN, 1.0).is_err());
        assert!(full_s24_sample(32_769.0, 1.0).is_err());

        let peak = f64::from(33_950.93_f32);
        let gain = 32_768.0 / peak;
        assert!(gain < 1.0);
        assert_eq!(
            full_s24_sample(33_950.93_f32, gain).unwrap(),
            [0xff, 0xff, 0x7f]
        );
    }

    #[test]
    fn writes_nine_silent_bed_tracks_lfe_on_four_and_full_objects_after_bed() {
        let first = PcmTrack {
            substream_index: 2,
            output_index: 0,
            scene_element_id: None,
            source: PcmTrackSource::AjocObject { object_index: 0 },
            samples: vec![16_384.0],
        };
        let lfe = PcmTrack {
            substream_index: 2,
            output_index: 1,
            scene_element_id: None,
            source: PcmTrackSource::Lfe,
            samples: vec![32_768.0],
        };
        let second = PcmTrack {
            substream_index: 2,
            output_index: 2,
            scene_element_id: None,
            source: PcmTrackSource::AjocObject { object_index: 1 },
            samples: vec![-16_384.0],
        };
        let audio = PreparedFullPcm {
            objects: vec![&first, &second],
            lfe: Some(&lfe),
            frames: 1,
            source_peak: 32_768.0,
            linear_gain: 1.0,
        };
        let mut pcm = Vec::new();
        write_full_s24le(&mut pcm, &audio).expect("PCM 应可写出");
        assert_eq!(pcm.len(), 12 * 3);
        for track in [0usize, 1, 2, 4, 5, 6, 7, 8, 9] {
            assert_eq!(
                pcm.get(track * 3..track * 3 + 3),
                Some([0, 0, 0].as_slice())
            );
        }
        assert_eq!(
            pcm.get(3 * 3..3 * 3 + 3),
            Some([0xff, 0xff, 0x7f].as_slice())
        );
        assert_eq!(pcm.get(10 * 3..10 * 3 + 3), Some([0, 0, 0x40].as_slice()));
        assert_eq!(pcm.get(11 * 3..11 * 3 + 3), Some([0, 0, 0xc0].as_slice()));
    }
}
