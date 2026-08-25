//! 64 子带复数 QMF 滤波器组（`TS103190-1:v1.4.1:5.7.3`、`5.7.4`）。
//!
//! `Pseudocode 65` 是分析，`Pseudocode 66` 是合成，两者共用表 D.3 的 640 抽头
//! 原型窗 `QWIN`。窗取自规范随附 C 表，不经人工转写；构建期另核对两条与摘要
//! 无关的结构判据，见 `build.rs` 的 `emit_qmf_window`。
//!
//! 分析调制仍按定义式直算。合成调制按 ADR-0008 利用输出 `n` 与 `127-n` 的精确
//! 负共轭关系成对累加，共享一半乘法与相位加载；每个输出内部仍按原有子带顺序累加，
//! 因此保持逐位 PCM。128 点 FFT 加前/后旋转虽更快，但会改变加法结合顺序，已因
//! 无法通过逐位门禁而否决。三角值同样查构建期生成的表，运行期不做求值。
//!
//! 滤波器状态与工作区都由调用方提供，不分配；生存期与 `Ac4DecoderSession` 的
//! 边界一起定，此处只固定「不分配」这一条。

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "全部下标由 64/128/640/1280 这几个常量与已核对过的时隙数派生，\
              两条伪码的多重偏移用显式下标比迭代器更贴近原文，便于逐行核对"
)]

use crate::aspx::tables::NUM_QMF_SUBBANDS;

/// 表 D.3 的原型窗抽头数。
pub const NUM_QMF_WIN_COEF: usize = 640;

/// 子带数，`5.7.3.2` 规定恒为 64。
pub(crate) const SUBBANDS: usize = NUM_QMF_SUBBANDS as usize;
/// `Pseudocode 65` 折叠后的向量长度。
const FOLDED: usize = 2 * SUBBANDS;
/// `Pseudocode 66` 的合成状态长度。
const SYNTH_STATE: usize = 10 * FOLDED;

/// 本实现为终端 Core PCM 保留的 2 倍表示因子。
///
/// `Pseudocode 62`/`64` 与 `5.5.3` 对该因子的落位并不自洽；生产链按 ADR-0006
/// 的跨条款裁决，在 Core PCM 跨进或跨出绝对标度的 QMF 工具链时做互逆换算。
/// 纯 [`analyse`]/[`synthesise`] 始终保持 `Pseudocode 65`/`66` 的字面量级。
const AC4_PCM_INTERFACE_GAIN: f32 = 2.0;

/// 单位圆表的点数，见 [`modulation`]。
const MODULATION_POINTS: usize = 512;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/qmf_window.rs"));
}

mod generated_modulation {
    include!(concat!(env!("OUT_DIR"), "/qmf_modulation.rs"));
}

pub(crate) use generated::QMF_WINDOW;

/// 一个 QMF 时隙的 64 个复数子带样本。
///
/// 实部虚部分开存放而不是 `[(f32, f32); 64]`：A-SPX 的包络与噪声处理逐子带
/// 遍历实部或模值，分开存能让那些循环连续读。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QmfSlot {
    /// 实部，下标即子带号。
    pub re: [f32; SUBBANDS],
    /// 虚部。
    pub im: [f32; SUBBANDS],
}

impl QmfSlot {
    /// 全零时隙。
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            re: [0.0; SUBBANDS],
            im: [0.0; SUBBANDS],
        }
    }
}

impl Default for QmfSlot {
    fn default() -> Self {
        Self::zero()
    }
}

/// 分析滤波器组的跨帧状态，即 `Pseudocode 65` 的 `qmf_filt`。
#[derive(Debug, PartialEq)]
pub struct QmfAnalysisState {
    filt: [f32; NUM_QMF_WIN_COEF],
}

impl QmfAnalysisState {
    /// 建立全零状态。首帧之前没有信号，等价于前置静音。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            filt: [0.0; NUM_QMF_WIN_COEF],
        }
    }
}

impl Default for QmfAnalysisState {
    fn default() -> Self {
        Self::new()
    }
}

/// 合成滤波器组的跨帧状态，即 `Pseudocode 66` 的 `qsyn_filt`。
///
/// 规范写作 1 280 个样本，其中 1 152 个跨调用保留——即每时隙左移 128 之后
/// 剩下的部分。
#[derive(Debug, PartialEq)]
pub struct QmfSynthesisState {
    filt: [f32; SYNTH_STATE],
}

impl QmfSynthesisState {
    /// 建立全零状态。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            filt: [0.0; SYNTH_STATE],
        }
    }
}

impl Default for QmfSynthesisState {
    fn default() -> Self {
        Self::new()
    }
}

