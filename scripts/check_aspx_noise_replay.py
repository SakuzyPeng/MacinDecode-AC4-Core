#!/usr/bin/env python3
"""比较噪声原语检查点与本地生成器重放；需要本地 NumPy，不加载参考库。

参考矩阵使用已验证接口的固定 2^23 幅度换算，不拟合相位、增益、表下标或延迟。
onset 抑制和零电平仍核对时钟，但不能据此验证噪声表值。结果仅为 observation。
"""

from __future__ import annotations
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import sys
import numpy as np

MODES = ("literal", "qmf_origin")


def integer(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError("整数字段越界")
    return value


def load_cases(path):
    rows = json.loads(path.read_text())
    if not isinstance(rows, list) or not 1 <= len(rows) <= 4096:
        raise ValueError("案例数量越界")
    ids, groups = set(), set()
    previous_group = None
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str) or re.fullmatch(r"[A-Za-z0-9]+", row["id"]) is None or row["id"] in ids:
            raise ValueError("案例身份无效或重复")
        ids.add(row["id"])
        for key, lo, hi in [("group", 0, 4096), ("factor", 1, 2), ("left_ats", 0, 3), ("right_ats", 16, 19), ("sbx", 0, 63), ("subbands", 1, 44), ("seed", 0, 511)]:
            integer(row[key], lo, hi)
        integer(row["envelopes"], 1, 4)
        if row["sbx"] + row["subbands"] > 64 or row["envelopes"] not in (1, 2, 4):
            raise ValueError("矩阵几何无效")
        if row["envelopes"] != 1 and (row["left_ats"] != 0 or row["right_ats"] != 16):
            raise ValueError("多包络构造案例的边界不符")
        if type(row["onset"]) is not bool or type(row["reset"]) is not bool:
            raise ValueError("标志必须为布尔值")
        if isinstance(row["level"], bool) or not isinstance(row["level"], (int, float)) or not math.isfinite(row["level"]) or not 0 <= row["level"] <= np.finfo(np.float32).max:
            raise ValueError("电平必须为有限非负 f32")
        if row["group"] != previous_group:
            if row["group"] in groups:
                raise ValueError("连续组不能在离开后重新出现")
            groups.add(row["group"])
            previous_group = row["group"]
    return rows


def floats(path, shape):
    count = math.prod(shape)
    if path.stat().st_size != count * 4:
        raise ValueError(f"矩阵长度不符：{path.name}")
    a = np.fromfile(path, dtype="<f4").reshape(shape)
    if not np.isfinite(a).all():
        raise ValueError(f"矩阵有非有限值：{path.name}")
    return a


def ordered_bits(a):
    bits = np.where(a == 0, np.float32(0), a).astype("<f4").view("<u4")
    return np.where(bits & 0x80000000, ~bits, bits ^ 0x80000000).astype(np.int64)


def empty_stats():
    return {"cases": 0, "clock_mismatch_cases": 0, "complex_samples": 0, "differing_components": 0, "bit_differing_components": 0, "signed_zero_only_differences": 0, "max_abs_error": 0.0, "max_ulp": 0}


def accumulate(stats, x, y, clock_diff):
    stats["cases"] += 1
    stats["clock_mismatch_cases"] += int(clock_diff)
    stats["complex_samples"] += x.size // 2
    stats["differing_components"] += int(np.count_nonzero(x != y))
    bits_differ = x.view("<u4") != y.view("<u4")
    stats["bit_differing_components"] += int(np.count_nonzero(bits_differ))
    stats["signed_zero_only_differences"] += int(np.count_nonzero(bits_differ & (x == 0) & (y == 0)))
    stats["max_abs_error"] = max(stats["max_abs_error"], float(np.max(abs(x.astype(float) - y.astype(float)))))
    stats["max_ulp"] = max(stats["max_ulp"], int(np.max(abs(ordered_bits(x) - ordered_bits(y)))))


