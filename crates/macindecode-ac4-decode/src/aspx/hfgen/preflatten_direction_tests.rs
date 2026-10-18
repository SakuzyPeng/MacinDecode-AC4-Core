//! 生产预平坦化回归：直接消费 P85 的结果，不在测试端替换增益方向。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试用固定的有界频带与 38 个时隙构造输入"
)]

extern crate std;
use super::*;
use crate::aspx::bands::AspxBandTables;
use crate::aspx::preflatten::pre_flatten;
use crate::aspx::tna::TnaDelay;
use std::vec;
use std::vec::Vec;

fn output(
    low: &[QmfSlot],
    bands: &AspxBandTables,
    master: bool,
    gains: Option<&PreFlattenGains>,
    first: usize,
    last: usize,
) -> (Vec<QmfSlot>, Vec<usize>) {
    filtered_output(low, bands, master, gains, first, last, false)
}

fn filtered_output(
    low: &[QmfSlot],
    bands: &AspxBandTables,
    master: bool,
    gains: Option<&PreFlattenGains>,
    first: usize,
    last: usize,
    prediction: bool,
) -> (Vec<QmfSlot>, Vec<usize>) {
    let patches = PatchTable::derive(bands, master, true).unwrap();
    let mut sources = Vec::new();
    for i in 0..usize::from(patches.count()) {
        let start = usize::from(patches.start_sb(i).unwrap());
        sources.extend(start..start + usize::from(patches.num_sb(i).unwrap()));
    }
    assert_eq!(sources.len(), usize::from(bands.num_sb_aspx()));
    let borders: Vec<_> = (0..=usize::from(bands.num_sbg_noise()))
        .map(|i| bands.noise_border(i).unwrap())
        .collect();
    let mut filters = TnaFilters::new();
    let (a0, a1) = if prediction {
        (
            Coefficient {
                re: 0.25,
                im: 0.125,
            },
            Coefficient {
                re: -0.0625,
                im: 0.1875,
            },
        )
    } else {
        (Coefficient::ZERO, Coefficient::ZERO)
    };
    filters.fill_for_test(bands.sba(), a0, a1);
    let mut delay = TnaDelay::new();
    delay.advance(&low[..4], 4).unwrap();
    let chirp: Vec<_> = (0..bands.num_sbg_noise())
        .map(|g| {
            if prediction {
                0.5 + f32::from(g % 3) * 0.125
            } else {
                0.0
            }
        })
        .collect();
    let mut result = vec![QmfSlot::zero(); last - first];
    hf_generate(
        ExtendedLowBand::new(&delay, low),
        &patches,
        &filters,
        &chirp,
        &borders,
        gains,
        first,
        last,
        &mut result,
    )
    .unwrap();
    (result, sources)
}

fn decibels(slot: &QmfSlot, band: usize) -> f64 {
    let re = f64::from(slot.re[band]);
    let im = f64::from(slot.im[band]);
    10.0 * (re * re + im * im).log10()
}

