//! 音调游标的同输入重放：历史字面起点与当前 QMF 起点各配初相 1／0。
//! qmf_origin 直接调用修复后的生产 generate；其余变体仅在测试端代换游标。
//! seed 采用参考的下一项表示；本地 previous 则保存上一项，两者相差一。
//! 只写入显式指定的本地目录，不引入公共参数或修改生产算法。

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
    first_frame: bool,
    factor: u8,
    left: u8,
    seed: u8,
    pattern: u8,
    profile: u8,
    level: f32,
    bands: AspxBandTables,
    interval: AspxInterval,
}

fn parse(line: &str) -> Result<Case, String> {
    let words: Vec<_> = line.split_whitespace().collect();
    if words.len() != 20
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
    let first_frame = flag(2)?;
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
    let seed = byte(14)?;
    let _onset = flag(15)?; // onset 只抑制噪声；保留为参考分支控制。
    let pattern = byte(16)?;
    let profile = byte(18)?;
    let _force_present = flag(19)?;
    let level: f32 = words[17].parse().map_err(|_| "invalid level".to_string())?;
    if group > 4096
        || seed > 3
        || pattern > 5
        || profile > 2
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
        first_frame,
        factor,
        left,
        seed,
        pattern,
        profile,
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
            let absolute = sb + usize::from(case.bands.sbx());
            let active = match case.pattern {
                0 => true,
                1 => absolute % 2 == 0,
                2 => absolute % 2 == 1,
                3 => sb == 0,
                4 => sb + 1 == usize::from(case.bands.num_sb_aspx()),
                _ => false,
            };
            let level = if !active || (case.profile == 1 && env % 2 == 0) {
                0.0
            } else if case.profile == 2 {
                case.level / (1u32 << env) as f32
            } else {
                case.level
            };
            assert!(gains.set_sine_level_for_test(sb, env, level));
        }
    }
    gains
}

fn render(
    case: &Case,
    converted_origin: bool,
    initial_zero: bool,
    previous: &mut ToneCursor,
) -> (Vec<QmfSlot>, u8, u8, bool) {
    let before = previous.previous;
    let mut cursor = *previous;
    if case.first_frame && (!converted_origin || initial_zero) {
        cursor.previous = if initial_zero { 3 } else { 0 };
    }
    if !converted_origin {
        // 仅在测试中复现历史字面式多出的表项，不搜索输出相位。
        // qmf_origin 不注入补偿，直接传递逻辑游标和实际 first_frame。
        let extra = case.left * (case.factor - 1);
        cursor.previous = (cursor.previous + extra) % 4;
    }
    let injected = cursor.previous;
    let call_first = case.first_frame && converted_origin && !initial_zero;
    let last = case
        .interval
        .sig_border(usize::from(case.interval.num_atsg_sig()))
        .expect("last");
    let slots =
        usize::try_from((last - i16::from(case.left)) * i16::from(case.factor)).expect("slots");
    let mut output = vec![QmfSlot::zero(); slots];
    generate(
        &gains(case),
        &case.bands,
        &case.interval,
        case.factor,
        call_first,
        &mut cursor,
        &mut output,
    )
    .expect("generate");
    *previous = cursor;
    (output, before, injected, call_first)
}

#[test]
#[ignore = "需要显式的本地音调原语输入与输出目录"]
fn replay_tone_cursor_checkpoints() {
    let input = std::env::var_os("MACINAC4_TONE_REPLAY_INPUT").expect("input manifest");
    let directory = std::env::var_os("MACINAC4_TONE_REPLAY_OUTPUT").expect("output directory");
    let cases: Vec<_> = std::fs::read_to_string(input)
        .expect("read cases")
        .lines()
        .map(|line| parse(line).unwrap_or_else(|e| panic!("{e}")))
        .collect();
    assert!(!cases.is_empty() && cases.len() <= 4096);
    let root = Path::new(&directory);
    std::fs::create_dir(root).expect("create new output directory with an existing parent");
    let mut log = std::fs::File::create(root.join("results.txt")).expect("result output");
    let mut ids = BTreeSet::new();
    for (mode, converted, initial_zero) in [
        ("literal", false, false),
        ("qmf_origin", true, false),
        ("literal_initial_zero", false, true),
        ("qmf_origin_initial_zero", true, true),
    ] {
        let mut group = None;
        let mut cursor = ToneCursor::new();
        for case in &cases {
            assert!(ids.insert((case.id.clone(), mode)));
            if group != Some(case.group) {
                // 显式状态编码转换，不是对参考相位进行拟合。
                cursor.previous = (case.seed + 3) % 4;
                group = Some(case.group);
            }
            let (values, before, injected, call_first) =
                render(case, converted, initial_zero, &mut cursor);
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
                "{} {} {} {} {} {} {} {} {}",
                case.id,
                mode,
                before,
                injected,
                cursor.previous,
                (cursor.previous + 1) % 4,
                case.bands.num_sb_aspx(),
                values.len(),
                u8::from(call_first)
            )
            .expect("write result");
        }
    }
}

#[test]
fn rejects_bad_tone_replay_geometry_flags_and_levels() {
    for line in [
        "",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 25 0 0 0 1 0 0",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 26 4 0 0 1 0 0",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 26 0 0 6 1 0 0",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 26 0 0 0 NaN 0 0",
        "x 0 0 2 1 16 1 1 6 0 3 0 36 26 0 0 0 1 0 2",
    ] {
        assert!(parse(line).is_err());
    }
}

#[test]
fn production_origin_and_diagnostic_initial_phase_are_independent() {
    let case =
        parse("x 0 1 2 2 16 1 1 6 0 3 0 36 26 0 0 0 1 0 0").unwrap_or_else(|e| panic!("{e}"));
    for (converted, initial_zero, expected) in [
        (false, false, (0.0, -1.0)),
        (true, false, (0.0, 1.0)),
        (false, true, (-1.0, 0.0)),
        (true, true, (1.0, 0.0)),
    ] {
        let (values, before, injected, call_first) =
            render(&case, converted, initial_zero, &mut ToneCursor::new());
        if converted && !initial_zero {
            assert_eq!(before, injected, "生产模式不应注入游标补偿");
            assert!(call_first, "生产模式应实际覆盖 first_frame 分支");
        }
        assert_eq!((values[0].re[36], values[0].im[36]), expected);
        assert_eq!(values[0].im[37], -expected.1);
    }
}
