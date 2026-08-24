//! `no_std` 下的实数函数（见 ADR-0005）。
//!
//! `core` 只提供四则运算、`abs`、`copysign` 与位转换；`sqrt` 起就不在其中。
//! `5.7.6.4` 的 HF 生成需要三个：`Pseudocode 85` 的 `log10` 与 `pow(10, ·)`
//! ——两者都归结为 `log2`/`exp2`——以及 `Pseudocode 90`–`94` 的六处 `sqrt`。
//! 该节其余 15 处 `pow` 都是平方、`0…3` 次整数幂或编译期常量，不需要通用幂。
//!
//! # 为什么是级数而不是表
//!
//! [ADR-0003](../../../docs/decisions/0003-trigonometric-tables-for-the-transform.md)
//! 用构建期 `libm` 生成三角表并冻结摘要，因为那些函数只在**有限个已知点**上
//! 取值。这里不同：参数是运行期的任意实数，查表解决不了，只能算。
//!
//! 关键前提是 **f64 的四则运算在 `no_std` 下完全可用**。因此三个函数都先把
//! 参数规约到一个窄区间，在 f64 里用收敛很快的级数算到远超 f32 所需的精度，
//! 再按调用点需要收窄。`log2` 的关键运算用高低两个 f64 保留舍入残差；规约用
//! 位运算，是精确的，误差只来自级数截断与 f64 本身的舍入。
//!
//! # 判据
//!
//! 级数系数是可以写错的，而写错之后结果仍然"看着像"那个函数。因此判据分三层：
//!
//! - **精确点**：2 的整数次幂上 `log2` 与 `exp2` 必须给出精确整数，完全平方数
//!   上 `sqrt` 必须精确。这一层只检验规约路径。
//! - **代数律**：`log2(x·y) = log2(x) + log2(y)`、`exp2(a+b) = exp2(a)·exp2(b)`、
//!   两者互逆。这一层不依赖任何具体算法，级数系数错了就不成立。
//! - **独立实现对照**：测试里与 `std` 的同名函数逐点比 ulp。`std` 走 LLVM
//!   intrinsic 与系统 libm，与本模块的级数完全无关。
//!
//! 三层缺一不可：只有精确点会漏掉级数；只有代数律会漏掉整体缩放；只有 ulp
//! 对照则把正确性寄托在 `std` 上，而 `std` 在目标侧并不存在。

#![expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "指数上的整数加减都在 f64 的指数域内，量级不超过 1075；收窄同样\
              发生在已经夹住的区间内，是本模块的目的而非意外；级数系数按定长\
              数组自身的长度倒序取用"
)]

use core::f64::consts::{LN_2, SQRT_2};

/// f64 的尾数位数。
const MANTISSA_BITS: u32 = 52;
/// f64 的指数偏置。
const EXPONENT_BIAS: i32 = 1023;
/// 尾数掩码。
const MANTISSA_MASK: u64 = (1u64 << MANTISSA_BITS) - 1;
/// 指数域全 1，用于取原始指数。
const EXPONENT_MASK: u64 = 0x7ff;
/// `1.0` 的位模式，用来把尾数拼成 `[1, 2)` 的数。
const ONE_BITS: u64 = (EXPONENT_BIAS as u64) << MANTISSA_BITS;
/// 归一化次正规数用的比例，`2^54`。
const SUBNORMAL_SCALE: f64 = 18_014_398_509_481_984.0;
/// 上一行比例的以 2 为底的对数。
const SUBNORMAL_SHIFT: i32 = 54;
/// f64 正规数的最小指数。
const MIN_NORMAL_EXPONENT: i32 = -1022;
/// f64 正规数的最大指数。
const MAX_NORMAL_EXPONENT: i32 = 1023;

