//! 本地真实向量上的源 AU 元数据与 Scene 对照；不在无表 CI 中隐式读媒体。
#![cfg(feature = "metadata-decode")]
use macindecode_ac4_inspect::{
    CoreGridStability, FieldStatus, InspectOptions, MetadataDetail, inspect_path_with_options,
};
#[cfg(feature = "audio-decode")]
use std::fs;
use std::path::PathBuf;

fn vector(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vectors")
        .join(case)
}

#[test]
#[ignore = "requires the local probe_bed_only AC-4 vector and specification tables"]
fn metadata_only_scan_observes_the_complete_real_core_grid() {
    let report = inspect_path_with_options(
        vector("probe_bed_only/encoded/master_ac4_256K.m4a"),
        InspectOptions::default().with_metadata_detail(MetadataDetail::Full),
    )
    .unwrap();
    let observation = report.core_layouts.observations.first().unwrap();
    assert_eq!(
        observation.observed_core_grid.observed_frames,
        report.source.frame_count
    );
    assert_eq!(observation.observed_core_grid.missing_frames, 0);
    assert_eq!(
        observation.observed_core_grid.stability,
        CoreGridStability::Stable
    );
    assert_eq!(
        observation.derived_speaker_layout.status,
        FieldStatus::Present
    );
    assert_eq!(
        observation
            .derived_speaker_layout
            .value
            .as_ref()
            .and_then(serde_json::Value::as_str),
        Some("5.1")
    );
    assert_eq!(observation.object_to_speaker.len(), 6);
    assert!(report.render_text().contains("Derived speaker layout: 5.1"));
}

#[cfg(feature = "audio-decode")]
#[test]
#[ignore = "requires local A-JOC vectors and specification tables"]
fn source_metadata_matches_scene_by_control_source_access_unit() {
    use macindecode_ac4_metadata::{
        Ac4MetadataConfig, Ac4MetadataSession, AccessUnit, AccessUnitContext, ObjectMetadataDomain,
        PresentationScope,
    };
    use macindecode_ac4_mp4::Ac4Mp4;
    use macindecode_ac4_scene::{
        Ac4DecoderConfig, Ac4DecoderSession, DecodeMode, PresentationSelection,
    };
    use std::collections::BTreeMap;
    for case in [
        "probe_bed_only/encoded/master_ac4_256K.m4a",
        "probe_axes_single_object/encoded/master_ac4_768K.m4a",
        "probe_subframe_updates/encoded/master_ac4_768K.m4a",
    ] {
        let data = fs::read(vector(case)).unwrap();
        let media = Ac4Mp4::parse(&data).unwrap();
        for mode in [DecodeMode::Core, DecodeMode::Full] {
            let mut metadata = Ac4MetadataSession::new(
                Ac4MetadataConfig::new()
                    .with_detail(MetadataDetail::Full)
                    .with_presentations(PresentationScope::Selected(
                        PresentationSelection::AutoUnique,
                    )),
            )
            .unwrap();
            let mut scene = Ac4DecoderSession::new(
                Ac4DecoderConfig::new(PresentationSelection::AutoUnique).with_decode_mode(mode),
            );
            let domain = if mode == DecodeMode::Core {
                ObjectMetadataDomain::Core
            } else {
                ObjectMetadataDomain::Full
            };
            let mut updates = BTreeMap::new();
            let mut compared = 0usize;
            for packet in media.access_units() {
                let packet = packet.unwrap();
                let index = u64::from(packet.info.index);
                let input = AccessUnit::new(packet.payload, AccessUnitContext::new(index));
                let source = metadata.observe_access_unit(input).unwrap();
                for update in source
                    .object_updates()
                    .iter()
                    .filter(|u| u.target.domain == domain)
                {
                    assert!(
                        updates
                            .insert(
                                (index, update.raw.object_index, update.raw.block_index),
                                *update
                            )
                            .is_none()
                    );
                }
                let decoded = scene.decode_access_unit(input).unwrap();
                if let Some(actual) = decoded.presentation_metadata() {
                    let expected = source
                        .presentation_metadata(actual.presentation_index())
                        .unwrap();
                    assert_eq!(actual.payload(), expected.payload());
                    assert_eq!(
                        actual.effective_drc_configuration(),
                        expected.effective_drc_configuration()
                    );
                    assert_eq!(
                        actual.effective_group_gain_codes(),
                        expected.effective_group_gain_codes()
                    );
                }
                if let Some(actual) = decoded.audio_metadata() {
                    let expected = source
                        .audio_metadata()
                        .iter()
                        .find(|a| a.substream_index == actual.substream_index)
                        .unwrap();
                    assert_eq!(actual.result, expected.result);
                    assert_eq!(
                        actual.effective_de_configuration,
                        expected.effective_de_configuration
                    );
                }
                for frame in decoded.frames() {
                    for update in frame.metadata_updates() {
                        let Some(raw) = update.raw() else { continue };
                        let block = raw.block();
                        let expected = updates
                            .get(&(
                                raw.control_source_access_unit_index(),
                                block.object_index,
                                block.block_index,
                            ))
                            .expect("Scene update must have a source AU observation");
                        assert_eq!(block, expected.raw, "{case} {mode:?}");
                        assert_eq!(
                            update.state().raw().effective(),
                            expected.target.state,
                            "{case} {mode:?}"
                        );
                        assert_eq!(
                            update.state().raw().additional(),
                            expected.target.additional,
                            "{case} {mode:?}"
                        );
                        if let Some(ramp) = expected.ramp_duration_samples {
                            assert_eq!(u32::from(raw.timing().block().ramp_duration), ramp);
                        }
                        compared = compared.saturating_add(1);
                    }
                }
            }
            assert!(
                compared > 0,
                "{case} {mode:?}: must compare real OAMD updates"
            );
        }
    }
}