/// QMF 无法执行的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QmfError {
    /// 输入 PCM 的长度不是 64 的整数倍。
    UnalignedInput { samples: usize },
    /// 输出容量与时隙数不符。
    SlotCountMismatch { expected: usize, provided: usize },
}

/// `Pseudocode 65` 的分析滤波。
///
/// `pcm` 的长度必须是 64 的整数倍，每 64 个样本产出一个时隙；`slots` 的长度
/// 必须等于时隙数。
///
/// # Errors
///
/// 长度不成立时返回 [`QmfError`]，且不改写状态。
pub fn analyse(
    pcm: &[f32],
    state: &mut QmfAnalysisState,
    slots: &mut [QmfSlot],
) -> Result<(), QmfError> {
    if pcm.len() % SUBBANDS != 0 {
        return Err(QmfError::UnalignedInput { samples: pcm.len() });
    }
    let timeslots = pcm.len() / SUBBANDS;
    if slots.len() != timeslots {
        return Err(QmfError::SlotCountMismatch {
            expected: timeslots,
            provided: slots.len(),
        });
    }

    let mut folded = [0.0f64; FOLDED];
    for (ts, slot) in slots.iter_mut().enumerate() {
        // 左移 64，再按「越高下标越旧」喂入新样本。
        state
            .filt
            .copy_within(0..NUM_QMF_WIN_COEF - SUBBANDS, SUBBANDS);
        for sb in 0..SUBBANDS {
            state.filt[SUBBANDS - 1 - sb] = pcm[ts * SUBBANDS + sb];
        }

        // 加窗并折叠成 128 项。
        for (n, cell) in folded.iter_mut().enumerate() {
            let mut sum = 0.0f64;
            for k in 0..5 {
                let index = n + k * FOLDED;
                sum += f64::from(state.filt[index]) * f64::from(QMF_WINDOW[index]);
            }
            *cell = sum;
        }

        // 调制：Q[sb] = Σ_n u[n] · exp(j·(π/128)·(sb+0.5)·(2n−1))。
        for sb in 0..SUBBANDS {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &value) in folded.iter().enumerate() {
                let (cos, sin) = analysis_phase(sb, n);
                re += value * cos;
                im += value * sin;
            }
            slot.re[sb] = re as f32;
            slot.im[sb] = im as f32;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "qmf-split-profile", inline(never))]
#[cfg_attr(not(feature = "qmf-split-profile"), inline(always))]
fn advance_synthesis_state(state: &mut QmfSynthesisState) {
    state.filt.copy_within(0..SYNTH_STATE - FOLDED, FOLDED);
    #[cfg(feature = "qmf-split-profile")]
    core::hint::black_box(state.filt[FOLDED]);
}

#[cfg_attr(feature = "qmf-split-profile", inline(never))]
#[cfg_attr(not(feature = "qmf-split-profile"), inline(always))]
fn modulate_synthesis_slot(slot: &QmfSlot, state: &mut QmfSynthesisState) {
    // 复数子带经 N 矩阵回到 128 个实数样本；`1/64` 是规范写在矩阵里的。
    //
    // 成对处理 n 与 127-n。令 q=2sb+1、a=q(2n+1)，两行的指数分别为
    // a-256q ≡ a-256 与 -a，因此相位恰为 -exp(ja) 与 conj(exp(ja))。
    // 一对输出共享 re*cos 和 im*sin；两个累加器仍各自按 sb=0…63 的定义顺序
    // 前进，所以不会改变 f64 加法结合顺序或最终 f32 舍入。
    for n in 0..SUBBANDS {
        let mut low_sum = 0.0f64;
        let mut high_sum = 0.0f64;
        for sb in 0..SUBBANDS {
            let (cos, sin) = modulation(sb, 2 * n as isize + 1);
            let real = f64::from(slot.re[sb]) * cos;
            let imaginary = f64::from(slot.im[sb]) * sin;
            low_sum += imaginary - real;
            high_sum += real + imaginary;
        }
        state.filt[n] = (low_sum / SUBBANDS as f64) as f32;
        state.filt[FOLDED - 1 - n] = (high_sum / SUBBANDS as f64) as f32;
    }
}

