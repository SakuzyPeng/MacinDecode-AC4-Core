#!/usr/bin/env python3
"""比较固定 QMF 基底下的 HF 搬运和单子带音调合成；不拟合相位、增益或延迟。

H 案例使用已验证参考接口的 ×4 HF 尺度，S 案例使用 ×2^17 PCM 尺度。
它们均是低层接口换算，不替换对象 PCM 的比较尺度。
"""
from __future__ import annotations
import argparse
import json
import math
from pathlib import Path
import re
import sys
import numpy as np
if __package__:
    from .check_aspx_noise_replay import floats, integer, ordered_bits
else:
    from check_aspx_noise_replay import floats, integer, ordered_bits

HF_MODES = ("native", "input_basis", "operator_basis")
TONE_MODES = ("native", "basis", "advance", "basis_advance")
ROTATION = -np.exp(1j*np.pi*(np.arange(64)+.5)/128)


def complex_values(a):
    return a[..., 0].astype(float)+1j*a[..., 1].astype(float)


def components(a):
    return np.stack([a.real, a.imag], axis=-1).astype("<f4")


def metrics(x, y):
    if x.shape != y.shape or not x.size or not np.isfinite(x).all() or not np.isfinite(y).all():
        raise ValueError("比较矩阵形状或有限性错误")
    delta = y.astype(float)-x.astype(float)
    signal = float(np.sum(x.astype(float)**2))
    error = float(np.sum(delta**2))
    different = x.view("<u4") != y.view("<u4")
    nz = x != 0
    return dict(components=int(x.size), signal_energy=signal, error_energy=error,
                snr_db=10*math.log10(signal/error) if signal and error else None,
                max_abs_error=float(np.max(abs(delta))),
                max_relative_error_nonzero_reference=float(np.max(abs(delta[nz]/x[nz]))) if np.any(nz) else 0,
                max_ulp=int(np.max(abs(ordered_bits(x)-ordered_bits(y)))),
                differing_components=int(np.count_nonzero(x != y)),
                bit_differing_components=int(np.count_nonzero(different)),
                signed_zero_only_differences=int(np.count_nonzero(different & (x == 0) & (y == 0))))


def load_cases(path):
    rows = json.loads(path.read_text())
    if not isinstance(rows, list) or not 1 <= len(rows) <= 4096:
        raise ValueError("案例数量错误")
    ids = set()
    for r in rows:
        if not isinstance(r, dict) or not isinstance(r.get("id"), str) or re.fullmatch(r"[A-Za-z0-9]+", r["id"]) is None or r["id"] in ids:
            raise ValueError("案例身份错误")
        ids.add(r["id"])
        if r["kind"] == "H":
            first, last = integer(r["first"], 0, 37), integer(r["last"], 1, 38)
            targets, sources = r["targets"], r["sources"]
            if not isinstance(sources, list) or not 1 <= len(sources) <= 5 or len(targets) != len(sources)+1:
                raise ValueError("patch 表长错误")
            for b in targets: integer(b, 0, 64)
            for src, a, b in zip(sources, targets[:-1], targets[1:], strict=True):
                integer(src, 0, 63)
                if a >= b or src+b-a > targets[0]:
                    raise ValueError("patch 边界或低带源错误")
            if len(r["config"]) != 5:
                raise ValueError("配置字段数错误")
            for v, high in zip(r["config"][:4], [1, 15, 15, 3], strict=True): integer(v, 0, high)
            integer(r["config"][4], 0, 255)
            if r["config"][4] not in (*range(8), 255):
                raise ValueError("xover 必须明确给出或标为待按交叉子带恢复")
        elif r["kind"] == "S":
            slots = integer(r["slots"], 10, 1024)
            first, last = integer(r["first"], 0, slots-1), integer(r["last"], 1, slots)
            integer(r["band"], 0, 63)
            integer(r["seed"], 0, 3)
        else:
            raise ValueError("未知案例种类")
        if first >= last:
            raise ValueError("空区间")
    return rows


