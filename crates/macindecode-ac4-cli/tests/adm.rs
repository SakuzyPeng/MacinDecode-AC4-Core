#![cfg(feature = "audio-decode")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试按固定 BW64/CHNA/PCM 布局核对已生成文件"
)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use macindecode_ac4_mp4::{BoxIter, SampleTable, find_box, find_path};

use common::{require_probe_axes_vector, success};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn assert_well_formed_xml(xml: &str) {
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        let event = reader.read_event().expect("AXML 应是结构完整的 XML");
        if matches!(event, quick_xml::events::Event::Eof) {
            break;
        }
    }
}

fn assert_output_exists_diagnostic(stderr: &[u8]) {
    let diagnostics = std::str::from_utf8(stderr)
        .expect("诊断应为 UTF-8")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("诊断应为 JSONL"))
        .collect::<Vec<_>>();
    assert_eq!(
        diagnostics.last().and_then(|item| item["code"].as_str()),
        Some("output.exists")
    );
}

fn unique_output(suffix: &str) -> PathBuf {
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "macinac4-adm-{}-{serial}-{suffix}.wav",
        std::process::id()
    ))
}

fn raw_stream_from_vector(vector: &Path) -> Vec<u8> {
    let data = fs::read(vector).expect("应能读取 MP4 向量");
    let moov = find_box(&data, b"moov").expect("向量应有 moov");
    let stbl = BoxIter::new(moov.payload)
        .flatten()
        .filter(|item| item.is(b"trak"))
        .find_map(|trak| {
            let stbl = find_path(trak.payload, &[*b"mdia", *b"minf", *b"stbl"])?;
            let stsd = find_box(stbl.payload, b"stsd")?;
            BoxIter::new(stsd.payload.get(8..)?)
                .flatten()
                .any(|entry| entry.is(b"ac-4"))
                .then_some(stbl)
        })
        .expect("向量应有 AC-4 sample table");
    let table = SampleTable::parse(stbl.payload).expect("sample table 应合法");
    let mut raw = Vec::new();
    for item in table.iter() {
        let info = item.expect("sample 应合法");
        let start = usize::try_from(info.offset).expect("sample offset 应可表示");
        let size = usize::try_from(info.size).expect("sample size 应可表示");
        let frame = data
            .get(start..start.saturating_add(size))
            .expect("sample 应位于文件内");
        raw.extend_from_slice(&0xAC40u16.to_be_bytes());
        let extended = u32::try_from(frame.len()).expect("frame 应小于 4 GiB");
        if extended < u32::from(u16::MAX) {
            raw.extend_from_slice(
                &u16::try_from(extended)
                    .expect("已检查短尺寸范围")
                    .to_be_bytes(),
            );
        } else {
            assert!(extended <= 0x00FF_FFFF, "sync frame 应可用 24 比特定界");
            raw.extend_from_slice(&u16::MAX.to_be_bytes());
            raw.extend_from_slice(&extended.to_be_bytes()[1..]);
        }
        raw.extend_from_slice(frame);
    }
    raw
}

#[derive(Debug)]
struct ParsedBw64<'a> {
    bw64_size: u64,
    data_size: u64,
    sample_count: u64,
    chunks: Vec<([u8; 4], &'a [u8])>,
}

fn parse_bw64(data: &[u8]) -> ParsedBw64<'_> {
    parse_64bit_wave(data, b"BW64")
}

fn parse_rf64(data: &[u8]) -> ParsedBw64<'_> {
    parse_64bit_wave(data, b"RF64")
}

