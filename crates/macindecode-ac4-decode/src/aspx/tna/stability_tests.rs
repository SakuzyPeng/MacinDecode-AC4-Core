//! 固定 f32 时间序列的 TNA 数值调查；原始解、实际存储系数和 HF 分开记录。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "离线输入先核对表 189/192 几何、大小与有限性"
)]
extern crate std;
use super::*;
use crate::aspx::hfgen::hf_generate;
use crate::aspx::patches::PatchTable;
use std::borrow::ToOwned;
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::vec;
use std::vec::Vec;

const LAYOUTS: [(usize, usize); 8] = [
    (6, 3),
    (8, 3),
    (12, 3),
    (15, 3),
    (16, 3),
    (24, 6),
    (30, 6),
    (32, 6),
];

/// 自有 f32 反例 b217。精确有理数给出 |alpha0|²−16 = +6.236892900235211e−9。
/// 固定位型避免宿主三角函数差异；独立证明见 check_tna_stability.py。
fn boundary_values() -> Vec<[f32; 2]> {
    const BITS: [u32; 84] = [
        723141345, 0, 728933277, 725520095, 722616597, 739630475, 2885630915, 747401760,
        2903043499, 747509288, 2912391511, 2892340186, 2915758778, 2917687300, 2894113697,
        2929331502, 783394392, 2935766601, 798212456, 2930855530, 806327502, 794925444, 807152515,
        814025185, 2946897327, 823712670, 2975693475, 827771211, 2988010555, 813727447, 2994902188,
        2988720674, 2991579214, 3004251751, 851663412, 3012711301, 872466808, 3014251555,
        882490232, 852244422, 887229010, 886176528, 876629607, 899178615, 3046534033, 906407754,
        3062781511, 904782200, 3071585373, 3055850210, 3073836679, 3078119114, 899116031,
        3088724043, 944105804, 3094130171, 957803165, 3086981141, 965372963, 956836935, 965058179,
        973802255, 3112524029, 982949299, 3136191929, 985905535, 3147446726, 953418224, 3153505500,
        3149483455, 3147765736, 3163884465, 1014176862, 3171797149, 1032281865, 3172200258,
        1041770028, 1019970691, 1045423155, 1046738590, 1026920977, 1058658272, 3207344431,
        1065353216,
    ];
    BITS.as_chunks::<2>()
        .0
        .iter()
        .map(|b| b.map(f32::from_bits))
        .collect()
}

#[test]
fn near_singular_boundary_is_clamped_from_the_original_samples() {
    let values = boundary_values();
    let (_, _, actual) = coefficients(&values, 32);
    assert_eq!(
        actual, [0.0; 4],
        "数学上模长已超过 4，不能被协方差舍入翻到保留分支"
    );
}

#[test]
fn exactly_four_is_still_cleared_without_a_threshold_tolerance() {
    let values: Vec<_> = (0..42).map(|t| [2f32.powi(t - 41), 0.0]).collect();
    let (_, raw, stored) = coefficients(&values, 32);
    assert_eq!(raw, [-4.0, 0.0, 0.0, 0.0]);
    assert_eq!(stored, [0.0; 4]);
}

fn input(values: &[[f32; 2]]) -> (TnaDelay, Vec<QmfSlot>) {
    let mut delay = TnaDelay::new();
    let mut low = vec![QmfSlot::zero(); values.len() - 4];
    for (t, &[re, im]) in values.iter().enumerate() {
        let slot = if t < 4 {
            &mut delay.tail[t]
        } else {
            &mut low[t - 4]
        };
        slot.re[0] = re;
        slot.im[0] = im;
    }
    (delay, low)
}

fn coefficients(values: &[[f32; 2]], n: usize) -> (Covariance, [f64; 4], [f32; 4]) {
    let (history, low) = input(values);
    let ext = ExtendedLowBand::new(&history, &low);
    let cov = covariance(ext, 0, values.len());
    let (a, b) = solve_projected(ext, 0, values.len()).unwrap();
    let mut filters = TnaFilters::new();
    prediction_filters(ext, n, 1, &mut filters).unwrap();
    let (x, y) = filters.coefficients(0).unwrap();
    (cov, [a.re, a.im, b.re, b.im], [x.re, x.im, y.re, y.im])
}

fn save_f32(path: &Path, data: impl Iterator<Item = f32>) {
    let mut f = std::fs::File::create_new(path).unwrap();
    for v in data {
        assert!(v.is_finite());
        f.write_all(&v.to_le_bytes()).unwrap();
    }
}

