//! 连续本地链：每段实际消费上一段的结果，各帧保留自己的生产状态。
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "测试几何由现有区间推导器验证，逐边界比较使用同一有界布局"
)]
use super::*;
use std::boxed::Box;
use std::io::Write;

#[derive(Debug, PartialEq)]
struct Boundary {
    bits: Vec<u32>,
    energy: [f64; 4],
    estimate_sum: f64,
    gain_range: [f32; 2],
    noise: u16,
    tone: u8,
    carry: u8,
}

fn collect(q: &[QmfSlot], bits: &mut Vec<u32>) -> f64 {
    let mut energy = 0.0;
    for slot in q {
        for b in 0..64 {
            for value in [slot.re[b], slot.im[b]] {
                assert!(value.is_finite());
                bits.push(value.to_bits());
                energy += f64::from(value).powi(2);
            }
        }
    }
    energy
}

fn interval_frame(frame: usize, slots: u8) -> AspxChannelFraming {
    let (mut params, _) = match frame {
        0 => right_overrun_interval(slots, 1),
        1 => left_shifted_interval(slots, 1),
        _ => split_interval(slots),
    };
    params.tsg_ptr = match frame {
        0 => 1,
        1 => 2,
        _ => -1,
    };
    let interval = AspxInterval::derive(&params, slots, 0, true, i16::from(slots)).unwrap();
    assert_eq!(interval.num_atsg_sig(), 2);
    framing(params, interval, false, false)
}

fn check_estimate(
    ws: &AspxWorkspace,
    bands: &AspxBandTables,
    interval: &AspxInterval,
    factor: usize,
    interpolation: bool,
    estimated: &EnvelopeEstimate,
) {
    let first = usize::try_from(interval.sig_border(0).unwrap()).unwrap() * factor;
    for e in 0..2 {
        let a = usize::try_from(interval.sig_border(e).unwrap()).unwrap() * factor - first;
        let z = usize::try_from(interval.sig_border(e + 1).unwrap()).unwrap() * factor - first;
        for sb in usize::from(bands.sbx())..usize::from(bands.sbz()) {
            let (lo, hi) = if interpolation {
                (sb, sb + 1)
            } else {
                let high = interval.freq_res(e).unwrap();
                let groups = if high {
                    bands.num_sbg_sig_highres()
                } else {
                    bands.num_sbg_sig_lowres()
                };
                let border = |g| {
                    usize::from(if high {
                        bands.sig_highres_border(g).unwrap()
                    } else {
                        bands.sig_lowres_border(g).unwrap()
                    })
                };
                (0..usize::from(groups))
                    .map(|g| (border(g), border(g + 1)))
                    .find(|&(a, b)| a <= sb && sb < b)
                    .unwrap()
            };
            let mut power = 0.0;
            for slot in &ws.q_high[a..z] {
                for b in lo..hi {
                    power += f64::from(slot.re[b]).powi(2) + f64::from(slot.im[b]).powi(2);
                }
            }
            power /= ((z - a) * (hi - lo)) as f64;
            let actual = f64::from(
                estimated
                    .estimated(sb - usize::from(bands.sbx()), e)
                    .unwrap(),
            );
            assert!(
                (actual - power).abs() <= power * 2e-6,
                "估计未消费实际 HF：e={e}, sb={sb}"
            );
        }
    }
}

