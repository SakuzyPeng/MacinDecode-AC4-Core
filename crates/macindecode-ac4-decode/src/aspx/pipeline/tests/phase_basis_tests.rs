//! 用任意逐带四分之一周旋转验证完整本地链的表示不变量。
//! 旋转只存在于测试驱动；不采用参考库的相位作为生产策略。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "测试布局由现有推导器验证，四分之一周旋转只交换实虚部及符号"
)]
use super::*;
use std::boxed::Box;
use std::io::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Defect {
    None,
    Operator,
    Noise,
    Tone,
    History,
    Carry,
}

fn phase(kind: u8, band: usize) -> usize {
    match kind {
        0 => 0,
        1 => (band * band + band / 3 + 1) % 4,
        2 => (3 * band + band / 5 + 2) % 4,
        _ => unreachable!(),
    }
}

fn rotate_pair(re: f32, im: f32, quarter: usize) -> (f32, f32) {
    match quarter % 4 {
        0 => (re, im),
        1 => (-im, re),
        2 => (-re, -im),
        3 => (im, -re),
        _ => unreachable!(),
    }
}

fn rotate(q: &mut [QmfSlot], kind: u8, inverse: bool) {
    for slot in q {
        for b in 0..64 {
            let n = phase(kind, b);
            let n = if inverse { (4 - n) % 4 } else { n };
            (slot.re[b], slot.im[b]) = rotate_pair(slot.re[b], slot.im[b], n);
        }
    }
}

struct RotatedRun {
    state: Box<AspxChannelState>,
    workspace: Box<AspxWorkspace>,
    basis: u8,
    defect: Defect,
}

impl RotatedRun {
    fn new(basis: u8, defect: Defect) -> Self {
        Self {
            state: Box::new(AspxChannelState::new()),
            workspace: Box::new(AspxWorkspace::new()),
            basis,
            defect,
        }
    }

