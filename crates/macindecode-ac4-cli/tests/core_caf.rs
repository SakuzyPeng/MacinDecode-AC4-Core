#![cfg(feature = "audio-decode")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试按固定 CAF chunk 与 interleaved PCM 布局核对产物"
)]

mod common;

use common::{require_probe_axes_vector, success};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn unique_output(extension: &str) -> PathBuf {
    let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "macinac4-core-caf-{}-{serial}.{extension}",
        std::process::id(),
    ))
}

fn wave_pcm(data: &[u8]) -> &[u8] {
    assert_eq!(&data[0..4], b"RIFF");
    assert_eq!(&data[8..12], b"WAVE");
    let mut offset = 12usize;
    while offset + 8 <= data.len() {
        let size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = offset + 8;
        let end = body.checked_add(size).expect("WAVE chunk 大小不应溢出");
        assert!(end <= data.len(), "WAVE chunk 不应越过文件末尾");
        if &data[offset..offset + 4] == b"data" {
            return &data[body..end];
        }
        offset = end + (size & 1);
    }
    panic!("WAVE 应包含 data chunk");
}

#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn exports_real_768k_grid_as_float_514_caf() {
    let vector = require_probe_axes_vector();
    let output_path = unique_output("caf");
    let source_path = unique_output("wav");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-core-caf")
        .arg(&vector)
        .arg("--output")
        .arg(&output_path)
        .output()
        .expect("应能启动 CLI");
    assert!(
        output.status.success(),
        "export-core-caf 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "严格直接映射不应产生 lossy warning"
    );
    let stdout = success("export-core-caf", &output.stdout);
    let result = &stdout["result"];
    assert_eq!(result["audio"]["format"], "caf_lpcm_f32le");
    assert_eq!(result["audio"]["channels"], 10);
    assert_eq!(result["audio"]["frames"], 288_000);
    assert_eq!(result["container"], "CAF");
    assert_eq!(result["layout"], "5.1.4");
    assert_eq!(result["channel_order"], "L R C LFE Ls Rs Vhl Vhr Ltr Rtr");
    assert_eq!(result["objects"].as_array().map(Vec::len), Some(9));
    assert_eq!(result["objects"][0]["ajoc_input"], 0);
    assert_eq!(result["objects"][0]["speaker"], "L");
    assert_eq!(result["objects"][0]["track_index"], 1);
    assert_eq!(result["tracks"][3]["speaker"], "LFE");
    assert_eq!(result["tracks"][3]["role"], "lfe");
    assert_eq!(result["tracks"][6]["ajoc_input"], 5);
    assert_eq!(result["tracks"][8]["ajoc_input"], 7);
    assert_eq!(
        result["scale"],
        "fixed_linear_gain=0.000030517578125;internal_±32768_to_pcm_f32le;normalization=none;limiter=none"
    );

    let source_output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-aspx-pcm")
        .arg(&vector)
        .arg("--output")
        .arg(&source_path)
        .output()
        .expect("应能启动 A-SPX PCM 对照导出");
    assert!(
        source_output.status.success(),
        "export-aspx-pcm 失败：{}",
        String::from_utf8_lossy(&source_output.stderr)
    );
    success("export-aspx-pcm", &source_output.stdout);

    let data = fs::read(&output_path).expect("core CAF 应存在");
    assert_eq!(&data[0..4], b"caff");
    assert_eq!(&data[8..12], b"desc");
    assert_eq!(&data[28..32], b"lpcm");
    assert_eq!(u32::from_be_bytes(data[32..36].try_into().unwrap()), 0x3);
    assert_eq!(u32::from_be_bytes(data[44..48].try_into().unwrap()), 10);
    assert_eq!(u32::from_be_bytes(data[48..52].try_into().unwrap()), 32);
    assert_eq!(&data[52..56], b"chan");
    assert_eq!(
        u32::from_be_bytes(data[64..68].try_into().unwrap()),
        (195u32 << 16) | 10
    );
    assert_eq!(&data[76..80], b"data");
    let data_size = i64::from_be_bytes(data[80..88].try_into().unwrap());
    assert_eq!(data_size, 4 + 288_000 * 10 * 4);
    assert_eq!(u32::from_be_bytes(data[88..92].try_into().unwrap()), 0);
    assert_eq!(data.len(), 92 + 288_000 * 10 * 4);

    let source_wave = fs::read(&source_path).expect("A-SPX PCM 对照应存在");
    let source_pcm = wave_pcm(&source_wave);
    let caf_pcm = &data[92..];
    assert_eq!(source_pcm.len(), caf_pcm.len());
    let source_for_track = [0usize, 1, 2, 9, 3, 4, 5, 6, 7, 8];
    let mut nonzero = [false; 10];
    for (frame, (source, actual)) in source_pcm
        .as_chunks::<{ 10 * 4 }>()
        .0
        .iter()
        .zip(caf_pcm.as_chunks::<{ 10 * 4 }>().0.iter())
        .enumerate()
    {
        for (track, source_channel) in source_for_track.iter().copied().enumerate() {
            let source_offset = source_channel * 4;
            let source_sample =
                f32::from_le_bytes(source[source_offset..source_offset + 4].try_into().unwrap());
            let actual_offset = track * 4;
            let actual_sample =
                f32::from_le_bytes(actual[actual_offset..actual_offset + 4].try_into().unwrap());
            let expected = source_sample * (1.0f32 / 32_768.0);
            assert_eq!(
                actual_sample.to_bits(),
                expected.to_bits(),
                "frame {frame} / track {} 未按 q/LFE 来源固定缩放",
                track + 1
            );
            nonzero[track] |= actual_sample != 0.0;
        }
    }
    for track in [0usize, 1, 2, 4, 5, 6, 7, 8, 9] {
        assert!(nonzero[track], "q 来源输出轨 {} 不应全静音", track + 1);
    }

    fs::remove_file(&output_path).expect("应能清理 core CAF 测试产物");
    fs::remove_file(&source_path).expect("应能清理 A-SPX PCM 对照产物");
}
