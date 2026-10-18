//! 固定 HF 检查点的 P90 重放。标度和正弦使用占位值，只导出实际能量估计。
//! 后续增益对照另用已有 gain_replay_tests 注入真实目标和控制，不把占位值当真实元数据。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "测试入口先核对矩阵长度、有限值、频带来源和有界时间布局"
)]
extern crate std;
use super::*;
use crate::aspx::frames::AspxIntervalParams;
use crate::aspx::tables::IntervalClass;
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::string::{String, ToString};
use std::vec::Vec;

struct Case {
    id: String,
    bands: AspxBandTables,
    interval: AspxInterval,
    interpolation: bool,
}

fn next<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<&'a str, String> {
    words.next().ok_or_else(|| "truncated case".to_string())
}
fn number<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<u8, String> {
    next(words)?.parse().map_err(|_| "invalid byte".to_string())
}
fn flag<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<bool, String> {
    match number(words)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("invalid flag".to_string()),
    }
}
fn parse(line: &str) -> Result<Case, String> {
    let mut w = line.split_whitespace();
    let id = next(&mut w)?.to_string();
    if id.len() > 63 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("invalid identity".to_string());
    }
    let master = flag(&mut w)?;
    let bands = AspxBandTables::derive(
        master,
        number(&mut w)?,
        number(&mut w)?,
        number(&mut w)?,
        number(&mut w)?,
    )
    .map_err(|e| format!("bands: {e:?}"))?;
    if number(&mut w)? != bands.sbx() || number(&mut w)? != bands.sbz() {
        return Err("band endpoints".to_string());
    }
    let interpolation = flag(&mut w)?;
    let envelopes = number(&mut w)?;
    if !(1..=4).contains(&envelopes) {
        return Err("envelope count".to_string());
    }
    let mut borders = Vec::new();
    for _ in 0..=envelopes {
        borders.push(number(&mut w)?);
    }
    if borders[0] > 3
        || !(16..=19).contains(&borders[usize::from(envelopes)])
        || borders.windows(2).any(|p| p[0] >= p[1])
    {
        return Err("time borders".to_string());
    }
    let mut params = AspxIntervalParams::fixfix(envelopes);
    params.int_class = IntervalClass::VarVar;
    params.var_bord_left = Some(borders[0]);
    params.var_bord_right = Some(borders[usize::from(envelopes)] - 16);
    params.num_rel_left = envelopes - 1;
    for i in 0..usize::from(envelopes - 1) {
        let step = borders[i + 1] - borders[i];
        if !(2..=8).contains(&step) || !step.is_multiple_of(2) {
            return Err("unsupported relative border".to_string());
        }
        params.rel_bord_left[i] = step;
    }
    for i in 0..usize::from(envelopes) {
        params.freq_res[i] = flag(&mut w)?;
    }
    let interval =
        AspxInterval::derive(&params, 16, 0, true, 16).map_err(|e| format!("interval: {e:?}"))?;
    for (i, &b) in borders.iter().enumerate() {
        if interval.sig_border(i) != Some(i16::from(b)) {
            return Err("derived time differs".to_string());
        }
    }
    for high in [false, true] {
        let (groups, border) = signal_tiling(&bands, high);
        if usize::from(number(&mut w)?) != groups + 1 {
            return Err("frequency border count".to_string());
        }
        for i in 0..=groups {
            if number(&mut w)? != border(i) {
                return Err("frequency border source".to_string());
            }
        }
    }
    if w.next().is_some() {
        return Err("trailing case data".to_string());
    }
    Ok(Case {
        id,
        bands,
        interval,
        interpolation,
    })
}

fn run(case: &Case, q: &[QmfSlot]) -> EnvelopeEstimate {
    let mut sf = ScaleFactors::new();
    sf.fill_for_test(
        usize::from(case.interval.num_atsg_sig()),
        usize::from(case.bands.num_sbg_sig_highres()),
        usize::from(case.bands.num_sbg_noise()),
        1.0,
        0.0,
    );
    let first = usize::try_from(case.interval.sig_border(0).unwrap()).unwrap() * 2;
    let mut out = EnvelopeEstimate::new();
    estimate(
        &q[first..],
        &case.bands,
        &case.interval,
        &sf,
        &[false; 64],
        SinePlacement::from_params(-1),
        case.interpolation,
        2,
        &mut SineState::new(),
        &mut out,
    )
    .expect("production estimate");
    out
}

#[test]
#[ignore = "requires normalized fixed HF inputs; raw media and native layouts stay local"]
fn replay_envelope_estimates() {
    let data = std::env::var("MACINAC4_ESTIMATE_REPLAY_DATA").expect("input directory");
    let output = std::env::var("MACINAC4_ESTIMATE_REPLAY_OUTPUT").expect("fresh output file");
    let data = Path::new(&data);
    let mut out = std::fs::File::create_new(output).unwrap();
    let mut ids = BTreeSet::new();
    for line in std::fs::read_to_string(data.join("cases.txt"))
        .unwrap()
        .lines()
    {
        let c = parse(line).expect("validated case");
        assert!(ids.insert(c.id.clone()));
        let raw = std::fs::read(data.join(format!("{}.f32", c.id))).unwrap();
        assert_eq!(raw.len(), 40 * 64 * 2 * 4);
        let values: Vec<_> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect();
        assert!(values.iter().all(|v| v.is_finite()));
        let q: Vec<_> = values
            .as_chunks::<128>()
            .0
            .iter()
            .map(|v| {
                let mut slot = QmfSlot::zero();
                for b in 0..64 {
                    slot.re[b] = v[2 * b];
                    slot.im[b] = v[2 * b + 1];
                }
                slot
            })
            .collect();
        let estimate = run(&c, &q);
        for env in 0..usize::from(estimate.envelopes()) {
            for b in 0..usize::from(estimate.subbands()) {
                let value = estimate.estimated(b, env).unwrap();
                assert!(value.is_finite() && value >= 0.0);
                writeln!(out, "{} {env} {b} {:08x}", c.id, value.to_bits()).unwrap();
            }
        }
    }
    assert!(!ids.is_empty());
}

