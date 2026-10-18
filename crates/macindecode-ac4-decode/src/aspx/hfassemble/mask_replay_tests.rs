//! 固定调整后增益，重放生产门控及历史无条件注噪对照。
//! production 使用原噪声与预算掩码；literal 在测试端强制开启消费，
//! masked 再清零噪声副本以保留修复前的独立诊断，生成器只运行一次。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "离线输入先验证有限值、边界、数量与频带来源，再使用有界索引"
)]
extern crate std;
use super::*;
use crate::aspx::dequant::ScaleFactors;
use crate::aspx::frames::AspxIntervalParams;
use crate::aspx::hfadjust::{EnvelopeEstimate, SinePlacement, SineState, estimate};
use crate::aspx::hfgain::{AdjustedGains, LimiterMode, adjust};
use crate::aspx::noisegen::{NoiseCursor, generate as generate_noise};
use crate::aspx::tonegen::{ToneCursor, generate as generate_tone};
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

struct Case {
    id: String,
    bands: AspxBandTables,
    interval: AspxInterval,
    factor: u8,
    onset_mask: u8,
    pattern: u8,
    sine: f32,
    noise: f32,
    zero_hf: bool,
}

fn parse(line: &str) -> Result<Case, String> {
    let w: Vec<_> = line.split_whitespace().collect();
    if w.len() != 15
        || w[0].is_empty()
        || w[0].len() > 63
        || !w[0].bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err("invalid fields or identity".to_string());
    }
    let num = |i: usize| w[i].parse::<u8>().map_err(|_| "invalid byte".to_string());
    let flag = |i: usize| match num(i)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("invalid flag".to_string()),
    };
    let bands = AspxBandTables::derive(flag(1)?, num(2)?, num(3)?, num(4)?, num(5)?)
        .map_err(|e| format!("bands: {e:?}"))?;
    if bands.sbx() != num(6)? || bands.num_sb_aspx() != num(7)? {
        return Err("band source".to_string());
    }
    let factor = num(8)?;
    let envelopes = num(9)?;
    let onset_mask = num(10)?;
    let pattern = num(11)?;
    let sine = w[12].parse::<f32>().map_err(|_| "sine level".to_string())?;
    let noise = w[13]
        .parse::<f32>()
        .map_err(|_| "noise level".to_string())?;
    let zero_hf = flag(14)?;
    if !matches!(factor, 1 | 2)
        || !matches!(envelopes, 1 | 2 | 4)
        || onset_mask >= (1 << envelopes)
        || pattern > 6
        || !sine.is_finite()
        || sine <= 0.0
        || !noise.is_finite()
        || noise < 0.0
    {
        return Err("case range".to_string());
    }
    let interval = AspxInterval::derive(&AspxIntervalParams::fixfix(envelopes), 16, 1, true, 16)
        .map_err(|e| format!("interval: {e:?}"))?;
    Ok(Case {
        id: w[0].to_string(),
        bands,
        interval,
        factor,
        onset_mask,
        pattern,
        sine,
        noise,
        zero_hf,
    })
}

fn sine_level(c: &Case, band: usize, env: usize) -> f32 {
    let lo = usize::from(c.bands.sbx());
    let n = usize::from(c.bands.num_sb_aspx());
    let enabled = match c.pattern {
        0 => false,
        1 => true,
        2 => band.is_multiple_of(2),
        3 => !band.is_multiple_of(2),
        4 => band == lo,
        5 => band == lo + n - 1,
        6 => (band - lo + env).is_multiple_of(2),
        _ => unreachable!(),
    };
    if enabled { c.sine } else { 0.0 }
}

fn fixed_gains(c: &Case) -> AdjustedGains {
    let mut sf = ScaleFactors::new();
    sf.fill_for_test(
        usize::from(c.interval.num_atsg_sig()),
        usize::from(c.bands.num_sbg_sig_highres()),
        usize::from(c.bands.num_sbg_noise()),
        1.0,
        1.0,
    );
    let mut estimated = EnvelopeEstimate::new();
    estimate(
        &[QmfSlot::zero(); 38],
        &c.bands,
        &c.interval,
        &sf,
        &[false; 64],
        SinePlacement::from_params(-1),
        true,
        c.factor,
        &mut SineState::new(),
        &mut estimated,
    )
    .unwrap();
    let mut g = AdjustedGains::new();
    adjust(&estimated, &c.bands, LimiterMode::Off, &mut g).unwrap();
    for e in 0..usize::from(g.envelopes()) {
        for b in 0..usize::from(g.subbands()) {
            assert!(g.set_signal_gain_for_test(b, e, 1.25));
            assert!(g.set_noise_level_for_test(b, e, c.noise));
            assert!(g.set_sine_level_for_test(
                b,
                e,
                sine_level(c, b + usize::from(c.bands.sbx()), e)
            ));
            // 这里是调整后增益的受控入口；P99 的真实估计来源另由 hfgain 测试覆盖。
            assert!(g.set_noise_enabled_for_test(
                b,
                e,
                c.onset_mask & (1 << e) == 0
                    && sine_level(c, b + usize::from(c.bands.sbx()), e) == 0.0,
            ));
        }
    }
    g
}

