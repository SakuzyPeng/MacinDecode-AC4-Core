//! DAMF 导出流程门面。

mod full;
mod manifest;
mod metadata;
mod package;
mod yaml;

use manifest::*;
use metadata::*;
use package::*;
use yaml::{finish_lines as finish_yaml_lines, quote as yaml_scalar};

use super::*;
#[cfg(test)]
use crate::metadata_batch::default_output_metadata_event;
use crate::metadata_batch::{MetadataBatch, MetadataElement, OutputMetadataEvent};
use crate::scene_batch::{DiagnosticSceneBatch, SceneBatchError, collect_diagnostic_scene_batch};
#[cfg(test)]
use crate::scene_export::parse_scene_selector;
use crate::scene_export::{
    BYTES_PER_SAMPLE, MAX_PROBE_OBJECTS, MappingWarning, OUTPUT_SAMPLE_RATE, PinkNoise, SAMPLE_MAX,
    WarningSet, position, rescale_u64, scene_selector, select_metadata_elements, selector_seed,
    validate_selected_common as validate_scene_common, zone_components,
};
use crate::wire::{CliError, DiagnosticCode};
use crate::{DamfPresentationType, DecodeMode};
#[cfg(test)]
use macindecode_ac4_bitstream::oamd::{
    AdditionalObjectMetadata, PositionCoding, QuantizedPosition,
};
use macindecode_ac4_bitstream::oamd::{
    ObjectGainState, ObjectPriorityState, WidthUpdate, ZoneUpdate,
};
use macindecode_ac4_scene::{DecodeMode as SceneSessionDecodeMode, PresentationSelection};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

const BED_CHANNELS: [&str; 10] = [
    "L", "R", "C", "LFE", "Lss", "Rss", "Lrs", "Rrs", "Lts", "Rts",
];

