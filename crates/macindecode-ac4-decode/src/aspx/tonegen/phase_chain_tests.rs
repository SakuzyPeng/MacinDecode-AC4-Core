//! 固定音调幅度，实际执行发生器、跨帧组装、合并与合成；仅供独立相位调查。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "固定合法频带和区间先验证，后续下标由这些有界值派生"
)]
extern crate std;
use super::*;
use crate::aspx::dequant::ScaleFactors;
use crate::aspx::frames::AspxIntervalParams;
use crate::aspx::hfadjust::{EnvelopeEstimate, SinePlacement, SineState, estimate};
use crate::aspx::hfassemble::{HfDelay, assemble};
use crate::aspx::hfgain::{LimiterMode, adjust};
use crate::aspx::interleave::{AspxOutputDelay, combine};
use crate::aspx::qmf::{QMF_WINDOW, QmfSynthesisState, synthesise};
use crate::aspx::tables::IntervalClass;
use std::format;
use std::io::Write;
use std::path::Path;
use std::vec;
use std::vec::Vec;

const FRAMES: usize = 4;
const FRAME_ATS: u8 = 15; // 非 4 的倍数，使重置／冻结游标的错误不能被绕圈掩盖。
const TAIL: usize = 16;

#[derive(Clone, Copy)]
struct Case {
    band: usize,
    seed: u8,
    factor: u8,
    carry: u8,
    profile: u8,
}

struct Run {
    qmf: Vec<QmfSlot>,
    pcm: Vec<f32>,
    envelope: Vec<f32>,
    last: Vec<u8>,
}

fn interval(frame: usize, carry: u8) -> AspxInterval {
    let left = if frame == 0 { 0 } else { carry };
    let right = if frame + 1 == FRAMES { 0 } else { carry };
    let mut p = AspxIntervalParams::fixfix(1);
    if left != 0 || right != 0 {
        p.int_class = match (left != 0, right != 0) {
            (false, true) => IntervalClass::FixVar,
            (true, false) => IntervalClass::VarFix,
            _ => IntervalClass::VarVar,
        };
        p.var_bord_left = Some(left);
        p.var_bord_right = Some(right);
    }
    let value = AspxInterval::derive(&p, FRAME_ATS, 1, true, i16::from(FRAME_ATS)).unwrap();
    assert_eq!(value.num_atsg_sig(), 1);
    assert_eq!(value.sig_border(0), Some(i16::from(left)));
    assert_eq!(value.sig_border(1), Some(i16::from(FRAME_ATS + right)));
    value
}

fn fixed_gains(
    bands: &AspxBandTables,
    interval: &AspxInterval,
    factor: u8,
    band: usize,
    level: f32,
) -> AdjustedGains {
    let mut sf = ScaleFactors::new();
    sf.fill_for_test(
        1,
        usize::from(bands.num_sbg_sig_highres()),
        usize::from(bands.num_sbg_noise()),
        1.0,
        1.0,
    );
    let mut estimated = EnvelopeEstimate::new();
    estimate(
        &[QmfSlot::zero(); 38],
        bands,
        interval,
        &sf,
        &[false; 64],
        SinePlacement::from_params(-1),
        true,
        factor,
        &mut SineState::new(),
        &mut estimated,
    )
    .unwrap();
    let mut g = AdjustedGains::new();
    adjust(&estimated, bands, LimiterMode::Off, &mut g).unwrap();
    for b in 0..usize::from(g.subbands()) {
        assert!(g.set_signal_gain_for_test(b, 0, 0.0));
        assert!(g.set_noise_level_for_test(b, 0, 0.0));
        assert!(g.set_sine_level_for_test(
            b,
            0,
            if b + usize::from(bands.sbx()) == band {
                level
            } else {
                0.0
            }
        ));
    }
    g
}

