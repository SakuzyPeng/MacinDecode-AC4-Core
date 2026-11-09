//! 仅测试的固定相位候选；默认不执行算术，不改变表、游标、音调或公共接口。
//! R(k) = -exp(i*pi*(k+0.5)/128)。候选是物理算法变化，不能当作无损换表示。
#![expect(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试入口使用已验证的 64 带布局；固定旋转只写当前生成区间"
)]

extern crate std;
use crate::aspx::{patches::PatchTable, qmf::QmfSlot};
use std::{cell::RefCell, format, string::String, sync::OnceLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Baseline,
    NoiseInverseR,
    PatchRatio,
    NoiseAndPatch,
}

impl Mode {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name {
            "baseline" => Some(Self::Baseline),
            "noise_inverse_r" => Some(Self::NoiseInverseR),
            "patch_ratio" => Some(Self::PatchRatio),
            "noise_and_patch" => Some(Self::NoiseAndPatch),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::NoiseInverseR => "noise_inverse_r",
            Self::PatchRatio => "patch_ratio",
            Self::NoiseAndPatch => "noise_and_patch",
        }
    }
}

#[derive(Debug, Default)]
struct State {
    active: bool,
    mode: Mode,
    noise_components: usize,
    patch_components: usize,
    max_energy_error_over_bound: f64,
}

std::thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

#[derive(Debug)]
pub(crate) struct Scope;

pub(crate) fn enter(mode: Mode) -> Scope {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        assert!(!state.active, "相位候选作用域不能嵌套");
        *state = State {
            active: true,
            mode,
            ..State::default()
        };
    });
    Scope
}

impl Drop for Scope {
    fn drop(&mut self) {
        STATE.with(|state| *state.borrow_mut() = State::default());
    }
}

pub(crate) fn summary() -> String {
    STATE.with(|state| {
        let state = state.borrow();
        format!("{{\"mode\":\"{}\",\"noise_components\":{},\"patch_components\":{},\"max_energy_error_over_bound\":{}}}\n",
            state.mode.name(), state.noise_components, state.patch_components, state.max_energy_error_over_bound)
    })
}

fn rotate(re: f32, im: f32, c: f64, s: f64) -> (f32, f32, f64) {
    let (x, y) = (f64::from(re), f64::from(im));
    let a = (x * c - y * s) as f32;
    let b = (x * s + y * c) as f32;
    assert!(a.is_finite() && b.is_finite());
    let energy = x * x + y * y;
    let after = f64::from(a) * f64::from(a) + f64::from(b) * f64::from(b);
    // 两次 f32 舍入，包含次正规数绝对误差；f64 三角/运算误差另留固定余量。
    let rounding =
        (2.0 * energy).sqrt() * f64::from(f32::EPSILON) / 2.0 + f64::from(f32::from_bits(1));
    let bound = 2.0 * energy.sqrt() * rounding + rounding * rounding + 64.0 * f64::EPSILON * energy;
    let ratio = (after - energy).abs() / bound;
    assert!(ratio <= 1.0, "单位复旋转改变能量超出固定舍入界");
    (a, b, ratio)
}

pub(crate) fn noise(sbx: u8, sbz: u8, output: &mut [QmfSlot]) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !matches!(state.mode, Mode::NoiseInverseR | Mode::NoiseAndPatch) {
            return;
        }
        assert!(sbx <= sbz && sbz <= 64);
        static COEFFICIENTS: OnceLock<[(f64, f64); 64]> = OnceLock::new();
        let coefficients = COEFFICIENTS.get_or_init(|| {
            core::array::from_fn(|band| {
                let angle = core::f64::consts::PI * (band as f64 + 0.5) / 128.0;
                (-angle.cos(), angle.sin())
            })
        });
        for slot in output {
            for (band, &(c, s)) in coefficients
                .iter()
                .enumerate()
                .take(usize::from(sbz))
                .skip(usize::from(sbx))
            {
                let (re, im, ratio) = rotate(slot.re[band], slot.im[band], c, s);
                slot.re[band] = re;
                slot.im[band] = im;
                state.noise_components += 1;
                state.max_energy_error_over_bound = state.max_energy_error_over_bound.max(ratio);
            }
        }
    });
}