    fn step(
        &mut self,
        pcm: &[f32],
        bands: &AspxBandTables,
        params: HfGenParams<'_>,
        envelopes: EnvelopeInput<'_>,
        adjustment: HfAdjustParams<'_>,
    ) -> (Vec<QmfSlot>, Vec<f32>) {
        let (next_history, qscf) =
            decode_envelope_input(bands, envelopes, false, self.state.envelope).unwrap();
        let mut factors = ScaleFactors::new();
        dequantise(&qscf, envelopes.framing.qmode_env, &mut factors).unwrap();
        let (slots, offset) = validate_frame(pcm, None).unwrap();
        let factor = frame_timeslot_factor(pcm.len()).unwrap();
        let aspx = high_band_interval(&envelopes.framing.interval, slots, factor).unwrap();
        let tables = hf_gen_tables(bands, params).unwrap();
        let limiter = hf_adjust_limiter(bands, &tables, adjustment).unwrap();
        validate_hf_carryover(aspx, &self.state, factor, slots).unwrap();
        self.workspace.prepare_frame();
        analyse_ac4_pcm(
            pcm,
            &mut self.state.analysis,
            &mut self.workspace.q_in[..slots],
        )
        .unwrap();
        rotate(&mut self.workspace.q_in[..slots], self.basis, false);
        let extended = slots + usize::from(offset);
        low_band(
            &self.workspace.q_in[..slots],
            &mut self.state.low_band,
            bands.sbx(),
            offset,
            &mut self.workspace.q_low[..extended],
        )
        .unwrap();

        if self.defect == Defect::History && !adjustment.first_frame {
            let view = ExtendedLowBand::new(&self.state.tna, &[]);
            let mut stale = [QmfSlot::zero(); 4];
            for (t, s) in stale.iter_mut().enumerate() {
                for b in 0..64 {
                    let (re, im) = view.sample(b, t).unwrap();
                    s.re[b] = re;
                    s.im[b] = im;
                }
            }
            rotate(&mut stale, self.basis, true);
            self.state.tna.advance(&stale, 4).unwrap();
        }
        generate_hf_from_low_band(
            bands,
            params,
            &tables,
            aspx,
            &mut self.state,
            &mut self.workspace,
            slots,
            extended,
        )
        .unwrap();
        if self.defect != Defect::Operator {
            for i in 0..usize::from(tables.patches.count()) {
                let source = usize::from(tables.patches.start_sb(i).unwrap());
                let target = usize::from(tables.patches.border(i).unwrap());
                for j in 0..usize::from(tables.patches.num_sb(i).unwrap()) {
                    let p = source + j;
                    let k = target + j;
                    let turn = (phase(self.basis, k) + 4 - phase(self.basis, p)) % 4;
                    for q in &mut self.workspace.q_high[..aspx.timeslots()] {
                        (q.re[k], q.im[k]) = rotate_pair(q.re[k], q.im[k], turn);
                    }
                }
            }
        }
        let (next_sines, _, gains) = adjust_high_band(
            &self.workspace.q_high[..aspx.timeslots()],
            bands,
            &tables,
            envelopes,
            adjustment,
            limiter.as_ref(),
            &factors,
            factor,
            self.state.sine,
        )
        .unwrap();
        generate_noise(
            &gains,
            bands,
            &envelopes.framing.interval,
            factor,
            adjustment.master_reset,
            &mut self.state.noise,
            &mut self.workspace.noise[..aspx.timeslots()],
        )
        .unwrap();
        generate_tone(
            &gains,
            bands,
            &envelopes.framing.interval,
            factor,
            adjustment.first_frame,
            &mut self.state.tone,
            &mut self.workspace.sine[..aspx.timeslots()],
        )
        .unwrap();
        if self.defect != Defect::Noise {
            rotate(
                &mut self.workspace.noise[..aspx.timeslots()],
                self.basis,
                false,
            );
        }
        if self.defect != Defect::Tone {
            rotate(
                &mut self.workspace.sine[..aspx.timeslots()],
                self.basis,
                false,
            );
        }
        if self.defect == Defect::Carry && !adjustment.first_frame {
            for ts in 0..usize::from(self.state.hf.carried()) {
                rotate(
                    core::slice::from_mut(self.state.hf.tail_mut_for_test(ts)),
                    self.basis,
                    true,
                );
            }
        }
        assemble(
            &gains,
            bands,
            &envelopes.framing.interval,
            factor,
            slots as u8,
            &self.workspace.q_high[..aspx.timeslots()],
            &self.workspace.noise[..aspx.timeslots()],
            &self.workspace.sine[..aspx.timeslots()],
            &mut self.state.hf,
            &mut self.workspace.y[..aspx.last],
        )
        .unwrap();
        self.state.envelope = next_history;
        self.state.sine = next_sines;
        let mut output = combine_to_qmf(&mut self.state, &mut self.workspace, slots, offset)
            .unwrap()
            .to_vec();
        rotate(&mut output, self.basis, true);
        let mut pcm = vec![0.0; slots * 64];
        synthesise_ac4_pcm(&output, &mut self.state.synthesis, &mut pcm).unwrap();
        (output, pcm)
    }
}

#[derive(Debug)]
struct Score {
    values: usize,
    bit_differences: usize,
    maximum: f32,
    error_energy: f64,
    signal_energy: f64,
}

