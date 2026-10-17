#!/usr/bin/env python3
"""独立比较 8/48 与 64 子带 QMF；几何偏移固定，不搜索相位、增益或延迟。

许可窗、参考脉冲响应和真实输入均为外部数据。本脚本仅用于观察，不能启用生产策略。
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

import numpy as np

if __package__:
    from .check_aspx_basis_replay import components, complex_values, metrics
    from .check_aspx_noise_replay import floats, integer
    from .check_asf_imdct_checkpoints import identifier, identical, synthesize_block
    from .check_qmf_analysis_checkpoints import (
        AMPLITUDES, FREQUENCIES, PHASES, fft_analysis, grid_input, ideal_phases,
    )
else:
    from check_aspx_basis_replay import components, complex_values, metrics
    from check_aspx_noise_replay import floats, integer
    from check_asf_imdct_checkpoints import identifier, identical, synthesize_block
    from check_qmf_analysis_checkpoints import (
        AMPLITUDES, FREQUENCIES, PHASES, fft_analysis, grid_input, ideal_phases,
    )


def center_offset(width):
    if width not in (8, 48, 64):
        raise ValueError("未覆盖的 QMF 宽度")
    return (64/width-1)/2


def delay_kernel(delta, radius):
    if radius not in (128, 256, 512) or not np.isfinite(delta) or abs(delta) > 4:
        raise ValueError("非法诊断插值参数")
    index = np.arange(-radius, radius+1, dtype=float)
    kernel = np.sinc(index-delta)*np.kaiser(len(index), 12)
    return kernel/np.sum(kernel)


def fractional_delay(pcm, delta, radius):
    x = np.asarray(pcm, dtype=float)
    if x.ndim != 1 or not len(x) or not np.isfinite(x).all():
        raise ValueError("非法 PCM")
    kernel = delay_kernel(delta, radius)
    size = 1 << (len(x)+len(kernel)-2).bit_length()
    y = np.fft.irfft(np.fft.rfft(x, size)*np.fft.rfft(kernel, size), size)
    return y[radius:radius+len(x)].astype('<f4')


def sample_centered(pcm, width, radius=256):
    """在已知物理网格取样；使用整数余数确定相位，不增加抗混叠滤波。"""
    center_offset(width)
    if len(pcm) % 64:
        raise ValueError("输入不是完整时隙")
    index = np.arange(len(pcm)*width//64, dtype=np.int64)
    numerator = (2*index+1)*64-width
    whole, remainder = divmod(numerator, 2*width)
    result = np.empty(len(index), dtype='<f4')
    for phase in np.unique(remainder):
        shifted = fractional_delay(pcm, -float(phase)/(2*width), radius)
        mask = remainder == phase
        result[mask] = shifted[whole[mask]]
    return result


def ideal_kernel(width, window):
    center_offset(width)
    w = np.asarray(window, dtype=float)
    if w.shape != (10*width,) or not np.isfinite(w).all():
        raise ValueError("窗形状错误")
    m = np.arange(10*width)
    k = np.arange(width)
    rotation = -np.exp(1j*np.pi*(k+.5)*(1/width-1/128))
    return (w[:, None]*np.exp(1j*np.pi*((m % (2*width))-.5)[:, None]
                             *(k+.5)[None, :]/width)*rotation/(2*width))


def impulse_kernel(width, observations):
    if set(observations) != set(range(width)):
        raise ValueError("缺失或重复的样本位置脉冲")
    result = np.empty((10*width, width), dtype=complex)
    for phase, values in observations.items():
        if values.shape != (24, width) or not np.isfinite(values).all():
            raise ValueError("脉冲结果形状错误")
        if np.any(values[:2]) or np.any(values[12:]):
            raise ValueError("脉冲位置或滤波器支持区间错误")
        result[np.arange(10)*width+width-1-phase] = values[2:12]
    return result


def apply_kernel(pcm, kernel, history=None):
    width = kernel.shape[1]
    if kernel.shape != (10*width, width) or len(pcm) % width or not np.isfinite(kernel).all():
        raise ValueError("算子形状错误")
    x = np.asarray(pcm, dtype=float)
    h = np.zeros((9, width)) if history is None else np.asarray(history, dtype=float)
    if h.shape != (9, width) or not np.isfinite(h).all() or not np.isfinite(x).all():
        raise ValueError("历史或输入无效")
    stream = np.concatenate((h[::-1].reshape(-1), x))
    windows = np.lib.stride_tricks.sliding_window_view(stream, 10*width)[::width][:, ::-1]
    return windows @ kernel


def frequency_response(kernel, frequencies, centered=True):
    width = kernel.shape[1]
    m = np.arange(10*width)
    position = -((m+.5)*64/width+.5)
    if not centered:
        position -= center_offset(width)
    return np.exp(2j*np.pi*np.asarray(frequencies)[:, None]*position/48000) @ kernel


def phase_factor(width, frequencies):
    centers = (np.arange(width)+.5)*375
    return np.exp(-2j*np.pi*(np.asarray(frequencies)[:, None]-centers)
                  *center_offset(width)/48000)


def score(ref, prediction):
    return metrics(np.asarray(ref, dtype='<f4'), components(prediction))


def source_power(ref, prediction):
    a = np.sum(ref.astype(float)**2, axis=-1)
    b = abs(prediction)**2
    return dict(relative_error_norm=float(np.linalg.norm(b-a)/np.linalg.norm(a)) if np.any(a) else None,
                aggregate_relative_bias=float((b.sum()-a.sum())/a.sum()) if np.any(a) else None)


def validate_cases(cases):
    names, scopes = set(), {}
    if not isinstance(cases, list) or not 1 <= len(cases) <= 100:
        raise ValueError("案例数错误")
    for case in cases:
        name = identifier(case['id'])
        scope = identifier(case['scope'])
        owner = integer(case['owner'], -1, 8)
        track = integer(case['transport_track'], 0, 9)
        width = integer(case['width'], 8, 48)
        last = integer(case['last_au'], 3, 1000)
        if name in names or width != (8 if owner == -1 else 48):
            raise ValueError("重复案例或错误宽度")
        if track != (0 if owner == -1 else (owner+6) % 9+1):
            raise ValueError("来源不符合 P14 置换")
        names.add(name)
        state = scopes.setdefault(scope, (last, set()))
        if state[0] != last or owner in state[1]:
            raise ValueError("重复所有者或区间不一致")
        state[1].add(owner)
        layouts = case['layouts']
        if not isinstance(layouts, list) or len(layouts) != last+1:
            raise ValueError("缺失前置历史或后置插值保护帧")
        for lengths in layouts:
            if not lengths or sum(lengths) != 2048:
                raise ValueError("窗口布局未覆盖整帧")
            for n in lengths:
                integer(n, 128, 2048)
                if n not in (128, 256, 512, 1024, 2048):
                    raise ValueError("未覆盖的块长")
        if owner != -1:
            targets, sources = case['targets'], case['sources']
            if not 1 <= len(sources) <= 5 or len(targets) != len(sources)+1:
                raise ValueError("patch 数量错误")
            for x in targets:
                integer(x, 1, 64)
            for source, lo, hi in zip(sources, targets[:-1], targets[1:], strict=True):
                integer(source, 0, 63)
                if lo >= hi or source+hi-lo > targets[0] or targets[0] > width:
                    raise ValueError("patch 源或目标越界")
    if any(owners != set(range(-1, 9)) for _, owners in scopes.values()):
        raise ValueError("缺失真实物理源")


def validate_grids(grids):
    keys, ids = set(), set()
    for case in grids:
        name = identifier(case['id'])
        n, pattern = integer(case['n'], 8, 64), integer(case['pattern'], 0, 3)
        if type(case['centered']) is not bool or name in ids:
            raise ValueError("公共波形身份或标志错误")
        key = n, pattern, case['centered']
        if key in keys:
            raise ValueError("重复公共波形几何")
        keys.add(key)
        ids.add(name)
    expected = {(n, p, c) for n, patterns in [(8, range(2)), (48, range(4))]
                for p in patterns for c in (False, True)} | {(64, p, False) for p in range(4)}
    if keys != expected:
        raise ValueError("公共波形几何覆盖不完整")


def short_pcm(case, spectra, windows):
    width = case['width']
    h, previous = np.zeros(32*width), 32*width
    result = []
    for lines, lengths in zip(spectra, case['layouts'], strict=True):
        offset = 0
        for original in lengths:
            n = original*width//64
            if np.any(lines[offset+n:offset+original]):
                raise ValueError("缩短变换会舍弃非零谱线")
            taper = min(n, previous)
            pcm, h, previous = synthesize_block(lines[offset:offset+original], original, width, h,
                                               previous, windows[width, taper])
            result.extend(pcm)
            offset += original
    result = np.asarray(result, dtype='<f4')
    delay = 352*width//64
    delayed = np.zeros_like(result)
    delayed[delay:] = result[:-delay]
    return delayed


def compare(manifest, data):
    m = json.loads(manifest.read_text())
    if m.get('schema') != 'macinac4.normalized-qmf-width' or type(m.get('version')) is not int or m['version'] != 1:
        raise ValueError("未支持的实验协议")
    validate_cases(m['cases'])
    validate_grids(m['grids'])
    windows = {n: floats(data/f'window{n}.f32', (10*n,)) for n in (8, 48, 64)}
    for n, window in windows.items():
        index = np.arange(10*n)
        prototype = window*np.where((index//(2*n)) % 2, -1, 1)
        if prototype[0] != 0 or not np.array_equal(prototype[1:], prototype[:0:-1]):
            raise ValueError("本实验的原型对称性前提不成立")
    ideal = {n: ideal_kernel(n, w) for n, w in windows.items()}
    measured, properties = {}, []
    for n in (8, 48):
        observations = {}
        for phase in range(n):
            name = f'basis{n:02}i{phase:02}'
            x = floats(data/f'{name}.input.f32', (24, n))
            expected = np.zeros_like(x)
            expected[2, phase] = 1
            y = floats(data/f'{name}.reference.f32', (24, n, 2))
            if not identical(x, expected) or not identical(y, floats(data/f'{name}.repeat.f32', y.shape)):
                raise ValueError("单位脉冲配方或独立重复不一致")
            observations[phase] = complex_values(y)
        measured[n] = impulse_kernel(n, observations)
        positions = np.arange(10*n)*64/n
        interpolated = np.interp(positions, np.arange(640), windows[64]).astype('<f4')
        properties.append(dict(width=n,delta_samples_48k=center_offset(n),symmetry_exact=True,
                               prototype_interpolation=metrics(windows[n], interpolated),
                               measured_vs_ideal_kernel=score(components(measured[n]), ideal[n])))
    native_windows = {(8, 256): floats(data/'kbd8n256.f32', (256,))}
    for length in (96, 192, 384, 768, 1536):
        native_windows[48, length] = floats(data/f'kbd48n{length}.f32', (length,))
    results = []
    for case in m['cases']:
        name, n, last = case['id'], case['width'], case['last_au']
        slots = (last+1)*32
        start = (last-2)*32
        x = floats(data/f'{name}.pcm64.f32', (slots*64,))
        spectra = floats(data/f'{name}.spectra.f32', (last+1, 2048))
        ref = floats(data/f'{name}.native_qmf.f32', (64, n, 2))
        if not identical(ref, floats(data/f'{name}.native_repeat.f32', ref.shape)):
            raise ValueError("真实 QMF 独立重复不一致")
        native_input = floats(data/f'{name}.native_input.f32', (64*n,))
        before = floats(data/f'{name}.native_before.f32', (9, n))
        after = floats(data/f'{name}.native_after.f32', (9, n))
        if not identical(after, native_input.reshape(64, n)[-9:][::-1].copy()):
            raise ValueError("捕获结束历史不是最近九个输入块")
        basis = -np.exp(1j*np.pi*(np.arange(64)+.5)/128)/128
        carrier = np.exp(1j*np.pi*(np.arange(n)+.5)*center_offset(n)/64)
        predictions, controls = {}, {}
        for mode, radius in [('base', 0), ('shift256', 256), ('shift512', 512)]:
            source = x if radius == 0 else fractional_delay(x, center_offset(n), radius)
            if radius and not identical(source, floats(data/f'{name}.{mode}.input.f32', source.shape)):
                raise ValueError("生产重放输入不是固定几何延迟")
            production = complex_values(floats(data/f'{name}.{mode}.qmf64.f32', (slots, 64, 2)))*basis
            pre, post = ideal_phases(64)
            model = fft_analysis(source.reshape(-1, 64), np.zeros((9, 64)), windows[64], pre, post)['qmf']
            controls[mode] = score(components(production), model)
            predictions[mode] = production[start:start+64, :n]*(carrier if radius else 1)
        sampled = sample_centered(x, n)
        predictions['same_source_native_operator'] = apply_kernel(sampled, measured[n])[start:start+64]
        sampled512 = apply_kernel(sample_centered(x, n, 512), measured[n])[start:start+64]
        short = short_pcm(case, spectra, native_windows)
        predictions['short_imdct_native_operator'] = apply_kernel(short, measured[n])[start:start+64]
        row = dict(id=name, scope=case['scope'], width=n, owner=case['owner'],
                   nonzero=bool(np.any(ref)), production_controls=controls,
                   comparisons={key: score(ref, value) for key, value in predictions.items()},
                   radius_convergence=score(components(predictions['shift512']), predictions['shift256']),
                   sampled_source_convergence=score(components(sampled512), predictions['same_source_native_operator']),
                   captured_input_operator_control=score(ref, apply_kernel(native_input, measured[n], before)),
                   source_sampling=metrics(native_input, sampled[start*n:(start+64)*n]),
                   short_source_control=metrics(native_input, short[start*n:(start+64)*n]))
        reference = complex_values(ref)
        d, c, b, e = (predictions[k] for k in ('base', 'shift256', 'same_source_native_operator', 'short_imdct_native_operator'))
        row['decomposition_max_abs_closure_error'] = float(np.max(abs((d-reference)-((d-c)+(c-b)+(b-e)+(e-reference)))))
        row['isolated_comparisons'] = dict(window_and_sampling_operator=score(components(b), c),
                                          source_realization=score(components(e), b))
        if case['owner'] != -1:
            low = floats(data/f'{name}.low.f32', (38, case['targets'][0], 2))
            if not identical(low, ref[26:64, :case['targets'][0]].copy()):
                raise ValueError("分析到 A-SPX 低带的交接错位")
            bands = sorted({source+i for source, lo, hi in zip(case['sources'], case['targets'][:-1], case['targets'][1:]) for i in range(hi-lo)})
            row['hf_source_bands'] = bands
            selected = ref[26:64, bands].copy()
            row['hf_source'] = {key: score(selected, value[26:64, bands]) for key, value in predictions.items()}
            row['hf_source_power'] = {key: source_power(selected, value[26:64, bands]) for key, value in predictions.items()}
            row['hf_sampled_source_convergence'] = score(components(sampled512[26:64, bands]), predictions['same_source_native_operator'][26:64, bands])
        results.append(row)
    sweeps = []
    offsets = np.array([-187.5, -93.75, 0., 93.75, 187.5])
    for n in (8, 48):
        frequencies = ((np.arange(n)+.5)*375)[:, None]+offsets
        freq = frequencies.reshape(-1)
        native = frequency_response(measured[n], freq)
        mathematical = frequency_response(ideal[n], freq)
        base = frequency_response(ideal[64], freq)[:, :n]
        predicted = base*phase_factor(n, freq)
        bands = np.repeat(np.arange(n), len(offsets))
        select = np.arange(len(freq)), bands
        ratio = mathematical[select]/predicted[select]
        native_ratio = native[select]/predicted[select]
        sweeps.append(dict(width=n, cases=len(freq), offsets_hz=offsets.tolist(),
                           ideal_phase_residual_max_radians=float(np.max(abs(np.angle(ratio)))),
                           measured_phase_residual_max_radians=float(np.max(abs(np.angle(native_ratio)))),
                           measured_magnitude_ratio_range=[float(abs(native_ratio).min()), float(abs(native_ratio).max())]))
    grids = []
    for case in m['grids']:
        name = identifier(case['id'])
        n, pattern = integer(case['n'], 8, 64), integer(case['pattern'], 0, 3)
        if n not in (8, 48, 64) or type(case['centered']) is not bool:
            raise ValueError("公共波形几何错误")
        x = floats(data/f'{name}.input.f32', (76, n))
        if np.max(abs(x-grid_input(n, pattern, case['centered']))) > 1e-7:
            raise ValueError("公共波形输入配方错误")
        ref = floats(data/f'{name}.reference.f32', (76, n, 2))
        if not identical(ref, floats(data/f'{name}.repeat.f32', ref.shape)):
            raise ValueError("公共波形参考不重复")
        freq = np.array(FREQUENCIES[pattern], dtype=float)
        amp = np.array(AMPLITUDES)*np.exp(1j*np.array(PHASES))/2
        freq, amp = np.concatenate((freq, -freq)), np.concatenate((amp, amp.conj()))
        time = (np.arange(12, 76)+1)*64-12*64
        oscillator = np.exp(2j*np.pi*time[:, None]*freq/48000)*amp
        base = frequency_response(ideal[64], freq)[:, :n]
        phase = base*phase_factor(n, freq)
        if not case['centered']:
            phase *= np.exp(-2j*np.pi*freq[:, None]*center_offset(n)/48000)
        native = frequency_response(measured[n] if n != 64 else ideal[64], freq, case['centered'])
        grids.append(dict(id=name, width=n, centered=case['centered'], pattern=pattern,
                          base=score(ref[12:], oscillator@base), phase_only=score(ref[12:], oscillator@phase),
                          impulse_operator=score(ref[12:], oscillator@native)))
    return dict(schema='macinac4.qmf-width-comparison', version=1, status='observation_only',
                phase_gain_delay_or_order_fitting=False, properties=properties, cases=results,
                passband_sweeps=sweeps, grids=grids)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('data', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        result = compare(args.manifest, args.data)
        with args.output.open('x') as target:
            json.dump(result, target, ensure_ascii=False, indent=2, allow_nan=False)
            target.write('\n')
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'QMF 宽度检查失败：{error}', file=sys.stderr)
        return 2
    print(json.dumps({'cases': len(result['cases']), 'grids': len(result['grids'])}))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
