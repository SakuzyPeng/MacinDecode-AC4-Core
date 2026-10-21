#!/usr/bin/env python3
"""固定 QMF 宽度输入通过同一本地下游；参考距离仅为观察，不作为算法选择门槛。"""
from __future__ import annotations

import argparse
from functools import lru_cache
import hashlib
import json
from pathlib import Path

import numpy as np

try:
    from .check_aspx_basis_replay import complex_values, components, metrics
    from .check_aspx_downstream_replay import estimate_power
    from .check_aspx_noise_replay import floats, integer
    from .check_asf_imdct_checkpoints import identifier
    from .check_qmf_phase_chain import synthesise_fir
except ImportError:
    from check_aspx_basis_replay import complex_values, components, metrics
    from check_aspx_downstream_replay import estimate_power
    from check_aspx_noise_replay import floats, integer
    from check_asf_imdct_checkpoints import identifier
    from check_qmf_phase_chain import synthesise_fir

VARIANTS = ["d", "c", "c512", "b", "b512", "e", "a"]
PAIRS = [("a", v) for v in ("d", "c", "b", "e")] + [
    ("c", "d"), ("b", "c"), ("e", "b"), ("c512", "c"), ("b512", "b")]
SUFFIXES = ["alpha", "pre", "history", "hf", "levels", "noise", "tone", "y",
            "state", "merged", "pcm"]


def validate_manifest(m):
    if (m.get("schema"), m.get("version"), m.get("variants")) != (
            "macinac4.qmf-width-downstream", 1, VARIANTS):
        raise ValueError("宽度下游协议错误")
    if m.get("common_to_local") != "Q_common / R64 * 2^23" or m.get("initial_cursors") != [0, 0]:
        raise ValueError("表示或起始状态不符")
    known = set()
    for c in m["cases"]:
        identifier(c["id"])
        key = c["scope"], integer(c["call"], 0, 8), integer(c["interpolation"], 0, 1)
        if key in known or c["id"] != f"{c['scope']}c{c['call']}i{c['interpolation']}":
            raise ValueError("重复或错位案例")
        known.add(key)
        lo, hi = c["sbx"], c["sbz"]
        if lo != 36 or hi != (62 if c["scope"] == "tone" else 56):
            raise ValueError("本轮频带不符")
        if [c[k] for k in ("master", "start", "stop", "noise", "xover")] != [
                1, 6, 0 if c["scope"] == "tone" else 1, 3, 0]:
            raise ValueError("本轮频带推导配置不符")
        expected_sources = [28, 18 if hi == 62 else 24]
        if c["sources"] != expected_sources or c["targets"] != [36, 44, hi]:
            raise ValueError("patch 来源不符")
        times = c["signal_borders"]
        if (c["first"], c["last"]) != (2 * times[0], 2 * times[-1]):
            raise ValueError("时间单位或边界错误")
        if (c["envelopes"] != [[2*a, 2*b] for a, b in zip(times, times[1:])]
                or len(c["controls"]) != len(times)-1
                or len(c["freq_res"]) != len(times)-1):
            raise ValueError("包络遗漏或错位")
        if not 0 <= c["first"] <= 6 or not 32 <= c["last"] <= 38 or any(
                a >= b for a, b in zip(times, times[1:])):
            raise ValueError("无效时间范围")
        if len(c["chirp"]) != 2 or not all(np.isfinite(x) and 0 <= x <= 1 for x in c["chirp"]):
            raise ValueError("chirp 无效")
        for borders in (c["low"], c["high"]):
            if len(borders) < 2 or borders[0] != lo or borders[-1] != hi or any(
                    a >= b for a, b in zip(borders, borders[1:])):
                raise ValueError("信号频带边界无效")
        for high in c["freq_res"]:
            integer(high, 0, 1)
        for r in c["controls"]:
            if type(r["onset"]) is not bool or r["limiter"] != [36, 44, hi]:
                raise ValueError("固定控制来源不符")
            for k in ["target", "noise_ratio", "sine_index", "sine_area", "noise_level", "sine_level"]:
                v = np.asarray(r[k])
                if v.shape != (hi-lo,) or not np.isfinite(v).all() or np.any(v < 0):
                    raise ValueError("无效固定控制")
            if any(x <= 0 for x in r["target"]):
                raise ValueError("目标非正")
            if not all(type(x) is bool for k in ("sine_index", "sine_area") for x in r[k]):
                raise ValueError("正弦状态不是布尔值")
    if known != {(s, c, i) for s in ("tone", "music100", "music33")
                 for c in range(9) for i in range(2)}:
        raise ValueError("案例覆盖不完整")