#[cfg_attr(feature = "qmf-split-profile", inline(never))]
#[cfg_attr(not(feature = "qmf-split-profile"), inline(always))]
fn accumulate_synthesis_polyphase(
    state: &QmfSynthesisState,
    windowed: &mut [f64; NUM_QMF_WIN_COEF],
    pcm: &mut [f32],
    timeslot: usize,
) {
    // 抽取 g、加窗、按 64 相位求和。
    for n in 0..5 {
        for sb in 0..SUBBANDS {
            let low = FOLDED * n + sb;
            let high = FOLDED * n + SUBBANDS + sb;
            windowed[low] = f64::from(state.filt[2 * FOLDED * n + sb]) * f64::from(QMF_WINDOW[low]);
            windowed[high] = f64::from(state.filt[2 * FOLDED * n + 3 * SUBBANDS + sb])
                * f64::from(QMF_WINDOW[high]);
        }
    }
    for sb in 0..SUBBANDS {
        let mut sum = 0.0f64;
        for n in 0..10 {
            sum += windowed[SUBBANDS * n + sb];
        }
        pcm[timeslot * SUBBANDS + sb] = sum as f32;
    }
}

/// `Pseudocode 66` 的合成滤波。
///
/// # Errors
///
/// 长度不成立时返回 [`QmfError`]，且不改写状态。
#[cfg_attr(feature = "qmf-split-profile", inline(never))]
pub fn synthesise(
    slots: &[QmfSlot],
    state: &mut QmfSynthesisState,
    pcm: &mut [f32],
) -> Result<(), QmfError> {
    if pcm.len() % SUBBANDS != 0 {
        return Err(QmfError::UnalignedInput { samples: pcm.len() });
    }
    let timeslots = pcm.len() / SUBBANDS;
    if slots.len() != timeslots {
        return Err(QmfError::SlotCountMismatch {
            expected: timeslots,
            provided: slots.len(),
        });
    }

    let mut windowed = [0.0f64; NUM_QMF_WIN_COEF];
    for (ts, slot) in slots.iter().enumerate() {
        advance_synthesis_state(state);
        modulate_synthesis_slot(slot, state);
        accumulate_synthesis_polyphase(state, &mut windowed, pcm, ts);
    }
    Ok(())
}

/// 把本解码器重建出的 Core PCM 分析到规范 QMF 量级。
///
/// 本解码器的终端 Core PCM 按 `TS103190-1:v1.4.1:5.5.3` 的块切换示例保留
/// 2 倍表示；若把它不经换算送入 A-SPX/A-JOC，低带直通能量会多 6 dB，而
/// `Pseudocode 82` 传输来的绝对高带目标不随之改变。这里先执行字面的
/// [`analyse`]，再按 ADR-0006 撤销该表示因子。独立使用滤波器组时仍应直接调用
/// [`analyse`]；本换算是对规范内部量级歧义的裁决，不是 `5.5.3` 明文定义的接口规则。
///
/// # Errors
///
/// 与 [`analyse`] 相同。错误返回时不改写状态或输出。
pub fn analyse_ac4_pcm(
    pcm: &[f32],
    state: &mut QmfAnalysisState,
    slots: &mut [QmfSlot],
) -> Result<(), QmfError> {
    analyse(pcm, state, slots)?;
    for slot in slots {
        for value in slot.re.iter_mut().chain(slot.im.iter_mut()) {
            *value /= AC4_PCM_INTERFACE_GAIN;
        }
    }
    Ok(())
}

/// 把终端 QMF 输出合成为本解码器的 Core PCM 呈现量级。
///
/// 这是 [`analyse_ac4_pcm`] 的另一侧：所有 A-SPX/A-JOC QMF 域工具完成后才补回
/// 终端 Core PCM 的 2 倍表示因子。独立使用滤波器组时仍应直接调用
/// [`synthesise`]。证据等级与推翻条件见 ADR-0006。
///
/// # Errors
///
/// 与 [`synthesise`] 相同。错误返回时不改写状态或输出。
pub fn synthesise_ac4_pcm(
    slots: &[QmfSlot],
    state: &mut QmfSynthesisState,
    pcm: &mut [f32],
) -> Result<(), QmfError> {
    synthesise(slots, state, pcm)?;
    for sample in pcm {
        *sample *= AC4_PCM_INTERFACE_GAIN;
    }
    Ok(())
}

/// 分析调制的相位 `exp(j·(π/128)·(sb+0.5)·(2n−1))`。
fn analysis_phase(sb: usize, n: usize) -> (f64, f64) {
    modulation(sb, 2 * (n as isize) - 1)
}

/// 合成调制的相位 `exp(j·(π/128)·(sb+0.5)·(2n−255))`。
#[cfg(test)]
fn synthesis_phase(sb: usize, n: usize) -> (f64, f64) {
    modulation(sb, 2 * (n as isize) - 255)
}