#[derive(Debug, Clone, Copy)]
struct SelectedObject {
    scene: MetadataElement,
    damf_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DamfEssence {
    Probe,
    Full { has_lfe: bool },
}

type OutputEvent = OutputMetadataEvent;

fn cli_error(code: DiagnosticCode, message: impl Into<String>) -> CliError {
    CliError::new(super::COMMAND, code, message)
}

fn batch_error(error: SceneBatchError) -> CliError {
    match error {
        SceneBatchError::Selection(message) => cli_error(DiagnosticCode::SelectionInvalid, message),
        SceneBatchError::Unsupported {
            message,
            scene_path,
        } => {
            let error = cli_error(DiagnosticCode::UnsupportedCodingPath, message);
            match scene_path {
                Some(path) => error.with_context("scene_path", path.label()),
                None => error,
            }
        }
        SceneBatchError::Invariant(message) => {
            cli_error(DiagnosticCode::InternalInvariantFailed, message)
        }
        SceneBatchError::Failed(message) => cli_error(DiagnosticCode::ParseFailed, message),
    }
}

pub(super) fn run(args: ExportDamfArgs) -> Result<String, CliError> {
    if !args.probe_level_dbfs.is_finite() || !(-96.0..=0.0).contains(&args.probe_level_dbfs) {
        return Err(cli_error(
            DiagnosticCode::SelectionInvalid,
            "--probe-level-dbfs must be within -96..=0",
        ));
    }
    if args.output.exists() {
        return Err(cli_error(
            DiagnosticCode::OutputExists,
            format!("Output path already exists: {}", args.output.display()),
        ));
    }
    let parent = output_parent(&args.output);
    if !parent.is_dir() {
        return Err(cli_error(
            DiagnosticCode::OutputCreateFailed,
            format!(
                "Output parent directory does not exist: {}",
                parent.display()
            ),
        ));
    }

    let data = fs::read(&args.input).map_err(|error| {
        cli_error(
            DiagnosticCode::InputReadFailed,
            format!("Failed to read {}: {error}", args.input.display()),
        )
    })?;
    let mode = match args.mode {
        DecodeMode::Full => SceneSessionDecodeMode::Full,
        DecodeMode::Core => SceneSessionDecodeMode::Core,
    };
    let requested = match args.presentation {
        Some(index) => PresentationSelection::Index(u32::try_from(index).map_err(|_| {
            cli_error(
                DiagnosticCode::SelectionInvalid,
                "Presentation index exceeds u32",
            )
        })?),
        None => PresentationSelection::AutoUnique,
    };
    let DiagnosticSceneBatch { metadata } =
        collect_diagnostic_scene_batch(&data, requested, mode).map_err(batch_error)?;
    let selected = select_objects(&metadata, &args.object, args.all_objects)
        .map_err(|message| cli_error(DiagnosticCode::SelectionInvalid, message))?;
    validate_selected_common(&selected)
        .map_err(|message| cli_error(DiagnosticCode::SelectionInvalid, message))?;
    let stem = choose_stem(&args)
        .map_err(|message| cli_error(DiagnosticCode::SelectionInvalid, message))?;
    let duration = rescale_u64(
        metadata.duration_samples,
        metadata.sample_rate,
        OUTPUT_SAMPLE_RATE,
    )
    .map_err(|message| cli_error(DiagnosticCode::InternalInvariantFailed, message))?;
    if duration == 0 {
        return Err(cli_error(
            DiagnosticCode::InputInvalid,
            "Presentation duration is zero after applying edits",
        ));
    }

    let mut warnings = WarningSet::default();
    let manifest = build_manifest(
        &stem,
        args.fps.as_str(),
        &selected,
        DamfPresentationType::Home,
        DamfEssence::Probe,
        &mut warnings,
    );
    let metadata_json = build_metadata(&metadata, &selected, duration, &mut warnings)
        .map_err(|message| cli_error(DiagnosticCode::MappingUnsupported, message))?;
    if args.strict_mapping && !warnings.items.is_empty() {
        return Err(cli_error(
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
    write_package(
        &args.output,
        &stem,
        &manifest,
        &metadata_json,
        duration,
        &selected,
        args.probe_level_dbfs,
    )?;
    summary_json(&args.output, &stem, duration, &selected, &warnings.items)
        .map_err(|message| cli_error(DiagnosticCode::SerializationFailed, message))
}

pub(super) fn run_full(args: ExportFullDamfArgs) -> Result<String, CliError> {
    full::run(args)
}

fn select_objects(
    metadata: &MetadataBatch,
    selectors: &[String],
    all: bool,
) -> Result<Vec<SelectedObject>, String> {
    let chosen =
        select_metadata_elements(metadata, selectors, all).map_err(|error| error.to_string())?;
    if chosen.len() > MAX_PROBE_OBJECTS {
        return Err(format!(
            "Selected {} objects; the DAMF 7.1.2 bed supports at most {MAX_PROBE_OBJECTS}",
            chosen.len()
        ));
    }
    Ok(chosen
        .into_iter()
        .enumerate()
        .map(|(index, scene)| SelectedObject {
            scene,
            damf_id: u32::try_from(BED_CHANNELS.len().saturating_add(index)).unwrap_or(u32::MAX),
        })
        .collect())
}

fn choose_stem(args: &ExportDamfArgs) -> Result<String, String> {
    choose_stem_from(&args.input, args.stem.clone())
}

fn choose_stem_from(input: &Path, requested: Option<String>) -> Result<String, String> {
    let stem = requested.or_else(|| {
        input
            .file_stem()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
    });
    let stem = stem.ok_or("Cannot derive a DAMF stem from the input file name; use --stem")?;
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || stem.contains('/')
        || stem.contains('\\')
        || stem.chars().any(char::is_control)
    {
        return Err(format!("Invalid DAMF stem: {stem:?}"));
    }
    Ok(stem)
}

#[cfg(test)]
fn parse_selector(raw: &str) -> Result<(Option<u32>, u8), String> {
    parse_scene_selector(raw).map_err(|error| error.to_string())
}

fn validate_selected_common(selected: &[SelectedObject]) -> Result<(), String> {
    let scenes = selected
        .iter()
        .map(|object| object.scene)
        .collect::<Vec<_>>();
    validate_scene_common(&scenes).map_err(|error| error.to_string())
}

fn summary_json(
    output: &Path,
    stem: &str,
    duration: u64,
    selected: &[SelectedObject],
    warnings: &[MappingWarning],
) -> Result<String, String> {
    let root = fs::canonicalize(output)
        .map_err(|error| format!("Failed to canonicalize output path: {error}"))?;
    let file = |suffix: &str| root.join(format!("{stem}{suffix}"));
    let objects = selected
        .iter()
        .map(|object| {
            format!(
                "{{\"selector\":{},\"damf_id\":{}}}",
                json_quote(&scene_selector(&object.scene)),
                object.damf_id
            )
        })
        .collect::<Vec<_>>()
        .join(",");
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
    Ok(format!(
        "{{\"manifest\":{},\"metadata\":{},\"audio\":{},\"sample_rate\":48000,\"duration_samples\":{duration},\"objects\":[{objects}],\"unmapped\":[{warning_json}]}}",
        json_quote(&file(".atmos").display().to_string()),
        json_quote(&file(".atmos.metadata").display().to_string()),
        json_quote(&file(".atmos.audio").display().to_string()),
    ))
}

fn yaml_quote(value: &str) -> String {
    yaml_scalar(value)
}

fn json_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            value if value.is_control() => {
                out.push_str(&format!("\\u{:04x}", u32::from(value)));
            }
            value => out.push(value),
        }
    }
    out.push('\"');
    out
}

fn number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let mut out = format!("{value:.9}");
    while out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.pop();
    }
    out
}