def close(actual, expected, label, relative=3e-6):
    a, b = np.asarray(actual, float), np.asarray(expected, float)
    if a.shape != b.shape or not a.size or not np.isfinite(a).all() or not np.isfinite(b).all():
        raise ValueError(f"{label}: 无效数值或形状")
    # 最大值归一化只用于独立计算舍入容差；所有比较分量仍完整计分。
    scale = float(np.max(abs(b)))
    if np.max(abs(a-b)) > relative * scale:
        raise ValueError(f"{label}: 不符合独立边界")


def gain_model(energy, r, mode):
    """用能量恒等式独立记账，常数为已固定生产策略，不读取生产源码。"""
    e = np.asarray(energy, float)
    s, q, noise, sine = (np.asarray(r[k], dtype="<f4").astype(float)
                         for k in ("target", "noise_ratio", "noise_level", "sine_level"))
    area = np.asarray(r["sine_area"], bool)
    active = (sine == 0) & (not r["onset"])
    initial = np.sqrt(s * np.where(area, q, 1) /
                      ((e+1) * np.where(area | (not r["onset"]), 1+q, 1))).astype("<f4")
    gain = initial.copy()
    limited_noise = noise.astype("<f4")
    boost = np.ones_like(gain)
    clipped = np.zeros(len(e), bool)
    ceiling = np.zeros(len(e), bool)
    capped = np.zeros(len(e), bool)
    budgets = []
    for a, b in zip(r["limiter"], r["limiter"][1:]):
        span = slice(a-r["limiter"][0], b-r["limiter"][0])
        if mode == 2:
            group = np.sqrt(s[span].sum()/(1e-12+e[span].sum())) * 1.41254
            maximum = np.float32(min(group, 1e5))
            clipped[span] = initial[span] > maximum
            ceiling[span] = (group >= 1e5) & clipped[span]
            gain[span] = np.minimum(initial[span], maximum)
            limited_noise[span] = np.minimum(
                limited_noise[span],
                (noise[span]*float(maximum)/initial[span]).astype("<f4"))
        before = np.sum(e[span]*gain[span].astype(float)**2 + sine[span]**2
                        + active[span]*limited_noise[span].astype(float)**2)
        wanted = float(s[span].sum())
        if mode:
            raw = np.sqrt((wanted+1e-12)/(before+1e-12))
            capped[span] = raw > 1.584893192
            boost[span] = np.float32(min(raw, 1.584893192))
        budget = before*float(boost[span][0])**2
        budgets.append(dict(target=wanted, preboost=before, adjusted=budget,
                            relative_gap=(budget-wanted)/wanted))
    values = np.column_stack((gain*boost, limited_noise*boost,
                              sine.astype("<f4")*boost, boost, active.astype("<f4")))
    return values.astype("<f4"), dict(limited=clipped, hard_ceiling=ceiling,
                                      boost_cap=capped, below_unit_floor=e < 1), budgets


def hf_model(stream, first, last, c, alpha, pre, enabled):
    mapping = np.concatenate([np.arange(s, s+b-a)
                              for s, a, b in zip(c["sources"], c["targets"], c["targets"][1:])])
    z = complex_values(stream)
    coeff = alpha[:, ::2].astype(float) + 1j*alpha[:, 1::2].astype(float)
    chirp = np.repeat(c["chirp"], np.diff(c["targets"]))
    t = np.arange(first, last)[:, None]
    p = mapping[None, :]
    # stream[0:4] 是 TNA 历史；生产预测完整残差后再除以源带增益。
    value = (z[t+4, p] + coeff[mapping, 0]*chirp*z[t+2, p]
             + coeff[mapping, 1]*chirp**2*z[t, p])
    if enabled:
        value /= pre[mapping]
    return components(value)


def preflatten_model(low, first, last, lo):
    energy = np.mean(np.sum(low[first:last, :lo].astype(float)**2, axis=-1), axis=0)
    db = 10*np.log10(1+energy)
    u = np.linspace(-1, 1, lo)
    design = np.column_stack([u**i for i in range(4)])
    fit = design @ np.linalg.lstsq(design, db, rcond=None)[0]
    return 10**((fit-db.mean())/20)


