//! 真实 full A-JOC 重建对象音频到 DAMF 三件套的导出。
//!
//! 音频布局与 full ADM 完全一致：前十轨是 7.1.2 compatibility bed，
//! 可选 LFE 只占第 4 轨，其余 bed 静音；A-JOC 对象从第 11 轨开始。
//! `home` 与 `3dof` 只改变 manifest 的版本和 presentation type。

use super::*;
use crate::ExportFullDamfArgs;
use crate::scene_batch::{FullSceneBatch, SceneBatchError, collect_full_scene_batch};
use crate::scene_export::{
    FULL_LFE_BED_CHANNEL, FullSceneError, FullSceneErrorKind, FullSourceSelection, PreparedFullPcm,
    map_full_pcm, prepare_full_pcm, select_full_sources, write_full_s24le,
};
use macindecode_ac4_scene::PresentationSelection;

#[derive(Debug)]
struct FullDamfSelection {
    source: FullSourceSelection,
    objects: Vec<SelectedObject>,
}

fn full_error(code: DiagnosticCode, message: impl Into<String>) -> CliError {
    CliError::new(super::super::FULL_COMMAND, code, message)
}

pub(super) fn run(args: ExportFullDamfArgs) -> Result<String, CliError> {
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
    if pcm.sample_rate != OUTPUT_SAMPLE_RATE {
        return Err(full_error(
            DiagnosticCode::UnsupportedCodingPath,
            format!(
                "DAMF requires {OUTPUT_SAMPLE_RATE} Hz, but full-object PCM is {} Hz; resampling is not supported",
                pcm.sample_rate
            ),
        )
        .with_context("sample_rate", pcm.sample_rate.to_string()));
    }
    let selection = select_full_objects(&metadata)?;
    validate_selected_common(&selection.objects)
        .map_err(|message| full_error(DiagnosticCode::SelectionInvalid, message))?;
    let mapped = map_full_pcm(&metadata, &pcm, &selection.source).map_err(full_scene_error)?;
    let audio = prepare_full_pcm(mapped);
    let duration = u64::try_from(audio.frames).map_err(|_| {
        full_error(
            DiagnosticCode::InternalInvariantFailed,
            "Full DAMF PCM frame count exceeds u64",
        )
    })?;
    if duration == 0 {
        return Err(full_error(
            DiagnosticCode::InputInvalid,
            "Presentation duration is zero after applying edits",
        ));
    }
    let stem = choose_stem_from(&args.input, args.stem.clone())
        .map_err(|message| full_error(DiagnosticCode::SelectionInvalid, message))?;

    let mut warnings = WarningSet::default();
    let manifest = build_manifest(
        &stem,
        args.fps.as_str(),
        &selection.objects,
        args.presentation_type,
        DamfEssence::Full {
            has_lfe: selection.source.lfe.is_some(),
        },
        &mut warnings,
    );
    let metadata_json = build_metadata(&metadata, &selection.objects, duration, &mut warnings)
        .map_err(|message| full_error(DiagnosticCode::MappingUnsupported, message))?;
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

    let channels = BED_CHANNELS
        .len()
        .checked_add(selection.objects.len())
        .ok_or_else(|| {
            full_error(
                DiagnosticCode::InternalInvariantFailed,
                "Full DAMF channel-count overflow",
            )
        })?;
    write_package_with_audio(
        super::super::FULL_COMMAND,
        &args.output,
        &stem,
        &manifest,
        &metadata_json,
        |path| {
            write_caf_with_audio(path, duration, channels, |writer| {
                write_full_s24le(writer, &audio)
            })
            .map_err(|message| full_error(DiagnosticCode::OutputWriteFailed, message))
        },
    )?;
    summary_json(
        &args.output,
        &stem,
        duration,
        &selection,
        &audio,
        args.presentation_type,
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

fn select_full_objects(metadata: &MetadataBatch) -> Result<FullDamfSelection, CliError> {
    let source = select_full_sources(metadata).map_err(full_scene_error)?;
    if source.objects.len() > MAX_PROBE_OBJECTS {
        return Err(full_error(
            DiagnosticCode::UnsupportedCodingPath,
            format!(
                "Full A-JOC has {} objects; the DAMF 7.1.2 bed supports at most {MAX_PROBE_OBJECTS}",
                source.objects.len()
            ),
        ));
    }
    let objects = source
        .objects
        .iter()
        .copied()
        .enumerate()
        .map(|(index, scene)| {
            let damf_id = BED_CHANNELS
                .len()
                .checked_add(index)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    full_error(
                        DiagnosticCode::InternalInvariantFailed,
                        "Full DAMF object ID exceeds u32",
                    )
                })?;
            Ok(SelectedObject { scene, damf_id })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    Ok(FullDamfSelection { source, objects })
}

#[allow(clippy::too_many_arguments)]
fn summary_json(
    output: &Path,
    stem: &str,
    duration: u64,
    selection: &FullDamfSelection,
    audio: &PreparedFullPcm<'_>,
    presentation_type: DamfPresentationType,
    warnings: &[MappingWarning],
) -> Result<String, String> {
    let root = fs::canonicalize(output)
        .map_err(|error| format!("Failed to canonicalize output path: {error}"))?;
    let file = |suffix: &str| root.join(format!("{stem}{suffix}"));
    let objects = selection
        .objects
        .iter()
        .zip(&audio.objects)
        .enumerate()
        .map(|(index, (object, channel))| {
            format!(
                "{{\"selector\":{},\"ajoc_object\":{index},\"source_output_channel\":{},\"damf_id\":{},\"track_index\":{}}}",
                json_quote(&scene_selector(&object.scene)),
                channel.output_index,
                object.damf_id,
                object.damf_id.saturating_add(1)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut tracks = BED_CHANNELS
        .iter()
        .enumerate()
        .map(|(index, speaker)| {
            let essence = if index == FULL_LFE_BED_CHANNEL && audio.lfe.is_some() {
                "lfe"
            } else {
                "silence"
            };
            let source = if index == FULL_LFE_BED_CHANNEL {
                audio
                    .lfe
                    .map(|channel| {
                        format!(",\"source_output_channel\":{}", channel.output_index)
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            format!(
                "{{\"damf_id\":{index},\"track_index\":{},\"role\":\"bed\",\"speaker\":{},\"essence\":{}{source}}}",
                index.saturating_add(1),
                json_quote(speaker),
                json_quote(essence)
            )
        })
        .collect::<Vec<_>>();
    tracks.extend(selection.objects.iter().zip(&audio.objects).enumerate().map(
        |(index, (object, channel))| {
            format!(
                "{{\"damf_id\":{},\"track_index\":{},\"role\":\"object\",\"selector\":{},\"ajoc_object\":{index},\"source_output_channel\":{}}}",
                object.damf_id,
                object.damf_id.saturating_add(1),
                json_quote(&scene_selector(&object.scene)),
                channel.output_index
            )
        },
    ));
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
    Ok(format!(
        "{{\"manifest\":{},\"metadata\":{},\"audio\":{},\"sample_rate\":{OUTPUT_SAMPLE_RATE},\"duration_samples\":{duration},\"package_version\":{},\"presentation_type\":{},\"objects\":[{objects}],\"tracks\":[{}],\"scale\":{},\"bandwidth\":\"aspx\",\"channel_order\":\"7.1.2_bed_then_ajoc_full_objects\",\"unmapped\":[{warning_json}]}}",
        json_quote(&file(".atmos").display().to_string()),
        json_quote(&file(".atmos.metadata").display().to_string()),
        json_quote(&file(".atmos.audio").display().to_string()),
        json_quote(presentation_type.version()),
        json_quote(presentation_type.as_str()),
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
}
