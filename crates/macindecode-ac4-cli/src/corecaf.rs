//! 固定 A-JOC core 对象网格到 CoreAudio 扬声器布局 CAF 的直接导出。
//!
//! `b_static_dmx = 0` 的 core 在规范上仍是对象，不是声道。本命令只接受本项目
//! 已验证的四套静态、单位增益、无空间修饰的 OAMD 网格；它把对应 `Qin_AJOC`
//! 直接重排到 Apple layout tag 的槽位。任何不能证明等价的输入都 fail closed。

use crate::ExportCoreCafArgs;
use crate::wire::{CliError, DiagnosticCode};

const COMMAND: &str = "export-core-caf";

fn caf_error(code: DiagnosticCode, message: impl Into<String>) -> CliError {
    CliError::new(COMMAND, code, message)
}

#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run(_args: ExportCoreCafArgs) -> Result<String, CliError> {
    Err(caf_error(
        DiagnosticCode::FeatureRequired,
        "export-core-caf requires rebuilding macinac4 with --features audio-decode",
    ))
}

#[cfg(feature = "audio-decode")]
mod enabled {
    use super::*;
    use crate::caf::{CafChannelLayout, FloatCafWriter};
    use crate::metadata_batch::{
        MetadataBatch, MetadataElement, OutputMetadataEvent, project_metadata_events,
    };
    use crate::scene_batch::{SceneBatchError, collect_core_scene_batch};
    use crate::scene_export::{
        CoreMappedPcm, CoreSceneError, CoreSceneErrorKind, CoreSourceSelection, map_core_pcm,
        scene_selector, select_core_sources, validate_selected_common, zone_components,
    };
    use macindecode_ac4_bitstream::oamd::{
        BedRenderInfo, ObjectGainState, Trim, TrimConfig, TrimConfigMode, WidthUpdate,
    };
    use macindecode_ac4_scene::PresentationSelection;
    use serde_json::{Value, json};
    use std::fs::{self, File, OpenOptions};
    use std::path::{Path, PathBuf};

    const PCM_SCALE: f32 = 1.0 / 32_768.0;
    const SCALE_DESCRIPTION: &str = "fixed_linear_gain=0.000030517578125;internal_±32768_to_pcm_f32le;normalization=none;limiter=none";
    const WRITE_BLOCK_FRAMES: usize = 1_024;
    const OBSERVED_ENCODER_TRIM_MODES: [TrimConfigMode; 9] = [
        TrimConfigMode::Default,
        TrimConfigMode::Disabled,
        TrimConfigMode::Disabled,
        TrimConfigMode::Default,
        TrimConfigMode::Disabled,
        TrimConfigMode::Default,
        TrimConfigMode::Default,
        TrimConfigMode::Disabled,
        TrimConfigMode::Default,
    ];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TrackSource {
        Q(usize),
        Lfe,
    }

    #[derive(Debug, Clone, Copy)]
    struct SpeakerTrack {
        label: &'static str,
        source: TrackSource,
    }

    #[derive(Debug)]
    struct CoreSpeakerLayout {
        caf: CafChannelLayout,
        positions: &'static [(u8, u8, i8)],
        tracks: &'static [SpeakerTrack],
    }

    const GRID_5: [(u8, u8, i8); 5] = [(0, 0, 0), (62, 0, 0), (31, 0, 0), (0, 62, 0), (62, 62, 0)];
    const GRID_7: [(u8, u8, i8); 7] = [
        (0, 0, 0),
        (62, 0, 0),
        (31, 0, 0),
        (0, 62, 0),
        (62, 62, 0),
        (0, 31, 15),
        (62, 31, 15),
    ];
    const GRID_9: [(u8, u8, i8); 9] = [
        (0, 0, 0),
        (62, 0, 0),
        (31, 0, 0),
        (0, 62, 0),
        (62, 62, 0),
        (0, 0, 15),
        (62, 0, 15),
        (0, 62, 15),
        (62, 62, 15),
    ];
    const GRID_11: [(u8, u8, i8); 11] = [
        (0, 0, 0),
        (62, 0, 0),
        (31, 0, 0),
        (0, 31, 0),
        (62, 31, 0),
        (0, 0, 15),
        (62, 0, 15),
        (0, 62, 15),
        (62, 62, 15),
        (0, 62, 0),
        (62, 62, 0),
    ];

