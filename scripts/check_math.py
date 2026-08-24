#!/usr/bin/env python3
"""以高精度十进制复算 `math.rs` 的锚点表（ADR-0005）。

    ./scripts/check_math.py
    ./scripts/check_math.py --precision 120

`math.rs` 的三个函数是级数实现，没有可冻结的表，因此 ADR-0003 那套「冻结完整
位序列的 SHA-256」在这里用不上。取而代之的锚点是源码里的一张采样表：单元测试
要求实现命中它，本脚本要求它本身正确。两边都改才能一起漂移。

单元测试里还有一层与 `std` 的 ulp 对照，那一层更密，但它把正确性寄托在宿主的
libm 上，而目标侧根本没有 `std`。本脚本用 Python 的 `Decimal` 走另一条路——
`sqrt` 用其内建的正确舍入开方，`ln` 与 `exp` 用 `Decimal` 的高精度实现——与
Rust 侧的牛顿迭代和 atanh/指数级数没有共同来源。

只用标准库，不需要规范 PDF。由 CI 的 quality 检查运行。
"""

from __future__ import annotations

import argparse
import re
import struct
import sys
from decimal import Decimal, getcontext
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SOURCE = REPO_ROOT / "crates/macindecode-ac4-bitstream/src/math.rs"

ROOT_LOG_PATTERN = re.compile(
    r"const ROOT_AND_LOG_ANCHORS: \[\(u64, u64, u64\); \d+\] = \[(.*?)\];", re.S
)
EXP_PATTERN = re.compile(r"const EXP_ANCHORS: \[\(u64, u64\); \d+\] = \[(.*?)\];", re.S)
TRIPLE = re.compile(r"\(0x([0-9a-f]{16}), 0x([0-9a-f]{16}), 0x([0-9a-f]{16})\)")
PAIR = re.compile(r"\(0x([0-9a-f]{16}), 0x([0-9a-f]{16})\)")


def from_bits(bits: int) -> float:
    return struct.unpack("<d", struct.pack("<Q", bits))[0]


def to_bits(value: float) -> int:
    return struct.unpack("<Q", struct.pack("<d", value))[0]


def ulp_gap(actual: int, expected: int) -> int:
    """同号有限数的 ulp 差，按 IEEE-754 的单调编码。"""
    return abs(actual - expected)


def read_anchors() -> tuple[list[tuple[int, int, int]], list[tuple[int, int]]]:
    try:
        text = SOURCE.read_text(encoding="utf-8")
    except OSError as error:
        raise ValueError(f"无法读取 {SOURCE}：{error}") from error
    root_log = ROOT_LOG_PATTERN.search(text)
    exponential = EXP_PATTERN.search(text)
    if root_log is None or exponential is None:
        raise ValueError("在 math.rs 中找不到锚点表")
    triples = [
        (int(a, 16), int(b, 16), int(c, 16))
        for a, b, c in TRIPLE.findall(root_log.group(1))
    ]
    pairs = [(int(a, 16), int(b, 16)) for a, b in PAIR.findall(exponential.group(1))]
    if not triples or not pairs:
        raise ValueError("锚点表为空")
    return triples, pairs


def reference_sqrt(value: float) -> int:
    return to_bits(float(Decimal(value).sqrt()))


def reference_log2(value: float) -> int:
    return to_bits(float(Decimal(value).ln() / Decimal(2).ln()))


def reference_exp2(value: float) -> int:
    return to_bits(float((Decimal(value) * Decimal(2).ln()).exp()))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--precision", type=int, default=80)
    args = parser.parse_args()
    if args.precision < 40:
        print("精度过低，至少需要 40 位", file=sys.stderr)
        return 2
    getcontext().prec = args.precision

    try:
        triples, pairs = read_anchors()
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1

    failures = 0
    worst = 0
    for raw_input, raw_root, raw_log in triples:
        value = from_bits(raw_input)
        for name, stored, reference in (
            ("sqrt", raw_root, reference_sqrt(value)),
            ("log2", raw_log, reference_log2(value)),
        ):
            gap = ulp_gap(stored, reference)
            worst = max(worst, gap)
            if gap != 0:
                print(
                    f"  {name}({value!r})：锚点 0x{stored:016x}"
                    f" 与高精度复算 0x{reference:016x} 差 {gap} ulp",
                    file=sys.stderr,
                )
                failures += 1

    for raw_input, raw_expected in pairs:
        value = from_bits(raw_input)
        reference = reference_exp2(value)
        gap = ulp_gap(raw_expected, reference)
        worst = max(worst, gap)
        if gap != 0:
            print(
                f"  exp2({value!r})：锚点 0x{raw_expected:016x}"
                f" 与高精度复算 0x{reference:016x} 差 {gap} ulp",
                file=sys.stderr,
            )
            failures += 1

    total = len(triples) * 2 + len(pairs)
    if failures:
        print(f"{failures}/{total} 个锚点与高精度复算不符", file=sys.stderr)
        return 1
    print(
        f"实数函数锚点核对通过：{total} 个值在 {args.precision} 位十进制下逐位一致"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
