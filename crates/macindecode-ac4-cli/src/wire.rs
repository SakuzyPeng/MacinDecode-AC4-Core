//! CLI v1 成功响应与 JSON Lines 诊断。
//!
//! 内部 trace 继续使用面向解析与重建的状态；这里显式投影到 wire DTO，避免
//! 内部字段调整意外改变公共契约。

use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use crate::inspect::InspectReport;

pub(crate) const RESULT_SCHEMA: &str = "macinac4.cli-result";
pub(crate) const DIAGNOSTIC_SCHEMA: &str = "macinac4.cli-diagnostic";
pub(crate) const VERSION: u8 = 1;

/// 声明稳定诊断码及其 wire 名称。
///
/// 枚举与测试用的 `DiagnosticCode::ALL` 从同一份列表生成。此前手写测试清单时，
/// 新增 `UnsupportedCodingPath` 却在清单和发布 Schema 两边同时漏掉，所谓“一致性”
/// 测试因此比较了两份同样不完整的列表并假通过。
macro_rules! diagnostic_codes {
    ($( $variant:ident => $name:literal, )+) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        #[allow(dead_code, reason = "stable diagnostic codes are shared by all feature sets")]
        pub(crate) enum DiagnosticCode {
            $( #[serde(rename = $name)] $variant, )+
        }

        #[cfg(test)]
        impl DiagnosticCode {
            /// 全部诊断码，与枚举同源。
            const ALL: &'static [Self] = &[ $( Self::$variant, )+ ];
        }
    };
}

diagnostic_codes! {
    CliInvalidArguments => "cli.invalid_arguments",
    FeatureRequired => "feature.required",
    InputReadFailed => "input.read_failed",
    InputInvalid => "input.invalid",
    ParseFailed => "parse.failed",
    SelectionInvalid => "selection.invalid",
    MappingUnsupported => "mapping.unsupported",
    MappingLossy => "mapping.lossy",
    OutputExists => "output.exists",
    OutputCreateFailed => "output.create_failed",
    OutputWriteFailed => "output.write_failed",
    OutputCommitFailed => "output.commit_failed",
    UnsupportedCodingPath => "unsupported.coding_path",
    SerializationFailed => "serialization.failed",
    InternalInvariantFailed => "internal.invariant_failed",
}

#[derive(Debug)]
pub(crate) struct CliError {
    pub(crate) command: String,
    pub(crate) code: DiagnosticCode,
    pub(crate) message: String,
    pub(crate) context: BTreeMap<String, Value>,
}

impl CliError {
    pub(crate) fn new(
        command: impl Into<String>,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            command: command.into(),
            code,
            message: message.into(),
            context: BTreeMap::new(),
        }
    }

    pub(crate) fn with_context(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.context.insert(key.to_owned(), value.into());
        self
    }
}

#[derive(Serialize)]
struct Diagnostic<'a> {
    schema: &'static str,
    version: u8,
    level: &'a str,
    command: &'a str,
    code: DiagnosticCode,
    message: &'a str,
    context: &'a BTreeMap<String, Value>,
}

pub(crate) fn write_error(error: &CliError) {
    write_diagnostic(
        "error",
        &error.command,
        error.code,
        &error.message,
        &error.context,
    );
}

fn write_diagnostic(
    level: &str,
    command: &str,
    code: DiagnosticCode,
    message: &str,
    context: &BTreeMap<String, Value>,
) {
    let diagnostic = Diagnostic {
        schema: DIAGNOSTIC_SCHEMA,
        version: VERSION,
        level,
        command,
        code,
        message,
        context,
    };
    let stderr = std::io::stderr();
    let mut writer = stderr.lock();
    if serde_json::to_writer(&mut writer, &diagnostic).is_ok() {
        let _ = writer.write_all(b"\n");
    }
}

#[derive(Serialize)]
struct SuccessEnvelope<'a> {
    schema: &'static str,
    version: u8,
    command: &'a str,
    result: &'a SuccessResult,
}

#[derive(Serialize)]
#[serde(untagged)]
enum SuccessResult {
    Trace(Box<TraceResult>),
    Inspect(Box<InspectWireResult>),
    Export(Box<ExportResult>),
}

#[derive(Serialize)]
struct InspectWireResult {
    #[serde(rename = "inspectResult")]
    inspect_result: InspectReport,
}

pub(crate) struct PreparedSuccess {
    command: String,
    result: SuccessResult,
    warnings: Vec<Value>,
}