/// 把有限正数分解为 `m · 2^e`，`m ∈ [1, 2)`。
///
/// 位运算，无舍入。次正规先乘 `2^54` 抬进正规区再补偿指数。
fn decompose(x: f64) -> (f64, i32) {
    let mut bits = x.to_bits();
    let mut correction = 0;
    if (bits >> MANTISSA_BITS) & EXPONENT_MASK == 0 {
        bits = (x * SUBNORMAL_SCALE).to_bits();
        correction = SUBNORMAL_SHIFT;
    }
    let raw = ((bits >> MANTISSA_BITS) & EXPONENT_MASK) as i32;
    let mantissa = f64::from_bits((bits & MANTISSA_MASK) | ONE_BITS);
    (mantissa, raw - EXPONENT_BIAS - correction)
}

/// `2^n`，`n` 必须在 f64 正规数的指数范围内。
fn exp2i(n: i32) -> f64 {
    debug_assert!((MIN_NORMAL_EXPONENT..=MAX_NORMAL_EXPONENT).contains(&n));
    f64::from_bits((((n + EXPONENT_BIAS) as u64) & EXPONENT_MASK) << MANTISSA_BITS)
}

/// 平方根。
///
/// 规约到 `m ∈ [1, 4)` 与偶数指数后做牛顿迭代 `y ← (y + m/y)/2`。线性初值
/// `(m+1)/2` 的相对误差是 `(√m−1)²/(2√m)`，在 `m = 4` 处取到最大的 1/4；牛顿
/// 迭代二次收敛，`0,25 → 3,1×10⁻² → 4,9×10⁻⁴ → 1,2×10⁻⁷ → 7,2×10⁻¹⁵ →
/// 2,6×10⁻²⁹`，五步已越过 f64 的分辨率。取固定六步而不设收敛判据，省掉一个
/// 可能不终止的循环。
///
/// 特殊值按 IEEE-754：负数与 NaN 给 NaN，`±0` 原样返回（含 `-0.0`），
/// `+∞` 给 `+∞`。
#[must_use]
pub fn sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 || x.is_infinite() {
        return x;
    }
    let (mantissa, exponent) = decompose(x);
    // 指数取偶数，余下的一位并进尾数，于是 m ∈ [1, 4)。
    let (mantissa, exponent) = if exponent % 2 == 0 {
        (mantissa, exponent)
    } else {
        (mantissa * 2.0, exponent - 1)
    };
    let mut root = 0.5 * mantissa + 0.5;
    let mut step = 0;
    while step < 6 {
        root = 0.5 * (root + mantissa / root);
        step += 1;
    }
    root * exp2i(exponent / 2)
}

/// `ln(m)` 的级数系数 `1/(2k+1)`，`k = 0…11`。
///
/// `ln(m) = 2·Σ t^(2k+1)/(2k+1)`，`t = (m−1)/(m+1)`。把 `m` 折到
/// `[√2/2, √2)` 后 `|t| ≤ (√2−1)/(√2+1) < 0,1716`，末项量级 `2·t^25/25`
/// 约 `10⁻²⁰`，远在 f64 的分辨率之下。
const LOG_SERIES: [f64; 12] = [
    1.0,
    1.0 / 3.0,
    1.0 / 5.0,
    1.0 / 7.0,
    1.0 / 9.0,
    1.0 / 11.0,
    1.0 / 13.0,
    1.0 / 15.0,
    1.0 / 17.0,
    1.0 / 19.0,
    1.0 / 21.0,
    1.0 / 23.0,
];

/// 精确 `ln(2)` 减去 [`LN_2`] 后正确舍入到 f64 的低位余项。
const LN_2_LOW: f64 = 2.319_046_813_846_299_6e-17;

/// 用 Dekker 拆分返回 `left * right` 的舍入误差。
///
/// 这里只用于 `log2` 的规约区间，操作数量级都小于 4，不存在拆分乘法上溢。
fn product_error(left: f64, right: f64, product: f64) -> f64 {
    const SPLITTER: f64 = 134_217_729.0; // 2^27 + 1
    let left_scaled = SPLITTER * left;
    let left_high = left_scaled - (left_scaled - left);
    let left_low = left - left_high;
    let right_scaled = SPLITTER * right;
    let right_high = right_scaled - (right_scaled - right);
    let right_low = right - right_high;
    ((left_high * right_high - product) + left_high * right_low + left_low * right_high)
        + left_low * right_low
}