fn save(path: &Path, q: &[QmfSlot]) {
    let mut f = std::fs::File::create_new(path).unwrap();
    for slot in q {
        for b in 0..64 {
            for value in [slot.re[b], slot.im[b]] {
                assert!(value.is_finite());
                f.write_all(&value.to_le_bytes()).unwrap();
            }
        }
    }
}

#[test]
#[ignore = "requires explicit local mask fixtures"]
fn replay_noise_mask_cases() {
    let input = std::env::var("MACINAC4_MASK_REPLAY_INPUT").unwrap();
    let output = std::env::var("MACINAC4_MASK_REPLAY_OUTPUT").unwrap();
    let output = Path::new(&output);
    std::fs::create_dir(output).unwrap();
    let mut log = std::fs::File::create_new(output.join("states.txt")).unwrap();
    let mut ids = BTreeSet::new();
    for line in std::fs::read_to_string(input).unwrap().lines() {
        let c = parse(line).expect("validated case");
        assert!(ids.insert(c.id.clone()));
        let slots = usize::from(c.factor) * 16;
        let lo = usize::from(c.bands.sbx());
        let n = usize::from(c.bands.num_sb_aspx());
        let g = fixed_gains(&c);
        let mut q = vec![QmfSlot::zero(); slots];
        if !c.zero_hf {
            for (t, slot) in q.iter_mut().enumerate() {
                for b in lo..lo + n {
                    slot.re[b] = 0.125 + (t as f32 % 5.0 - 2.0) * 0.03125 + b as f32 * 0.00390625;
                    slot.im[b] = (b as f32 % 3.0 - 1.0) * 0.0625;
                }
            }
        }
        let mut noise = vec![QmfSlot::zero(); slots];
        let mut tone = vec![QmfSlot::zero(); slots];
        let mut nc = NoiseCursor::new();
        let mut tc = ToneCursor::new();
        generate_noise(
            &g,
            &c.bands,
            &c.interval,
            c.factor,
            true,
            &mut nc,
            &mut noise,
        )
        .unwrap();
        generate_tone(
            &g,
            &c.bands,
            &c.interval,
            c.factor,
            true,
            &mut tc,
            &mut tone,
        )
        .unwrap();
        let mut masked = noise.clone();
        for (t, slot) in masked.iter_mut().enumerate() {
            let env = t * usize::from(g.envelopes()) / slots;
            for b in lo..lo + n {
                if c.onset_mask & (1 << env) != 0 || sine_level(&c, b, env) > 0.0 {
                    slot.re[b] = 0.0;
                    slot.im[b] = 0.0;
                }
            }
        }
        let mut legacy = g.clone();
        for e in 0..usize::from(g.envelopes()) {
            for b in 0..n {
                assert!(legacy.set_noise_enabled_for_test(b, e, true));
            }
        }
        for (mode, gains, input) in [
            ("literal", &legacy, &noise),
            ("masked", &legacy, &masked),
            ("production", &g, &noise),
        ] {
            let mut out = vec![QmfSlot::zero(); slots];
            assemble(
                gains,
                &c.bands,
                &c.interval,
                c.factor,
                slots as u8,
                &q,
                input,
                &tone,
                &mut HfDelay::new(),
                &mut out,
            )
            .unwrap();
            save(&output.join(format!("{}.{mode}.f32", c.id)), &out);
        }
        writeln!(log, "{} {} {}", c.id, nc.previous(), tc.previous()).unwrap();
    }
    assert!(!ids.is_empty());
}

#[test]
fn validates_mask_geometry_levels_and_frequency_source() {
    let good = "m0 1 6 1 3 0 36 20 2 4 10 6 1 1 0";
    assert!(parse(good).is_ok());
    for input in [
        "",
        "../m0 1 6 1 3 0 36 20 2 4 10 6 1 1 0",
        "m0 1 6 1 3 0 35 20 2 4 10 6 1 1 0",
        "m0 1 6 1 3 0 36 20 2 3 7 6 1 1 0",
        "m0 1 6 1 3 0 36 20 2 4 16 6 1 1 0",
        "m0 1 6 1 3 0 36 20 2 4 10 7 1 1 0",
        "m0 1 6 1 3 0 36 20 2 4 10 6 NaN 1 0",
        "m0 1 6 1 3 0 36 20 2 4 10 6 1 -1 0",
    ] {
        assert!(parse(input).is_err());
    }
}

#[test]
fn sine_pattern_uses_absolute_parity_and_changes_between_envelopes() {
    let mut c = parse("m0 1 6 0 3 5 47 15 2 4 10 2 1 1 0").unwrap();
    assert!(sine_level(&c, 47, 0) == 0.0 && sine_level(&c, 48, 0) > 0.0);
    c.pattern = 6;
    assert!(sine_level(&c, 47, 0) > 0.0 && sine_level(&c, 47, 1) == 0.0);
    assert!(sine_level(&c, 48, 0) == 0.0 && sine_level(&c, 48, 1) > 0.0);
}

