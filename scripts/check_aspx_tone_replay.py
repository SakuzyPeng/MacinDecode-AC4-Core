#!/usr/bin/env python3
"""比较音调原语与现有生产 generate 的四种测试调用方式。

固定使用 native × 2^23 幅度换算和 next=(last+1)%4 状态表示转换；
不拟合相位、幅度或延迟。qmf_origin 直接调用生产，其余模式仅在测试端代换。
兼容修复前的历史输出，并在报告中明确其旧生产起点。
"""
from __future__ import annotations
import argparse
from collections import Counter
import json
import math
from pathlib import Path
import re
import sys
import numpy as np

if __package__:
    from .check_aspx_noise_replay import accumulate, empty_stats, floats, integer
else:
    from check_aspx_noise_replay import accumulate, empty_stats, floats, integer

MODES = ("literal", "qmf_origin", "literal_initial_zero", "qmf_origin_initial_zero")
LEGACY_ALIASES = {"production": "literal", "initial_zero": "literal_initial_zero"}
LEGACY_MODES = ("production", "qmf_origin", "initial_zero", "qmf_origin_initial_zero")
FIELDS = ("group", "first_frame", "factor", "left_ats", "right_ats", "envelopes", "master", "start_freq", "stop_freq", "noise_sbg", "xover", "sbx", "subbands", "seed", "onset", "pattern", "level", "profile", "force_present")
TABLE = np.array([(1, 0), (0, 1), (-1, 0), (0, -1)], dtype="<f4")


def load_cases(path):
    cases, ids, groups, previous_group = [], set(), set(), None
    for line in path.read_text().splitlines():
        parts = line.split()
        if len(parts) != 20 or re.fullmatch(r"[A-Za-z0-9]+", parts[0]) is None or parts[0] in ids:
            raise ValueError("案例字段数或身份无效")
        row = dict(id=parts[0])
        ids.add(parts[0])
        for name, value in zip(FIELDS, parts[1:], strict=True):
            row[name] = float(value) if name == "level" else int(value)
        for name in ("first_frame", "onset", "force_present", "master"):
            integer(row[name], 0, 1)
        for name, lo, hi in [("group", 0, 4096), ("factor", 1, 2), ("left_ats", 0, 3), ("right_ats", 16, 19), ("envelopes", 1, 4), ("start_freq", 0, 15), ("stop_freq", 0, 15), ("noise_sbg", 0, 3), ("xover", 0, 31), ("sbx", 0, 63), ("subbands", 1, 44), ("seed", 0, 3), ("pattern", 0, 5), ("profile", 0, 2)]:
            integer(row[name], lo, hi)
        if row["sbx"] + row["subbands"] > 64 or row["envelopes"] not in (1, 2, 4):
            raise ValueError("矩阵几何无效")
        if row["envelopes"] != 1 and (row["left_ats"] != 0 or row["right_ats"] != 16):
            raise ValueError("多包络构造边界不符")
        if not math.isfinite(row["level"]) or not 0 <= row["level"] <= np.finfo(np.float32).max:
            raise ValueError("电平必须为有限非负 f32")
        if row["group"] != previous_group:
            if row["group"] in groups:
                raise ValueError("连续组不能离开后重新出现")
            groups.add(row["group"])
            previous_group = row["group"]
        cases.append(row)
    if not 1 <= len(cases) <= 4096:
        raise ValueError("案例数量越界")
    return cases


def levels(c, env):
    lo, n, pattern = c["sbx"], c["subbands"], c["pattern"]
    bands = np.arange(lo, lo+n)
    masks = [np.ones(n, dtype=bool), bands % 2 == 0, bands % 2 == 1, bands == lo, bands == lo+n-1, np.zeros(n, dtype=bool)]
    value = np.float32(c["level"])
    if c["profile"] == 1 and env % 2 == 0:
        value = np.float32(0)
    if c["profile"] == 2:
        value = np.float32(value / 2**env)
    return masks[pattern].astype("<f4") * value


def model(seed, count, lo, amplitude):
    x = TABLE[(seed + np.arange(count)) % 4, None, :] * amplitude[None, :, None]
    x[:, :, 1] *= np.where(np.arange(lo, lo+len(amplitude)) % 2 == 0, 1, -1)
    return x