fn io_error(error: std::io::Error) -> String {
    format!("Failed to write CAF: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata_batch::{MediaSpan, MetadataElementId, MetadataElementKind, MetadataEvent};
    use macindecode_ac4_bitstream::oamd::{
        ObjectBasicState, ObjectMetadataState, ObjectRenderState, OtherPropertiesUpdate,
    };
    use macindecode_ac4_scene::{
        HeadTrackingPolicy, HeadphonePolicy, HeadphonePolicyState, HeadphoneRenderMode,
    };

    fn test_scene(substream: u32, object: u8) -> MetadataElement {
        MetadataElement {
            element_id: MetadataElementId::new(
                u64::from(substream)
                    .saturating_mul(256)
                    .saturating_add(u64::from(object)),
            ),
            substream_index: substream,
            object_index: object,
            kind: MetadataElementKind::DynamicObject,
            common: None,
            common_conflict: false,
        }
    }

    fn test_batch(elements: Vec<MetadataElement>) -> MetadataBatch {
        MetadataBatch {
            headphone_events: Vec::new(),
            headphone_only_orders: Default::default(),
            sample_rate: 48_000,
            duration_samples: 48_000,
            media_span: Some(MediaSpan {
                start_sample: 0,
                end_sample: 48_000,
            }),
            decode_mode: SceneSessionDecodeMode::Full,
            elements,
            events: Vec::new(),
        }
    }

    mod headphone_export {
        use super::*;
        use crate::metadata_batch::{HeadphoneMetadataEvent, project_headphone_events};
        use macindecode_ac4_scene::HeadphonePolicyIssue;
        use std::collections::BTreeMap;

        fn policy(head_relative: bool, far: bool) -> HeadphonePolicyState {
            HeadphonePolicyState::Resolved(HeadphonePolicy::new(
                if far {
                    HeadphoneRenderMode::Far
                } else {
                    HeadphoneRenderMode::Near
                },
                if head_relative {
                    HeadTrackingPolicy::HeadRelative
                } else {
                    HeadTrackingPolicy::SceneRelative
                },
            ))
        }

        fn batch() -> (MetadataBatch, SelectedObject) {
            let scene = test_scene(2, 1);
            let mut batch = test_batch(vec![scene]);
            for (sample, x, gain, ramp, order) in [(0, 0, 42, 0, 0), (1000, 62, 30, 1000, 1)] {
                let mut state = default_inactive_event().state;
                state.active = true;
                state.render.as_mut().unwrap().position.x = x;
                state.basic.as_mut().unwrap().gain = ObjectGainState::Quantized(gain);
                batch.events.push(MetadataEvent {
                    sample_position: sample,
                    element_id: scene.element_id,
                    stream_order: order,
                    ramp_samples: ramp,
                    state,
                    additional: Default::default(),
                });
            }
            batch.headphone_events.push(HeadphoneMetadataEvent {
                sample_position: 0,
                element_id: scene.element_id,
                stream_order: 0,
                policy: policy(false, false),
            });
            (batch, SelectedObject { scene, damf_id: 10 })
        }

        /// 独立读取输出的字段事件，不复用 writer 的状态或投影代码。
        fn object_records(text: &str) -> Vec<BTreeMap<String, String>> {
            let mut records = Vec::<BTreeMap<String, String>>::new();
            for line in text.lines() {
                if let Some(id) = line.strip_prefix("  - ID: ") {
                    let mut record = BTreeMap::new();
                    record.insert("ID".to_owned(), id.to_owned());
                    records.push(record);
                } else if let Some((key, value)) = line.trim().split_once(": ")
                    && let Some(record) = records.last_mut()
                {
                    record.insert(key.to_owned(), value.to_owned());
                }
            }
            records
                .into_iter()
                .filter(|record| record.get("ID").is_some_and(|id| id == "10"))
                .collect()
        }

        fn continuous_curve(records: &[BTreeMap<String, String>], key: &str) -> Vec<f64> {
            let mut curve = Vec::new();
            for sample in 0..=3000 {
                let (mut start, mut length, mut from, mut target) = (0u64, 0u64, 0.0, 0.0);
                for record in records {
                    let time = record["samplePos"].parse::<u64>().unwrap();
                    if time > sample {
                        break;
                    }
                    let Some(value) = record.get(key) else {
                        continue;
                    };
                    let elapsed = time.saturating_sub(start);
                    let current = if length == 0 || elapsed >= length {
                        target
                    } else {
                        from + (target - from) * elapsed as f64 / length as f64
                    };
                    from = current;
                    target = if key == "pos" {
                        value
                            .trim_start_matches('[')
                            .split(',')
                            .next()
                            .unwrap()
                            .parse()
                            .unwrap()
                    } else {
                        value.parse().unwrap()
                    };
                    start = time;
                    length = record["rampLength"].parse().unwrap();
                }
                let elapsed = sample.saturating_sub(start);
                curve.push(if length == 0 || elapsed >= length {
                    target
                } else {
                    from + (target - from) * elapsed as f64 / length as f64
                });
            }
            curve
        }

        #[test]
        fn discrete_changes_at_ramp_start_middle_and_end_preserve_position_and_gain() {
            let (mut batch, selected) = batch();
            let baseline =
                build_metadata(&batch, &[selected], 48_000, &mut WarningSet::default()).unwrap();
            for (sample, head, far, order) in [
                (1000, true, false, 2),
                (1250, false, false, 3),
                (1500, true, true, 4),
                (1500, false, true, 5),
                (1750, false, true, 6),
                (2000, true, true, 7),
            ] {
                batch.headphone_events.push(HeadphoneMetadataEvent {
                    sample_position: sample,
                    element_id: selected.scene.element_id,
                    stream_order: order,
                    policy: policy(head, far),
                });
            }
            let output =
                build_metadata(&batch, &[selected], 48_000, &mut WarningSet::default()).unwrap();
            let baseline = object_records(&baseline);
            let output = object_records(&output);
            assert_eq!(output.len(), 5, "同刻更新应合并，重复策略不应产生记录");
            for sample in [1250, 1500, 2000] {
                let record = output
                    .iter()
                    .find(|record| record["samplePos"] == sample.to_string())
                    .unwrap();
                assert_eq!(
                    record.len(),
                    3,
                    "局部事件只应包含 ID、samplePos 与一个已改变的字段"
                );
                assert!(!record.contains_key("rampLength"));
                assert!(!record.contains_key("pos"));
                assert!(!record.contains_key("gain"));
            }
            assert_eq!(
                continuous_curve(&baseline, "pos"),
                continuous_curve(&output, "pos")
            );
            assert_eq!(
                continuous_curve(&baseline, "gain"),
                continuous_curve(&output, "gain")
            );
            assert_eq!(continuous_curve(&output, "pos").get(1500), Some(&0.0));
            assert_eq!(continuous_curve(&output, "gain").get(1500), Some(&-22.0));
            let combined = output
                .iter()
                .find(|record| record["samplePos"] == "1000")
                .unwrap();
            assert_eq!(combined["rampLength"], "1000");
            assert_eq!(combined["headTrackMode"], "head relative");
        }

        #[test]
        fn object_headphone_only_updates_do_not_retarget_an_existing_ramp() {
            let (mut batch, selected) = batch();
            let mut repeated = *batch.events.get(1).unwrap();
            repeated.sample_position = 1500;
            repeated.stream_order = 9;
            batch.events.push(repeated);
            batch.headphone_only_orders.insert(9);
            batch.headphone_events.push(HeadphoneMetadataEvent {
                sample_position: 1500,
                element_id: selected.scene.element_id,
                stream_order: 9,
                policy: policy(true, false),
            });
            let output =
                build_metadata(&batch, &[selected], 48_000, &mut WarningSet::default()).unwrap();
            let records = object_records(&output);
            let patch = records
                .iter()
                .find(|record| record["samplePos"] == "1500")
                .unwrap();
            assert_eq!(patch.len(), 3);
            assert_eq!(continuous_curve(&records, "pos").get(1750), Some(&0.5));
        }

        #[test]
        fn policy_projection_preserves_preroll_and_uses_last_same_sample_value() {
            let (mut batch, selected) = batch();
            batch.media_span = Some(MediaSpan {
                start_sample: 2000,
                end_sample: 3500,
            });
            batch.headphone_events = vec![
                HeadphoneMetadataEvent {
                    sample_position: -100,
                    element_id: selected.scene.element_id,
                    stream_order: 0,
                    policy: policy(true, false),
                },
                HeadphoneMetadataEvent {
                    sample_position: 2500,
                    element_id: selected.scene.element_id,
                    stream_order: 1,
                    policy: policy(false, false),
                },
                HeadphoneMetadataEvent {
                    sample_position: 2500,
                    element_id: selected.scene.element_id,
                    stream_order: 2,
                    policy: policy(true, true),
                },
            ];
            let events = project_headphone_events(&batch, &selected.scene, 48_000, 48_000).unwrap();
            assert_eq!(
                events
                    .iter()
                    .map(|event| (event.sample, event.policy))
                    .collect::<Vec<_>>(),
                vec![
                    (0, HeadphonePolicyState::Unspecified),
                    (2000, policy(true, false)),
                    (2500, policy(true, true)),
                    (3500, HeadphonePolicyState::Unspecified),
                ]
            );
        }

        #[test]
        fn discrete_projection_rescales_absolute_samples_and_rejects_bad_bounds() {
            let (mut batch, selected) = batch();
            batch.sample_rate = 44_100;
            batch.duration_samples = 4410;
            batch.media_span = Some(MediaSpan {
                start_sample: 441,
                end_sample: 3528,
            });
            batch.headphone_events.push(HeadphoneMetadataEvent {
                sample_position: 882,
                element_id: selected.scene.element_id,
                stream_order: 1,
                policy: policy(true, true),
            });
            let events = project_headphone_events(&batch, &selected.scene, 48_000, 4800).unwrap();
            assert_eq!(
                events.iter().map(|event| event.sample).collect::<Vec<_>>(),
                vec![0, 480, 960, 3840]
            );
            assert!(project_headphone_events(&batch, &selected.scene, 48_000, 3800).is_err());
            batch.sample_rate = 0;
            assert!(project_headphone_events(&batch, &selected.scene, 48_000, 4800).is_err());
        }

        #[test]
        fn unsupported_policy_fails_and_mid_warning_depends_on_effective_policy() {
            let (mut batch, selected) = batch();
            batch.headphone_events.first_mut().unwrap().policy =
                HeadphonePolicyState::Unsupported(HeadphonePolicyIssue::ConflictingGroups);
            assert!(
                build_metadata(&batch, &[selected], 48_000, &mut WarningSet::default())
                    .unwrap_err()
                    .contains("conflicting headphone")
            );
            batch.headphone_events.first_mut().unwrap().policy = HeadphonePolicyState::Resolved(
                HeadphonePolicy::new(HeadphoneRenderMode::Mid, HeadTrackingPolicy::SceneRelative),
            );
            let mut warnings = WarningSet::default();
            let output = build_metadata(&batch, &[selected], 48_000, &mut warnings).unwrap();
            assert!(output.contains("binauralRenderMode: undefined"));
            assert!(!warnings.items.is_empty());
            batch.headphone_events.first_mut().unwrap().policy = policy(false, false);
            batch.events.first_mut().unwrap().additional.headphone =
                Some(macindecode_ac4_bitstream::oamd::ObjectHeadphone {
                    render_mode: 3,
                    head_tracking_disabled: true,
                });
            let mut warnings = WarningSet::default();
            let output = build_metadata(&batch, &[selected], 48_000, &mut warnings).unwrap();
            assert!(output.contains("binauralRenderMode: near"));
            assert!(
                warnings.items.is_empty(),
                "未采用的 raw Mid 不应触发 DAMF warning"
            );
        }
    }

    #[test]
    fn selector_parser_accepts_qualified_and_bare_values() {
        assert_eq!(parse_selector("2:7"), Ok((Some(2), 7)));
        assert_eq!(parse_selector("7"), Ok((None, 7)));
        assert!(parse_selector("2:7:9").is_err());
    }

    #[test]
    fn object_selection_rejects_ambiguity_and_all_objects_is_sorted_and_excludes_lfe() {
        let mut lfe = test_scene(1, 0);
        lfe.kind = MetadataElementKind::LfeBed;
        let batch = test_batch(vec![test_scene(3, 1), lfe, test_scene(2, 1)]);

        assert!(
            select_objects(&batch, &["1".to_owned()], false)
                .expect_err("裸对象号应有歧义")
                .contains("is ambiguous")
        );
        assert!(
            select_objects(&batch, &["2:1".to_owned(), "2:1".to_owned()], false)
                .expect_err("重复 selector 必须失败")
                .contains("selected twice")
        );
        let all = select_objects(&batch, &[], true).expect("all-objects 应成功");
        assert_eq!(
            all.iter()
                .map(|item| {
                    (
                        item.scene.substream_index,
                        item.scene.object_index,
                        item.damf_id,
                    )
                })
                .collect::<Vec<_>>(),
            [(2, 1, 10), (3, 1, 11)]
        );
    }

    #[test]
    fn bare_output_directory_uses_current_directory_as_parent() {
        assert_eq!(output_parent(Path::new("package")), Path::new("."));
    }

    #[test]
    fn rescale_44100_to_48000_uses_absolute_rational_rounding() {
        assert_eq!(rescale_u64(44_100, 44_100, 48_000), Ok(48_000));
    }

    #[test]
    fn edit_crop_rejects_a_ramp_crossing_sample_zero() {
        let scene = test_scene(2, 1);
        let event = MetadataEvent {
            sample_position: -1_000,
            element_id: scene.element_id,
            stream_order: 0,
            ramp_samples: 2_048,
            state: default_inactive_event().state,
            additional: AdditionalObjectMetadata::default(),
        };
        let batch = MetadataBatch {
            headphone_events: Vec::new(),
            headphone_only_orders: Default::default(),
            sample_rate: 48_000,
            duration_samples: 10_000,
            media_span: Some(MediaSpan {
                start_sample: 0,
                end_sample: 10_000,
            }),
            decode_mode: SceneSessionDecodeMode::Full,
            elements: vec![scene],
            events: vec![event],
        };
        let selected = SelectedObject { scene, damf_id: 10 };
        assert!(
            output_events(&batch, &selected, 10_000)
                .expect_err("穿过 edit 起点的 ramp 必须拒绝")
                .contains("ramp")
        );
    }

    #[test]
    fn metadata_events_follow_the_selected_scene_element_identity() {
        let scene = test_scene(2, 1);
        let mut selected_state = default_inactive_event().state;
        selected_state.active = true;
        let event = |element_id, stream_order, state| MetadataEvent {
            sample_position: 0,
            element_id,
            stream_order,
            ramp_samples: 0,
            state,
            additional: AdditionalObjectMetadata::default(),
        };
        let batch = MetadataBatch {
            headphone_events: Vec::new(),
            headphone_only_orders: Default::default(),
            sample_rate: 48_000,
            duration_samples: 1_000,
            media_span: Some(MediaSpan {
                start_sample: 0,
                end_sample: 1_000,
            }),
            decode_mode: SceneSessionDecodeMode::Full,
            elements: vec![scene],
            events: vec![
                event(
                    MetadataElementId::new(999),
                    0,
                    default_inactive_event().state,
                ),
                event(scene.element_id, 1, selected_state),
            ],
        };
        let selected = select_objects(&batch, &[], true).expect("应能选择对象");
        let selected = selected.first().expect("应选择一个对象");

        let output = output_events(&batch, selected, 1_000).expect("事件应可映射");
        assert_eq!(output.len(), 1);
        assert!(
            output.first().is_some_and(|event| event.state.active),
            "不得混入其他 Scene element 的事件"
        );
    }

    #[test]
    fn extended_precision_respects_axis_transform_and_absolute_z_sign() {
        let additional = AdditionalObjectMetadata {
            extended_position: Some(macindecode_ac4_bitstream::oamd::ExtendedPrecisionPosition {
                presence: 0b111,
                x: Some(0),
                y: Some(0),
                z: Some(0),
            }),
            ..AdditionalObjectMetadata::default()
        };
        let (x, y, z) = position(
            QuantizedPosition {
                x: 31,
                y: 31,
                z: -3,
                coding: PositionCoding::AbsoluteNegative,
            },
            additional,
        );
        assert!((x - 1.0 / 155.0).abs() < f64::EPSILON);
        assert!((y + 1.0 / 155.0).abs() < f64::EPSILON);
        assert!((z - (-3.0 / 15.0 - 1.0 / 75.0)).abs() < f64::EPSILON);

        let (_, _, differential_z) = position(
            QuantizedPosition {
                x: 31,
                y: 31,
                z: -3,
                coding: PositionCoding::Differential,
            },
            additional,
        );
        assert!((differential_z - (-3.0 / 15.0 + 1.0 / 75.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn object_event_maps_gain_priority_zone_size_and_headphone_fields() {
        let scene = test_scene(2, 1);
        let selected = SelectedObject { scene, damf_id: 10 };
        let event = OutputEvent {
            sample: 123,
            ramp: 512,
            state: ObjectMetadataState {
                active: false,
                basic: Some(ObjectBasicState {
                    gain: ObjectGainState::Quantized(42),
                    priority: ObjectPriorityState::Quantized(31),
                }),
                render: Some(ObjectRenderState {
                    position: QuantizedPosition {
                        x: 31,
                        y: 31,
                        z: 0,
                        coding: PositionCoding::AbsolutePositive,
                    },
                    zone: ZoneUpdate {
                        grouped_defaults: false,
                        group_zone_flag: Some(0b011),
                        zone_mask: Some(2),
                    },
                    other_properties: OtherPropertiesUpdate {
                        width: Some(WidthUpdate::Cartesian { x: 31, y: 15, z: 0 }),
                        screen_factor_code: Some(7),
                        depth_factor: Some(3),
                        ..OtherPropertiesUpdate::default()
                    },
                }),
            },
            additional: AdditionalObjectMetadata {
                trim_disabled: true,
                headphone: Some(macindecode_ac4_bitstream::oamd::ObjectHeadphone {
                    render_mode: 3,
                    head_tracking_disabled: true,
                }),
                ..AdditionalObjectMetadata::default()
            },
        };
        let mut lines = Vec::new();
        let mut warnings = WarningSet::default();
        append_object_event(
            &mut lines,
            &selected,
            event,
            HeadphonePolicyState::Resolved(HeadphonePolicy::new(
                HeadphoneRenderMode::Mid,
                HeadTrackingPolicy::HeadRelative,
            )),
            "2:1",
            &mut warnings,
        )
        .expect("字段应可映射");
        let metadata = lines.join("\n");
        for expected in [
            "samplePos: 123",
            "active: false",
            "snap: true",
            "elevation: false",
            "zones: no sides",
            "size3D: [1, 0.483870968, 0]",
            "importance: 1",
            "gain: -28",
            "rampLength: 512",
            "trimBypass: true",
            "screenFactor: 1",
            "depthFactor: 2",
            "headTrackMode: head relative",
            "binauralRenderMode: undefined",
        ] {
            assert!(metadata.contains(expected), "缺少 {expected:?}：{metadata}");
        }
        assert_eq!(warnings.items.len(), 1);
        assert_eq!(
            warnings.items.first().map(|warning| warning.field),
            Some("binaural_render_mode")
        );
    }

    #[test]
    fn oamd_head_tracking_changes_metadata_without_a_presentation_type_override() {
        let scene = test_scene(2, 1);
        let selected = SelectedObject { scene, damf_id: 10 };
        let event = |disabled| OutputEvent {
            sample: 0,
            additional: AdditionalObjectMetadata {
                headphone: Some(macindecode_ac4_bitstream::oamd::ObjectHeadphone {
                    render_mode: 1,
                    head_tracking_disabled: disabled,
                }),
                ..AdditionalObjectMetadata::default()
            },
            ..default_inactive_event()
        };
        let render = |disabled| {
            let mut lines = Vec::new();
            append_object_event(
                &mut lines,
                &selected,
                event(disabled),
                HeadphonePolicyState::Resolved(HeadphonePolicy::new(
                    HeadphoneRenderMode::Near,
                    if disabled {
                        HeadTrackingPolicy::HeadRelative
                    } else {
                        HeadTrackingPolicy::SceneRelative
                    },
                )),
                "2:1",
                &mut WarningSet::default(),
            )
            .expect("headTrackMode 应可映射");
            lines.join("\n")
        };
        let scene_relative = render(false);
        let head_relative = render(true);
        assert!(scene_relative.contains("headTrackMode: scene relative"));
        assert!(head_relative.contains("headTrackMode: head relative"));
        assert_ne!(scene_relative, head_relative);
    }

    #[test]
    fn unsupported_zone_is_reported_and_reserved_zone_fails() {
        let mut warnings = WarningSet::default();
        assert_eq!(
            zone_fields(
                ZoneUpdate {
                    grouped_defaults: false,
                    group_zone_flag: Some(0b100),
                    zone_mask: Some(6),
                },
                "2:1",
                Some(0),
                &mut warnings,
            ),
            Ok((false, true, "all"))
        );
        assert_eq!(warnings.items.len(), 1);
        assert!(
            zone_fields(
                ZoneUpdate {
                    grouped_defaults: false,
                    group_zone_flag: Some(0b100),
                    zone_mask: Some(7),
                },
                "2:1",
                Some(0),
                &mut warnings,
            )
            .is_err()
        );
    }

    #[test]
    fn pink_noise_is_deterministic_and_seeded_per_object() {
        let mut first = PinkNoise::new(7);
        let mut same = PinkNoise::new(7);
        let mut other = PinkNoise::new(8);
        for _ in 0..64 {
            assert_eq!(first.next(), same.next());
        }
        assert_ne!(PinkNoise::new(7).next(), other.next());
    }

    #[test]
    fn caf_is_deterministic_little_endian_and_uses_independent_object_seeds() {
        let first_path =
            std::env::temp_dir().join(format!("macinac4-caf-test-{}-a.caf", std::process::id()));
        let second_path =
            std::env::temp_dir().join(format!("macinac4-caf-test-{}-b.caf", std::process::id()));
        let selected = [
            SelectedObject {
                scene: test_scene(2, 1),
                damf_id: 10,
            },
            SelectedObject {
                scene: test_scene(2, 2),
                damf_id: 11,
            },
        ];
        write_caf(&first_path, 512, &selected, -18.0).expect("应能写第一份 CAF");
        write_caf(&second_path, 512, &selected, -18.0).expect("应能写第二份 CAF");
        let first = fs::read(&first_path).expect("应能读第一份 CAF");
        let second = fs::read(&second_path).expect("应能读第二份 CAF");
        assert_eq!(first, second, "相同 selector 的 CAF 必须逐字节确定");
        assert_eq!(first.get(32..36), Some(2u32.to_be_bytes().as_slice()));

        let channels = BED_CHANNELS.len() + selected.len();
        let frame = 500usize;
        let frame_start = 68usize + frame * channels * 3;
        let object_a = first
            .get(frame_start + BED_CHANNELS.len() * 3..frame_start + 11 * 3)
            .expect("对象 A 样本应存在");
        let object_b = first
            .get(frame_start + 11 * 3..frame_start + 12 * 3)
            .expect("对象 B 样本应存在");
        assert_ne!(object_a, [0, 0, 0]);
        assert_ne!(object_a, object_b, "不同 selector 必须使用独立种子");

        let mut generator = PinkNoise::new(selector_seed(&selected[0].scene));
        let mut expected = 0i32;
        for sample in 0..=frame {
            let edge = sample.min(511usize.saturating_sub(sample));
            let fade_gain = if edge < 480 { edge as f64 / 480.0 } else { 1.0 };
            expected = (generator.next() * 10.0f64.powf(-18.0 / 20.0) * SAMPLE_MAX * fade_gain)
                .round()
                .clamp(-SAMPLE_MAX, SAMPLE_MAX) as i32;
        }
        assert_eq!(
            object_a,
            expected.to_le_bytes().get(..3).unwrap_or(&[]),
            "24-bit payload 应取有符号样本的小端低三字节"
        );

        fs::remove_file(first_path).expect("应能清理第一份 CAF");
        fs::remove_file(second_path).expect("应能清理第二份 CAF");
    }

    #[test]
    fn full_home_and_3dof_manifests_only_change_version_and_type() {
        let selected = [SelectedObject {
            scene: test_scene(2, 1),
            damf_id: 10,
        }];
        let mut home_warnings = WarningSet::default();
        let home = build_manifest(
            "full",
            "29.97df",
            &selected,
            DamfPresentationType::Home,
            DamfEssence::Full { has_lfe: true },
            &mut home_warnings,
        );
        let mut dof_warnings = WarningSet::default();
        let dof = build_manifest(
            "full",
            "29.97df",
            &selected,
            DamfPresentationType::ThreeDof,
            DamfEssence::Full { has_lfe: true },
            &mut dof_warnings,
        );
        assert!(home_warnings.items.is_empty());
        assert!(dof_warnings.items.is_empty());
        assert_eq!(
            home.replacen("version: 0.5.1", "version: 0.6.0", 1)
                .replacen("  - type: home", "  - type: 3dof", 1),
            dof
        );
        assert!(home.contains("creationTool: MacinDecode-AC4-Core full A-JOC"));
        assert!(home.contains("AC-4 full object 2:1"));
    }
}
