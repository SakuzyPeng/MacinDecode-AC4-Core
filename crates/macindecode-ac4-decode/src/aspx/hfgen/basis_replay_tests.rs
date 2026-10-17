//! 同输入 HF 搬运与 QMF 合成的基底对照。只在测试中应用已固定的 R(k)，
//! 不修改生产分析／合成、音调初相或 patch 系数，不拟合相位和增益。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "离线测试入口先验证矩阵形状、频带、区间及固定格式，再使用有界下标"
)]
extern crate std;
use super::*;
use crate::aspx::bands::AspxBandTables;
use crate::aspx::qmf::{QmfSynthesisState, synthesise};
use crate::aspx::tna::TnaDelay;
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

enum Case {
    Hf {
        id: String,
        bands: AspxBandTables,
        patches: PatchTable,
        first: usize,
        last: usize,
    },
    Tone {
        id: String,
        slots: usize,
    },
}
impl Case {
    fn id(&self) -> &str {
        match self {
            Self::Hf { id, .. } | Self::Tone { id, .. } => id,
        }
    }
}
fn parse(line: &str) -> Result<Case, String> {
    let p: Vec<_> = line.split_whitespace().collect();
    if p.len() < 3 || p[1].is_empty() || !p[1].bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("invalid identity".to_string());
    }
    let n = |i: usize| -> Result<usize, String> {
        p.get(i)
            .ok_or_else(|| "truncated record".to_string())?
            .parse()
            .map_err(|_| "invalid integer".to_string())
    };
    let byte = |i: usize| -> Result<u8, String> {
        u8::try_from(n(i)?).map_err(|_| "out of range".to_string())
    };
    let id = p[1].to_string();
    match p[0] {
        "H" => {
            let master = match n(2)? {
                0 => false,
                1 => true,
                _ => return Err("master flag".to_string()),
            };
            // 255 表示检查点没有直接提供 xover；只用捕获的交叉子带恢复，
            // 主表严格递增，要求唯一解，并继续逐项核对完整 patch 表。
            let start = byte(3)?;
            let stop = byte(4)?;
            let noise = byte(5)?;
            let xover = byte(6)?;
            let crossover = byte(10)?;
            let candidates: Vec<_> = (0..=7)
                .filter(|&x| xover == 255 || x == xover)
                .filter_map(|x| AspxBandTables::derive(master, start, stop, noise, x).ok())
                .filter(|b| b.sbx() == crossover)
                .collect();
            if candidates.len() != 1 {
                return Err("crossover not uniquely determined".to_string());
            }
            let bands = candidates.into_iter().next().expect("unique crossover");
            // 此实验的输入与完整解码状态均已核对为 48 kHz。
            let patches = PatchTable::derive(&bands, master, true)
                .map_err(|e| format!("patch derivation: {e:?}"))?;
            let first = n(7)?;
            let last = n(8)?;
            let count = n(9)?;
            if first >= last
                || last > 38
                || count == 0
                || count != usize::from(patches.count())
                || p.len() != 11 + 2 * count
            {
                return Err("geometry or field count".to_string());
            }
            for j in 0..=count {
                if patches.border(j) != Some(byte(10 + j)?) {
                    return Err(format!(
                        "target table differs for {id}, patch {j}: local {:?}, captured {}",
                        patches.border(j),
                        byte(10 + j)?
                    ));
                }
            }
            for j in 0..count {
                if patches.start_sb(j) != Some(byte(11 + count + j)?) {
                    return Err("source table differs".to_string());
                }
            }
            Ok(Case::Hf {
                id,
                bands,
                patches,
                first,
                last,
            })
        }
        "S" => {
            let band = n(2)?;
            let seed = n(3)?;
            let first = n(4)?;
            let last = n(5)?;
            let slots = n(6)?;
            if p.len() != 7
                || band >= 64
                || seed >= 4
                || first >= last
                || last > slots
                || !(10..=1024).contains(&slots)
            {
                return Err("tone geometry".to_string());
            }
            Ok(Case::Tone { id, slots })
        }
        _ => Err("unknown record kind".to_string()),
    }
}
fn read_slots(path: &Path, count: usize) -> Vec<QmfSlot> {
    let raw = std::fs::read(path).expect("read input matrix");
    assert_eq!(raw.len(), count * 128 * 4);
    let mut slots = vec![QmfSlot::zero(); count];
    for (row, bytes) in slots.iter_mut().zip(raw.as_chunks::<512>().0) {
        for (b, pair) in bytes.as_chunks::<8>().0.iter().enumerate() {
            row.re[b] = f32::from_le_bytes(pair[..4].try_into().expect("real"));
            row.im[b] = f32::from_le_bytes(pair[4..].try_into().expect("imaginary"));
            assert!(row.re[b].is_finite() && row.im[b].is_finite());
        }
    }
    slots
}
fn save_slots(path: &Path, slots: &[QmfSlot]) {
    let mut f = std::fs::File::create(path).expect("new matrix");
    for slot in slots {
        for b in 0..64 {
            for x in [slot.re[b], slot.im[b]] {
                assert!(x.is_finite());
                f.write_all(&x.to_le_bytes()).expect("write matrix");
            }
        }
    }
}
fn multiply(re: f32, im: f32, cr: f64, ci: f64) -> (f32, f32) {
    (
        (f64::from(re) * cr - f64::from(im) * ci) as f32,
        (f64::from(re) * ci + f64::from(im) * cr) as f32,
    )
}
fn inverse_rotation(slot: &mut QmfSlot) {
    for b in 0..64 {
        let angle = std::f64::consts::PI * (b as f64 + 0.5) / 128.0;
        let (sin, cos) = angle.sin_cos();
        (slot.re[b], slot.im[b]) = multiply(slot.re[b], slot.im[b], -cos, sin);
    }
}

