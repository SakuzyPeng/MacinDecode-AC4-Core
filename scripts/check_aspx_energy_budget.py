#!/usr/bin/env python3
"""审计实际 A-SPX 链的能量分配；不读取参考 PCM，不拟合增益或相位。

输入由 capture_ajoc_input_boundary 的 MACINAC4_HF_COMPONENTS 观察器产生。
P90/P94/P95–101 分别核对，再用表 D.2 重建噪声和实际 f32 组装。
有限窗口的信号／噪声互相关单独记账，不要求样本能量等于统计预算。
TS103190-1:v1.4.1:5.7.6.4.2–5.7.6.4.5；非官方符合性测试。
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import sys

import numpy as np

EPS = float(np.finfo(np.float32).eps)
FLOOR = 1e-12
MAX_BOOST = 1.584893192
FLOAT_FIELDS = ('estimated', 'target', 'ratio', 'initial_noise', 'initial_sine',
                'signal', 'noise', 'sine', 'boost')


def integer(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise ValueError('整数身份或边界越界')
    return value


def borders(value, low, high):
    if (not isinstance(value, list) or len(value) < 2
            or value[0] != low or value[-1] != high):
        raise ValueError('分组首末边界不符')
    for x in value:
        integer(x, low, high)
    if any(a >= b for a, b in zip(value, value[1:])):
        raise ValueError('分组边界必须严格递增')
    return value


def vector(value, count, boolean=False):
    if not isinstance(value, list) or len(value) != count:
        raise ValueError('矩阵长度不符')
    if boolean:
        if any(type(v) is not bool for v in value):
            raise ValueError('标志必须为布尔值')
        return np.array(value, bool)
    if any(type(v) not in (float, int) or not math.isfinite(v)
           or v < 0 or v > np.finfo(np.float32).max for v in value):
        raise ValueError('需要有限非负 f32')
    return np.array(value, '<f4').astype(float)


def checked_metadata(meta):
    """把 Rust 的最短十进制 f32 恢复成精确二进制值，再进行 f64 运算。"""
    integer(meta['au'], 1, 2**63-1)
    integer(meta['source'], 0, 63)
    lo, hi = integer(meta['sbx'], 0, 63), integer(meta['sbz'], 1, 64)
    if lo >= hi:
        raise ValueError('高带范围为空')
    factor = integer(meta['factor'], 1, 2)
    edges = meta['borders']
    if not isinstance(edges, list) or not 2 <= len(edges) <= 6:
        raise ValueError('信号包络数量越界')
    for x in edges:
        integer(x, 0, 38 // factor)
    borders(edges, edges[0], edges[-1])
    e, n = len(edges)-1, hi-lo
    a = meta['gain_audit']
    if type(a['interpolation']) is not bool:
        raise ValueError('interpolation 必须为布尔值')
    result = {k: vector(a[k], e*n).reshape(e, n) for k in FLOAT_FIELDS}
    for k in ('sine_index', 'sine_area'):
        result[k] = vector(a[k], e*n, True).reshape(e, n)
    result['onset'] = vector(a['onset'], e, True)
    if np.any(result['target'] < np.finfo(np.float32).tiny) or np.any(result['ratio'] < np.finfo(np.float32).tiny):
        raise ValueError('实际反量化标度因子须为正规数')
    if not isinstance(a['signal_borders'], list) or len(a['signal_borders']) != e:
        raise ValueError('缺失信号分组')
    result['signal_borders'] = [borders(x, lo, hi) for x in a['signal_borders']]
    if not isinstance(a['limiter_borders'], list):
        raise ValueError('limiter 分组必须为列表')
    result['limiter_borders'] = borders(a['limiter_borders'], lo, hi) if a['limiter_borders'] else []
    result['interpolation'] = a['interpolation']
    mask = meta['noise_enabled']
    if not isinstance(mask, list) or len(mask) != e*n or any(type(v) is not int or v not in (0, 1) for v in mask):
        raise ValueError('注噪掩码无效')
    result['enabled'] = np.array(mask, bool).reshape(e, n)
    if not np.array_equal(vector(meta['signal_gains'], e*n).reshape(e, n), result['signal']):
        raise ValueError('观察到的增益不是组装实际消费的增益')
    cursors = meta['noise_cursors']
    if not isinstance(cursors, list) or len(cursors) != 2:
        raise ValueError('缺失噪声游标')
    for x in cursors:
        integer(x, 0, 511)
    if (cursors[0] + (edges[-1]-edges[0])*factor*n) % 512 != cursors[1]:
        raise ValueError('噪声游标没有按完整区间推进')
    for env, freq in enumerate(result['signal_borders']):
        for left, right in zip(freq, freq[1:]):
            b = slice(left-lo, right-lo)
            if (np.any(result['target'][env, b] != result['target'][env, left-lo])
                    or np.any(result['sine_area'][env, b] != np.any(result['sine_index'][env, b]))):
                raise ValueError('P91/P93 的分组映射不符')
    return result


def close(actual, expected, name, units=8):
    """固定前向误差界：至多八次 f32 舍入；不从实测误差选阈值。"""
    actual, expected = np.asarray(actual, float), np.asarray(expected, float)
    scale = np.maximum(abs(expected), float(np.nextafter(np.float32(0), np.float32(1))))
    relative = abs(actual-expected)/scale
    if not np.isfinite(relative).all() or np.any(relative > units*EPS):
        raise ValueError(f'{name} 超出固定浮点误差界')
    return float(np.max(relative, initial=0))


def predict_gain(a, lo):
    """按功率方程计算 P95–101；仅在规范对应的 f32 矩阵边界收窄。"""
    s, q, energy = a['target'], a['ratio'], a['estimated']
    denominator = (1+energy)*np.where(a['sine_area'] | ~a['onset'][:, None], 1+q, 1)
    initial = np.sqrt(s*np.where(a['sine_area'], q, 1)/denominator).astype('<f4').astype(float)
    gain, noise = initial.copy(), a['initial_noise'].copy()
    cap, boost = np.full_like(gain, np.inf), np.ones_like(gain)
    groups = []
    edges = a['limiter_borders']
    if edges:
        for env in range(s.shape[0]):
            for left, right in zip(edges, edges[1:]):
                b = slice(left-lo, right-lo)
                total = math.fsum(s[env, b])
                maximum = min(1.41254*math.sqrt(total/(FLOOR+math.fsum(energy[env, b]))), 1e5)
                cap[env, b] = float(np.float32(maximum))
                if np.any(initial[env, b] <= 0):
                    raise ValueError('P97 初始增益不可除')
                gain[env, b] = np.minimum(initial[env, b], cap[env, b])
                noise[env, b] = np.minimum(noise[env, b], (noise[env, b]*cap[env, b]/initial[env, b]).astype('<f4'))
                before = math.fsum(energy[env, b]*gain[env, b]**2 + a['initial_sine'][env, b]**2
                                   + np.where(a['enabled'][env, b], noise[env, b]**2, 0))
                raw_boost = math.sqrt((FLOOR+total)/(FLOOR+before))
                boost[env, b] = float(np.float32(min(raw_boost, MAX_BOOST)))
                groups.append(dict(env=env, first=left, last=right, target=total,
                                   estimated=math.fsum(energy[env, b]), cap=float(cap[env, left-lo]),
                                   limited_subbands=int(np.sum(initial[env, b] > cap[env, b])),
                                   before_boost=before, boost=float(boost[env, left-lo]),
                                   boost_capped=raw_boost > MAX_BOOST,
                                   floor_adjusted_budget=before*(FLOOR+total)/(FLOOR+before)))
    return dict(initial=initial, cap=cap, limited_gain=gain, limited_noise=noise, boost=boost,
                signal=(gain*boost).astype('<f4').astype(float),
                noise=(noise*boost).astype('<f4').astype(float),
                sine=(a['initial_sine']*boost).astype('<f4').astype(float), groups=groups)


def energy_terms(a, env, signal, noise, sine, assembled, band):
    sig, noi, ton, y = [v[:, band].astype(float) for v in (signal, noise, sine, assembled)]
    mean_energy = lambda value: float(np.sum(value*value)/len(value))
    enabled = a['enabled'][env, band]
    modeled_signal = float(np.sum(a['estimated'][env, band]*a['signal'][env, band]**2))
    modeled_noise = float(np.sum(np.where(enabled, a['noise'][env, band]**2, 0)))
    modeled_sine = float(np.sum(a['sine'][env, band]**2))
    cross = float(2*np.sum(sig*noi+sig*ton+noi*ton)/len(sig))
    totals = dict(target=float(np.sum(a['target'][env, band])),
                  modeled_signal=modeled_signal, modeled_noise=modeled_noise,
                  modeled_sine=modeled_sine, signal=mean_energy(sig),
                  noise=mean_energy(noi), sine=mean_energy(ton), cross=cross,
                  assembled=mean_energy(y))
    totals['signal_grouping_and_multiply_rounding'] = totals['signal']-modeled_signal
    totals['noise_window_delta'] = totals['noise']-modeled_noise
    totals['sine_rounding'] = totals['sine']-modeled_sine
    totals['assembly_rounding'] = totals['assembled']-mean_energy(sig+noi+ton)
    reconstructed = sum(totals[k] for k in ('modeled_signal', 'modeled_noise', 'modeled_sine',
                        'signal_grouping_and_multiply_rounding', 'noise_window_delta',
                        'sine_rounding', 'cross', 'assembly_rounding'))
    # 相消很强时用各项绝对量控制 f64 求和误差，不能用相消后的微小 Y 作误差分母。
    scale = sum(abs(totals[k]) for k in ('signal', 'noise', 'sine', 'cross', 'assembly_rounding'))
    if abs(reconstructed-totals['assembled']) > 64*np.finfo(float).eps*scale:
        raise ValueError('能量分解不闭合')
    return totals


def audit(meta, data, table):
    a = checked_metadata(meta)
    lo, hi, factor = meta['sbx'], meta['sbz'], meta['factor']
    edges = [v*factor for v in meta['borders']]
    slots, n = edges[-1]-edges[0], hi-lo
    for key in ('high', 'noise', 'sine', 'assembled'):
        shape = (edges[-1] if key == 'assembled' else slots, 64, 2)
        if (data[key].shape != shape or data[key].dtype != np.dtype('<f4')
                or not np.isfinite(data[key]).all()):
            raise ValueError(f'{key} 长度、类型或有限性不符')
    if table.shape != (512, 2) or table.dtype != np.dtype('<f4') or not np.isfinite(table).all():
        raise ValueError('噪声表必须为 512 个有限 f32 复数')
    expected_noise = np.sqrt(a['target']*a['ratio']/(1+a['ratio']))
    expected_sine = np.sqrt(a['target']/(1+a['ratio']))*a['sine_index']
    errors = dict(initial_noise=close(a['initial_noise'], expected_noise, 'P94 噪声幅度'),
                  initial_sine=close(a['initial_sine'], expected_sine, 'P94 正弦幅度'))
    expected_mask = ~a['onset'][:, None] & (a['initial_sine'] == 0)
    if not np.array_equal(a['enabled'], expected_mask):
        raise ValueError('注噪条件与 P99 不符')
    prediction = predict_gain(a, lo)
    for key in ('signal', 'noise', 'sine', 'boost'):
        errors[key] = close(a[key], prediction[key], f'P95–101 {key}')
    report = []
    noise_indices = (meta['noise_cursors'][0]+np.arange(1, slots*n+1).reshape(slots, n)) % 512
    for env, (first, last) in enumerate(zip(edges, edges[1:])):
        window = slice(first-edges[0], last-edges[0])
        high = data['high'][window, lo:hi]
        power = np.sum(high.astype(float)**2, axis=2)
        actual_per_band = np.mean(power, axis=0)
        estimated = actual_per_band.copy()
        if not a['interpolation']:
            for left, right in zip(a['signal_borders'][env], a['signal_borders'][env][1:]):
                b = slice(left-lo, right-lo)
                estimated[b] = np.mean(power[:, b])
        errors['estimate'] = max(errors.get('estimate', 0), close(a['estimated'][env], estimated, 'P90 区域平均', 2))
        noise = table[noise_indices[window]]*a['noise'][env].astype('<f4')[None, :, None]
        if noise.tobytes() != data['noise'][window, lo:hi].tobytes():
            raise ValueError('P102 噪声不符合实际电平、表或游标')
        sine = data['sine'][window, lo:hi]
        close(np.sum(sine.astype(float)**2, axis=2), a['sine'][env]**2, 'P104 音调功率', 2)
        signal = high*a['signal'][env].astype('<f4')[None, :, None]
        rendered = signal.copy()
        enabled = a['enabled'][env]
        rendered[:, enabled] += noise[:, enabled]
        rendered += sine
        y = data['assembled'][first:last, lo:hi]
        if rendered.tobytes() != y.tobytes():
            raise ValueError('P106–108 实际组装并非逐位一致')
        noise = noise.copy()
        noise[:, ~enabled] = 0
        totals = energy_terms(a, env, signal, noise, sine, y, slice(None))
        overlap = slice(max(lo, 32)-lo, min(hi, 48)-lo)
        selected_totals = energy_terms(a, env, signal, noise, sine, y, overlap) if max(lo, 32) < min(hi, 48) else None
        report.append(dict(env=env, first=first, last=last,
                           limited_subbands=int(np.sum(prediction['initial'][env] > prediction['cap'][env])),
                           noise_enabled_subbands=int(np.sum(enabled)),
                           boost_min=float(np.min(a['boost'][env])), boost_max=float(np.max(a['boost'][env])),
                           totals=totals, subbands_32_48=selected_totals))
    for group in prediction['groups']:
        env, b = group['env'], slice(group['first']-lo, group['last']-lo)
        group['after_boost'] = float(np.sum(a['estimated'][env, b]*a['signal'][env, b]**2
                                           + a['sine'][env, b]**2
                                           + np.where(a['enabled'][env, b], a['noise'][env, b]**2, 0)))
        expected = group['before_boost']*group['boost']**2
        close(group['after_boost'], expected, 'P101 组预算', 4)
    return dict(au=meta['au'], source=meta['source'], sbx=lo, sbz=hi,
                interpolation=a['interpolation'], limiter=bool(a['limiter_borders']),
                subband_envelopes=a['target'].size, noise_cursors=meta['noise_cursors'],
                max_relative_errors=errors, noise_bit_exact=True, assembly_bit_exact=True,
                envelopes=report, limiter_groups=prediction['groups'])


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def audit_file(path, table):
    meta = json.loads(path.read_text())
    checked_metadata(meta)
    data, hashes = {}, {'metadata': digest(path)}
    for name in ('high', 'noise', 'sine', 'assembled'):
        source = path.with_suffix(f'.{name}.f32')
        if source.stat().st_size > 38*64*2*4 or source.stat().st_size % (64*2*4):
            raise ValueError('QMF 文件长度越界')
        data[name] = np.fromfile(source, '<f4').reshape(-1, 64, 2)
        hashes[name] = digest(source)
    return dict(metadata=str(path), sha256=hashes, **audit(meta, data, table))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('metadata', type=Path, nargs='+')
    parser.add_argument('--noise-table', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if len(set(p.resolve() for p in args.metadata)) != len(args.metadata):
            raise ValueError('重复的采集路径')
        if args.noise_table.stat().st_size != 512*2*4:
            raise ValueError('噪声表长度错误')
        table = np.fromfile(args.noise_table, '<f4').reshape(512, 2)
        result = dict(schema='macinac4.aspx-energy-budget', version=1,
                      status='local_equations_and_observations',
                      noise_table_sha256=digest(args.noise_table),
                      records=[audit_file(p, table) for p in args.metadata])
        with args.output.open('x') as stream:
            json.dump(result, stream, ensure_ascii=False, allow_nan=False, indent=2)
            stream.write('\n')
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(f'能量审计失败：{error}', file=sys.stderr)
        return 2
    print(f"已审计 {len(result['records'])} 个实际 A-SPX 调用。")
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