fn compare_basis(config: usize, basis: u8, defect: Defect) -> Score {
    let bands = AspxBandTables::derive(
        true,
        6,
        if config == 0 { 1 } else { 0 },
        3,
        if config == 0 { 0 } else { 5 },
    )
    .unwrap();
    let modes = vec![2; usize::from(bands.num_sbg_noise())];
    let mut state = Box::new(AspxChannelState::new());
    let mut ws = Box::new(AspxWorkspace::new());
    let mut info = AspxIntermediates::default();
    let mut changed = RotatedRun::new(basis, defect);
    let mut score = Score {
        values: 0,
        bit_differences: 0,
        maximum: 0.0,
        error_energy: 0.0,
        signal_energy: 0.0,
    };
    let mut seed = 847012u32;
    for frame_index in 0..4 {
        let (mut p, _) = match frame_index {
            0 => right_overrun_interval(16, 1),
            1 => left_shifted_interval(16, 1),
            _ => split_interval(16),
        };
        p.tsg_ptr = match frame_index {
            0 => 1,
            1 => 2,
            _ => -1,
        };
        let interval = AspxInterval::derive(&p, 16, 0, true, 16).unwrap();
        let frame = framing(p, interval, false, false);
        let data = envelope_data(&bands, frame_index as i16);
        let pcm: Vec<_> = (0..2048)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = ((seed >> 8) as f64 / 16777216.0 - 0.5) * 2500.0;
                (noise + 3000.0 * ((frame_index * 2048 + i) as f64 * 0.037).sin()) as f32
            })
            .collect();
        let params = HfGenParams {
            master_freq_scale: true,
            base_samp_freq_48: true,
            chirp_modes: &modes,
            pre_flattening: true,
        };
        let harmonic = vec![frame_index != 2; usize::from(bands.num_sbg_sig_highres())];
        let adjustment = HfAdjustParams {
            add_harmonic: &harmonic,
            interpolation: true,
            limiter: true,
            limiter_off_boost: false,
            master_reset: frame_index == 0,
            first_frame: frame_index == 0,
        };
        let mut expected = vec![0.0; 2048];
        frame_with_scale_factors(
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
            &mut info,
            &mut expected,
        )
        .unwrap();
        let (q, actual) = changed.step(
            &pcm,
            &bands,
            params,
            EnvelopeInput {
                framing: &frame,
                data: &data,
            },
            adjustment,
        );
        assert_eq!(state.analysis, changed.state.analysis);
        assert_eq!(state.noise, changed.state.noise);
        assert_eq!(state.tone, changed.state.tone);
        if defect == Defect::None {
            assert_eq!(state.sine, changed.state.sine);
            assert_eq!(state.chirp, changed.state.chirp);
            assert_eq!(q, &ws.q_out[..32]);
        }
        for (a, b) in expected.into_iter().zip(actual) {
            assert!(a.is_finite() && b.is_finite());
            let delta = f64::from(b) - f64::from(a);
            score.values += 1;
            score.bit_differences += usize::from(a.to_bits() != b.to_bits());
            score.maximum = score.maximum.max((b - a).abs());
            score.error_energy += delta * delta;
            score.signal_energy += f64::from(a) * f64::from(a);
        }
    }
    score
}

fn verify_basis() -> Vec<(usize, u8, Defect, Score)> {
    let mut results = Vec::new();
    for config in 0..2 {
        for basis in 0..3 {
            let score = compare_basis(config, basis, Defect::None);
            assert_eq!(score.maximum, 0.0, "config={config}, basis={basis}");
            assert_eq!(score.bit_differences, 0);
            results.push((config, basis, Defect::None, score));
        }
    }
    for defect in [
        Defect::Operator,
        Defect::Noise,
        Defect::Tone,
        Defect::History,
        Defect::Carry,
    ] {
        let score = compare_basis(0, 1, defect);
        assert!(
            score.error_energy > 1e-10 * score.signal_energy,
            "反例必须可辨：{defect:?}, {score:?}"
        );
        results.push((0, 1, defect, score));
    }
    results
}

#[test]
fn consistent_basis_changes_preserve_pcm_and_partial_changes_are_detected() {
    verify_basis();
}

#[test]
#[ignore = "requires explicit local basis trace output"]
fn export_phase_basis_controls() {
    let path = std::env::var("MACINAC4_PHASE_BASIS_OUTPUT").unwrap();
    let mut f = std::fs::File::create_new(path).unwrap();
    writeln!(
        f,
        "config basis defect values bit_differences max_abs error_energy signal_energy"
    )
    .unwrap();
    for (config, basis, defect, s) in verify_basis() {
        writeln!(
            f,
            "{config} {basis} {defect:?} {} {} {} {} {}",
            s.values, s.bit_differences, s.maximum, s.error_energy, s.signal_energy
        )
        .unwrap();
    }
}