/// `log2` 规约区间内使用的双 f64 数。
///
/// 高低两项保留普通 f64 运算丢掉的舍入残差；所有乘加仍是独立算子，不依赖 FMA。
#[derive(Clone, Copy)]
struct DoubleDouble {
    high: f64,
    low: f64,
}

impl DoubleDouble {
    const fn exact(value: f64) -> Self {
        Self {
            high: value,
            low: 0.0,
        }
    }

    fn from_sum(left: f64, right: f64) -> Self {
        let high = left + right;
        let virtual_right = high - left;
        let virtual_left = high - virtual_right;
        let low = (left - virtual_left) + (right - virtual_right);
        Self { high, low }
    }

    fn add(self, other: Self) -> Self {
        let high = Self::from_sum(self.high, other.high);
        Self::from_sum(high.high, high.low + self.low + other.low)
    }

    fn subtract(self, other: Self) -> Self {
        self.add(Self {
            high: -other.high,
            low: -other.low,
        })
    }

    fn multiply(self, other: Self) -> Self {
        let product = self.high * other.high;
        let error = product_error(self.high, other.high, product)
            + self.high * other.low
            + self.low * other.high
            + self.low * other.low;
        Self::from_sum(product, error)
    }

    fn divide(self, other: Self) -> Self {
        let quotient = self.high / other.high;
        let estimate = Self::exact(quotient);
        let remainder = self.subtract(other.multiply(estimate));
        let correction = (remainder.high + remainder.low) / other.high;
        let estimate = Self::from_sum(quotient, correction);
        let remainder = self.subtract(other.multiply(estimate));
        let correction = (remainder.high + remainder.low) / other.high;
        estimate.add(Self::exact(correction))
    }

    fn rounded(self) -> f64 {
        self.high + self.low
    }
}

/// 以 2 为底的对数。
///
/// 分解出 `x = m · 2^e` 后把 `m` 折到以 1 为中心的区间，再用 `atanh` 形式的
/// 级数——它比直接对 `ln(1+u)` 展开收敛快得多，因为参数被压到了 0,1716 以内。
///
/// 特殊值：负数与 NaN 给 NaN，`+0` 给 `−∞`，`+∞` 给 `+∞`。
#[must_use]
pub fn log2(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_infinite() {
        return x;
    }
    let (mantissa, exponent) = decompose(x);
    // 折到 [√2/2, √2)：两端的 |t| 相等，级数在整个定义域上一样快。
    let (mantissa, exponent) = if mantissa > SQRT_2 {
        (mantissa * 0.5, exponent + 1)
    } else {
        (mantissa, exponent)
    };
    // `mantissa - 1` 由 Sterbenz 引理保证精确；`mantissa + 1` 的舍入残差则由
    // DoubleDouble::from_sum 保留。若直接在单个 f64 中相除，这个低位误差在 √2
    // 规约切点附近足以把 log2 推开 4 ulp。
    let numerator = DoubleDouble::exact(mantissa - 1.0);
    let denominator = DoubleDouble::from_sum(mantissa, 1.0);
    let ratio = numerator.divide(denominator);
    let squared = ratio.multiply(ratio);
    let mut sum = DoubleDouble::exact(0.0);
    let mut index = LOG_SERIES.len();
    while index > 0 {
        index -= 1;
        sum = sum
            .multiply(squared)
            .add(DoubleDouble::exact(LOG_SERIES[index]));
    }
    // ln(m) = 2·t·Σ；LN_2_LOW 补上 core 常量舍掉的低位。
    let logarithm = DoubleDouble::exact(2.0)
        .multiply(ratio)
        .multiply(sum)
        .divide(DoubleDouble {
            high: LN_2,
            low: LN_2_LOW,
        });
    logarithm
        .add(DoubleDouble::exact(f64::from(exponent)))
        .rounded()
}

