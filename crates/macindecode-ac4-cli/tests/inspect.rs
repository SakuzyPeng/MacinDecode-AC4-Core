#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试按固定 AC-4 与 ISO BMFF 布局构造极小文件"
)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use common::success;
use serde_json::Value;

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn temp_path(extension: &str) -> PathBuf {
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "macinac4-inspect-{}-{serial}.{extension}",
        std::process::id()
    ))
}

fn run_inspect(path: &Path, format: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_macinac4"));
    command.arg("inspect").arg(path);
    if let Some(format) = format {
        command.arg("--format").arg(format);
    }
    command.output().expect("inspect 应可启动")
}

fn pack_bits(bits: &str) -> Vec<u8> {
    let count = bits
        .chars()
        .filter(|character| matches!(character, '0' | '1'))
        .count();
    let mut output = vec![0u8; count.div_ceil(8)];
    let mut index = 0usize;
    for character in bits.chars() {
        if matches!(character, '0' | '1') {
            if character == '1' {
                output[index / 8] |= 1 << (7 - index % 8);
            }
            index += 1;
        }
    }
    output
}

/// 单 presentation、单 channel-based group、单物理 substream 的完整帧。
fn topology_frame(sequence_counter: u16, iframe: bool, surround: bool) -> Vec<u8> {
    let iframe = u8::from(iframe);
    let group = if surround {
        // ch_mode=4 (5.1)
        "1 0 1 1 1110 0 0 1 01 0"
    } else {
        // ch_mode=1 (stereo)
        "1 0 1 1 10 0 0 1 01 0"
    };
    let bits = format!(
        // bitstream_version=2、48 kHz、frame_rate_index=13；presentation 与 audio
        // substream 均为 independent，确保首帧是完整配置基准。
        "10{sequence_counter:010b} 0 1 1101 {iframe} 1 0 0 \
         1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00 \
         {group} \
         10 0 0000000011 0 0000000100"
    );
    let mut output = pack_bits(&bits);
    // presentation metadata 与 audio metadata 分别使用物理 index 0、1。
    output.extend_from_slice(&[0x55, 0x04, 0x00]);
    output.extend_from_slice(&[0x00, 0x00, 0x00, 0x20]);
    output
}

fn truncated_presentation_metadata_frame(sequence_counter: u16) -> Vec<u8> {
    let bits = format!(
        // 与 topology_frame 相同，但 presentation substream 只声明并携带两个字节；
        // topology/index table 完整，截断发生在 bounded presentation syntax 内。
        "10{sequence_counter:010b} 0 1 1101 1 1 0 0 \
         1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00 \
         1 0 1 1 10 0 0 1 01 0 \
         10 0 0000000010 0 0000000100"
    );
    let mut output = pack_bits(&bits);
    output.extend_from_slice(&[0x55, 0x04]);
    output.extend_from_slice(&[0x00, 0x00, 0x00, 0x20]);
    output
}

fn truncated_audio_metadata_frame(sequence_counter: u16) -> Vec<u8> {
    let bits = format!(
        // 完整 topology 与 presentation metadata，audio substream 则被严格定界为一字节。
        "10{sequence_counter:010b} 0 1 1101 1 1 0 0 \
         1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00 \
         1 0 1 1 10 0 0 1 01 0 \
         10 0 0000000011 0 0000000001"
    );
    let mut output = pack_bits(&bits);
    output.extend_from_slice(&[0x55, 0x04, 0x00]);
    output.push(0x00);
    output
}

fn duplicate_audio_reference_frame(sequence_counter: u16) -> Vec<u8> {
    let presentation = "1 0 000 0 00 000 0 00 00 0 000 0 0 0 1 00";
    let bits = format!(
        // n_presentations=2；两个 anonymous presentation 均引用 group 0，而 group 0
        // 只引用物理 audio substream 1。
        "10{sequence_counter:010b} 0 1 1101 1 0 1 00 0 0 0 \
         {presentation} {presentation} \
         1 0 1 1 10 0 0 1 01 0 \
         10 0 0000000011 0 0000000100"
    );
    let mut output = pack_bits(&bits);
    output.extend_from_slice(&[0x55, 0x04, 0x00]);
    output.extend_from_slice(&[0x00, 0x00, 0x00, 0x20]);
    output
}