impl PreparedSuccess {
    pub(crate) fn write(self) -> Result<(), CliError> {
        for warning in &self.warnings {
            let mut context = BTreeMap::new();
            if let Some(object) = warning.as_object() {
                context.extend(
                    object
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
            }
            write_diagnostic(
                "warning",
                &self.command,
                DiagnosticCode::MappingLossy,
                "Some AC-4 metadata could not be mapped losslessly",
                &context,
            );
        }

        let envelope = SuccessEnvelope {
            schema: RESULT_SCHEMA,
            version: VERSION,
            command: &self.command,
            result: &self.result,
        };
        let stdout = std::io::stdout();
        let mut writer = stdout.lock();
        serde_json::to_writer_pretty(&mut writer, &envelope).map_err(|error| {
            CliError::new(
                &self.command,
                DiagnosticCode::SerializationFailed,
                "Failed to serialize success response",
            )
            .with_context("cause", error.to_string())
        })?;
        writer.write_all(b"\n").map_err(|error| {
            CliError::new(
                &self.command,
                DiagnosticCode::OutputWriteFailed,
                "Failed to write to standard output",
            )
            .with_context("cause", error.to_string())
        })
    }
}

#[derive(Serialize)]
struct TraceResult {
    source: TraceSource,
    frames: Value,
    validation: TraceValidation,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TraceSource {
    Mp4 {
        boxes: Value,
        track: Value,
        presentation: Value,
        dac4: Value,
        derived: Value,
        first_samples: Value,
    },
    AnnexG {
        payload_bytes: Value,
        escaped_size_frames: Value,
        crc: Value,
        first_frames: Value,
    },
}

#[derive(Serialize)]
struct TraceValidation {
    topology: ValidationSection,
    oamd: ValidationSection,
    audio_substream: ValidationSection,
    ajoc: Option<ValidationSection>,
}

#[derive(Serialize)]
struct ValidationSection {
    coverage: Value,
    references: Value,
    timing: Value,
    configuration: Value,
    spectrum: Value,
    pcm: Value,
    invariants: Value,
    observations: Value,
}

impl ValidationSection {
    fn empty() -> Self {
        Self {
            coverage: json!({}),
            references: json!({}),
            timing: json!({}),
            configuration: json!({}),
            spectrum: json!({}),
            pcm: json!({}),
            invariants: json!({}),
            observations: json!({}),
        }
    }
}

#[derive(Serialize)]
struct Artifact {
    kind: String,
    path: String,
    bytes: u64,
}

#[derive(Serialize)]
struct ExportAudio {
    sample_rate_hz: Value,
    bit_depth: Value,
    channels: Value,
    frames: Value,
    format: String,
}

#[derive(Serialize)]
struct ExportResult {
    artifacts: Vec<Artifact>,
    audio: ExportAudio,
    objects: Value,
    unmapped: Value,
    #[serde(flatten)]
    details: BTreeMap<String, Value>,
}

pub(crate) fn prepare(command: &str, legacy: &str) -> Result<PreparedSuccess, CliError> {
    let normalized = normalize_nonfinite_json(legacy);
    let value: Value = serde_json::from_str(&normalized).map_err(|error| {
        CliError::new(
            command,
            DiagnosticCode::SerializationFailed,
            "Internal result is not valid JSON",
        )
        .with_context("cause", error.to_string())
    })?;
    let (result, warnings) = if command == "trace" {
        (
            SuccessResult::Trace(Box::new(trace_result(value, command)?)),
            Vec::new(),
        )
    } else {
        let export = export_result(command, value)?;
        let warnings = export.unmapped.as_array().cloned().unwrap_or_default();
        (SuccessResult::Export(Box::new(export)), warnings)
    };
    Ok(PreparedSuccess {
        command: command.to_owned(),
        result,
        warnings,
    })
}

/// 将 typed inspect 报告包装进 CLI v1 成功 envelope。
pub(crate) fn prepare_inspect(report: InspectReport) -> Result<PreparedSuccess, CliError> {
    Ok(PreparedSuccess {
        command: "inspect".to_owned(),
        result: SuccessResult::Inspect(Box::new(InspectWireResult {
            inspect_result: report,
        })),
        warnings: Vec::new(),
    })
}

fn trace_result(value: Value, command: &str) -> Result<TraceResult, CliError> {
    let mut root = object(value, command)?;
    let mut topology = object(
        root.remove("topology")
            .ok_or_else(|| missing(command, "topology"))?,
        command,
    )?;
    let oamd = object(
        topology.remove("oamd").unwrap_or_else(|| json!({})),
        command,
    )?;
    let audio = object(
        topology
            .remove("audio_substream")
            .unwrap_or_else(|| json!({})),
        command,
    )?;
    let ajoc = topology.remove("ajoc_audio");

    let validation = TraceValidation {
        topology: topology_validation(topology),
        oamd: oamd_validation(oamd),
        audio_substream: audio_validation(audio),
        ajoc: match ajoc {
            None | Some(Value::Null) => None,
            Some(value) => Some(ajoc_validation(object(value, command)?)),
        },
    };

    if root.contains_key("container") {
        let mut container = object(
            root.remove("container")
                .ok_or_else(|| missing(command, "container"))?,
            command,
        )?;
        let mut frames = object(
            root.remove("frames")
                .ok_or_else(|| missing(command, "frames"))?,
            command,
        )?;
        let source = TraceSource::Mp4 {
            boxes: container
                .remove("top_level_boxes")
                .unwrap_or_else(|| json!([])),
            track: Value::Object(container),
            presentation: root.remove("presentation").unwrap_or_else(|| json!({})),
            dac4: root.remove("dac4").unwrap_or_else(|| json!({})),
            derived: root.remove("derived").unwrap_or_else(|| json!({})),
            first_samples: frames.remove("first").unwrap_or_else(|| json!([])),
        };
        Ok(TraceResult {
            source,
            frames: Value::Object(frames),
            validation,
        })
    } else {
        let mut frames = object(
            root.remove("frames")
                .ok_or_else(|| missing(command, "frames"))?,
            command,
        )?;
        let source = TraceSource::AnnexG {
            payload_bytes: take(&mut frames, "payload_bytes"),
            escaped_size_frames: take(&mut frames, "escaped_frame_sizes"),
            crc: json!({
                "protected_frames": take(&mut frames, "crc_protected"),
                "failures": take(&mut frames, "crc_failures")
            }),
            first_frames: take(&mut frames, "first"),
        };
        Ok(TraceResult {
            source,
            frames: Value::Object(frames),
            validation,
        })
    }
}

fn topology_validation(mut values: Map<String, Value>) -> ValidationSection {
    let mut out = ValidationSection::empty();
    out.coverage = group(
        &mut values,
        &["frames_parsed", "parse_failures", "first_error"],
    );
    out.references = group(
        &mut values,
        &[
            "substream_size_overruns",
            "dangling_group_references",
            "substream_reference_failures",
        ],
    );
    out.timing = group(
        &mut values,
        &[
            "stss_random_access_mismatches",
            "full_random_access_frames",
            "audio_only_random_access_frames",
            "source_changes",
            "reset_events",
            "waiting_for_random_access_frames",
            "awaiting_random_access",
            "decoding_delay",
        ],
    );
    out.configuration = group(
        &mut values,
        &[
            "frames_differing_from_first",
            "scene_path",
            "presentations",
            "substream_groups",
            "total_objects",
            "config_generations",
        ],
    );
    out.observations = Value::Object(values);
    out
}

fn oamd_validation(mut values: Map<String, Value>) -> ValidationSection {
    let mut out = ValidationSection::empty();
    out.coverage = group(
        &mut values,
        &["located", "parsed", "failures", "first_error"],
    );
    out.timing = group(
        &mut values,
        &[
            "timing_frames",
            "timing_carryover_frames",
            "max_align_bits",
            "max_block_offset_samples",
            "max_ramp_duration",
        ],
    );
    let mut configuration = map_from(group(
        &mut values,
        &[
            "common_data_frames",
            "common_data_sync_mismatches",
            "dyndata_blocks",
            "history_dependent_blocks",
        ],
    ));
    configuration.insert(
        "object_info_blocks".to_owned(),
        range(&mut values, "min_obj_info_blocks", "max_obj_info_blocks"),
    );
    out.configuration = Value::Object(configuration);
    out.observations = Value::Object(values);
    out
}

fn audio_validation(mut values: Map<String, Value>) -> ValidationSection {
    let mut out = ValidationSection::empty();
    out.coverage = group(
        &mut values,
        &["located", "parsed", "failures", "first_error"],
    );
    let mut configuration = Map::new();
    configuration.insert(
        "audio_size_bytes".to_owned(),
        range(&mut values, "min_audio_size", "max_audio_size"),
    );
    configuration.insert(
        "metadata_bytes".to_owned(),
        range(&mut values, "min_metadata_bytes", "max_metadata_bytes"),
    );
    for key in [
        "max_tools_metadata_bits",
        "dialnorm_frames",
        "substream_loudness_frames",
    ] {
        configuration.insert(key.to_owned(), take(&mut values, key));
    }
    out.configuration = Value::Object(configuration);
    out.observations = Value::Object(values);
    out
}

fn ajoc_validation(mut values: Map<String, Value>) -> ValidationSection {
    let mut out = ValidationSection::empty();
    out.coverage = group(
        &mut values,
        &[
            "frames",
            "parsed",
            "substreams",
            "parsed_substreams",
            "failures",
            "first_error",
        ],
    );
    let mut timing = map_from(group(
        &mut values,
        &[
            "max_dmx_obj_info_blocks",
            "max_umx_obj_info_blocks",
            "dmx_object_info_blocks",
            "umx_object_info_blocks",
            "derive_timing_from_dmx",
            "intra_frame_update_frames",
        ],
    ));
    timing.insert(
        "fill_bits".to_owned(),
        range(&mut values, "min_fill_bits", "max_fill_bits"),
    );
    out.timing = Value::Object(timing);
    out.configuration = group(
        &mut values,
        &[
            "some_signals_inactive",
            "oamd_extension_present",
            "companding_frames",
            "companding_active_frames",
            "position_changes",
            "differential_positions",
            "state_failures",
        ],
    );
    let mut spectrum = map_from(group(
        &mut values,
        &[
            "aspx_add_harmonic_frames",
            "aspx_interleaved_frames",
            "aspx_variable_framing_frames",
            "aspx_balance_frames",
            "scale_factor_bands",
            "scale_factor_failures",
            "scale_factor_first_error",
            "scaled_lines",
            "scaled_peak",
            "scaled_nonfinite",
            "scale_failures",
            "scale_first_error",
            "ungrouped_lines",
            "ungroup_failures",
            "ungroup_count_mismatch",
            "ungroup_energy_drift",
            "ungroup_first_error",
        ],
    ));
    spectrum.insert(
        "scale_factor".to_owned(),
        range(&mut values, "scale_factor_min", "scale_factor_max"),
    );
    out.spectrum = Value::Object(spectrum);
    out.pcm = group(
        &mut values,
        &[
            "pcm_frames",
            "pcm_samples",
            "pcm_peak",
            "pcm_nonfinite",
            "synthesis_failures",
            "synthesis_first_error",
            "pcm_silent_input_frames",
            "pcm_zero_output_with_nonzero_input_frames",
            "ajoc_reconstruction_failures",
            "ajoc_reconstruction_first_error",
            "objects_nonfinite",
            "objects_nonfinite_first_error",
            "object_shape_mismatches",
            "object_shape_first_error",
        ],
    );
    out.invariants = Value::Object({
        let mut map = Map::new();
        map.insert(
            "reconstruction".to_owned(),
            take(&mut values, "reconstruction_invariants"),
        );
        map
    });
    out.observations = Value::Object(values);
    out
}

fn export_result(command: &str, value: Value) -> Result<ExportResult, CliError> {
    let mut legacy = object(value, command)?;
    let objects = legacy.remove("objects").unwrap_or_else(|| json!([]));
    let unmapped = legacy.remove("unmapped").unwrap_or_else(|| json!([]));
    let object_count = objects.as_array().map_or(0, Vec::len);
    let mut details = BTreeMap::new();

    match command {
        "export-damf" => {
            let manifest = string(&mut legacy, "manifest", command)?;
            let metadata = string(&mut legacy, "metadata", command)?;
            let audio_path = string(&mut legacy, "audio", command)?;
            let artifacts = vec![
                artifact("damf_manifest", &manifest, command)?,
                artifact("damf_metadata", &metadata, command)?,
                artifact("caf_audio", &audio_path, command)?,
            ];
            let audio = ExportAudio {
                sample_rate_hz: take(&mut legacy, "sample_rate"),
                bit_depth: json!(24),
                channels: json!(10usize.saturating_add(object_count)),
                frames: take(&mut legacy, "duration_samples"),
                format: "caf_s24le".to_owned(),
            };
            details.insert("package".to_owned(), json!({"stem_artifacts": 3}));
            Ok(ExportResult {
                artifacts,
                audio,
                objects,
                unmapped,
                details,
            })
        }
        "export-full-damf" => {
            let manifest = string(&mut legacy, "manifest", command)?;
            let metadata = string(&mut legacy, "metadata", command)?;
            let audio_path = string(&mut legacy, "audio", command)?;
            let artifacts = vec![
                artifact("full_damf_manifest", &manifest, command)?,
                artifact("full_damf_metadata", &metadata, command)?,
                artifact("full_damf_audio", &audio_path, command)?,
            ];
            let audio = ExportAudio {
                sample_rate_hz: take(&mut legacy, "sample_rate"),
                bit_depth: json!(24),
                channels: json!(10usize.saturating_add(object_count)),
                frames: take(&mut legacy, "duration_samples"),
                format: "caf_s24le".to_owned(),
            };
            details.insert(
                "package".to_owned(),
                json!({
                    "version": take(&mut legacy, "package_version"),
                    "type": take(&mut legacy, "presentation_type"),
                    "stem_artifacts": 3
                }),
            );
            for key in ["tracks", "scale", "bandwidth", "channel_order"] {
                details.insert(key.to_owned(), take(&mut legacy, key));
            }
            Ok(ExportResult {
                artifacts,
                audio,
                objects,
                unmapped,
                details,
            })
        }
        "export-adm-bwf" | "export-full-adm-bwf" => {
            let path = string(&mut legacy, "file", command)?;
            let artifact_kind = match command {
                "export-full-adm-bwf" => "full_adm_bwf",
                _ => "adm_bwf",
            };
            let artifacts = vec![artifact(artifact_kind, &path, command)?];
            let audio = ExportAudio {
                sample_rate_hz: take(&mut legacy, "sample_rate"),
                bit_depth: take(&mut legacy, "bit_depth"),
                channels: take(&mut legacy, "channels"),
                frames: take(&mut legacy, "duration_samples"),
                format: "pcm_s24le".to_owned(),
            };
            details.insert("profile".to_owned(), take(&mut legacy, "adm_version"));
            for key in ["container", "compatibility", "axml_bytes", "dbmd_bytes"] {
                details.insert(key.to_owned(), take(&mut legacy, key));
            }
            if command == "export-full-adm-bwf" {
                for key in ["tracks", "scale", "bandwidth", "channel_order"] {
                    details.insert(key.to_owned(), take(&mut legacy, key));
                }
            }
            Ok(ExportResult {
                artifacts,
                audio,
                objects,
                unmapped,
                details,
            })
        }
        "export-core-caf" => {
            let path = string(&mut legacy, "file", command)?;
            let artifacts = vec![artifact("core_speaker_caf", &path, command)?];
            let audio = ExportAudio {
                sample_rate_hz: take(&mut legacy, "sample_rate"),
                bit_depth: json!(32),
                channels: take(&mut legacy, "channels"),
                frames: take(&mut legacy, "frames"),
                format: legacy
                    .remove("format")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "caf_lpcm_f32le".to_owned()),
            };
            for key in [
                "container",
                "layout",
                "tracks",
                "scale",
                "bandwidth",
                "channel_order",
            ] {
                details.insert(key.to_owned(), take(&mut legacy, key));
            }
            Ok(ExportResult {
                artifacts,
                audio,
                objects,
                unmapped,
                details,
            })
        }
        "export-core-pcm" | "export-aspx-pcm" | "export-objects-pcm" => {
            let path = string(&mut legacy, "file", command)?;
            let kind = match command {
                "export-core-pcm" => "core_pcm_wave",
                "export-aspx-pcm" => "aspx_pcm_wave",
                "export-objects-pcm" => "objects_pcm_wave",
                _ => unreachable!("match arm is restricted to PCM commands"),
            };
            let artifacts = vec![artifact(kind, &path, command)?];
            let audio = ExportAudio {
                sample_rate_hz: take(&mut legacy, "sample_rate"),
                bit_depth: json!(32),
                channels: take(&mut legacy, "channels"),
                frames: take(&mut legacy, "frames"),
                format: legacy
                    .remove("format")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "wave_extensible_ieee_float32".to_owned()),
            };
            for key in ["tracks", "scale", "bandwidth"] {
                details.insert(key.to_owned(), take(&mut legacy, key));
            }
            if command != "export-core-pcm" {
                // 逐路顺序与核心带那份不同，必须自述，否则三层 WAVE 看不出区别。
                details.insert(
                    "channel_order".to_owned(),
                    take(&mut legacy, "channel_order"),
                );
            }
            Ok(ExportResult {
                artifacts,
                audio,
                objects,
                unmapped,
                details,
            })
        }
        _ => Err(CliError::new(
            command,
            DiagnosticCode::InternalInvariantFailed,
            "Unknown command result",
        )),
    }
}