fn assembly_onset_observation(onset: bool) -> ([QmfSlot; 32], [QmfSlot; 32], u16) {
    use crate::aspx::hfassemble::{HfDelay, assemble};
    use crate::aspx::hfgain::{AdjustedGains, LimiterMode, adjust};
    use crate::aspx::noisegen::{NoiseCursor, generate};

    let case = fixture(true);
    let q = [QmfSlot::zero(); 32];
    let mut sf = ScaleFactors::new();
    sf.fill_for_test(
        1,
        usize::from(case.bands.num_sbg_sig_highres()),
        usize::from(case.bands.num_sbg_noise()),
        2.0,
        1.0,
    );
    let mut sines = SineState::new();
    sines.placement_at_end = onset;
    let mut estimated = EnvelopeEstimate::new();
    estimate(
        &q,
        &case.bands,
        &case.interval,
        &sf,
        &[false; 64],
        SinePlacement::from_params(-1),
        true,
        2,
        &mut sines,
        &mut estimated,
    )
    .unwrap();
    assert_eq!(estimated.sine_onset(0), Some(onset));
    let mut gains = AdjustedGains::new();
    adjust(&estimated, &case.bands, LimiterMode::Off, &mut gains).unwrap();
    let mut noise = [QmfSlot::zero(); 32];
    let mut cursor = NoiseCursor::new();
    generate(
        &gains,
        &case.bands,
        &case.interval,
        2,
        true,
        &mut cursor,
        &mut noise,
    )
    .unwrap();
    let mut out = [QmfSlot::zero(); 32];
    assemble(
        &gains,
        &case.bands,
        &case.interval,
        2,
        32,
        &q,
        &noise,
        &q,
        &mut HfDelay::new(),
        &mut out,
    )
    .unwrap();
    (out, noise, cursor.previous())
}

#[test]
fn onset_budget_reaches_assembly_without_changing_generated_noise_or_clock() {
    let (ordinary, ordinary_noise, ordinary_cursor) = assembly_onset_observation(false);
    let (onset, onset_noise, onset_cursor) = assembly_onset_observation(true);
    assert_eq!(ordinary, ordinary_noise);
    assert_eq!(ordinary_noise, onset_noise);
    assert!(ordinary_noise.iter().any(|slot| slot.re[36] != 0.0));
    assert_eq!(onset, [QmfSlot::zero(); 32]);
    assert_eq!((ordinary_cursor, onset_cursor), (128, 128));
}

#[test]
#[ignore = "diagnostic export of current production onset behavior"]
fn replay_assembly_onset_observation() {
    let directory = std::env::var("MACINAC4_ASSEMBLY_OBSERVATION_OUTPUT").unwrap();
    let directory = Path::new(&directory);
    std::fs::create_dir(directory).unwrap();
    for onset in [false, true] {
        let (out, _, _) = assembly_onset_observation(onset);
        let path = directory.join(format!("onset{}.f32", u8::from(onset)));
        let mut file = std::fs::File::create_new(path).unwrap();
        for slot in out {
            for b in 0..64 {
                for value in [slot.re[b], slot.im[b]] {
                    assert!(value.is_finite());
                    file.write_all(&value.to_le_bytes()).unwrap();
                }
            }
        }
    }
}

fn fixture(interpolation: bool) -> Case {
    let bands = AspxBandTables::derive(true, 6, 1, 3, 0).unwrap();
    let interval = AspxInterval::derive(&AspxIntervalParams::fixfix(1), 16, 1, true, 16).unwrap();
    Case {
        id: "fixture".to_string(),
        bands,
        interval,
        interpolation,
    }
}

#[test]
fn production_estimate_preserves_time_average_and_group_energy() {
    let mut q = [QmfSlot::zero(); 32];
    let case = fixture(true);
    let lo = usize::from(case.bands.sbx());
    for (t, slot) in q.iter_mut().enumerate() {
        slot.re[lo] = if t < 16 { 2.0 } else { 4.0 };
    }
    let per_band = run(&case, &q);
    assert_eq!(per_band.estimated(0, 0), Some(10.0));
    let grouped = run(&fixture(false), &q);
    let width = usize::from(case.bands.sig_lowres_border(1).unwrap()) - lo;
    assert!(width > 1);
    for b in 0..width {
        assert_eq!(grouped.estimated(b, 0), Some(10.0 / width as f32));
    }
    q[0].re[lo] = 8.0;
    assert_ne!(run(&case, &q).estimated(0, 0), per_band.estimated(0, 0));
}

#[test]
fn rejects_invalid_manifest_geometry_and_sources() {
    for text in [
        "",
        "../x",
        "x 2",
        "x 1 6 1 3 0 35 56",
        "x 1 6 1 3 0 36 56 2",
    ] {
        assert!(parse(text).is_err());
    }
    let bands = fixture(true).bands;
    let mut line = "x 1 6 1 3 0 36 56 1 1 0 16 0".to_string();
    for high in [false, true] {
        let (n, b) = signal_tiling(&bands, high);
        line.push_str(&format!(" {}", n + 1));
        for i in 0..=n {
            line.push_str(&format!(" {}", b(i)));
        }
    }
    assert!(parse(&line).is_ok());
    assert!(parse(&(line.clone() + " 1")).is_err());
    assert!(parse(&line.replace(" 0 16 0", " 4 16 0")).is_err());
    assert!(parse(&line.replacen("36", "35", 1)).is_err());
}