/// 以 2 为底的指数。
///
/// 拆成 `x = n + f`（`n` 取最近整数，`|f| ≤ 1/2`）后，`2^f = e^(f·ln2)` 用
/// 指数级数算：`|f·ln2| ≤ 0,3466`，`17` 项的截断误差约 `10⁻²³`。
///
/// `n` 落在正规指数范围之外时分两次乘，避免可表示的结果被中间的下溢吃掉。
#[must_use]
pub fn exp2(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    // 1024 已溢出，−1075 之下连最小的次正规都到不了。
    if x >= 1024.0 {
        return f64::INFINITY;
    }
    if x <= -1075.0 {
        return 0.0;
    }
    let whole = if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    };
    // |x| ≤ 1075 且 whole 是最近整数，这个减法是精确的。
    let fraction = (x - f64::from(whole)) * LN_2;
    let mut sum = 1.0;
    let mut term = 17u32;
    while term > 0 {
        sum = sum * (fraction / f64::from(term)) + 1.0;
        term -= 1;
    }
    let clamped = whole.clamp(MIN_NORMAL_EXPONENT, MAX_NORMAL_EXPONENT);
    let residual = whole - clamped;
    if residual == 0 {
        return sum * exp2i(whole);
    }
    // `whole` 取最近整数，因此它可以落在正规指数范围之外，而结果仍然可表示：
    // `2^1023,5 ≈ 1,34×10³⁰⁸` 小于 `f64::MAX`，`2^-1074` 也还是个次正规数。
    // 直接构造 `2^whole` 会在中间步骤溢出或下溢，把可表示的结果算成 `±∞` 或 0
    // ——与 `aspx::dequant` 里 `2^127,5` 那处是同一类错误。
    //
    // 两次乘法的先后在整个可达区间上实测都是 0 ulp（先乘 `2^clamped` 时，
    // `sum < 1` 的中间值虽然会掉进次正规，但最终结果的有效位数更少，丢掉的位
    // 本就不参与最终舍入）。这里取「先小幅调整、再一次到位」，因为它的中间值
    // 恒为正规数，不必依赖上面那个吸收论证。
    sum * exp2i(residual) * exp2i(clamped)
}

/// f32 平方根，中间全程 f64。
///
/// 单次收窄比在 f32 里迭代少一次中间舍入，理由与 ADR-0003 第 5 条用
/// `libm::sqrt` 而非 `sqrtf` 相同。
#[must_use]
pub fn sqrt_f32(x: f32) -> f32 {
    sqrt(f64::from(x)) as f32
}

#[cfg(test)]
#[expect(
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "下标来自同一用例构造的定长数组，越界即是该用例要报告的失败；\
              ulp 计数转 f64 只用于与容差比较，量级远小于 2^53"
)]
mod tests {
    extern crate std;

    use super::*;
    use std::format;
    use std::vec::Vec;

    /// 两个 f64 相差几个 ulp。
    ///
    /// 同号有限数按位模式之差算；这对 IEEE-754 的单调编码成立。
    fn ulp_difference(actual: f64, expected: f64) -> u64 {
        if actual == expected {
            return 0;
        }
        assert!(
            actual.is_finite() && expected.is_finite(),
            "{actual} 与 {expected} 应为有限数"
        );
        assert!(
            actual.signum() == expected.signum() || actual == 0.0 || expected == 0.0,
            "{actual} 与 {expected} 应同号"
        );
        actual.to_bits().abs_diff(expected.to_bits())
    }

    /// 覆盖指数全域的采样点。
    ///
    /// 量级用 `std` 的 `exp2` 构造，**不用本模块的**：拿被测函数造样本的话，
    /// 它错了样本会跟着错，判据就自证了。
    fn samples() -> Vec<f64> {
        let mut values = Vec::new();
        for exponent in [-1070i32, -1022, -700, -60, -3, -1, 0, 1, 3, 60, 700, 1020] {
            for mantissa in [1.0f64, 1.0009765625, 1.25, SQRT_2, 1.5, 1.9999] {
                let value = mantissa * f64::from(exponent).exp2();
                if value > 0.0 && value.is_finite() {
                    values.push(value);
                }
            }
        }
        values
    }

