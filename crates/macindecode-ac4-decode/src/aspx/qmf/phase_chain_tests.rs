//! 分析—搬运—合成的独立相位调查入口，生产默认算法不变。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "测试输入先校验固定几何与有限值，再进行有界索引"
)]
extern crate std;
use super::*;
use crate::aspx::bands::AspxBandTables;
use crate::aspx::hfgen::hf_generate;
use crate::aspx::lowband::{LowBandDelay, low_band};
use crate::aspx::patches::PatchTable;
use crate::aspx::tna::{Coefficient, ExtendedLowBand, TnaDelay, TnaFilters};
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

struct Case {
    id: String,
    slots: usize,
    checkpoint: usize,
    delay: u8,
    patches: Option<PatchTable>,
}

fn parse(line: &str) -> Result<Case, String> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() < 5
        || fields[0].is_empty()
        || fields[0].len() > 63
        || !fields[0].bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err("invalid identity or field count".to_string());
    }
    let num = |i: usize| {
        fields
            .get(i)
            .ok_or("truncated case")?
            .parse::<usize>()
            .map_err(|_| "invalid integer")
    };
    let byte = |i: usize| u8::try_from(num(i)?).map_err(|_| "invalid byte");
    let flag = |i| match num(i)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("invalid flag"),
    };
    let slots = num(2)?;
    let checkpoint = num(3)?;
    let delay = byte(4)?;
    if !(32..=512).contains(&slots)
        || checkpoint == 0
        || checkpoint < usize::from(delay)
        || checkpoint > slots.saturating_sub(26)
        || !matches!(delay, 0 | 6)
    {
        return Err("invalid timeline".to_string());
    }
    let patches = match fields[1] {
        "full" if fields.len() == 5 => None,
        "patch" if fields.len() == 10 => {
            let master = flag(5)?;
            let bands = AspxBandTables::derive(master, byte(6)?, byte(7)?, byte(8)?, byte(9)?)
                .map_err(|e| format!("bands: {e:?}"))?;
            Some(PatchTable::derive(&bands, master, true).map_err(|e| format!("patch: {e:?}"))?)
        }
        // 单带原语控制也覆盖奇数差；不把它称为合法编码 patch。
        "route" if fields.len() == 7 => {
            let source = byte(5)?;
            let target = byte(6)?;
            if source >= 64 || target >= 64 {
                return Err("invalid route".to_string());
            }
            Some(PatchTable::from_parts(&[1], &[source], target))
        }
        _ => return Err("unknown case layout".to_string()),
    };
    Ok(Case {
        id: fields[0].to_string(),
        slots,
        checkpoint,
        delay,
        patches,
    })
}

fn mapped(case: &Case, q: &[QmfSlot], state: &mut LowBandDelay) -> Vec<QmfSlot> {
    let mut low = vec![QmfSlot::zero(); q.len() + usize::from(case.delay)];
    if case.delay == 0 {
        low.copy_from_slice(q);
    } else {
        low_band(q, state, 64, case.delay, &mut low).unwrap();
    }
    let Some(patches) = &case.patches else {
        return low[..q.len()].to_vec();
    };
    let mut filters = TnaFilters::new();
    filters.fill_for_test(64, Coefficient::ZERO, Coefficient::ZERO);
    let delay = TnaDelay::new();
    let mut result = vec![QmfSlot::zero(); q.len()];
    hf_generate(
        ExtendedLowBand::new(&delay, &low),
        patches,
        &filters,
        &[0.0],
        &[
            patches.border(0).unwrap(),
            patches.border(usize::from(patches.count())).unwrap(),
        ],
        None,
        0,
        q.len(),
        &mut result,
    )
    .unwrap();
    result
}

fn write_floats(path: &Path, values: impl IntoIterator<Item = f32>) {
    let mut f = std::fs::File::create_new(path).unwrap();
    for v in values {
        assert!(v.is_finite());
        f.write_all(&v.to_le_bytes()).unwrap();
    }
}

fn save_slots(path: &Path, q: &[QmfSlot]) {
    write_floats(
        path,
        q.iter()
            .flat_map(|s| (0..64).flat_map(move |b| [s.re[b], s.im[b]])),
    );
}