def stats():
    return {**empty_stats(), "max_relative_error_nonzero_reference": 0.0, "nonzero_against_zero_reference": 0}


def add_stats(s, x, y, different_clock):
    accumulate(s, x, y, different_clock)
    nz = x != 0
    if np.any(nz):
        s["max_relative_error_nonzero_reference"] = max(s["max_relative_error_nonzero_reference"], float(np.max(np.abs((y[nz].astype(float)-x[nz]) / x[nz]))))
    s["nonzero_against_zero_reference"] += int(np.count_nonzero((~nz) & (y != 0)))


def compare(cases_path, reference_dir, local_dir):
    cases = load_cases(cases_path)
    records = [json.loads(s) for s in (reference_dir / "tone_probes.jsonl").read_text().splitlines()]
    if len(records) != len(cases)+1 or records[-1] != {"complete": True, "cases": len(cases)}:
        raise ValueError("参考记录不完整")
    if [r.get("id") for r in records[:-1]] != [c["id"] for c in cases]:
        raise ValueError("参考调用身份或顺序不符")
    known = {c["id"]: c for c in cases}
    local, source_modes, observed_modes = {}, {}, set()
    for line in (local_dir / "results.txt").read_text().splitlines():
        p = line.split()
        if len(p) != 9:
            raise ValueError("本地结果字段数不符")
        name, source_mode, *numbers = p
        mode = LEGACY_ALIASES.get(source_mode, source_mode)
        observed_modes.add(source_mode)
        if name not in known or mode not in MODES or (name, mode) in local:
            raise ValueError("本地结果身份无效或重复")
        values = list(map(int, numbers))
        for value in values[:4]:
            integer(value, 0, 3)
        c = known[name]
        if values[3] != (values[2]+1) % 4 or values[4:6] != [c["subbands"], (c["right_ats"]-c["left_ats"])*c["factor"]]:
            raise ValueError("本地状态表示或矩阵几何不符")
        integer(values[6], 0, 1)
        local[name, mode] = values
        source_modes[name, mode] = source_mode
    if len(local) != len(cases)*len(MODES):
        raise ValueError("遗漏本地案例或模式")
    if observed_modes == set(LEGACY_MODES):
        legacy = True
    elif observed_modes == set(MODES):
        legacy = False
    else:
        raise ValueError("混用历史与当前重放模式")
    group_counts = Counter(c["group"] for c in cases)
    total, by_kind, details = {m: stats() for m in MODES}, {}, []
    previous_group, ref_previous, noise_previous, local_previous = None, 0, 0, {}
    model_diff, clock_diff, informative, envelope_count = 0, 0, 0, 0
    for c, ref in zip(cases, records[:-1], strict=True):
        for key in ("group", "factor", "left_ats", "right_ats", "envelopes", "sbx", "subbands", "level", "onset", "first_frame", "pattern", "profile", "force_present"):
            if (np.float32(ref.get(key)) != np.float32(c[key]) if key == "level" else ref.get(key) != c[key]):
                raise ValueError("参考参数与声明输入不符")
        if ref.get("finite") is not True or ref.get("guards") is not True or ref.get("save") != 0:
            raise ValueError("参考保护检查失败")
        for key in ("sine_before", "sine_after"):
            integer(ref[key], 0, 3)
        for key in ("noise_before", "noise_after"):
            integer(ref[key], 0, 511)
        if c["group"] != previous_group:
            previous_group, ref_previous, noise_previous = c["group"], c["seed"], 0
            local_previous = {m: (c["seed"]+3) % 4 for m in MODES}
        if c["first_frame"]:
            ref_previous = 0
        if ref["sine_before"] != ref_previous or ref["noise_before"] != noise_previous:
            raise ValueError("参考游标未接上指定初值或前一区间")
        a, b = c["left_ats"]*c["factor"], c["right_ats"]*c["factor"]
        n, lo, slots = c["subbands"], c["sbx"], b-a
        if len(ref.get("envelope_checks", [])) != c["envelopes"]:
            raise ValueError("遗漏包络")
        expected = np.zeros((slots, n, 2), dtype="<f4")
        phase = ref_previous
        for env, check in enumerate(ref["envelope_checks"]):
            start, stop = a+slots*env//c["envelopes"], a+slots*(env+1)//c["envelopes"]
            amp = levels(c, env)
            present = int(bool(c["force_present"] or np.any(amp != 0)))
            if check.get("env") != env or check.get("first") != start or check.get("last") != stop or check.get("present") != present or check.get("before") != phase:
                raise ValueError("包络身份、边界、音调标志或起始状态不符")
            expected[start-a:stop-a] = model(phase, stop-start, lo, amp)
            end = integer(check["after"], 0, 3)
            clock_diff += end != (phase+stop-start) % 4
            phase = end
            envelope_count += 1
        if phase != ref["sine_after"] or ref["noise_after"] != (noise_previous+slots*n) % 512:
            raise ValueError("包络结束状态或噪声推进量不符")
        raw = floats(reference_dir / (c["id"]+".qmf.f32"), (40, 64, 2))
        x = raw[a:b, lo:lo+n].copy() * np.float32(2**23)
        if not np.isfinite(x).all():
            raise ValueError("参考幅度换算溢出")
        raw[a:b, lo:lo+n] = 0
        if np.any(raw != 0):
            raise ValueError("参考写出声明的时隙或子带之外")
        model_diff += int(np.count_nonzero(x != expected))
        informative += bool(np.any(expected != 0))
        kind = "initialization" if c["first_frame"] else "chain" if group_counts[c["group"]] > 1 else "multi_envelope" if c["envelopes"] > 1 else "nonzero_origin" if c["left_ats"] and c["factor"] == 2 else "control"
        kind_stats = by_kind.setdefault(kind, {m: stats() for m in MODES})
        detail = dict(id=c["id"], kind=kind, reference_before=ref_previous, reference_after=ref["sine_after"], modes={})
        for mode in MODES:
            v = local[c["id"], mode]
            converted, initial_zero = "qmf_origin" in mode, "initial_zero" in mode
            base = (3 if initial_zero else 0) if c["first_frame"] else local_previous[mode]
            extra = c["left_ats"]*(c["factor"]-1)
            if legacy:
                injected = (base - (extra if converted else 0)) % 4
                call_first = c["first_frame"] and not converted and not initial_zero
            else:
                if converted and not initial_zero:
                    base = local_previous[mode]  # 生产模式原样传递游标，first_frame 在函数内处理。
                injected = (base + (0 if converted else extra)) % 4
                call_first = c["first_frame"] and converted and not initial_zero
            if v[0] != local_previous[mode] or v[1] != injected or v[6] != int(call_first):
                raise ValueError("本地逻辑、注入状态或初始化标志不符")
            y = floats(local_dir / f"{c['id']}_{source_modes[c['id'], mode]}.f32", (slots, n, 2))
            mismatch = v[3] != ref["sine_after"]
            add_stats(total[mode], x, y, mismatch)
            add_stats(kind_stats[mode], x, y, mismatch)
            detail["modes"][mode] = dict(logical_previous=v[0], injected_previous=v[1], returned_last=v[2], normalized_next=v[3], clock_mismatch=mismatch, differing_components=int(np.count_nonzero(x != y)))
            local_previous[mode] = v[2]
        details.append(detail)
        ref_previous, noise_previous = ref["sine_after"], ref["noise_after"]
    return dict(schema="macinac4.aspx-tone-replay-comparison", version=2, status="observation_only", local_production_origin="literal" if legacy else "qmf", direct_production_mode="literal" if legacy else "qmf_origin", cases=len(cases), informative_cases=informative, envelopes=envelope_count, reference_to_local_amplitude_scale=2**23, cursor_conversion="next=(last+1)%4", phase_or_index_fit=False, reference_model_differing_components=model_diff, reference_clock_mismatch_envelopes=int(clock_diff), total=total, by_kind=by_kind, details=details)


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
        print(f"音调重放检查失败：{exc}", file=sys.stderr)
        return 2
    print(json.dumps({"cases": report["cases"], "total": report["total"]}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