fn unsupported_topology_frame(sequence_counter: u16) -> Vec<u8> {
    // bitstream_version=3 + variable_bits(2) value 1 => 4。TOC 本身完整，但当前
    // ETSI 版本尚未定义对应 presentation syntax。
    pack_bits(&format!("11 01 0 {sequence_counter:010b} 0 1 1101 1 1 0"))
}

fn crc16(data: &[u8]) -> u16 {
    let mut register = 0u16;
    for &byte in data {
        register ^= u16::from(byte) << 8;
        for _ in 0..8 {
            let overflow = register & 0x8000 != 0;
            register <<= 1;
            if overflow {
                register ^= 0x8005;
            }
        }
    }
    register
}

fn annex_g_of(frames: &[(Vec<u8>, bool)], with_crc: bool) -> Vec<u8> {
    let mut stream = Vec::new();
    for (frame, corrupt_crc) in frames {
        stream.extend_from_slice(if with_crc {
            &[0xAC, 0x41]
        } else {
            &[0xAC, 0x40]
        });
        stream.extend_from_slice(
            &u16::try_from(frame.len())
                .expect("测试帧小于 64 KiB")
                .to_be_bytes(),
        );
        stream.extend_from_slice(frame);
        if with_crc {
            let protected_start = stream
                .len()
                .checked_sub(frame.len() + 2)
                .expect("测试帧含尺寸字段");
            let mut crc = crc16(&stream[protected_start..]);
            if *corrupt_crc {
                crc ^= 1;
            }
            stream.extend_from_slice(&crc.to_be_bytes());
        }
    }
    stream
}

fn mp4_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8usize + payload.len()).expect("测试 box 小于 4 GiB");
    let mut output = Vec::with_capacity(size as usize);
    output.extend_from_slice(&size.to_be_bytes());
    output.extend_from_slice(box_type);
    output.extend_from_slice(payload);
    output
}

fn full_box_table(box_type: &[u8; 4], words: &[u32]) -> Vec<u8> {
    let mut payload = vec![0u8; 4];
    for word in words {
        payload.extend_from_slice(&word.to_be_bytes());
    }
    mp4_box(box_type, &payload)
}

fn header_timing(timescale: u32, duration: u32) -> Vec<u8> {
    let mut payload = vec![0u8; 20];
    payload[12..16].copy_from_slice(&timescale.to_be_bytes());
    payload[16..20].copy_from_slice(&duration.to_be_bytes());
    payload
}

fn sample_description() -> Vec<u8> {
    let mut sample_entry = vec![0u8; 28];
    // dsi_version=0、bitstream_version=2、fs_index=1、frame_rate_index=13、
    // n_presentations=1。
    sample_entry.extend_from_slice(&mp4_box(b"dac4", &[0x00, 0xBA, 0x01]));
    let sample_entry = mp4_box(b"ac-4", &sample_entry);
    let mut payload = vec![0u8; 8];
    payload[4..8].copy_from_slice(&1u32.to_be_bytes());
    payload.extend_from_slice(&sample_entry);
    mp4_box(b"stsd", &payload)
}

fn audio_track(chunk_offset: u32, frames: &[Vec<u8>]) -> Vec<u8> {
    let count = u32::try_from(frames.len()).expect("测试 sample 数适合 u32");
    let mut stbl_payload = sample_description();
    stbl_payload.extend_from_slice(&full_box_table(b"stts", &[1, count, 2_048]));
    stbl_payload.extend_from_slice(&full_box_table(b"stsc", &[1, 1, count, 1]));
    let mut sizes = vec![0, count];
    sizes.extend(
        frames
            .iter()
            .map(|frame| u32::try_from(frame.len()).expect("测试 sample 适合 u32")),
    );
    stbl_payload.extend_from_slice(&full_box_table(b"stsz", &sizes));
    stbl_payload.extend_from_slice(&full_box_table(b"stco", &[1, chunk_offset]));
    let stbl = mp4_box(b"stbl", &stbl_payload);
    let minf = mp4_box(b"minf", &stbl);
    let duration = count.checked_mul(2_048).expect("测试时长适合 u32");
    let mut mdia_payload = mp4_box(b"mdhd", &header_timing(48_000, duration));
    mdia_payload.extend_from_slice(&minf);
    mp4_box(b"trak", &mp4_box(b"mdia", &mdia_payload))
}

