//! 带绝对 PCM 样本位置的分析、低带延迟、dry 矩阵及合成连续链。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "自有测试夹具的帧、子带和数据点均为固定有界值"
)]
extern crate std;
use super::*;
use crate::aspx::{
    lowband::{LowBandDelay, low_band},
    qmf::{QMF_WINDOW, QmfAnalysisState, QmfSynthesisState, analyse_ac4_pcm, synthesise_ac4_pcm},
};
use std::{
    format,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    vec::Vec,
};

fn save(path: &Path, values: impl IntoIterator<Item = f32>) {
    let mut f = BufWriter::new(File::create_new(path).unwrap());
    for v in values {
        f.write_all(&v.to_le_bytes()).unwrap();
    }
    f.flush().unwrap();
}

#[test]
#[ignore = "exports absolute-sample timing probes into an explicit new directory"]
fn replay_ajoc_timing() {
    std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(|| {
        let root = PathBuf::from(std::env::var("MACINAC4_AJOC_TIMING_OUTPUT").unwrap());
        std::fs::create_dir(&root).unwrap();
        save(&root.join("window.f32"), QMF_WINDOW);
        for slots in [24usize, 30, 32] {
            let n = slots * 64;
            let delay = match slots {24 => 96, 30 => 288, _ => 352};
            for start in [0, slots / 2, slots - 1] {
                for ramp in [1u8, 2, 31, 32, 33, 64] {
                    for pattern in ["impulse", "step"] {
                        let dir = root.join(format!("s{slots}p{start}r{ramp}{pattern}"));
                        std::fs::create_dir(&dir).unwrap();
                        std::fs::write(dir.join("case.json"), format!(
                            "{{\"slots\":{slots},\"start\":{start},\"ramp\":{ramp},\"pattern\":\"{pattern}\",\"pcm_delay\":{delay},\"frames\":5}}\n"
                        )).unwrap();
                        let mut pcm = vec![0.0f32; 5 * n];
                        if pattern == "impulse" {
                            for i in [n / 2, n + 63, 2 * n - 1, 3 * n + 32] { pcm[i] = 1.0; }
                        } else { pcm[n + 3..].fill(0.25); }
                        save(&dir.join("pcm_input.f32"), pcm.iter().copied());
                        let mut delayed = vec![0.0; pcm.len()];
                        delayed[delay..].copy_from_slice(&pcm[..pcm.len() - delay]);
                        let mut analysis = QmfAnalysisState::new();
                        let mut synthesis = QmfSynthesisState::new();
                        let mut low = LowBandDelay::new();
                        let mut state = AjocReconstructionState::new();
                        let mut workspace = AjocWorkspace::new();
                        let mut pcm_out = Vec::new();
                        for frame in 0..5 {
                            let points = match frame {
                                0 => vec![(0, 1)],
                                1 => vec![(start as u8, ramp)],
                                3 => vec![(0, 64), (slots as u8 - 1, 2)],
                                _ => Vec::new(),
                            };
                            let mut ajoc = Ajoc::for_test(0, points.len() as u8, 1, 1);
                            ajoc.data_points = super::super::syntax::AjocDataPoints::with_points_for_test(&points);
                            let controls = [AjocObjectControl::for_test(true, 1, false, false, &[true], &[])];
                            let mut raw = AjocObjectMatrix::for_test(1, points.len() as u8, 1, 0);
                            for p in 0..points.len() {
                                raw.set_dry_for_test(p, 0, &[match frame {0 => 60, 1 => 70, _ if p == 0 => 40, _ => 55}], false);
                            }
                            let mut q = vec![QmfSlot::zero(); slots];
                            analyse_ac4_pcm(&delayed[frame * n..(frame + 1) * n], &mut analysis, &mut q).unwrap();
                            let mut low_out = vec![QmfSlot::zero(); slots + 6];
                            low_band(&q, &mut low, 64, 6, &mut low_out).unwrap();
                            let mut input = [empty_channel_frame()];
                            input[0][..slots].copy_from_slice(&low_out[..slots]);
                            let mut output = [empty_channel_frame()];
                            matrix_probe::select(dir.clone(), frame + 1, true);
                            reconstruct_frame(&ajoc, &controls, &[raw], slots as u8,
                                &input, &mut state, &mut workspace, &mut output).unwrap();
                            let mut samples = vec![0.0; n];
                            synthesise_ac4_pcm(&output[0][..slots], &mut synthesis, &mut samples).unwrap();
                            pcm_out.extend(samples);
                        }
                        save(&dir.join("pcm_output.f32"), pcm_out);
                    }
                }
            }
        }
        matrix_probe::clear();
    }).unwrap().join().unwrap();
}
