#!/usr/bin/env python3
"""固定分析输入和 SEC，分开比较当前相加与测试端无交织选择；不拟合参考。"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import numpy as np
try:
    from .check_aspx_basis_replay import complex_values, metrics
    from .check_aspx_noise_replay import floats, integer
    from .check_asf_imdct_checkpoints import identifier
    from .check_qmf_phase_chain import synthesise_fir
except ImportError:
    from check_aspx_basis_replay import complex_values, metrics
    from check_aspx_noise_replay import floats, integer
    from check_asf_imdct_checkpoints import identifier
    from check_qmf_phase_chain import synthesise_fir


def merge_model(q, y, crossover, delay=6):
    integer(crossover, 4, 63)
    integer(delay, 3, 6)
    if delay not in (3, 6) or q.shape != (96, 64, 2) or y.shape != q.shape or not np.isfinite(q).all() or not np.isfinite(y).all():
        raise ValueError("合并输入、延迟或有限性错误")
    if np.any(y[:, :crossover]) or np.any(y[64:]):
        raise ValueError("SEC 写入低带或超出声明区间")
    delayed = np.concatenate((q[32-delay:], np.zeros((32-delay, 64, 2), '<f4')))
    added = delayed+y
    selected = added.copy()
    selected[:, crossover:] = y[:, crossover:]
    return delayed, added, selected


def verify_merge(actual, expected):
    if actual.shape != expected.shape or not np.isfinite(actual).all() or actual.tobytes() != expected.tobytes():
        raise ValueError("生产合并不符合固定输入、延迟与声明模式")


def energy_terms(leak, sec, actual):
    if leak.shape != sec.shape or actual.shape != sec.shape or not leak.size:
        raise ValueError("能量分解形状错误")
    l, s, a = (x.astype(float) for x in (leak, sec, actual))
    if not all(np.isfinite(x).all() for x in (l, s, a)):
        raise ValueError("能量分解包含非有限数")
    le, se = float(np.sum(l*l)), float(np.sum(s*s))
    cross, measured = float(2*np.sum(l*s)), float(np.sum(a*a))
    closure = abs(measured-(le+se+cross))
    if closure > 5e-7*(le+se):
        raise ValueError("遗漏交叉项、错位或合并能量不闭合")
    return dict(leak=le, sec=se, cross=cross, combined=measured,
                rounding_delta=measured-(le+se+cross),
                relative_change=(measured-se)/se if se else None,
                coherence=cross/(2*np.sqrt(le*se)) if le*se else None,
                closure_abs=closure)


def pcm_stats(selected, added):
    if selected.shape != (6144,) or added.shape != selected.shape or not all(np.isfinite(v).all() for v in (selected, added)):
        raise ValueError("诊断 PCM 长度或有限性错误")
    # 两段控制加完整归零尾部；固定频带用整段 DFT，不搜索窗口或时延。
    x, y = selected.astype(float), added.astype(float)
    f = np.fft.rfftfreq(len(x), 1/48000)
    band = (f >= 12000) & (f < 18000)
    a, b = np.fft.rfft(x), np.fft.rfft(y)
    return dict(full=metrics(selected, added),
                band_12_18khz=dict(signal_energy=float(2*np.sum(abs(a[band])**2)/len(x)),
                                   error_energy=float(2*np.sum(abs(b[band]-a[band])**2)/len(x))),
                selected_peak=float(np.max(abs(x))), added_peak=float(np.max(abs(y))))


def validate_cases(cases):
    if not isinstance(cases, list) or not 1 <= len(cases) <= 4096:
        raise ValueError("案例数量错误")
    ids, local, reference, synthetic = set(), set(), set(), set()
    for c in cases:
        for k in ('id', 'input', 'sec'):
            identifier(c[k])
        if c['id'] in ids:
            raise ValueError("重复案例")
        ids.add(c['id'])
        lo = integer(c['sbx'], 4, 63)
        integer(c['sbz'], lo+1, 64)
        if c['kind'] == 'local':
            stem = f"{c['scope']}c{c['call']}i{c['interpolation']}{c['variant']}p{c['preflat']}m{c['mode']}"
            if c['previous_chain'] != stem or c['input'] != f"{c['scope']}c{c['call']}{c['variant']}q" or c['sec'] != stem+'y':
                raise ValueError("本地输入或 SEC 来源错位")
            key = (c['scope'], c['call'], c['variant'], c['interpolation'], c['preflat'], c['mode'])
            if key in local:
                raise ValueError("重复本地选项")
            local.add(key)
        elif c['kind'] == 'reference_fixed':
            key = c['scope'], c['call']
            if key in reference or c['variant'] != 'a':
                raise ValueError("重复或未知参考输入")
            reference.add(key)
            if c['reference'] != f"{c['scope']}c{c['call']}reference.f32":
                raise ValueError("参考出口身份不符")
            if c['input'] != f"{c['scope']}c{c['call']}aq" or c['sec'] != f"{c['scope']}c{c['call']}referencey":
                raise ValueError("参考来源错位")
        elif c['kind'] == 'synthetic':
            key = lo, c['phase_quarters']
            if key in synthetic:
                raise ValueError("重复构造输入")
            synthetic.add(key)
        else:
            raise ValueError("未知案例类型")
    scopes = ('tone', 'music100', 'music33')
    expected = {(s, c, v, i, p, m) for s in scopes for c in range(9)
                for v in ('a', 'b', 'b512', 'c', 'c512', 'd', 'e')
                for i in range(2) for p in range(2) for m in range(3)}
    if local != expected or reference != {(s, c) for s in scopes for c in range(9)} or synthetic != {(b, p) for b in (4, 36, 47, 63) for p in range(4)}:
        raise ValueError("覆盖遗漏或额外案例")


def compare(manifest, data, output):
    m = json.loads(manifest.read_text())
    if (m.get('schema'), m.get('version')) != ('macinac4.hf-merge-inputs', 1) or m.get('fit_parameters') is not False:
        raise ValueError("协议或拟合策略错误")
    validate_cases(m['cases'])
    expected_inputs = {'cases.txt', 'window.f32'} | {c[k]+'.f32' for c in m['cases'] for k in ('input', 'sec')} | {c['reference'] for c in m['cases'] if c['kind'] == 'reference_fixed'}
    if set(m['files']) != expected_inputs:
        raise ValueError("输入摘要覆盖不完整")
    for name, digest in m['files'].items():
        if hashlib.sha256((data/name).read_bytes()).hexdigest() != digest:
            raise ValueError(f"固定输入已改变：{name}")
    lines = [f"{c['id']} {c['input']} {c['sec']} {c['sbx']}" for c in m['cases']]
    if (data/'cases.txt').read_text().splitlines() != lines:
        raise ValueError("重放参数与声明不符")
    expected_output = {'replay_metadata.json'} | {f"{c['id']}.{mode}.{suffix}.f32" for c in m['cases'] for mode in ('add', 'select') for suffix in ('qmf', 'pcm')}
    if {p.name for p in output.iterdir()} != expected_output:
        raise ValueError("输出遗漏或多余")
    if json.loads((output/'replay_metadata.json').read_text()) != dict(schema='macinac4.merge-replay', version=1, delay=6, synthesis_initial_state='zero', candidate='select_sec_above_crossover_after_advancing_raw_history'):
        raise ValueError("合并模式或初始状态不符")
    window = floats(data/'window.f32', (640,))
    result = []
    for c in m['cases']:
        q = floats(data/f"{c['input']}.f32", (96, 64, 2))
        y = floats(data/f"{c['sec']}.f32", q.shape)
        lo = c['sbx']
        delayed, add, select = merge_model(q, y, lo)
        if c['kind'] == 'synthetic':
            expected_q = np.zeros_like(q)
            expected_q[:, lo-1, 0] = np.arange(96)+1
            z = [1, 1j, -1, -1j][c['phase_quarters']]
            expected_q[:, lo] = [z.real, z.imag]
            expected_y = np.zeros_like(y)
            expected_y[:64, lo, 0] = 1
            if q.tobytes() != expected_q.tobytes() or y.tobytes() != expected_y.tobytes():
                raise ValueError("构造输入偏离固定配方")
        pcm, qmf = {}, {}
        for mode, expected in [('add', add), ('select', select)]:
            actual = floats(output/f"{c['id']}.{mode}.qmf.f32", q.shape)
            verify_merge(actual, expected)
            p = floats(output/f"{c['id']}.{mode}.pcm.f32", (6144,))
            model = 2*synthesise_fir(complex_values(actual), window)
            if np.max(abs(p-model)) > 2e-6*np.max(abs(model)):
                raise ValueError("合成没有消费同一合并结果")
            if np.any(actual[70:]) or np.any(p[80*64:]):
                raise ValueError("未包含完整结束尾部")
            pcm[mode], qmf[mode] = p, actual
        reference = None
        if c['kind'] == 'reference_fixed':
            ref = floats(data/c['reference'], (32, 64, 2))
            if not np.array_equal(ref, select[:32]):
                raise ValueError("固定真实 SEC 未复现无交织出口")
            reference = dict(add=metrics(ref, add[:32].copy()), select=metrics(ref, select[:32].copy()))
        # 所有真实案例按当前完整 32 时隙计分；不把构造延续冒充真实下一 AU。
        leak, sec = delayed[:32, lo:], y[:32, lo:]
        result.append(dict(**c, energy=energy_terms(leak, sec, add[:32, lo:]),
                           high_band_difference=metrics(select[:32, lo:].copy(), add[:32, lo:].copy()),
                           leak_by_band=np.sum(delayed[:32].astype(float)**2, axis=(0, 2)).tolist(),
                           reference_comparison=reference, diagnostic_pcm=pcm_stats(pcm['select'], pcm['add'])))
    return dict(schema='macinac4.hf-merge-comparison', version=1, status='observation_only',
                production_changed=False, output_fitting=False, cases=result)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for key in ('manifest', 'data', 'output'):
        p.add_argument(key, type=Path)
    p.add_argument('--report', type=Path, required=True)
    a = p.parse_args()
    try:
        result = compare(a.manifest, a.data, a.output)
        with a.report.open('x') as f:
            json.dump(result, f, ensure_ascii=False, indent=2, allow_nan=False)
            f.write('\n')
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'合并检查失败：{error}')
        return 2
    print(json.dumps(dict(cases=len(result['cases']), status=result['status'])))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