#[test]
#[ignore = "需要显式的本地 HF／QMF 基底输入与新输出目录"]
fn replay_basis_checkpoints() {
    let input = std::env::var_os("MACINAC4_BASIS_REPLAY_INPUT").expect("manifest");
    let directory = std::env::var_os("MACINAC4_BASIS_REPLAY_OUTPUT").expect("output");
    let path = Path::new(&input);
    let parent = path.parent().expect("input directory");
    let cases: Vec<_> = std::fs::read_to_string(path)
        .expect("manifest text")
        .lines()
        .map(|s| parse(s).unwrap_or_else(|e| panic!("{e}")))
        .collect();
    assert!(!cases.is_empty() && cases.len() <= 4096);
    let mut ids = BTreeSet::new();
    for c in &cases {
        assert!(ids.insert(c.id()));
    }
    let root = Path::new(&directory);
    std::fs::create_dir(root).expect("fresh output with existing parent");
    let mut log = std::fs::File::create(root.join("results.txt")).expect("result log");
    for case in cases {
        match case {
            Case::Hf {
                id,
                bands,
                patches,
                first,
                last,
            } => {
                let mut filters = TnaFilters::new();
                filters.fill_for_test(64, Coefficient::ZERO, Coefficient::ZERO);
                let delay = TnaDelay::new();
                let borders = [bands.sbx(), bands.sbz()];
                for (mode, source) in [
                    ("native", "native"),
                    ("input_basis", "converted"),
                    ("operator_basis", "converted"),
                ] {
                    let q = read_slots(&parent.join(format!("{id}.{source}.f32")), 40);
                    let mut output = vec![QmfSlot::zero(); last - first];
                    hf_generate(
                        ExtendedLowBand::new(&delay, &q),
                        &patches,
                        &filters,
                        &[0.0],
                        &borders,
                        None,
                        first,
                        last,
                        &mut output,
                    )
                    .expect("HF generation");
                    if mode == "operator_basis" {
                        for i in 0..usize::from(patches.count()) {
                            let src = usize::from(patches.start_sb(i).expect("source"));
                            let dst = usize::from(patches.border(i).expect("target"));
                            let angle = std::f64::consts::PI * (src as f64 - dst as f64) / 128.0;
                            let (sin, cos) = angle.sin_cos();
                            for slot in &mut output {
                                for j in 0..usize::from(patches.num_sb(i).expect("length")) {
                                    let b = dst + j;
                                    (slot.re[b], slot.im[b]) =
                                        multiply(slot.re[b], slot.im[b], cos, sin);
                                }
                            }
                        }
                    }
                    save_slots(&root.join(format!("{id}.{mode}.f32")), &output);
                    writeln!(log, "H {id} {mode} {} {} {}", first, last, bands.sbx())
                        .expect("record");
                }
            }
            Case::Tone { id, slots } => {
                let input = read_slots(&parent.join(format!("{id}.native.f32")), slots);
                for (mode, converted, advance) in [
                    ("native", false, false),
                    ("basis", true, false),
                    ("advance", false, true),
                    ("basis_advance", true, true),
                ] {
                    let mut q = input.clone();
                    for slot in &mut q {
                        if advance {
                            for b in 0..64 {
                                let sign = if b % 2 == 0 { 1.0 } else { -1.0 };
                                (slot.re[b], slot.im[b]) = (-sign * slot.im[b], sign * slot.re[b]);
                            }
                        }
                        if converted {
                            inverse_rotation(slot);
                        }
                    }
                    let mut pcm = vec![0.0; slots * 64];
                    synthesise(&q, &mut QmfSynthesisState::new(), &mut pcm).expect("synthesis");
                    let mut f = std::fs::File::create(root.join(format!("{id}.{mode}.f32")))
                        .expect("PCM output");
                    for x in pcm {
                        assert!(x.is_finite());
                        f.write_all(&x.to_le_bytes()).expect("PCM write");
                    }
                    writeln!(log, "S {id} {mode} {slots}").expect("record");
                }
            }
        }
    }
}

#[test]
fn rejects_misaligned_basis_replay_geometry_and_patch_sources() {
    for text in [
        "",
        "S ../x 36 0 3 32 48",
        "S x 64 0 3 32 48",
        "S x 36 4 3 32 48",
        "S x 36 0 32 32 48",
        "H x 1 6 0 3 0 0 32 2 36 44 62 28 19",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
    assert!(parse("H x 1 6 0 3 0 0 32 2 36 44 62 28 18").is_ok());
}