#[test]
#[ignore = "requires fixed local inputs and fresh output on a verified cache volume"]
fn replay_tna_stability_inputs() {
    let input_path = std::env::var("MACINAC4_TNA_STABILITY_INPUT").unwrap();
    let output_path = std::env::var("MACINAC4_TNA_STABILITY_OUTPUT").unwrap();
    let path = Path::new(&input_path);
    let root = Path::new(&output_path);
    std::fs::create_dir(root).unwrap();
    std::fs::write(root.join("replay_metadata.json"),
        "{\"schema\":\"macinac4.tna-stability-replay\",\"version\":1,\"solver\":\"orthogonal_residual\"}\n").unwrap();
    let mut ids = BTreeSet::new();
    let mut log = std::fs::File::create_new(root.join("results.txt")).unwrap();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let p: Vec<_> = line.split_whitespace().collect();
        assert_eq!(p.len(), 4);
        let id = p[0];
        assert!(
            !id.is_empty()
                && id.len() <= 63
                && id.bytes().all(|x| x.is_ascii_alphanumeric())
                && ids.insert(id.to_owned())
        );
        let n: usize = p[1].parse().unwrap();
        let offset: usize = p[2].parse().unwrap();
        let shift: i32 = p[3].parse().unwrap();
        assert!(LAYOUTS.contains(&(n, offset)) && (-130..=155).contains(&shift));
        let bytes = std::fs::read(path.parent().unwrap().join(format!("{id}.f32"))).unwrap();
        assert_eq!(bytes.len(), (n + offset + 4) * 8);
        let values: Vec<_> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|x| {
                [
                    f32::from_le_bytes(x[..4].try_into().unwrap()),
                    f32::from_le_bytes(x[4..].try_into().unwrap()),
                ]
            })
            .collect();
        assert!(values.iter().flatten().all(|x| x.is_finite()));
        let (cov, raw, stored) = coefficients(&values, n);
        assert_eq!(cov.get(1, 1).im, 0.0);
        assert_eq!(cov.get(2, 2).im, 0.0);
        let mut f = std::fs::File::create_new(root.join(format!("{id}.f64"))).unwrap();
        for v in [
            cov.get(0, 1).re,
            cov.get(0, 1).im,
            cov.get(0, 2).re,
            cov.get(0, 2).im,
            cov.get(1, 1).re,
            cov.get(1, 2).re,
            cov.get(1, 2).im,
            cov.get(2, 2).re,
        ]
        .into_iter()
        .chain(raw)
        {
            assert!(v.is_finite());
            f.write_all(&v.to_le_bytes()).unwrap();
        }
        save_f32(&root.join(format!("{id}.coeff.f32")), stored.into_iter());
        // HF 仅验证给定实际系数的消费。按输入预声明的二次幂归一化，避免故意的
        // f32 极端输入使输出加法溢出；预测器本身仍消费原始幅度。
        let normalized: Vec<_> = values
            .iter()
            .map(|v| v.map(|x| (f64::from(x) * 2f64.powi(shift)) as f32))
            .collect();
        let (history, low) = input(&normalized);
        let ext = ExtendedLowBand::new(&history, &low);
        let mut filters = TnaFilters::new();
        filters.fill_for_test(
            1,
            Coefficient {
                re: stored[0],
                im: stored[1],
            },
            Coefficient {
                re: stored[2],
                im: stored[3],
            },
        );
        let patch = PatchTable::from_parts(&[1], &[0], 36);
        for (i, chirp) in [0.0, 0.6, 0.9, 0.98, 1.0].into_iter().enumerate() {
            let mut high = vec![QmfSlot::zero(); n + offset];
            hf_generate(
                ext,
                &patch,
                &filters,
                &[chirp],
                &[36, 37],
                None,
                0,
                n + offset,
                &mut high,
            )
            .unwrap();
            assert!(high.iter().all(|q| {
                (0..64)
                    .filter(|&b| b != 36)
                    .all(|b| q.re[b] == 0.0 && q.im[b] == 0.0)
            }));
            save_f32(
                &root.join(format!("{id}.hf{i}.f32")),
                high.iter().flat_map(|q| [q.re[36], q.im[36]]),
            );
        }
        writeln!(log, "{id} {n} {offset} {shift}").unwrap();
    }
    assert!(!ids.is_empty());
}