fn run(c: Case) -> Run {
    assert!(
        (10..62).contains(&c.band)
            && c.seed < 4
            && matches!(c.factor, 1 | 2)
            && c.carry <= 3
            && c.profile <= 1
    );
    // 两个合法模板覆盖可用音调带 10..61；高带模板故意从奇数交叉子带 19 起。
    let bands = if c.band < 19 {
        AspxBandTables::derive(false, 0, 0, 0, 0).unwrap()
    } else {
        AspxBandTables::derive(true, 0, 0, 0, 1).unwrap()
    };
    assert!((usize::from(bands.sbx())..usize::from(bands.sbz())).contains(&c.band));
    let frame_slots = usize::from(FRAME_ATS * c.factor);
    let mut cursor = ToneCursor {
        previous: (c.seed + 3) % 4,
    };
    let mut hf = HfDelay::new();
    let mut output_delay = AspxOutputDelay::new();
    let mut synthesis = QmfSynthesisState::new();
    let mut result = Run {
        qmf: Vec::new(),
        pcm: Vec::new(),
        envelope: vec![0.0; FRAMES * frame_slots + TAIL],
        last: Vec::new(),
    };
    for frame in 0..FRAMES {
        let interval = interval(frame, c.carry);
        let head =
            usize::try_from(interval.sig_border(0).unwrap()).unwrap() * usize::from(c.factor);
        let end = usize::try_from(interval.sig_border(1).unwrap()).unwrap() * usize::from(c.factor);
        let level = if c.profile == 0 {
            1.0
        } else {
            [0.0, 1.0, 0.0, 0.25][frame]
        };
        result.envelope[frame * frame_slots + head..frame * frame_slots + end].fill(level);
        let gains = fixed_gains(&bands, &interval, c.factor, c.band, level);
        let mut tones = vec![QmfSlot::zero(); end - head];
        // seed=1 的首调用原样使用生产初始化；其它种子只在测试中预置相同状态表示。
        generate(
            &gains,
            &bands,
            &interval,
            c.factor,
            frame == 0 && c.seed == 1,
            &mut cursor,
            &mut tones,
        )
        .unwrap();
        result.last.push(cursor.previous());
        let mut y = vec![QmfSlot::zero(); end];
        let zeros = vec![QmfSlot::zero(); end - head];
        assemble(
            &gains,
            &bands,
            &interval,
            c.factor,
            frame_slots as u8,
            &zeros,
            &zeros,
            &tones,
            &mut hf,
            &mut y,
        )
        .unwrap();
        let mut q = vec![QmfSlot::zero(); frame_slots];
        combine(
            &vec![QmfSlot::zero(); frame_slots],
            &y,
            3 * c.factor,
            &mut output_delay,
            &mut q,
        )
        .unwrap();
        let mut pcm = vec![0.0; frame_slots * 64];
        synthesise(&q, &mut synthesis, &mut pcm).unwrap();
        result.qmf.extend(q);
        result.pcm.extend(pcm);
    }
    assert_eq!(hf.carried(), 0);
    let zero = vec![QmfSlot::zero(); TAIL];
    let mut tail = vec![0.0; TAIL * 64];
    synthesise(&zero, &mut synthesis, &mut tail).unwrap();
    assert_eq!(synthesis, QmfSynthesisState::new());
    result.qmf.extend(zero);
    result.pcm.extend(tail);
    result
}

/// 独立闭式：在子带中心，四项相位恰好消去 FIR 的逐时隙载波旋转。
/// 只剩按绝对时间卷积的实包络；不读取 SINE_TABLE 或生产调制表。
fn predict(c: Case, envelope: &[f32]) -> Vec<f64> {
    let omega = core::f64::consts::PI * (c.band as f64 + 0.5) / 64.0;
    let sign = if c.band.is_multiple_of(2) { 1.0 } else { -1.0 };
    (0..envelope.len() * 64)
        .map(|n| {
            let mut amplitude = 0.0;
            for j in (n % 64..640.min(n + 1)).step_by(64) {
                let a = f64::from(QMF_WINDOW[j]) * if (j / 128) % 2 == 0 { 1.0 } else { -1.0 };
                amplitude += f64::from(envelope[(n - j) / 64]) * a / 64.0;
            }
            amplitude
                * (omega * (n as f64 - 127.5)
                    + sign * f64::from(c.seed) * core::f64::consts::FRAC_PI_2)
                    .cos()
        })
        .collect()
}