def compare(cases_path, reference_dir, local_dir):
    cases = load_cases(cases_path)
    records = [json.loads(line) for line in (reference_dir / "noise_probes.jsonl").read_text().splitlines()]
    if len(records) != len(cases) + 1 or records[-1].get("complete") is not True or records[-1].get("cases") != len(cases):
        raise ValueError("参考记录不完整")
    references = records[:-1]
    if [r.get("id") for r in references] != [r["id"] for r in cases]:
        raise ValueError("参考调用身份或顺序不符")
    local = {}
    known = {r["id"]: r for r in cases}
    for line in (local_dir / "results.txt").read_text().splitlines():
        parts = line.split()
        if len(parts) != 7:
            raise ValueError("本地结果字段数不符")
        name, mode, *numbers = parts
        if name not in known or mode not in MODES or (name, mode) in local:
            raise ValueError("本地结果身份无效或重复")
        values = list(map(int, numbers))
        for value in values[:3]:
            integer(value, 0, 511)
        c = known[name]
        if values[3:] != [c["subbands"], (c["right_ats"] - c["left_ats"]) * c["factor"]]:
            raise ValueError("本地子带或时隙数量不符")
        local[name, mode] = values
    if len(local) != len(cases) * 2:
        raise ValueError("缺失本地案例或模式")
    table = floats(local_dir / "table.f32", (512, 2))
    if len({tuple(row) for row in table}) != 512:
        raise ValueError("表项不唯一，不能用来核对索引")
    total = {mode: empty_stats() for mode in MODES}
    by_kind, details = {}, []
    ref_model_components, ref_clock_cases, informative = 0, 0, 0
    previous_group, previous = None, 0
    continuous_end = {}
    for c, ref in zip(cases, references, strict=True):
        for key in ("group", "factor", "left_ats", "right_ats", "envelopes", "sbx", "subbands", "level", "onset"):
            if ref.get(key) != c[key]:
                raise ValueError("参考参数与声明输入不符")
        if ref.get("finite") is not True or ref.get("guards") is not True or ref.get("save") != 0:
            raise ValueError("参考保护检查失败")
        for key in ("noise_before", "noise_after"):
            integer(ref[key], 0, 511)
        if c["group"] != previous_group:
            previous_group, previous = c["group"], c["seed"]
        if c["reset"]:
            previous = 0
        if previous != ref["noise_before"]:
            raise ValueError("参考游标未接上前一区间或指定初值")
        a, b = c["left_ats"] * c["factor"], c["right_ats"] * c["factor"]
        lo, n = c["sbx"], c["subbands"]
        slots = b - a
        expected_end = (previous + slots * n) % 512
        ref_clock_cases += ref["noise_after"] != expected_end
        indices = (previous + np.arange(slots * n) + 1) % 512
        expected = (table[indices] * np.float32(c["level"])).reshape(slots, n, 2)
        if c["onset"]:
            expected.fill(0)
        raw = floats(reference_dir / (c["id"] + ".qmf.f32"), (40, 64, 2))
        x = raw[a:b, lo:lo+n].copy() * np.float32(2**23)
        raw[a:b, lo:lo+n] = 0
        if np.any(raw != 0):
            raise ValueError("参考写出了声明区间／子带之外的样本")
        ref_model_components += int(np.count_nonzero(x != expected))
        informative += c["level"] > 0 and not c["onset"]
        if c.get("kind") == "continuous":
            start = c["global_frame"] * 16 * c["factor"] + a
            if start != continuous_end.get(c["group"], 0):
                raise ValueError("连续序列的全局 QMF 区间有空洞或重叠")
            continuous_end[c["group"]] = start + slots
        kind = str(c.get("kind", "unspecified"))
        stats = by_kind.setdefault(kind, {m: empty_stats() for m in MODES})
        detail = {"id": c["id"], "factor": c["factor"], "left_ats": c["left_ats"], "reference_before": previous, "reference_after": ref["noise_after"], "informative_samples": c["level"] > 0 and not c["onset"], "modes": {}}
        for mode in MODES:
            values = local[c["id"], mode]
            y = floats(local_dir / f"{c['id']}_{mode}.f32", (slots, n, 2))
            if c["onset"]:
                y.fill(0)
            clock_diff = values[2] != ref["noise_after"]
            accumulate(total[mode], x, y, clock_diff)
            accumulate(stats[mode], x, y, clock_diff)
            detail["modes"][mode] = {"logical_before": values[0], "injected_before": values[1], "after": values[2], "clock_mismatch": clock_diff, "differing_components": int(np.count_nonzero(x != y))}
        details.append(detail)
        previous = ref["noise_after"]
    return {"schema": "macinac4.aspx-noise-replay-comparison", "version": 1, "status": "observation_only", "cases": len(cases), "informative_cases": informative, "reference_to_local_amplitude_scale": 2**23, "phase_or_index_fit": False, "reference_integer_model_mismatch_cases": int(ref_clock_cases), "reference_table_model_differing_components": ref_model_components, "continuous_groups": len(continuous_end), "total": total, "by_kind": by_kind, "details": details, "local_table_sha256": hashlib.sha256((local_dir / "table.f32").read_bytes()).hexdigest()}


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("cases", type=Path)
    p.add_argument("reference", type=Path)
    p.add_argument("local", type=Path)
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args(argv)
    try:
        report = compare(args.cases, args.reference, args.local)
        with args.output.open("x") as f:
            json.dump(report, f, ensure_ascii=False, indent=2, allow_nan=False)
            f.write("\n")
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(f"噪声重放检查失败：{exc}", file=sys.stderr)
        return 2
    print(json.dumps({"cases": report["cases"], "total": report["total"]}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
