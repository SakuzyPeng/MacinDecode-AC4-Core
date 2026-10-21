//! 宽度调查的测试专用入口：全部变体使用相同生产下游，不执行参考 DSP。
//! 固定目标／音调元数据在 P90 后接入；估计能量必须来自本次实际 HF。
//! 第二段是明确的构造延续：重复末 32 个分析时隙及末包络控制，不冒充下一真实 AU。
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "离线测试先校验固定矩阵、频带和时间布局，再按有界下标记录边界"
)]
extern crate std;
use super::estimate_replay_tests::{Case, parse, run};
use super::*;
use crate::aspx::frames::AspxIntervalParams;
use crate::aspx::hfassemble::{HfDelay, assemble};
use crate::aspx::hfgain::{AdjustedGains, LimiterMode, adjust};
use crate::aspx::hfgen::hf_generate;
use crate::aspx::interleave::{AspxOutputDelay, combine};
use crate::aspx::limiter::LimiterTable;
use crate::aspx::noisegen::{NoiseCursor, generate as noise_generate};
use crate::aspx::patches::PatchTable;
use crate::aspx::preflatten::{PreFlattenGains, pre_flatten};
use crate::aspx::qmf::{QmfSynthesisState, synthesise_ac4_pcm};
use crate::aspx::tables::IntervalClass;
use crate::aspx::tna::{ExtendedLowBand, TnaDelay, TnaFilters, prediction_filters};
use crate::aspx::tonegen::{ToneCursor, generate as tone_generate};
use std::collections::BTreeSet;
use std::format;
use std::io::Write;
use std::path::Path;
use std::vec;
use std::vec::Vec;

const VARIANTS: [&str; 7] = ["d", "c", "c512", "b", "b512", "e", "a"];

fn floats(path: &Path, count: usize) -> Vec<f32> {
    let raw = std::fs::read(path).unwrap();
    assert_eq!(raw.len(), count * 4, "{path:?}");
    let values: Vec<_> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&v| f32::from_le_bytes(v))
        .collect();
    assert!(values.iter().all(|v| v.is_finite()), "{path:?}");
    values
}

fn slots(values: &[f32]) -> Vec<QmfSlot> {
    assert_eq!(values.len() % 128, 0);
    values
        .as_chunks::<128>()
        .0
        .iter()
        .map(|v| {
            let mut q = QmfSlot::zero();
            for b in 0..64 {
                q.re[b] = v[2 * b];
                q.im[b] = v[2 * b + 1];
            }
            q
        })
        .collect()
}

fn save(path: &Path, values: impl IntoIterator<Item = f32>) {
    let mut f = std::io::BufWriter::new(std::fs::File::create_new(path).unwrap());
    for v in values {
        assert!(v.is_finite());
        f.write_all(&v.to_le_bytes()).unwrap();
    }
    f.flush().unwrap();
}

fn save_q(path: &Path, q: &[QmfSlot]) {
    save(
        path,
        q.iter()
            .flat_map(|s| (0..64).flat_map(move |b| [s.re[b], s.im[b]])),
    );
}

// 元数据 seam 与既有增益重放相同；新能量不能被检查点的旧能量覆盖。
fn apply_controls(estimated: &mut EnvelopeEstimate, controls: &[f32]) {
    let n = usize::from(estimated.subbands());
    let envs = usize::from(estimated.envelopes());
    assert_eq!(controls.len(), envs * (1 + 6 * n));
    let mut pointer = -1;
    for e in 0..envs {
        let offset = e * (1 + 6 * n);
        assert!(matches!(controls[offset], 0.0 | 1.0));
        if controls[offset] == 1.0 {
            assert_eq!(pointer, -1, "本轮夹具最多包含一个 onset");
            pointer = e as i8;
        }
        for b in 0..n {
            let v = &controls[offset + 1 + 6 * b..offset + 7 + 6 * b];
            assert!(v[0] > 0.0 && v.iter().all(|v| v.is_finite() && *v >= 0.0));
            assert!(matches!(v[2], 0.0 | 1.0) && matches!(v[3], 0.0 | 1.0));
            estimated.scf_sig[b][e] = v[0];
            estimated.scf_noise[b][e] = v[1];
            estimated.sine_idx[b][e] = v[2] != 0.0;
            estimated.sine_area[b][e] = v[3] != 0.0;
            estimated.noise_lev[b][e] = v[4];
            estimated.sine_lev[b][e] = v[5];
        }
    }
    estimated.placement = SinePlacement::from_params(pointer);
    estimated.prev_at_end = false;
}

struct State {
    history: TnaDelay,
    hf: HfDelay,
    noise: NoiseCursor,
    tone: ToneCursor,
    output: AspxOutputDelay,
    synthesis: QmfSynthesisState,
}