pub(crate) fn patch(patches: &PatchTable, output: &mut [QmfSlot]) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !matches!(state.mode, Mode::PatchRatio | Mode::NoiseAndPatch) {
            return;
        }
        for index in 0..usize::from(patches.count()) {
            let source = patches.start_sb(index).unwrap();
            let target = patches.border(index).unwrap();
            let count = patches.num_sb(index).unwrap();
            let angle = core::f64::consts::PI * (f64::from(source) - f64::from(target)) / 128.0;
            let (c, s) = (angle.cos(), angle.sin());
            for slot in output.iter_mut() {
                for band in usize::from(target)..usize::from(target) + usize::from(count) {
                    let (re, im, ratio) = rotate(slot.re[band], slot.im[band], c, s);
                    slot.re[band] = re;
                    slot.im[band] = im;
                    state.patch_components += 1;
                    state.max_energy_error_over_bound =
                        state.max_energy_error_over_bound.max(ratio);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    fn signal() -> [QmfSlot; 2] {
        core::array::from_fn(|time| {
            let mut slot = QmfSlot::zero();
            for band in 0..64 {
                slot.re[band] = (band + time + 1) as f32 / 16.0;
                slot.im[band] = (band % 13) as f32 / 8.0 - 0.5;
            }
            slot
        })
    }

    fn bits(data: &[QmfSlot]) -> std::vec::Vec<u32> {
        data.iter()
            .flat_map(|s| s.re.iter().chain(&s.im).map(|v| v.to_bits()))
            .collect()
    }

    #[test]
    fn baseline_and_scope_drop_leave_all_bits_unchanged() {
        let original = signal();
        let mut actual = original;
        let patches = PatchTable::from_parts(&[8], &[8], 32);
        {
            let _scope = enter(Mode::Baseline);
            noise(32, 40, &mut actual);
            patch(&patches, &mut actual);
        }
        assert_eq!(bits(&actual), bits(&original));
        {
            let _scope = enter(Mode::NoiseInverseR);
        }
        noise(32, 40, &mut actual);
        assert_eq!(bits(&actual), bits(&original));
        assert!(Mode::parse("unknown").is_none());
    }

    #[test]
    fn noise_inverse_rotation_keeps_other_bands_and_cross_channel_products() {
        let original = signal();
        let mut actual = original;
        let _scope = enter(Mode::NoiseInverseR);
        noise(32, 48, &mut actual);
        for band in 0..64 {
            if !(32..48).contains(&band) {
                for time in 0..2 {
                    assert_eq!(actual[time].re[band], original[time].re[band]);
                    assert_eq!(actual[time].im[band], original[time].im[band]);
                }
                continue;
            }
            let product = |q: &[QmfSlot; 2]| {
                (
                    f64::from(q[0].re[band]) * f64::from(q[1].re[band])
                        + f64::from(q[0].im[band]) * f64::from(q[1].im[band]),
                    f64::from(q[0].im[band]) * f64::from(q[1].re[band])
                        - f64::from(q[0].re[band]) * f64::from(q[1].im[band]),
                )
            };
            let (a, b) = product(&original);
            let (c, d) = product(&actual);
            assert!(
                (a - c).abs() + (b - d).abs() < 8.0 * f64::from(f32::EPSILON) * (a.abs() + b.abs())
            );
        }
        assert_ne!(bits(&actual), bits(&original));
    }

    #[test]
    fn splitting_noise_does_not_change_phase_or_add_another_rotation() {
        let mut whole = signal();
        let mut split = signal();
        let _scope = enter(Mode::NoiseInverseR);
        noise(4, 64, &mut whole);
        for part in split.chunks_mut(1) {
            noise(4, 64, part);
        }
        assert_eq!(bits(&whole), bits(&split));
    }

    #[test]
    fn patch_uses_source_over_target_and_keeps_uncovered_tail() {
        let patches = PatchTable::from_parts(&[2, 3], &[8, 17], 32);
        let mut output = vec![QmfSlot::zero(); 1];
        output[0].re.fill(1.0);
        let _scope = enter(Mode::PatchRatio);
        patch(&patches, &mut output);
        for (start, end, source) in [(32, 34, 8), (34, 37, 17)] {
            let angle = core::f64::consts::PI * f64::from(source - start) / 128.0;
            for band in start..end {
                assert_eq!(output[0].re[band as usize], angle.cos() as f32);
                assert_eq!(output[0].im[band as usize], angle.sin() as f32);
            }
        }
        assert_eq!(output[0].re[37], 1.0);
        assert_eq!(output[0].im[37], 0.0);
    }

    #[test]
    fn zero_and_subnormal_components_respect_absolute_rounding_bound() {
        let mut slots = [QmfSlot::zero(); 1];
        for band in 0..64 {
            slots[0].re[band] = f32::from_bits(band as u32);
            slots[0].im[band] = -f32::from_bits((band * 3) as u32);
        }
        let _scope = enter(Mode::NoiseInverseR);
        noise(0, 64, &mut slots);
        assert!(
            slots[0]
                .re
                .iter()
                .chain(&slots[0].im)
                .all(|v| v.is_finite())
        );
    }

    #[test]
    fn rotation_direction_and_half_band_origin_match_decimal_anchors() {
        // 独立 80 位 Decimal/Machin π/Taylor 正弦余弦；就近偶数舍入到 f32。
        let anchors = [
            (0, 0xbf7f_fb11, 0x3c49_0e90),
            (15, 0xbf6d_b293, 0x3ebe_1d4a),
            (31, 0xbf37_3a23, 0x3f32_c8c9),
            (32, 0xbf32_c8c9, 0x3f37_3a23),
            (47, 0xbec9_b953, 0x3f6b_4b0c),
            (63, 0xbc49_0e90, 0x3f7f_fb11),
        ];
        let mut output = [QmfSlot::zero(); 1];
        output[0].re.fill(1.0);
        {
            let _scope = enter(Mode::NoiseInverseR);
            noise(0, 64, &mut output);
        }
        for (band, re, im) in anchors {
            assert_eq!(output[0].re[band].to_bits(), re);
            assert_eq!(output[0].im[band].to_bits(), im);
        }
        let _scope = enter(Mode::PatchRatio);
        for (source, target, re, im) in [
            (28, 36, 0x3f7b_14be, 0xbe47_c5c2),
            (24, 44, 0x3f61_c598, 0xbef1_5aea),
            (8, 40, 0x3f35_04f3, 0xbf35_04f3),
        ] {
            output[0] = QmfSlot::zero();
            output[0].re.fill(1.0);
            patch(
                &PatchTable::from_parts(&[1], &[source], target),
                &mut output,
            );
            assert_eq!(output[0].re[usize::from(target)].to_bits(), re);
            assert_eq!(output[0].im[usize::from(target)].to_bits(), im);
        }
    }
}
