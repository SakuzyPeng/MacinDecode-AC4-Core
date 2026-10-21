//! ADR-0024 的独立生产回归与历史观察出口；不通过参考差分得分决定规则。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试只遍历已验证的 32 时隙和 64 子带"
)]
use super::*;
use std::boxed::Box;
use std::io::Write;

struct Observation {
    crossover: u8,
    frequency: f64,
    low_energy: f64,
    sec_energy: f64,
    max_extra: f32,
}

fn full_pipeline_observation() -> Observation {
    let bands = hf_bands();
    let modes = chirp_modes(&bands);
    let (params, interval) = split_interval(16);
    let framing = framing(params, interval, false, false);
    let data = envelope_data(&bands, 0);
    let mut state = Box::new(AspxChannelState::new());
    let mut ws = Box::new(AspxWorkspace::new());
    let mut intermediate = AspxIntermediates::default();
    let frequency = (f64::from(bands.sbx()) - 0.125) * core::f64::consts::PI / 64.0;
    for frame in 0..3 {
        let pcm: Vec<_> = (0..2048)
            .map(|i| (16.0 * (frequency * ((frame * 2048 + i) as f64)).cos()) as f32)
            .collect();
        frame_with_scale_factors_qmf(
            &pcm,
            &bands,
            hf_params(&modes),
            EnvelopeInput {
                framing: &framing,
                data: &data,
            },
            adjust_params(&bands),
            &mut state,
            &mut ws,
            &mut intermediate,
        )
        .unwrap();
    }
    let (mut max_extra, mut low_energy, mut sec_energy) = (0.0f32, 0.0f64, 0.0f64);
    for t in 0..32 {
        for b in 0..64 {
            for (actual, sec) in [
                (ws.q_out[t].re[b], ws.y[t].re[b]),
                (ws.q_out[t].im[b], ws.y[t].im[b]),
            ] {
                assert!(actual.is_finite() && sec.is_finite());
                if b < usize::from(bands.sbx()) {
                    low_energy += f64::from(actual).powi(2);
                } else {
                    max_extra = max_extra.max((actual - sec).abs());
                    sec_energy += f64::from(sec).powi(2);
                }
            }
        }
    }
    assert!(
        low_energy > 0.0 && sec_energy > 0.0,
        "反例必须实际包含低带和 SEC"
    );
    Observation {
        crossover: bands.sbx(),
        frequency,
        low_energy,
        sec_energy,
        max_extra,
    }
}

#[test]
fn noninterleaved_full_pipeline_selects_sec_above_crossover() {
    let observation = full_pipeline_observation();
    assert_eq!(
        observation.max_extra, 0.0,
        "unflagged high-band input reaches the complete production output"
    );
}

#[test]
fn selected_merge_keeps_raw_history_across_crossover_and_delay_changes() {
    let mut state = Box::new(AspxChannelState::new());
    let mut dry_state = Box::new(AspxChannelState::new());
    let mut ws = Box::new(AspxWorkspace::new());
    let mut dry = Box::new(AspxWorkspace::new());
    let mut history = Vec::new();
    let mut start = 0usize;
    for (slots, offset, sbx) in [
        (32, 6, 36u8),
        (32, 6, 47),
        (15, 3, 10),
        (30, 6, 63),
        (16, 3, 4),
    ] {
        ws.prepare_frame();
        dry.prepare_frame();
        for t in 0..slots {
            for b in 0..64 {
                let tag = ((start + t) * 64 + b + 1) as f32;
                ws.q_in[t].re[b] = tag;
                ws.q_in[t].im[b] = -tag - 0.25;
                if (usize::from(sbx)..usize::from(sbx) + (64 - usize::from(sbx)).div_ceil(2))
                    .contains(&b)
                {
                    ws.y[t].re[b] = 0.5 * tag + 0.125;
                    ws.y[t].im[b] = 0.25 * tag;
                }
            }
        }
        dry.q_in.copy_from_slice(&ws.q_in);
        history.extend_from_slice(&ws.q_in[..slots]);
        let input = ws.q_in;
        let sec = ws.y;
        let unused = ws.q_out[slots..].to_vec();
        combine_hf_to_qmf(&mut state, &mut ws, slots, offset, sbx).unwrap();
        combine_to_qmf(&mut dry_state, &mut dry, slots, offset).unwrap();
        for (t, &sec_slot) in sec.iter().enumerate().take(slots) {
            let delayed = (start + t)
                .checked_sub(usize::from(offset))
                .map_or(QmfSlot::zero(), |i| history[i]);
            for b in 0..64 {
                assert_eq!(dry.q_out[t].re[b], delayed.re[b], "干通路保留所有子带");
                assert_eq!(dry.q_out[t].im[b], delayed.im[b]);
                let expected = if b < usize::from(sbx) {
                    delayed
                } else {
                    sec_slot
                };
                assert_eq!(ws.q_out[t].re[b], expected.re[b]);
                assert_eq!(ws.q_out[t].im[b], expected.im[b]);
            }
        }
        assert_eq!(
            state.output, dry_state.output,
            "两种出口都必须保存同一原始全带历史"
        );
        assert_eq!(ws.q_in, input, "不能通过修改分析输入来实现选择");
        assert_eq!(ws.y, sec, "合并不能改写包含尾部的已组装 SEC");
        assert_eq!(ws.q_out[slots..], unused, "不改帧外输出");
        start += slots;
    }
}