/// `exp(j·(π/128)·(sb+0.5)·m)` 的实部与虚部，`m` 为奇数。
///
/// `(π/128)·(sb+0.5)·m = 2π·(2sb+1)·m / 512`，因此查一张 512 点的单位圆表即
/// 可，运行期不做三角求值。`(2sb+1)·m` 恒为奇数，所以只用得到奇数下标；表仍
/// 按整圈存，见 `build.rs` 的说明。
fn modulation(sb: usize, m: isize) -> (f64, f64) {
    let index = ((2 * sb as isize + 1) * m).rem_euclid(MODULATION_POINTS as isize) as usize;
    let [cos, sin] = generated_modulation::QMF_MODULATION[index];
    (f64::from(cos), f64::from(sin))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::format;
    use std::time::{Duration, Instant};
    use std::vec;
    use std::vec::Vec;

    /// 原型窗的两条结构判据在运行期再核一遍。
    ///
    /// 构建期已经核过同样两条，这里重复是为了让**默认拿到生成结果的读者**也能
    /// 在测试里看到它们：镜像只在 128 的倍数处反号，每个相位的平方和为 1。
    #[test]
    fn the_prototype_window_is_mirrored_and_power_complementary() {
        assert_eq!(QMF_WINDOW.len(), NUM_QMF_WIN_COEF);
        assert_eq!(QMF_WINDOW[0], 0.0);
        for n in 1..NUM_QMF_WIN_COEF {
            let mirrored = QMF_WINDOW[NUM_QMF_WIN_COEF - n];
            let expected = if n % 128 == 0 { -mirrored } else { mirrored };
            assert_eq!(QMF_WINDOW[n], expected, "QWIN[{n}] 的镜像关系");
        }

        let mut worst = 0.0f64;
        for phase in 0..SUBBANDS {
            let sum: f64 = (0..NUM_QMF_WIN_COEF / SUBBANDS)
                .map(|k| {
                    let value = f64::from(QMF_WINDOW[SUBBANDS * k + phase]);
                    value * value
                })
                .sum();
            worst = worst.max((sum - 1.0).abs());
        }
        assert!(worst <= 1.0e-6, "多相功率互补最大偏差 {worst:e}");
    }

    /// 分析接合成的总延迟，单位为样本。
    ///
    /// 不是 `640 − 64 = 576`：`Pseudocode 65` 把一块的第一个样本喂到 `filt[63]`
    /// （块内最旧），而两族调制的指数 `2n−1` 与 `2n−255` 又各带半个样本的偏移。
    /// 64 种块内输入相位的冲激峰值都恰好落在「输入位置 + 577」，主瓣对 1 的
    /// 最大偏差为 `2.4×10⁻⁷`，见
    /// [`all_input_phases_have_symmetric_finite_polyphase_responses`]。
    const ROUND_TRIP_DELAY: usize = 577;

    fn deterministic_signal(count: usize) -> Vec<f32> {
        let mut state = 0x3c6e_f372_fe94_f82b_u64;
        (0..count)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 40) as i32 - 8_388_608) as f32 / 8_388_608.0
            })
            .collect()
    }

    /// 拆分前 `Pseudocode 66` 的字面实现，仅用于锁定符号级归因改写的逐位等价性。
    fn synthesise_monolithic_reference(
        slots: &[QmfSlot],
        state: &mut QmfSynthesisState,
        pcm: &mut [f32],
    ) {
        assert_eq!(pcm.len(), slots.len() * SUBBANDS);
        let mut windowed = [0.0f64; NUM_QMF_WIN_COEF];
        for (ts, slot) in slots.iter().enumerate() {
            state.filt.copy_within(0..SYNTH_STATE - FOLDED, FOLDED);
            for n in 0..FOLDED {
                let mut sum = 0.0f64;
                for sb in 0..SUBBANDS {
                    let (cos, sin) = synthesis_phase(sb, n);
                    sum += f64::from(slot.re[sb]) * cos - f64::from(slot.im[sb]) * sin;
                }
                state.filt[n] = (sum / SUBBANDS as f64) as f32;
            }
            for n in 0..5 {
                for sb in 0..SUBBANDS {
                    let low = FOLDED * n + sb;
                    let high = FOLDED * n + SUBBANDS + sb;
                    windowed[low] =
                        f64::from(state.filt[2 * FOLDED * n + sb]) * f64::from(QMF_WINDOW[low]);
                    windowed[high] = f64::from(state.filt[2 * FOLDED * n + 3 * SUBBANDS + sb])
                        * f64::from(QMF_WINDOW[high]);
                }
            }
            for sb in 0..SUBBANDS {
                let mut sum = 0.0f64;
                for n in 0..10 {
                    sum += windowed[SUBBANDS * n + sb];
                }
                pcm[ts * SUBBANDS + sb] = sum as f32;
            }
        }
    }

    fn deterministic_slots(count: usize) -> Vec<QmfSlot> {
        let values = deterministic_signal(count * 2 * SUBBANDS);
        let mut slots = vec![QmfSlot::zero(); count];
        for (index, slot) in slots.iter_mut().enumerate() {
            let start = index * 2 * SUBBANDS;
            slot.re.copy_from_slice(&values[start..start + SUBBANDS]);
            slot.im
                .copy_from_slice(&values[start + SUBBANDS..start + 2 * SUBBANDS]);
        }
        slots
    }

    /// `Pseudocode 66` 合成调制的逐行定义式，作为候选实现的测试 oracle。
    fn modulate_synthesis_slot_direct(slot: &QmfSlot, state: &mut QmfSynthesisState) {
        for n in 0..FOLDED {
            let mut sum = 0.0f64;
            for sb in 0..SUBBANDS {
                let (cos, sin) = synthesis_phase(sb, n);
                sum += f64::from(slot.re[sb]) * cos - f64::from(slot.im[sb]) * sin;
            }
            state.filt[n] = (sum / SUBBANDS as f64) as f32;
        }
    }

    fn finite_edge_slots() -> Vec<QmfSlot> {
        let values = [
            0.0f32,
            -0.0f32,
            f32::from_bits(1),
            -f32::from_bits(1),
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            1.0,
            -1.0,
            f32::MAX,
            -f32::MAX,
        ];
        (0..values.len())
            .map(|offset| {
                let mut slot = QmfSlot::zero();
                for sb in 0..SUBBANDS {
                    slot.re[sb] = values[(sb + offset) % values.len()];
                    slot.im[sb] = values[(3 * sb + offset + 1) % values.len()];
                }
                slot
            })
            .collect()
    }

    fn assert_synthesis_head_bit_exact(
        actual: &QmfSynthesisState,
        expected: &QmfSynthesisState,
        context: &str,
    ) {
        for (index, (&actual, &expected)) in actual.filt[..FOLDED]
            .iter()
            .zip(&expected.filt[..FOLDED])
            .enumerate()
        {
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{context} 的调制输出 {index} 必须逐位相同"
            );
        }
    }

    #[test]
    fn synthesis_phase_rows_form_exact_negated_conjugate_pairs() {
        for n in 0..SUBBANDS {
            for sb in 0..SUBBANDS {
                let root = modulation(sb, 2 * n as isize + 1);
                let low = synthesis_phase(sb, n);
                let high = synthesis_phase(sb, FOLDED - 1 - n);
                assert_eq!(low.0.to_bits(), (-root.0).to_bits());
                assert_eq!(low.1.to_bits(), (-root.1).to_bits());
                assert_eq!(high.0.to_bits(), root.0.to_bits());
                assert_eq!(high.1.to_bits(), (-root.1).to_bits());
            }
        }
    }

    #[test]
    fn paired_modulation_is_bit_exact_to_the_definition() {
        let mut slots = deterministic_slots(19);
        slots.extend(finite_edge_slots());
        for (slot_index, slot) in slots.iter().enumerate() {
            let mut direct = QmfSynthesisState::new();
            let mut paired = QmfSynthesisState::new();
            modulate_synthesis_slot_direct(slot, &mut direct);
            modulate_synthesis_slot(slot, &mut paired);
            assert_synthesis_head_bit_exact(&paired, &direct, &format!("paired slot {slot_index}"));
        }
    }

    fn benchmark_modulation(
        label: &str,
        iterations: usize,
        mut run: impl FnMut(&QmfSlot, &mut QmfSynthesisState),
    ) -> Duration {
        let slots = deterministic_slots(19);
        let mut state = QmfSynthesisState::new();
        let started = Instant::now();
        for iteration in 0..iterations {
            let slot = std::hint::black_box(&slots[iteration % slots.len()]);
            run(slot, std::hint::black_box(&mut state));
            std::hint::black_box(&state.filt[..FOLDED]);
        }
        let elapsed = started.elapsed();
        std::eprintln!(
            "{label}: {:.3} ns/slot",
            elapsed.as_nanos() as f64 / iterations as f64
        );
        elapsed
    }

    /// 手动运行：
    /// `cargo test -p macindecode-ac4-bitstream --release --features audio-decode qmf_paired_modulation_benchmark -- --ignored --nocapture`
    #[test]
    #[ignore = "手动运行的 QMF 成对调制微基准"]
    fn qmf_paired_modulation_benchmark() {
        const ITERATIONS: usize = 20_000;
        let mut direct = Vec::with_capacity(5);
        let mut paired = Vec::with_capacity(5);
        for round in 0..5 {
            if round % 2 == 0 {
                direct.push(benchmark_modulation(
                    "direct",
                    ITERATIONS,
                    modulate_synthesis_slot_direct,
                ));
                paired.push(benchmark_modulation(
                    "paired",
                    ITERATIONS,
                    modulate_synthesis_slot,
                ));
            } else {
                paired.push(benchmark_modulation(
                    "paired",
                    ITERATIONS,
                    modulate_synthesis_slot,
                ));
                direct.push(benchmark_modulation(
                    "direct",
                    ITERATIONS,
                    modulate_synthesis_slot_direct,
                ));
            }
        }
        direct.sort_unstable();
        paired.sort_unstable();

        std::eprintln!(
            "median speedup: paired {:.3}x",
            direct[2].as_secs_f64() / paired[2].as_secs_f64(),
        );
    }

    #[test]
    fn split_synthesis_phases_are_bit_exact_to_monolithic_reference() {
        let slots = deterministic_slots(19);
        let mut split_state = QmfSynthesisState::new();
        let mut reference_state = QmfSynthesisState::new();

        for (call_index, frame) in [&slots[..7], &slots[7..]].into_iter().enumerate() {
            let mut split = vec![0.0f32; frame.len() * SUBBANDS];
            let mut reference = vec![0.0f32; frame.len() * SUBBANDS];
            synthesise(frame, &mut split_state, &mut split).expect("拆分实现应成功");
            synthesise_monolithic_reference(frame, &mut reference_state, &mut reference);

            for (sample_index, (split, reference)) in split.iter().zip(&reference).enumerate() {
                assert_eq!(
                    split.to_bits(),
                    reference.to_bits(),
                    "调用 {call_index} 的 PCM 样本 {sample_index} 必须逐位相同"
                );
            }
            for (state_index, (split, reference)) in split_state
                .filt
                .iter()
                .zip(&reference_state.filt)
                .enumerate()
            {
                assert_eq!(
                    split.to_bits(),
                    reference.to_bits(),
                    "调用 {call_index} 的状态 {state_index} 必须逐位相同"
                );
            }
        }
    }

    /// 分析接合成必须重建原信号，只差固定延迟。
    ///
    /// 这是本模块最强的判据：它一次覆盖两条伪码的状态左移、窗下标、折叠与抽取
    /// 顺序、两族调制相位以及 `1/64` 归一化。任何一处出错都会让重建塌掉，而不是
    /// 差一个常数。
    ///
    /// `1e-3` 是这条固定宽带信号的回归预算，不是滤波器组的单一特征值。实测相对
    /// 偏差 `8.7154e-4`；Python 双精度参考实现与未经 f32 舍入的原型窗也得到同一
    /// 数字，说明它不是查表精度造成的，但该值仍依赖输入信号与测量窗口。
    #[test]
    fn analysis_then_synthesis_reconstructs_with_a_fixed_delay() {
        const SLOTS: usize = 40;
        const SAMPLES: usize = SLOTS * SUBBANDS;
        const DELAY: usize = ROUND_TRIP_DELAY;

        let signal = deterministic_signal(SAMPLES);
        let mut analysis = QmfAnalysisState::new();
        let mut synthesis = QmfSynthesisState::new();
        let mut slots = vec![QmfSlot::zero(); SLOTS];
        let mut out = vec![0.0f32; SAMPLES];

        analyse(&signal, &mut analysis, &mut slots).expect("分析应成功");
        synthesise(&slots, &mut synthesis, &mut out).expect("合成应成功");

        let mut worst = 0.0f64;
        let mut energy = 0.0f64;
        for index in DELAY..SAMPLES {
            let expected = f64::from(signal[index - DELAY]);
            let actual = f64::from(out[index]);
            worst = worst.max((actual - expected).abs());
            energy += expected * expected;
        }
        let rms = (energy / (SAMPLES - DELAY) as f64).sqrt();
        assert!(
            worst / rms <= 1.0e-3,
            "QMF 往返最大偏差 {worst:e}（信号 RMS {rms:e}）超出预算"
        );
    }

    /// AC-4 PCM 的 2 倍因子只跨 QMF 域边界撤销并补回；两条规范伪码本身不改。
    #[test]
    fn ac4_pcm_boundary_moves_the_interface_gain_outside_qmf() {
        const SLOTS: usize = 12;
        const SAMPLES: usize = SLOTS * SUBBANDS;

        let signal = deterministic_signal(SAMPLES);
        let mut raw_analysis = QmfAnalysisState::new();
        let mut ac4_analysis = QmfAnalysisState::new();
        let mut raw_slots = vec![QmfSlot::zero(); SLOTS];
        let mut ac4_slots = vec![QmfSlot::zero(); SLOTS];
        analyse(&signal, &mut raw_analysis, &mut raw_slots).expect("规范分析");
        analyse_ac4_pcm(&signal, &mut ac4_analysis, &mut ac4_slots).expect("AC-4 PCM 分析");
        assert_eq!(raw_analysis, ac4_analysis, "接口换算不得改变分析历史");
        for (raw, ac4) in raw_slots.iter().zip(&ac4_slots) {
            for (raw, ac4) in raw
                .re
                .iter()
                .chain(raw.im.iter())
                .zip(ac4.re.iter().chain(ac4.im.iter()))
            {
                assert_eq!(*ac4, *raw / AC4_PCM_INTERFACE_GAIN);
            }
        }

        let mut raw_synthesis = QmfSynthesisState::new();
        let mut ac4_synthesis = QmfSynthesisState::new();
        let mut raw_pcm = vec![0.0f32; SAMPLES];
        let mut ac4_pcm = vec![0.0f32; SAMPLES];
        synthesise(&raw_slots, &mut raw_synthesis, &mut raw_pcm).expect("规范合成");
        synthesise_ac4_pcm(&raw_slots, &mut ac4_synthesis, &mut ac4_pcm).expect("AC-4 PCM 合成");
        assert_eq!(raw_synthesis, ac4_synthesis, "接口换算不得改变合成历史");
        for (raw, ac4) in raw_pcm.iter().zip(ac4_pcm) {
            assert_eq!(ac4, raw * AC4_PCM_INTERFACE_GAIN);
        }
    }

    /// 64 种块内输入相位各有一条有限、对称的多相冲激响应。
    ///
    /// 临界抽取让往返级联以 64 个样本为周期变化，不能把 `signal[0]` 的响应外推成
    /// 单一 LTI 滤波器。对每个输入相位 `p`，结构性抽头都落在
    /// `p + 577 + 128m`（`|m| ≤ 4`）并关于 `p + 577` 逐位对称，但抽头值随 `p`
    /// 变化。全相位实测主瓣对 1 的最大偏差为 `2.4×10⁻⁷`，最大旁瓣
    /// `2.229×10⁻⁴`，离开 128 网格的 f32 噪声不超过 `1.409×10⁻⁸`；网格上
    /// `|m| ≥ 5` 的尾部精确为零。
    ///
    /// 这条判据锁定每个多相分支的延迟、旁瓣位置、对称、有限长度和实现噪声底，
    /// 不从分支对称推出全局线性相位或单一幅频响应。
    #[test]
    fn all_input_phases_have_symmetric_finite_polyphase_responses() {
        const SLOTS: usize = 40;
        const SAMPLES: usize = SLOTS * SUBBANDS;
        /// 调制周期，即两个子带跳。
        const HOP: usize = 2 * SUBBANDS;

        let mut worst_main_error = (0.0f64, 0usize);
        let mut worst_sidelobe = (0.0f64, 0usize, 0usize);
        let mut worst_off_grid = (0.0f64, 0usize, 0usize);

        for phase in 0..SUBBANDS {
            let mut signal = vec![0.0f32; SAMPLES];
            signal[phase] = 1.0;
            let mut analysis = QmfAnalysisState::new();
            let mut synthesis = QmfSynthesisState::new();
            let mut slots = vec![QmfSlot::zero(); SLOTS];
            let mut out = vec![0.0f32; SAMPLES];
            analyse(&signal, &mut analysis, &mut slots).expect("分析应成功");
            synthesise(&slots, &mut synthesis, &mut out).expect("合成应成功");

            let center = ROUND_TRIP_DELAY + phase;
            let peak = out
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.abs().total_cmp(&right.1.abs()))
                .expect("输出非空");
            assert_eq!(peak.0, center, "输入相位 {phase} 的延迟应为 577");

            let main_error = (f64::from(out[center]) - 1.0).abs();
            if main_error > worst_main_error.0 {
                worst_main_error = (main_error, phase);
            }

            for (index, value) in out.iter().enumerate() {
                let distance = index.abs_diff(center);
                if distance % HOP != 0 {
                    let magnitude = f64::from(value.abs());
                    if magnitude > worst_off_grid.0 {
                        worst_off_grid = (magnitude, phase, index);
                    }
                } else if distance > 4 * HOP {
                    assert_eq!(
                        *value, 0.0,
                        "输入相位 {phase}、相对中心 {distance} 处的网格尾部应为零"
                    );
                }
            }

            for offset in (HOP..=4 * HOP).step_by(HOP) {
                let left = out[center - offset];
                let right = out[center + offset];
                assert_eq!(
                    left.to_bits(),
                    right.to_bits(),
                    "输入相位 {phase}、偏移 ±{offset} 的多相抽头应逐位对称"
                );
                let magnitude = f64::from(left).abs();
                if magnitude > worst_sidelobe.0 {
                    worst_sidelobe = (magnitude, phase, offset);
                }
            }
        }

        assert!(
            worst_main_error.0 <= 1.0e-6,
            "输入相位 {} 的主瓣误差 {:e} 超出预算",
            worst_main_error.1,
            worst_main_error.0
        );
        assert!(
            worst_sidelobe.0 <= 2.5e-4,
            "输入相位 {}、偏移 {} 的旁瓣 {:e} 超出预算",
            worst_sidelobe.1,
            worst_sidelobe.2,
            worst_sidelobe.0
        );
        assert!(
            worst_off_grid.0 <= 2.0e-8,
            "输入相位 {}、输出 {} 的离网格噪声 {:e} 超出预算",
            worst_off_grid.1,
            worst_off_grid.2,
            worst_off_grid.0
        );
    }

    /// 静音进、静音出，且不在状态里留下残留。
    #[test]
    fn silence_stays_silent() {
        const SLOTS: usize = 12;
        let mut analysis = QmfAnalysisState::new();
        let mut synthesis = QmfSynthesisState::new();
        let mut slots = vec![QmfSlot::zero(); SLOTS];
        let mut out = vec![0.0f32; SLOTS * SUBBANDS];

        analyse(&vec![0.0f32; SLOTS * SUBBANDS], &mut analysis, &mut slots).expect("分析应成功");
        assert!(
            slots
                .iter()
                .all(|slot| slot.re.iter().chain(slot.im.iter()).all(|&v| v == 0.0)),
            "静音输入应给出静音子带"
        );
        synthesise(&slots, &mut synthesis, &mut out).expect("合成应成功");
        assert!(out.iter().all(|&value| value == 0.0), "静音子带应给出静音");
    }

    /// 非法长度一律拒绝，且不推进状态。
    #[test]
    fn invalid_lengths_are_rejected_without_touching_state() {
        let mut analysis = QmfAnalysisState::new();
        let mut synthesis = QmfSynthesisState::new();
        let mut slots = vec![QmfSlot::zero(); 2];

        assert_eq!(
            analyse(&vec![0.0f32; 100], &mut analysis, &mut slots),
            Err(QmfError::UnalignedInput { samples: 100 })
        );
        assert_eq!(
            analyse(&vec![0.0f32; 3 * SUBBANDS], &mut analysis, &mut slots),
            Err(QmfError::SlotCountMismatch {
                expected: 3,
                provided: 2
            })
        );
        assert!(
            analysis.filt.iter().all(|&value| value == 0.0),
            "拒绝不应写入分析状态"
        );

        let mut pcm = vec![0.0f32; 3 * SUBBANDS];
        assert_eq!(
            synthesise(&slots, &mut synthesis, &mut pcm),
            Err(QmfError::SlotCountMismatch {
                expected: 3,
                provided: 2
            })
        );
        assert!(
            synthesis.filt.iter().all(|&value| value == 0.0),
            "拒绝不应写入合成状态"
        );
    }

    /// 状态跨调用延续：分两次喂入与一次喂入等价。
    #[test]
    fn state_carries_across_calls() {
        const SLOTS: usize = 20;
        const SAMPLES: usize = SLOTS * SUBBANDS;
        let signal = deterministic_signal(SAMPLES);

        let once = {
            let mut state = QmfAnalysisState::new();
            let mut slots = vec![QmfSlot::zero(); SLOTS];
            analyse(&signal, &mut state, &mut slots).expect("分析应成功");
            slots
        };
        let split = {
            let mut state = QmfAnalysisState::new();
            let mut slots = vec![QmfSlot::zero(); SLOTS];
            let (head, tail) = slots.split_at_mut(SLOTS / 2);
            analyse(&signal[..SAMPLES / 2], &mut state, head).expect("前半应成功");
            analyse(&signal[SAMPLES / 2..], &mut state, tail).expect("后半应成功");
            slots
        };
        for (index, (a, b)) in once.iter().zip(split.iter()).enumerate() {
            assert_eq!(a, b, "时隙 {index} 应与一次喂入逐位相同");
        }
    }
}
