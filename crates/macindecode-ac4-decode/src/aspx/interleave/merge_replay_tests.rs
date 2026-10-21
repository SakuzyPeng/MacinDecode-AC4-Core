//! 固定同一分析输入与 SEC，观察合并策略；候选只在测试中选择输出。
//! 原始全带历史仍由生产 combine 推进，不能预先裁掉历史中的高带。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "本地重放先校验固定 96 时隙输入、有限值、身份与交叉子带"
)]
extern crate std;
use super::*;
use crate::aspx::qmf::{QmfSynthesisState, synthesise_ac4_pcm};
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::vec;
use std::vec::Vec;

fn load(path: &Path) -> Vec<QmfSlot> {
    let raw = std::fs::read(path).unwrap();
    assert_eq!(raw.len(), 96 * 128 * 4);
    raw.as_chunks::<512>()
        .0
        .iter()
        .map(|row| {
            let mut q = QmfSlot::zero();
            for (b, pair) in row.as_chunks::<8>().0.iter().enumerate() {
                q.re[b] = f32::from_le_bytes(pair[..4].try_into().unwrap());
                q.im[b] = f32::from_le_bytes(pair[4..].try_into().unwrap());
                assert!(q.re[b].is_finite() && q.im[b].is_finite());
            }
            q
        })
        .collect()
}

fn save(path: &Path, values: impl IntoIterator<Item = f32>) {
    let mut f = std::io::BufWriter::new(std::fs::File::create_new(path).unwrap());
    for v in values {
        assert!(v.is_finite());
        f.write_all(&v.to_le_bytes()).unwrap();
    }
    f.flush().unwrap();
}

fn name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 63 && value.bytes().all(|v| v.is_ascii_alphanumeric())
}

#[test]
#[ignore = "requires fixed local QMF and SEC fixtures; no reference library is loaded"]
fn replay_fixed_merge_inputs() {
    let data = std::env::var("MACINAC4_MERGE_DATA").unwrap();
    let output = std::env::var("MACINAC4_MERGE_OUTPUT").unwrap();
    let (data, output) = (Path::new(&data), Path::new(&output));
    std::fs::create_dir(output).unwrap();
    let text = std::fs::read_to_string(data.join("cases.txt")).unwrap();
    let mut ids = BTreeSet::new();
    for line in text.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        assert_eq!(parts.len(), 4);
        let (id, input, sec) = (parts[0], parts[1], parts[2]);
        assert!([id, input, sec].into_iter().all(name) && ids.insert(id));
        let crossover: usize = parts[3].parse().unwrap();
        assert!((4..64).contains(&crossover));
        let q = load(&data.join(format!("{input}.f32")));
        let y = load(&data.join(format!("{sec}.f32")));
        assert!(y.iter().all(|s| {
            s.re[..crossover]
                .iter()
                .chain(&s.im[..crossover])
                .all(|v| *v == 0.0)
        }));
        for mode in ["add", "select"] {
            let mut delay = AspxOutputDelay::new();
            let mut synth = QmfSynthesisState::new();
            let zero = [QmfSlot::zero(); 32];
            combine(&q[..32], &zero, 6, &mut delay, &mut [QmfSlot::zero(); 32]).unwrap();
            let mut merged = vec![QmfSlot::zero(); 96];
            let mut pcm = vec![0.0; 96 * 64];
            for frame in 0..3 {
                let input = if frame < 2 {
                    &q[(frame + 1) * 32..(frame + 2) * 32]
                } else {
                    &zero
                };
                let sec = &y[frame * 32..(frame + 1) * 32];
                let out = &mut merged[frame * 32..(frame + 1) * 32];
                combine(input, sec, 6, &mut delay, out).unwrap();
                if mode == "select" {
                    for (s, extension) in out.iter_mut().zip(sec) {
                        s.re[crossover..].copy_from_slice(&extension.re[crossover..]);
                        s.im[crossover..].copy_from_slice(&extension.im[crossover..]);
                    }
                }
                synthesise_ac4_pcm(out, &mut synth, &mut pcm[frame * 2048..(frame + 1) * 2048])
                    .unwrap();
            }
            assert!(
                merged[70..]
                    .iter()
                    .all(|s| s.re.iter().chain(&s.im).all(|v| *v == 0.0))
            );
            assert!(pcm[80 * 64..].iter().all(|v| *v == 0.0));
            save(
                &output.join(format!("{id}.{mode}.qmf.f32")),
                merged
                    .iter()
                    .flat_map(|s| (0..64).flat_map(move |b| [s.re[b], s.im[b]])),
            );
            save(&output.join(format!("{id}.{mode}.pcm.f32")), pcm);
        }
    }
    assert!(!ids.is_empty());
    std::fs::write(output.join("replay_metadata.json"), "{\"schema\":\"macinac4.merge-replay\",\"version\":1,\"delay\":6,\"synthesis_initial_state\":\"zero\",\"candidate\":\"select_sec_above_crossover_after_advancing_raw_history\"}\n").unwrap();
}