struct Step<'a> {
    case: &'a Case,
    controls: &'a [f32],
    chirp: &'a [f32],
    mode: u8,
    preflat: bool,
}

impl Step<'_> {
    fn run(&self, low: &[QmfSlot], state: &mut State, output: &Path) -> Vec<QmfSlot> {
        let c = self.case;
        let bands = &c.bands;
        let first = usize::try_from(c.interval.sig_border(0).unwrap()).unwrap() * 2;
        let last = usize::try_from(
            c.interval
                .sig_border(usize::from(c.interval.num_atsg_sig()))
                .unwrap(),
        )
        .unwrap()
            * 2;
        assert_eq!(low.len(), 38);
        let original = low.to_vec();
        let patches = PatchTable::derive(bands, true, true).unwrap();
        let limiter = LimiterTable::derive(bands, &patches).unwrap();
        let borders: Vec<_> = (0..=usize::from(bands.num_sbg_noise()))
            .map(|i| bands.noise_border(i).unwrap())
            .collect();
        let mut filters = TnaFilters::new();
        let ext = ExtendedLowBand::new(&state.history, low);
        prediction_filters(ext, 32, bands.sba(), &mut filters).unwrap();
        save(
            &output.with_extension("alpha.f32"),
            (0..64).flat_map(|b| {
                let (a, z) = filters.coefficients(b).unwrap_or_default();
                [a.re, a.im, z.re, z.im]
            }),
        );
        let mut pre = PreFlattenGains::new();
        pre_flatten(low, usize::from(bands.sbx()), first, last, &mut pre).unwrap();
        save(
            &output.with_extension("pre.f32"),
            (0..64).map(|b| pre.gain(b).unwrap_or(1.0)),
        );
        let mut high = vec![QmfSlot::zero(); last - first];
        hf_generate(
            ext,
            &patches,
            &filters,
            self.chirp,
            &borders,
            self.preflat.then_some(&pre),
            first,
            last,
            &mut high,
        )
        .unwrap();
        state.history.advance(low, 32).unwrap();
        let next = ExtendedLowBand::new(&state.history, &[]);
        let mut history = [QmfSlot::zero(); 4];
        for (t, q) in history.iter_mut().enumerate() {
            for b in 0..64 {
                (q.re[b], q.im[b]) = next.sample(b, t).unwrap();
            }
        }
        assert_eq!(history, low[28..32], "必须携带原始低带，不能携带滤波残差");
        save_q(&output.with_extension("history.f32"), &history);
        save_q(&output.with_extension("hf.f32"), &high);
        let mut estimate_input = vec![QmfSlot::zero(); first];
        estimate_input.extend_from_slice(&high);
        let mut estimated = run(c, &estimate_input);
        apply_controls(&mut estimated, self.controls);
        let mode = match self.mode {
            0 => LimiterMode::Off,
            1 => LimiterMode::OffWithBoost {
                table: &limiter,
                patches: &patches,
            },
            2 => LimiterMode::On {
                table: &limiter,
                patches: &patches,
            },
            _ => unreachable!(),
        };
        let mut gains = AdjustedGains::new();
        adjust(&estimated, bands, mode, &mut gains).unwrap();
        save(
            &output.with_extension("levels.f32"),
            (0..usize::from(estimated.envelopes())).flat_map(|e| {
                let estimated = &estimated;
                let gains = &gains;
                (0..usize::from(estimated.subbands())).flat_map(move |b| {
                    [
                        estimated.estimated(b, e).unwrap(),
                        gains.signal_gain(b, e).unwrap(),
                        gains.noise_level(b, e).unwrap(),
                        gains.sine_level(b, e).unwrap(),
                        gains.boost_factor(b, e).unwrap(),
                        f32::from(u8::from(gains.noise_enabled(b, e).unwrap())),
                    ]
                })
            }),
        );
        let before = [state.noise.previous(), u16::from(state.tone.previous())];
        let mut noise = vec![QmfSlot::zero(); high.len()];
        let mut tone = noise.clone();
        noise_generate(
            &gains,
            bands,
            &c.interval,
            2,
            false,
            &mut state.noise,
            &mut noise,
        )
        .unwrap();
        tone_generate(
            &gains,
            bands,
            &c.interval,
            2,
            false,
            &mut state.tone,
            &mut tone,
        )
        .unwrap();
        let mut y = vec![QmfSlot::zero(); last];
        assemble(
            &gains,
            bands,
            &c.interval,
            2,
            32,
            &high,
            &noise,
            &tone,
            &mut state.hf,
            &mut y,
        )
        .unwrap();
        assert_eq!(low, original, "下游不可改写原始低带");
        save_q(&output.with_extension("noise.f32"), &noise);
        save_q(&output.with_extension("tone.f32"), &tone);
        save_q(&output.with_extension("y.f32"), &y);
        save(
            &output.with_extension("state.f32"),
            [
                f32::from(before[0]),
                f32::from(before[1]),
                f32::from(state.noise.previous()),
                f32::from(state.tone.previous()),
                f32::from(state.hf.carried()),
            ],
        );
        let mut merged = vec![QmfSlot::zero(); 32];
        combine(&low[6..38], &y, 6, &mut state.output, &mut merged).unwrap();
        save_q(&output.with_extension("merged.f32"), &merged);
        let mut pcm = vec![0.0; 2048];
        synthesise_ac4_pcm(&merged, &mut state.synthesis, &mut pcm).unwrap();
        save(&output.with_extension("pcm.f32"), pcm);
        y
    }
}