fn parse_64bit_wave<'a>(data: &'a [u8], container: &[u8; 4]) -> ParsedBw64<'a> {
    assert_eq!(data.get(..4), Some(container.as_slice()));
    assert_eq!(data.get(4..8), Some(u32::MAX.to_le_bytes().as_slice()));
    assert_eq!(data.get(8..12), Some(b"WAVE".as_slice()));
    assert_eq!(data.get(12..16), Some(b"ds64".as_slice()));
    let ds64_len = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;
    assert!(ds64_len >= 28);
    let bw64_size = u64::from_le_bytes(data[20..28].try_into().unwrap());
    let data_size = u64::from_le_bytes(data[28..36].try_into().unwrap());
    let sample_count = u64::from_le_bytes(data[36..44].try_into().unwrap());

    let mut chunks = Vec::new();
    let mut offset = 12usize;
    while offset + 8 <= data.len() {
        let id: [u8; 4] = data[offset..offset + 4].try_into().unwrap();
        let size32 = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
        let size = if id == *b"data" && size32 == u32::MAX {
            usize::try_from(data_size).expect("测试 dataSize 应可表示")
        } else {
            usize::try_from(size32).expect("测试 chunk size 应可表示")
        };
        let start = offset + 8;
        let end = start.checked_add(size).expect("chunk end 不应溢出");
        let payload = data.get(start..end).expect("chunk 应位于文件内");
        chunks.push((id, payload));
        offset = end + (size & 1);
    }
    assert_eq!(offset, data.len());
    ParsedBw64 {
        bw64_size,
        data_size,
        sample_count,
        chunks,
    }
}

impl<'a> ParsedBw64<'a> {
    fn chunk(&self, id: &[u8; 4]) -> &'a [u8] {
        self.chunks
            .iter()
            .find_map(|(found, payload)| (found == id).then_some(*payload))
            .unwrap_or_else(|| panic!("缺少 chunk {}", String::from_utf8_lossy(id)))
    }
}

