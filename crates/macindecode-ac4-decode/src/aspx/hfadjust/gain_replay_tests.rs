//! 增益边界的本地重放工具；作为 hfadjust 的测试子模块接收已转换的检查点。
//!
//! 每条记录是一份独立包络。P95–101 不跨包络携带状态，因此用单包络时间布局
//! 承载矩阵，并显式保存原包络的 sine_onset；不重算上游估计或反量化。
//! 文件不含参考实现布局、地址或媒体。格式为版本头、记录数，随后逐记录的
//! id、五个频带配置值、onset、四份带长度的边界表，及逐子带的七个估计值。
//! 输出逐行给出 id、模式、子带及四个 f32 的十六进制位表示。

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "离线测试夹具严格检查数量和取值后使用有界下标"
)]

extern crate std;

use super::*;
use crate::aspx::frames::AspxIntervalParams;
use crate::aspx::hfgain::{AdjustedGains, LimiterMode, adjust};
use crate::aspx::limiter::LimiterTable;
use crate::aspx::patches::PatchTable;
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::string::{String, ToString};
use std::vec::Vec;

struct Record {
    id: String,
    bands: AspxBandTables,
    patches: PatchTable,
    limiter: LimiterTable,
    estimate: EnvelopeEstimate,
}

fn token<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<&'a str, String> {
    words
        .next()
        .ok_or_else(|| "truncated checkpoint".to_string())
}

fn number<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<u8, String> {
    token(words)?
        .parse()
        .map_err(|_| "invalid integer".to_string())
}

fn flag<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<bool, String> {
    match number(words)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("invalid flag".to_string()),
    }
}

fn level<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<f32, String> {
    let value: f32 = token(words)?
        .parse()
        .map_err(|_| "invalid level".to_string())?;
    if !value.is_finite() || value < 0.0 {
        return Err("nonfinite or negative level".to_string());
    }
    Ok(value)
}

fn borders<'a>(
    words: &mut impl Iterator<Item = &'a str>,
    expected: impl Iterator<Item = u8>,
) -> Result<(), String> {
    let count = number(words)?;
    let expected: Vec<_> = expected.collect();
    if usize::from(count) != expected.len() {
        return Err("border count mismatch".to_string());
    }
    for border in expected {
        if number(words)? != border {
            return Err("border value mismatch".to_string());
        }
    }
    Ok(())
}

fn parse(text: &str) -> Result<Vec<Record>, String> {
    let mut words = text.split_whitespace();
    if token(&mut words)? != "MACINAC4_GAIN_REPLAY" || number(&mut words)? != 1 {
        return Err("unsupported checkpoint format".to_string());
    }
    let count: usize = token(&mut words)?
        .parse()
        .map_err(|_| "invalid count".to_string())?;
    if count == 0 || count > 4096 {
        return Err("record count out of range".to_string());
    }
    let mut ids = BTreeSet::new();
    let mut records = Vec::new();
    for _ in 0..count {
        let id = token(&mut words)?.to_string();
        if !ids.insert(id.clone()) {
            return Err("duplicate record identity".to_string());
        }
        let master = flag(&mut words)?;
        let start = number(&mut words)?;
        let stop = number(&mut words)?;
        let noise = number(&mut words)?;
        let xover = number(&mut words)?;
        let onset = flag(&mut words)?;
        let bands = AspxBandTables::derive(master, start, stop, noise, xover)
            .map_err(|e| format!("invalid bands: {e:?}"))?;
        let patches = PatchTable::derive(&bands, false, master)
            .map_err(|e| format!("invalid patches: {e:?}"))?;
        let limiter = LimiterTable::derive(&bands, &patches)
            .map_err(|e| format!("invalid limiter: {e:?}"))?;
        borders(
            &mut words,
            (0..=usize::from(bands.num_sbg_sig_lowres()))
                .filter_map(|i| bands.sig_lowres_border(i)),
        )?;
        borders(
            &mut words,
            (0..=usize::from(bands.num_sbg_sig_highres()))
                .filter_map(|i| bands.sig_highres_border(i)),
        )?;
        borders(
            &mut words,
            (0..=usize::from(bands.num_sbg_noise())).filter_map(|i| bands.noise_border(i)),
        )?;
        borders(
            &mut words,
            (0..=usize::from(limiter.count())).filter_map(|i| limiter.border(i)),
        )?;
        let mut estimate = EnvelopeEstimate::new();
        estimate.source_bands = bands;
        estimate.source_interval =
            AspxInterval::derive(&AspxIntervalParams::fixfix(1), 16, 1, true, 16)
                .map_err(|e| format!("invalid interval: {e:?}"))?;
        estimate.source_num_ts_in_ats = 2;
        estimate.subbands = bands.num_sb_aspx();
        estimate.envelopes = 1;
        estimate.prev_at_end = onset;
        for sb in 0..usize::from(estimate.subbands) {
            estimate.est_sig[sb][0] = level(&mut words)?;
            estimate.scf_sig[sb][0] = level(&mut words)?;
            if estimate.scf_sig[sb][0] == 0.0 {
                return Err("signal target must be positive".to_string());
            }
            estimate.scf_noise[sb][0] = level(&mut words)?;
            estimate.sine_idx[sb][0] = flag(&mut words)?;
            estimate.sine_area[sb][0] = flag(&mut words)?;
            estimate.noise_lev[sb][0] = level(&mut words)?;
            estimate.sine_lev[sb][0] = level(&mut words)?;
        }
        records.push(Record {
            id,
            bands,
            patches,
            limiter,
            estimate,
        });
    }
    if words.next().is_some() {
        return Err("trailing checkpoint data".to_string());
    }
    Ok(records)
}