    /// 2 的整数次幂上 `log2` 与 `exp2` 必须精确。
    ///
    /// 这一层只检验规约路径：尾数恰为 1 时级数取值为 0，结果完全由指数决定。
    #[test]
    fn powers_of_two_are_exact_in_both_directions() {
        for exponent in -1022..=1023i32 {
            let power = exp2i(exponent);
            assert_eq!(
                log2(power),
                f64::from(exponent),
                "log2(2^{exponent}) 应精确"
            );
            assert_eq!(exp2(f64::from(exponent)), power, "exp2({exponent}) 应精确");
        }
        // 次正规区的 2 的幂同样要精确。
        for exponent in -1074..-1022i32 {
            let power = exp2(f64::from(exponent));
            assert!(power > 0.0, "2^{exponent} 应为正");
            assert_eq!(
                log2(power),
                f64::from(exponent),
                "次正规的 log2(2^{exponent}) 应精确"
            );
        }
    }

    /// 完全平方数上 `sqrt` 必须精确。
    #[test]
    fn perfect_squares_are_exact() {
        let mut root = 1.0f64;
        while root <= 4_294_967_296.0 {
            assert_eq!(sqrt(root * root), root, "sqrt({root}²) 应精确");
            root *= 2.0;
        }
        for integer in 1..=4096u32 {
            let root = f64::from(integer);
            assert_eq!(sqrt(root * root), root, "sqrt({integer}²) 应精确");
        }
    }

    /// 对数律与指数律：不依赖任何具体算法。
    ///
    /// 级数系数写错会同时破坏这两条，而精确点判据看不出来——2 的幂上级数根本
    /// 不参与求值。
    ///
    /// **两条都只在正规数上成立。** 次正规的有效位数不足 53 位，`left·right`
    /// 本身就带上了远大于 `log2` 自身误差的舍入，`log2(x·y) − log2(x) − log2(y)`
    /// 因此度量的是乘法的损失而不是本模块的。次正规路径由
    /// `powers_of_two_are_exact_in_both_directions` 与 ulp 对照两条覆盖。
    ///
    /// 判据取**绝对**偏差：`log2` 的结果可以任意接近 0，相对偏差在那里没有意义。
    #[test]
    fn the_algebraic_laws_hold_within_a_few_ulps() {
        let values = samples();
        let mut worst_log = 0.0f64;
        let mut worst_exp = 0u64;
        for (index, &left) in values.iter().enumerate() {
            for &right in values.iter().skip(index) {
                let product = left * right;
                if product < f64::MIN_POSITIVE || !product.is_finite() {
                    continue;
                }
                if left < f64::MIN_POSITIVE || right < f64::MIN_POSITIVE {
                    continue;
                }
                let deviation = (log2(product) - (log2(left) + log2(right))).abs();
                worst_log = worst_log.max(deviation);
                // |log2| 最大约 1074，f64 的相对分辨率 1,1×10⁻¹⁶，故绝对偏差
                // 的量级上限约 1,2×10⁻¹³；取 1×10⁻¹² 留一位余量。
                assert!(
                    deviation < 1e-12,
                    "log2({left}·{right}) 与两项之和绝对差 {deviation}"
                );
            }
        }
        for &left in &[-700.5f64, -60.25, -1.5, -0.5, 0.0, 0.5, 1.5, 60.25, 700.5] {
            for &right in &[-300.75f64, -2.5, -0.25, 0.25, 2.5, 300.75] {
                let combined = exp2(left + right);
                let separate = exp2(left) * exp2(right);
                if !combined.is_finite() || combined == 0.0 || !separate.is_finite() {
                    continue;
                }
                let difference = ulp_difference(combined, separate);
                worst_exp = worst_exp.max(difference);
                assert!(
                    difference <= 4,
                    "exp2({left}+{right}) 与两项之积相差 {difference} ulp"
                );
            }
        }
        std::println!("对数律最大绝对偏差 {worst_log:e}，指数律最大偏差 {worst_exp} ulp");
    }