fn artifact(kind: &str, path: &str, command: &str) -> Result<Artifact, CliError> {
    let bytes = std::fs::metadata(Path::new(path))
        .map_err(|error| {
            CliError::new(
                command,
                DiagnosticCode::InternalInvariantFailed,
                "Failed to read the generated artifact size",
            )
            .with_context("path", path)
            .with_context("cause", error.to_string())
        })?
        .len();
    Ok(Artifact {
        kind: kind.to_owned(),
        path: path.to_owned(),
        bytes,
    })
}

fn object(value: Value, command: &str) -> Result<Map<String, Value>, CliError> {
    value.as_object().cloned().ok_or_else(|| {
        CliError::new(
            command,
            DiagnosticCode::SerializationFailed,
            "Internal result has an invalid JSON shape",
        )
    })
}

fn string(values: &mut Map<String, Value>, key: &str, command: &str) -> Result<String, CliError> {
    values
        .remove(key)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| missing(command, key))
}

fn missing(command: &str, key: &str) -> CliError {
    CliError::new(
        command,
        DiagnosticCode::InternalInvariantFailed,
        "Internal result is missing a required field",
    )
    .with_context("field", key)
}

fn take(values: &mut Map<String, Value>, key: &str) -> Value {
    values.remove(key).unwrap_or(Value::Null)
}