#[test]
fn production_preflattening_removes_cubic_shape_at_high_energy() {
    for (master, start, stop, noise, xover) in
        [(false, 0, 0, 0, 0), (true, 6, 1, 3, 0), (true, 6, 0, 3, 5)]
    {
        let bands = AspxBandTables::derive(master, start, stop, noise, xover).unwrap();
        let lo = usize::from(bands.sbx());
        for shape in [[16.0, 0.0, 0.0], [-16.0, 0.0, 0.0], [12.0, -4.0, 3.0]] {
            let mut low = [QmfSlot::zero(); 38];
            for slot in &mut low {
                for b in 0..lo {
                    let x = 2.0 * b as f64 / (lo - 1) as f64 - 1.0;
                    let db = 80.0 + shape[0] * x + shape[1] * x * x + shape[2] * x * x * x;
                    let amplitude = 10.0f64.powf(db / 20.0) as f32;
                    slot.re[b] = amplitude * 0.6;
                    slot.im[b] = amplitude * 0.8;
                }
            }
            let mean = (0..lo).map(|b| decibels(&low[0], b)).sum::<f64>() / lo as f64;
            for (first, last) in [(0, 32), (2, 38)] {
                let mut gains = PreFlattenGains::new();
                pre_flatten(&low, lo, first, last, &mut gains).unwrap();
                let (bypass, sources) = output(&low, &bands, master, None, first, last);
                let (rendered, _) = output(&low, &bands, master, Some(&gains), first, last);
                for slot in &rendered {
                    for b in lo..usize::from(bands.sbz()) {
                        let deviation = (decibels(slot, b) - mean).abs();
                        assert!(
                            deviation < 2e-4,
                            "生产未消除粗谱形：sbx={lo}, shape={shape:?}, interval=[{first},{last}), band={b}, deviation={deviation} dB"
                        );
                    }
                }
                // None 必须是原样搬运，不能隐含选取其中一个平滑方向。
                for slot in &bypass {
                    for (i, &source) in sources.iter().enumerate() {
                        assert_eq!(slot.re[lo + i].to_bits(), low[0].re[source].to_bits());
                        assert_eq!(slot.im[lo + i].to_bits(), low[0].im[source].to_bits());
                    }
                }
            }
        }
    }
}

#[test]
fn production_preflattening_keeps_silent_and_tiny_inputs_finite() {
    let bands = AspxBandTables::derive(true, 6, 1, 3, 0).unwrap();
    for amplitude in [0.0, 1e-12, 1.0] {
        let mut low = [QmfSlot::zero(); 38];
        for slot in &mut low {
            for b in 0..usize::from(bands.sbx()) {
                slot.re[b] = amplitude * b as f32;
                slot.im[b] = -amplitude;
            }
        }
        let mut gains = PreFlattenGains::new();
        pre_flatten(&low, usize::from(bands.sbx()), 2, 38, &mut gains).unwrap();
        {
            let (rendered, _) = output(&low, &bands, true, Some(&gains), 2, 38);
            for slot in rendered {
                for b in usize::from(bands.sbx())..usize::from(bands.sbz()) {
                    assert!(slot.re[b].is_finite() && slot.im[b].is_finite());
                    if amplitude == 0.0 {
                        assert_eq!((slot.re[b], slot.im[b]), (0.0, 0.0));
                    }
                }
            }
        }
    }
}

/// 对等距对称网格使用四条显式正交多项式投影。
/// 与生产的 Cholesky 正规方程不同，也不读取它的拟合阶数或系数。
fn independent_cubic_projection(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    let u: Vec<_> = (0..n)
        .map(|b| 2.0 * b as f64 / (n - 1) as f64 - 1.0)
        .collect();
    let s2 = u.iter().map(|v| v * v).sum::<f64>();
    let s4 = u.iter().map(|v| v.powi(4)).sum::<f64>();
    let basis: [Vec<f64>; 4] = [
        vec![1.0; n],
        u.clone(),
        u.iter().map(|v| v * v - s2 / n as f64).collect(),
        u.iter().map(|v| v * v * v - s4 / s2 * v).collect(),
    ];
    let mut projected = vec![0.0; n];
    for vector in &basis {
        let coefficient = values.iter().zip(vector).map(|(v, p)| v * p).sum::<f64>()
            / vector.iter().map(|p| p * p).sum::<f64>();
        for (out, p) in projected.iter_mut().zip(vector) {
            *out += coefficient * p;
        }
    }
    projected
}

