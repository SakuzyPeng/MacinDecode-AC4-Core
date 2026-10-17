#!/usr/bin/env python3
"""核对归一化增益检查点与 Rust 测试重放输出；不加载任何参考库。

输入是本地转换器提供的 JSON 记录列表，每条记录有唯一 id、case、call、env、
bitstream_limiter、estimated，以及逐子带 [signal, noise, sine] reference。
转换器须先独立验证来源、表示换算和包络完整性。本工具检查结果身份和数值，
不把生成报告或很小的误差解释为规范符合性。
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
import re
import struct
import sys

MODES = ("off", "boost", "on")
FIELDS = ("signal_gain", "noise_level", "sine_level")


def nonnegative(value: object) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError("数值必须为有限非负数")
    value = float(value)
    if not math.isfinite(value) or value < 0:
        raise ValueError("数值必须为有限非负数")
    try:
        packed = struct.pack("<f", value)
    except OverflowError as exc:
        raise ValueError("数值超出 f32 范围") from exc
    if not math.isfinite(struct.unpack("<f", packed)[0]):
        raise ValueError("数值超出 f32 范围")
    return value


def validate_records(records: object) -> dict[str, dict]:
    if not isinstance(records, list) or not 1 <= len(records) <= 4096:
        raise ValueError("检查点数量超界")
    by_id = {}
    identities = set()
    for row in records:
        if not isinstance(row, dict):
            raise ValueError("检查点须为对象")
        name = row.get("id")
        if not isinstance(name, str) or not name or any(c.isspace() for c in name) or name in by_id:
            raise ValueError("检查点 id 缺失或重复")
        case = row.get("case")
        if not isinstance(case, str) or not case:
            raise ValueError("缺少案例身份")
        for key in ("call", "env", "bitstream_limiter"):
            if type(row.get(key)) is not int or row[key] < 0:
                raise ValueError("调用、包络、标志须为非负整数")
        if row["env"] >= 5 or row["bitstream_limiter"] not in (0, 1):
            raise ValueError("包络或 limiter 标志超界")
        identity = (case, row["call"], row["env"], row["bitstream_limiter"])
        if identity in identities:
            raise ValueError("调用与包络身份重复")
        identities.add(identity)
        estimated = row.get("estimated")
        reference = row.get("reference")
        if not isinstance(estimated, list) or not 1 <= len(estimated) <= 64:
            raise ValueError("子带数量超界")
        if not isinstance(reference, list) or len(reference) != len(estimated):
            raise ValueError("参考子带数量不符")
        for value, expected in zip(estimated, reference, strict=True):
            nonnegative(value)
            if not isinstance(expected, list) or len(expected) != 3:
                raise ValueError("参考矩阵必须有信号、噪声、音调三列")
            for item in expected:
                nonnegative(item)
        by_id[name] = row
    return by_id


def parse_local(text: str, records: dict[str, dict]) -> dict[tuple, tuple]:
    values = {}
    for line in text.splitlines():
        words = line.split()
        if len(words) != 7:
            raise ValueError("重放行字段数不符")
        name, mode, index, *bits = words
        if name not in records or mode not in MODES:
            raise ValueError("未知检查点或模式")
        try:
            index = int(index)
        except ValueError as exc:
            raise ValueError("无效子带下标") from exc
        if not 0 <= index < len(records[name]["reference"]):
            raise ValueError("子带下标超界")
        key = name, mode, index
        if key in values:
            raise ValueError("重放行身份重复")
        if any(re.fullmatch(r"[0-9a-fA-F]{8}", b) is None for b in bits):
            raise ValueError("无效 f32 位表示")
        decoded = tuple(struct.unpack("<f", struct.pack("<I", int(b, 16)))[0] for b in bits)
        for value in decoded:
            nonnegative(value)
        values[key] = decoded
    expected = sum(len(row["reference"]) * len(MODES) for row in records.values())
    if len(values) != expected:
        raise ValueError("缺失检查点、模式或子带")
    return values


def f32_bits(value: float) -> int:
    # +0 和 -0 在本数值比较中等价。
    return struct.unpack("<I", struct.pack("<f", value if value else 0.0))[0]


def statistics(pairs: list[tuple[float, float]]) -> dict:
    errors = [abs(a - b) for a, b in pairs]
    energy = math.fsum(b * b for _, b in pairs)
    error_energy = math.fsum(d * d for d in errors)
    return {
        "values": len(pairs),
        "max_abs_error": max(errors, default=0),
        "max_relative_error_nonzero": max((d / abs(b) for d, (_, b) in zip(errors, pairs, strict=True) if b), default=0),
        "zero_reference_values": sum(b == 0 for _, b in pairs),
        "max_zero_reference_abs_error": max((d for d, (_, b) in zip(errors, pairs, strict=True) if b == 0), default=0),
        "max_ulp": max((abs(f32_bits(a) - f32_bits(b)) for a, b in pairs), default=0),
        "f32_equal_values": sum(f32_bits(a) == f32_bits(b) for a, b in pairs),
        "sum_squared_error": error_energy,
        "relative_l2_error": math.sqrt(error_energy / energy) if energy else None,
    }


def compare(records: object, local_text: str) -> dict:
    by_id = validate_records(records)
    local = parse_local(local_text, by_id)
    groups: dict[tuple, list[list[tuple[float, float]]]] = {}
    per_record = []
    for name, row in by_id.items():
        for mode in MODES:
            pairs = [[(local[name, mode, sb][column], ref[column]) for sb, ref in enumerate(row["reference"])] for column in range(3)]
            key = row["case"], row["bitstream_limiter"], mode
            target = groups.setdefault(key, [[], [], []])
            for column in range(3):
                target[column].extend(pairs[column])
            per_record.append({"id": name, "mode": mode, "fields": {field: statistics(pairs[c]) for c, field in enumerate(FIELDS)}})
    return {
        "schema": "macinac4.aspx-gain-replay-comparison",
        "version": 1,
        "status": "observation_only",
        "records": len(by_id),
        "subband_envelopes": sum(len(r["reference"]) for r in by_id.values()),
        "ulp_reference": "normalized reference rounded to f32; absolute/relative errors retain input precision",
        "groups": [{"case": case, "bitstream_limiter": flag, "mode": mode, "fields": {field: statistics(pairs[c]) for c, field in enumerate(FIELDS)}} for (case, flag, mode), pairs in sorted(groups.items())],
        "per_record": per_record,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkpoints", type=Path)
    parser.add_argument("local", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        report = compare(json.loads(args.checkpoints.read_text()), args.local.read_text())
        with args.output.open("x") as output:
            json.dump(report, output, ensure_ascii=False, indent=2, allow_nan=False)
            output.write("\n")
    except (OSError, ValueError) as exc:
        print(f"增益检查失败：{exc}", file=sys.stderr)
        return 2
    print(f"已比较 {report['records']} 个包络、{report['subband_envelopes']} 个子带包络，结果仅为观察。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
