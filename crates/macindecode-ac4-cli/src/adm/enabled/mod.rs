//! ADM 导出流程门面。

mod axml;
mod dbmd;
mod full;
mod wave;

use axml::*;
use dbmd::*;
use wave::*;

use super::*;
use crate::metadata_batch::{
    MetadataBatch, MetadataElement, OutputMetadataEvent, project_metadata_events,
};
#[cfg(test)]
use crate::scene_batch::SceneBatchPath;
use crate::scene_batch::{DiagnosticSceneBatch, SceneBatchError, collect_diagnostic_scene_batch};
use crate::scene_export::{
    BYTES_PER_SAMPLE, MAX_PROBE_OBJECTS, MappingWarning, OUTPUT_SAMPLE_RATE, PinkNoise, SAMPLE_MAX,
    WarningSet, position, rescale_u64, scene_selector, select_metadata_elements, selector_seed,
    validate_selected_common as validate_scene_common, zone_components,
};
use crate::wire::{CliError, DiagnosticCode};
use crate::{AdmCompatibility, DecodeMode, MasterFrameRate};
use macindecode_ac4_bitstream::oamd::{
    ObjectGainState, ObjectPriorityState, OtherPropertiesUpdate, WidthUpdate, ZoneUpdate,
};
use macindecode_ac4_scene::{DecodeMode as SceneSessionDecodeMode, PresentationSelection};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const DBMD_VERSION: [u8; 4] = [0x06, 0x00, 0x00, 0x01];
const DBMD_ATMOS_SUPPLEMENTAL_SYNC: [u8; 4] = [0xbd, 0x6f, 0x72, 0xf8];
const ADM_PROGRAMME_NAME: &str = "Atmos_Master";
const BED_CHANNELS: [BedChannel; 10] = [
    BedChannel::new("RoomCentricLeft", "RC_L", -1.0, 1.0, 0.0),
    BedChannel::new("RoomCentricRight", "RC_R", 1.0, 1.0, 0.0),
    BedChannel::new("RoomCentricCenter", "RC_C", 0.0, 1.0, 0.0),
    BedChannel::new("RoomCentricLFE", "RC_LFE", -1.0, 1.0, -1.0),
    BedChannel::new("RoomCentricLeftSideSurround", "RC_Lss", -1.0, 0.0, 0.0),
    BedChannel::new("RoomCentricRightSideSurround", "RC_Rss", 1.0, 0.0, 0.0),
    BedChannel::new("RoomCentricLeftRearSurround", "RC_Lrs", -1.0, -1.0, 0.0),
    BedChannel::new("RoomCentricRightRearSurround", "RC_Rrs", 1.0, -1.0, 0.0),
    BedChannel::new("RoomCentricLeftTopSurround", "RC_Lts", -1.0, 0.0, 1.0),
    BedChannel::new("RoomCentricRightTopSurround", "RC_Rts", 1.0, 0.0, 1.0),
];

#[derive(Debug, Clone, Copy)]
struct BedChannel {
    name: &'static str,
    label: &'static str,
    x: f64,
    y: f64,
    z: f64,
}

impl BedChannel {
    const fn new(name: &'static str, label: &'static str, x: f64, y: f64, z: f64) -> Self {
        Self {
            name,
            label,
            x,
            y,
            z,
        }
    }
}