    const TRACKS_51: [SpeakerTrack; 6] = [
        SpeakerTrack::new("L", TrackSource::Q(0)),
        SpeakerTrack::new("R", TrackSource::Q(1)),
        SpeakerTrack::new("C", TrackSource::Q(2)),
        SpeakerTrack::new("LFE", TrackSource::Lfe),
        SpeakerTrack::new("Ls", TrackSource::Q(3)),
        SpeakerTrack::new("Rs", TrackSource::Q(4)),
    ];
    const TRACKS_512: [SpeakerTrack; 8] = [
        SpeakerTrack::new("L", TrackSource::Q(0)),
        SpeakerTrack::new("R", TrackSource::Q(1)),
        SpeakerTrack::new("C", TrackSource::Q(2)),
        SpeakerTrack::new("LFE", TrackSource::Lfe),
        SpeakerTrack::new("Ls", TrackSource::Q(3)),
        SpeakerTrack::new("Rs", TrackSource::Q(4)),
        SpeakerTrack::new("Ltm", TrackSource::Q(5)),
        SpeakerTrack::new("Rtm", TrackSource::Q(6)),
    ];
    const TRACKS_514: [SpeakerTrack; 10] = [
        SpeakerTrack::new("L", TrackSource::Q(0)),
        SpeakerTrack::new("R", TrackSource::Q(1)),
        SpeakerTrack::new("C", TrackSource::Q(2)),
        SpeakerTrack::new("LFE", TrackSource::Lfe),
        SpeakerTrack::new("Ls", TrackSource::Q(3)),
        SpeakerTrack::new("Rs", TrackSource::Q(4)),
        SpeakerTrack::new("Vhl", TrackSource::Q(5)),
        SpeakerTrack::new("Vhr", TrackSource::Q(6)),
        SpeakerTrack::new("Ltr", TrackSource::Q(7)),
        SpeakerTrack::new("Rtr", TrackSource::Q(8)),
    ];
    const TRACKS_714: [SpeakerTrack; 12] = [
        SpeakerTrack::new("L", TrackSource::Q(0)),
        SpeakerTrack::new("R", TrackSource::Q(1)),
        SpeakerTrack::new("C", TrackSource::Q(2)),
        SpeakerTrack::new("LFE", TrackSource::Lfe),
        SpeakerTrack::new("Ls", TrackSource::Q(3)),
        SpeakerTrack::new("Rs", TrackSource::Q(4)),
        SpeakerTrack::new("Rls", TrackSource::Q(9)),
        SpeakerTrack::new("Rrs", TrackSource::Q(10)),
        SpeakerTrack::new("Vhl", TrackSource::Q(5)),
        SpeakerTrack::new("Vhr", TrackSource::Q(6)),
        SpeakerTrack::new("Ltr", TrackSource::Q(7)),
        SpeakerTrack::new("Rtr", TrackSource::Q(8)),
    ];

    static LAYOUT_51: CoreSpeakerLayout = CoreSpeakerLayout {
        caf: CafChannelLayout::Mpeg51A,
        positions: &GRID_5,
        tracks: &TRACKS_51,
    };
    static LAYOUT_512: CoreSpeakerLayout = CoreSpeakerLayout {
        caf: CafChannelLayout::Atmos512,
        positions: &GRID_7,
        tracks: &TRACKS_512,
    };
    static LAYOUT_514: CoreSpeakerLayout = CoreSpeakerLayout {
        caf: CafChannelLayout::Atmos514,
        positions: &GRID_9,
        tracks: &TRACKS_514,
    };
    static LAYOUT_714: CoreSpeakerLayout = CoreSpeakerLayout {
        caf: CafChannelLayout::Atmos714,
        positions: &GRID_11,
        tracks: &TRACKS_714,
    };

    impl SpeakerTrack {
        const fn new(label: &'static str, source: TrackSource) -> Self {
            Self { label, source }
        }
    }

    pub(super) fn run(args: ExportCoreCafArgs) -> Result<String, CliError> {
        ensure_output(&args.output)?;
        let data = fs::read(&args.input).map_err(|error| {
            caf_error(
                DiagnosticCode::InputReadFailed,
                format!("Failed to read {}: {error}", args.input.display()),
            )
        })?;
        let requested = match args.presentation {
            None => PresentationSelection::AutoUnique,
            Some(index) => PresentationSelection::Index(u32::try_from(index).map_err(|_| {
                caf_error(
                    DiagnosticCode::SelectionInvalid,
                    "Presentation index exceeds u32",
                )
            })?),
        };
        let batch = collect_core_scene_batch(&data, requested).map_err(batch_error)?;
        let metadata = batch.metadata;
        let pcm = batch.pcm;
        let selection = select_core_sources(&metadata).map_err(scene_error)?;
        validate_selected_common(&selection.objects)
            .map_err(|error| caf_error(DiagnosticCode::SelectionInvalid, error.to_string()))?;
        validate_direct_common(&selection)?;
        let mapped = map_core_pcm(&metadata, &pcm, &selection).map_err(scene_error)?;
        if mapped.frames == 0 {
            return Err(caf_error(
                DiagnosticCode::InputInvalid,
                "Presentation duration is zero after applying edits",
            ));
        }
        if selection.lfe.is_none() || mapped.lfe.is_none() {
            return Err(caf_error(
                DiagnosticCode::MappingUnsupported,
                "Direct-speaker CAF requires object 0 to be an LFE bed with independent PCM; silence substitution is not allowed",
            ));
        }
        let layout = layout_for_count(selection.objects.len())?;
        let duration = u64::try_from(mapped.frames).map_err(|_| {
            caf_error(
                DiagnosticCode::InternalInvariantFailed,
                "Core CAF PCM frame count exceeds u64",
            )
        })?;
        validate_speaker_grid(&metadata, &selection, layout, pcm.sample_rate, duration)?;
        write_atomic_caf(&args.output, pcm.sample_rate, &mapped, layout)?;
        summary_json(&args.output, pcm.sample_rate, &selection, &mapped, layout)
    }