#[test]
fn regularized_prediction_is_finite_and_power_of_two_scale_invariant() {
    for (n, offset) in LAYOUTS {
        for pattern in 0..4 {
            let base: Vec<_> = (0..n + offset + 4)
                .map(|t| {
                    let t = t as f64;
                    let z = match pattern {
                        0 => (0.0, 0.0),
                        1 => ((0.23 * t).cos(), (0.23 * t).sin()),
                        2 => (
                            (0.23 * t).cos() + 0.001 * (0.81 * t).cos(),
                            (0.23 * t).sin() + 0.001 * (0.81 * t).sin(),
                        ),
                        _ => (0.5 + 0.01 * t, 0.25 - 0.003 * t * t),
                    };
                    [z.0 as f32, z.1 as f32]
                })
                .collect();
            let (_, _, expected) = coefficients(&base, n);
            let rotated: Vec<_> = base.iter().map(|&[re, im]| [-im, re]).collect();
            assert_eq!(
                coefficients(&rotated, n).2,
                expected,
                "精确四分之一周旋转不应改变预测系数"
            );
            for exponent in [-100, -50, 0, 50, 100] {
                let scale = 2f64.powi(exponent);
                let scaled: Vec<_> = base
                    .iter()
                    .map(|v| v.map(|x| (f64::from(x) * scale) as f32))
                    .collect();
                assert!(
                    scaled.iter().zip(&base).all(|(a, b)| a
                        .iter()
                        .zip(b)
                        .all(|(&a, &b)| f64::from(a) == f64::from(b) * scale)),
                    "前提：输入缩放须精确"
                );
                let (cov, raw, actual) = coefficients(&scaled, n);
                assert!(raw.iter().all(|x| x.is_finite()));
                assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
                let product = cov.get(1, 1).re * cov.get(2, 2).re;
                if product > 0.0 {
                    let denominator = product - cov.get(1, 2).norm_sqr() / (1.0 + 1.0 / 1048576.0);
                    assert!(denominator > 0.0);
                    assert!(
                        denominator / product
                            >= ((1.0 / 1048576.0) / (1.0 + 1.0 / 1048576.0)) * (1.0 - 1e-7)
                    );
                }
            }
        }
    }
}

#[test]
fn finite_extreme_input_keeps_finite_coefficients_and_bad_later_band_is_transactional() {
    let tiny = f32::from_bits(1);
    for value in [tiny, f32::MIN_POSITIVE, f32::MAX] {
        let (_, raw, stored) = coefficients(&vec![[value, -value]; 42], 32);
        assert!(raw.iter().all(|v| v.is_finite()));
        assert_eq!(stored, [-1.0, 0.0, 0.0, 0.0]);
    }
    let values = vec![[1.0, 0.5]; 42];
    let (history, mut low) = input(&values);
    let mut out = TnaFilters::new();
    out.fill_for_test(
        2,
        Coefficient { re: 0.25, im: 0.5 },
        Coefficient {
            re: -0.5,
            im: 0.125,
        },
    );
    let before = [out.coefficients(0), out.coefficients(1)];
    low[4].re[1] = f32::NAN;
    assert_eq!(
        prediction_filters(ExtendedLowBand::new(&history, &low), 32, 2, &mut out),
        Err(TnaError::NonFiniteCoefficient { subband: 1 })
    );
    assert_eq!(out.subbands(), 2);
    assert_eq!([out.coefficients(0), out.coefficients(1)], before);

    // 第一延迟列全零也不能让末个当前样本中的 NaN/Inf 被零分母分支吞掉。
    for invalid in [f32::NAN, f32::INFINITY] {
        let mut values = vec![[0.0; 2]; 42];
        values[40][0] = invalid;
        let (history, low) = input(&values);
        assert_eq!(
            prediction_filters(ExtendedLowBand::new(&history, &low), 32, 1, &mut out),
            Err(TnaError::NonFiniteCoefficient { subband: 0 })
        );
        assert_eq!([out.coefficients(0), out.coefficients(1)], before);
    }
}

#[test]
fn second_normal_equation_includes_the_independent_ridge_budget() {
    let values: Vec<_> = (0..42).map(|t| [1.0, t as f32 * 0.25]).collect();
    let (_, raw, stored) = coefficients(&values, 32);
    let a0 = (raw[0], raw[1]);
    let a1 = (raw[2], raw[3]);
    assert!(a1.0 * a1.0 + a1.1 * a1.1 > 0.1);
    assert!(stored.iter().any(|x| *x != 0.0));
    let mut projection = (0.0, 0.0);
    let mut c12 = (0.0, 0.0);
    let mut c11 = 0.0;
    let mut energy = 0.0;
    for t in (4..42).step_by(2) {
        let [yr, yi] = values[t].map(f64::from);
        let [xr, xi] = values[t - 2].map(f64::from);
        let [wr, wi] = values[t - 4].map(f64::from);
        let rr = yr + a0.0 * xr - a0.1 * xi + a1.0 * wr - a1.1 * wi;
        let ri = yi + a0.0 * xi + a0.1 * xr + a1.0 * wi + a1.1 * wr;
        projection.0 += rr * wr + ri * wi;
        projection.1 += ri * wr - rr * wi;
        c12.0 += xr * wr + xi * wi;
        c12.1 += xi * wr - xr * wi;
        c11 += xr * xr + xi * xi;
        energy += yr * yr + yi * yi;
    }
    // 不调用生产 solve 或 covariance 生成期望值；直接检查残差的第二个梯度。
    let ridge = (c12.0 * c12.0 + c12.1 * c12.1) / (c11 * 1048577.0);
    let gr = projection.0 + ridge * a1.0;
    let gi = projection.1 + ridge * a1.1;
    assert!(
        gr.hypot(gi) < energy * 1e-12,
        "第二条正规方程不符：{gr}, {gi}"
    );
}
