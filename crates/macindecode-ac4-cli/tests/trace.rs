#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试按固定 ISO BMFF 布局构造极小文件"
)]

mod common;

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use common::success;

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn mp4_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8usize + payload.len()).expect("测试 box 小于 4 GiB");
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(box_type);
    out.extend_from_slice(payload);
    out
}

fn append_box(parent: &mut Vec<u8>, box_type: &[u8; 4], payload: &[u8]) {
    parent.extend_from_slice(&mp4_box(box_type, payload));
}

fn header_timing(timescale: u32, duration: u32) -> Vec<u8> {
    let mut payload = vec![0u8; 20];
    payload[12..16].copy_from_slice(&timescale.to_be_bytes());
    payload[16..20].copy_from_slice(&duration.to_be_bytes());
    payload
}

fn edit_list(segment_duration: u32, media_time: i32) -> Vec<u8> {
    let mut payload = vec![0u8; 20];
    payload[4..8].copy_from_slice(&1u32.to_be_bytes());
    payload[8..12].copy_from_slice(&segment_duration.to_be_bytes());
    payload[12..16].copy_from_slice(&media_time.to_be_bytes());
    payload[16..18].copy_from_slice(&1i16.to_be_bytes());
    payload
}

fn sample_description(entry: Vec<u8>) -> Vec<u8> {
    let mut payload = vec![0u8; 8];
    payload[4..8].copy_from_slice(&1u32.to_be_bytes());
    payload.extend_from_slice(&entry);
    mp4_box(b"stsd", &payload)
}

fn video_track() -> Vec<u8> {
    let stsd = sample_description(mp4_box(b"avc1", &[]));
    let stbl = mp4_box(b"stbl", &stsd);
    let minf = mp4_box(b"minf", &stbl);
    let mdia = mp4_box(b"mdia", &minf);
    let elst = mp4_box(b"elst", &edit_list(1_024, 0));
    let edts = mp4_box(b"edts", &elst);
    let mut payload = edts;
    payload.extend_from_slice(&mdia);
    mp4_box(b"trak", &payload)
}

fn ac4_sample_entry() -> Vec<u8> {
    let mut payload = vec![0u8; 28];
    // ac4_dsi_version=0, bitstream_version=2, fs_index=1,
    // frame_rate_index=13, n_presentations=1。
    append_box(&mut payload, b"dac4", &[0x00, 0xBA, 0x01]);
    mp4_box(b"ac-4", &payload)
}

fn full_box_table(box_type: &[u8; 4], words: &[u32]) -> Vec<u8> {
    let mut payload = vec![0u8; 4];
    for word in words {
        payload.extend_from_slice(&word.to_be_bytes());
    }
    mp4_box(box_type, &payload)
}

fn audio_track(chunk_offset: u32) -> Vec<u8> {
    let mut stbl_payload = sample_description(ac4_sample_entry());
    stbl_payload.extend_from_slice(&full_box_table(b"stts", &[1, 2, 2_048]));
    stbl_payload.extend_from_slice(&full_box_table(b"stsc", &[1, 1, 2, 1]));
    stbl_payload.extend_from_slice(&full_box_table(b"stsz", &[3, 2]));
    stbl_payload.extend_from_slice(&full_box_table(b"stco", &[1, chunk_offset]));
    stbl_payload.extend_from_slice(&full_box_table(b"stss", &[2, 1, 2]));

    let stbl = mp4_box(b"stbl", &stbl_payload);
    let minf = mp4_box(b"minf", &stbl);
    let mut mdia_payload = mp4_box(b"mdhd", &header_timing(48_000, 4_096));
    mdia_payload.extend_from_slice(&minf);
    let mdia = mp4_box(b"mdia", &mdia_payload);

    let elst = mp4_box(b"elst", &edit_list(2_048, 2_048));
    let edts = mp4_box(b"edts", &elst);
    let mut trak_payload = edts;
    trak_payload.extend_from_slice(&mdia);
    mp4_box(b"trak", &trak_payload)
}

fn moov(chunk_offset: u32) -> Vec<u8> {
    let mut payload = mp4_box(b"mvhd", &header_timing(48_000, 2_048));
    payload.extend_from_slice(&video_track());
    payload.extend_from_slice(&audio_track(chunk_offset));
    mp4_box(b"moov", &payload)
}

fn raw_toc(sequence_counter: u16) -> Vec<u8> {
    let bits = format!("10{sequence_counter:010b}011101110");
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (index, bit) in bits.bytes().enumerate() {
        if bit == b'1' {
            out[index / 8] |= 1 << (7 - index % 8);
        }
    }
    out
}

fn multitrack_mp4() -> Vec<u8> {
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\0isom");
    let placeholder = moov(0);
    let chunk_offset =
        u32::try_from(ftyp.len() + placeholder.len() + 8).expect("测试文件偏移小于 4 GiB");
    let moov = moov(chunk_offset);
    let mut media = raw_toc(0);
    media.extend_from_slice(&raw_toc(1));
    let mdat = mp4_box(b"mdat", &media);

    let mut file = ftyp;
    file.extend_from_slice(&moov);
    file.extend_from_slice(&mdat);
    file
}