    /// `exp2` 与 `log2` 互逆。
    ///
    /// **容差必须随指数量级走，这是数学必然而不是实现的余地。** `log2(x)` 的
    /// 量级到 1024 时，f64 在那里的分辨率是 `2^-42`；`exp2` 把这个误差放大成
    /// `2^-42·ln2 ≈ 1,6×10⁻¹³` 的相对误差，折合 `x` 的约 718 ulp。换句话说，
    /// 即便两个函数都正确舍入，`x = 2^-1022` 附近的往返也回不到 2 ulp 以内。
    ///
    /// 因此判据取 `|log2(x)|·ln2 + 4` 个 ulp：正比项是上面那个下界，常数项留给
    /// 两次各自的舍入。写死一个宽松的魔数会同时放过真实的缺陷。
    #[test]
    fn the_two_functions_invert_each_other() {
        let mut worst_ratio = 0.0f64;
        for &value in &samples() {
            let logarithm = log2(value);
            let round_trip = exp2(logarithm);
            if !round_trip.is_finite() || round_trip == 0.0 {
                continue;
            }
            let difference = ulp_difference(round_trip, value);
            let allowed = logarithm.abs() * core::f64::consts::LN_2 + 4.0;
            worst_ratio = worst_ratio.max(difference as f64 / allowed);
            assert!(
                (difference as f64) <= allowed,
                "exp2(log2({value})) 相差 {difference} ulp，容差 {allowed:.1}"
            );
        }
        for exponent in -1000..=1000i32 {
            let value = f64::from(exponent) * 0.5;
            let round_trip = log2(exp2(value));
            assert!(
                (round_trip - value).abs() < 1e-12,
                "log2(exp2({value})) 得 {round_trip}"
            );
        }
        std::println!("往返偏差与理论界之比最大 {worst_ratio:.3}");
    }

    /// `sqrt` 的平方回到原值。
    #[test]
    fn the_square_root_squares_back() {
        for &value in &samples() {
            let root = sqrt(value);
            let squared = root * root;
            if !squared.is_finite() || squared == 0.0 {
                continue;
            }
            let relative = ((squared - value) / value).abs();
            assert!(relative < 1e-15, "sqrt({value})² 相对偏差 {relative}");
        }
    }

    /// 与 `std` 的同名函数逐点比 ulp。
    ///
    /// `std` 走 LLVM intrinsic 与系统 libm，与本模块的级数完全独立。这是三层
    /// 判据里唯一能发现"级数收敛到了别的函数"的一层。
    #[test]
    fn every_value_is_within_one_ulp_of_the_standard_library() {
        let values = samples();
        let mut worst = [0u64; 3];
        let mut worst_at = [0.0f64; 3];

        for &value in &values {
            let pairs = [
                (sqrt(value), value.sqrt(), 0usize),
                (log2(value), value.log2(), 1),
            ];
            for (actual, expected, slot) in pairs {
                if !expected.is_finite() {
                    continue;
                }
                let difference = ulp_difference(actual, expected);
                if difference > worst[slot] {
                    worst[slot] = difference;
                    worst_at[slot] = value;
                }
            }
        }
        // 扫到 −1075：次正规区（−1074…−1022）必须覆盖，`exp2` 在那里要分两次
        // 乘，顺序错了会先把有效位丢在中间值上，只有与 std 对照才看得出来。
        let mut subnormal_worst = 0u64;
        for step in -2150..=2048i32 {
            let value = f64::from(step) * 0.5;
            let expected = value.exp2();
            if !expected.is_finite() || expected == 0.0 {
                continue;
            }
            let difference = ulp_difference(exp2(value), expected);
            if expected < f64::MIN_POSITIVE {
                subnormal_worst = subnormal_worst.max(difference);
            }
            if difference > worst[2] {
                worst[2] = difference;
                worst_at[2] = value;
            }
        }
        std::println!("次正规区 exp2 最大偏差 {subnormal_worst} ulp");

        let names = ["sqrt", "log2", "exp2"];
        let report = (0..3)
            .map(|slot| format!("{} {} ulp @ {}", names[slot], worst[slot], worst_at[slot]))
            .collect::<Vec<_>>()
            .join("，");
        std::println!("与 std 的最大偏差：{report}");
        for slot in 0..3 {
            assert!(
                worst[slot] <= 1,
                "{} 与 std 相差 {} ulp（在 {}）",
                names[slot],
                worst[slot],
                worst_at[slot]
            );
        }
    }