fn read_qmf(path: &std::path::Path) -> Vec<QmfSlot> {
    let raw = std::fs::read(path).unwrap();
    assert_eq!(raw.len(), 96 * 128 * 4);
    raw.as_chunks::<512>()
        .0
        .iter()
        .map(|row| {
            let mut q = QmfSlot::zero();
            for (b, pair) in row.as_chunks::<8>().0.iter().enumerate() {
                q.re[b] = f32::from_le_bytes(pair[..4].try_into().unwrap());
                q.im[b] = f32::from_le_bytes(pair[4..].try_into().unwrap());
                assert!(q.re[b].is_finite() && q.im[b].is_finite());
            }
            q
        })
        .collect()
}

#[test]
#[ignore = "requires frozen investigation inputs and the original select candidate outputs"]
fn production_merge_reproduces_frozen_selection_candidate() {
    let data = std::env::var("MACINAC4_MERGE_DATA").unwrap();
    let expected = std::env::var("MACINAC4_MERGE_EXPECTED").unwrap();
    let (data, expected) = (std::path::Path::new(&data), std::path::Path::new(&expected));
    let mut count = 0;
    for line in std::fs::read_to_string(data.join("cases.txt"))
        .unwrap()
        .lines()
    {
        let p: Vec<_> = line.split_whitespace().collect();
        assert_eq!(p.len(), 4);
        assert!(p[..3].iter().all(|s| !s.is_empty()
            && s.len() <= 63
            && s.bytes().all(|v| v.is_ascii_alphanumeric())));
        let sbx: u8 = p[3].parse().unwrap();
        assert!((4..64).contains(&sbx));
        let q = read_qmf(&data.join(std::format!("{}.f32", p[1])));
        let y = read_qmf(&data.join(std::format!("{}.f32", p[2])));
        let candidate = read_qmf(&expected.join(std::format!("{}.select.qmf.f32", p[0])));
        let pcm = std::fs::read(expected.join(std::format!("{}.select.pcm.f32", p[0]))).unwrap();
        assert_eq!(pcm.len(), 6144 * 4);
        let mut state = Box::new(AspxChannelState::new());
        let mut ws = Box::new(AspxWorkspace::new());
        ws.q_in.copy_from_slice(&q[..32]);
        combine_to_qmf(&mut state, &mut ws, 32, 6).unwrap();
        for frame in 0..3 {
            if frame < 2 {
                ws.q_in
                    .copy_from_slice(&q[(frame + 1) * 32..(frame + 2) * 32]);
            } else {
                ws.q_in.fill(QmfSlot::zero());
            }
            ws.y[..32].copy_from_slice(&y[frame * 32..(frame + 1) * 32]);
            let got = combine_hf_to_qmf(&mut state, &mut ws, 32, 6, sbx).unwrap();
            for (a, b) in got.iter().zip(&candidate[frame * 32..(frame + 1) * 32]) {
                assert_eq!(a.re.map(f32::to_bits), b.re.map(f32::to_bits));
                assert_eq!(a.im.map(f32::to_bits), b.im.map(f32::to_bits));
            }
            let mut rendered = [0.0; 2048];
            synthesise_ac4_pcm(got, &mut state.synthesis, &mut rendered).unwrap();
            for (i, sample) in rendered.iter().enumerate() {
                let offset = (frame * 2048 + i) * 4;
                assert_eq!(sample.to_le_bytes(), pcm[offset..offset + 4]);
            }
        }
        count += 1;
    }
    assert_eq!(count, 2311, "完整冻结案例集不可遗漏");
    std::println!("{count} fixed QMF/PCM candidates reproduced bit-for-bit");
}

#[test]
#[ignore = "requires an explicit observation file for before/after comparison"]
fn observe_noninterleaved_pipeline_merge() {
    let path = std::env::var("MACINAC4_MERGE_PIPELINE_OUTPUT").unwrap();
    let Observation {
        crossover,
        frequency,
        low_energy,
        sec_energy,
        max_extra,
    } = full_pipeline_observation();
    let mut file = std::fs::File::create_new(path).unwrap();
    writeln!(file, "{{\"crossover\":{crossover},\"input_hz\":{},\"frames\":3,\"low_energy\":{low_energy},\"sec_energy\":{sec_energy},\"max_extra\":{max_extra},\"sec_only_holds\":{}}}", frequency*48000.0/(2.0*core::f64::consts::PI),max_extra==0.0).unwrap();
    if std::env::var_os("MACINAC4_MERGE_REQUIRE_SEC_ONLY").is_some() {
        assert_eq!(
            max_extra, 0.0,
            "unflagged high-band input reaches the complete production output"
        );
    }
}
