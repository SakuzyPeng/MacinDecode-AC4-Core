#![cfg(feature = "audio-decode")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试按固定 CAF 头布局核对已生成文件"
)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use macindecode_ac4_mp4::{BoxIter, SampleTable, find_box, find_path};

use common::{require_probe_axes_vector, success};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn warning_diagnostics(stderr: &[u8]) -> Vec<serde_json::Value> {
    std::str::from_utf8(stderr)
        .expect("stderr 应为 UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("warning 应为 JSONL"))
        .collect()
}

fn unique_output() -> PathBuf {
    let serial = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("macinac4-damf-{}-{serial}", std::process::id()))
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
            let short = u16::try_from(extended).expect("已检查短尺寸范围");
            raw.extend_from_slice(&short.to_be_bytes());
        } else {
            assert!(extended <= 0x00FF_FFFF, "sync frame 应可用 24 比特定界");
            raw.extend_from_slice(&u16::MAX.to_be_bytes());
            raw.extend_from_slice(&extended.to_be_bytes()[1..]);
        }
        raw.extend_from_slice(frame);
    }
    raw
}

fn bw64_pcm(data: &[u8]) -> &[u8] {
    assert_eq!(data.get(..4), Some(b"BW64".as_slice()));
    assert_eq!(data.get(12..16), Some(b"ds64".as_slice()));
    let data_size = usize::try_from(u64::from_le_bytes(
        data[28..36].try_into().expect("ds64 dataSize"),
    ))
    .expect("测试 PCM 应可表示为 usize");
    let mut offset = 12usize;
    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let size32 =
            u32::from_le_bytes(data[offset + 4..offset + 8].try_into().expect("chunk size"));
        let size = if id == b"data" && size32 == u32::MAX {
            data_size
        } else {
            usize::try_from(size32).expect("chunk size 应可表示")
        };
        let start = offset + 8;
        let end = start.checked_add(size).expect("chunk end 不应溢出");
        if id == b"data" {
            return data.get(start..end).expect("data chunk 应位于文件内");
        }
        offset = end + (size & 1);
    }
    panic!("BW64 缺少 data chunk")
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn exports_valid_three_piece_probe_with_presented_timeline() {
    let vector = require_probe_axes_vector();
    let output_dir = unique_output();
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-damf")
        .arg(&vector)
        .arg("--output")
        .arg(&output_dir)
        .arg("--object")
        .arg("2:1")
        .output()
        .expect("应能启动 CLI");
    assert!(
        output.status.success(),
        "export-damf 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = success("export-damf", &output.stdout);
    assert_eq!(stdout["result"]["audio"]["frames"], 288000);
    assert_eq!(stdout["result"]["audio"]["format"], "caf_s24le");
    assert_eq!(stdout["result"]["objects"][0]["selector"], "2:1");
    let warnings = warning_diagnostics(&output.stderr);
    assert_eq!(
        warnings.len(),
        stdout["result"]["unmapped"].as_array().unwrap().len()
    );
    assert!(
        warnings
            .iter()
            .all(|item| item["code"] == "mapping.lossy" && item["level"] == "warning")
    );

    let stem = "master_ac4_768K";
    let manifest_path = output_dir.join(format!("{stem}.atmos"));
    let metadata_path = output_dir.join(format!("{stem}.atmos.metadata"));
    let audio_path = output_dir.join(format!("{stem}.atmos.audio"));
    let manifest = fs::read_to_string(&manifest_path).expect("manifest 应存在");
    let metadata = fs::read_to_string(&metadata_path).expect("metadata 应存在");
    let audio = fs::read(&audio_path).expect("CAF 应存在");

    assert!(manifest.contains("version: 0.5.1"));
    assert!(manifest.contains("metadata: \"master_ac4_768K.atmos.metadata\""));
    assert!(manifest.contains("audio: \"master_ac4_768K.atmos.audio\""));
    assert!(manifest.contains("ID: 10"));
    assert!(metadata.contains("sampleRate: 48000"));
    assert!(metadata.contains("samplePos: 0\n    active: true\n    pos: [-1, 0, 0]"));
    assert!(metadata.contains("samplePos: 2048"));
    assert!(metadata.contains("rampLength: 2048"));

    assert_eq!(audio.get(..4), Some(b"caff".as_slice()));
    let channels = u32::from_be_bytes(audio[44..48].try_into().expect("CAF channel field"));
    assert_eq!(channels, 11, "10 路静音 bed 加 1 路对象探针");
    let data_size = i64::from_be_bytes(audio[56..64].try_into().expect("CAF data size"));
    assert_eq!(data_size, 288_000 * 11 * 3 + 4);
    assert_eq!(audio.len(), 68 + 288_000 * 11 * 3);

    fs::remove_dir_all(&output_dir).expect("应能清理测试输出");
}

#[test]
fn refuses_to_overwrite_an_existing_output_directory() {
    let output_dir = unique_output();
    fs::create_dir(&output_dir).expect("应能创建冲突目录");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-damf")
        .arg("definitely-unused.ac4")
        .arg("--output")
        .arg(&output_dir)
        .arg("--object")
        .arg("2:1")
        .output()
        .expect("应能启动 CLI");
    assert!(!output.status.success());
    let diagnostic: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("stderr 应为诊断 JSONL");
    assert_eq!(diagnostic["code"], "output.exists");
    fs::remove_dir(&output_dir).expect("应能清理冲突目录");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn exports_a_raw_ac4_sync_stream_on_the_codec_timeline() {
    let vector = require_probe_axes_vector();
    let output_dir = unique_output();
    let raw_path = output_dir.with_extension("ac4");
    fs::write(&raw_path, raw_stream_from_vector(&vector)).expect("应能写临时裸流");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-damf")
        .arg(&raw_path)
        .arg("--output")
        .arg(&output_dir)
        .arg("--object")
        .arg("2:1")
        .output()
        .expect("应能启动 CLI");
    assert!(
        output.status.success(),
        "裸流 export-damf 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = success("export-damf", &output.stdout);
    assert_eq!(
        stdout["result"]["audio"]["frames"], 292864,
        "裸流应按 143 个 2048-sample codec frame 累积"
    );
    let stem = raw_path
        .file_stem()
        .and_then(|value| value.to_str())
        .expect("临时文件名应为 UTF-8");
    let metadata = fs::read_to_string(output_dir.join(format!("{stem}.atmos.metadata")))
        .expect("裸流 metadata 应存在");
    assert!(metadata.contains("samplePos: 0"));

    fs::remove_dir_all(&output_dir).expect("应能清理裸流输出");
    fs::remove_file(&raw_path).expect("应能清理临时裸流");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn strict_mapping_fails_before_creating_the_package() {
    let vector = require_probe_axes_vector();
    let output_dir = unique_output();
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-damf")
        .arg(&vector)
        .arg("--output")
        .arg(&output_dir)
        .arg("--object")
        .arg("2:1")
        .arg("--strict-mapping")
        .output()
        .expect("应能启动 CLI");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Strict mapping rejected"),
        "实际向量的九配置 trim 应作为无法无损映射项：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output_dir.exists(), "失败不得留下半包");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn exports_full_home_and_3dof_with_identical_metadata_and_real_adm_pcm() {
    let vector = require_probe_axes_vector();
    let home_dir = unique_output();
    let dof_dir = unique_output();
    let adm_path = unique_output().with_extension("wav");
    let run = |output: &Path, presentation_type: &str| {
        Command::new(env!("CARGO_BIN_EXE_macinac4"))
            .arg("export-full-damf")
            .arg(&vector)
            .arg("--output")
            .arg(output)
            .arg("--presentation-type")
            .arg(presentation_type)
            .arg("--fps")
            .arg("29.97df")
            .arg("--stem")
            .arg("full")
            .output()
            .expect("应能启动 full DAMF CLI")
    };

    let home_output = run(&home_dir, "home");
    assert!(
        home_output.status.success(),
        "home full DAMF 失败：{}",
        String::from_utf8_lossy(&home_output.stderr)
    );
    let home_stdout = success("export-full-damf", &home_output.stdout);
    let home_result = &home_stdout["result"];
    assert_eq!(home_result["package"]["version"], "0.5.1");
    assert_eq!(home_result["package"]["type"], "home");
    assert_eq!(home_result["package"]["stem_artifacts"], 3);
    assert_eq!(home_result["audio"]["format"], "caf_s24le");
    assert_eq!(home_result["audio"]["channels"], 30);
    assert_eq!(home_result["audio"]["frames"], 288_000);
    assert_eq!(home_result["objects"].as_array().map(Vec::len), Some(20));
    assert_eq!(home_result["tracks"].as_array().map(Vec::len), Some(30));
    assert_eq!(home_result["objects"][0]["selector"], "2:1");
    assert_eq!(home_result["objects"][0]["ajoc_object"], 0);
    assert_eq!(home_result["objects"][0]["source_output_channel"], 1);
    assert_eq!(home_result["objects"][0]["damf_id"], 10);
    assert_eq!(home_result["objects"][19]["track_index"], 30);
    assert_eq!(home_result["tracks"][3]["essence"], "lfe");
    assert_eq!(home_result["tracks"][3]["source_output_channel"], 0);
    assert_eq!(
        home_result["channel_order"],
        "7.1.2_bed_then_ajoc_full_objects"
    );
    assert_eq!(
        home_result["artifacts"]
            .as_array()
            .expect("artifacts 应为数组")
            .iter()
            .map(|item| item["kind"].as_str().expect("kind 应为字符串"))
            .collect::<Vec<_>>(),
        [
            "full_damf_manifest",
            "full_damf_metadata",
            "full_damf_audio"
        ]
    );

    let dof_output = run(&dof_dir, "3dof");
    assert!(
        dof_output.status.success(),
        "3DoF full DAMF 失败：{}",
        String::from_utf8_lossy(&dof_output.stderr)
    );
    let dof_stdout = success("export-full-damf", &dof_output.stdout);
    let dof_result = &dof_stdout["result"];
    assert_eq!(dof_result["package"]["version"], "0.6.0");
    assert_eq!(dof_result["package"]["type"], "3dof");
    for key in [
        "audio",
        "objects",
        "tracks",
        "scale",
        "bandwidth",
        "channel_order",
        "unmapped",
    ] {
        assert_eq!(home_result[key], dof_result[key], "{key} 不应随类型变化");
    }

    let home_manifest = fs::read_to_string(home_dir.join("full.atmos")).expect("home manifest");
    let dof_manifest = fs::read_to_string(dof_dir.join("full.atmos")).expect("3DoF manifest");
    let converted = home_manifest
        .replacen("version: 0.5.1", "version: 0.6.0", 1)
        .replacen("  - type: home", "  - type: 3dof", 1);
    assert_eq!(converted, dof_manifest, "manifest 只能改变版本与 type");
    assert!(home_manifest.contains("fps: 29.97df"));
    assert!(home_manifest.contains("AC-4 full object 2:20"));

    let home_metadata = fs::read(home_dir.join("full.atmos.metadata")).expect("home metadata");
    let dof_metadata = fs::read(dof_dir.join("full.atmos.metadata")).expect("3DoF metadata");
    assert_eq!(home_metadata, dof_metadata);
    let metadata_text = std::str::from_utf8(&home_metadata).expect("metadata 应为 UTF-8");
    assert!(metadata_text.contains("headTrackMode: scene relative"));
    assert!(metadata_text.contains("rampLength: 2048"));
    assert!(metadata_text.contains("size:"));

    let home_audio = fs::read(home_dir.join("full.atmos.audio")).expect("home CAF");
    let dof_audio = fs::read(dof_dir.join("full.atmos.audio")).expect("3DoF CAF");
    assert_eq!(home_audio, dof_audio);
    assert_eq!(home_audio.get(..4), Some(b"caff".as_slice()));
    assert_eq!(
        u32::from_be_bytes(home_audio[32..36].try_into().expect("CAF flags")),
        2,
        "CAF 必须是 signed little-endian integer"
    );
    assert_eq!(
        u32::from_be_bytes(home_audio[44..48].try_into().expect("CAF channels")),
        30
    );
    let caf_pcm = home_audio.get(68..).expect("CAF payload 应从 68 开始");
    assert_eq!(caf_pcm.len(), 288_000 * 30 * 3);
    let mut nonzero = [false; 30];
    for frame in caf_pcm.as_chunks::<{ 30 * 3 }>().0 {
        for (track, sample) in frame.as_chunks::<3>().0.iter().enumerate() {
            nonzero[track] |= *sample != [0, 0, 0];
        }
    }
    for track in [0usize, 1, 2, 4, 5, 6, 7, 8, 9] {
        assert!(!nonzero[track], "bed track {} 必须静音", track + 1);
    }
    assert!(
        nonzero[10..].iter().any(|value| *value),
        "full Objects 不得退化为全静音或试听探针"
    );

    let adm_output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-adm-bwf")
        .arg(&vector)
        .arg("--output")
        .arg(&adm_path)
        .output()
        .expect("应能启动 full ADM CLI");
    assert!(
        adm_output.status.success(),
        "full ADM 失败：{}",
        String::from_utf8_lossy(&adm_output.stderr)
    );
    let adm = fs::read(&adm_path).expect("full ADM 应存在");
    assert_eq!(
        caf_pcm,
        bw64_pcm(&adm),
        "DAMF CAF 与 full ADM PCM 必须逐字节一致"
    );

    fs::remove_dir_all(home_dir).expect("应能清理 home package");
    fs::remove_dir_all(dof_dir).expect("应能清理 3DoF package");
    fs::remove_file(adm_path).expect("应能清理 full ADM");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn full_strict_mapping_fails_without_leaving_a_package() {
    let vector = require_probe_axes_vector();
    let output_dir = unique_output();
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-damf")
        .arg(&vector)
        .arg("--output")
        .arg(&output_dir)
        .arg("--strict-mapping")
        .output()
        .expect("应能启动 full DAMF CLI");
    assert!(!output.status.success());
    let diagnostics = warning_diagnostics(&output.stderr);
    assert_eq!(
        diagnostics.last().and_then(|item| item["code"].as_str()),
        Some("mapping.unsupported")
    );
    assert!(!output_dir.exists(), "full 映射失败不得留下半包");
}