@lru_cache(maxsize=1024)
def prediction_model(raw, lo):
    """原始复数时序的增广最小二乘（SVD），不复写生产残差投影算法。"""
    z = complex_values(np.frombuffer(raw, "<f4").reshape(42, lo, 2))
    out = np.zeros((lo, 4), "<f4")
    for b in range(lo):
        y, x1, x2 = z[4::2, b], z[2:-2:2, b], z[:-4:2, b]
        power = np.vdot(x1, x1).real
        if power == 0:
            continue
        ridge = abs(np.vdot(x1, x2))**2/power * 2**-20/(1+2**-20)
        design = np.vstack((np.column_stack((x1, x2)), [0, np.sqrt(ridge)]))
        alpha = np.linalg.lstsq(design, -np.r_[y, 0], rcond=None)[0]
        if np.any(abs(alpha) >= 4):
            alpha[:] = 0
        out[b] = [alpha[0].real, alpha[0].imag, alpha[1].real, alpha[1].imag]
    return out


def inspect_step(c, data, root, prefix, step, preflat, mode, table, previous=None):
    lo, hi = c["sbx"], c["sbz"]
    n = hi-lo
    stream = np.concatenate((data, data[32:64]))
    pos = 26+32*step
    low = stream[pos:pos+38]
    if step:
        first, last = c["last"]-32, 32
        controls = [dict(c["controls"][-1], onset=False)]
        borders, freq = [[first, last]], [c["freq_res"][-1]]
    else:
        first, last = c["first"], c["last"]
        controls, borders, freq = c["controls"], c["envelopes"], c["freq_res"]
    read = lambda suffix, shape: floats(root/f"{prefix}s{step}.{suffix}.f32", shape)
    alpha = read("alpha", (64, 4))
    pre = read("pre", (64,))
    hf = read("hf", (last-first, 64, 2))
    levels = read("levels", (len(borders), n, 6))
    noise = read("noise", hf.shape)
    tone = read("tone", hf.shape)
    y = read("y", (last, 64, 2))
    state = read("state", (5,))
    merged = read("merged", (32, 64, 2))
    pcm = read("pcm", (2048,))
    history = read("history", (4, 64, 2))
    if not np.array_equal(history.view("<u4"), low[28:32].copy().view("<u4")):
        raise ValueError("原始 TNA 历史交接错误")
    close(pre[:lo], preflatten_model(low, first, last, lo), "预平坦化", 3e-6)
    close(alpha[:lo], prediction_model(stream[pos-4:pos+38, :lo].copy().tobytes(), lo),
          "原始时序预测器", 3e-6)
    close(hf[:, lo:hi], hf_model(stream[pos-4:pos+38], first, last, c, alpha, pre, preflat),
          "HF 消费与源带映射", 3e-6)
    before_noise, before_tone = [0, 0] if previous is None else previous["state"][2:4]
    expected_state = [before_noise, before_tone, (before_noise+(last-first)*n) % 512,
                      (before_tone+last-first) % 4, last-32]
    if not np.array_equal(state, expected_state):
        raise ValueError("游标推进或尾部长度错误")
    prefix_y = np.zeros_like(y[:first]) if previous is None else previous["y"][32:]
    if not np.array_equal(y[:first].view("<u4"), prefix_y.copy().view("<u4")):
        raise ValueError("HF 尾部被重算或丢失")
    states, budgets = [], []
    signal = np.zeros_like(hf)
    injected_noise = np.zeros_like(hf)
    for env, (a, b) in enumerate(borders):
        r = controls[env]
        section = slice(a-first, b-first)
        groups = c["high"] if freq[env] else c["low"]
        energy = estimate_power(hf[section, lo:hi], [x-lo for x in groups], c["interpolation"])
        close(levels[env, :, 0], energy, "包络估计实际输入", 1e-7)
        model, branch, budget = gain_model(energy, r, mode)
        close(levels[env, :, 1:], model, "增益与预算", 2e-6)
        states.append(branch)
        budgets.extend(budget)
        g = levels[env]
        signal[section, lo:hi] = hf[section, lo:hi] * g[:, 1][None, :, None]
        raster = before_noise+np.arange((a-first)*n, (b-first)*n)+1
        expected_noise = table[raster.astype(int) % 512].reshape(b-a, n, 2)*g[:, 2][None, :, None]
        close(noise[section, lo:hi], expected_noise, "噪声游标与电平", 1e-7)
        # 当前真实固定控制没有音调；如将来扩大输入必须显式补音调 oracle。
        if np.any(g[:, 3]) or np.any(tone[section]):
            raise ValueError("未声明的音调覆盖")
        injected_noise[section, lo:hi] = noise[section, lo:hi]*g[:, 5][None, :, None]
    expected_y = (signal+injected_noise)+tone
    if not np.array_equal(y[first:].view("<u4"), expected_y.view("<u4")):
        raise ValueError("组装没有消费本地 HF／增益／预算")
    if not np.array_equal(merged.view("<u4"), (low[:32]+y[:32]).view("<u4")):
        raise ValueError("合并边界错误")
    # 不把随机噪声的有限区间能量误当成标度因子的期望能量。
    sig = signal[:, lo:hi].astype(float)
    noi = injected_noise[:, lo:hi].astype(float)
    actual = y[first:, lo:hi].astype(float)
    energy_terms = dict(signal=float(np.sum(sig**2)), noise=float(np.sum(noi**2)),
                        cross=float(2*np.sum(sig*noi)), assembled=float(np.sum(actual**2)))
    closure = abs(energy_terms["assembled"]-sum(energy_terms[k] for k in ("signal", "noise", "cross")))
    if closure > 5e-7*max(energy_terms["assembled"], energy_terms["signal"]+energy_terms["noise"]):
        raise ValueError("组装能量分解不闭合")
    return dict(hf=hf[:, lo:hi].copy(), alpha=alpha, pre=pre, levels=levels, signal=signal[:, lo:hi].copy(),
                y=y, active_y=y[first:, lo:hi].copy(), merged=merged, pcm=pcm, state=state,
                branches={k: np.concatenate([s[k] for s in states]) for k in states[0]},
                budgets=budgets, energy=energy_terms)


