//! 从已接受的包络／反量化输入检查 P94，避免先舍入微小功率而丢掉可表示幅度。
#![expect(
    clippy::arithmetic_side_effects,
    reason = "构造只用有界的单步差分、合法频带表和正规数二次幂"
)]
extern crate std;

use super::*;
use crate::aspx::{
    dequant::dequantise,
    envelope::{self, EnvelopeDeltas, EnvelopeHistory, EnvelopeScaleFactors, SbgIndexMap},
    frames::AspxIntervalParams,
};
use std::vec;

fn power_of_two(exponent: i16) -> f32 {
    assert!((-126..=127).contains(&exponent));
    f32::from_bits(((i32::from(exponent) + 127) as u32) << 23)
}

fn decoded_scales(
    bands: &AspxBandTables,
    signal_exponent: i16,
    noise_exponent: i16,
) -> ScaleFactors {
    let signal_target = signal_exponent - 6;
    let noise_target = 6 - noise_exponent;
    let map = SbgIndexMap::derive(bands).unwrap();
    let mut history = EnvelopeHistory::new();
    let mut quantized = EnvelopeScaleFactors::new();
    let mut scales = ScaleFactors::new();
    let steps = signal_target
        .unsigned_abs()
        .max(noise_target.unsigned_abs());
    for step in 0..=steps {
        let signal_delta = if step != 0 && step <= signal_target.unsigned_abs() {
            signal_target.signum()
        } else {
            0
        };
        let noise_delta = if step != 0 && step <= noise_target.unsigned_abs() {
            noise_target.signum()
        } else {
            0
        };
        let signal = vec![signal_delta; usize::from(bands.num_sbg_sig_lowres())];
        let noise = vec![noise_delta; usize::from(bands.num_sbg_noise())];
        envelope::decode(
            &[EnvelopeDeltas {
                data: &signal,
                time_direction: step != 0,
                high_resolution: false,
            }],
            &[EnvelopeDeltas {
                data: &noise,
                time_direction: step != 0,
                high_resolution: false,
            }],
            &map,
            false,
            &mut history,
            &mut quantized,
        )
        .unwrap();
        dequantise(&quantized, true, &mut scales).unwrap();
        assert!(scales.sig(0, 0).unwrap().is_normal());
        assert!(scales.noise(0, 0).unwrap().is_normal());
    }
    assert_eq!(scales.sig(0, 0).unwrap(), power_of_two(signal_exponent));
    assert_eq!(scales.noise(0, 0).unwrap(), power_of_two(noise_exponent));
    scales
}

fn run(signal_exponent: i16, noise_exponent: i16) -> (AspxBandTables, EnvelopeEstimate) {
    let bands = AspxBandTables::derive(false, 0, 0, 0, 0).unwrap();
    let scales = decoded_scales(&bands, signal_exponent, noise_exponent);
    let interval = AspxInterval::derive(&AspxIntervalParams::fixfix(1), 16, 1, true, 16).unwrap();
    let mut result = EnvelopeEstimate::new();
    estimate(
        &[QmfSlot::zero(); 16],
        &bands,
        &interval,
        &scales,
        &[true; 64],
        SinePlacement::from_params(-1),
        true,
        1,
        &mut SineState::new(),
        &mut result,
    )
    .unwrap();
    (bands, result)
}

#[test]
fn representable_noise_amplitude_survives_power_underflow() {
    let (bands, result) = run(-126, -24);
    // 100 位 Decimal 对 S=2^-126, Q=2^-24 的 sqrt(S*Q/(1+Q)) 独立舍入。
    for band in 0..usize::from(bands.num_sb_aspx()) {
        assert_eq!(
            result.noise_level(band, 0).unwrap().to_bits(),
            0x1a00_0000,
            "P94 中间功率可下溢，但输出幅度 2^-75 必须保留"
        );
    }
}

#[test]
fn subnormal_split_factor_keeps_noise_and_sine_precision() {
    let (bands, result) = run(-126, 6);
    // 100 位 Decimal：不能先把 S/65 舍入到 f32 次正规数再开方。
    assert_eq!(result.noise_level(0, 0).unwrap().to_bits(), 0x1ffe_05ec);
    let first_mid = (usize::from(bands.sig_highres_border(0).unwrap())
        + usize::from(bands.sig_highres_border(1).unwrap()))
        / 2
        - usize::from(bands.sbx());
    assert!(result.sine_index(first_mid, 0).unwrap());
    assert_eq!(
        result.sine_level(first_mid, 0).unwrap().to_bits(),
        0x1e7e_05ec
    );
}

#[test]
fn ordinary_power_split_keeps_existing_f32_rounding() {
    for signal_exponent in [-100, -32, 0, 32, 64, 120] {
        for noise_exponent in [-24, -8, 0, 6] {
            let (bands, result) = run(signal_exponent, noise_exponent);
            let signal = power_of_two(signal_exponent);
            let noise = power_of_two(noise_exponent);
            let factor = signal / (1.0 + noise);
            let noise_power = factor * noise;
            assert!(factor.is_normal() && noise_power.is_normal());
            for band in 0..usize::from(bands.num_sb_aspx()) {
                assert_eq!(
                    result.noise_level(band, 0).unwrap().to_bits(),
                    sqrt_f32(noise_power).to_bits()
                );
                let sine =
                    sqrt_f32(factor * f32::from(u8::from(result.sine_index(band, 0).unwrap())));
                assert_eq!(
                    result.sine_level(band, 0).unwrap().to_bits(),
                    sine.to_bits()
                );
            }
        }
    }
}

#[test]
fn positive_normal_scale_factors_keep_both_amplitudes_and_the_power_budget() {
    for signal_exponent in -126..=127 {
        for noise_exponent in [-126, -64, -24, 0, 6, 64, 127] {
            let signal = power_of_two(signal_exponent);
            let ratio = power_of_two(noise_exponent);
            let (sine, noise) = generated_levels(signal, ratio, true);
            assert!(sine > 0.0 && sine.is_finite());
            assert!(noise > 0.0 && noise.is_finite());
            let power = f64::from(sine) * f64::from(sine) + f64::from(noise) * f64::from(noise);
            assert!(
                (power / f64::from(signal) - 1.0).abs() <= 5.0 * f64::from(f32::EPSILON),
                "P94 两份功率应合成原目标：S=2^{signal_exponent}, Q=2^{noise_exponent}"
            );
            let (absent, same_noise) = generated_levels(signal, ratio, false);
            assert_eq!(absent, 0.0);
            assert_eq!(same_noise.to_bits(), noise.to_bits());
        }
    }
}

#[test]
fn normal_subnormal_power_boundary_has_no_amplitude_jump() {
    let signal = power_of_two(-100);
    let centre = power_of_two(-26).to_bits();
    let mut previous = None;
    for bits in centre - 3..=centre + 3 {
        let (_, noise) = generated_levels(signal, f32::from_bits(bits), false);
        if let Some(last) = previous {
            assert!(noise.to_bits() >= last);
            assert!(noise.to_bits() - last <= 2);
        }
        previous = Some(noise.to_bits());
    }
    assert_eq!(generated_levels(1.0, 0.0, true), (1.0, 0.0));
    assert_eq!(generated_levels(0.0, 1.0, true), (0.0, 0.0));
}