    /// `√2` 规约切点附近必须保住分母与换底计算的低位。
    ///
    /// 第一项曾与正确舍入结果相差 4 ulp；其余项覆盖修复过程中暴露的两侧路径，
    /// 避免只把一个反例调到容差内。
    #[test]
    fn logarithm_reduction_boundaries_stay_within_one_ulp() {
        for bits in [
            0x3ff6_a09e_6670_05c9,
            0x3ff6_a09e_6674_a03f,
            0x3ff6_a09e_666f_ff28,
            0x3fe6_eaf9_4f87_64e1,
            0x3fe7_9cdd_69fc_a78b,
        ] {
            let value = f64::from_bits(bits);
            let difference = ulp_difference(log2(value), value.log2());
            assert!(
                difference <= 1,
                "log2({value}) 在规约边界与 std 相差 {difference} ulp"
            );
        }
    }

    /// 高精度锚点：`(输入, sqrt, log2)` 的 f64 位模式。
    ///
    /// 由 `scripts/check_math.py` 以 80 位十进制独立复算并核对。级数实现全体
    /// 正确到 1 ulp 只说明它自洽，说明不了它算的是不是这个函数——`std` 对照
    /// 在目标侧不存在，锚点则是随源码一起走的那份证据。
    const ROOT_AND_LOG_ANCHORS: [(u64, u64, u64); 18] = [
        (0x0000000000000001, 0x1e60000000000000, 0xc090c80000000000),
        (0x0008000000000000, 0x1ff6a09e667f3bcd, 0xc08ff80000000000),
        (0x0010000000000000, 0x2000000000000000, 0xc08ff00000000000),
        (0x1430000000000000, 0x2a10000000000000, 0xc085e00000000000),
        (0x3c30000000000000, 0x3e10000000000000, 0xc04e000000000000),
        (0x3fb999999999999a, 0x3fd43d136248490f, 0xc00a934f0979a371),
        (0x3fe0000000000000, 0x3fe6a09e667f3bcd, 0xbff0000000000000),
        (0x3fd5555555555555, 0x3fe279a74590331c, 0xbff95c01a39fbd69),
        (0x3ff0000000000000, 0x3ff0000000000000, 0x0000000000000000),
        (0x3ff0000000000001, 0x3ff0000000000000, 0x3cb71547652b82fd),
        (0x3ff6a09e667005c9, 0x3ff306fe0a2b51da, 0x3fdfffffffc1ee40),
        (0x3ff8000000000000, 0x3ff3988e1409212e, 0x3fe2b803473f7ad1),
        (0x4000000000000000, 0x3ff6a09e667f3bcd, 0x3ff0000000000000),
        (0x4008000000000000, 0x3ffbb67ae8584caa, 0x3ff95c01a39fbd68),
        (0x4024000000000000, 0x40094c583ada5b53, 0x400a934f0979a371),
        (0x43b0000000000000, 0x41d0000000000000, 0x404e000000000000),
        (0x6bb0000000000000, 0x55d0000000000000, 0x4085e00000000000),
        (0x7fefffffffffffff, 0x5fefffffffffffff, 0x4090000000000000),
    ];

    /// 高精度锚点：`(输入, exp2)` 的 f64 位模式。
    const EXP_ANCHORS: [(u64, u64); 16] = [
        (0xc090ca0000000000, 0x0000000000000001),
        (0xc090b80000000000, 0x0000000000000010),
        (0xc08ff40000000000, 0x000b504f333f9de6),
        (0xc085e20000000000, 0x142ae89f995ad3ad),
        (0xc04e100000000000, 0x3c2d5818dcfba487),
        (0xbff8000000000000, 0x3fd6a09e667f3bcd),
        (0xbfe0000000000000, 0x3fe6a09e667f3bcd),
        (0xbe112e0be826d695, 0x3fefffffffa0bc0d),
        (0x0000000000000000, 0x3ff0000000000000),
        (0x3e112e0be826d695, 0x3ff00000002fa1f9),
        (0x3fe0000000000000, 0x3ff6a09e667f3bcd),
        (0x3ff8000000000000, 0x4006a09e667f3bcd),
        (0x400a934f0979a372, 0x4024000000000001),
        (0x404e100000000000, 0x43b172b83c7d517b),
        (0x4085e20000000000, 0x6bb306fe0a31b715),
        (0x408ffc0000000000, 0x7fe6a09e667f3bcd),
    ];