#[test]
#[ignore = "requires explicit local phase-chain manifest and fresh output directory"]
fn replay_qmf_phase_chain() {
    let input = std::env::var("MACINAC4_QMF_CHAIN_INPUT").unwrap();
    let output = std::env::var("MACINAC4_QMF_CHAIN_OUTPUT").unwrap();
    let input = Path::new(&input);
    let root = Path::new(&output);
    std::fs::create_dir(root).unwrap();
    write_floats(&root.join("qwin.f32"), QMF_WINDOW);
    let mut ids = BTreeSet::new();
    let mut log = std::fs::File::create_new(root.join("results.txt")).unwrap();
    for line in std::fs::read_to_string(input).unwrap().lines() {
        let c = parse(line).unwrap();
        assert!(ids.insert(c.id.clone()));
        let bytes =
            std::fs::read(input.parent().unwrap().join(format!("{}.input.f32", c.id))).unwrap();
        assert_eq!(bytes.len(), c.slots * 64 * 4);
        let pcm: Vec<_> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert!(pcm.iter().all(|v| v.is_finite()));
        assert!(
            pcm[c.checkpoint * 64..].iter().all(|v| *v == 0.0),
            "完整滤波尾部须由明确零输入驱动"
        );
        let mut a = QmfAnalysisState::new();
        let mut s = QmfSynthesisState::new();
        let mut l = LowBandDelay::new();
        let mut q = vec![QmfSlot::zero(); c.slots];
        let mut h = Vec::new();
        let mut y = vec![0.0; c.slots * 64];
        for (first, last) in [(0, c.checkpoint), (c.checkpoint, c.slots)] {
            analyse(&pcm[first * 64..last * 64], &mut a, &mut q[first..last]).unwrap();
            let part = mapped(&c, &q[first..last], &mut l);
            synthesise(&part, &mut s, &mut y[first * 64..last * 64]).unwrap();
            h.extend_from_slice(&part);
            if first == 0 {
                write_floats(&root.join(format!("{}.analysis_state.f32", c.id)), a.filt);
                write_floats(&root.join(format!("{}.synthesis_state.f32", c.id)), s.filt);
            }
        }
        assert!(a.filt.iter().chain(&s.filt).all(|x| *x == 0.0));
        // 不同调用分段必须保留同一物理时间轴与历史。
        let mut aa = QmfAnalysisState::new();
        let mut ss = QmfSynthesisState::new();
        let mut ll = LowBandDelay::new();
        let mut qq = vec![QmfSlot::zero(); c.slots];
        let mut yy = vec![0.0; c.slots * 64];
        let mut start = 0;
        let mut chunk = 0;
        while start < c.slots {
            let mut end = (start + [7, 11, 16, 32][chunk % 4]).min(c.slots);
            if c.slots - end < usize::from(c.delay) {
                end = c.slots;
            }
            analyse(&pcm[start * 64..end * 64], &mut aa, &mut qq[start..end]).unwrap();
            let part = mapped(&c, &qq[start..end], &mut ll);
            synthesise(&part, &mut ss, &mut yy[start * 64..end * 64]).unwrap();
            start = end;
            chunk += 1;
        }
        assert_eq!(a, aa);
        assert_eq!(s, ss);
        assert_eq!(l, ll);
        assert!(y.iter().zip(&yy).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert_eq!(q, qq);
        save_slots(&root.join(format!("{}.analysis.f32", c.id)), &q);
        save_slots(&root.join(format!("{}.mapped.f32", c.id)), &h);
        write_floats(&root.join(format!("{}.pcm.f32", c.id)), y);
        let routes: Vec<_> = if let Some(p) = c.patches {
            (0..usize::from(p.count()))
                .flat_map(|i| {
                    let src = p.start_sb(i).unwrap();
                    let dst = p.border(i).unwrap();
                    (0..p.num_sb(i).unwrap()).map(move |j| (src + j, dst + j))
                })
                .collect()
        } else {
            (0..64).map(|b| (b, b)).collect()
        };
        write!(
            log,
            "{} {} {} {} {}",
            c.id,
            c.slots,
            c.checkpoint,
            c.delay,
            routes.len()
        )
        .unwrap();
        for (p, k) in routes {
            write!(log, " {p} {k}").unwrap();
        }
        writeln!(log).unwrap();
    }
    assert!(!ids.is_empty());
}

#[test]
fn phase_manifest_rejects_wrong_geometry_or_unbounded_routes() {
    assert!(parse("a route 64 32 6 28 36").is_ok());
    assert!(parse("a patch 64 32 6 1 6 1 3 0").is_ok());
    for s in [
        "",
        "../a route 64 32 6 28 36",
        "a route 64 63 0 28 36",
        "a full 64 32 3",
        "a full 40 5 6",
        "a route 64 32 0 64 36",
        "a patch 64 32 0 2 6 1 3 0",
    ] {
        assert!(parse(s).is_err(), "{s}");
    }
    assert!(parse(&format!("a full 64 {} 0", usize::MAX)).is_err());
}

#[test]
fn every_derived_patch_preserves_source_target_parity() {
    let mut valid = 0;
    let mut pairs = 0;
    for master in [false, true] {
        for start in 0..8 {
            for stop in 0..4 {
                for noise in 0..4 {
                    for xover in 0..8 {
                        let Ok(b) = AspxBandTables::derive(master, start, stop, noise, xover)
                        else {
                            continue;
                        };
                        for fs48 in [false, true] {
                            let p = PatchTable::derive(&b, master, fs48).unwrap();
                            valid += 1;
                            for i in 0..usize::from(p.count()) {
                                let source = p.start_sb(i).unwrap();
                                let target = p.border(i).unwrap();
                                assert_eq!(
                                    source % 2,
                                    target % 2,
                                    "master={master},start={start},stop={stop},noise={noise},xover={xover},fs48={fs48}"
                                );
                                pairs += usize::from(p.num_sb(i).unwrap());
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!((valid, pairs), (3606, 66930));
    std::println!("phase parity census: {valid} layouts, {pairs} subband routes");
}

type ComplexSlot = [[f64; 2]; 64];

/// 消去 P65/P66 的折叠、环形状态与查表后得到的绝对时间 FIR。
/// QWIN 是共享的锁定输入，本测试不重新审计窗表；调制值不读取生产相位表。
fn fir_kernel(origin: f64, scale: f64) -> Vec<ComplexSlot> {
    (0..640)
        .map(|j| {
            let prototype = f64::from(QMF_WINDOW[j]) * if (j / 128) % 2 == 0 { 1.0 } else { -1.0 };
            core::array::from_fn(|band| {
                let omega = core::f64::consts::PI * (band as f64 + 0.5) / 64.0;
                let (sin, cos) = (omega * (j as f64 - origin)).sin_cos();
                [prototype * cos * scale, prototype * sin * scale]
            })
        })
        .collect()
}

fn analyse_fir(pcm: &[f32], kernel: &[ComplexSlot]) -> Vec<ComplexSlot> {
    (0..pcm.len() / 64)
        .map(|t| {
            let newest = 64 * t + 63;
            let mut out = [[0.0; 2]; 64];
            for (j, tap) in kernel.iter().enumerate().take((newest + 1).min(640)) {
                let value = f64::from(pcm[newest - j]);
                for (sum, coefficient) in out.iter_mut().zip(tap) {
                    sum[0] += value * coefficient[0];
                    sum[1] += value * coefficient[1];
                }
            }
            out
        })
        .collect()
}

fn synthesise_fir(qmf: &[ComplexSlot], kernel: &[ComplexSlot]) -> Vec<f64> {
    (0..qmf.len() * 64)
        .map(|n| {
            let mut sum = 0.0;
            for j in (n % 64..640.min(n + 1)).step_by(64) {
                for (q, coefficient) in qmf[(n - j) / 64].iter().zip(&kernel[j]) {
                    sum += q[0] * coefficient[0] - q[1] * coefficient[1];
                }
            }
            sum
        })
        .collect()
}

fn map_fir(qmf: &[ComplexSlot], routes: &[(usize, usize)], delay: usize) -> Vec<ComplexSlot> {
    let mut out = vec![[[0.0; 2]; 64]; qmf.len()];
    for (target, source) in out[delay..].iter_mut().zip(qmf) {
        for &(p, k) in routes {
            target[k] = source[p];
        }
    }
    out
}

fn require_forward_bound(expected: f64, actual: f64, bound: f64, stage: &str) {
    assert!(expected.is_finite() && actual.is_finite());
    assert!(
        (actual - expected).abs() <= bound,
        "{stage}: expected={expected}, actual={actual}, bound={bound}"
    );
}

#[test]
fn independent_fir_predicts_analysis_patch_and_synthesis() {
    let analysis_kernel = fir_kernel(0.5, 1.0);
    let synthesis_kernel = fir_kernel(127.5, 1.0 / 64.0);
    let window_sum: f64 = QMF_WINDOW.iter().map(|v| f64::from(v.abs())).sum();
    let polyphase_sum = (0..64)
        .map(|n| {
            (n..640)
                .step_by(64)
                .map(|j| f64::from(QMF_WINDOW[j].abs()))
                .sum::<f64>()
        })
        .fold(0.0f64, f64::max);
    for signal in 0..4 {
        let mut pcm = vec![0.0; 40 * 64];
        if signal < 3 {
            pcm[128 + [0, 31, 63][signal]] = 1.0;
        } else {
            let mut seed = 847012u32;
            for value in &mut pcm[..512] {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *value = ((seed >> 8) as f32 / 16777216.0 - 0.5) * 0.25;
            }
        }
        let expected_a = analyse_fir(&pcm, &analysis_kernel);
        let mut actual_a = vec![QmfSlot::zero(); 40];
        let mut analysis_state = QmfAnalysisState::new();
        analyse(&pcm, &mut analysis_state, &mut actual_a).unwrap();
        let amplitude = pcm.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max);
        let a_bound = 4.0 * f64::from(f32::EPSILON) * amplitude * window_sum;
        for (expected, actual) in expected_a.iter().zip(&actual_a) {
            for (band, pair) in expected.iter().enumerate() {
                require_forward_bound(
                    pair[0],
                    f64::from(actual.re[band]),
                    a_bound,
                    "analysis real",
                );
                require_forward_bound(
                    pair[1],
                    f64::from(actual.im[band]),
                    a_bound,
                    "analysis imag",
                );
            }
        }
        assert!(analysis_state.filt.iter().all(|v| *v == 0.0));
        for layout in 0..3 {
            // P71 的两个已独立核对布局：期望路由不读生产 patch 表。
            let (routes, suffix): (Vec<_>, _) = match layout {
                0 => ((0..64).map(|b| (b, b)).collect(), "full"),
                1 => (
                    (28..36).zip(36..44).chain((24..36).zip(44..56)).collect(),
                    "patch",
                ),
                _ => ((21..36).zip(47..62).collect(), "patch"),
            };
            for delay in [0, 6] {
                let config = match layout {
                    1 => " 1 6 1 3 0",
                    2 => " 1 6 0 3 5",
                    _ => "",
                };
                let case = parse(&format!("regression {suffix} 40 8 {delay}{config}")).unwrap();
                let actual_h = mapped(&case, &actual_a, &mut LowBandDelay::new());
                let a64: Vec<ComplexSlot> = actual_a
                    .iter()
                    .map(|s| core::array::from_fn(|b| [f64::from(s.re[b]), f64::from(s.im[b])]))
                    .collect();
                let same_input_h = map_fir(&a64, &routes, delay);
                for (expected, actual) in same_input_h.iter().zip(&actual_h) {
                    for (band, pair) in expected.iter().enumerate() {
                        assert_eq!(
                            *pair,
                            [f64::from(actual.re[band]), f64::from(actual.im[band])]
                        );
                    }
                }
                let expected_h = map_fir(&expected_a, &routes, delay);
                let expected_y = synthesise_fir(&expected_h, &synthesis_kernel);
                let same_input_y = synthesise_fir(&same_input_h, &synthesis_kernel);
                let mut synthesis_state = QmfSynthesisState::new();
                let mut actual_y = vec![0.0; pcm.len()];
                synthesise(&actual_h, &mut synthesis_state, &mut actual_y).unwrap();
                let q_max = same_input_h
                    .iter()
                    .map(|s| s.iter().map(|q| q[0].hypot(q[1])).sum::<f64>())
                    .fold(0.0, f64::max);
                let s_bound = 4.0 * f64::from(f32::EPSILON) * q_max * polyphase_sum / 64.0;
                let chain_bound = s_bound + a_bound * routes.len() as f64 * polyphase_sum / 64.0;
                for ((expected, same_input), actual) in
                    expected_y.iter().zip(&same_input_y).zip(&actual_y)
                {
                    require_forward_bound(*same_input, f64::from(*actual), s_bound, "synthesis");
                    require_forward_bound(*expected, f64::from(*actual), chain_bound, "chain");
                }
                assert!(synthesis_state.filt.iter().all(|v| *v == 0.0));
            }
        }
    }
}
