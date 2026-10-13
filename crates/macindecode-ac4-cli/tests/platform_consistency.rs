//! 六平台使用同一组非零合成输入与冻结位摘要；没有本地媒体依赖。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "测试按明确的合成夹具容量和 PCM 布局断言"
)]

#[path = "common/consistency_fixture.rs"]
mod fixture;

use fixture::{CASES, Case, FRAME_SAMPLES, FRAMES};
use macindecode_ac4_decode::full_ajoc::{
    FullAjocAudioFrameInput, FullAjocDecodeMode, FullAjocDecoder, FullAjocFrameProvenance,
    FullAjocPcmSource, FullAjocSyntaxFrameInput,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_pcm(bytes: &[u8], channels: usize, frames: usize) -> bool {
    if channels == 0 || frames == 0 || bytes.len() != channels * frames * 4 {
        return false;
    }
    let mut nonzero = vec![false; channels];
    for (index, sample) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let value = f32::from_le_bytes(*sample);
        if !value.is_finite() {
            return false;
        }
        nonzero[index % channels] |= value != 0.0;
    }
    nonzero.into_iter().all(|value| value)
}

fn input(frame: &fixture::Frame, case: Case, index: usize) -> FullAjocAudioFrameInput<'_> {
    FullAjocAudioFrameInput {
        syntax: FullAjocSyntaxFrameInput {
            payload: &frame.payload,
            context: frame.context,
            substream_index: 1,
            physical_substreams: 1,
        },
        provenance: FullAjocFrameProvenance::new(index as u64)
            .with_source_sample_start((index * FRAME_SAMPLES) as i64)
            .with_presentation_sample_start((index * FRAME_SAMPLES) as i64),
        lfe_position: case.lfe.then_some(0),
        mode: FullAjocDecodeMode::RequireFull,
    }
}

fn decode(decoder: &mut FullAjocDecoder, case: Case) -> [Vec<u8>; 3] {
    let mut pcm = [Vec::new(), Vec::new(), Vec::new()];
    for index in 0..FRAMES {
        let frame = fixture::frame(case, index);
        let decoded = decoder
            .decode_complete_audio_frame(input(&frame, case, index))
            .unwrap_or_else(|error| panic!("{} frame {index}: {error}", case.name));
        let output = decoded.output();
        let asf = decoded.frontend().asf();
        let channels = 1 + usize::from(case.lfe);
        assert_eq!(asf.channels(), channels);
        assert_eq!(output.diagnostic_channels(), channels);
        assert_eq!(
            output.reconstructed_channels(),
            case.objects + usize::from(case.lfe)
        );
        for i in 0..channels {
            assert_eq!(
                output.diagnostic_channel(i).unwrap().source(),
                if i == 1 {
                    FullAjocPcmSource::Lfe
                } else {
                    FullAjocPcmSource::AjocInput
                }
            );
        }
        for i in 0..output.reconstructed_channels() {
            assert_eq!(
                output.reconstructed_channel(i).unwrap().source(),
                if case.lfe && i == 0 {
                    FullAjocPcmSource::Lfe
                } else {
                    FullAjocPcmSource::AjocObject(i - usize::from(case.lfe))
                }
            );
        }
        if index > 0 {
            assert!(!output.observation().warmup());
            assert!(output.observation().reconstructed());
            assert_eq!(output.observation().wet(), case.wet);
            let aligned = output.aligned_side_information().unwrap();
            assert_eq!(aligned.provenance().access_unit_index(), index as u64 - 1);
            assert_eq!(
                aligned.provenance().source_sample_start(),
                Some(((index - 1) * FRAME_SAMPLES) as i64)
            );
            assert_eq!(
                aligned.provenance().presentation_sample_start(),
                Some(((index - 1) * FRAME_SAMPLES) as i64)
            );
            assert_eq!(aligned.frame_length(), FRAME_SAMPLES as u16);
            assert_eq!(aligned.sampling_frequency_hz(), 48_000);
        }
        let stages = [
            (0..channels)
                .map(|i| asf.channel(i).unwrap().samples())
                .collect::<Vec<_>>(),
            (0..channels)
                .map(|i| output.diagnostic_channel(i).unwrap().samples())
                .collect(),
            (0..output.reconstructed_channels())
                .map(|i| output.reconstructed_channel(i).unwrap().samples())
                .collect(),
        ];
        for (stage, planes) in stages.iter().enumerate() {
            for samples in planes {
                assert_eq!(samples.len(), FRAME_SAMPLES);
            }
            for sample in 0..FRAME_SAMPLES {
                for plane in planes {
                    assert!(plane[sample].is_finite());
                    pcm[stage].extend_from_slice(&plane[sample].to_bits().to_le_bytes());
                }
            }
        }
    }
    for (stage, samples) in pcm.iter().enumerate() {
        let channels = if stage == 2 { case.objects } else { 1 } + usize::from(case.lfe);
        assert!(
            valid_pcm(samples, channels, FRAMES * FRAME_SAMPLES),
            "{} stage {stage} 不能有空、静音或非有限声道",
            case.name
        );
    }
    pcm
}