fn moov(chunk_offset: u32, frames: &[Vec<u8>]) -> Vec<u8> {
    let duration = u32::try_from(frames.len())
        .expect("测试 sample 数适合 u32")
        .checked_mul(2_048)
        .expect("测试时长适合 u32");
    let mut payload = mp4_box(b"mvhd", &header_timing(48_000, duration));
    payload.extend_from_slice(&audio_track(chunk_offset, frames));
    mp4_box(b"moov", &payload)
}

fn minimal_mp4(frames: &[Vec<u8>]) -> Vec<u8> {
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\0isom");
    let placeholder = moov(0, frames);
    let chunk_offset = u32::try_from(ftyp.len() + placeholder.len() + 8).expect("测试文件适合 u32");
    let moov = moov(chunk_offset, frames);
    let media = frames.iter().flatten().copied().collect::<Vec<_>>();
    let mdat = mp4_box(b"mdat", &media);
    [ftyp, moov, mdat].concat()
}

fn json_result(path: &Path) -> Value {
    let output = run_inspect(path, Some("json"));
    assert!(
        output.status.success(),
        "inspect 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    success("inspect", &output.stdout)["result"]["inspectResult"].clone()
}

#[test]
fn raw_json_reports_crc_variable_iframes_and_unique_physical_substreams() {
    let path = temp_path("ac4");
    let frames = [
        (topology_frame(0, true, false), false),
        (topology_frame(1, false, false), false),
        (topology_frame(2, true, false), false),
        (topology_frame(3, false, false), true),
        (topology_frame(4, false, false), false),
        (topology_frame(5, true, false), false),
    ];
    fs::write(&path, annex_g_of(&frames, true)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(result["source"]["kind"], "annex_g");
    assert_eq!(result["source"]["frame_count"], 6);
    assert_eq!(result["source"]["track_index"]["status"], "not_applicable");
    assert_eq!(result["stream"]["sync_word"]["value"], "0xAC41");
    assert_eq!(result["stream"]["sync_word"]["raw_code"], 0xAC41);
    assert_eq!(result["stream"]["crc_errors"]["value"], true);
    assert_eq!(
        result["stream"]["crc_errors"]["raw_code"],
        serde_json::json!({"protected_frames": 6, "failures": 1})
    );
    assert_eq!(
        result["stream"]["i_frame_interval"]["value"]["kind"],
        "variable"
    );
    assert_eq!(result["stream"]["i_frame_interval"]["value"]["minimum"], 2);
    assert_eq!(result["stream"]["i_frame_interval"]["value"]["maximum"], 3);
    assert_eq!(result["stream"]["number_of_presentations"]["value"], 1);
    assert_eq!(result["stream"]["number_of_audio_substreams"]["value"], 1);
    assert_eq!(
        result["presentations"][0]["audio_substreams"]["value"],
        serde_json::json!([1])
    );
    assert!(
        result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| { issue["code"] == "crc_mismatch" && issue["frame_index"] == 3 })
    );
}

#[test]
fn raw_json_reports_fixed_iframe_interval_without_crc() {
    let path = temp_path("ac4");
    let frames = [
        (topology_frame(0, true, false), false),
        (topology_frame(1, false, false), false),
        (topology_frame(2, true, false), false),
        (topology_frame(3, false, false), false),
        (topology_frame(4, true, false), false),
    ];
    fs::write(&path, annex_g_of(&frames, false)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(result["stream"]["sync_word"]["value"], "0xAC40");
    assert_eq!(result["stream"]["crc_errors"]["status"], "not_present");
    assert_eq!(
        result["stream"]["i_frame_interval"]["value"],
        serde_json::json!({"kind": "fixed", "frames": 2, "display": "2"})
    );
}

#[test]
fn frames_before_the_first_complete_configuration_do_not_bias_substream_bitrates() {
    let path = temp_path("ac4");
    let frames = [
        (topology_frame(0, false, false), false),
        (topology_frame(1, true, false), false),
    ];
    fs::write(&path, annex_g_of(&frames, false)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(result["presentations"][0]["bit_rate"]["status"], "unknown");
    assert_eq!(
        result["audio_substreams"][0]["bit_rate"]["status"],
        "unknown"
    );
    assert!(result["issues"].as_array().unwrap().iter().any(|issue| {
        issue["code"] == "frames_before_complete_configuration" && issue["frame_index"] == 1
    }));
}

#[test]
fn duplicate_presentation_references_count_one_physical_audio_substream() {
    let path = temp_path("ac4");
    let frames = [(duplicate_audio_reference_frame(0), false)];
    fs::write(&path, annex_g_of(&frames, false)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(result["stream"]["number_of_presentations"]["value"], 2);
    assert_eq!(result["stream"]["number_of_audio_substreams"]["value"], 1);
    assert_eq!(
        result["presentations"][0]["audio_substreams"]["value"],
        serde_json::json!([1])
    );
    assert_eq!(
        result["presentations"][1]["audio_substreams"]["value"],
        serde_json::json!([1])
    );
    assert_eq!(result["audio_substreams"][0]["index"], 1);
}

#[test]
fn known_unsupported_topology_returns_a_usable_report_and_issue() {
    let path = temp_path("ac4");
    let frames = [(unsupported_topology_frame(0), false)];
    fs::write(&path, annex_g_of(&frames, false)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(result["source"]["frame_count"], 1);
    assert_eq!(result["stream"]["bitstream_version"]["value"], 4);
    assert_eq!(
        result["stream"]["number_of_presentations"]["status"],
        "unknown"
    );
    assert_eq!(
        result["stream"]["number_of_audio_substreams"]["status"],
        "unknown"
    );
    assert!(
        result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| { issue["code"] == "topology_unsupported" && issue["frame_index"] == 0 })
    );
    assert!(
        result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| { issue["code"] == "complete_configuration_unavailable" })
    );
}

#[test]
fn mp4_json_uses_sample_timing_and_marks_transport_fields_not_applicable() {
    let path = temp_path("m4a");
    let frames = vec![
        topology_frame(0, true, false),
        topology_frame(1, false, false),
        topology_frame(2, true, false),
    ];
    fs::write(&path, minimal_mp4(&frames)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(result["source"]["kind"], "mp4");
    assert_eq!(result["source"]["track_index"]["value"], 0);
    assert_eq!(result["source"]["duration"]["value"], 0.128);
    assert_eq!(result["stream"]["sample_rate"]["value"], 48_000);
    assert_eq!(result["stream"]["frame_rate"]["value"]["numerator"], 48_000);
    assert_eq!(
        result["stream"]["frame_rate"]["value"]["denominator"],
        2_048
    );
    assert_eq!(
        result["stream"]["frame_rate"]["raw_code"]["frame_rate_index"],
        13
    );
    assert_eq!(result["stream"]["sync_word"]["status"], "not_applicable");
    assert_eq!(result["stream"]["crc_errors"]["status"], "not_applicable");
    assert_eq!(
        result["stream"]["estimated_average_bit_rate"]["status"],
        "not_present"
    );
}

#[test]
fn topology_change_marks_related_fields_unknown_and_records_frame() {
    let path = temp_path("ac4");
    let frames = [
        (topology_frame(0, true, false), false),
        (topology_frame(1, true, true), false),
    ];
    fs::write(&path, annex_g_of(&frames, false)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(result["presentations"][0]["summary"]["status"], "unknown");
    assert_eq!(
        result["stream"]["number_of_presentations"]["status"],
        "unknown"
    );
    assert_eq!(
        result["stream"]["number_of_audio_substreams"]["status"],
        "unknown"
    );
    assert_eq!(
        result["presentations"][0]["presentation_id"]["status"],
        "unknown"
    );
    assert_eq!(
        result["presentations"][0]["presentation_type"]["status"],
        "unknown"
    );
    assert_eq!(
        result["presentations"][0]["minimal_compatibility_level"]["status"],
        "unknown"
    );
    assert_eq!(result["presentations"][0]["multi_pid"]["status"], "unknown");
    assert_eq!(result["presentations"][0]["bit_rate"]["status"], "unknown");
    assert_eq!(
        result["presentations"][0]["audio_substreams"]["status"],
        "unknown"
    );
    assert_eq!(
        result["presentations"][0]["dialogue_normalization"]["status"],
        "unknown"
    );
    for section in [
        "loudness",
        "dynamic_range_control",
        "mixing_metadata",
        "downmix",
    ] {
        assert!(
            result["presentations"][0][section]
                .as_object()
                .unwrap()
                .values()
                .all(|field| field["status"] == "unknown"),
            "{section} 应全部标记为 unknown"
        );
    }
    assert_eq!(
        result["audio_substreams"][0]["channel_layout"]["status"],
        "unknown"
    );
    assert_eq!(
        result["audio_substreams"][0]["preprocessing"]["preferred_downmix"]["status"],
        "unknown"
    );
    assert!(
        result["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| { issue["code"] == "configuration_changed" && issue["frame_index"] == 1 })
    );
}

#[test]
fn json_exposes_all_five_field_states_and_raw_codes() {
    let path = temp_path("m4a");
    let frames = vec![
        topology_frame(0, true, false),
        topology_frame(1, true, true),
    ];
    fs::write(&path, minimal_mp4(&frames)).unwrap();
    let result = json_result(&path);
    let _ = fs::remove_file(&path);

    let encoded = serde_json::to_string(&result).unwrap();
    for state in [
        "present",
        "not_present",
        "not_applicable",
        "unknown",
        "unsupported",
    ] {
        assert!(
            encoded.contains(&format!("\"status\":\"{state}\"")),
            "缺少字段状态 {state}"
        );
    }
    assert_eq!(result["stream"]["bitstream_version"]["raw_code"], 2);
    assert_eq!(result["stream"]["i_frame"]["raw_code"], true);
    assert_eq!(
        result["presentations"][0]["metadata_authentication_id"]["status"],
        "unsupported"
    );
}

#[test]
fn text_default_matches_explicit_format_and_exact_snapshot() {
    let path = temp_path("ac4");
    let frames = [(topology_frame(0, true, false), false)];
    fs::write(&path, annex_g_of(&frames, false)).unwrap();
    let default = run_inspect(&path, None);
    let explicit = run_inspect(&path, Some("text"));
    let _ = fs::remove_file(&path);

    assert!(default.status.success());
    assert_eq!(default.stderr, b"");
    assert_eq!(default.stdout, explicit.stdout);
    assert!(default.stdout.ends_with(b"\n"));
    assert!(!default.stdout.ends_with(b"\n\n"));
    assert_eq!(
        String::from_utf8(default.stdout).unwrap(),
        include_str!("fixtures/inspect-text.txt")
    );
}

#[test]
fn fatal_inputs_return_nonzero_structured_diagnostics() {
    for (name, bytes, code) in [
        ("empty", Vec::new(), "input.invalid"),
        (
            "truncated",
            vec![0xAC, 0x40, 0x00, 0x20, 0x00],
            "parse.failed",
        ),
        (
            "truncated-presentation-metadata",
            annex_g_of(&[(truncated_presentation_metadata_frame(0), false)], false),
            "parse.failed",
        ),
        (
            "truncated-audio-metadata",
            annex_g_of(&[(truncated_audio_metadata_frame(0), false)], false),
            "parse.failed",
        ),
        ("not-mp4", b"not an AC-4 file".to_vec(), "parse.failed"),
    ] {
        let path = temp_path(name);
        fs::write(&path, bytes).unwrap();
        let output = run_inspect(&path, None);
        let _ = fs::remove_file(&path);
        assert!(!output.status.success(), "{name} 不应成功");
        assert!(output.stdout.is_empty());
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["schema"], "macinac4.cli-diagnostic");
        assert_eq!(diagnostic["command"], "inspect");
        assert_eq!(diagnostic["code"], code);
    }
}

fn vector(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
#[ignore = "requires local A-JOC 1500K vector"]
fn local_ajoc_1500k_metadata_regression() {
    let result = json_result(&vector(
        "vectors/probe_axes_single_object/encoded/master_ac4_1500K.m4a",
    ));
    let stream = &result["stream"];
    let presentation = &result["presentations"][0];
    assert_eq!(stream["sample_rate"]["value"], 48_000);
    assert_eq!(stream["frame_rate"]["value"]["decimal"], 23.438);
    assert_eq!(stream["bitstream_version"]["value"], 2);
    assert_eq!(presentation["minimal_compatibility_level"]["value"], 4);
    assert_eq!(
        presentation["summary"]["value"],
        "Object-Based main (single_group)"
    );
    assert_eq!(presentation["dialogue_normalization"]["value"], -16.0);
    assert_eq!(
        presentation["loudness"]["integrated_level_gated"]["value"],
        -15.7
    );
    assert_eq!(presentation["loudness"]["maximum_true_peak"]["value"], -9.6);
    assert_eq!(
        presentation["dynamic_range_control"]["home_theater_avr"]["value"],
        "Music light"
    );
    assert_eq!(result["issues"], serde_json::json!([]));
}

#[test]
#[ignore = "requires local DME native IMS 256K vector"]
fn local_dme_ims_256k_metadata_regression() {
    let result = json_result(&vector(
        "vectors/probe_axes_single_object/encoded/master_ac4_dme_ims_general_damf_256K.m4a",
    ));
    assert_eq!(result["stream"]["frame_rate"]["value"]["decimal"], 24.0);
    assert_eq!(result["stream"]["i_frame_interval"]["value"]["frames"], 24);
    assert_eq!(
        result["audio_substreams"][0]["channel_layout"]["value"],
        "Stereo / IMS"
    );
    assert_eq!(
        result["presentations"][0]["dynamic_range_control"]["home_theater_avr"]["value"],
        "Film light"
    );
    assert_eq!(
        result["audio_substreams"][0]["dialogue_enhancement"]["enabled"]["value"],
        true
    );
    assert_eq!(
        result["audio_substreams"][0]["dialogue_enhancement"]["max_gain"]["value"],
        9
    );
    assert!(result["issues"].as_array().unwrap().iter().any(|issue| {
        issue["code"] == "presentation_metadata_changed"
            && issue["message"]
                .as_str()
                .is_some_and(|message| message.contains("dialnorm"))
    }));
}

#[test]
#[ignore = "requires local DME topology vectors"]
fn local_stereo_surround_immersive_ims_and_ajoc_topologies() {
    for (relative, configuration, layout, object_coded) in [
        (
            "vectors/probe_bed_only/encoded/master_ac4_dme_channel_stereo_64K.m4a",
            "stereo",
            "stereo",
            false,
        ),
        (
            "vectors/probe_bed_only/encoded/master_ac4_dme_channel_5_1_128K.m4a",
            "5.1",
            "5.1",
            false,
        ),
        (
            "vectors/probe_bed_only/encoded/master_ac4_dme_channel_5_1_4_192K.m4a",
            "7.1.4",
            "5.1.4",
            false,
        ),
        (
            "vectors/probe_axes_single_object/encoded/master_ac4_dme_ims_general_damf_256K.m4a",
            "Stereo / IMS",
            "Stereo / IMS",
            false,
        ),
        (
            "vectors/probe_axes_single_object/encoded/master_ac4_1500K.m4a",
            "Object-Based",
            "Object-Based",
            true,
        ),
    ] {
        let result = json_result(&vector(relative));
        let substream = &result["audio_substreams"][0];
        assert_eq!(substream["channel_configuration"]["value"], configuration);
        assert_eq!(substream["channel_layout"]["value"], layout);
        assert_eq!(substream["object_coded"]["value"], object_coded);
    }
}