fn run_sequence(
    layout: (usize, usize, u8, u8),
    mode: u8,
    interpolation: bool,
    harmonics: bool,
    fresh_workspace: bool,
) -> (Box<AspxChannelState>, Vec<Boundary>) {
    let (samples, slots, factor, ats) = layout;
    let bands = hf_bands();
    let modes = chirp_modes(&bands);
    let mut state = Box::new(AspxChannelState::new());
    let mut control = Box::new(AspxChannelState::new());
    let mut ws = Box::new(AspxWorkspace::new());
    let mut dry = Box::new(AspxWorkspace::new());
    let mut intermediates = AspxIntermediates::default();
    let mut dry_intermediates = AspxIntermediates::default();
    let mut qin_history = Vec::new();
    let mut tail = Vec::new();
    let mut records = Vec::new();
    let mut changed_hf = false;
    let mut observed_sine = false;
    let mut previous_noise = 0usize;
    let mut generated_slots = 0usize;
    for frame_index in 0..4 {
        // 分批时仅替换可重建工作区，持续保留本地状态；区间本身完全相同。
        if fresh_workspace && frame_index == 2 {
            *ws = AspxWorkspace::new();
        }
        let frame = interval_frame(frame_index, ats);
        let interval = &frame.interval;
        let first = usize::try_from(interval.sig_border(0).unwrap()).unwrap() * usize::from(factor);
        let last = usize::try_from(interval.sig_border(2).unwrap()).unwrap() * usize::from(factor);
        let body = last - first;
        let pcm: Vec<_> = (0..samples)
            .map(|i| {
                let t = (frame_index * samples + i) as f64;
                (7000.0 * (0.017 * t).sin()
                    + 2300.0 * (0.131 * t).cos()
                    + 400.0 * (0.337 * t).sin()) as f32
            })
            .collect();
        let data = envelope_data(&bands, frame_index as i16);
        let harmonic =
            vec![harmonics && frame_index != 3; usize::from(bands.num_sbg_sig_highres())];
        let mut params = hf_params(&modes);
        params.pre_flattening = true;
        let adjustment = HfAdjustParams {
            interpolation,
            add_harmonic: &harmonic,
            limiter: mode == 2,
            limiter_off_boost: mode == 1,
            first_frame: frame_index == 0,
            master_reset: frame_index == 0,
        };
        frame_with_scale_factors_qmf(
            &pcm,
            &bands,
            params,
            EnvelopeInput {
                framing: &frame,
                data: &data,
            },
            adjustment,
            &mut state,
            &mut ws,
            &mut intermediates,
        )
        .unwrap();
        params.pre_flattening = false;
        frame_with_scale_factors_qmf(
            &pcm,
            &bands,
            params,
            EnvelopeInput {
                framing: &frame,
                data: &data,
            },
            adjustment,
            &mut control,
            &mut dry,
            &mut dry_intermediates,
        )
        .unwrap();
        assert_eq!(ws.q_in, dry.q_in);
        assert_eq!(ws.q_low, dry.q_low);
        assert_eq!(state.analysis, control.analysis);
        assert_eq!(state.low_band, control.low_band);
        assert_eq!(state.output, control.output);
        assert_eq!(state.synthesis, control.synthesis);
        assert_eq!(state.tna, control.tna);
        assert_eq!(state.chirp, control.chirp);
        assert_eq!(state.envelope, control.envelope);
        assert_eq!(state.sine, control.sine);
        assert_eq!(state.noise, control.noise);
        assert_eq!(state.tone, control.tone);
        changed_hf |= ws.q_high[..body] != dry.q_high[..body];
        check_estimate(
            &ws,
            &bands,
            interval,
            usize::from(factor),
            interpolation,
            &intermediates.estimate,
        );

        let tables = hf_gen_tables(&bands, params).unwrap();
        let limiter = LimiterTable::derive(&bands, &tables.patches).unwrap();
        let selected = match mode {
            0 => LimiterMode::Off,
            1 => LimiterMode::OffWithBoost {
                table: &limiter,
                patches: &tables.patches,
            },
            _ => LimiterMode::On {
                table: &limiter,
                patches: &tables.patches,
            },
        };
        let mut recomputed = AdjustedGains::new();
        adjust(&intermediates.estimate, &bands, selected, &mut recomputed).unwrap();
        assert_eq!(recomputed, intermediates.gains, "增益未消费本地估计");

        assert_eq!(first, tail.len());
        assert_eq!(&ws.y[..first], tail.as_slice(), "跨帧成品被重新生成");
        for t in first..last {
            let env = usize::from(
                t >= usize::try_from(interval.sig_border(1).unwrap()).unwrap()
                    * usize::from(factor),
            );
            let expected_onset = matches!((frame_index, env), (0, 1) | (2, 0));
            assert_eq!(intermediates.estimate.sine_onset(env), Some(expected_onset));
            for b in usize::from(bands.sbx())..usize::from(bands.sbz()) {
                let sb = b - usize::from(bands.sbx());
                let gain = intermediates.gains.signal_gain(sb, env).unwrap();
                let inject =
                    !expected_onset && intermediates.estimate.sine_level(sb, env).unwrap() == 0.0;
                let s = t - first;
                for (source, noise, sine, actual) in [
                    (
                        ws.q_high[s].re[b],
                        ws.noise[s].re[b],
                        ws.sine[s].re[b],
                        ws.y[t].re[b],
                    ),
                    (
                        ws.q_high[s].im[b],
                        ws.noise[s].im[b],
                        ws.sine[s].im[b],
                        ws.y[t].im[b],
                    ),
                ] {
                    let mut expected = gain * source;
                    if inject {
                        expected += noise;
                    }
                    expected += sine;
                    assert_eq!(actual.to_bits(), expected.to_bits(), "组装边界错位");
                    observed_sine |= sine != 0.0;
                    if frame_index == 3 {
                        assert_eq!(sine, 0.0);
                    }
                }
            }
        }
        tail = ws.y[slots..last].to_vec();
        assert_eq!(usize::from(state.hf.carried()), tail.len());
        qin_history.extend_from_slice(&ws.q_in[..slots]);
        let delay = usize::from(ts_offset_hfgen(samples as u16).unwrap());
        for t in 0..slots {
            let input = (frame_index * slots + t)
                .checked_sub(delay)
                .map_or(QmfSlot::zero(), |i| qin_history[i]);
            for b in 0..64 {
                assert_eq!(
                    ws.q_out[t].re[b].to_bits(),
                    if b < usize::from(bands.sbx()) {
                        input.re[b]
                    } else {
                        ws.y[t].re[b]
                    }
                    .to_bits()
                );
                assert_eq!(
                    ws.q_out[t].im[b].to_bits(),
                    if b < usize::from(bands.sbx()) {
                        input.im[b]
                    } else {
                        ws.y[t].im[b]
                    }
                    .to_bits()
                );
            }
        }
        previous_noise = (previous_noise + body * usize::from(bands.num_sb_aspx())) % 512;
        generated_slots += body;
        assert_eq!(usize::from(state.noise.previous()), previous_noise);
        assert_eq!(usize::from(state.tone.previous()), generated_slots % 4);
        let mut bits = Vec::new();
        let mut energy = [0.0; 4];
        for (i, q) in [
            &ws.q_high[..body],
            &ws.noise[..body],
            &ws.y[..last],
            &ws.q_out[..slots],
        ]
        .into_iter()
        .enumerate()
        {
            energy[i] = collect(q, &mut bits);
        }
        collect(&ws.sine[..body], &mut bits);
        let mut estimate_sum = 0.0;
        let mut gain_range = [f32::INFINITY, 0.0f32];
        for e in 0..2 {
            for b in 0..usize::from(bands.num_sb_aspx()) {
                let estimate = intermediates.estimate.estimated(b, e).unwrap();
                assert!(estimate.is_finite() && estimate >= 0.0);
                estimate_sum += f64::from(estimate);
                bits.push(estimate.to_bits());
                for value in [
                    intermediates.gains.signal_gain(b, e).unwrap(),
                    intermediates.gains.noise_level(b, e).unwrap(),
                    intermediates.gains.sine_level(b, e).unwrap(),
                    intermediates.gains.boost_factor(b, e).unwrap(),
                ] {
                    assert!(value.is_finite() && value >= 0.0);
                    bits.push(value.to_bits());
                }
                let g = intermediates.gains.signal_gain(b, e).unwrap();
                gain_range[0] = gain_range[0].min(g);
                gain_range[1] = gain_range[1].max(g);
            }
        }
        records.push(Boundary {
            bits,
            energy,
            estimate_sum,
            gain_range,
            noise: state.noise.previous(),
            tone: state.tone.previous(),
            carry: state.hf.carried(),
        });
    }
    assert!(changed_hf, "预平坦化必须实际改变 HF，不能使用退化对照");
    assert_eq!(observed_sine, harmonics);
    assert_ne!(state.tna, TnaDelay::new());
    (state, records)
}