#[test]
fn synthetic_pcm_matches_frozen_bits() {
    let baseline = baseline();
    for case in CASES {
        let pcm = decode(&mut FullAjocDecoder::new(), case);
        assert_eq!(
            snapshot(case, &pcm),
            baseline["cases"][case.name],
            "{}",
            case.name
        );
    }
}

fn snapshot(case: Case, pcm: &[Vec<u8>; 3]) -> Value {
    json!({
        "input_sha256": hash(&fixture::sync_stream(case)),
        "sample_rate": 48000,
        "frames": FRAMES * FRAME_SAMPLES,
        "channels": [1 + usize::from(case.lfe), 1 + usize::from(case.lfe), case.objects + usize::from(case.lfe)],
        "pcm_sha256": pcm.iter().map(|p| hash(p)).collect::<Vec<_>>(),
    })
}

fn baseline() -> Value {
    serde_json::from_str(include_str!("fixtures/platform-consistency.json")).unwrap()
}

fn registry_matches(baseline: &Value) -> bool {
    let Some(cases) = baseline["cases"].as_object() else {
        return false;
    };
    baseline["version"] == 1
        && baseline["stages"] == json!(["core", "aspx", "objects"])
        && cases.len() == CASES.len()
        && !cases.is_empty()
        && CASES.iter().all(|case| cases.contains_key(case.name))
}

