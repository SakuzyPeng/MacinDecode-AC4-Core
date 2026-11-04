//! 已有 MP4/edit 配对的实际批次观察，不扩展产品容器语义。
#![allow(
    clippy::arithmetic_side_effects,
    reason = "测试限定已锁定的六秒配对素材"
)]
use super::*;
use std::{fs::File, io::Write, path::Path};

#[test]
#[ignore = "requires the existing axes MP4/raw pair and a new output directory"]
fn capture_audio_metadata_container_pair() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(capture)
        .unwrap()
        .join()
        .unwrap();
}

fn capture() {
    let mp4 = std::fs::read(std::env::var("MACINAC4_SYNC_MP4").unwrap()).unwrap();
    let raw = std::fs::read(std::env::var("MACINAC4_SYNC_RAW").unwrap()).unwrap();
    let output = std::env::var("MACINAC4_SYNC_CONTAINER_OUTPUT").unwrap();
    let output = Path::new(&output);
    std::fs::create_dir(output).unwrap();
    let source = Ac4Mp4::parse(&mp4).unwrap();
    let timeline = source.presentation_timeline::<8>().unwrap();
    assert_eq!(timeline.priming_samples(48_000).unwrap(), 2048);
    assert_eq!(
        timeline.presentation_sample_shift(48_000).unwrap(),
        Some(-2048)
    );
    assert_eq!(
        timeline.presentation_duration_samples(48_000).unwrap(),
        288_000
    );
    let packets: Vec<_> = source.access_units().map(|a| a.unwrap()).collect();
    let sync: Vec<_> = SyncFrameIter::new(&raw).map(|a| a.unwrap()).collect();
    assert_eq!(packets.len(), 143);
    assert_eq!(packets.len(), sync.len());
    for (a, b) in packets.iter().zip(&sync) {
        assert_eq!(a.payload, b.raw_frame, "两个容器必须携带完全相同的 AU");
    }
    let raw_batch = collect_full_scene_batch(&raw, PresentationSelection::AutoUnique).unwrap();
    let mp4_batch = collect_full_scene_batch(&mp4, PresentationSelection::AutoUnique).unwrap();
    assert_eq!(raw_batch.pcm.tracks.len(), 21);
    assert_eq!(raw_batch.pcm.tracks.len(), mp4_batch.pcm.tracks.len());
    let mut log = File::create_new(output.join("container.jsonl")).unwrap();
    writeln!(log, "{{\"kind\":\"timeline\",\"aus\":143,\"edit_samples\":2048,\"raw_samples\":292864,\"presentation_samples\":288000,\"payloads_equal\":true,\"metadata_events\":{}}}", raw_batch.metadata.events.len()).unwrap();
    for (a, b) in raw_batch.pcm.tracks.iter().zip(&mp4_batch.pcm.tracks) {
        assert_eq!(a.samples.len(), 292_864);
        assert_eq!(b.samples.len(), 288_000);
        assert_eq!(a.source, b.source);
        assert_eq!(a.scene_element_id, b.scene_element_id);
        assert!(
            a.samples
                .get(2048..290_048)
                .unwrap()
                .iter()
                .zip(&b.samples)
                .all(|(x, y)| x.to_bits() == y.to_bits())
        );
        writeln!(log, "{{\"kind\":\"track\",\"channel\":{},\"raw_first\":2048,\"presentation_first\":0,\"samples\":288000,\"bit_exact\":true}}", a.output_index).unwrap();
    }
    assert_eq!(
        raw_batch.metadata.events.len(),
        mp4_batch.metadata.events.len()
    );
    for (a, b) in raw_batch
        .metadata
        .events
        .iter()
        .zip(&mp4_batch.metadata.events)
    {
        assert_eq!(
            MetadataEvent {
                sample_position: a.sample_position - 2048,
                ..*a
            },
            *b
        );
        writeln!(log, "{{\"kind\":\"event\",\"order\":{},\"raw_sample\":{},\"presentation_sample\":{},\"ramp\":{},\"state_identity\":true}}", a.stream_order, a.sample_position, b.sample_position, b.ramp_samples).unwrap();
    }
}