fn verify() -> Vec<(usize, u8, bool, bool, Vec<Boundary>)> {
    let mut records = Vec::new();
    for (samples, slots, factor, ats) in FACTOR_TIERS {
        for mode in 0..3 {
            for interpolation in [false, true] {
                for harmonic in [false, true] {
                    let (state, continuous) = run_sequence(
                        (samples, slots, factor, ats),
                        mode,
                        interpolation,
                        harmonic,
                        false,
                    );
                    let (other, batched) = run_sequence(
                        (samples, slots, factor, ats),
                        mode,
                        interpolation,
                        harmonic,
                        true,
                    );
                    assert_eq!(state, other);
                    assert_eq!(continuous, batched, "同一区间序列的分批处理应逐位相同");
                    records.push((samples, mode, interpolation, harmonic, continuous));
                }
            }
        }
    }
    records
}

#[test]
fn preflattened_local_chain_preserves_boundaries_and_continuous_state() {
    assert_eq!(verify().len(), 24);
}

#[test]
#[ignore = "requires explicit local boundary trace output"]
fn export_preflatten_chain_boundaries() {
    let path = std::env::var("MACINAC4_PREFLATTEN_CHAIN_OUTPUT").unwrap();
    let mut file = std::fs::File::create_new(path).unwrap();
    writeln!(file,"samples mode interpolation harmonic frame hf_energy noise_energy y_energy output_energy estimate_sum min_gain max_gain noise_cursor tone_cursor carry").unwrap();
    for (samples, mode, interpolation, harmonic, rows) in verify() {
        for (frame, row) in rows.into_iter().enumerate() {
            writeln!(
                file,
                "{samples} {mode} {interpolation} {harmonic} {frame} {} {} {} {} {} {} {} {} {} {}",
                row.energy[0],
                row.energy[1],
                row.energy[2],
                row.energy[3],
                row.estimate_sum,
                row.gain_range[0],
                row.gain_range[1],
                row.noise,
                row.tone,
                row.carry
            )
            .unwrap();
        }
    }
}