def compare(cases_path, inputs, local):
    cases = load_cases(cases_path)
    known = {r["id"]: r for r in cases}
    observations = {}
    for line in (local/"results.txt").read_text().splitlines():
        p = line.split()
        if len(p) < 3 or p[1] not in known:
            raise ValueError("本地记录身份错误")
        kind, name, mode = p[:3]
        r = known[name]
        modes = HF_MODES if r["kind"] == "H" else TONE_MODES
        if kind != r["kind"] or mode not in modes or (name, mode) in observations:
            raise ValueError("本地模式错误或重复")
        expected = [r["first"], r["last"], r["targets"][0]] if kind == "H" else [r["slots"]]
        if list(map(int, p[3:])) != expected:
            raise ValueError("本地记录的区间或布局错位")
        observations[name, mode] = True
    if len(observations) != sum(3 if r["kind"] == "H" else 4 for r in cases):
        raise ValueError("缺失本地模式或案例")
    hf, tones = [], []
    table = np.array([(1,0), (0,1), (-1,0), (0,-1)], dtype="<f4")
    for r in cases:
        name = r["id"]
        if r["kind"] == "H":
            a, b = r["first"], r["last"]
            lo, hi = r["targets"][0], r["targets"][-1]
            source = floats(inputs/f"{name}.native.f32", (40,64,2))
            converted = floats(inputs/f"{name}.converted.f32", (40,64,2))
            ref = floats(inputs/f"{name}.reference.f32", (40,64,2))
            rot = floats(inputs/f"{name}.rotated_reference.f32", (40,64,2))
            source_map = np.concatenate([np.arange(s,s+y-x) for s,x,y in zip(r["sources"],r["targets"][:-1],r["targets"][1:],strict=True)])
            ref_native = ref[a:b,lo:hi].copy()*np.float32(4)
            ref_local = components(complex_values(ref_native)/ROTATION[lo:hi])
            data = {m:floats(local/f"{name}.{m}.f32",(b-a,64,2)) for m in HF_MODES}
            for values in data.values():
                if np.any(values[:,:lo] != 0) or np.any(values[:,hi:] != 0):
                    raise ValueError("本地写出目标子带范围之外")
            for v, original in [(ref, source), (rot, converted)]:
                copy = v.copy();copy[a:b,lo:hi] = original[a:b,lo:hi]
                if not np.array_equal(copy, original):
                    raise ValueError("参考改变 HF 写入范围以外的值")
            parts = []
            for start, end, src in zip(r["targets"][:-1],r["targets"][1:],r["sources"],strict=True):
                x = complex_values(ref_local[:,start-lo:end-lo]);y = complex_values(data["input_basis"][:,start:end])
                cross = np.sum(np.conj(y)*x)
                parts.append(dict(source=src,target=start,length=end-start,
                    predicted_operator_phase_deg=(src-start)*180/128,
                    observed_reference_over_plain_copy_phase_deg=float(np.angle(cross,deg=True)) if abs(cross) else None))
            hf.append(dict(id=name,config=r["config"],first=a,last=b,targets=r["targets"],sources=r["sources"],patches=parts,
                input_basis_transform=metrics(components(complex_values(source)/ROTATION),converted),
                native_quarter_copy=metrics(source[a:b,source_map],ref_native),
                same_basis_local=metrics(ref_native,data["native"][:,lo:hi]),
                native_rotated_vs_local=metrics(rot[a:b,lo:hi].copy()*np.float32(4),data["input_basis"][:,lo:hi]),
                unchanged_operator=metrics(ref_local,data["input_basis"][:,lo:hi]),
                transformed_operator=metrics(ref_local,data["operator_basis"][:,lo:hi])))
        else:
            slots, band, a, b = r["slots"], r["band"], r["first"], r["last"]
            q = floats(inputs/f"{name}.native.f32", (slots,64,2))
            expected = np.zeros_like(q)
            expected[a:b,band] = table[(r["seed"]+np.arange(b-a))%4]
            expected[a:b,band,1] *= 1 if band%2 == 0 else -1
            if np.any(q != expected):
                raise ValueError("音调输入不匹配声明的子带、游标或区间")
            ref = floats(inputs/f"{name}.reference.f32", (slots,64))*np.float32(2**17)
            tones.append(dict(id=name,band=band,seed=r["seed"],slots=slots,
                modes={m:metrics(ref,floats(local/f"{name}.{m}.f32", (slots,64))) for m in TONE_MODES},
                production_initial_one_over_reference_zero_phase_deg=float(np.angle((1j if band%2==0 else -1j)*ROTATION[band],deg=True))))
    return dict(schema="macinac4.aspx-basis-replay-comparison",version=1,status="observation_only",
                rotation="R(k)=-exp(j*pi*(k+0.5)/128)",phase_or_gain_fitting=False,
                hf_reference_amplitude_scale=4,tone_pcm_reference_amplitude_scale=2**17,
                hf_cases=len(hf),tone_cases=len(tones),hf=hf,tones=tones)


def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("cases",type=Path);p.add_argument("inputs",type=Path);p.add_argument("local",type=Path)
    p.add_argument("--output",type=Path,required=True);args=p.parse_args(argv)
    try:
        report=compare(args.cases,args.inputs,args.local)
        with args.output.open("x") as f:json.dump(report,f,ensure_ascii=False,indent=2,allow_nan=False);f.write("\n")
    except (OSError,ValueError,KeyError,TypeError) as e:
        print(f"基底重放检查失败：{e}",file=sys.stderr);return 2
    print(json.dumps({k:report[k] for k in ['hf_cases','tone_cases','status']},ensure_ascii=False));return 0


if __name__=="__main__":raise SystemExit(main())