impl AdmCompatibility {
    const fn container_id(self) -> &'static [u8; 4] {
        match self {
            Self::Standard => b"BW64",
            Self::Logic => b"RF64",
        }
    }

    const fn container_name(self) -> &'static str {
        match self {
            Self::Standard => "BW64",
            Self::Logic => "RF64",
        }
    }

    const fn clock_units_per_second(self) -> u64 {
        match self {
            Self::Standard => 1_000_000_000,
            Self::Logic => 100_000,
        }
    }

    const fn clock_fraction_digits(self) -> usize {
        match self {
            Self::Standard => 9,
            Self::Logic => 5,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SelectedObject {
    scene: MetadataElement,
    ordinal: u16,
    track_index: u16,
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

pub(super) fn run_full(args: ExportFullAdmBwfArgs) -> Result<String, CliError> {
    full::run(args)
}

pub(super) fn run(args: ExportAdmBwfArgs) -> Result<String, CliError> {
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
    format_sample_time(duration, args.compatibility)
        .map_err(|message| cli_error(DiagnosticCode::MappingUnsupported, message))?;

    let mut warnings = WarningSet::default();
    append_common_warnings(&selected, &mut warnings);
    let axml = build_axml(
        ADM_PROGRAMME_NAME,
        &metadata,
        &selected,
        duration,
        args.compatibility,
        &mut warnings,
    )
    .map_err(|message| cli_error(DiagnosticCode::MappingUnsupported, message))?;
    let chna = build_chna(&selected)
        .map_err(|message| cli_error(DiagnosticCode::MappingUnsupported, message))?;
    let dbmd = match args.compatibility {
        AdmCompatibility::Standard => None,
        AdmCompatibility::Logic => Some(
            build_logic_dbmd(BED_CHANNELS.len().saturating_add(selected.len()), args.fps)
                .map_err(|message| cli_error(DiagnosticCode::MappingUnsupported, message))?,
        ),
    };
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
    write_atomic_adm(
        &args.output,
        duration,
        &selected,
        args.probe_level_dbfs,
        axml.as_bytes(),
        &chna,
        dbmd.as_deref(),
        args.compatibility,
    )?;
    summary_json(
        &args.output,
        duration,
        &selected,
        axml.len(),
        dbmd.as_ref().map_or(0, Vec::len),
        args.compatibility,
        &warnings.items,
    )
    .map_err(|message| cli_error(DiagnosticCode::SerializationFailed, message))
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
            "Selected {} objects; the 7.1.2 ADM audition probe supports at most {MAX_PROBE_OBJECTS}",
            chosen.len()
        ));
    }
    chosen
        .into_iter()
        .enumerate()
        .map(|(index, scene)| {
            let ordinal = u16::try_from(index.saturating_add(1))
                .map_err(|_| "ADM object ordinal exceeds u16".to_owned())?;
            let track_index =
                u16::try_from(BED_CHANNELS.len().saturating_add(index.saturating_add(1)))
                    .map_err(|_| "ADM track index exceeds u16".to_owned())?;
            Ok(SelectedObject {
                scene,
                ordinal,
                track_index,
            })
        })
        .collect()
}

fn validate_selected_common(selected: &[SelectedObject]) -> Result<(), String> {
    let scenes = selected
        .iter()
        .map(|object| object.scene)
        .collect::<Vec<_>>();
    validate_scene_common(&scenes).map_err(|error| error.to_string())
}

fn append_common_warnings(selected: &[SelectedObject], warnings: &mut WarningSet) {
    let common = selected.first().and_then(|item| item.scene.common);
    if selected.iter().any(|item| item.scene.common != common) {
        warnings.push(
            "presentation",
            None,
            "common",
            "Selected A-JOC substreams have inconsistent presentation-level OAMD common metadata; ADM omits the conflicting value",
        );
    }
    if selected.iter().any(|item| item.scene.common_conflict) {
        warnings.push(
            "presentation",
            None,
            "common",
            "OAMD common changes over the input timeline and cannot be represented exactly by a static ADM programme",
        );
    }
    let Some(common) = common else {
        return;
    };
    if common.master_screen_size_ratio_code.is_some() {
        warnings.push(
            "presentation",
            None,
            "screen",
            "OAMD common screen ratio is not mapped to ADM referenceScreen",
        );
    }
    if common.bed_object_chan_distribute {
        warnings.push(
            "presentation",
            None,
            "bed_distribution",
            "The OAMD bed/object channel-distribution tool has no ADM programme equivalent",
        );
    }
    if common.trim.present {
        warnings.push(
            "presentation",
            None,
            "trim",
            "OAMD trim has no general ADM equivalent",
        );
    }
    if common.bed_render_info.present {
        warnings.push(
            "presentation",
            None,
            "bed_render_info",
            "The OAMD bed render/downmix tool has no general ADM equivalent",
        );
    }
    if common.headphone.present {
        warnings.push(
            "presentation",
            None,
            "headphone",
            "OAMD common headphone mode cannot be converted losslessly to ADM headphoneVirtualise",
        );
    }
}