    /// 实现必须命中高精度锚点，最多差 1 ulp。
    #[test]
    fn the_high_precision_anchors_are_reproduced() {
        let mut worst = 0u64;
        for (input, expected_root, expected_log) in ROOT_AND_LOG_ANCHORS {
            let value = f64::from_bits(input);
            let root = ulp_difference(sqrt(value), f64::from_bits(expected_root));
            let logarithm = ulp_difference(log2(value), f64::from_bits(expected_log));
            worst = worst.max(root).max(logarithm);
            assert!(root <= 1, "sqrt({value:e}) 与锚点差 {root} ulp");
            assert!(logarithm <= 1, "log2({value:e}) 与锚点差 {logarithm} ulp");
        }
        for (input, expected) in EXP_ANCHORS {
            let value = f64::from_bits(input);
            let difference = ulp_difference(exp2(value), f64::from_bits(expected));
            worst = worst.max(difference);
            assert!(difference <= 1, "exp2({value}) 与锚点差 {difference} ulp");
        }
        std::println!("与高精度锚点的最大偏差 {worst} ulp");
    }

    /// 特殊值按 IEEE-754。
    #[test]
    fn the_special_values_follow_ieee_754() {
        assert!(sqrt(-1.0).is_nan(), "负数的平方根应为 NaN");
        assert!(sqrt(f64::NAN).is_nan());
        assert_eq!(sqrt(0.0), 0.0);
        assert!(
            sqrt(-0.0).is_sign_negative() && sqrt(-0.0) == 0.0,
            "sqrt(−0) 应为 −0"
        );
        assert_eq!(sqrt(f64::INFINITY), f64::INFINITY);

        assert!(log2(-1.0).is_nan(), "负数的对数应为 NaN");
        assert!(log2(f64::NAN).is_nan());
        assert_eq!(log2(0.0), f64::NEG_INFINITY);
        assert_eq!(log2(f64::INFINITY), f64::INFINITY);
        assert_eq!(log2(1.0), 0.0);

        assert!(exp2(f64::NAN).is_nan());
        assert_eq!(exp2(f64::INFINITY), f64::INFINITY);
        assert_eq!(exp2(f64::NEG_INFINITY), 0.0);
        assert_eq!(exp2(0.0), 1.0);
        assert_eq!(exp2(2000.0), f64::INFINITY, "远超上界应给 +∞");
        assert_eq!(exp2(-2000.0), 0.0, "远低于下界应给 0");
    }

    /// f32 平方根与硬件的正确舍入结果逐位相同。
    ///
    /// `f32::sqrt` 是 IEEE-754 要求正确舍入的基本运算，编译成一条硬件指令，
    /// 与本模块的牛顿迭代毫无关系。逐位相同同时说明两件事：f64 中间量的精度
    /// 足够，以及单次收窄没有触发双重舍入。
    #[test]
    fn the_single_precision_root_is_bit_identical_to_the_hardware() {
        let mut checked = 0u32;
        let mut bits = 0u32;
        while bits <= 0x7f7f_ffff {
            let value = f32::from_bits(bits);
            if value.is_finite() {
                assert_eq!(
                    sqrt_f32(value).to_bits(),
                    value.sqrt().to_bits(),
                    "sqrt_f32({value}) 与硬件结果不同"
                );
                checked += 1;
            }
            // 步长取一个与 2 的幂互素的奇数，尾数与指数都扫得到。
            bits += 0x0001_3579;
        }
        // 0x7f7fffff / 0x13579 = 27000，加上起点共 27001 个采样。
        assert_eq!(checked, 27_001, "采样数应覆盖整个 f32 正域");
        for &value in &[0.0f32, f32::MIN_POSITIVE, f32::MAX, 1e-40] {
            assert_eq!(
                sqrt_f32(value).to_bits(),
                value.sqrt().to_bits(),
                "边界 {value} 的平方根应与硬件一致"
            );
        }
    }
}
