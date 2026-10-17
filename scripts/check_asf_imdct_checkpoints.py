#!/usr/bin/env python3
"""比较 ASF 频谱、独立 IMDCT 重放及分析器之前的 PCM；不搜索排列、增益或延迟。

只消费外部归一化检查点。窗由调用方提供，不分发参考表或许可表。
较短 IMDCT 和居中窗取样是诊断模型，不修改生产实现。
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

import numpy as np

if __package__:
    from .check_aspx_basis_replay import components, complex_values, metrics
    from .check_aspx_noise_replay import floats, integer
else:
    from check_aspx_basis_replay import components, complex_values, metrics
    from check_aspx_noise_replay import floats, integer


def identifier(value):
    if not isinstance(value, str) or re.fullmatch(r"[A-Za-z0-9]+", value) is None:
        raise ValueError("非法记录身份")
    return value


def score(reference, prediction):
    return metrics(np.asarray(reference, dtype='<f4'), np.asarray(prediction, dtype='<f4'))


def identical(x, y):
    return x.shape == y.shape and np.array_equal(x.view('<u4'), y.view('<u4'))


def dct_iv(values):
    """未归一化的余弦定义式；用 2N 点 FFT 分解，测试另与密集矩阵核对。"""
    x = np.asarray(values, dtype=float)
    if x.ndim != 1 or len(x) < 4 or len(x) % 4 or not np.isfinite(x).all():
        raise ValueError("非法 DCT 输入")
    n = len(x)
    index = np.arange(n)
    before = x * np.exp(-1j * np.pi * index / (2*n))
    return (np.fft.fft(before, n=2*n)[:n]
            * np.exp(-1j * np.pi * (index+.5) / (2*n))).real


def unfold(values):
    y = np.asarray(values)
    n = len(y)
    return np.concatenate((y[n//2:], -y[n//2:][::-1], -y[:n//2][::-1], -y[:n//2]))


def paired_pre_rotation(values):
    x = np.asarray(values, dtype=float)
    n = len(x)
    return .5*(x[::2]+1j*x[-1::-2])*np.exp(-1j*np.pi*(np.arange(n//2)+.25)/n)


def paired_post_rotation(values):
    n = 2*len(values)
    z = values*np.exp(-1j*np.pi*np.arange(n//2)/n)
    result = np.empty(n)
    result[::2], result[-1::-2] = 2*z.real, -2*z.imag
    return result


def centered_window(window, length):
    if length < 4 or length > len(window) or not np.isfinite(window).all():
        raise ValueError("非法窗取样")
    position = (np.arange(length)+.5)*len(window)/length-.5
    return np.interp(position, np.arange(len(window)), window).astype('<f4')


def synthesize_block(lines, original_length, width, history, previous, window):
    """按 P63/P64 的时间轴诊断较短变换；采用已声明的原始接口尺度。"""
    n = original_length*width//64
    frame = len(history)
    if original_length*width % 64 or len(lines) < n or n > frame:
        raise ValueError("变换尺寸不匹配")
    if not 4 <= previous <= frame or (frame-n) % 2 or (frame-previous) % 2:
        raise ValueError("非法历史长度")
    if len(window) != min(n, previous) or not np.isfinite(window).all():
        raise ValueError("重叠窗长度错误")
    # 本实验原语将 1/M 归一化放在谱线上；2^-16 是该内部边界的固定尺度。
    # 生产 Core PCM 另有 factor 2，故此模型的 64 宽控制与 Core PCM * 2^-17 比较。
    x = unfold(dct_iv(np.asarray(lines[:n], dtype=float)/(original_length*65536)))
    m = len(window)
    left = np.concatenate((np.zeros((n-m)//2), window, np.ones((n-m)//2)))
    right = np.concatenate((np.ones((previous-m)//2), window[::-1], np.zeros((previous-m)//2)))
    h = np.asarray(history, dtype=float).copy()
    skip, old = (frame-n)//2, (frame-previous)//2
    h[old:old+previous] *= right
    h[skip:skip+n] += x[:n]*left
    pcm = h[:n].copy()
    h[:skip] = h[n:n+skip].copy()
    h[skip:skip+n] = x[n:]
    return pcm, h, n


def validate_frames(rows):
    if not isinstance(rows, list) or not 1 <= len(rows) <= 100000:
        raise ValueError("帧列表错误")
    known, ids, tracks = {}, set(), {}
    for r in rows:
        name = identifier(r['id'])
        dataset = identifier(r['dataset'])
        au = integer(r['au'], 1, 10000)
        owner = integer(r['ajoc_input'], -1, 8)
        track = integer(r['track'], 0, 9)
        # 当前协议固定九路 fullband + LFE。P2 P14 把末三路移到 A-JOC 输入开头。
        expected_owner = -1 if track == 0 else ((track-1+3) % 9)
        if owner != expected_owner:
            raise ValueError("传输顺序不符合已声明的 A-JOC 置换")
        integer(r['element'], 0, 16)
        integer(r['channel'], 0, 2)
        lengths = r['lengths']
        if not isinstance(lengths, list) or not lengths or len(lengths) > 16:
            raise ValueError("块列表错误")
        for n in lengths:
            integer(n, 128, 2048)
            if n not in (128, 256, 512, 1024, 2048):
                raise ValueError("未覆盖的变换族")
        key = dataset, au, owner
        if sum(lengths) != 2048 or key in known or name in ids:
            raise ValueError("遗漏样本或重复帧")
        group = tracks.setdefault((dataset, au), set())
        if track in group:
            raise ValueError("重复传输声道")
        group.add(track)
        known[key] = r
        ids.add(name)
    for dataset, au in tracks:
        if tracks[dataset, au] != set(range(10)) or any((dataset, au, j) not in known for j in range(-1, 9)):
            raise ValueError("声道所有权不完整")
    return known


def validate_blocks(rows, frames):
    groups, ids = {}, set()
    for r in rows:
        name = identifier(r['id'])
        scope = identifier(r['scope'])
        if type(r['inactive_workspace_zeroed']) is not bool:
            raise ValueError("未声明原生重放是否清零无效工作区")
        key = identifier(r['dataset']), integer(r['au'], 1, 10000), integer(r['ajoc_input'], -1, 8)
        if key not in frames or name in ids:
            raise ValueError("未知所有者或重复块")
        ids.add(name)
        width = 8 if key[2] == -1 else 48
        if integer(r['width'], 8, 48) != width:
            raise ValueError("分析宽度不匹配")
        n = integer(r['n'], 16, 1536)
        previous = integer(r['previous'], 16, 1536)
        offset = integer(r['offset'], 0, 2047)
        prefix = max(previous-n, 0)//2
        integer(r['prefix'], 0, 2048)
        integer(r['span'], 16, 2048)
        if n*64 % width or r['prefix'] != prefix or r['span'] != prefix+max(n, previous//2):
            raise ValueError("原始重叠范围错误")
        groups.setdefault((scope, *key), []).append((offset, n*64//width, previous))
    for (_, *key), blocks in groups.items():
        offset = 0
        expected = []
        for length in frames[tuple(key)]['lengths']:
            expected.append((offset, length))
            offset += length
        if [(offset, length) for offset, length, _ in blocks] != expected:
            raise ValueError("遗漏、乱序或错位的变换块")
        width = 8 if key[-1] == -1 else 48
        dataset, au, owner = key
        if (dataset, au-1, owner) not in frames:
            raise ValueError("缺失上一帧的块长历史")
        if blocks[0][2] != frames[dataset, au-1, owner]['lengths'][-1]*width//64:
            raise ValueError("跨帧上一块长度不连续")
        for before, after in zip(blocks, blocks[1:]):
            if after[2] != before[1]*width//64:
                raise ValueError("帧内上一块长度不连续")
    return groups


def probe_input(n, pattern):
    x = np.zeros(n, dtype='<f4')
    if pattern == 1:
        x[0] = 1
    elif pattern == 2:
        x[-1] = 1
    elif pattern in (3, 4):
        state = 847012 if pattern == 3 else 419284
        for j in range(n):
            state = (1664525*state+1013904223) & 0xffffffff
            x[j] = ((state >> 8)/2**24-.5)*.25
    elif pattern != 0:
        raise ValueError("未知探针")
    return x


def compare(manifest, data):
    m = json.loads(manifest.read_text())
    if m.get('schema') != 'macinac4.normalized-asf-imdct-checkpoints' or type(m.get('version')) is not int or m['version'] != 1:
        raise ValueError("检查点格式错误")
    frames = validate_frames(m['frames'])
    groups = validate_blocks(m['blocks'], frames)
    local_windows = {n: floats(data/f'window{n}.f32', (n,)) for n in (128, 256, 512, 1024, 2048)}
    frame_data = {key: (floats(data/f"{r['id']}.lines.f32", (2048,)),
                        floats(data/f"{r['id']}.pcm.f32", (2048,))) for key, r in frames.items()}
    blocks = []
    for r in m['blocks']:
        name, n, width = r['id'], r['n'], r['width']
        x = floats(data/f'{name}.input.f32', (n,))
        post = floats(data/f'{name}.post.f32', (n,))
        for suffix, count in [('input', n), ('pre', n), ('fft', n), ('post', n), ('result', n), ('before', r['span']), ('after', r['span'])]:
            value = floats(data/f'{name}.{suffix}.f32', (count,))
            repeat = floats(data/f'{name}.{suffix}.repeat.f32', (count,))
            if not identical(value, repeat):
                raise ValueError(f"独立进程检查点不重复：{name}/{suffix}")
            if suffix == 'before' and np.any(value[max(n, r['previous'])//2:] != 0):
                raise ValueError("未清零无效的历史工作区后缀")
            if suffix in ('result', 'after') and not identical(value, floats(data/f'{name}.{suffix}.replay.f32', (count,))):
                raise ValueError("原生重放不逐位一致")
        original = n*64//width
        lines = frame_data[r['dataset'], r['au'], r['ajoc_input']][0]
        start = r['offset']
        if np.any(lines[start+n:start+original] != 0):
            raise ValueError("缩短变换会丢弃非零谱线")
        pre = floats(data/f'{name}.pre.f32', (n//2, 2))
        fft = floats(data/f'{name}.fft.f32', (n//2, 2))
        blocks.append(dict(id=name, scope=r['scope'], width=width, n=n, previous=r['previous'],
                           native_replay_inactive_workspace_zeroed=r['inactive_workspace_zeroed'],
                           nonzero=bool(np.any(x)), components=n,
                           spectrum=score(x, lines[start:start+n].astype(float)/(original*65536)),
                           dct=score(post, dct_iv(x)),
                           pre_stage=score(pre, components(paired_pre_rotation(x))),
                           fft_stage=score(fft, components(np.fft.fft(complex_values(pre)))),
                           post_stage=score(post, paired_post_rotation(complex_values(fft)))))
    probes = []
    probe_keys = set()
    for r in m['probes']:
        name = identifier(r['id'])
        n = integer(r['n'], 16, 1536)
        pattern = integer(r['pattern'], 0, 4)
        key = identifier(r['scope']), integer(r['width'], 8, 48), n, pattern
        if key in probe_keys:
            raise ValueError("重复探针")
        probe_keys.add(key)
        x = floats(data/f'{name}.input.f32', (n,))
        y = floats(data/f'{name}.post.f32', (n,))
        if (not identical(x, probe_input(n, pattern))
                or not identical(x, floats(data/f'{name}.input.repeat.f32', (n,)))
                or not identical(y, floats(data/f'{name}.post.repeat.f32', (n,)))):
            raise ValueError("探针配方或重复检查失败")
        probes.append(dict(id=name, width=r['width'], n=n, pattern=pattern, dct=score(y, dct_iv(x))))
    expected_probes = {(r['scope'], r['width'], r['n'], p) for r in m['blocks'] for p in range(5)}
    if probe_keys != expected_probes:
        raise ValueError("探针覆盖不完整")
    scopes, used_groups, pcm_results, controls, windows = set(), set(), [], [], []
    seen_windows = set()
    for scope in m['scopes']:
        name = identifier(scope['id'])
        dataset = identifier(scope['dataset'])
        last = integer(scope['last_au'], 3, 10000)
        integer(scope['first_au'], last-1, last-1)
        if name in scopes or scope['first_au'] != last-1 or scope['delay_samples_48k'] != 352:
            raise ValueError("错误区间或延迟")
        scopes.add(name)
        for owner in range(-1, 9):
            width = 8 if owner == -1 else 48
            for au in (last-1, last):
                key = name, dataset, au, owner
                if key not in groups:
                    raise ValueError("缺失变换检查点")
                used_groups.add(key)
            native_windows = {}
            needed = set()
            previous_original = 2048
            for au in range(1, last+1):
                if (dataset, au, owner) not in frames:
                    raise ValueError("缺失预热历史帧")
                for original in frames[dataset, au, owner]['lengths']:
                    needed.add(min(original, previous_original))
                    previous_original = original
            for original in sorted(needed):
                size = original*width//64
                w = floats(data/f'{name}w{width}n{size}.f32', (size,))
                native_windows[size] = w
                window_key = name, width, original
                if window_key not in seen_windows:
                    seen_windows.add(window_key)
                    windows.append(dict(scope=name, width=width, original=original,
                                        metrics=score(w, centered_window(local_windows[original], size))))
            native_pcm = floats(data/f'{name}o{owner+1}.input.f32', (64*width,))
            if not identical(native_pcm, floats(data/f'{name}o{owner+1}.input.repeat.f32', (64*width,))):
                raise ValueError("QMF 输入不重复")
            for mode, actual_width in [('production64', 64), ('resized', width), ('captured_window', width)]:
                frame = actual_width*32
                h, previous, pcm, core = np.zeros(frame), frame, [], []
                for au in range(1, last+1):
                    key = dataset, au, owner
                    if key not in frames:
                        raise ValueError("缺失预热历史帧")
                    lines, original_pcm = frame_data[key]
                    core.extend(original_pcm)
                    offset = 0
                    for original in frames[key]['lengths']:
                        n = original*actual_width//64
                        taper = min(n, previous)
                        if mode == 'captured_window':
                            w = native_windows[taper]
                        else:
                            w = centered_window(local_windows[taper*64//actual_width], taper)
                        out, h, previous = synthesize_block(lines[offset:offset+original], original, actual_width, h, previous, w)
                        pcm.extend(out)
                        offset += original
                if mode == 'production64':
                    controls.append(dict(scope=name, owner=owner, metrics=score(np.asarray(core)*2**-17, pcm)))
                else:
                    start = (last-2)*frame-352*width//64
                    pcm_results.append(dict(scope=name, owner=owner, width=width, mode=mode,
                                            metrics=score(native_pcm, pcm[start:start+2*frame])))
    if used_groups != set(groups):
        raise ValueError("检查点属于未声明的区间")
    return dict(schema='macinac4.asf-imdct-checkpoint-comparison', version=1,
                status='observation_only', gain_phase_delay_or_order_fitting=False,
                blocks=blocks, probes=probes, windows=windows, pcm=pcm_results, production_controls=controls)


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
        print(f'ASF/IMDCT 检查点校验失败：{error}', file=sys.stderr)
        return 2
    print(json.dumps({'blocks': len(result['blocks']), 'probes': len(result['probes']), 'pcm': len(result['pcm'])}))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