#[test]
#[ignore = "requires normalized width inputs; outputs and licensed material stay local"]
fn replay_qmf_width_downstream() {
    let data = std::env::var("MACINAC4_WIDTH_CHAIN_DATA").unwrap();
    let output = std::env::var("MACINAC4_WIDTH_CHAIN_OUTPUT").unwrap();
    let (data, output) = (Path::new(&data), Path::new(&output));
    std::fs::create_dir(output).unwrap();
    std::fs::write(
        output.join("replay_metadata.json"),
        "{\"schema\":\"macinac4.width-chain-replay\",\"version\":1,\"prediction_solver\":\"orthogonal_residual\",\"preflatten_gain_convention\":\"slope_minus_mean\",\"initial_cursors\":[0,0],\"successor\":\"repeat_last_analysis_au\"}\n",
    ).unwrap();
    let mut ids = BTreeSet::new();
    for line in std::fs::read_to_string(data.join("cases.txt"))
        .unwrap()
        .lines()
    {
        let c = parse(line).unwrap();
        assert!(ids.insert(c.id.clone()));
        let envs = usize::from(c.interval.num_atsg_sig());
        let n = usize::from(c.bands.num_sb_aspx());
        let controls = floats(
            &data.join(format!("{}.controls.f32", c.id)),
            envs * (1 + 6 * n),
        );
        let chirp = floats(
            &data.join(format!("{}.chirp.f32", c.id)),
            usize::from(c.bands.num_sbg_noise()),
        );
        assert!(chirp.iter().all(|v| (0.0..=1.0).contains(v)));
        let first = usize::try_from(c.interval.sig_border(0).unwrap()).unwrap() * 2;
        let last = usize::try_from(c.interval.sig_border(envs).unwrap()).unwrap() * 2;
        let mut params = AspxIntervalParams::fixfix(1);
        params.int_class = IntervalClass::VarFix;
        params.var_bord_left = Some(((last - 32) / 2) as u8);
        params.freq_res[0] = c.interval.freq_res(envs - 1).unwrap();
        let next = Case {
            id: c.id.clone(),
            bands: c.bands,
            interval: AspxInterval::derive(&params, 16, 0, true, 16).unwrap(),
            interpolation: c.interpolation,
        };
        // 末包络控制延续，不复制其 onset 事件。
        let mut next_controls = controls[(envs - 1) * (1 + 6 * n)..].to_vec();
        next_controls[0] = 0.0;
        for variant in VARIANTS {
            let mut stream = slots(&floats(
                &data.join(format!("{}.{}.f32", c.id, variant)),
                64 * 128,
            ));
            stream.extend_from_within(32..64);
            for preflat in [false, true] {
                for mode in 0..3 {
                    let mut state = State {
                        history: TnaDelay::new(),
                        hf: HfDelay::new(),
                        noise: NoiseCursor::new(),
                        tone: ToneCursor::new(),
                        output: AspxOutputDelay::new(),
                        synthesis: QmfSynthesisState::new(),
                    };
                    state.history.advance(&stream[22..26], 4).unwrap();
                    // 只预热合并器的分析历史；合成器仍从声明的零状态开始。
                    combine(
                        &stream[..32],
                        &[QmfSlot::zero(); 32],
                        6,
                        &mut state.output,
                        &mut [QmfSlot::zero(); 32],
                    )
                    .unwrap();
                    assert!(state.hf.prefill_silence(first));
                    let prefix = format!("{}{}p{}m{}", c.id, variant, u8::from(preflat), mode);
                    let y = Step {
                        case: &c,
                        controls: &controls,
                        chirp: &chirp,
                        mode,
                        preflat,
                    }
                    .run(
                        &stream[26..64],
                        &mut state,
                        &output.join(format!("{prefix}s0")),
                    );
                    let z = Step {
                        case: &next,
                        controls: &next_controls,
                        chirp: &chirp,
                        mode,
                        preflat,
                    }
                    .run(
                        &stream[58..96],
                        &mut state,
                        &output.join(format!("{prefix}s1")),
                    );
                    assert_eq!(&y[32..], &z[..last - 32], "已处理的 HF 尾部必须原样交接");
                }
            }
        }
    }
    assert!(!ids.is_empty());
}
