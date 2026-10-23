//! 仅供本地实验的公共 Scene 入口观察；时间预期由外部独立模型计算。
#![allow(clippy::arithmetic_side_effects, reason = "测试约束 400 帧整数坐标")]
extern crate std;

use crate::*;
use macindecode_ac4_bitstream::{Ac4Toc, SyncFrameIter};
use std::{fs::File, io::Write, path::Path, vec::Vec};

#[test]
#[ignore = "requires local complete stream and a new explicit output directory"]
fn capture_oamd_scene_timing() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(capture)
        .unwrap()
        .join()
        .unwrap();
}

fn capture() {
    let input = std::env::var("MACINAC4_OAMD_INPUT").unwrap();
    let output = std::env::var("MACINAC4_OAMD_OUTPUT").unwrap();
    let output = Path::new(&output);
    std::fs::create_dir(output).unwrap();
    let data = std::fs::read(input).unwrap();
    let mut records = File::create_new(output.join("scene.jsonl")).unwrap();
    let mut pcm = std::io::BufWriter::new(File::create_new(output.join("objects.f32")).unwrap());
    let mut session =
        Ac4DecoderSession::new(Ac4DecoderConfig::default().with_decode_mode(DecodeMode::Full));
    let mut source_start = 0i64;
    let mut count = 0;
    for (index, result) in SyncFrameIter::new(&data).enumerate() {
        let sync = result.unwrap();
        let toc = Ac4Toc::parse(sync.raw_frame).unwrap();
        let duration = toc.codec_frame_len_base(1).unwrap();
        let au = u64::try_from(index).unwrap() + 1;
        let decoded = session
            .decode_access_unit(AccessUnit::new(
                sync.raw_frame,
                AccessUnitContext::new(au)
                    .with_source_sample_start(source_start)
                    .with_presentation_sample_start(source_start),
            ))
            .unwrap();
        let frames: Vec<_> = decoded.frames().collect();
        assert_eq!(frames.len(), 1);
        let frame = frames.first().unwrap();
        writeln!(records, "{{\"kind\":\"frame\",\"au\":{au},\"source_start\":{source_start},\"codec_start\":{},\"duration\":{duration},\"objects\":{},\"updates\":{}}}",
            frame.timeline().codec_sample_start(), frame.objects().len(), frame.metadata_updates().len()).unwrap();
        for update in frame.metadata_updates() {
            let Some(raw) = update.raw() else { continue };
            let timing = raw.timing();
            let block = raw.block();
            let bt = timing.block();
            writeln!(records, "{{\"kind\":\"event\",\"au\":{au},\"source_au\":{},\"object\":{},\"block\":{},\"offset\":{},\"ramp\":{},\"sample_offset\":{},\"offset_source\":\"{:?}\",\"factor\":{},\"ramp_code\":{},\"ramp_encoding\":\"{:?}\",\"timing_updated\":{},\"order\":{}}}",
                raw.control_source_access_unit_index(), block.object_index, block.block_index,
                update.offset_samples(), update.ramp_duration_samples(), timing.sample_offset(),
                timing.offset_source(), bt.block_offset_factor, bt.ramp_duration_code,
                bt.ramp_duration_encoding, timing.updated_in_source_access_unit(), update.stream_order()).unwrap();
        }
        for object in frame.objects() {
            for plane in object.pcm().planes() {
                for sample in plane.samples() {
                    assert!(sample.is_finite());
                    pcm.write_all(&sample.to_le_bytes()).unwrap();
                }
            }
        }
        source_start += i64::from(duration);
        count += 1;
    }
    assert!(count > 0);
    pcm.flush().unwrap();
    writeln!(
        records,
        "{{\"kind\":\"eof\",\"aus\":{count},\"source_samples\":{source_start}}}"
    )
    .unwrap();
}