fn range(values: &mut Map<String, Value>, min: &str, max: &str) -> Value {
    json!({
        "min": take(values, min),
        "max": take(values, max),
    })
}

fn group(values: &mut Map<String, Value>, keys: &[&str]) -> Value {
    let mut grouped = Map::new();
    for key in keys {
        if let Some(value) = values.remove(*key) {
            grouped.insert((*key).to_owned(), value);
        }
    }
    Value::Object(grouped)
}

fn map_from(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn normalize_nonfinite_json(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied_until = 0;
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;

    while let Some(&byte) = bytes.get(index) {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index = index.saturating_add(1);
            continue;
        }
        if byte == b'"' {
            in_string = true;
            index = index.saturating_add(1);
            continue;
        }
        let Some(token_len) = nonfinite_token_len(bytes, index) else {
            index = index.saturating_add(1);
            continue;
        };
        output.push_str(input.get(copied_until..index).unwrap_or_default());
        output.push_str("null");
        index = index.saturating_add(token_len);
        copied_until = index;
    }
    output.push_str(input.get(copied_until..).unwrap_or_default());
    output
}

fn nonfinite_token_len(input: &[u8], index: usize) -> Option<usize> {
    const TOKENS: [&[u8]; 6] = [b"-Infinity", b"Infinity", b"-inf", b"NaN", b"nan", b"inf"];

    let prefix_is_boundary = index == 0
        || input
            .get(index.saturating_sub(1))
            .is_some_and(|byte| json_value_boundary(*byte));
    if !prefix_is_boundary {
        return None;
    }
    TOKENS.into_iter().find_map(|token| {
        let end = index.checked_add(token.len())?;
        if input.get(index..end) != Some(token) {
            return None;
        }
        let suffix_is_boundary = input.get(end).is_none_or(|byte| json_value_boundary(*byte));
        suffix_is_boundary.then_some(token.len())
    })
}