fn output_parent(output: &Path) -> &Path {
    output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn summary_json(
    output: &Path,
    duration: u64,
    selected: &[SelectedObject],
    axml_bytes: usize,
    dbmd_bytes: usize,
    compatibility: AdmCompatibility,
    warnings: &[MappingWarning],
) -> Result<String, String> {
    let output = fs::canonicalize(output)
        .map_err(|error| format!("Failed to canonicalize output path: {error}"))?;
    let objects = selected
        .iter()
        .map(|object| {
            format!(
                "{{\"selector\":{},\"track_index\":{},\"audio_object_id\":{}}}",
                json_quote(&scene_selector(&object.scene)),
                object.track_index,
                json_quote(&object_id(object.track_index))
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
        "{{\"file\":{},\"container\":{},\"compatibility\":{},\"adm_version\":\"ITU-R_BS.2076-2\",\"sample_rate\":48000,\"bit_depth\":24,\"channels\":{},\"duration_samples\":{duration},\"axml_bytes\":{axml_bytes},\"dbmd_bytes\":{dbmd_bytes},\"objects\":[{objects}],\"unmapped\":[{warning_json}]}}",
        json_quote(&output.display().to_string()),
        json_quote(compatibility.container_name()),
        json_quote(match compatibility {
            AdmCompatibility::Standard => "standard",
            AdmCompatibility::Logic => "logic",
        }),
        BED_CHANNELS.len().saturating_add(selected.len()),
    ))
}

fn bed_pack_id() -> String {
    "AP_00011001".to_owned()
}

fn bed_channel_id(index: u16) -> String {
    format!("AC_0001{:04x}", 0x1000u16.saturating_add(index))
}

fn object_pack_id(ordinal: u16) -> String {
    format!("AP_0003{:04x}", 0x1000u16.saturating_add(ordinal))
}

fn object_channel_id(ordinal: u16) -> String {
    format!("AC_0003{:04x}", 0x1000u16.saturating_add(ordinal))
}

fn object_id(track_index: u16) -> String {
    format!("AO_{:04x}", 0x1000u16.saturating_add(track_index))
}

fn track_uid(track_index: u16) -> String {
    format!("ATU_{track_index:08x}")
}

fn block_id(channel_id: &str, block: u32) -> String {
    format!("AB_{}_{block:08x}", channel_id.trim_start_matches("AC_"))
}

fn format_sample_time(samples: u64, compatibility: AdmCompatibility) -> Result<String, String> {
    format_sample_time_at(samples, OUTPUT_SAMPLE_RATE, compatibility)
}

fn format_sample_time_at(
    samples: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
) -> Result<String, String> {
    let units =
        samples_to_clock_units_at(samples, sample_rate, compatibility.clock_units_per_second())?;
    format_clock_units(
        units,
        compatibility.clock_units_per_second(),
        compatibility.clock_fraction_digits(),
    )
}

fn format_sample_span_at(
    start: u64,
    end: u64,
    sample_rate: u32,
    compatibility: AdmCompatibility,
) -> Result<String, String> {
    let units_per_second = compatibility.clock_units_per_second();
    let start_units = samples_to_clock_units_at(start, sample_rate, units_per_second)?;
    let end_units = samples_to_clock_units_at(end, sample_rate, units_per_second)?;
    let duration_units = end_units
        .checked_sub(start_units)
        .ok_or("ADM block-duration underflow")?;
    format_clock_units(
        duration_units,
        units_per_second,
        compatibility.clock_fraction_digits(),
    )
}

fn samples_to_clock_units_at(
    samples: u64,
    sample_rate: u32,
    units_per_second: u64,
) -> Result<u64, String> {
    if sample_rate == 0 {
        return Err("ADM output sample rate is zero".to_owned());
    }
    let scaled = u128::from(samples)
        .checked_mul(u128::from(units_per_second))
        .ok_or("ADM time-rescaling overflow")?;
    let rounded = scaled
        .checked_add(u128::from(sample_rate / 2))
        .ok_or("ADM time-rounding overflow")?
        .checked_div(u128::from(sample_rate))
        .ok_or("ADM output sample rate is zero")?;
    u64::try_from(rounded).map_err(|_| "ADM time value exceeds u64".to_owned())
}

fn format_clock_units(
    units: u64,
    units_per_second: u64,
    fraction_digits: usize,
) -> Result<String, String> {
    let seconds = units
        .checked_div(units_per_second)
        .ok_or("ADM clock units per second is zero")?;
    let fraction = units
        .checked_rem(units_per_second)
        .ok_or("ADM clock units per second is zero")?;
    let hours = seconds / 3_600;
    if hours > 99 {
        return Err(
            "ADM BS.2076 time fields cannot represent durations of 100 hours or more".to_owned(),
        );
    }
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    Ok(format!(
        "{hours:02}:{minutes:02}:{seconds:02}.{fraction:0fraction_digits$}"
    ))
}

fn samples_to_nanoseconds_at(samples: u64, sample_rate: u32) -> Result<u64, String> {
    samples_to_clock_units_at(samples, sample_rate, 1_000_000_000)
}

fn format_seconds_span_at(start: u64, end: u64, sample_rate: u32) -> Result<String, String> {
    let start_ns = samples_to_nanoseconds_at(start, sample_rate)?;
    let end_ns = samples_to_nanoseconds_at(end, sample_rate)?;
    let nanoseconds = end_ns
        .checked_sub(start_ns)
        .ok_or("ADM interpolation-duration underflow")?;
    let seconds = nanoseconds / 1_000_000_000;
    let fraction = nanoseconds % 1_000_000_000;
    let mut out = format!("{seconds}.{fraction:09}");
    while out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.push('0');
    }
    Ok(out)
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
                write!(&mut out, "\\u{:04x}", u32::from(value))
                    .expect("writing to String cannot fail");
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
    let mut out = format!("{value:.12}");
    while out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.pop();
    }
    out
}