def compare(manifest, inputs, production):
    m = json.loads(manifest.read_text())
    validate_manifest(m)
    expected_inputs = {"cases.txt", "table.f32", "window.f32"} | {
        f"{c['id']}.{s}.f32" for c in m["cases"] for s in VARIANTS+["controls", "chirp"]}
    if set(m["input_sha256"]) != expected_inputs:
        raise ValueError("输入清单遗漏或增加文件")
    for name, digest in m["input_sha256"].items():
        if hashlib.sha256((inputs/name).read_bytes()).hexdigest() != digest:
            raise ValueError(f"固定输入被改写：{name}")
    lines = []
    for c in m["cases"]:
        values = [c["id"], *[c[k] for k in ("master", "start", "stop", "noise", "xover")],
                  c["sbx"], c["sbz"], c["interpolation"], len(c["envelopes"]),
                  *c["signal_borders"], *c["freq_res"], len(c["low"]), *c["low"],
                  len(c["high"]), *c["high"]]
        lines.append(" ".join(map(str, values)))
        controls = []
        for r in c["controls"]:
            controls.append(int(r["onset"]))
            for b in range(c["sbz"]-c["sbx"]):
                controls.extend(r[k][b] for k in (
                    "target", "noise_ratio", "sine_index", "sine_area", "noise_level", "sine_level"))
        expected = np.asarray(controls, "<f4")
        actual = floats(inputs/f"{c['id']}.controls.f32", expected.shape)
        if not np.array_equal(actual.view("<u4"), expected.view("<u4")):
            raise ValueError("固定控制不是声明的元数据")
        if not np.array_equal(floats(inputs/f"{c['id']}.chirp.f32", (2,)),
                              np.asarray(c["chirp"], "<f4")):
            raise ValueError("固定 chirp 不符")
    if (inputs/"cases.txt").read_text().splitlines() != lines:
        raise ValueError("生产重放参数与实验清单不符")
    metadata = json.loads((production/"replay_metadata.json").read_text())
    if metadata != dict(schema="macinac4.width-chain-replay", version=1,
                        prediction_solver="orthogonal_residual",
                        preflatten_gain_convention="slope_minus_mean",
                        initial_cursors=[0, 0], successor="repeat_last_analysis_au"):
        raise ValueError("生产重放算法约定不符")
    expected_outputs = {"replay_metadata.json"} | {
        f"{c['id']}{v}p{p}m{mode}s{step}.{suffix}.f32"
        for c in m["cases"] for v in VARIANTS for p in range(2) for mode in range(3)
        for step in range(2) for suffix in SUFFIXES}
    if {p.name for p in production.iterdir()} != expected_outputs:
        raise ValueError("边界输出存在遗漏或多余记录")
    table = floats(inputs/"table.f32", (512, 2))
    window = floats(inputs/"window.f32", (640,))
    results, observations = [], []
    for c in m["cases"]:
        source_bands = sorted({s+j for s, a, b in zip(c["sources"], c["targets"], c["targets"][1:])
                               for j in range(b-a)})
        runs = {}
        q = {v: floats(inputs/f"{c['id']}.{v}.f32", (64, 64, 2)) for v in VARIANTS}
        if any(np.any(v[:, c["sbx"]:]) for v in q.values()):
            raise ValueError("低带输入夹带未声明高带")
        for p in range(2):
            for mode in range(3):
                for v in VARIANTS:
                    prefix = f"{c['id']}{v}p{p}m{mode}"
                    one = inspect_step(c, q[v], production, prefix, 0, bool(p), mode, table)
                    two = inspect_step(c, q[v], production, prefix, 1, bool(p), mode, table, one)
                    combined = np.concatenate((one["merged"], two["merged"]))
                    expected_pcm = 2*synthesise_fir(complex_values(combined), window)
                    close(np.concatenate((one["pcm"], two["pcm"])), expected_pcm,
                          "连续合成消费本地合并结果", 2e-6)
                    runs[v, p, mode] = one, two
                    observations.append(dict(
                        id=c["id"], variant=v, preflat=p, mode=mode,
                        branches=[{k: int(x.sum()) for k, x in step["branches"].items()} for step in (one, two)],
                        budgets=[step["budgets"] for step in (one, two)],
                        energy=[step["energy"] for step in (one, two)]))
                for ref, other in PAIRS:
                    a, az = runs[ref, p, mode]
                    b, bz = runs[other, p, mode]
                    stages = {k: metrics(a[k], b[k])
                              for k in ("hf", "signal", "active_y", "merged")}
                    stages["pre"] = metrics(a["pre"][source_bands].copy(),
                                             b["pre"][source_bands].copy())
                    # 能量、幅度增益和噪声电平量纲不同，禁止拼成一个 SNR。
                    for column, name in enumerate(("estimated", "gain", "noise_level", "sine_level", "boost")):
                        stages[name] = metrics(a["levels"][:, :, column].copy(),
                                               b["levels"][:, :, column].copy())
                    stages["source"] = metrics(q[ref][26:64, source_bands].copy(),
                                               q[other][26:64, source_bands].copy())
                    stages["alpha"] = metrics(a["alpha"][source_bands].copy(),
                                              b["alpha"][source_bands].copy())
                    stages["successor_y"] = metrics(az["active_y"], bz["active_y"])
                    stages["diagnostic_pcm"] = metrics(np.concatenate((a["pcm"], az["pcm"])),
                                                       np.concatenate((b["pcm"], bz["pcm"])))
                    results.append(dict(id=c["id"], scope=c["scope"], call=c["call"],
                                        interpolation=c["interpolation"], preflat=p, mode=mode,
                                        pair=f"{other}-{ref}", stages=stages,
                                        branch_changes={k: int(np.count_nonzero(a["branches"][k] != b["branches"][k]))
                                                        for k in a["branches"]}))
    return dict(schema="macinac4.qmf-width-downstream-comparison", version=1,
                status="observation_only", output_fitting=False, runs=len(observations),
                real_envelope_variants=sum(len(c["envelopes"]) for c in m["cases"])*42,
                results=results, observations=observations)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("manifest", type=Path)
    p.add_argument("inputs", type=Path)
    p.add_argument("production", type=Path)
    p.add_argument("--output", required=True, type=Path)
    a = p.parse_args()
    try:
        report = compare(a.manifest, a.inputs, a.production)
        with a.output.open("x") as f:
            json.dump(report, f, ensure_ascii=False, indent=2, allow_nan=False)
            f.write("\n")
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"宽度下游检查失败：{error}")
        return 2
    print(json.dumps({k: report[k] for k in ("runs", "real_envelope_variants", "status")}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