const fn json_value_boundary(byte: u8) -> bool {
    matches!(
        byte,
        b':' | b',' | b'[' | b']' | b'{' | b'}' | b' ' | b'\n' | b'\r' | b'\t'
    )
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "测试以固定 JSON 属性核对非有限值归一化"
)]
mod tests {
    use super::*;

    static NEXT_ARTIFACT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[derive(Debug)]
    struct TestArtifacts {
        root: std::path::PathBuf,
    }

    impl TestArtifacts {
        fn new() -> Self {
            use std::sync::atomic::Ordering;

            let serial = NEXT_ARTIFACT_ROOT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "macinac4-wire-contract-{}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&root).expect("应能创建 wire contract 临时目录");
            Self { root }
        }

        fn write(&self, name: &str) -> String {
            let path = self.root.join(name);
            std::fs::write(&path, b"fixture").expect("应能写入 artifact 夹具");
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for TestArtifacts {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn prepared_stdout(command: &str, legacy: Value) -> Vec<u8> {
        let prepared = prepare(command, &legacy.to_string()).expect("wire 投影应成功");
        let envelope = SuccessEnvelope {
            schema: RESULT_SCHEMA,
            version: VERSION,
            command: &prepared.command,
            result: &prepared.result,
        };
        serde_json::to_vec(&envelope).expect("成功响应应可序列化")
    }

    fn damf_stdout(artifacts: &TestArtifacts) -> Vec<u8> {
        prepared_stdout(
            "export-damf",
            json!({
                "manifest": artifacts.write("probe.atmos"),
                "metadata": artifacts.write("probe.atmos.metadata"),
                "audio": artifacts.write("probe.atmos.audio"),
                "sample_rate": 48_000,
                "duration_samples": 2_048,
                "objects": [{"selector": "2:1"}],
                "unmapped": []
            }),
        )
    }

    fn full_damf_stdout(artifacts: &TestArtifacts) -> Vec<u8> {
        prepared_stdout(
            "export-full-damf",
            json!({
                "manifest": artifacts.write("full.atmos"),
                "metadata": artifacts.write("full.atmos.metadata"),
                "audio": artifacts.write("full.atmos.audio"),
                "sample_rate": 48_000,
                "duration_samples": 2_048,
                "package_version": "0.6.0",
                "presentation_type": "3dof",
                "objects": [{
                    "selector": "2:1",
                    "ajoc_object": 0,
                    "source_output_channel": 1,
                    "damf_id": 10,
                    "track_index": 11
                }],
                "tracks": [{
                    "damf_id": 3,
                    "track_index": 4,
                    "role": "bed",
                    "speaker": "LFE",
                    "essence": "lfe",
                    "source_output_channel": 0
                }],
                "scale": "internal_±32768_to_pcm_s24le",
                "bandwidth": "aspx",
                "channel_order": "7.1.2_bed_then_ajoc_full_objects",
                "unmapped": []
            }),
        )
    }

    #[test]
    fn nonfinite_numbers_become_json_null() {
        let normalized = normalize_nonfinite_json(
            r#"{"nan": NaN, "positive": inf, "negative": -inf, "long": Infinity, "values": [nan, -Infinity], "finite": 1.5}"#,
        );
        let value: Value = serde_json::from_str(&normalized).expect("归一化后应为 JSON");
        assert_eq!(value["nan"], Value::Null);
        assert_eq!(value["positive"], Value::Null);
        assert_eq!(value["negative"], Value::Null);
        assert_eq!(value["long"], Value::Null);
        assert_eq!(value["values"], json!([null, null]));
        assert_eq!(value["finite"], 1.5);
    }

    #[test]
    fn nonfinite_names_inside_json_strings_stay_unchanged() {
        let input =
            r#"{"path":"/tmp/probe: inf.wav","name":"NaN","escaped":"quote \": -Infinity"}"#;
        assert_eq!(normalize_nonfinite_json(input), input);
    }

    /// 前后都是 JSON 边界字符的 token：只有字符串状态跟踪能挡住它。
    ///
    /// 上一条用例里的三个 token 各自被前缀或后缀边界检查挡住，与是否跟踪字符串
    /// 无关——把 `in_string` 整个短路掉它依然通过。错误消息是 `{error:?}` 直接
    /// 塞进 JSON 字符串的，`inf` 落在两个边界字符之间完全可能发生。
    #[test]
    fn a_bounded_token_inside_a_string_needs_the_string_state() {
        let input = r#"{"detail":"drift inf , 见附录","drift":inf}"#;
        assert_eq!(
            normalize_nonfinite_json(input),
            r#"{"detail":"drift inf , 见附录","drift":null}"#
        );
    }

    #[test]
    fn every_export_projection_matches_the_schema_without_media() {
        let artifacts = TestArtifacts::new();
        let damf = damf_stdout(&artifacts);
        let full_damf = full_damf_stdout(&artifacts);
        let adm = prepared_stdout(
            "export-adm-bwf",
            json!({
                "file": artifacts.write("probe-adm.wav"),
                "sample_rate": 48_000,
                "bit_depth": 24,
                "channels": 11,
                "duration_samples": 2_048,
                "adm_version": "ITU-R_BS.2076-2",
                "container": "BW64",
                "compatibility": "standard",
                "axml_bytes": 1_024,
                "dbmd_bytes": null,
                "objects": [{"selector": "2:1"}],
                "unmapped": []
            }),
        );
        let core = prepared_stdout(
            "export-core-pcm",
            json!({
                "file": artifacts.write("probe-core.wav"),
                "format": "wave_extensible_ieee_float32",
                "sample_rate": 48_000,
                "channels": 2,
                "frames": 2_048,
                "tracks": [{"substream": 0, "element": 0, "channel": 0}],
                "scale": "±32768",
                "bandwidth": "core_only"
            }),
        );
        let aspx = prepared_stdout(
            "export-aspx-pcm",
            json!({
                "file": artifacts.write("probe-aspx.wav"),
                "format": "wave_extensible_ieee_float32",
                "sample_rate": 48_000,
                "channels": 2,
                "frames": 2_048,
                "tracks": [
                    {"substream": 0, "role": "ajoc_input", "ajoc_input": 0},
                    {"substream": 0, "role": "lfe"}
                ],
                "scale": "±32768",
                "bandwidth": "aspx",
                "channel_order": "ajoc_input_then_lfe"
            }),
        );
        let objects_pcm = prepared_stdout(
            "export-objects-pcm",
            json!({
                "file": artifacts.write("probe-objects.wav"),
                "format": "wave_extensible_ieee_float32",
                "sample_rate": 48_000,
                "channels": 3,
                "frames": 2_048,
                "tracks": [
                    {"substream": 0, "role": "ajoc_object", "ajoc_object": 0, "output_channel": 0},
                    {"substream": 0, "role": "lfe", "output_channel": 1},
                    {"substream": 0, "role": "ajoc_object", "ajoc_object": 1, "output_channel": 2}
                ],
                "scale": "±32768",
                "bandwidth": "aspx",
                "channel_order": "ajoc_objects_with_lfe_reinserted"
            }),
        );
        let full_adm = prepared_stdout(
            "export-full-adm-bwf",
            json!({
                "file": artifacts.write("probe-full-adm.wav"),
                "sample_rate": 48_000,
                "bit_depth": 24,
                "channels": 30,
                "duration_samples": 2_048,
                "adm_version": "ITU-R_BS.2076-2",
                "container": "RF64",
                "compatibility": "logic",
                "axml_bytes": 4_096,
                "dbmd_bytes": 564,
                "objects": [{
                    "selector": "2:1",
                    "role": "object",
                    "ajoc_object": 0,
                    "source_output_channel": 1,
                    "track_index": 11
                }],
                "tracks": [{
                    "track_index": 4,
                    "role": "bed",
                    "essence": "lfe",
                    "source_output_channel": 0
                }],
                "scale": "internal_±32768_to_pcm_s24le",
                "bandwidth": "aspx",
                "channel_order": "7.1.2_bed_then_ajoc_full_objects",
                "unmapped": []
            }),
        );
        let core_caf = prepared_stdout(
            "export-core-caf",
            json!({
                "file": artifacts.write("probe-core.caf"),
                "container": "CAF",
                "layout": "5.1.4",
                "format": "caf_lpcm_f32le",
                "sample_rate": 48_000,
                "channels": 10,
                "frames": 2_048,
                "objects": [{"selector": "2:1", "role": "object"}],
                "tracks": [{"track_index": 1, "speaker": "L", "role": "ajoc_input"}],
                "scale": "fixed_linear_gain=0.000030517578125;normalization=none;limiter=none",
                "bandwidth": "aspx",
                "channel_order": "L R C LFE Ls Rs Vhl Vhr Ltr Rtr",
                "unmapped": []
            }),
        );

        for (command, stdout) in [
            ("export-damf", damf),
            ("export-full-damf", full_damf),
            ("export-adm-bwf", adm),
            ("export-full-adm-bwf", full_adm),
            ("export-core-caf", core_caf),
            ("export-core-pcm", core),
            ("export-aspx-pcm", aspx),
            ("export-objects-pcm", objects_pcm),
        ] {
            crate::result_schema::success(command, &stdout);
        }
    }

    #[test]
    #[should_panic(expected = "键必须恰好匹配 export-damf")]
    fn export_schema_rejects_a_field_owned_by_another_command() {
        let artifacts = TestArtifacts::new();
        let stdout = damf_stdout(&artifacts);
        let mut value: Value = serde_json::from_slice(&stdout).expect("响应应为 JSON");
        value["result"]
            .as_object_mut()
            .expect("result 应为对象")
            .insert("tracks".to_owned(), json!([]));
        let changed = serde_json::to_vec(&value).expect("变异响应应可序列化");
        crate::result_schema::success("export-damf", &changed);
    }

    #[test]
    #[should_panic(expected = "$.result.source.crc 缺少 schema 声明的必需键")]
    fn annex_g_crc_is_checked_as_a_closed_object() {
        let stdout = prepared_stdout(
            "trace",
            json!({
                "topology": {},
                "frames": {
                    "payload_bytes": 0,
                    "escaped_frame_sizes": 0,
                    "crc_protected": 0,
                    "crc_failures": 0,
                    "first": []
                }
            }),
        );
        let mut value = crate::result_schema::success("trace", &stdout);
        value
            .pointer_mut("/result/source/crc")
            .and_then(Value::as_object_mut)
            .expect("crc 应为对象")
            .remove("failures");
        let changed = serde_json::to_vec(&value).expect("变异响应应可序列化");
        crate::result_schema::success("trace", &changed);
    }

    #[test]
    fn diagnostic_code_enum_matches_the_published_schema() {
        let schema: Value =
            serde_json::from_str(include_str!("../schema/cli-diagnostic-v1.schema.json"))
                .expect("诊断 schema 应为 JSON");
        let published = schema
            .pointer("/properties/code/enum")
            .and_then(Value::as_array)
            .expect("诊断 schema 应声明 code enum");
        let implemented = DiagnosticCode::ALL
            .iter()
            .copied()
            .map(|code| serde_json::to_value(code).expect("诊断 code 应可序列化"))
            .collect::<Vec<_>>();
        assert_eq!(implemented.as_slice(), published.as_slice());
    }

    #[test]
    fn success_schema_is_valid_json_and_covers_every_command() {
        let schema: Value =
            serde_json::from_str(include_str!("../schema/cli-result-v1.schema.json"))
                .expect("成功响应 schema 应为 JSON");
        assert_eq!(
            schema.pointer("/properties/command/enum"),
            Some(&json!([
                "trace",
                "inspect",
                "export-damf",
                "export-full-damf",
                "export-adm-bwf",
                "export-full-adm-bwf",
                "export-core-caf",
                "export-core-pcm",
                "export-aspx-pcm",
                "export-objects-pcm"
            ]))
        );
    }
}
