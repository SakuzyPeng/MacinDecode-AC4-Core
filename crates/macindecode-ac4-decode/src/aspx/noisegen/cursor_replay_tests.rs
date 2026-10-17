//! 噪声游标的同输入重放。qmf_origin 直接调用修复后的生产 generate；literal
//! 只在测试中增加确定的初始游标偏移，重现历史字面式。保留模式名以读取历史记录。
//! 原始表和样本只写入显式指定的本地目录，不随仓库分发。

#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "测试输入在使用下标前严格检查数量、范围与派生布局"
)]

extern crate std;

use super::*;
use crate::aspx::dequant::ScaleFactors;
use crate::aspx::frames::AspxIntervalParams;
use crate::aspx::hfadjust::{EnvelopeEstimate, SinePlacement, SineState, estimate};
use crate::aspx::hfgain::{LimiterMode, adjust};
use crate::aspx::tables::IntervalClass;
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::string::{String, ToString};
use std::vec;
use std::vec::Vec;

struct Case {
    id: String,
    group: u16,
    reset: bool,
    factor: u8,
    left: u8,
    seed: u16,
    level: f32,
    bands: AspxBandTables,
    interval: AspxInterval,
}

fn parse(line: &str) -> Result<Case, String> {
    let words: Vec<_> = line.split_whitespace().collect();
    if words.len() != 17
        || words[0].is_empty()
        || !words[0].bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err("invalid case fields or identity".to_string());
    }
    let number = |i: usize| {
        words[i]
            .parse::<u16>()
            .map_err(|_| "invalid integer".to_string())
    };
    let flag = |i: usize| match number(i)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("invalid flag".to_string()),
    };
    let byte = |i: usize| u8::try_from(number(i)?).map_err(|_| "integer out of range".to_string());
    let group = number(1)?;
    let reset = flag(2)?;
    let factor = byte(3)?;
    let left = byte(4)?;
    let right = byte(5)?;
    let envelopes = byte(6)?;
    let master = flag(7)?;
    let bands = AspxBandTables::derive(master, byte(8)?, byte(9)?, byte(10)?, byte(11)?)
        .map_err(|e| format!("band error: {e:?}"))?;
    if bands.sbx() != byte(12)? || bands.num_sb_aspx() != byte(13)? {
        return Err("captured geometry differs from derived bands".to_string());
    }
    let seed = number(14)?;
    let _onset = flag(15)?; // 发生器总是生成；onset 的消费者掩码由外部比较器独立检查。
    let level: f32 = words[16].parse().map_err(|_| "invalid level".to_string())?;
    if group > 4096
        || seed > 511
        || !matches!(factor, 1 | 2)
        || left > 3
        || !(16..=19).contains(&right)
        || !matches!(envelopes, 1 | 2 | 4)
        || (envelopes != 1 && (left != 0 || right != 16))
        || !level.is_finite()
        || level < 0.0
    {
        return Err("case values out of range".to_string());
    }
    let mut params = AspxIntervalParams::fixfix(envelopes);
    if left != 0 || right != 16 {
        params.int_class = match (left != 0, right != 16) {
            (true, false) => IntervalClass::VarFix,
            (false, true) => IntervalClass::FixVar,
            _ => IntervalClass::VarVar,
        };
        params.var_bord_left = Some(left);
        params.var_bord_right = Some(right - 16);
    }
    let interval = AspxInterval::derive(&params, 16, 1, true, 16)
        .map_err(|e| format!("interval error: {e:?}"))?;
    if interval.sig_border(0) != Some(i16::from(left))
        || interval.sig_border(usize::from(envelopes)) != Some(i16::from(right))
    {
        return Err("derived interval differs".to_string());
    }
    Ok(Case {
        id: words[0].to_string(),
        group,
        reset,
        factor,
        left,
        seed,
        level,
        bands,
        interval,
    })
}

fn gains(case: &Case) -> AdjustedGains {
    let mut sf = ScaleFactors::new();
    sf.fill_for_test(
        usize::from(case.interval.num_atsg_sig()),
        usize::from(case.bands.num_sbg_sig_highres()),
        usize::from(case.bands.num_sbg_noise()),
        1.0,
        1.0,
    );
    let high = [QmfSlot::zero(); 40];
    let mut estimated = EnvelopeEstimate::new();
    estimate(
        &high,
        &case.bands,
        &case.interval,
        &sf,
        &[false; 32],
        SinePlacement::from_params(-1),
        true,
        case.factor,
        &mut SineState::new(),
        &mut estimated,
    )
    .expect("estimate provenance");
    let mut gains = AdjustedGains::new();
    adjust(&estimated, &case.bands, LimiterMode::Off, &mut gains).expect("gain provenance");
    for sb in 0..usize::from(gains.subbands()) {
        for env in 0..usize::from(gains.envelopes()) {
            assert!(gains.set_noise_level_for_test(sb, env, case.level));
        }
    }
    gains
}