/// 构造一个完整拓扑帧：单 presentation、单 stereo group、单 substream。
///
/// 位串取自 `topology.rs` 的单元测试。`ndot` 控制 `b_pres_ndot`，从而决定该帧
/// 是否构成完整随机访问点——`b_iframe_global` 始终为 1，因此 `ndot` 为假时该帧
/// 只是「仅音频可起解」。
fn topology_frame(sequence_counter: u16, ndot: bool) -> Vec<u8> {
    let pres_ndot = u8::from(ndot);
    let bits = format!(
        // ac4_toc 前置：bitstream_version=2、sequence_counter、b_wait_frames=0、
        // fs_index=1、frame_rate_index=13、b_iframe_global=1、单 presentation、
        // b_payload_base=0、b_program_id=0
        "10{sequence_counter:010b} 0 1 1101 1 1 0 0 \
         1 0 000 0 00 000 0 00 00 0 000 0 0 0 {pres_ndot} 00 \
         1 0 1 1 10 0 0 1 00 0 \
         01 0"
    );
    let mut out = vec![
        0u8;
        bits.chars()
            .filter(|c| *c == '0' || *c == '1')
            .count()
            .div_ceil(8)
    ];
    let mut index = 0usize;
    for bit in bits.chars() {
        if bit == '0' || bit == '1' {
            if bit == '1' {
                out[index / 8] |= 1 << (7 - index % 8);
            }
            index += 1;
        }
    }
    // payload_base 为 0，单 substream 且未传尺寸时载荷延伸到帧尾。
    out.push(0);
    out
}

/// 把若干完整帧封装为 `Annex G` 裸流。
fn annex_g_of(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = Vec::new();
    for frame in frames {
        stream.extend_from_slice(&0xAC40u16.to_be_bytes());
        stream.extend_from_slice(
            &u16::try_from(frame.len())
                .expect("测试帧小于 64 KiB")
                .to_be_bytes(),
        );
        stream.extend_from_slice(frame);
    }
    stream
}

fn annex_g_stream() -> Vec<u8> {
    let mut stream = Vec::new();
    for sequence in [1_019, 1_020, 1] {
        let frame = raw_toc(sequence);
        stream.extend_from_slice(&0xAC40u16.to_be_bytes());
        stream.extend_from_slice(
            &u16::try_from(frame.len())
                .expect("测试帧小于 64 KiB")
                .to_be_bytes(),
        );
        stream.extend_from_slice(&frame);
    }
    stream
}

#[test]
fn selects_ac4_track_and_preserves_negative_pts() {
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("macinac4-m1-{}-{serial}.mp4", std::process::id()));
    fs::write(&path, multitrack_mp4()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("trace")
        .arg(&path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);

    assert!(
        output.status.success(),
        "trace 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = success("trace", &output.stdout);
    let source = &stdout["result"]["source"];
    assert_eq!(source["kind"], "mp4");
    assert_eq!(source["track"]["track_index"], 1);
    assert_eq!(source["track"]["sample_count"], 2);
    assert_eq!(source["first_samples"][0]["media_pts"], 0);
    assert_eq!(source["first_samples"][0]["presentation_time"], -2048);
    assert_eq!(source["first_samples"][1]["presentation_time"], 0);
}

#[test]
fn enumerates_equivalent_annex_g_boundaries() {
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "macinac4-m1-raw-{}-{serial}.ac4",
        std::process::id()
    ));
    fs::write(&path, annex_g_stream()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("trace")
        .arg(&path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);

    assert!(
        output.status.success(),
        "trace 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = success("trace", &output.stdout);
    let result = &stdout["result"];
    assert_eq!(result["source"]["kind"], "annex_g");
    assert_eq!(result["frames"]["count"], 3);
    assert_eq!(result["source"]["payload_bytes"], 9);
    assert_eq!(result["frames"]["iframe_global_frames"], 3);
    assert_eq!(result["frames"]["sequence_first"], 1019);
    assert_eq!(result["frames"]["sequence_last"], 1);
    assert_eq!(result["frames"]["sequence_discontinuities"], 0);
}

/// 裸流路径同样运行拓扑与 reset 状态机。
///
/// 拼接向量覆盖的正是这几条路径：单一来源的流里 `source_changes`、
/// `waiting_for_random_access_frames` 恒为 0，只有真实的来源变化才走得到。
#[test]
fn raw_path_reports_source_change_and_waits_for_random_access() {
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "macinac4-splice-{}-{serial}.ac4",
        std::process::id()
    ));
    // 帧 0 是完整起解点，执行初始重置；帧 2 的计数跳变触发来源变化，但要等到
    // 帧 4 才重新成为完整起解点。
    let frames = vec![
        topology_frame(1, true),
        topology_frame(2, false),
        topology_frame(50, false),
        topology_frame(51, false),
        topology_frame(52, true),
    ];
    fs::write(&path, annex_g_of(&frames)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("trace")
        .arg(&path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);

    assert!(
        output.status.success(),
        "trace 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = success("trace", &output.stdout);
    let topology = &stdout["result"]["validation"]["topology"];
    assert_eq!(topology["coverage"]["frames_parsed"], 5);
    assert_eq!(topology["coverage"]["parse_failures"], 0);
    assert_eq!(topology["timing"]["source_changes"], 1);
    assert_eq!(topology["timing"]["reset_events"], 2);
    assert_eq!(topology["timing"]["waiting_for_random_access_frames"], 2);
    assert_eq!(topology["timing"]["awaiting_random_access"], false);
    // 裸流没有 stss，该比对必须被跳过而不是默认记为失配。
    assert_eq!(topology["timing"]["stss_random_access_mismatches"], 0);
}