fn adm_io_error(error: std::io::Error) -> String {
    format!("Failed to write ADM BWF: {error}")
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::metadata_batch::{MetadataElementId, MetadataElementKind};

    #[test]
    fn scene_batch_error_reports_only_the_actual_scene_path() {
        let direct = batch_error(SceneBatchError::Unsupported {
            message: "direct-object 尚未覆盖".to_owned(),
            scene_path: Some(SceneBatchPath::DirectObject),
        });
        assert_eq!(direct.code, DiagnosticCode::UnsupportedCodingPath);
        assert_eq!(
            direct
                .context
                .get("scene_path")
                .and_then(serde_json::Value::as_str),
            Some("direct_object")
        );

        let unknown = batch_error(SceneBatchError::unsupported("尚未识别编码路径"));
        assert_eq!(unknown.code, DiagnosticCode::UnsupportedCodingPath);
        assert!(!unknown.context.contains_key("scene_path"));
    }

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

    #[test]
    fn sample_time_rounds_absolute_positions_to_nanoseconds() {
        assert_eq!(
            format_sample_time(2_048, AdmCompatibility::Standard).expect("时间应合法"),
            "00:00:00.042666667"
        );
        assert_eq!(
            format_sample_time(288_000, AdmCompatibility::Standard).expect("时间应合法"),
            "00:00:06.000000000"
        );
        assert_eq!(
            format_sample_span_at(2_048, 4_096, OUTPUT_SAMPLE_RATE, AdmCompatibility::Standard,)
                .expect("跨度应合法"),
            "00:00:00.042666666",
            "duration 必须使用相邻绝对端点之差维持十进制时间连续"
        );
    }

    #[test]
    fn logic_clock_uses_five_digits_and_absolute_endpoint_rounding() {
        assert_eq!(
            format_sample_time(2_048, AdmCompatibility::Logic).expect("时间应合法"),
            "00:00:00.04267"
        );
        assert_eq!(
            format_sample_time(4_096, AdmCompatibility::Logic).expect("时间应合法"),
            "00:00:00.08533"
        );
        assert_eq!(
            format_sample_span_at(2_048, 4_096, OUTPUT_SAMPLE_RATE, AdmCompatibility::Logic,)
                .expect("跨度应合法"),
            "00:00:00.04266"
        );
        assert_eq!(
            format_seconds_span_at(2_048, 4_096, OUTPUT_SAMPLE_RATE).expect("插值跨度应合法"),
            "0.042666666",
            "非时钟型 interpolationLength 保留原有精度"
        );
    }

    #[test]
    fn chna_uses_one_based_tracks_and_fixed_adm_ids() {
        let selected = [SelectedObject {
            scene: test_scene(2, 1),
            ordinal: 1,
            track_index: 11,
        }];
        let chna = build_chna(&selected).expect("CHNA 应可构造");
        assert_eq!(u16::from_le_bytes(chna[0..2].try_into().unwrap()), 11);
        assert_eq!(u16::from_le_bytes(chna[2..4].try_into().unwrap()), 11);
        let object = &chna[4 + 10 * 40..4 + 11 * 40];
        assert_eq!(u16::from_le_bytes(object[0..2].try_into().unwrap()), 11);
        assert_eq!(&object[2..14], b"ATU_0000000b");
        assert_eq!(&object[14..28], b"AT_00031001_01");
        assert_eq!(&object[28..39], b"AP_00031001");
        assert_eq!(object[39], 0);
    }

    #[test]
    fn small_files_are_forced_to_bw64_with_ds64() {
        let path =
            std::env::temp_dir().join(format!("macinac4-adm-bw64-test-{}.wav", std::process::id()));
        let selected = [SelectedObject {
            scene: test_scene(2, 1),
            ordinal: 1,
            track_index: 11,
        }];
        let chna = build_chna(&selected).expect("CHNA 应可构造");
        let file = File::create(&path).expect("应能创建测试文件");
        write_adm_wave(
            file,
            4,
            &selected,
            -18.0,
            b"<ebuCoreMain/>",
            &chna,
            None,
            AdmCompatibility::Standard,
        )
        .expect("应能写小型 BW64");
        let data = fs::read(&path).expect("应能读取测试文件");
        assert_eq!(data.get(..4), Some(b"BW64".as_slice()));
        assert_eq!(data.get(4..8), Some(u32::MAX.to_le_bytes().as_slice()));
        assert_eq!(data.get(8..12), Some(b"WAVE".as_slice()));
        assert_eq!(data.get(12..16), Some(b"ds64".as_slice()));
        assert_eq!(data.get(16..20), Some(28u32.to_le_bytes().as_slice()));
        let bw64_size = u64::from_le_bytes(data[20..28].try_into().unwrap());
        let data_size = u64::from_le_bytes(data[28..36].try_into().unwrap());
        assert_eq!(bw64_size, u64::try_from(data.len()).unwrap() - 8);
        assert_eq!(data_size, 4 * 11 * 3);
        assert_eq!(data.get(36..44), Some(0u64.to_le_bytes().as_slice()));
        assert_eq!(data.get(44..48), Some(0u32.to_le_bytes().as_slice()));
        assert_eq!(data.get(48..52), Some(b"fmt ".as_slice()));
        let data_offset = data
            .windows(4)
            .position(|window| window == b"data")
            .expect("应含 data chunk");
        assert_eq!(
            data.get(data_offset + 4..data_offset + 8),
            Some(u32::MAX.to_le_bytes().as_slice())
        );
        fs::remove_file(path).expect("应能清理测试文件");
    }

    #[test]
    fn logic_dbmd_is_structural_and_checksum_closed() {
        let dbmd = build_logic_dbmd(11, MasterFrameRate::Fps24).expect("应能生成 11 路 DBMD");
        assert_eq!(dbmd.len(), 526);
        assert_eq!(dbmd.get(..4), Some(DBMD_VERSION.as_slice()));

        let mut offset = 4usize;
        let mut segments = Vec::new();
        while dbmd[offset] != 0 {
            let id = dbmd[offset];
            let size = usize::from(u16::from_le_bytes([dbmd[offset + 1], dbmd[offset + 2]]));
            let payload_start = offset + 3;
            let payload_end = payload_start + size;
            let payload = &dbmd[payload_start..payload_end];
            let checksum = dbmd[payload_end];
            let sum = payload
                .iter()
                .fold(size as u8, |sum, byte| sum.wrapping_add(*byte));
            assert_eq!(sum.wrapping_add(checksum), 0, "segment {id} 校验和");
            segments.push((id, payload, checksum));
            offset = payload_end + 1;
        }
        assert_eq!(
            segments.iter().map(|item| item.0).collect::<Vec<_>>(),
            [7, 9, 10]
        );
        assert_eq!(segments[0].1.len(), 96);
        assert_eq!(segments[0].2, 0xad);
        assert_eq!(segments[1].1.len(), 248);
        assert_eq!(&segments[1].1[..22], b"Created by MacinDecode");
        assert_eq!(&segments[1].1[32..53], b"MacinDecode AC-4 Core");
        assert!(
            !dbmd
                .windows(b"Dolby".len())
                .any(|window| window == b"Dolby")
        );
        assert_eq!(&segments[1].1[96..99], &[0, 1, 0]);
        let supplemental = segments[2].1;
        assert_eq!(supplemental.len(), 164);
        assert_eq!(
            supplemental.get(..4),
            Some(DBMD_ATMOS_SUPPLEMENTAL_SYNC.as_slice())
        );
        assert_eq!(
            u16::from_le_bytes(supplemental[4..6].try_into().unwrap()),
            11
        );
        assert!(supplemental[142..153].iter().all(|value| *value == 0));
        assert_eq!(
            &supplemental[153..164],
            &[
                0x44, 0x44, 0x44, 0x40, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44
            ]
        );
        assert_eq!(&dbmd[offset..], &[0, 0]);
        assert!(build_logic_dbmd(129, MasterFrameRate::Fps24).is_err());
    }

    #[test]
    fn logic_dbmd_encodes_the_requested_frame_rate() {
        for (frame_rate, expected) in [
            (MasterFrameRate::Fps23976, 0x21),
            (MasterFrameRate::Fps24, 0x22),
            (MasterFrameRate::Fps25, 0x23),
            (MasterFrameRate::Fps2997, 0x25),
            (MasterFrameRate::Fps2997Drop, 0x24),
            (MasterFrameRate::Fps30, 0x26),
        ] {
            let dbmd = build_logic_dbmd(11, frame_rate).expect("应能生成 DBMD");
            // version + segment 7 + segment 9 header + Atmos payload offset 111。
            assert_eq!(dbmd[218], expected, "{}", frame_rate.as_str());
        }
    }

    #[test]
    fn json_escaping_is_deterministic() {
        assert_eq!(json_quote("a\n\""), "\"a\\n\\\"\"");
    }
}
