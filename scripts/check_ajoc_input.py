#!/usr/bin/env python3
"""独立核对真实 A-JOC 输入的路由、控制来源、低带与 LFE 时间轴。"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np

try:
    from .check_aspx_basis_replay import ROTATION, complex_values, components, metrics
    from .check_aspx_noise_replay import floats, integer
    from .check_qmf_phase_chain import WINDOW_SHA256, analyse_fir, prototype, require_bound
except ImportError:
    from check_aspx_basis_replay import ROTATION, complex_values, components, metrics
    from check_aspx_noise_replay import floats, integer
    from check_qmf_phase_chain import WINDOW_SHA256, analyse_fir, prototype, require_bound


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def source_order(fullband, lfe):
    integer(fullband, 1, 16)
    if type(lfe) is not bool:
        raise ValueError('LFE 必须是布尔值')
    last = 2 + fullband % 2 if fullband > 3 else 0
    return [source + int(lfe) for source in
            list(range(fullband-last, fullband)) + list(range(fullband-last))]


def validate_frames(rows, selected):
    if not rows or not selected or len(set(selected)) != len(selected):
        raise ValueError('缺少帧、空窗口或重复窗口')
    first = rows[0]
    n = integer(first['samples'], 1536, 2048)
    if n not in (1536, 1920, 2048):
        raise ValueError('未支持的帧长')
    delay = {1536: 96, 1920: 288, 2048: 352}[n]
    order = source_order(first['fullband'], first['lfe'])
    channels = first['fullband'] + int(first['lfe'])
    actual_selected = []
    for au, row in enumerate(rows, 1):
        if row['au'] != au or row['source_au'] != (au-1 if au > 1 else None):
            raise ValueError('控制来源不是正确的表 188 到期 AU')
        if any(row[k] != first[k] for k in ('samples', 'channels', 'fullband', 'lfe', 'substream')):
            raise ValueError('本次连续调查的拓扑改变')
        if row['channels'] != channels or row['pcm_delay'] != delay or row['control_delay'] != 1:
            raise ValueError('PCM/控制延迟或通道数不符合声明的长帧布局')
        if row['source_order'] != order or row['reconstructed'] is not (au > 1):
            raise ValueError('输入路由或预热边界不正确')
        if au > 1:
            if not isinstance(row['crossover'], list) or len(row['crossover']) != channels:
                raise ValueError('逐源交叉子带不完整')
            for source, band in enumerate(row['crossover']):
                integer(band, 4, 64)
                if (source == 0 and row['lfe']) != (band == 64):
                    raise ValueError('LFE 与完整 A-SPX 的频带角色不符')
        elif row['crossover'] is not None:
            raise ValueError('预热不应冒充已到期控制')
        if type(row['selected']) is not bool or type(row['wet']) is not bool:
            raise ValueError('窗口和 wet 状态类型错误')
        if row['selected']:
            actual_selected.append(au)
    if actual_selected != sorted(selected) or any(au < 2 for au in selected):
        raise ValueError('选择窗口遗漏、额外或尚无到期控制')
    return n, channels, delay, order


def verify_route(producers, consumers, lfe_values, order, lfe):
    if len(consumers) != len(order) or len(producers) != len(order)+int(lfe):
        raise ValueError('生产者或消费者数量错误')
    for target, source in enumerate(order):
        if producers[source].shape != consumers[target].shape or producers[source].tobytes() != consumers[target].tobytes():
            raise ValueError('路由前和 A-JOC 消费入口不逐位一致')
    if lfe:
        if lfe_values is None or producers[0].tobytes() != lfe_values.tobytes():
            raise ValueError('LFE 分流错误')
    elif lfe_values is not None:
        raise ValueError('凭空增加 LFE')


def expected_low(core, au, pcm_delay, window):
    """绝对 PCM 时间轴 + 六时隙输出延迟；16 个前置时隙覆盖完整 FIR 支持。"""
    frames, channels, n = core.shape
    integer(au, 2, frames)
    begin = (au-1)*n//64
    prefix = max(0, begin-16)
    start, end = prefix*64-pcm_delay, (begin+n//64)*64-pcm_delay
    results, bounds = [], []
    for source in range(channels):
        stream = core[:, source, :].reshape(-1)
        x = np.zeros(end-start)
        left = max(0, start)
        # ADR-0006 的分析接口固定除二；不从参考 PCM 倒推尺度。
        x[left-start:] = stream[left:end].astype(float)/2
        q = analyse_fir(x, window)
        first = begin-prefix-6
        if first < 0:
            raise ValueError('FIR 预热不足')
        results.append(q[first:first+n//64])
        bounds.append(float(4*np.finfo(np.float32).eps*np.max(abs(x))*abs(prototype(window)).sum()))
    return results, bounds


def check_local(directory, selected):
    rows = [json.loads(line) for line in (directory/'frames.jsonl').read_text().splitlines()]
    n, channels, delay, order = validate_frames(rows, selected)
    if digest(directory/'window.f32') != WINDOW_SHA256:
        raise ValueError('原型窗不是已冻结的独立 FIR 输入')
    window = floats(directory/'window.f32', (640,))
    core = floats(directory/'core.f32', (len(rows), channels, n))
    results = []
    for au in selected:
        prefix = f'au{au:04}'
        row = rows[au-1]
        producers = [floats(directory/f'{prefix}.source{s}.f32', (n//64, 64, 2)) for s in range(channels)]
        consumers = [floats(directory/f'{prefix}.input{s}.f32', (n//64, 64, 2)) for s in range(len(order))]
        lfe = floats(directory/f'{prefix}.lfe.f32', (n//64, 64, 2)) if row['lfe'] else None
        verify_route(producers, consumers, lfe, order, row['lfe'])
        for kind, applied in [('aspx', 'applied'), ('ajoc', 'consumed')]:
            expected = (directory/f'{prefix}.raw_{kind}.txt').read_bytes()
            if not expected or expected != (directory/f'{prefix}.{applied}_{kind}.txt').read_bytes():
                raise ValueError('实际使用的完整控制不是同源 raw AU 的内容')
        if au-1 in selected:
            if (directory/f'au{au-1:04}.state_after.txt').read_bytes() != (directory/f'{prefix}.state_before.txt').read_bytes():
                raise ValueError('相邻真实 AU 的完整 A-SPX 历史没有交接')
        model, bounds = expected_low(core, au, delay, window)
        errors = []
        for source, band in enumerate(row['crossover']):
            actual = complex_values(producers[source])[:, :band]
            error = require_bound(model[source][:, :band], actual, bounds[source], '低带/LFE 绝对时间轴')
            errors.append(dict(source=source, bands=band, max_complex_error=error, forward_bound=bounds[source]))
        results.append(dict(au=au, source_au=au-1, source_order=order, route_bit_exact=True,
                            controls_bit_exact=True, low_band_and_lfe=errors))
    return dict(frames=len(rows), channels=channels, samples_per_frame=n,
                reconstructed_frames=sum(r['reconstructed'] for r in rows), wet_frames=sum(r['wet'] for r in rows),
                selected=results)


def verify_reference_binding(source_tokens, entry_tokens, produced, entry, repeated):
    # 本协议限定已测的 LFE + 9 fullband 前端布局；不外推固定 21 路对象排列。
    if (np.shape(source_tokens) != (9, 32) or np.shape(entry_tokens) != (10, 32)
            or produced.shape != (9, 32, 64, 2) or entry.shape != (10, 32, 64, 2)
            or repeated.shape != entry.shape):
        raise ValueError('参考边界形状不符')
    all_tokens = [v for row in entry_tokens for v in row]
    if any(type(v) is not int or v < 0 for v in all_tokens) or len(set(all_tokens)) != 320:
        raise ValueError('参考边界的源缓冲区身份重复或无效')
    if source_tokens != entry_tokens[1:]:
        raise ValueError('A-SPX 生产者与参考完整阶段入口的缓冲区身份不符')
    if not all(np.isfinite(x).all() for x in (produced, entry, repeated)):
        raise ValueError('参考边界包含非有限值')
    if produced.tobytes() != entry[1:].tobytes() or entry.tobytes() != repeated.tobytes():
        raise ValueError('参考交接或独立进程重复不是逐位一致')


def check_reference(reference, cases):
    case = cases[reference['case_id']]
    if reference['input_sha256'] != case['input_sha256']:
        raise ValueError('参考与本地不是同一输入')
    au = integer(reference['au'], 2, case['expected_frames'])
    if au not in case['selected_aus']:
        raise ValueError('参考窗口没有相同 AU 的本地消费者记录')
    directory = Path(reference['directory'])
    expected_names = {'aspx.f32', 'entry.f32', 'repeat.f32', 'identity.json'}
    if set(reference['files']) != expected_names or {p.name for p in directory.iterdir()} != expected_names:
        raise ValueError('参考文件清单不完整')
    for name, value in reference['files'].items():
        if digest(directory/name) != value:
            raise ValueError('参考检查点摘要改变')
    identities = json.loads((directory/'identity.json').read_text())
    produced = floats(directory/'aspx.f32', (9, 32, 64, 2))
    entry = floats(directory/'entry.f32', (10, 32, 64, 2))
    repeated = floats(directory/'repeat.f32', entry.shape)
    verify_reference_binding(identities['aspx_tokens'], identities['entry_tokens'], produced, entry, repeated)
    local = Path(case['directory'])
    row = json.loads((local/'frames.jsonl').read_text().splitlines()[au-1])
    if row['fullband'] != 9 or not row['lfe'] or row['samples'] != 2048:
        raise ValueError('本地布局超出此参考协议')
    results = []
    for target in range(9):
        source = row['source_order'][target]
        crossover = row['crossover'][source]
        actual = floats(local/f'au{au:04}.input{target}.f32', (32, 64, 2))
        # 已冻结的低层接口换算；不用末端 PCM 的 31-sample lag 或 2^-16 尺度。
        common = components(complex_values(actual)*ROTATION/2**23)
        ref = entry[target+1]
        results.append(dict(input=target, transmitted_source=source, native_stage_slot=target+1,
                            low=metrics(ref[:, :crossover].copy(), common[:, :crossover].copy()),
                            high_below_18khz=metrics(ref[:, crossover:48].copy(), common[:, crossover:48].copy())))
    return dict(case_id=reference['case_id'], au=au, reference_handoff_bit_exact=True,
                independent_process_repeat_bit_exact=True, input_scale='R64 / 2^23',
                timeslot_shift=0, output_fitting=False, status='observation_only', inputs=results)


def compare(manifest):
    m = json.loads(manifest.read_text())
    if (m.get('schema'), m.get('version')) != ('macinac4.ajoc-input-manifest', 1) or m.get('fit_parameters') is not False:
        raise ValueError('未知协议或未固定表示约定')
    if not m['cases'] or len({c['id'] for c in m['cases']}) != len(m['cases']):
        raise ValueError('案例缺失或重复')
    results = []
    for case in m['cases']:
        source = Path(case['input'])
        if digest(source) != case['input_sha256']:
            raise ValueError('输入媒体已改变')
        directory = Path(case['directory'])
        files = {p.name for p in directory.iterdir() if p.is_file()}
        if files != set(case['files']) or len(files) != len(list(directory.iterdir())):
            raise ValueError('检查点文件遗漏、多余或出现子目录')
        for name, expected in case['files'].items():
            if Path(name).name != name or digest(directory/name) != expected:
                raise ValueError('检查点摘要或路径错误')
        result = check_local(directory, case['selected_aus'])
        result['id'] = case['id']
        result['input_sha256'] = case['input_sha256']
        if result['frames'] != case['expected_frames']:
            raise ValueError('完整流帧数不符')
        results.append(result)
    references = m.get('references', [])
    if len({(r['case_id'], r['au']) for r in references}) != len(references):
        raise ValueError('重复参考窗口')
    by_case = {c['id']: c for c in m['cases']}
    compared = [check_reference(reference, by_case) for reference in references]
    return dict(schema='macinac4.ajoc-input-comparison', version=1, status='local_boundary_pass',
                production_dsp_changed=False, fit_parameters=False, cases=results, reference=compared)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    try:
        result = compare(args.manifest)
        with args.output.open('x') as stream:
            json.dump(result, stream, ensure_ascii=False, indent=2, allow_nan=False)
            stream.write('\n')
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        print(f'A-JOC 输入检查失败：{error}')
        return 2
    print(json.dumps(dict(status=result['status'], cases=len(result['cases']))))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
