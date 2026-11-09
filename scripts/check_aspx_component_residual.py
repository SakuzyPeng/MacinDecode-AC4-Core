#!/usr/bin/env python3
"""固定 QMF 表示分解实际 A-SPX 高带残差；不拟合相位、尺度或时间。"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path

import numpy as np

try:
    from .check_aspx_basis_replay import ROTATION, complex_values, components, metrics
    from .check_aspx_noise_mask import render
    from .check_aspx_noise_replay import floats, integer
    from .check_ajoc_input import source_order
except ImportError:
    from check_aspx_basis_replay import ROTATION, complex_values, components, metrics
    from check_aspx_noise_mask import render
    from check_aspx_noise_replay import floats, integer
    from check_ajoc_input import source_order


FIELDS = ('local_high', 'local_noise', 'local_sine', 'local_assembled',
          'local_gain', 'local_noise_enabled', 'native_high', 'native_gain', 'native_assembled')


def decompose(row, data, table):
    """两侧先独立核对组装，再以已冻结的 R64 换算实际消费分量。"""
    first, last = integer(row['first'], 0, 37), integer(row['last'], 1, 38)
    lo, hi = integer(row['sbx'], 4, 47), integer(row['sbz'], 33, 64)
    if first >= last or lo >= hi or hi-lo > 44 or max(lo, 32) >= min(hi, 48):
        raise ValueError('时隙或高频区间无效')
    if type(row['onset']) is not bool:
        raise ValueError('onset 必须为布尔值')
    noise_cursor = integer(row['noise_cursor'], 0, 511)
    tone_cursor = integer(row['tone_cursor'], 0, 3)
    shape = (last-first, hi-lo, 2)
    for name in FIELDS:
        expected = (hi-lo,) if name in ('local_gain', 'local_noise_enabled') else (hi-lo, 3) if name == 'native_gain' else shape
        if (np.shape(data[name]) != expected or np.asarray(data[name]).dtype != np.dtype('<f4')
                or not np.isfinite(data[name]).all()):
            raise ValueError(f'{name} 形状或有限性错误')
    mask = data['local_noise_enabled']
    if np.any((mask != 0) & (mask != 1)) or np.any(data['local_gain'] < 0) or np.any(data['native_gain'] < 0):
        raise ValueError('增益或注噪掩码无效')
    if np.any(data['local_sine']) or np.any(data['native_gain'][:, 2]):
        raise ValueError('本协议只覆盖没有音调分量的实际窗口')
    local_signal = data['local_high']*data['local_gain'][None, :, None]
    local_noise = data['local_noise'].copy()
    local_noise[:, mask == 0] = 0
    assembled = local_signal+local_noise
    assembled += data['local_sine']
    if not np.isfinite(assembled).all():
        raise ValueError('本地组装运算溢出')
    if assembled.tobytes() != data['local_assembled'].tobytes():
        raise ValueError('本地实际组装不符合采集的增益、注噪条件或时间边界')

    gain = data['native_gain']
    native_signal = data['native_high']*gain[:, 0][None, :, None]
    native_noise = render(np.zeros(shape, '<f4'), gain, table, lo, noise_cursor, tone_cursor, row['onset'], 'both')
    predicted = render(data['native_high'], gain, table, lo, noise_cursor, tone_cursor, row['onset'], 'both')
    if not all(np.isfinite(value).all() for value in (native_signal, native_noise, predicted)):
        raise ValueError('参考重放运算溢出')
    # 覆盖幅度还原、乘法和组装的舍入；不按此次观测残差选择阈值。
    bound = 8*np.finfo(np.float32).eps*(abs(native_signal.astype(float))+abs(native_noise.astype(float)))+8*np.finfo(np.float32).tiny
    error = abs(predicted.astype(float)-data['native_assembled'].astype(float))
    if np.any(error > bound):
        raise ValueError('参考组装超出固定浮点前向界')

    common_signal = components(complex_values(local_signal)*ROTATION[lo:hi])
    common_noise = components(complex_values(local_noise)*ROTATION[lo:hi])
    common_y = components(complex_values(data['local_assembled'])*ROTATION[lo:hi])
    band = slice(max(lo, 32)-lo, min(hi, 48)-lo)
    ds = common_signal[:, band].astype(float)-native_signal[:, band].astype(float)
    dn = common_noise[:, band].astype(float)-native_noise[:, band].astype(float)
    dy = common_y[:, band].astype(float)-data['native_assembled'][:, band].astype(float)
    amplitude_delta = local_noise[:, band].astype(float)-native_noise[:, band].astype(float)
    totals = dict(error=float(np.sum(dy**2)), signal_error=float(np.sum(ds**2)),
                  noise_error=float(np.sum(dn**2)), cross=float(2*np.sum(ds*dn)),
                  phase_only_candidate_error=float(np.sum((ds+amplitude_delta)**2)),
                  reference_signal=float(np.sum(native_signal[:, band].astype(float)**2)),
                  reference_noise=float(np.sum(native_noise[:, band].astype(float)**2)))
    return dict(native_assembly_max_error_over_bound=float(np.max(error/bound)), totals=totals,
                signal=metrics(native_signal[:, band].copy(), common_signal[:, band].copy()),
                noise=metrics(native_noise[:, band].copy(), common_noise[:, band].copy()),
                assembled=metrics(data['native_assembled'][:, band].copy(), common_y[:, band].copy()),
                noise_table_coordinates=metrics(native_noise[:, band].copy(), local_noise[:, band].copy()))


def checked_path(root, entry):
    path = root/entry['path']
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    if digest != entry['sha256']:
        raise ValueError('检查点摘要改变')
    return path


def validate_layout(rows, cases):
    if (not rows or not cases or not all(isinstance(c['id'], str) and c['id'] for c in cases)
            or len({c['id'] for c in cases}) != len(cases)):
        raise ValueError('案例为空或重复')
    known = {c['id']: c for c in cases}
    seen = set()
    groups = {}
    for row in rows:
        case = known[row['case']]
        order = source_order(case['fullband'], True)
        call = integer(row['call'], 0, len(order)-1)
        env = integer(row['env'], 0, 4)
        source = integer(row['source'], 1, case['fullband'])
        au = integer(row['au'], 2, 2**63-1)
        integer(case['au'], 2, 2**63-1)
        key = row['case'], call, env
        if key in seen or source != order[call] or au != case['au']:
            raise ValueError('实际源身份、AU 或包络重复/错误')
        if (any(type(token) is not int for token in row['producer_tokens']+row['consumer_tokens'])
                or row['producer_tokens'] != row['consumer_tokens']
                or row['producer_tokens'] != list(range(call*32, (call+1)*32))):
            raise ValueError('参考 A-SPX 出口与矩阵实际输入身份不符')
        seen.add(key)
        groups.setdefault((row['case'], call), []).append(row)
    if set(groups) != {(c['id'], i) for c in cases for i in range(c['fullband'])}:
        raise ValueError('存在遗漏的真实输入源')
    for group in groups.values():
        if [r['env'] for r in group] != list(range(len(group))):
            raise ValueError('包络顺序或数量错误')
        for previous, current in zip(group, group[1:]):
            if (previous['last'] != current['first']
                    or (previous['sbx'], previous['sbz']) != (current['sbx'], current['sbz'])):
                raise ValueError('包络边界不连续')


def compare(path):
    root = path.parent
    manifest = json.loads(path.read_text())
    if (manifest.get('schema'), manifest.get('version')) != ('macinac4.aspx-component-residual', 1):
        raise ValueError('未知协议')
    if manifest.get('fit_parameters') is not False:
        raise ValueError('不允许拟合表示参数')
    rows, cases = manifest['rows'], manifest['cases']
    validate_layout(rows, cases)
    for case in cases:
        checked_path(root, case['input'])
    table = floats(checked_path(root, manifest['noise_table']), (512, 2))
    details = []
    for row in rows:
        first, last, lo, hi = row['first'], row['last'], row['sbx'], row['sbz']
        # 在按输入尺寸读取文件之前约束几何。
        integer(first, 0, 37); integer(last, first+1, 38)
        integer(lo, 4, 47); integer(hi, max(lo+1, 33), min(lo+44, 64))
        data = {}
        if set(row['files']) != set(FIELDS):
            raise ValueError('分量文件清单缺失或多余')
        for name in FIELDS:
            shape = (hi-lo,) if name in ('local_gain', 'local_noise_enabled') else (hi-lo, 3) if name == 'native_gain' else (last-first, hi-lo, 2)
            data[name] = floats(checked_path(root, row['files'][name]), shape)
        details.append(dict(case=row['case'], call=row['call'], source=row['source'], env=row['env'],
                            **decompose(row, data, table)))
    summaries = []
    for case in cases:
        group = [r for r in details if r['case'] == case['id']]
        totals = {key: sum(r['totals'][key] for r in group) for key in group[0]['totals']}
        energy = totals['error']
        summaries.append(dict(id=case['id'], au=case['au'], calls=case['fullband'], envelopes=len(group), totals=totals,
                              energy_closure_relative_residual=(energy-totals['signal_error']-totals['noise_error']-totals['cross'])/energy if energy else 0,
                              phase_only_candidate_improvement_db=10*math.log10(energy/totals['phase_only_candidate_error']) if energy and totals['phase_only_candidate_error'] else None))
    return dict(schema='macinac4.aspx-component-residual-comparison', version=1,
                status='observation_only', fit_parameters=False, cases=summaries, details=details,
                limitations=['选定内部窗口的分量归因不等于整段对象 PCM 的修复收益。',
                             'noise_table_coordinates 和 phase-only 候选只诊断表示差异，不改变生产策略。',
                             '捕获来源与实际运行状态须由独立进程、完整 PCM 和源身份凭证另行验证。'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    result = compare(args.manifest)
    with args.output.open('x') as stream:
        json.dump(result, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write('\n')
    print(f"{len(result['cases'])} cases / {len(result['details'])} envelopes verified")


if __name__ == '__main__':
    main()