fn render(
    case: &Case,
    converted_origin: bool,
    previous: &mut NoiseCursor,
) -> (Vec<QmfSlot>, u16, u16) {
    let before = if case.reset { 0 } else { previous.previous };
    let mut cursor = *previous;
    if !converted_origin {
        // 在测试中复现历史字面式多出的表项；生产路径不注入游标补偿。
        // 不是从参考输出拟合位移；逻辑 before 与注入后的游标分别写出。
        let extra = usize::from(case.bands.num_sb_aspx())
            * usize::from(case.left)
            * usize::from(case.factor - 1);
        cursor.previous = ((usize::from(before) + extra) % 512) as u16;
    }
    let injected = cursor.previous;
    let last = case
        .interval
        .sig_border(usize::from(case.interval.num_atsg_sig()))
        .expect("last border");
    let slots = usize::try_from((last - i16::from(case.left)) * i16::from(case.factor))
        .expect("positive slots");
    let mut output = vec![QmfSlot::zero(); slots];
    generate(
        &gains(case),
        &case.bands,
        &case.interval,
        case.factor,
        converted_origin && case.reset,
        &mut cursor,
        &mut output,
    )
    .expect("generate");
    *previous = cursor;
    (output, before, injected)
}

#[test]
#[ignore = "需要显式的本地噪声原语输入与输出目录"]
fn replay_noise_cursor_checkpoints() {
    let input = std::env::var_os("MACINAC4_NOISE_REPLAY_INPUT").expect("input manifest");
    let directory = std::env::var_os("MACINAC4_NOISE_REPLAY_OUTPUT").expect("output directory");
    let cases: Vec<_> = std::fs::read_to_string(input)
        .expect("read cases")
        .lines()
        .map(|line| parse(line).unwrap_or_else(|e| panic!("{e}")))
        .collect();
    assert!(!cases.is_empty() && cases.len() <= 4096);
    let root = Path::new(&directory);
    std::fs::create_dir(root).expect("create new output directory with an existing parent");
    let mut table_file = std::fs::File::create(root.join("table.f32")).expect("table output");
    for pair in table::ASPX_NOISE {
        for value in pair {
            table_file
                .write_all(&value.to_le_bytes())
                .expect("write table");
        }
    }
    let mut log = std::fs::File::create(root.join("results.txt")).expect("result output");
    let mut ids = BTreeSet::new();
    for (mode, converted) in [("literal", false), ("qmf_origin", true)] {
        let mut group = None;
        let mut cursor = NoiseCursor::new();
        for case in &cases {
            assert!(ids.insert((case.id.clone(), mode)));
            if group != Some(case.group) {
                cursor.previous = case.seed;
                group = Some(case.group);
            }
            let (values, before, injected) = render(case, converted, &mut cursor);
            let mut file = std::fs::File::create(root.join(format!("{}_{}.f32", case.id, mode)))
                .expect("sample output");
            for slot in &values {
                for sb in usize::from(case.bands.sbx())..usize::from(case.bands.sbz()) {
                    for value in [slot.re[sb], slot.im[sb]] {
                        assert!(value.is_finite());
                        file.write_all(&value.to_le_bytes()).expect("write samples");
                    }
                }
            }
            writeln!(
                log,
                "{} {} {} {} {} {} {}",
                case.id,
                mode,
                before,
                injected,
                cursor.previous,
                case.bands.num_sb_aspx(),
                values.len()
            )
            .expect("write result");
        }
    }
}

#[test]
fn rejects_bad_geometry_and_nonfinite_replay_levels() {
    for line in [
        "",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 25 0 0 1",
        "x 0 0 3 1 16 1 1 6 0 3 0 36 26 0 0 1",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 26 512 0 1",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 26 0 0 NaN",
    ] {
        assert!(parse(line).is_err());
    }
}

#[test]
fn production_origin_uses_the_qmf_clock_and_historical_replay_remains_distinct() {
    let case = parse("x 0 0 2 2 16 1 1 6 0 3 0 36 26 511 0 1").unwrap_or_else(|e| panic!("{e}"));
    let mut literal = NoiseCursor { previous: 511 };
    let mut corrected = literal;
    let (a, _, _) = render(&case, false, &mut literal);
    let (b, _, _) = render(&case, true, &mut corrected);
    // 独立计数：28 个实际 QMF 时隙，每时隙 26 项，从 previous=511 继续。
    assert_eq!(corrected.previous(), (511 + 28 * 26) % 512);
    assert_eq!(literal.previous(), (511 + 30 * 26) % 512);
    assert_eq!(b[0].re[36], table::ASPX_NOISE[0][0]);
    assert_eq!(a[0].re[36], table::ASPX_NOISE[52][0]);
}
