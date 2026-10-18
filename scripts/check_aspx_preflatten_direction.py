#!/usr/bin/env python3
"""比较固定 HF 输入的 P85/P89 方向候选；不以参考吻合作为规范裁决。

四套输出均来自同一生产重放入口，构建时只替换声明的表达式：
literal 为调查起点的 P85/P89；multiply 改 P89 为乘法；reverse_gain 反转 P85 差值；
bypass 不施加预平坦化增益。manifest、输入及参考沿用 HF/TNA 的归一化协议。
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

try:
    from . import check_aspx_hf_replay as hf
    from .check_aspx_noise_replay import floats
    from .check_aspx_basis_replay import metrics
except ImportError:
    import check_aspx_hf_replay as hf
    from check_aspx_noise_replay import floats
    from check_aspx_basis_replay import metrics


MODES = ("literal", "multiply", "reverse_gain", "bypass")
SCALES = (("raw", 1.0), ("scaled", float(2**23)))


def source_map(case):
    source = []
    for lo, hi, start in zip(case["targets"][:-1], case["targets"][1:], case["sources"]):
        source.extend(range(start, start + hi - lo))
    if len(source) != case["sbz"] - case["sbx"]:
        raise ValueError("patch 未覆盖完整目标范围")
    if not source or min(source) < 0 or max(source) >= case["sbx"]:
        raise ValueError("patch 源超出低带")
    return np.array(source, dtype=int)


def gain_model(low, case, scale, reverse=False):
    """独立最小二乘；不使用输出增益反推拟合参数。"""
    n, first, last = case["sbx"], case["first"], case["last"]
    power = np.mean(np.sum(low[first:last, :n].astype(float)**2, axis=-1), axis=0) * scale**2
    envelope = 10 * np.log10(power + 1)
    x = np.linspace(-1, 1, n)
    design = np.vander(x, 4, increasing=True)
    fitted = design @ np.linalg.lstsq(design, envelope, rcond=None)[0]
    exponent = fitted - envelope.mean() if reverse else envelope.mean() - fitted
    return np.asarray(10**(exponent / 20), dtype="<f4")


def apply_gain(off, gain, source, mode):
    if mode not in MODES:
        raise ValueError("未知方向候选")
    off = np.asarray(off, dtype="<f4")
    gain = np.asarray(gain, dtype="<f4")
    if off.ndim != 3 or off.shape[1:] != (len(source), 2):
        raise ValueError("HF 形状错误")
    if gain.ndim != 1 or not np.isfinite(off).all() or not np.isfinite(gain).all() or np.any(gain <= 0):
        raise ValueError("无效 HF 或增益")
    if np.any(source < 0) or np.any(source >= len(gain)):
        raise ValueError("源子带越界")
    if mode == "bypass":
        return off.copy()
    mapped = gain[source][None, :, None]
    return off * mapped if mode == "multiply" else off / mapped


def validate_output(off, gain, source, actual, mode):
    expected = apply_gain(off, gain, source, mode)
    if not hf.identical(expected, actual):
        raise ValueError(f"{mode} 输出未按声明的源子带及运算方向生成")


def compare(manifest, data, references, variants):
    # 原检查器先验证全部 129 个输入及四个独立参考进程，含来源、时序与结束状态。
    hf.compare(manifest, data, references, variants / "hf_literal")
    m = json.loads(manifest.read_text())
    cases = m["cases"]
    root = {mode: variants / f"hf_{mode}" for mode in MODES}
    names = {p.name for p in root["literal"].iterdir() if p.is_file()}
    for mode in MODES[1:]:
        if {p.name for p in root[mode].iterdir() if p.is_file()} != names:
            raise ValueError(f"{mode} 重放文件缺失或多余")
    unchanged_files = [name for name in names if not name.endswith((".gain.f32", ".preflat.f32"))]
    for name in unchanged_files:
        expected = (root["literal"] / name).read_bytes()
        for mode in MODES[1:]:
            if (root[mode] / name).read_bytes() != expected:
                raise ValueError(f"{mode} 改变了预测器、旁路 HF 或状态：{name}")
    results = []
    slopes = []
    for c in cases:
        name, lo, hi, first, last = (c[k] for k in ("id", "sbx", "sbz", "first", "last"))
        mapping = source_map(c)
        low = floats(data / f"{name}.qmf.f32", (40, 64, 2))
        ref = floats(references / m["reference_runs"][0] / f"{name}.reference0.f32", (40, 64, 2))[first:last, lo:hi] * np.float32(4)
        record = dict(id=name, kind=c["kind"], scope=c["scope"], first=first, last=last, results={})
        for unit, scale in SCALES:
            off = floats(root["literal"] / f"{name}.{unit}.off.f32", (last-first, 64, 2))[:, lo:hi].copy()
            outputs = {}
            for mode in MODES:
                gain = floats(root[mode] / f"{name}.{unit}.gain.f32", (64,))
                expected_gain = gain_model(low, c, scale, mode == "reverse_gain")
                if np.any(gain[:lo] <= 0) or not np.allclose(gain[:lo], expected_gain, rtol=2e-6, atol=0):
                    raise ValueError(f"{mode} 的 P85 增益与独立拟合不符")
                output = floats(root[mode] / f"{name}.{unit}.preflat.f32", (last-first, 64, 2))
                valid = output[:, lo:hi].copy()
                output[:, lo:hi] = 0
                if np.any(output):
                    raise ValueError("HF 写出了目标子带范围")
                validate_output(off, gain, mapping, valid, mode)
                outputs[mode] = valid
                record["results"][f"{unit}_{mode}"] = dict(reference=metrics(ref, valid), gain=metrics(expected_gain, gain[:lo].copy()))
            record["results"][f"{unit}_repair_rounding"] = metrics(outputs["multiply"], outputs["reverse_gain"])
            if c["kind"] == "preflat_probe" and c["pattern"] in ("rising", "falling"):
                # 同一源子带出现多次时仅统计一次，避免 patch 重复加权。
                sources, positions = np.unique(mapping, return_index=True)
                original = low[first:last, sources].copy()
                min_power = float(np.min(np.mean(np.sum(low[first:last, :lo].astype(float)**2, axis=-1), axis=0)) * scale**2)
                row = dict(id=name, unit=unit, amplitude_exp=c["amplitude_exp"], pattern=c["pattern"],
                           input_db_per_band=hf.slope(original, sources), min_power=min_power,
                           floor_negligible=min_power >= 1e4,
                           output_db_per_band={mode: hf.slope(out[:, positions], sources) for mode, out in outputs.items()})
                slopes.append(row)
                if row["floor_negligible"]:
                    for mode in ("multiply", "reverse_gain"):
                        if abs(row["output_db_per_band"][mode]) > 2e-4:
                            raise ValueError("方向修正没有消除高能量探针的线性斜率")
        results.append(record)
    return dict(schema="macinac4.preflatten-direction-comparison", version=1, status="observation_only",
                phase_gain_or_delay_fitting=False, cases=results, slopes=slopes,
                reference_processes=len(m["reference_runs"]), unchanged_files_per_variant=len(unchanged_files),
                candidate_modes=list(MODES), production_policy_changed=False)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "data", "references", "variants"):
        p.add_argument(name, type=Path)
    p.add_argument("--output", type=Path, required=True)
    a = p.parse_args(argv)
    result = compare(a.manifest, a.data, a.references, a.variants)
    a.output.write_text(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n")
    print(f"验证 {len(result['cases'])} 个输入、四种方向候选及两个尺度")


if __name__ == "__main__":
    main()