#[test]
fn independent_projection_is_orthogonal_and_preserves_a_non_cubic_residual() {
    let values: Vec<_> = (0..36)
        .map(|b| 30.0 + b as f64 * 0.7 + if b % 2 == 0 { 4.0 } else { -4.0 })
        .collect();
    let fit = independent_cubic_projection(&values);
    let residual: Vec<_> = values.iter().zip(&fit).map(|(a, b)| a - b).collect();
    assert!(residual.iter().map(|v| v * v).sum::<f64>() > 100.0);
    for power in 0..=3 {
        let moment = residual
            .iter()
            .enumerate()
            .map(|(b, r)| r * (2.0 * b as f64 / 35.0 - 1.0).powi(power))
            .sum::<f64>();
        assert!(moment.abs() < 1e-10);
    }
    for w in fit.windows(5) {
        assert!((w[0] - 4.0 * w[1] + 6.0 * w[2] - 4.0 * w[3] + w[4]).abs() < 1e-10);
    }
}

#[test]
fn production_preserves_non_cubic_residual_and_applies_the_floor_before_fitting() {
    let bands = AspxBandTables::derive(true, 6, 1, 3, 0).unwrap();
    let n = usize::from(bands.sbx());
    for profile in 0..5 {
        let mut low = [QmfSlot::zero(); 38];
        for (t, slot) in low.iter_mut().enumerate() {
            for b in 0..n {
                let u = 2.0 * b as f64 / (n - 1) as f64 - 1.0;
                let power = match profile {
                    0 => 10.0f64.powf(
                        (50.0 + 9.0 * u - 2.0 * u * u + if b % 2 == 0 { 7.0 } else { -7.0 }) / 10.0,
                    ),
                    1 => {
                        if b == 29 {
                            1e8
                        } else {
                            1e4
                        }
                    }
                    2 => {
                        if b == 28 {
                            0.0
                        } else {
                            1e4 * (1.0 + u * u)
                        }
                    }
                    3 => 1e-12 * (1.0 + b as f64),
                    4 => 0.01 * (1.0 + b as f64).powi(2),
                    _ => unreachable!(),
                };
                let amplitude = power.sqrt() as f32;
                let phase = (0.23 * t as f64 + 0.11 * b as f64).sin() as f32;
                slot.re[b] = amplitude * phase;
                slot.im[b] = amplitude * (0.23 * t as f64 + 0.11 * b as f64).cos() as f32;
            }
        }
        for (first, last) in [(0, 16), (2, 38)] {
            let energy: Vec<_> = (0..n)
                .map(|b| {
                    let p = low[first..last]
                        .iter()
                        .map(|s| f64::from(s.re[b]).powi(2) + f64::from(s.im[b]).powi(2))
                        .sum::<f64>()
                        / (last - first) as f64;
                    10.0 * (p + 1.0).log10()
                })
                .collect();
            let mean = energy.iter().sum::<f64>() / n as f64;
            let fitted = independent_cubic_projection(&energy);
            let mut gains = PreFlattenGains::new();
            pre_flatten(&low, n, first, last, &mut gains).unwrap();
            for prediction in [false, true] {
                let (plain, sources) =
                    filtered_output(&low, &bands, true, None, first, last, prediction);
                let (rendered, _) =
                    filtered_output(&low, &bands, true, Some(&gains), first, last, prediction);
                for (base, actual) in plain.iter().zip(&rendered) {
                    for (i, &source) in sources.iter().enumerate() {
                        // 期望值由输入包络的独立投影给出，不读取生产 gain。
                        let correction = 10.0f64.powf((mean - fitted[source]) / 20.0);
                        for (x, y) in [
                            (base.re[n + i], actual.re[n + i]),
                            (base.im[n + i], actual.im[n + i]),
                        ] {
                            let expected = f64::from(x) * correction;
                            assert!(y.is_finite());
                            if expected == 0.0 {
                                assert_eq!(y, 0.0);
                            } else {
                                assert!(
                                    ((f64::from(y) - expected) / expected).abs() < 2e-6,
                                    "profile={profile}, prediction={prediction}, source={source}"
                                );
                                assert_eq!(x.is_sign_negative(), y.is_sign_negative());
                            }
                        }
                    }
                }
            }
        }
    }
}