fn dbmd_segments(dbmd: &[u8]) -> Vec<(u8, &[u8])> {
    assert_eq!(dbmd.get(..4), Some([0x06, 0, 0, 1].as_slice()));
    let mut segments = Vec::new();
    let mut offset = 4usize;
    while dbmd.get(offset).copied().is_some_and(|id| id != 0) {
        let id = dbmd[offset];
        let size = usize::from(u16::from_le_bytes(
            dbmd[offset + 1..offset + 3].try_into().unwrap(),
        ));
        let payload_start = offset + 3;
        let payload_end = payload_start + size;
        let payload = &dbmd[payload_start..payload_end];
        let checksum = dbmd[payload_end];
        let sum = payload.iter().fold(
            u16::try_from(size).unwrap().to_le_bytes()[0],
            |sum, byte| sum.wrapping_add(*byte),
        );
        assert_eq!(sum.wrapping_add(checksum), 0, "segment {id} 校验和应闭合");
        segments.push((id, payload));
        offset = payload_end + 1;
    }
    assert_eq!(dbmd[offset], 0, "DBMD 必须以 segment 0 结束");
    assert!(
        dbmd[offset + 1..].iter().all(|byte| *byte == 0),
        "terminator 后只能有 RIFF 对齐零"
    );
    segments
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn exports_forced_bw64_with_exact_adm_timeline() {
    let vector = require_probe_axes_vector();
    let output_path = unique_output("mp4");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-adm-bwf")
        .arg(&vector)
        .arg("--output")
        .arg(&output_path)
        .arg("--object")
        .arg("2:1")
        .output()
        .expect("应能启动 CLI");
    assert!(
        output.status.success(),
        "export-adm-bwf 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = success("export-adm-bwf", &output.stdout);
    let result = &stdout["result"];
    assert_eq!(result["container"], "BW64");
    assert_eq!(result["profile"], "ITU-R_BS.2076-2");
    assert_eq!(result["audio"]["frames"], 288000);
    assert_eq!(result["objects"][0]["track_index"], 11);

    let data = fs::read(&output_path).expect("ADM BW64 应存在");
    let parsed = parse_bw64(&data);
    assert_eq!(parsed.bw64_size, u64::try_from(data.len()).unwrap() - 8);
    assert_eq!(parsed.data_size, 288_000 * 11 * 3);
    assert_eq!(parsed.sample_count, 0);
    let chunk_ids = parsed.chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    assert_eq!(
        chunk_ids,
        [*b"ds64", *b"fmt ", *b"chna", *b"axml", *b"data"]
    );

    let format = parsed.chunk(b"fmt ");
    assert_eq!(u16::from_le_bytes(format[0..2].try_into().unwrap()), 1);
    assert_eq!(u16::from_le_bytes(format[2..4].try_into().unwrap()), 11);
    assert_eq!(u32::from_le_bytes(format[4..8].try_into().unwrap()), 48_000);
    assert_eq!(u16::from_le_bytes(format[14..16].try_into().unwrap()), 24);

    let chna = parsed.chunk(b"chna");
    assert_eq!(u16::from_le_bytes(chna[0..2].try_into().unwrap()), 11);
    assert_eq!(u16::from_le_bytes(chna[2..4].try_into().unwrap()), 11);
    let object_entry = &chna[4 + 10 * 40..4 + 11 * 40];
    assert_eq!(&object_entry[2..14], b"ATU_0000000b");
    assert_eq!(&object_entry[14..28], b"AT_00031001_01");
    assert_eq!(&object_entry[28..39], b"AP_00031001");

    let axml = std::str::from_utf8(parsed.chunk(b"axml")).expect("AXML 应为 UTF-8");
    assert_well_formed_xml(axml);
    assert!(axml.contains("version=\"ITU-R_BS.2076-2\""));
    assert!(axml.contains("end=\"00:00:06.000000000\""));
    assert!(axml.contains("<audioObjectIDRef>AO_100b</audioObjectIDRef>"));
    assert!(axml.contains("<audioTrackUIDRef>ATU_0000000b</audioTrackUIDRef>"));
    assert!(axml.contains("rtime=\"00:00:00.000000000\" duration=\"00:00:00.042666667\""));
    assert!(axml.contains("<position coordinate=\"X\">-1</position>"));
    assert!(axml.contains("interpolationLength=\"0.042666667\""));
    assert!(axml.contains("rtime=\"00:00:00.042666667\""));

    let pcm = parsed.chunk(b"data");
    assert_eq!(pcm.len(), 288_000 * 11 * 3);
    let frame = 500usize;
    let frame_start = frame * 11 * 3;
    assert!(
        pcm[frame_start..frame_start + 10 * 3]
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_ne!(&pcm[frame_start + 10 * 3..frame_start + 11 * 3], &[0, 0, 0]);

    fs::remove_file(output_path).expect("应能清理测试输出");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn exports_full_objects_as_standard_and_logic_adm_with_identical_pcm() {
    let vector = require_probe_axes_vector();
    let standard_path = unique_output("full-standard");
    let logic_path = unique_output("full-logic");

    let standard_output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-adm-bwf")
        .arg(&vector)
        .arg("--output")
        .arg(&standard_path)
        .output()
        .expect("应能启动 standard full ADM 导出");
    assert!(
        standard_output.status.success(),
        "standard full ADM 失败：{}",
        String::from_utf8_lossy(&standard_output.stderr)
    );
    let standard_stdout = success("export-full-adm-bwf", &standard_output.stdout);
    let standard_result = &standard_stdout["result"];
    assert_eq!(standard_result["artifacts"][0]["kind"], "full_adm_bwf");
    assert_eq!(standard_result["container"], "BW64");
    assert_eq!(standard_result["compatibility"], "standard");
    assert_eq!(standard_result["dbmd_bytes"], serde_json::Value::Null);
    assert_eq!(standard_result["audio"]["channels"], 30);
    assert_eq!(standard_result["audio"]["frames"], 288_000);
    assert_eq!(
        standard_result["objects"].as_array().map(Vec::len),
        Some(20)
    );
    assert_eq!(standard_result["tracks"].as_array().map(Vec::len), Some(30));
    assert_eq!(standard_result["tracks"][3]["essence"], "lfe");
    assert_eq!(standard_result["tracks"][3]["source_output_channel"], 0);
    assert_eq!(standard_result["objects"][0]["selector"], "2:1");
    assert_eq!(standard_result["objects"][0]["ajoc_object"], 0);
    assert_eq!(standard_result["objects"][0]["source_output_channel"], 1);
    assert_eq!(standard_result["objects"][19]["track_index"], 30);
    assert_eq!(
        standard_result["channel_order"],
        "7.1.2_bed_then_ajoc_full_objects"
    );

    let logic_output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-adm-bwf")
        .arg(&vector)
        .arg("--output")
        .arg(&logic_path)
        .arg("--compatibility")
        .arg("logic")
        .arg("--fps")
        .arg("29.97df")
        .output()
        .expect("应能启动 Logic full ADM 导出");
    assert!(
        logic_output.status.success(),
        "Logic full ADM 失败：{}",
        String::from_utf8_lossy(&logic_output.stderr)
    );
    let logic_stdout = success("export-full-adm-bwf", &logic_output.stdout);
    let logic_result = &logic_stdout["result"];
    assert_eq!(logic_result["container"], "RF64");
    assert_eq!(logic_result["compatibility"], "logic");
    assert_eq!(logic_result["dbmd_bytes"], 564);
    assert_eq!(logic_result["audio"], standard_result["audio"]);
    assert_eq!(logic_result["objects"], standard_result["objects"]);
    assert_eq!(logic_result["tracks"], standard_result["tracks"]);

    let standard_data = fs::read(&standard_path).expect("standard full ADM 应存在");
    let logic_data = fs::read(&logic_path).expect("Logic full ADM 应存在");
    let standard = parse_bw64(&standard_data);
    let logic = parse_rf64(&logic_data);
    assert_eq!(standard.data_size, 288_000 * 30 * 3);
    assert_eq!(logic.data_size, standard.data_size);
    assert_eq!(standard.sample_count, 0);
    assert_eq!(logic.sample_count, 288_000);
    assert_eq!(
        standard
            .chunks
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [*b"ds64", *b"fmt ", *b"chna", *b"axml", *b"data"]
    );
    assert_eq!(
        logic.chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [*b"ds64", *b"fmt ", *b"chna", *b"axml", *b"dbmd", *b"data"]
    );
    assert_eq!(standard.chunk(b"data"), logic.chunk(b"data"));

    let chna = standard.chunk(b"chna");
    assert_eq!(chna.len(), 4 + 30 * 40);
    assert_eq!(u16::from_le_bytes(chna[0..2].try_into().unwrap()), 30);
    assert_eq!(u16::from_le_bytes(chna[2..4].try_into().unwrap()), 30);
    let standard_axml =
        std::str::from_utf8(standard.chunk(b"axml")).expect("standard AXML 应为 UTF-8");
    let logic_axml = std::str::from_utf8(logic.chunk(b"axml")).expect("Logic AXML 应为 UTF-8");
    assert_well_formed_xml(standard_axml);
    assert_well_formed_xml(logic_axml);
    assert!(standard_axml.contains("AC-4 full 7.1.2 bed"));
    assert!(standard_axml.contains("AC-4 full object 2:1"));
    assert!(!standard_axml.contains("AC-4 core object"));
    assert!(standard_axml.contains("end=\"00:00:06.000000000\""));
    assert!(logic_axml.contains("end=\"00:00:06.00000\""));
    assert!(standard_axml.contains("interpolationLength=\"0.042666667\""));
    assert!(logic_axml.contains("interpolationLength=\"0.042666667\""));
    for track in 1..=30u16 {
        let uid = format!("UID=\"ATU_{track:08x}\"");
        assert!(standard_axml.contains(&uid), "AXML 缺少 {uid}");
    }

    let segments = dbmd_segments(logic.chunk(b"dbmd"));
    assert_eq!(
        segments.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [7, 9, 10]
    );
    assert_eq!(segments[1].1[111], 0x24, "29.97df 帧率码必须写入 segment 9");
    let supplemental = segments[2].1;
    assert_eq!(&supplemental[..4], &[0xbd, 0x6f, 0x72, 0xf8]);
    assert_eq!(
        u16::from_le_bytes(supplemental[4..6].try_into().unwrap()),
        30
    );
    assert_eq!(supplemental.len(), 202);
    let channel_flags = &supplemental[172..202];
    for (channel, flag) in channel_flags.iter().copied().enumerate() {
        assert_eq!(flag, if channel == 3 { 0x40 } else { 0x44 });
    }

    fs::remove_file(standard_path).expect("应能清理 standard full ADM");
    fs::remove_file(logic_path).expect("应能清理 Logic full ADM");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn logic_profile_exports_rf64_five_digit_clocks_and_valid_dbmd() {
    let vector = require_probe_axes_vector();
    let output_path = unique_output("logic");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-adm-bwf")
        .arg(&vector)
        .arg("--output")
        .arg(&output_path)
        .arg("--object")
        .arg("2:1")
        .arg("--compatibility")
        .arg("logic")
        .arg("--fps")
        .arg("29.97df")
        .output()
        .expect("应能启动 CLI");
    assert!(
        output.status.success(),
        "Logic profile 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = success("export-adm-bwf", &output.stdout);
    let result = &stdout["result"];
    assert_eq!(result["container"], "RF64");
    assert_eq!(result["compatibility"], "logic");
    assert_eq!(result["dbmd_bytes"], 526);

    let data = fs::read(&output_path).expect("Logic ADM RF64 应存在");
    let parsed = parse_rf64(&data);
    assert_eq!(parsed.bw64_size, u64::try_from(data.len()).unwrap() - 8);
    assert_eq!(parsed.data_size, 288_000 * 11 * 3);
    assert_eq!(parsed.sample_count, 288_000);
    let chunk_ids = parsed.chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    assert_eq!(
        chunk_ids,
        [*b"ds64", *b"fmt ", *b"chna", *b"axml", *b"dbmd", *b"data"]
    );

    let axml = std::str::from_utf8(parsed.chunk(b"axml")).expect("AXML 应为 UTF-8");
    assert!(axml.contains("end=\"00:00:06.00000\""));
    assert!(axml.contains("rtime=\"00:00:00.00000\" duration=\"00:00:00.04267\""));
    assert!(axml.contains("rtime=\"00:00:00.04267\" duration=\"00:00:00.04266\""));
    assert!(axml.contains("interpolationLength=\"0.042666667\""));

    let dbmd = parsed.chunk(b"dbmd");
    assert_eq!(dbmd.len(), 526);
    assert_eq!(dbmd.get(..4), Some([0x06, 0, 0, 1].as_slice()));
    let segment_9 = 104usize;
    assert_eq!(dbmd[segment_9], 9);
    assert_eq!(
        u16::from_le_bytes(dbmd[segment_9 + 1..segment_9 + 3].try_into().unwrap()),
        248
    );
    assert_eq!(
        &dbmd[segment_9 + 3 + 32..segment_9 + 3 + 53],
        b"MacinDecode AC-4 Core"
    );
    assert_eq!(
        &dbmd[segment_9 + 3..segment_9 + 3 + 22],
        b"Created by MacinDecode"
    );
    assert!(
        !dbmd
            .windows(b"Dolby".len())
            .any(|window| window == b"Dolby")
    );
    assert_eq!(dbmd[segment_9 + 3 + 111], 0x24);
    let segment_10 = segment_9 + 3 + 248 + 1;
    assert_eq!(dbmd[segment_10], 10);
    assert_eq!(
        &dbmd[segment_10 + 3..segment_10 + 7],
        &[0xbd, 0x6f, 0x72, 0xf8]
    );
    assert_eq!(
        u16::from_le_bytes(dbmd[segment_10 + 7..segment_10 + 9].try_into().unwrap()),
        11
    );

    fs::remove_file(output_path).expect("应能清理测试输出");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn raw_ac4_uses_codec_timeline_and_stays_bw64() {
    let vector = require_probe_axes_vector();
    let raw_path = unique_output("raw-input").with_extension("ac4");
    let output_path = unique_output("raw-output");
    fs::write(&raw_path, raw_stream_from_vector(&vector)).expect("应能写裸流夹具");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-adm-bwf")
        .arg(&raw_path)
        .arg("--output")
        .arg(&output_path)
        .arg("--object")
        .arg("2:1")
        .output()
        .expect("应能启动 CLI");
    assert!(
        output.status.success(),
        "裸流 export-adm-bwf 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = fs::read(&output_path).expect("裸流 ADM BW64 应存在");
    let parsed = parse_bw64(&data);
    assert_eq!(parsed.data_size, 292_864 * 11 * 3);
    let axml = std::str::from_utf8(parsed.chunk(b"axml")).expect("AXML 应为 UTF-8");
    assert!(axml.contains("end=\"00:00:06.101333333\""));

    fs::remove_file(raw_path).expect("应能清理裸流夹具");
    fs::remove_file(output_path).expect("应能清理测试输出");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn strict_mapping_leaves_no_partial_file() {
    let vector = require_probe_axes_vector();
    let strict_path = unique_output("strict");
    let strict = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-adm-bwf")
        .arg(&vector)
        .arg("--output")
        .arg(&strict_path)
        .arg("--object")
        .arg("2:1")
        .arg("--strict-mapping")
        .output()
        .expect("应能启动 CLI");
    assert!(!strict.status.success());
    assert!(!strict_path.exists(), "严格映射失败不应留下半文件");

    let full_strict_path = unique_output("full-strict");
    let full_strict = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-adm-bwf")
        .arg(&vector)
        .arg("--output")
        .arg(&full_strict_path)
        .arg("--strict-mapping")
        .output()
        .expect("应能启动 full ADM CLI");
    assert!(!full_strict.status.success());
    let errors = std::str::from_utf8(&full_strict.stderr)
        .expect("诊断应为 UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    assert_eq!(
        errors.last().and_then(|item| item["code"].as_str()),
        Some("mapping.unsupported")
    );
    assert!(
        !full_strict_path.exists(),
        "full 严格映射失败不应留下半文件"
    );
}

#[test]
fn existing_output_is_never_replaced() {
    let existing_path = unique_output("existing");
    fs::write(&existing_path, b"keep").expect("应能创建冲突文件");
    let existing = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-adm-bwf")
        .arg("definitely-unused.ac4")
        .arg("--output")
        .arg(&existing_path)
        .arg("--object")
        .arg("2:1")
        .output()
        .expect("应能启动 CLI");
    assert!(!existing.status.success());
    assert_output_exists_diagnostic(&existing.stderr);
    assert_eq!(fs::read(&existing_path).unwrap(), b"keep");
    fs::remove_file(existing_path).expect("应能清理冲突文件");

    let full_existing_path = unique_output("full-existing");
    fs::write(&full_existing_path, b"keep-full").expect("应能创建 full 冲突文件");
    let full_existing = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-adm-bwf")
        .arg("definitely-unused.ac4")
        .arg("--output")
        .arg(&full_existing_path)
        .output()
        .expect("应能启动 full ADM CLI");
    assert!(!full_existing.status.success());
    assert_output_exists_diagnostic(&full_existing.stderr);
    assert_eq!(fs::read(&full_existing_path).unwrap(), b"keep-full");
    fs::remove_file(full_existing_path).expect("应能清理 full 冲突文件");
}
