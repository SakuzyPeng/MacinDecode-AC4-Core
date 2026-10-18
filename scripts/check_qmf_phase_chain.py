#!/usr/bin/env python3
"""独立 FIR 与解析稳态模型核对 64 带分析、搬运、合成；不采用 DRP 相位。"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np

try:
    from .check_aspx_basis_replay import components, complex_values, metrics
    from .check_aspx_noise_replay import floats
    from .check_patch_tables import derive as derive_patch
except ImportError:
    from check_aspx_basis_replay import components, complex_values, metrics
    from check_aspx_noise_replay import floats
    from check_patch_tables import derive as derive_patch


WINDOW_SHA256 = "870d3c80e4fe7ee368b5da1f7db487dfa84be44c4cf8c82813b78fb06a8e94a2"
OMEGA = np.pi * (np.arange(64) + .5) / 64
PATCH_CONFIGS = ((1, 6, 1, 3, 0), (1, 6, 0, 3, 0), (1, 6, 0, 3, 5))


def routes_for(config):
    master, start, stop, _, xover = config
    (counts, sources, borders), _ = derive_patch(bool(master), start, stop, xover, True)
    return [[p+j, k+j] for n, p, k in zip(counts, sources, borders) for j in range(n)]


def case_line(c):
    values = [c['id'], c['layout'], c['slots'], c['checkpoint'], c['delay']]
    if c['layout'] == 'patch':
        values.extend(c['config'])
    elif c['layout'] == 'route':
        values.extend(c['routes'][0])
    return ' '.join(map(str, values))


def synthetic_cases():
    result = []
    layouts = [('full', None)] + [('patch', list(config)) for config in PATCH_CONFIGS]
    def mapped(kind, label, point, **extras):
        for layout_index, (layout, config) in enumerate(layouts):
            for delay in (0, 6):
                c = dict(id=f'{label}m{layout_index}d{delay}', kind=kind, layout=layout,
                         checkpoint=point, slots=point+32, delay=delay, **extras)
                if config is not None:
                    c['config'] = config
                c['routes'] = routes_for(config) if config else [[b,b] for b in range(64)]
                result.append(c)
    for offset in range(64):
        mapped('impulse', f'i{offset:02}', 8, offset=offset)
    for pattern, exponent in [('zero',0),('random',0),('random',-20),('random',8),('chirp',0),('alternating',0)]:
        mapped('control', f'c{len(result):04}', 32, pattern=pattern, exponent=exponent)
    for source in range(64):
        for offset in (-.25,0.,.25):
            for delay in (0,6):
                result.append(dict(id=f't{len(result):04}',kind='tone',layout='route',slots=160,checkpoint=128,
                                   delay=delay,routes=[[source,(source+8)%64]],offset_bins=offset,phase=(source%4)*.37))
    for source in (0,1,2,3,18,19,28,29,34,35,62,63):
        for offset in (0.,.25):
            result.append(dict(id=f'o{len(result):04}',kind='odd_control',layout='route',slots=160,checkpoint=128,
                               delay=0,routes=[[source,(source+1)%64]],offset_bins=offset,phase=.37))
    return result


def synthetic_input(c):
    x = np.zeros(c['slots']*64,dtype='<f4')
    count = c['checkpoint']*64
    t = np.arange(count)
    if c['kind'] == 'impulse':
        x[128+c['offset']] = 1
    elif c['kind'] in ('tone','odd_control'):
        source = c['routes'][0][0]
        omega = OMEGA[source]+c['offset_bins']*np.pi/64
        x[:count] = np.cos(omega*t+c['phase'])
    elif c['kind'] == 'control':
        pattern = c['pattern']
        if pattern == 'random':
            state = 847012
            for i in range(count):
                state = (1664525*state+1013904223) % 2**32
                x[i] = ((state >> 8)/2**24-.5)*.25*2**c['exponent']
        elif pattern == 'chirp':
            x[:count] = np.sin(.003*t+.000005*t*t)
        elif pattern == 'alternating':
            x[:count] = np.where(t%2, -.75, .25)
        elif pattern != 'zero':
            raise ValueError('未知构造输入')
    else:
        raise ValueError('真实输入须保留来源摘要')
    return x


def validate_manifest(m):
    if m.get('schema') != 'macinac4.qmf-phase-chain' or type(m.get('version')) is not int or m['version'] != 1:
        raise ValueError('未支持的相位链协议')
    cases=m['cases']
    if not isinstance(cases,list) or not cases or len({c['id'] for c in cases}) != len(cases):
        raise ValueError('案例缺失或重复')
    expected={c['id']:c for c in synthetic_cases()}
    actual={c['id']:{k:v for k,v in c.items() if k!='input_sha256'} for c in cases if c['kind']!='real'}
    if actual != expected:
        raise ValueError('构造案例覆盖或配方不完整')
    real=[c for c in cases if c['kind']=='real']
    keys={(c['scope'],c['owner'],c['layout']) for c in real}
    required={(scope,owner,layout) for scope in ('tone','music100','music33') for owner in range(-1,9)
              for layout in (('full',) if owner==-1 else ('full','patch'))}
    if len(real)!=57 or keys!=required:
        raise ValueError('真实输入覆盖不完整')
    for c in real:
        if (c['slots'],c['checkpoint'],c['delay'])!=(106,74,6):
            raise ValueError('真实输入的预热或尾部错误')
        routes=routes_for(c['config']) if c['layout']=='patch' else [[b,b] for b in range(64)]
        if c['routes']!=routes or c['source_start_slot']<0:
            raise ValueError('真实输入的路由或来源错误')
    return cases


def prototype(window):
    w = np.asarray(window, dtype=float)
    if w.shape != (640,) or not np.isfinite(w).all():
        raise ValueError("原型窗形状或有限性错误")
    return w * np.where((np.arange(640)//128) % 2, -1, 1)


def analyse_fir(pcm, window):
    """绝对样本索引的 640 抽头复 FIR；不使用生产折叠或相位表。"""
    x = np.asarray(pcm, dtype=float)
    if x.ndim != 1 or not len(x) or len(x) % 64 or not np.isfinite(x).all():
        raise ValueError("非法完整时隙输入")
    j = np.arange(640)
    kernel = prototype(window)[:, None] * np.exp(1j*(j[:, None]-.5)*OMEGA)
    position = 64*np.arange(len(x)//64)[:, None] + 63 - j
    history = x[np.maximum(position, 0)] * (position >= 0)
    return history @ kernel


def synthesise_fir(qmf, window):
    """每 64 个样本插入一列复数 QMF，再用 640 抽头带通 FIR 合成。"""
    q = np.asarray(qmf, dtype=complex)
    if q.ndim != 2 or q.shape[1] != 64 or not len(q) or not np.isfinite(q).all():
        raise ValueError("非法 QMF 矩阵")
    a = prototype(window)
    out = np.zeros((len(q), 64), dtype=float)
    active = np.flatnonzero(np.any(q != 0, axis=0))
    for lag in range(10):
        if lag >= len(q):
            break
        j = 64*lag + np.arange(64)
        kernel = (a[j, None]/64 * np.exp(1j*(j[:, None]-127.5)*OMEGA[active])).T
        out[lag:] += np.real(q[:len(q)-lag, active] @ kernel)
    return out.reshape(-1)


def map_bands(q, routes, delay):
    q = np.asarray(q, dtype=complex)
    if q.ndim != 2 or q.shape[1] != 64 or delay not in (0, 6):
        raise ValueError("映射时间或形状错误")
    out = np.zeros_like(q)
    targets = set()
    for source, target in routes:
        if not 0 <= source < 64 or not 0 <= target < 64 or target in targets:
            raise ValueError("子带路由越界或目标重复")
        targets.add(target)
        out[delay:, target] = q[:len(q)-delay, source] if delay else q[:, source]
    return out


def analysis_response(window, band, omega):
    j = np.arange(640)
    return np.exp(1j*(omega*63-OMEGA[band]/2)) * np.sum(prototype(window)*np.exp(1j*(OMEGA[band]-omega)*j))


def synthesis_response(window, band, omega):
    j = np.arange(640)
    return np.exp(-1j*OMEGA[band]*127.5) * np.sum(prototype(window)*np.exp(1j*(OMEGA[band]-omega)*j)) / 64


def carrier_phasor(window, source, target, omega, phase, frequency, delay=0):
    """固定输入余弦的解析输出复幅度，含实输入的两个频率分支与重采样像。"""
    def coefficient(nu):
        total = 0j
        for sign in (-1, 1):
            alias = (nu-sign*omega)*64/(2*np.pi)
            if abs(alias-round(alias)) < 1e-10:
                excitation = .5*np.exp(1j*sign*(phase-omega*64*delay))
                total += excitation * analysis_response(window, source, sign*omega) * synthesis_response(window, target, nu) / 64
        return total
    return coefficient(frequency) + np.conj(coefficient(-frequency))


def measured_phasor(pcm, frequency, first=32*64, last=96*64):
    x = np.asarray(pcm, dtype=float)
    if not 0 <= first < last <= len(x):
        raise ValueError("稳态计分区间越界")
    time = np.arange(first, last)
    return 2*np.sum(x[first:last]*np.exp(-1j*frequency*time))/(last-first)


def forward_bounds(pcm, q, window, routes):
    # 相位表 f32 量化与分段 f32 存储的保守前向界；以输入幅度和算子绝对和定界，
    # 不以接近零的输出作分母，也不根据实测残差选择容差。
    a = abs(prototype(window))
    analysis = 4*np.finfo(np.float32).eps * np.max(abs(pcm)) * a.sum()
    polyphase = np.max(a.reshape(10, 64).sum(axis=0))
    synthesis = 4*np.finfo(np.float32).eps * np.max(np.sum(abs(q), axis=1)) * polyphase / 64
    chain = synthesis + analysis * len(routes) * polyphase / 64
    return float(analysis), float(synthesis), float(chain)


def require_bound(expected, actual, bound, label):
    if (expected.shape != actual.shape or not np.isfinite(actual).all()
            or not np.isfinite(expected).all() or not np.isfinite(bound) or bound < 0):
        raise ValueError(f"{label} 的形状或有限性错误")
    error = float(np.max(abs(actual-expected)))
    if error > bound:
        raise ValueError(f"{label} 超出固定前向界：{error} > {bound}")
    return error


def compare(manifest, data, output):
    m = json.loads(manifest.read_text())
    cases = validate_manifest(m)
    if (data/'cases.txt').read_text().splitlines()!=[case_line(c) for c in cases]:
        raise ValueError('生产重放参数与清单不符')
    w = floats(output/"qwin.f32", (640,))
    if hashlib.sha256(w.tobytes()).hexdigest() != WINDOW_SHA256:
        raise ValueError("原型窗不符合锁定摘要")
    logs = {}
    for line in (output/"results.txt").read_text().splitlines():
        fields = line.split()
        if len(fields) < 5 or fields[0] in logs:
            raise ValueError("重复或截断的生产结果")
        slots, checkpoint, delay, count = map(int, fields[1:5])
        if len(fields) != 5+2*count:
            raise ValueError("路由数量错误")
        logs[fields[0]] = (slots, checkpoint, delay, [tuple(map(int, fields[i:i+2])) for i in range(5, len(fields), 2)])
    if set(logs) != {c["id"] for c in cases}:
        raise ValueError("生产结果覆盖不完整")
    cache = {}
    results = []
    for c in cases:
        name, slots, point, delay = (c[k] for k in ("id", "slots", "checkpoint", "delay"))
        if not name.isascii() or not name.isalnum() or len(name) > 63 or logs[name][:3] != (slots, point, delay):
            raise ValueError("身份或时间网格不符")
        routes = logs[name][3]
        if routes != [tuple(r) for r in c["routes"]]:
            raise ValueError("生产 patch 与声明的来源不符")
        x = floats(data/f"{name}.input.f32", (slots*64,))
        digest = hashlib.sha256(x.tobytes()).hexdigest()
        if digest != c["input_sha256"] or np.any(x[point*64:]):
            raise ValueError("输入或完整尾部不符")
        if c['kind']!='real' and not np.array_equal(x.view('<u4'),synthetic_input(c).view('<u4')):
            raise ValueError('构造输入与独立配方不符')
        if digest not in cache:
            cache[digest] = analyse_fir(x, w)
        expected_a = cache[digest]
        actual_a_components = floats(output/f"{name}.analysis.f32", (slots, 64, 2))
        actual_a = complex_values(actual_a_components)
        actual_h = complex_values(floats(output/f"{name}.mapped.f32", (slots, 64, 2)))
        actual_y = floats(output/f"{name}.pcm.f32", (slots*64,))
        if c['kind']=='real':
            recorded=floats(data/f'{name}.recorded_qmf.f32',(64,64,2))
            if hashlib.sha256(recorded.tobytes()).hexdigest()!=c['recorded_qmf_sha256']:
                raise ValueError('既有生产 QMF 检查点摘要不符')
            if not np.array_equal(actual_a_components[10:74].view('<u4'),recorded.view('<u4')):
                raise ValueError('预热后的真实窗口未复现既有生产 QMF')
        expected_h = map_bands(expected_a, routes, delay)
        direct_h = map_bands(actual_a, routes, delay)
        if not np.array_equal(actual_h, direct_h):
            raise ValueError("搬运改变了源样本或漏掉了固定延迟")
        expected_y = synthesise_fir(expected_h, w)
        same_input_y = synthesise_fir(actual_h, w)
        a_bound, s_bound, c_bound = forward_bounds(x, actual_h, w, routes)
        e_a = require_bound(expected_a, actual_a, a_bound, "分析 FIR")
        e_s = require_bound(same_input_y, actual_y, s_bound, "同输入合成 FIR")
        e_c = require_bound(expected_y, actual_y, c_bound, "完整搬运链")
        history = floats(output/f"{name}.analysis_state.f32", (640,))
        expected_history = np.zeros(640, dtype="<f4")
        available = min(point*64, 640)
        expected_history[:available] = x[point*64-available:point*64][::-1]
        if not np.array_equal(history.view('<u4'), expected_history.view('<u4')):
            raise ValueError("分析历史与绝对时间轴不符")
        synthesis_history = floats(output/f"{name}.synthesis_state.f32", (1280,))
        r = np.arange(128)
        matrix = np.exp(1j*(r[:, None]-127.5)*OMEGA) / 64
        expected_history_s = np.zeros((10, 128))
        for lag in range(min(point, 10)):
            expected_history_s[lag] = np.real(matrix @ actual_h[point-1-lag])
        state_bound = 4*np.finfo(np.float32).eps*np.max(np.sum(abs(actual_h), axis=1))/64
        require_bound(expected_history_s.reshape(-1), synthesis_history, state_bound, "合成历史")
        result = dict(id=name,kind=c["kind"],scope=c.get("scope"),delay=delay,nonzero_input=bool(np.any(x)),
                      analysis=metrics(components(expected_a),components(actual_a)),
                      synthesis=metrics(np.asarray(same_input_y,dtype='<f4'),actual_y),
                      chain=metrics(np.asarray(expected_y,dtype='<f4'),actual_y),
                      forward_bounds=dict(analysis=a_bound,synthesis=s_bound,chain=c_bound),
                      forward_errors=dict(analysis=e_a,synthesis=e_s,chain=e_c),
                      analysis_history_bit_exact=True,chunked_output_bit_exact=True,flushed_state_zero=True)
        if c["kind"] in ("tone", "odd_control"):
            p, k = routes[0]
            omega = OMEGA[p]+c["offset_bins"]*np.pi/64
            nu = OMEGA[k]+c["offset_bins"]*np.pi/64
            expected = carrier_phasor(w,p,k,omega,c["phase"],nu,delay)
            actual = measured_phasor(actual_y,nu)
            if abs(actual-expected) > 2*c_bound:
                raise ValueError("固定解析频移的复幅度不符")
            result["carrier"] = dict(source=p,target=k,frequency_hz=float(nu*48000/(2*np.pi)),
                                     expected=[float(expected.real),float(expected.imag)],
                                     actual=[float(actual.real),float(actual.imag)],abs_error=float(abs(actual-expected)),
                                     phase_error_rad=float(np.angle(actual/expected)) if expected != 0 and actual != 0 else None)
        results.append(result)
    return dict(schema="macinac4.qmf-phase-chain-comparison",version=1,status="observation_only",
                production_dsp_changed=False,reference_used_as_oracle=False,phase_gain_or_delay_fitting=False,cases=results)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ("manifest","data","output"):
        p.add_argument(name,type=Path)
    p.add_argument("--report",type=Path,required=True)
    a=p.parse_args();r=compare(a.manifest,a.data,a.output)
    a.report.write_text(json.dumps(r,ensure_ascii=False,indent=2,allow_nan=False)+"\n")
    print(f"verified {len(r['cases'])} complete phase chains")


if __name__=="__main__":
    main()