#[test]
fn fixture_registry_is_complete() {
    let expected = baseline();
    assert!(registry_matches(&expected));
    let mut missing = expected.clone();
    missing["cases"]
        .as_object_mut()
        .unwrap()
        .remove(CASES[0].name);
    assert!(!registry_matches(&missing));
    missing["cases"] = json!({});
    assert!(!registry_matches(&missing));
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "macinac4-consistency-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wave_data(wave: &[u8], channels: usize) -> &[u8] {
    assert_eq!(&wave[..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(wave[4..8].try_into().unwrap()) as usize + 8,
        wave.len()
    );
    assert_eq!(&wave[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes(wave[20..22].try_into().unwrap()), 0xfffe);
    assert_eq!(
        u16::from_le_bytes(wave[22..24].try_into().unwrap()) as usize,
        channels
    );
    assert_eq!(u32::from_le_bytes(wave[24..28].try_into().unwrap()), 48000);
    assert_eq!(u16::from_le_bytes(wave[34..36].try_into().unwrap()), 32);
    let mut offset = 12;
    while offset + 8 <= wave.len() {
        let size = u32::from_le_bytes(wave[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let end = offset + 8 + size;
        assert!(end <= wave.len());
        if &wave[offset..offset + 4] == b"data" {
            return &wave[offset + 8..end];
        }
        offset = end + (size & 1);
    }
    panic!("没有 PCM data chunk");
}

#[test]
fn cli_pcm_matches_engine_and_tracks() {
    let scratch = Scratch::new();
    for case in CASES {
        let engine = decode(&mut FullAjocDecoder::new(), case);
        let source = scratch.0.join(format!("{}.ac4", case.name));
        fs::write(&source, fixture::sync_stream(case)).unwrap();
        for (stage, name) in ["core", "aspx", "objects"].iter().enumerate() {
            let destination = scratch.0.join(format!("{}-{name}.wav", case.name));
            let command = format!("export-{name}-pcm");
            let result = Command::new(env!("CARGO_BIN_EXE_macinac4"))
                .args([
                    &command,
                    source.to_str().unwrap(),
                    "--output",
                    destination.to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{command} {}: {}",
                case.name,
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(
                result.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let report: Value = serde_json::from_slice(&result.stdout).unwrap();
            let report = &report["result"];
            let channels = if stage == 2 { case.objects } else { 1 } + usize::from(case.lfe);
            assert_eq!(report["audio"]["channels"], channels);
            assert_eq!(report["audio"]["frames"], FRAMES * FRAME_SAMPLES);
            assert_eq!(report["audio"]["sample_rate_hz"], 48000);
            let tracks = if stage == 0 {
                (0..channels)
                    .map(|element| json!({"substream": 1, "element": element, "channel": 0}))
                    .collect::<Vec<_>>()
            } else if stage == 1 {
                let mut tracks =
                    vec![json!({"substream": 1, "role": "ajoc_input", "ajoc_input": 0})];
                if case.lfe {
                    tracks.push(json!({"substream": 1, "role": "lfe"}));
                }
                tracks
            } else {
                (0..channels).map(|i| if case.lfe && i == 0 {
                    json!({"substream": 1, "role": "lfe", "output_channel": 0})
                } else {
                    json!({"substream": 1, "role": "ajoc_object", "ajoc_object": i - usize::from(case.lfe), "output_channel": i})
                }).collect()
            };
            assert_eq!(report["tracks"], json!(tracks));
            let wave = fs::read(destination).unwrap();
            let actual = wave_data(&wave, channels);
            assert!(valid_pcm(actual, channels, FRAMES * FRAME_SAMPLES));
            assert_eq!(
                hash(actual),
                hash(&engine[stage]),
                "{command} {}",
                case.name
            );
        }
    }
}

#[test]
fn history_is_exercised_and_reset_is_exact() {
    let mut decoder = FullAjocDecoder::new();
    let dry = decode(&mut decoder, CASES[0]);
    decoder.reset();
    assert_eq!(decode(&mut decoder, CASES[0]), dry);
    decoder.reset();
    let wet = decode(&mut decoder, CASES[1]);
    assert_eq!(dry[0], wet[0]);
    assert_eq!(dry[1], wet[1]);
    assert_ne!(dry[2], wet[2], "wet 路径必须改变非零对象 PCM");
    let no_prediction = decode(&mut FullAjocDecoder::new(), Case { tna: 0, ..CASES[0] });
    assert_eq!(dry[0], no_prediction[0]);
    assert_ne!(dry[1], no_prediction[1], "TNA 预测必须影响 A-SPX PCM");
    let different_noise = decode(
        &mut FullAjocDecoder::new(),
        Case {
            noise: 0,
            ..CASES[0]
        },
    );
    assert_eq!(dry[0], different_noise[0]);
    assert_ne!(dry[1], different_noise[1], "噪声包络必须影响 A-SPX PCM");
    // 完全相同的 ASF 频谱：这里的差异必须来自 overlap，而不是不同的输入。
    let mut decoder = FullAjocDecoder::new();
    let mut repeated = Vec::new();
    for index in 0..2 {
        let frame = fixture::frame_with_spectrum(CASES[0], index, 0);
        let decoded = decoder
            .decode_complete_audio_frame(input(&frame, CASES[0], index))
            .unwrap();
        repeated.push(
            decoded
                .frontend()
                .asf()
                .channel(0)
                .unwrap()
                .samples()
                .iter()
                .map(|s| s.to_bits())
                .collect::<Vec<_>>(),
        );
    }
    assert_ne!(repeated[0], repeated[1], "相同频谱的第二帧必须继承 overlap");
}

#[test]
fn fingerprints_reject_pcm_bit_order_and_length_changes() {
    let case = CASES[2];
    let pcm = decode(&mut FullAjocDecoder::new(), case);
    let expected = baseline()["cases"][case.name].clone();
    assert_eq!(snapshot(case, &pcm), expected);
    let mut changed = pcm.clone();
    changed[0][FRAME_SAMPLES * 8] ^= 1;
    assert_ne!(snapshot(case, &changed), expected);
    let mut changed = pcm.clone();
    for samples in changed[0].as_chunks_mut::<8>().0 {
        let (left, right) = samples.split_at_mut(4);
        left.swap_with_slice(right);
    }
    assert_ne!(snapshot(case, &changed), expected);
    let mut changed = pcm;
    changed[0][..4].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(!valid_pcm(&changed[0], 2, FRAMES * FRAME_SAMPLES));
    changed[2].truncate(changed[2].len() - 12);
    assert!(!valid_pcm(&changed[2], 3, FRAMES * FRAME_SAMPLES));
    assert_ne!(snapshot(case, &changed), expected);
    assert!(!valid_pcm(&[], 0, 0));
    assert!(!valid_pcm(&vec![0; FRAME_SAMPLES * 4], 1, FRAME_SAMPLES));
}

#[test]
fn cli_rejects_missing_empty_and_truncated_inputs() {
    let scratch = Scratch::new();
    let missing = scratch.0.join("missing.ac4");
    let empty = scratch.0.join("empty.ac4");
    let truncated = scratch.0.join("truncated.ac4");
    fs::write(&empty, []).unwrap();
    let stream = fixture::sync_stream(CASES[1]);
    fs::write(&truncated, &stream[..stream.len() - 3]).unwrap();
    for source in [missing, empty, truncated] {
        let destination = scratch.0.join("should-not-exist.wav");
        let result = Command::new(env!("CARGO_BIN_EXE_macinac4"))
            .arg("export-objects-pcm")
            .arg(source)
            .arg("--output")
            .arg(&destination)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert!(!destination.exists());
    }
}

#[test]
fn truncated_frame_invalidates_history_and_iframe_recovers() {
    let case = CASES[1];
    let first = fixture::frame(case, 0);
    let dependent = fixture::frame(case, 1);
    let mut decoder = FullAjocDecoder::new();
    decoder
        .decode_complete_audio_frame(input(&first, case, 0))
        .unwrap();
    let truncated = fixture::Frame {
        payload: dependent.payload[..3].to_vec(),
        ..fixture::frame(case, 1)
    };
    assert!(
        decoder
            .decode_complete_audio_frame(input(&truncated, case, 1))
            .is_err()
    );
    assert!(
        decoder
            .decode_complete_audio_frame(input(&dependent, case, 2))
            .is_err()
    );
    assert_eq!(
        decode(&mut decoder, case),
        decode(&mut FullAjocDecoder::new(), case)
    );
}