fn verify(c: Case, r: &Run) -> f64 {
    let frame_slots = usize::from(FRAME_ATS * c.factor);
    for (frame, &last) in r.last.iter().enumerate() {
        let end = (frame + 1) * frame_slots
            + if frame + 1 == FRAMES {
                0
            } else {
                usize::from(c.carry * c.factor)
            };
        assert_eq!(usize::from(last), (usize::from(c.seed) + end - 1) % 4);
    }
    for (t, q) in r.qmf.iter().enumerate() {
        for b in 0..64 {
            let sign = if b % 2 == 0 { 1.0 } else { -1.0 };
            let phase = (usize::from(c.seed) + t) % 4;
            // 整数坐标来自四分之一周几何，而非查生产的 SINE_TABLE。
            let re = if phase % 2 == 0 {
                if phase == 0 { 1.0 } else { -1.0 }
            } else {
                0.0
            };
            let im = if phase % 2 == 1 {
                if phase == 1 { sign } else { -sign }
            } else {
                0.0
            };
            let level = if b == c.band { r.envelope[t] } else { 0.0 };
            assert_eq!(
                (q.re[b], q.im[b]),
                (level * re, level * im),
                "band={}, slot={t}",
                c.band
            );
        }
    }
    let polyphase = (0..64)
        .map(|n| {
            (n..640)
                .step_by(64)
                .map(|j| f64::from(QMF_WINDOW[j].abs()))
                .sum::<f64>()
        })
        .fold(0.0, f64::max);
    let bound = 4.0 * f64::from(f32::EPSILON) * polyphase / 64.0;
    let expected = predict(c, &r.envelope);
    let max = r
        .pcm
        .iter()
        .zip(expected)
        .map(|(&a, b)| {
            assert!(a.is_finite() && b.is_finite());
            (f64::from(a) - b).abs()
        })
        .fold(0.0, f64::max);
    assert!(
        max <= bound,
        "band={}, seed={}, max={max}, bound={bound}",
        c.band,
        c.seed
    );
    max
}

#[test]
fn tone_phase_chain_matches_closed_form_across_carry_and_silence() {
    for band in [10, 11, 18, 19, 36, 37, 60, 61] {
        for seed in 0..4 {
            for factor in 1..=2 {
                for profile in 0..2 {
                    let mut baseline: Option<Vec<f32>> = None;
                    for carry in 0..4 {
                        let c = Case {
                            band,
                            seed,
                            factor,
                            carry,
                            profile,
                        };
                        let r = run(c);
                        verify(c, &r);
                        if profile == 0 {
                            if let Some(prior) = &baseline {
                                assert!(
                                    r.pcm
                                        .iter()
                                        .zip(prior)
                                        .all(|(a, b)| a.to_bits() == b.to_bits())
                                );
                            } else {
                                baseline = Some(r.pcm);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn write_floats(path: &Path, values: impl Iterator<Item = f32>) {
    let mut f = std::fs::File::create_new(path).unwrap();
    for v in values {
        assert!(v.is_finite());
        f.write_all(&v.to_le_bytes()).unwrap();
    }
}

#[test]
#[ignore = "requires fresh output on a verified external volume"]
fn export_tone_phase_chains() {
    let path = std::env::var("MACINAC4_TONE_PHASE_OUTPUT").unwrap();
    let root = Path::new(&path);
    std::fs::create_dir(root).unwrap();
    write_floats(&root.join("qwin.f32"), QMF_WINDOW.into_iter());
    let mut log = std::fs::File::create_new(root.join("cases.txt")).unwrap();
    for band in 10..62 {
        for seed in 0..2 {
            for factor in 1..=2 {
                for carry in [0, 3] {
                    for profile in 0..2 {
                        let c = Case {
                            band,
                            seed,
                            factor,
                            carry,
                            profile,
                        };
                        let r = run(c);
                        let error = verify(c, &r);
                        let id = format!("b{band:02}s{seed}f{factor}c{carry}p{profile}");
                        write_floats(&root.join(format!("{id}.pcm.f32")), r.pcm.into_iter());
                        write_floats(
                            &root.join(format!("{id}.qmf.f32")),
                            r.qmf
                                .iter()
                                .flat_map(|s| (0..64).flat_map(move |b| [s.re[b], s.im[b]])),
                        );
                        writeln!(
                            log,
                            "{id} {band} {seed} {factor} {carry} {profile} {} {} {} {} {error}",
                            r.last[0], r.last[1], r.last[2], r.last[3]
                        )
                        .unwrap();
                    }
                }
            }
        }
    }
}