#[test]
fn production_mask_keeps_carryover_and_both_clocks_continuous() {
    use crate::aspx::tables::IntervalClass;
    let mut c = parse("m0 1 6 1 3 0 36 20 2 2 2 0 1 1 0").unwrap();
    let mut p = AspxIntervalParams::fixfix(2);
    p.int_class = IntervalClass::FixVar;
    p.var_bord_right = Some(1);
    p.num_rel_right = 1;
    p.rel_bord_right[0] = 2;
    c.interval = AspxInterval::derive(&p, 16, 1, true, 16).unwrap();
    assert_eq!(c.interval.sig_border(1), Some(15));
    assert_eq!(c.interval.sig_border(2), Some(17));
    let lo = usize::from(c.bands.sbx());
    let hi = usize::from(c.bands.sbz());
    let mut delay = HfDelay::new();
    let mut cursor = NoiseCursor::new();
    let mut tone_cursor = ToneCursor::new();
    for frame in 0..2 {
        if frame == 1 {
            let mut p = AspxIntervalParams::fixfix(1);
            p.int_class = IntervalClass::VarFix;
            p.var_bord_left = Some(1);
            c.interval = AspxInterval::derive(&p, 16, 1, true, 16).unwrap();
        }
        let total = if frame == 0 { 34 } else { 32 };
        let head = if frame == 0 { 0 } else { 2 };
        let body = total - head;
        let gains = fixed_gains(&c);
        let mut q = vec![QmfSlot::zero(); body];
        for slot in &mut q {
            for b in lo..hi {
                slot.re[b] = if frame == 0 { 1.0 } else { 2.0 };
            }
        }
        let mut noise = vec![QmfSlot::zero(); body];
        generate_noise(
            &gains,
            &c.bands,
            &c.interval,
            2,
            frame == 0,
            &mut cursor,
            &mut noise,
        )
        .unwrap();
        assert!(noise.iter().all(|slot| slot.re[lo] != 0.0));
        let mut tone = vec![QmfSlot::zero(); body];
        generate_tone(
            &gains,
            &c.bands,
            &c.interval,
            2,
            frame == 0,
            &mut tone_cursor,
            &mut tone,
        )
        .unwrap();
        let mut out = vec![QmfSlot::zero(); total];
        assemble(
            &gains,
            &c.bands,
            &c.interval,
            2,
            32,
            &q,
            &noise,
            &tone,
            &mut delay,
            &mut out,
        )
        .unwrap();
        let inherited = if frame == 0 { &out[32..34] } else { &out[..2] };
        for slot in inherited {
            for b in lo..hi {
                assert_eq!(slot.re[b].to_bits(), 1.25f32.to_bits());
                assert_eq!(slot.im[b].to_bits(), 0.0f32.to_bits());
            }
        }
    }
    // 两帧共生成 34+30 个时隙，每时隙 20 子带；抑制四格也不减少消费。
    assert_eq!(cursor.previous(), 256);
    assert_eq!(tone_cursor.previous(), 0);
    assert_eq!(delay.carried, 0);
}

#[test]
fn production_mask_selects_each_envelope_and_band_without_suppressing_signal_or_tone() {
    let c = parse("m0 1 6 0 3 5 47 15 2 4 10 6 1 1 0").unwrap();
    let g = fixed_gains(&c);
    let mut q = vec![QmfSlot::zero(); 32];
    let mut noise = q.clone();
    let mut tone = q.clone();
    for t in 0..32 {
        for b in 47..62 {
            q[t].re[b] = 2.0;
            q[t].im[b] = -2.0;
            noise[t].re[b] = 8.0;
            noise[t].im[b] = -4.0;
            if (b - 47 + t / 8) % 2 == 0 {
                tone[t].re[b] = 1.0;
                tone[t].im[b] = -1.0;
            }
        }
    }
    let mut out = vec![QmfSlot::zero(); 32];
    assemble(
        &g,
        &c.bands,
        &c.interval,
        2,
        32,
        &q,
        &noise,
        &tone,
        &mut HfDelay::new(),
        &mut out,
    )
    .unwrap();
    for (t, slot) in out.iter().enumerate() {
        for b in 47..62 {
            let expected = match (t / 8, (b - 47) % 2) {
                (0 | 2, 1) => (10.5, -6.5),             // 普通无音调：信号 + 噪声。
                (0 | 2, 0) | (1 | 3, 1) => (3.5, -3.5), // 信号 + 音调。
                (1 | 3, 0) => (2.5, -2.5),              // onset 无音调：只留信号。
                _ => unreachable!(),
            };
            assert_eq!((slot.re[b], slot.im[b]), expected, "t={t}, b={b}");
        }
    }
}
