#!/usr/bin/env python3
"""以高精度十进制复算 `5.5` 的三张常量表与 QMF 调制表，核对冻结摘要。

    ./scripts/check_transform_tables.py              核对摘要与结构判据
    ./scripts/check_transform_tables.py --precision 200

ADR-0003 用锁定版本的 `libm` 生成旋转因子、KBD 左窗、IFFT 根与 QMF 调制表，
并冻结完整位序列的 SHA-256。摘要保证所有构建产出同一张表，**但不保证表值正确**——
所有机器会一致地产出同一个错值。构建期的结构判据也补不上这个缺口：平方恒等式只证明
内部配对自洽，共同角度偏移能同时逃过它、象限检查和单调性检查（实测三类扰动
中唯有摘要抓得住）。

本脚本提供那个缺失的锚点。三角路径不复用生成侧实现：π 由 Machin 公式算出，
`cos`/`sin` 用泰勒级数；旋转角本就在第一象限，IFFT 根先以整数指数约到第一
象限，再用换位、变号与共轭派生完整圆周。KBD 必须执行规范
给定的 `I₀` 级数；本脚本用 Decimal 高精度和不同终止判据重算，能核对生成侧
的 f64 取值、终止与舍入，但不把同一数学级数包装成第二种算法。f32 舍入由高精度
十进制中点比较决定。

只用标准库，不需要规范 PDF，也不需要 `pdfplumber`。ADR-0003 要求高精度工具
不进入构建依赖，故它是独立脚本，`build.rs` 不调用它。脚本会反向读取
`build.rs` 与 `build_support/` 的四份生产摘要并与 ADR 交叉核对，保证生产表与独立审计不能各自漂移。
**任何摘要更新都必须先跑通本脚本。**
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from decimal import Decimal, getcontext
from pathlib import Path

# 与 `asf::tables::TRANSFORM_LENGTHS_48` 同序；表值布局依赖这个顺序。
LENGTHS = [2048, 1920, 1536, 1024, 960, 768, 512, 480, 384, 256, 240, 192, 128, 120, 96]
# 表 186 的 α，以半整数存放（`6` 即 α = 3），与 `asf::imdct::KBD_ALPHA_HALVES_48` 一致。
ALPHA_HALVES = [6, 6, 6, 8, 8, 8, 9, 9, 9, 10, 10, 10, 12, 12, 12]

# ADR-0003 是审计期的规范来源；脚本不另存副本，并核对构建源码的生产副本。
ADR_PATH = (
    Path(__file__).resolve().parent.parent
    / "docs/decisions/0003-trigonometric-tables-for-the-transform.md"
)
BUILD_CRATE_PATH = (
    Path(__file__).resolve().parent.parent
    / "crates/macindecode-ac4-decode"
)
ADR_TABLE_MARKERS = {
    "旋转因子": "`IMDCT_ROTATION`",
    "KBD 左窗": "`KBD_LEFT`",
    "IFFT 根": "`IFFT_ROOTS`",
    "QMF 调制": "`QMF_MODULATION`",
}
BUILD_DIGEST_CONSTANTS = {
    "旋转因子": "IMDCT_ROTATION_SHA256",
    "KBD 左窗": "KBD_LEFT_SHA256",
    "IFFT 根": "IFFT_ROOTS_SHA256",
    "QMF 调制": "QMF_MODULATION_SHA256",
}
SHA256_PATTERN = re.compile(r"(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])")

# 构建期结构判据允许的偏差，见规范可追踪性 5.18。
TOLERANCE = 1e-7


def load_expected_digests() -> dict[str, str]:
    """从 ADR-0003 的表格读取四份冻结摘要，格式漂移时闭锁失败。"""
    text = ADR_PATH.read_text(encoding="utf-8")
    expected = {}
    for name, marker in ADR_TABLE_MARKERS.items():
        rows = [
            line
            for line in text.splitlines()
            if line.lstrip().startswith("|") and marker in line
        ]
        if len(rows) != 1:
            raise ValueError(f"ADR-0003 中 {marker} 表格行应恰有一处，实得 {len(rows)} 处")
        digests = SHA256_PATTERN.findall(rows[0])
        if len(digests) != 1:
            raise ValueError(f"ADR-0003 的 {marker} 行应恰有一份 SHA-256，实得 {len(digests)} 份")
        expected[name] = digests[0]
    return expected


def load_build_digests() -> dict[str, str]:
    """读取生产构建使用的摘要；常量缺失、重复或格式漂移时闭锁失败。"""
    paths = [BUILD_CRATE_PATH / "build.rs"]
    paths.extend(sorted((BUILD_CRATE_PATH / "build_support").rglob("*.rs")))
    text = "\n".join(path.read_text(encoding="utf-8") for path in paths)
    expected = {}
    for name, constant in BUILD_DIGEST_CONSTANTS.items():
        pattern = re.compile(
            rf"(?m)^const\s+{re.escape(constant)}\s*:\s*&str\s*=\s*"
            rf'"([0-9a-f]{{64}})";'
        )
        digests = pattern.findall(text)
        if len(digests) != 1:
            raise ValueError(
                f"build.rs/build_support 中 {constant} 应恰有一份 SHA-256，实得 {len(digests)} 份"
            )
        expected[name] = digests[0]
    return expected


def series_floor() -> Decimal:
    """级数终止阈值。

    `Decimal` 不会下溢到零，逐项累加必须用相对阈值收尾，否则循环不终止。
    """
    return Decimal(10) ** -(getcontext().prec + 5)


def arctan_inv(n: int) -> Decimal:
    """arctan(1/n)，用于 Machin 公式。"""
    tiny = series_floor()
    total = Decimal(0)
    term = Decimal(1) / n
    n_squared = Decimal(n) * n
    k = 0
    while True:
        contribution = term / (2 * k + 1)
        if contribution < tiny:
            return total
        total += contribution if k % 2 == 0 else -contribution
        term /= n_squared
        k += 1


def cos_taylor(x: Decimal) -> Decimal:
    """cos(x)。全部角度落在 `(0, π/2)`，无需参数规约。"""
    tiny = series_floor()
    total, term, k = Decimal(1), Decimal(1), 0
    while True:
        k += 1
        term = term * x * x / ((2 * k - 1) * (2 * k))
        if term < abs(total) * tiny:
            return total
        total += term if k % 2 == 0 else -term


def sin_taylor(x: Decimal) -> Decimal:
    tiny = series_floor()
    total, term, k = x, x, 0
    while True:
        k += 1
        term = term * x * x / ((2 * k) * (2 * k + 1))
        if term < abs(total) * tiny:
            return total
        total += term if k % 2 == 0 else -term


def bessel_i0(x: Decimal) -> Decimal:
    """`I(x) = Σ (x^k / (2^k k!))²`。

    全正项，先增后减；峰值在 `k ≈ x/2` 附近，故收尾判据要等过峰后才生效。
    """
    tiny = series_floor()
    half = x / 2
    total, term, k = Decimal(1), Decimal(1), 0
    peak = int(x) + 2
    while True:
        k += 1
        term = term * half / k
        squared = term * term
        if k > peak and squared < total * tiny:
            return total
        total += squared


class Rounder:
    """把高精度值正确舍入为 f32 位模式，就近偶数。

    同时记录余数最接近中点的距离：若它远大于工作精度的误差，则每一项的舍入
    判定都是确凿的，结论不在精度边缘。
    """

    def __init__(self) -> None:
        self.powers = {e: Decimal(2) ** e for e in range(-80, 40)}
        self.lower, self.upper = Decimal(1 << 23), Decimal(1 << 24)
        self.half = Decimal("0.5")
        self.closest_to_midpoint = Decimal(1)

    def bits(self, value: Decimal) -> int:
        if value == 0:
            return 0
        sign = 1 if value < 0 else 0
        magnitude = abs(value)

        # 由十进制指数估出二进制指数，再校正到 2^23 ≤ m < 2^24。
        exponent = int((magnitude.adjusted() + 1) * Decimal("3.3219280948873623")) - 24
        while magnitude / self.powers[exponent] >= self.upper:
            exponent += 1
        while magnitude / self.powers[exponent] < self.lower:
            exponent -= 1

        scaled = magnitude / self.powers[exponent]
        mantissa = int(scaled)
        remainder = scaled - mantissa
        self.closest_to_midpoint = min(
            self.closest_to_midpoint, abs(remainder - self.half)
        )
        if remainder > self.half or (remainder == self.half and mantissa % 2 == 1):
            mantissa += 1
        if mantissa == 1 << 24:
            mantissa = 1 << 23
            exponent += 1

        biased = exponent + 23 + 127
        if not 1 <= biased <= 254:
            raise ValueError(f"指数 {biased} 超出 f32 规格化范围")
        return (sign << 31) | (biased << 23) | (mantissa & 0x7F_FFFF)


def f32_from_bits(bits: int) -> float:
    """把位模式还原为 Python float，用于结构判据。"""
    import struct

    return struct.unpack("<f", bits.to_bytes(4, "little"))[0]


def build_rotation(pi: Decimal, rounder: Rounder) -> list[list[tuple[int, int]]]:
    """`xcos1[k] = −cos(2π(8k+1)/16N)`、`xsin1[k] = −sin(…)`，见 `Pseudocode 60`。"""
    tables = []
    for n in LENGTHS:
        pairs = []
        for k in range(n // 2):
            angle = pi * (8 * k + 1) / (8 * n)  # 2π(8k+1)/(16N)
            pairs.append(
                (
                    rounder.bits(-cos_taylor(angle)),
                    rounder.bits(-sin_taylor(angle)),
                )
            )
        tables.append(pairs)
    return tables


def build_kbd(pi: Decimal, rounder: Rounder) -> list[list[int]]:
    """`KBD_LEFT(N,n) = √(S(n)/S(N))`，见 `5.5.3`。

    `W` 按 `p = 0…N` 求和，含端点共 `N+1` 项；`α` 按 `N_W` 查表 186。
    """
    tables = []
    for index, nw in enumerate(LENGTHS):
        pi_alpha = pi * ALPHA_HALVES[index] / 2
        denominator = bessel_i0(pi_alpha)

        weights = []
        for p in range(nw + 1):
            ratio = Decimal(2 * p) / nw - 1
            inner = 1 - ratio * ratio
            argument = pi_alpha * (inner.sqrt() if inner > 0 else Decimal(0))
            weights.append(bessel_i0(argument) / denominator)

        total = sum(weights)
        column, prefix = [], Decimal(0)
        for p in range(nw):
            prefix += weights[p]
            column.append(rounder.bits((prefix / total).sqrt()))
        tables.append(column)
    return tables


def negate_f32_bits(bits: int) -> int:
    """逐位变号，并把数学上的零统一规范化为 `+0.0`。"""
    return 0 if bits & 0x7FFF_FFFF == 0 else bits ^ 0x8000_0000


def build_ifft_roots(
    pi: Decimal, rounder: Rounder
) -> list[list[tuple[int, int]]]:
    """`exp(+j 2πe/M)`，`M=N/2`，见 `Pseudocode 61` 与 ADR-0004。"""
    tables = []
    for n in LENGTHS:
        length = n // 2
        quarter, half = length // 4, length // 2
        roots = [(0, 0)] * length
        roots[0] = (0x3F80_0000, 0)
        roots[quarter] = (0, 0x3F80_0000)
        roots[half] = (0xBF80_0000, 0)

        for offset in range(1, quarter):
            angle = 2 * pi * offset / length
            cosine = rounder.bits(cos_taylor(angle))
            sine = rounder.bits(sin_taylor(angle))
            roots[offset] = (cosine, sine)
            roots[quarter + offset] = (negate_f32_bits(sine), cosine)

        for exponent in range(1, half + 1):
            real, imaginary = roots[exponent]
            roots[length - exponent] = (real, negate_f32_bits(imaginary))
        tables.append(roots)
    return tables


def build_qmf_modulation(pi: Decimal, rounder: Rounder) -> list[tuple[int, int]]:
    """`exp(+j 2πk/512)`，见 `5.7.3`/`5.7.4` 与 ADR-0003 第 3 条的第四张表。

    与 `build_ifft_roots` 同一套派生：只在第一象限求值，轴点精确写入，其余三
    象限由换位与共轭得出，因此数学上的零不会变成有限精度 π 带来的残差。
    """
    points = 512
    quarter, half = points // 4, points // 2
    roots: list[tuple[int, int]] = [(0, 0)] * points
    roots[0] = (0x3F80_0000, 0)
    roots[quarter] = (0, 0x3F80_0000)
    roots[half] = (0xBF80_0000, 0)
    for offset in range(1, quarter):
        angle = 2 * pi * offset / points
        cosine = rounder.bits(cos_taylor(angle))
        sine = rounder.bits(sin_taylor(angle))
        roots[offset] = (cosine, sine)
        roots[quarter + offset] = (negate_f32_bits(sine), cosine)
    for exponent in range(1, half + 1):
        real, imaginary = roots[exponent]
        roots[points - exponent] = (real, negate_f32_bits(imaginary))
    return roots


def digest_qmf_modulation(roots: list[tuple[int, int]]) -> bytes:
    return digest_rotation([roots])


def check_qmf_modulation_structure(roots: list[tuple[int, int]]) -> list[str]:
    """单位圆、共轭对称与四个轴点精确。"""
    problems, worst = [], 0.0
    for index, (cosine_bits, sine_bits) in enumerate(roots):
        cosine, sine = f32_from_bits(cosine_bits), f32_from_bits(sine_bits)
        worst = max(worst, abs(cosine * cosine + sine * sine - 1.0))
    if worst > 1e-6:
        problems.append(f"QMF 调制表模平方最大偏差 {worst:.3e}")
    axes = {0: (1.0, 0.0), 128: (0.0, 1.0), 256: (-1.0, 0.0), 384: (0.0, -1.0)}
    for index, (want_cos, want_sin) in axes.items():
        got = (f32_from_bits(roots[index][0]), f32_from_bits(roots[index][1]))
        if got != (want_cos, want_sin):
            problems.append(f"QMF 调制表第 {index} 项应为 {(want_cos, want_sin)}，实得 {got}")
    for exponent in range(1, 256):
        real, imaginary = roots[exponent]
        conj_real, conj_imaginary = roots[512 - exponent]
        if real != conj_real or imaginary != negate_f32_bits(conj_imaginary):
            problems.append(f"QMF 调制表第 {exponent} 项与第 {512 - exponent} 项不共轭")
            break
    if not problems:
        print(f"  QMF 调制表：512 项模平方最大偏差 {worst:.3e}，轴点精确，共轭对称")
    return problems


def digest_rotation(tables: list[list[tuple[int, int]]]) -> bytes:
    blob = bytearray()
    for pairs in tables:
        for cosine, sine in pairs:
            blob += cosine.to_bytes(4, "little") + sine.to_bytes(4, "little")
    return bytes(blob)


def digest_kbd(tables: list[list[int]]) -> bytes:
    blob = bytearray()
    for column in tables:
        for value in column:
            blob += value.to_bytes(4, "little")
    return bytes(blob)


def digest_ifft_roots(tables: list[list[tuple[int, int]]]) -> bytes:
    return digest_rotation(tables)


def check_rotation_structure(tables: list[list[tuple[int, int]]]) -> list[str]:
    """构建期对旋转因子的结构判据，见规范可追踪性 5.18。"""
    problems, worst = [], 0.0
    for n, pairs in zip(LENGTHS, tables):
        previous = None
        for k, (cosine_bits, sine_bits) in enumerate(pairs):
            cosine, sine = f32_from_bits(cosine_bits), f32_from_bits(sine_bits)
            deviation = abs(cosine * cosine + sine * sine - 1.0)
            worst = max(worst, deviation)
            if deviation > TOLERANCE:
                problems.append(f"N={n} k={k}：单位圆偏差 {deviation:.3e}")
            if not (cosine < 0 and sine < 0):
                problems.append(f"N={n} k={k}：应同在第三象限，实得 {cosine}, {sine}")
            if previous is not None and not (cosine > previous[0] and sine < previous[1]):
                problems.append(f"N={n} k={k}：xcos1 应递增、xsin1 应递减")
            previous = (cosine, sine)
    print(f"  旋转因子单位圆最大偏差 {worst:.3e}（容差 {TOLERANCE:.0e}）")
    return problems


def check_kbd_structure(tables: list[list[int]]) -> list[str]:
    """构建期对 KBD 左窗的结构判据。"""
    problems, worst = [], 0.0
    for n, column in zip(LENGTHS, tables):
        values = [f32_from_bits(bits) for bits in column]
        for index, value in enumerate(values):
            deviation = abs(value * value + values[n - 1 - index] ** 2 - 1.0)
            worst = max(worst, deviation)
            if deviation > TOLERANCE:
                problems.append(f"N={n} n={index}：Princen-Bradley 偏差 {deviation:.3e}")
            if not 0.0 < value <= 1.0:
                problems.append(f"N={n} n={index}：值 {value} 越出 (0, 1]")
        for index, (low, high) in enumerate(zip(values, values[1:])):
            if high < low:
                problems.append(f"N={n} n={index}：窗值应单调不减")
    print(f"  KBD Princen-Bradley 最大偏差 {worst:.3e}（容差 {TOLERANCE:.0e}）")
    return problems


def check_ifft_root_structure(
    tables: list[list[tuple[int, int]]],
) -> list[str]:
    """IFFT 根的轴点、共轭关系、象限与单位圆判据。"""
    problems, worst = [], 0.0
    for n, roots in zip(LENGTHS, tables):
        length, quarter = n // 2, n // 8
        axes = [
            (0, (0x3F80_0000, 0)),
            (quarter, (0, 0x3F80_0000)),
            (2 * quarter, (0xBF80_0000, 0)),
            (3 * quarter, (0, 0xBF80_0000)),
        ]
        for exponent, expected in axes:
            if roots[exponent] != expected:
                problems.append(
                    f"M={length} e={exponent}：轴点 {roots[exponent]} 应为 {expected}"
                )

        for exponent, (real_bits, imaginary_bits) in enumerate(roots):
            real = f32_from_bits(real_bits)
            imaginary = f32_from_bits(imaginary_bits)
            deviation = abs(real * real + imaginary * imaginary - 1.0)
            worst = max(worst, deviation)
            if deviation > TOLERANCE:
                problems.append(
                    f"M={length} e={exponent}：单位圆偏差 {deviation:.3e}"
                )
            if exponent and (
                roots[length - exponent][0] != real_bits
                or roots[length - exponent][1] != negate_f32_bits(imaginary_bits)
            ):
                problems.append(f"M={length} e={exponent}：共轭关系不成立")

        for exponent in range(1, quarter):
            real_bits, imaginary_bits = roots[exponent]
            if real_bits & 0x8000_0000 or imaginary_bits & 0x8000_0000:
                problems.append(f"M={length} e={exponent}：第一象限符号错误")
    print(f"  IFFT 根单位圆最大偏差 {worst:.3e}（容差 {TOLERANCE:.0e}）")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--precision",
        type=int,
        default=100,
        help="十进制工作精度，默认 100 位（约 332 bit）",
    )
    arguments = parser.parse_args()
    getcontext().prec = arguments.precision

    try:
        expected = load_expected_digests()
        build_expected = load_build_digests()
    except (OSError, ValueError) as error:
        print(f"无法读取冻结摘要：{error}", file=sys.stderr)
        return 1

    pi = 4 * (4 * arctan_inv(5) - arctan_inv(239))
    rounder = Rounder()

    print(f"以 {arguments.precision} 位十进制复算，π = {str(pi)[:32]}…")
    rotation = build_rotation(pi, rounder)
    kbd = build_kbd(pi, rounder)
    ifft_roots = build_ifft_roots(pi, rounder)
    qmf_modulation = build_qmf_modulation(pi, rounder)

    problems: list[str] = []
    for name in expected:
        if build_expected[name] != expected[name]:
            problems.append(
                f"{name} 的构建源码生产摘要与 ADR-0003 不符：\n"
                f"    build source {build_expected[name]}\n"
                f"    ADR-0003 {expected[name]}"
            )
    if not problems:
        print(f"  构建源码 {len(expected)} 份生产摘要与 ADR-0003 一致")

    blobs = {
        "旋转因子": digest_rotation(rotation),
        "KBD 左窗": digest_kbd(kbd),
        "IFFT 根": digest_ifft_roots(ifft_roots),
        "QMF 调制": digest_qmf_modulation(qmf_modulation),
    }
    for name, blob in blobs.items():
        actual = hashlib.sha256(blob).hexdigest()
        status = "一致" if actual == expected[name] else "**不符**"
        print(f"  {name} {len(blob)} 字节  SHA-256 {actual[:16]}… {status}")
        if actual != expected[name]:
            problems.append(
                f"{name} 摘要与 ADR-0003 记录不符：\n    实得 {actual}\n    应为 {expected[name]}"
            )

    problems += check_rotation_structure(rotation)
    problems += check_kbd_structure(kbd)
    problems += check_ifft_root_structure(ifft_roots)
    problems += check_qmf_modulation_structure(qmf_modulation)

    margin = rounder.closest_to_midpoint
    print(f"  舍入余数距中点最近 {margin:.3e}")
    if margin < Decimal(10) ** -(arguments.precision // 2):
        problems.append(
            f"某项舍入余数距中点仅 {margin:.3e}，接近工作精度，判定不可靠；请提高 --precision"
        )

    if problems:
        print(f"\n变换常量表核对失败 {len(problems)} 处：")
        for line in problems:
            print(f"  {line}")
        return 1

    lines = (
        sum(len(pairs) for pairs in rotation) * 2
        + sum(len(c) for c in kbd)
        + sum(len(roots) for roots in ifft_roots) * 2
        + len(qmf_modulation) * 2
    )
    print(f"\n核对通过：{len(LENGTHS)} 个变换长度，{lines} 个 f32 与 ADR-0003 冻结值一致")
    return 0


if __name__ == "__main__":
    sys.exit(main())