#[test]
#[ignore = "需要本地已验证的物理量检查点；不随仓库分发"]
fn replay_gain_checkpoints() {
    let input = std::env::var_os("MACINAC4_GAIN_REPLAY_INPUT").expect("missing checkpoint input");
    let output = std::env::var_os("MACINAC4_GAIN_REPLAY_OUTPUT").expect("missing replay output");
    let text = std::fs::read_to_string(input).expect("read checkpoint");
    let records = parse(&text).expect("validate checkpoint");
    let mut lines = Vec::new();
    for r in records {
        for (name, mode) in [
            ("off", LimiterMode::Off),
            (
                "boost",
                LimiterMode::OffWithBoost {
                    table: &r.limiter,
                    patches: &r.patches,
                },
            ),
            (
                "on",
                LimiterMode::On {
                    table: &r.limiter,
                    patches: &r.patches,
                },
            ),
        ] {
            let mut out = AdjustedGains::new();
            adjust(&r.estimate, &r.bands, mode, &mut out).expect("gain replay");
            for sb in 0..usize::from(r.bands.num_sb_aspx()) {
                let values = [
                    out.signal_gain(sb, 0),
                    out.noise_level(sb, 0),
                    out.sine_level(sb, 0),
                    out.boost_factor(sb, 0),
                ]
                .map(|x| x.expect("valid output index"));
                assert!(values.iter().all(|v| v.is_finite()));
                writeln!(
                    lines,
                    "{} {} {} {:08x} {:08x} {:08x} {:08x}",
                    r.id,
                    name,
                    sb,
                    values[0].to_bits(),
                    values[1].to_bits(),
                    values[2].to_bits(),
                    values[3].to_bits()
                )
                .expect("format result");
            }
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .expect("create fresh replay output");
    file.write_all(&lines).expect("write replay output");
}

#[test]
fn rejects_truncated_invalid_or_nonfinite_checkpoint_fields() {
    for text in [
        "",
        "MACINAC4_GAIN_REPLAY 2 1",
        "MACINAC4_GAIN_REPLAY 1 0",
        "MACINAC4_GAIN_REPLAY 1 4097",
        "MACINAC4_GAIN_REPLAY 1 1 a 2",
    ] {
        assert!(parse(text).is_err());
    }
    for text in ["NaN", "inf", "-1", "1e100"] {
        assert!(level(&mut text.split_whitespace()).is_err());
    }
}

#[test]
fn rejects_mismatched_border_source() {
    assert!(
        borders(
            &mut "3 36 44 56".split_whitespace(),
            [36, 44, 62].into_iter()
        )
        .is_err()
    );
    assert!(borders(&mut "2 36 44".split_whitespace(), [36, 44, 56].into_iter()).is_err());
    assert!(
        borders(
            &mut "3 36 44 56".split_whitespace(),
            [36, 44, 56].into_iter()
        )
        .is_ok()
    );
}

#[test]
fn accepts_complete_record_and_rejects_trailing_or_duplicate_records() {
    let bands = AspxBandTables::derive(false, 0, 0, 0, 0).expect("bands");
    let patches = PatchTable::derive(&bands, false, false).expect("patches");
    let limiter = LimiterTable::derive(&bands, &patches).expect("limiter");
    let mut body = "fixture 0 0 0 0 0 1\n".to_string();
    let tables: [Vec<_>; 4] = [
        (0..=usize::from(bands.num_sbg_sig_lowres()))
            .filter_map(|i| bands.sig_lowres_border(i))
            .collect(),
        (0..=usize::from(bands.num_sbg_sig_highres()))
            .filter_map(|i| bands.sig_highres_border(i))
            .collect(),
        (0..=usize::from(bands.num_sbg_noise()))
            .filter_map(|i| bands.noise_border(i))
            .collect(),
        (0..=usize::from(limiter.count()))
            .filter_map(|i| limiter.border(i))
            .collect(),
    ];
    for table in tables {
        body.push_str(&format!("{} ", table.len()));
        for border in table {
            body.push_str(&format!("{border} "));
        }
        body.push('\n');
    }
    for _ in 0..bands.num_sb_aspx() {
        body.push_str("2 1 0 0 0 0 0\n");
    }
    let text = format!("MACINAC4_GAIN_REPLAY 1 1\n{body}");
    let records = parse(&text).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].estimate.sine_onset(0), Some(true));
    assert!(parse(&format!("{text}extra")).is_err());
    assert!(parse(&format!("MACINAC4_GAIN_REPLAY 1 2\n{body}{body}")).is_err());
}