    fn layout_for_count(count: usize) -> Result<&'static CoreSpeakerLayout, CliError> {
        match count {
            5 => Ok(&LAYOUT_51),
            7 => Ok(&LAYOUT_512),
            9 => Ok(&LAYOUT_514),
            11 => Ok(&LAYOUT_714),
            _ => Err(caf_error(
                DiagnosticCode::MappingUnsupported,
                format!(
                    "Core has {count} full-range objects; direct-speaker CAF accepts only verified 5/7/9/11-channel grids"
                ),
            )),
        }
    }

    fn validate_speaker_grid(
        metadata: &MetadataBatch,
        selection: &CoreSourceSelection,
        layout: &CoreSpeakerLayout,
        sample_rate: u32,
        duration: u64,
    ) -> Result<(), CliError> {
        for (index, scene) in selection.objects.iter().enumerate() {
            let expected = layout.positions.get(index).copied().ok_or_else(|| {
                caf_error(
                    DiagnosticCode::InternalInvariantFailed,
                    "Core speaker-grid template is shorter than the object count",
                )
            })?;
            let events = project_metadata_events(metadata, scene, sample_rate, duration).map_err(
                |error| {
                    caf_error(
                        DiagnosticCode::MappingUnsupported,
                        format!(
                            "The presentation timeline for object {} cannot be mapped directly: {error}",
                            scene_selector(scene)
                        ),
                    )
                },
            )?;
            let mut previous_is_valid = false;
            for event in events {
                if event.ramp > 0 && !previous_is_valid {
                    let predecessor = ramp_predecessor(metadata, scene, event.sample)?;
                    validate_grid_event(scene, expected, predecessor)?;
                }
                validate_grid_event(scene, expected, event)?;
                previous_is_valid = true;
            }
        }
        Ok(())
    }

    fn validate_direct_common(selection: &CoreSourceSelection) -> Result<(), CliError> {
        let reference = selection.objects.first().and_then(|scene| scene.common);
        for scene in &selection.objects {
            let selector = scene_selector(scene);
            if scene.common_conflict {
                return Err(caf_error(
                    DiagnosticCode::MappingUnsupported,
                    format!(
                        "OAMD common for core object {selector} changes over the presentation timeline; direct CAF cannot freeze dynamic common metadata"
                    ),
                )
                .with_context("selector", selector)
                .with_context("field", "common"));
            }
            if scene.common != reference {
                return Err(caf_error(
                    DiagnosticCode::MappingUnsupported,
                    "Selected core objects have inconsistent OAMD common metadata; direct CAF cannot choose a unique rendering configuration",
                )
                .with_context("selector", selector)
                .with_context("field", "common"));
            }
        }

        let Some(common) = reference else {
            return Ok(());
        };
        let unsupported = !common.default_screen_size_ratio
            || common.master_screen_size_ratio_code.is_some()
            || common.bed_object_chan_distribute
            || common.bed_render_info != BedRenderInfo::default()
            || !direct_trim_is_supported(common.trim);
        if unsupported {
            return Err(caf_error(
                DiagnosticCode::MappingUnsupported,
                "OAMD common uses screen, bed render/distribution, warp, or custom trim metadata not supported by direct-speaker CAF",
            )
            .with_context("field", "common"));
        }
        // common/per-object headphone 只描述耳机渲染，不改变本命令的扬声器出口。
        Ok(())
    }

    fn direct_trim_is_supported(trim: Trim) -> bool {
        if !trim.present {
            return true;
        }
        let no_custom_values = trim.configs.iter().all(trim_config_has_no_custom_values);
        if !no_custom_values {
            return false;
        }
        let modes = trim.configs.map(|config| config.mode);
        let mode_is_safe = match trim.global_trim_mode {
            0 | 1 => modes.iter().all(|mode| *mode == TrimConfigMode::Inherit),
            2 => modes
                .iter()
                .all(|mode| matches!(mode, TrimConfigMode::Default | TrimConfigMode::Disabled)),
            _ => false,
        };
        if !mode_is_safe {
            return false;
        }
        match trim.warp_mode {
            0 => true,
            // 当前已验证编码链固定写保留的 0b11，但同时使用这套无自定义系数的
            // default/disabled profile。直接网格本就只对该实测 profile 开放；其余
            // 保留值或组合不能据此外推。
            3 => trim.global_trim_mode == 2 && modes == OBSERVED_ENCODER_TRIM_MODES,
            _ => false,
        }
    }

    fn trim_config_has_no_custom_values(config: &TrimConfig) -> bool {
        config.centre.is_none()
            && config.surround.is_none()
            && config.height.is_none()
            && config.top_bottom_y.is_none()
            && config.listener_y.is_none()
    }

    fn ramp_predecessor(
        metadata: &MetadataBatch,
        scene: &MetadataElement,
        sample: u64,
    ) -> Result<OutputMetadataEvent, CliError> {
        let visible_start = metadata
            .media_span
            .ok_or_else(|| {
                caf_error(
                    DiagnosticCode::MappingUnsupported,
                    "Object ramp has no visible media span, so its pre-interpolation state cannot be verified",
                )
            })?
            .start_sample;
        let visible_start = i64::try_from(visible_start).map_err(|_| {
            caf_error(
                DiagnosticCode::MappingUnsupported,
                "Object-ramp media start exceeds i64, so its pre-interpolation state cannot be verified",
            )
        })?;
        let predecessor = metadata
            .events
            .iter()
            .filter(|event| {
                event.element_id == scene.element_id && event.sample_position < visible_start
            })
            .max_by_key(|event| (event.sample_position, event.stream_order))
            .ok_or_else(|| {
                caf_error(
                    DiagnosticCode::MappingUnsupported,
                    format!(
                        "Core object {} starts a nonzero ramp at sample {sample} without a verifiable preroll state",
                        scene_selector(scene)
                    ),
                )
                .with_context("selector", scene_selector(scene))
                .with_context("sample", sample)
            })?;
        Ok(OutputMetadataEvent {
            sample,
            ramp: 0,
            state: predecessor.state,
            additional: predecessor.additional,
        })
    }

    fn validate_grid_event(
        scene: &MetadataElement,
        expected: (u8, u8, i8),
        event: OutputMetadataEvent,
    ) -> Result<(), CliError> {
        let selector = scene_selector(scene);
        let reject = |detail: String| {
            caf_error(DiagnosticCode::MappingUnsupported, detail)
                .with_context("selector", selector.clone())
                .with_context("sample", event.sample)
        };
        if !event.state.active {
            return Err(reject(format!(
                "Core object {selector} is inactive at sample {}; writing PCM directly would bypass object muting",
                event.sample
            )));
        }
        let basic = event.state.basic.ok_or_else(|| {
            reject(format!(
                "Core object {selector} lacks a complete basic state at sample {}",
                event.sample
            ))
        })?;
        if basic.gain != ObjectGainState::Default {
            return Err(reject(format!(
                "Core object {selector} does not use the default 0 dB gain at sample {}",
                event.sample
            )));
        }
        let render = event.state.render.ok_or_else(|| {
            reject(format!(
                "Core object {selector} lacks a complete render state at sample {}",
                event.sample
            ))
        })?;
        let actual = (render.position.x, render.position.y, render.position.z);
        if actual != expected || event.additional.extended_position.is_some() {
            return Err(reject(format!(
                "Core object {selector} has position {:?} at sample {}, not fixed template {:?}, or uses extended coordinates",
                actual, event.sample, expected
            )));
        }
        if !neutral_width(render.other_properties.width) {
            return Err(reject(format!(
                "Core object {selector} uses nonzero width at sample {}",
                event.sample
            )));
        }
        if zone_components(render.zone) != (false, true, 0) {
            return Err(reject(format!(
                "Core object {selector} uses a non-default zone/channel lock at sample {}",
                event.sample
            )));
        }
        let other = render.other_properties;
        if other.screen_factor_code.is_some()
            || other.depth_factor.is_some()
            || other.object_at_infinity.is_some()
            || other.distance_factor_code.is_some()
            || other.divergence_mode.is_some()
            || other.divergence_table.is_some()
            || other.divergence_code.is_some()
        {
            return Err(reject(format!(
                "Core object {selector} uses screen/depth/distance/infinity/divergence spatial modifiers at sample {}",
                event.sample
            )));
        }
        Ok(())
    }

    fn neutral_width(width: Option<WidthUpdate>) -> bool {
        matches!(
            width,
            None | Some(WidthUpdate::Uniform(0))
                | Some(WidthUpdate::Cartesian { x: 0, y: 0, z: 0 })
        )
    }

    fn ensure_output(output: &Path) -> Result<(), CliError> {
        match fs::symlink_metadata(output) {
            Ok(_) => {
                return Err(caf_error(
                    DiagnosticCode::OutputExists,
                    format!("Output path already exists: {}", output.display()),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(caf_error(
                    DiagnosticCode::OutputCreateFailed,
                    format!("Failed to inspect output path: {error}"),
                ));
            }
        }
        let parent = output_parent(output);
        if !parent.is_dir() {
            return Err(caf_error(
                DiagnosticCode::OutputCreateFailed,
                format!(
                    "Output parent directory does not exist: {}",
                    parent.display()
                ),
            ));
        }
        Ok(())
    }

    fn output_parent(output: &Path) -> &Path {
        output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    fn create_temp_file(output: &Path) -> Result<(PathBuf, File), CliError> {
        let parent = output_parent(output);
        let base = output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("core.caf");
        for attempt in 0..100u32 {
            let candidate = parent.join(format!(".{base}.tmp-{}-{attempt}", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(file) => return Ok((candidate, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(caf_error(
                        DiagnosticCode::OutputCreateFailed,
                        format!("Failed to create temporary CAF: {error}"),
                    ));
                }
            }
        }
        Err(caf_error(
            DiagnosticCode::OutputCreateFailed,
            "Failed to allocate a unique temporary CAF path",
        ))
    }

    fn write_atomic_caf(
        output: &Path,
        sample_rate: u32,
        mapped: &CoreMappedPcm<'_>,
        layout: &CoreSpeakerLayout,
    ) -> Result<(), CliError> {
        let (temp, file) = create_temp_file(output)?;
        let result = (|| {
            let mut writer =
                FloatCafWriter::new(file, sample_rate, layout.caf).map_err(|error| {
                    caf_error(
                        DiagnosticCode::OutputWriteFailed,
                        format!("Failed to write CAF header: {error}"),
                    )
                })?;
            let capacity = layout
                .tracks
                .len()
                .checked_mul(WRITE_BLOCK_FRAMES)
                .ok_or_else(|| {
                    caf_error(
                        DiagnosticCode::InternalInvariantFailed,
                        "CAF interleave buffer length overflow",
                    )
                })?;
            let mut interleaved = Vec::with_capacity(capacity);
            for start in (0..mapped.frames).step_by(WRITE_BLOCK_FRAMES) {
                let end = start.saturating_add(WRITE_BLOCK_FRAMES).min(mapped.frames);
                interleaved.clear();
                for frame in start..end {
                    for track in layout.tracks {
                        let sample = source_sample(mapped, track.source, frame)?;
                        interleaved.push(sample * PCM_SCALE);
                    }
                }
                writer.write_interleaved(&interleaved).map_err(|error| {
                    caf_error(
                        DiagnosticCode::OutputWriteFailed,
                        format!("Failed to write CAF PCM: {error}"),
                    )
                })?;
            }
            let file = writer.finish().map_err(|error| {
                caf_error(
                    DiagnosticCode::OutputWriteFailed,
                    format!("Failed to finish CAF data chunk: {error}"),
                )
            })?;
            file.sync_all().map_err(|error| {
                caf_error(
                    DiagnosticCode::OutputWriteFailed,
                    format!("Failed to sync temporary CAF: {error}"),
                )
            })?;
            drop(file);
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
                // 同目录 hard link 原子发布完整 inode；目标名已成功提交后，临时链接
                // 清理失败不应把完整产物伪报为导出失败。
                drop(fs::remove_file(temp));
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(caf_error(
                DiagnosticCode::OutputExists,
                format!(
                    "Output path was created while writing: {}",
                    output.display()
                ),
            )),
            Err(error) => Err(caf_error(
                DiagnosticCode::OutputCommitFailed,
                format!("Failed to commit core CAF atomically without clobbering: {error}"),
            )),
        }
    }

    fn source_sample(
        mapped: &CoreMappedPcm<'_>,
        source: TrackSource,
        frame: usize,
    ) -> Result<f32, CliError> {
        let channel = match source {
            TrackSource::Q(index) => mapped.objects.get(index).copied(),
            TrackSource::Lfe => mapped.lfe,
        }
        .ok_or_else(|| {
            caf_error(
                DiagnosticCode::InternalInvariantFailed,
                format!("CAF track source {source:?} does not exist"),
            )
        })?;
        channel.samples.get(frame).copied().ok_or_else(|| {
            caf_error(
                DiagnosticCode::InternalInvariantFailed,
                format!("CAF track source {source:?} is shorter than the declared duration"),
            )
        })
    }

    fn summary_json(
        output: &Path,
        sample_rate: u32,
        selection: &CoreSourceSelection,
        mapped: &CoreMappedPcm<'_>,
        layout: &CoreSpeakerLayout,
    ) -> Result<String, CliError> {
        let output = fs::canonicalize(output).map_err(|error| {
            caf_error(
                DiagnosticCode::SerializationFailed,
                format!("Failed to canonicalize CAF output path: {error}"),
            )
        })?;
        let lfe = selection.lfe.as_ref().ok_or_else(|| {
            caf_error(
                DiagnosticCode::InternalInvariantFailed,
                "CAF summary is missing the verified LFE object",
            )
        })?;
        let tracks = layout
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| match track.source {
                TrackSource::Q(q) => {
                    let scene = selection.objects.get(q).ok_or_else(|| {
                        caf_error(
                            DiagnosticCode::InternalInvariantFailed,
                            "CAF summary q source exceeds the object count",
                        )
                    })?;
                    Ok(json!({
                        "track_index": index.saturating_add(1),
                        "speaker": track.label,
                        "role": "ajoc_input",
                        "selector": scene_selector(scene),
                        "ajoc_input": q,
                    }))
                }
                TrackSource::Lfe => Ok(json!({
                    "track_index": index.saturating_add(1),
                    "speaker": track.label,
                    "role": "lfe",
                    "selector": scene_selector(lfe),
                })),
            })
            .collect::<Result<Vec<Value>, CliError>>()?;
        let objects = selection
            .objects
            .iter()
            .enumerate()
            .map(|(q, scene)| {
                let (track_index, speaker) = layout
                    .tracks
                    .iter()
                    .enumerate()
                    .find_map(|(track_index, track)| {
                        (track.source == TrackSource::Q(q))
                            .then_some((track_index.saturating_add(1), track.label))
                    })
                    .ok_or_else(|| {
                        caf_error(
                            DiagnosticCode::InternalInvariantFailed,
                            format!("CAF layout does not reference q{q}"),
                        )
                    })?;
                Ok(json!({
                    "selector": scene_selector(scene),
                    "role": "object",
                    "ajoc_input": q,
                    "speaker": speaker,
                    "track_index": track_index,
                }))
            })
            .collect::<Result<Vec<Value>, CliError>>()?;
        serde_json::to_string(&json!({
            "file": output.display().to_string(),
            "container": "CAF",
            "layout": layout.caf.name(),
            "format": "caf_lpcm_f32le",
            "sample_rate": sample_rate,
            "channels": layout.caf.channels(),
            "frames": mapped.frames,
            "objects": objects,
            "tracks": tracks,
            "scale": SCALE_DESCRIPTION,
            "bandwidth": "aspx",
            "channel_order": layout.caf.channel_order(),
            "unmapped": [],
        }))
        .map_err(|error| {
            caf_error(
                DiagnosticCode::SerializationFailed,
                format!("Failed to serialize core CAF summary: {error}"),
            )
        })
    }

    fn batch_error(error: SceneBatchError) -> CliError {
        match error {
            SceneBatchError::Selection(message) => {
                caf_error(DiagnosticCode::SelectionInvalid, message)
            }
            SceneBatchError::Unsupported {
                message,
                scene_path,
            } if message.contains("b_static_dmx") => {
                let error = caf_error(DiagnosticCode::UnsupportedCodingPath, message)
                    .with_context("coding_path", "static_downmix");
                match scene_path {
                    Some(path) => error.with_context("scene_path", path.label()),
                    None => error,
                }
            }
            SceneBatchError::Unsupported {
                message,
                scene_path,
            } => {
                let error = caf_error(DiagnosticCode::UnsupportedCodingPath, message);
                match scene_path {
                    Some(path) => error.with_context("scene_path", path.label()),
                    None => error,
                }
            }
            SceneBatchError::Invariant(message) => {
                caf_error(DiagnosticCode::InternalInvariantFailed, message)
            }
            SceneBatchError::Failed(message) => caf_error(DiagnosticCode::ParseFailed, message),
        }
    }

    fn scene_error(error: CoreSceneError) -> CliError {
        let code = match error.kind {
            CoreSceneErrorKind::SelectionInvalid => DiagnosticCode::SelectionInvalid,
            CoreSceneErrorKind::UnsupportedCodingPath => DiagnosticCode::UnsupportedCodingPath,
            CoreSceneErrorKind::InternalInvariant => DiagnosticCode::InternalInvariantFailed,
        };
        error
            .context
            .into_iter()
            .fold(caf_error(code, error.message), |out, (key, value)| {
                out.with_context(key, value)
            })
    }

    #[cfg(test)]
    mod tests {
        #![allow(
            clippy::indexing_slicing,
            reason = "测试按固定 core 网格与 Apple 声道槽位核对映射"
        )]

        use super::*;
        use crate::metadata_batch::{
            MediaSpan, MetadataElementId, MetadataElementKind, MetadataEvent,
        };
        use macindecode_ac4_bitstream::oamd::{
            AdditionalObjectMetadata, ObjectBasicState, ObjectMetadataState, ObjectPriorityState,
            ObjectRenderState, OtherPropertiesUpdate, PositionCoding, QuantizedPosition,
            ZoneUpdate,
        };
        use macindecode_ac4_scene::DecodeMode;

        fn scene(object: u8) -> MetadataElement {
            MetadataElement {
                element_id: MetadataElementId::new(u64::from(object)),
                substream_index: 2,
                object_index: object,
                kind: MetadataElementKind::DynamicObject,
                common: None,
                common_conflict: false,
            }
        }

        fn event(position: (u8, u8, i8)) -> OutputMetadataEvent {
            OutputMetadataEvent {
                sample: 0,
                ramp: 0,
                state: ObjectMetadataState {
                    active: true,
                    basic: Some(ObjectBasicState {
                        gain: ObjectGainState::Default,
                        priority: ObjectPriorityState::Default,
                    }),
                    render: Some(ObjectRenderState {
                        position: QuantizedPosition {
                            x: position.0,
                            y: position.1,
                            z: position.2,
                            coding: PositionCoding::AbsolutePositive,
                        },
                        zone: ZoneUpdate {
                            grouped_defaults: true,
                            group_zone_flag: None,
                            zone_mask: None,
                        },
                        other_properties: OtherPropertiesUpdate {
                            grouped_defaults: true,
                            ..OtherPropertiesUpdate::default()
                        },
                    }),
                },
                additional: AdditionalObjectMetadata::default(),
            }
        }

        fn metadata_event(
            sample_position: i64,
            ramp_samples: u32,
            position: (u8, u8, i8),
        ) -> MetadataEvent {
            let output = event(position);
            MetadataEvent {
                sample_position,
                element_id: MetadataElementId::new(1),
                stream_order: 0,
                ramp_samples,
                state: output.state,
                additional: output.additional,
            }
        }

        fn speaker_batch(events: Vec<MetadataEvent>) -> MetadataBatch {
            MetadataBatch {
                sample_rate: 48_000,
                duration_samples: 1_000,
                media_span: Some(MediaSpan {
                    start_sample: 0,
                    end_sample: 1_000,
                }),
                decode_mode: DecodeMode::Core,
                elements: vec![scene(1)],
                events,
            }
        }

        #[test]
        fn layout_table_keeps_714_rear_pair_before_heights() {
            let layout = layout_for_count(11).unwrap();
            assert_eq!(layout.caf, CafChannelLayout::Atmos714);
            assert_eq!(layout.tracks[6].source, TrackSource::Q(9));
            assert_eq!(layout.tracks[7].source, TrackSource::Q(10));
            assert_eq!(layout.tracks[8].source, TrackSource::Q(5));
            assert_eq!(layout.tracks[11].source, TrackSource::Q(8));
        }

        #[test]
        fn layout_table_accepts_only_observed_core_widths() {
            for (count, name) in [(5, "5.1"), (7, "5.1.2"), (9, "5.1.4"), (11, "7.1.4")] {
                assert_eq!(layout_for_count(count).unwrap().caf.name(), name);
            }
            assert_eq!(
                layout_for_count(6).unwrap_err().code,
                DiagnosticCode::MappingUnsupported
            );
        }

        #[test]
        fn scene_batch_errors_keep_core_caf_diagnostic_classes() {
            assert_eq!(
                batch_error(SceneBatchError::Selection("presentation 歧义".to_owned())).code,
                DiagnosticCode::SelectionInvalid
            );
            assert_eq!(
                batch_error(SceneBatchError::unsupported("活动 DE")).code,
                DiagnosticCode::UnsupportedCodingPath
            );
            let static_downmix = batch_error(SceneBatchError::unsupported(
                "b_static_dmx 需要 channel-based core downmix",
            ));
            assert_eq!(static_downmix.code, DiagnosticCode::UnsupportedCodingPath);
            assert_eq!(
                static_downmix
                    .context
                    .get("coding_path")
                    .and_then(Value::as_str),
                Some("static_downmix")
            );
            assert_eq!(
                batch_error(SceneBatchError::Invariant("对象非有限".to_owned())).code,
                DiagnosticCode::InternalInvariantFailed
            );
            assert_eq!(
                batch_error(SceneBatchError::Failed("帧解析失败".to_owned())).code,
                DiagnosticCode::ParseFailed
            );
        }

        #[test]
        fn all_four_grid_templates_accept_their_exact_integer_positions() {
            for layout in [&LAYOUT_51, &LAYOUT_512, &LAYOUT_514, &LAYOUT_714] {
                for (index, position) in layout.positions.iter().copied().enumerate() {
                    let object = u8::try_from(index.saturating_add(1)).unwrap();
                    validate_grid_event(&scene(object), position, event(position)).unwrap();
                }
            }
        }

        #[test]
        fn strict_grid_rejects_position_activity_gain_and_spatial_modifiers() {
            let selected = scene(1);
            let expected = GRID_5[0];

            let mut changed_position = event(expected);
            changed_position.state.render.as_mut().unwrap().position.x = 1;
            assert_eq!(
                validate_grid_event(&selected, expected, changed_position)
                    .unwrap_err()
                    .code,
                DiagnosticCode::MappingUnsupported
            );

            let mut inactive = event(expected);
            inactive.state.active = false;
            assert!(validate_grid_event(&selected, expected, inactive).is_err());

            let mut gain = event(expected);
            gain.state.basic.as_mut().unwrap().gain = ObjectGainState::Quantized(15);
            assert!(validate_grid_event(&selected, expected, gain).is_err());

            let mut width = event(expected);
            width.state.render.as_mut().unwrap().other_properties.width =
                Some(WidthUpdate::Uniform(1));
            assert!(validate_grid_event(&selected, expected, width).is_err());

            let mut distance = event(expected);
            distance
                .state
                .render
                .as_mut()
                .unwrap()
                .other_properties
                .distance_factor_code = Some(1);
            assert!(validate_grid_event(&selected, expected, distance).is_err());

            let mut infinity = event(expected);
            infinity
                .state
                .render
                .as_mut()
                .unwrap()
                .other_properties
                .object_at_infinity = Some(false);
            assert!(validate_grid_event(&selected, expected, infinity).is_err());

            let mut zone = event(expected);
            zone.state.render.as_mut().unwrap().zone.grouped_defaults = false;
            zone.state.render.as_mut().unwrap().zone.group_zone_flag = Some(1);
            assert!(validate_grid_event(&selected, expected, zone).is_err());
        }

        #[test]
        fn start_ramp_requires_a_matching_preroll_state() {
            let selection = CoreSourceSelection {
                substream: 2,
                objects: vec![scene(1)],
                lfe: None,
            };
            let expected = GRID_5[0];
            let target = metadata_event(0, 100, expected);
            let matching = speaker_batch(vec![metadata_event(-100, 100, expected), target]);
            validate_speaker_grid(&matching, &selection, &LAYOUT_51, 48_000, 1_000).unwrap();

            let off_grid = speaker_batch(vec![metadata_event(-100, 100, (31, 31, 0)), target]);
            assert_eq!(
                validate_speaker_grid(&off_grid, &selection, &LAYOUT_51, 48_000, 1_000)
                    .unwrap_err()
                    .code,
                DiagnosticCode::MappingUnsupported
            );

            let missing = speaker_batch(vec![target]);
            assert_eq!(
                validate_speaker_grid(&missing, &selection, &LAYOUT_51, 48_000, 1_000)
                    .unwrap_err()
                    .code,
                DiagnosticCode::MappingUnsupported
            );
        }

        #[test]
        fn common_gate_rejects_custom_tools_and_timeline_conflicts() {
            let configs = OBSERVED_ENCODER_TRIM_MODES.map(|mode| TrimConfig {
                mode,
                ..TrimConfig::default()
            });
            let known = macindecode_ac4_bitstream::oamd::OamdCommonData {
                default_screen_size_ratio: true,
                master_screen_size_ratio_code: None,
                bed_object_chan_distribute: false,
                add_data_bytes: Some(4),
                trim: Trim {
                    present: true,
                    warp_mode: 3,
                    global_trim_mode: 2,
                    configs,
                },
                bed_render_info: BedRenderInfo::default(),
                headphone: Default::default(),
            };
            let mut selected = scene(1);
            selected.common = Some(known);
            let selection = CoreSourceSelection {
                substream: 2,
                objects: vec![selected],
                lfe: None,
            };
            validate_direct_common(&selection).unwrap();

            let mut custom = selection.objects[0];
            let mut common = known;
            common.trim.configs[0].mode = TrimConfigMode::Custom;
            common.trim.configs[0].centre = Some(3);
            custom.common = Some(common);
            assert_eq!(
                validate_direct_common(&CoreSourceSelection {
                    substream: 2,
                    objects: vec![custom],
                    lfe: None,
                })
                .unwrap_err()
                .code,
                DiagnosticCode::MappingUnsupported
            );

            let mut conflict = selection.objects[0];
            conflict.common_conflict = true;
            assert_eq!(
                validate_direct_common(&CoreSourceSelection {
                    substream: 2,
                    objects: vec![conflict],
                    lfe: None,
                })
                .unwrap_err()
                .code,
                DiagnosticCode::MappingUnsupported
            );
        }

        #[test]
        fn publish_is_atomic_and_never_replaces_an_existing_target() {
            use std::sync::atomic::{AtomicU64, Ordering};
            use std::sync::{Arc, Barrier};

            static SERIAL: AtomicU64 = AtomicU64::new(0);
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let root = (0..100u32)
                .find_map(|attempt| {
                    let path = std::env::temp_dir().join(format!(
                        "macin-core-caf-publish-{}-{serial}-{attempt}",
                        std::process::id()
                    ));
                    match fs::create_dir(&path) {
                        Ok(()) => Some(path),
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                        Err(error) => panic!("无法创建 no-clobber 测试目录：{error}"),
                    }
                })
                .expect("应能分配 no-clobber 测试目录");
            let output = root.join("output.caf");
            let first = root.join("first.tmp");
            let second = root.join("second.tmp");
            fs::write(&first, b"first").unwrap();
            fs::write(&second, b"second").unwrap();

            let barrier = Arc::new(Barrier::new(2));
            let publish = |temp: PathBuf| {
                let barrier = Arc::clone(&barrier);
                let output = output.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    publish_temp_noclobber(&temp, &output)
                })
            };
            let first_handle = publish(first.clone());
            let second_handle = publish(second.clone());
            let first_result = first_handle.join().unwrap();
            let second_result = second_handle.join().unwrap();
            assert_eq!(
                usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
                1
            );
            for error in [first_result.as_ref().err(), second_result.as_ref().err()]
                .into_iter()
                .flatten()
            {
                assert_eq!(error.code, DiagnosticCode::OutputExists);
            }
            let expected: &[u8] = if first_result.is_ok() {
                b"first"
            } else {
                b"second"
            };
            assert_eq!(fs::read(&output).unwrap(), expected);

            for temp in [first, second] {
                if temp.exists() {
                    fs::remove_file(temp).unwrap();
                }
            }
            fs::remove_file(output).unwrap();
            fs::remove_dir(root).unwrap();
        }

        #[test]
        fn accepts_only_zero_or_absent_width() {
            assert!(neutral_width(None));
            assert!(neutral_width(Some(WidthUpdate::Uniform(0))));
            assert!(neutral_width(Some(WidthUpdate::Cartesian {
                x: 0,
                y: 0,
                z: 0,
            })));
            assert!(!neutral_width(Some(WidthUpdate::Uniform(1))));
        }
    }
}

#[cfg(feature = "audio-decode")]
pub(crate) fn run(args: ExportCoreCafArgs) -> Result<String, CliError> {
    enabled::run(args)
}
