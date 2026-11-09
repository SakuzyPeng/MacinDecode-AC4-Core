#!/usr/bin/env python3
"""六 tick 采样网格及音频／元数据交接检查；不搜索时间、相位或增益。"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np

try:
    from .check_oamd_consumer_timing import integer, sha, I64_MIN, I64_MAX, read_payloads, validate_native, validate_scene, canonical_events, validate_bridge, verify_variant
    from .check_ajoc_timing_stream import sync_frames, read_scene_export, check_reference_set
    from .check_qmf_phase_chain import synthesise_fir, analyse_fir, prototype, require_bound, WINDOW_SHA256, forward_bounds, synthetic_cases, synthetic_input
    from .check_asf_imdct_checkpoints import synthesize_block
    from .check_ajoc_timing import check_synthetic, scalar_schedule
except ImportError:
    from check_oamd_consumer_timing import integer, sha, I64_MIN, I64_MAX, read_payloads, validate_native, validate_scene, canonical_events, validate_bridge, verify_variant
    from check_ajoc_timing_stream import sync_frames, read_scene_export, check_reference_set
    from check_qmf_phase_chain import synthesise_fir, analyse_fir, prototype, require_bound, WINDOW_SHA256, forward_bounds, synthetic_cases, synthetic_input
    from check_asf_imdct_checkpoints import synthesize_block
    from check_ajoc_timing import check_synthetic, scalar_schedule


def ticks(sample):
    return integer(integer(sample) * 6)


def grid_tick(index, width):
    """已验证的居中网格 t=(i+1/2)*64/width-1/2，精确使用整数。"""
    integer(index)
    if type(width) is not int or width not in (8, 48, 64):
        raise ValueError('未验证的采样网格')
    numerator = 384 * index + 192
    if numerator % width:
        raise ValueError('采样点不能精确落在六 tick 网格')
    return integer(numerator // width - 3)


def translate(position, delta):
    return integer(integer(position) + integer(delta))


def exact_samples(value):
    integer(value)
    if value % 6:
        raise ValueError('分数采样位置不能取整为 PCM 下标')
    return value // 6


def analysis_support(slot, pcm_delay, lowband_slots=6):
    integer(slot)
    integer(pcm_delay, 0, 4096)
    integer(lowband_slots, 0, 32)
    end = 64 * (slot - lowband_slots) + 63 - pcm_delay
    return [ticks(end - 639), ticks(end)]


def synthesis_support(slot):
    integer(slot)
    return [ticks(64 * slot), ticks(64 * slot + 639)]


def imdct_positions(lengths, frame_length, first_centre):
    """P1 5.5.3 的块中心与等速 composition 读指针，独立于 Rust helper。"""
    integer(frame_length, 128, 2048)
    integer(first_centre)
    if not lengths:
        raise ValueError('遗漏块')
    previous, centre = None, first_centre
    rows = []
    for index, length in enumerate(lengths):
        integer(length, 128, frame_length)
        if length % 2:
            raise ValueError('块长必须为偶数')
        if previous is not None:
            centre = integer(centre + (previous + length) // 2)
        read_start = integer(centre - (frame_length + length) // 2)
        rows.append(dict(block=index, length=length, centre_tick=ticks(centre),
                         read_start_tick=ticks(read_start), read_end_tick=ticks(read_start + length),
                         new_history_start=(frame_length - length) // 2,
                         new_history_end=(frame_length + length) // 2))
        if index and rows[-2]['read_end_tick'] != rows[-1]['read_start_tick']:
            raise ValueError('composition 时间不连续')
        previous = length
    return rows


def validate_transfer(row):
    """相同 PCM 元素的缓冲视图，不允许静默前缀裁剪或少一个采样。"""
    frames = integer(row['samples'], 1, 4096)
    channels = integer(row['channels'], 1, 159)
    sample_bytes = integer(row['sample_bytes'], 1, 8)
    for name in ('returned_bytes', 'export_samples', 'export_sample_start', 'tick_start'):
        integer(row[name])
    if sample_bytes != 4:
        raise ValueError('只验证 Float32')
    if row['returned_bytes'] != frames * channels * sample_bytes:
        raise ValueError('返回尺寸错误')
    stride = integer(row['channel_stride_bytes'], frames * sample_bytes, I64_MAX)
    total = integer(row['allocation_bytes'], frames * sample_bytes, I64_MAX)
    if (channels - 1) * stride + frames * sample_bytes > total:
        raise ValueError('返回视图越界')
    start = integer(row['sample_start'])
    if row['export_sample_start'] != start or row['export_samples'] != frames or row['tick_start'] != ticks(start):
        raise ValueError('返回到序列化坐标改变')


def map_metadata(source_start, frame_samples, sample_offset, factor, api_offset):
    """三个具名坐标；只记录转换，不预设音频物理原点相同。"""
    integer(source_start)
    integer(frame_samples, 1, 4096)
    integer(sample_offset, 0, 31)
    integer(factor, 0, 63)
    integer(api_offset, -(2**31), 2**31 - 1)
    event = source_start + sample_offset + 32 * factor
    return dict(source_tick=ticks(event), scene_tick=ticks(event + frame_samples),
                oar_input_tick=ticks(event + frame_samples + api_offset))


def validate_common_events(rows, configurations):
    expected_ids = [c['id'] for c in configurations]
    groups = {}
    for row in rows:
        groups.setdefault(row['id'], []).append(row)
    if set(groups) != set(expected_ids) or len(expected_ids) != 16:
        raise ValueError('共同时间轴配置遗漏或重复')
    summaries = []
    for cfg in configurations:
        events = groups[cfg['id']]
        if len(events) != 399:
            raise ValueError('共同时间轴 AU 遗漏')
        for au, row in enumerate(events, 2):
            factor = cfg['factor'] if au == 100 else 0
            ramp = cfg['duration'] if au == 100 else 2048
            expected = map_metadata((au-2)*2048, 2048, 0, factor, 352)
            if (row['consumer_au'] != au or row['source_au'] != au-1 or row['objects'] != 21
                    or row['ramp_samples'] != ramp or row['sample_offset'] != 0
                    or row['block_factor'] != factor or row['api_offset'] != 352
                    or any(row[k] != v for k, v in expected.items())
                    or row['oar_preformat_tick'] != translate(expected['oar_input_tick'], -ticks(31))):
                raise ValueError('共同坐标漏用／重复延迟或绑定错误来源')
            start = cfg['changes'].get('ajoc_start_pos', 0) if au == 100 else 0
            slot = (au-1)*32+start
            if (row['reference_first_step_support'] != synthesis_support(slot)
                    or row['local_first_step_support'] != synthesis_support(slot+1)):
                raise ValueError('两种插值日程的时隙或支持区间错误')
        summaries.append(dict(id=cfg['id'], events=399, target=events[98],
                              coordinate_difference_after_formatter_ticks=ticks(321),
                              physical_synchronization_adjudicated=False))
    return summaries


def apply_edit(source_tick, edit_samples):
    return translate(source_tick, -ticks(integer(edit_samples, 0, I64_MAX)))


def format_block(prefix, samples, carry, limit, output_gain):
    """已锁定参考的 32 采样转换器：已转换前缀 + 待转换输入共保留 31 项。

    limit 和 output_gain 来自实际状态；乘 256 与限幅顺序来自消费者指令，
    不从 PCM 残差拟合。只验证本批 start/end gain 相等的路径。
    """
    carry = integer(carry, 0, 31)
    prefix, samples = np.asarray(prefix, dtype='<f4'), np.asarray(samples, dtype='<f4')
    if (prefix.ndim != 2 or prefix.shape[1] != 32 or samples.ndim != 2
            or samples.shape[0] != prefix.shape[0] or not samples.shape[1]
            or not np.isfinite(samples).all() or not np.isfinite(limit) or limit <= 0
            or not np.isfinite(output_gain) or output_gain < 0):
        raise ValueError('格式转换形状、状态或数值非法')
    leading = 31 - carry
    if not np.isfinite(prefix[:, :leading]).all() or not np.isfinite(prefix[:, 32-carry:]).all():
        raise ValueError('有效保留状态非有限')
    joined = np.concatenate((prefix[:, 32-carry:], samples), axis=1)
    processed = (joined.shape[1] // 32) * 32
    gain = np.float32(np.float32(output_gain) * np.float32(256))
    converted = (np.minimum(np.maximum(joined[:, :processed], -np.float32(limit)), np.float32(limit))*gain).astype('<f4')
    available = np.concatenate((prefix[:, :leading], converted), axis=1)
    count = samples.shape[1]
    if available.shape[1] < count:
        raise ValueError('转换器不能提供声明的输出长度')
    result = available[:, :count]
    remaining = joined.shape[1] - processed
    state = np.zeros_like(prefix)
    state[:, :31-remaining] = available[:, count:]
    state[:, 32-remaining:] = joined[:, processed:]
    return result, state, remaining


def exact_bytes(a, b, description):
    if np.asarray(a).shape != np.asarray(b).shape or np.asarray(a, dtype='<f4').tobytes() != np.asarray(b, dtype='<f4').tobytes():
        raise ValueError(description)


REFERENCE_PARAMETERS = dict(ajoc_core_decode=0, target_reference_level=0, setup_b_dap_enable=0,
                            drc_enable=0, limiter_enable=0, dap_param_ieq_profile=0,
                            dap_param_ieq_strength=0, presentation_selection_mode=0,
                            presentation_selection_b_select_by_presentation_index=1,
                            presentation_selection_index=1, cplx_sample_mem_size=4, buffer_format=7,
                            setup_output_cplx_level=12287, cplx_num_channels=21,
                            output_channel_config=12032, output_num_channels=21)


def validate_reference_gate(status, calls, params, offsets):
    count = len(offsets)
    expected = dict(backend='drp-runtime', runtime_profile='DRP 4.3.0.19350 arm64', layout='9.1.6',
                    success=True, error=None, frames=count, blocks=count, sample_rate=48000,
                    decoder_raw_bytes=count*2048*21*4, oar_raw_bytes=0)
    if status != expected or calls != [[offset, 0, 2048*21*4, 1] for offset in offsets]:
        raise ValueError('参考完整 AU、返回状态或输出尺寸错误')
    if params != [f'{key}={value} result=0' for key, value in REFERENCE_PARAMETERS.items()]:
        raise ValueError('参考参数缺失、重复、失败或与基准不同')


def validate_padded_pcm(pcm, active_channels):
    """按已验证的合成通道数检查物理 21 路 API 缓冲；不按信号能量选轨。"""
    integer(active_channels, 1, 21)
    pcm = np.asarray(pcm)
    if pcm.ndim != 2 or pcm.shape[1] != 21 or not len(pcm) or not np.isfinite(pcm).all():
        raise ValueError('参考物理 PCM 形状或有限性错误')
    if np.any(pcm[:, active_channels:] != 0):
        raise ValueError('未描述的物理填充通道含非零 PCM，不能丢弃')


def validate_synthesis_channels(row, au, active_channels):
    integer(active_channels, 1, 21)
    if active_channels != 21 and 'synthesis_channels' not in row:
        raise ValueError('部分物理通道必须有实际合成通道数凭证')
    expected = active_channels if au > 1 else 0
    if integer(row.get('synthesis_channels', expected), 0, 21) != expected:
        raise ValueError('实际合成通道数与声明不符')
    counts = row['counts']
    if (len(counts) != 21 or any(type(count) is not int for count in counts)
            or counts != ([32]*active_channels+[0]*(21-active_channels) if au > 1 else [0]*21)):
        raise ValueError('合成原语调用缺失或填充通道被误当作实际合成')


def check_native(case):
    root = Path(case['directory'])
    count = case['expected_frames']
    active = integer(case.get('active_channels', 21), 1, 21)
    runs = []
    for mode in ('r7', 'r8'):
        receipt = json.loads((root/mode/'receipt.json').read_text())
        valid = [a for a in receipt['attempts'] if a['valid']]
        if len(valid) != 1:
            raise ValueError('缺少唯一有效的完整参考捕获')
        accepted = valid[0]
        if accepted['returncode'] != 0 or accepted['calls'] != count or accepted['pcm_sha256'] != case['expected_pcm_sha256']:
            raise ValueError('参考返回或完整 PCM 摘要错误')
        directory = Path(accepted['directory'])
        if receipt['input_sha256'] != case['input_sha256'] or sha(case['input']) != case['input_sha256']:
            raise ValueError('参考码流来源错误')
        frames = sync_frames(Path(case['input']).read_bytes())
        if len(frames) != count:
            raise ValueError('参考完整输入 AU 数量错误')
        status = json.loads((directory/'capture.status.json').read_text())
        calls = [list(map(int, line.split(','))) for line in (directory/'capture.calls.csv').read_text().splitlines()]
        params = (directory/'capture.params.txt').read_text().splitlines()
        validate_reference_gate(status, calls, params, [f['offset'] for f in frames])
        if accepted['status'] != status:
            raise ValueError('参考回执与实际状态不符')
        rows = read_rows(directory/'terminal.jsonl')
        if len(rows) != count:
            raise ValueError('参考终端 AU 遗漏')
        raw_path = directory/'capture.raw'
        if sha(raw_path) != case['expected_pcm_sha256']:
            raise ValueError('参考捕获文件摘要错误')
        pcm = np.memmap(raw_path, '<f4', mode='r').reshape(count, 2048, 21)
        validate_padded_pcm(pcm.reshape(-1, 21), active)
        selected = []
        hashes = {}
        prior_au, prior_tail = None, None
        for au, row in enumerate(rows, 1):
            validate_transfer(row)
            validate_synthesis_channels(row, au, active)
            if (row['au'] != au or row['sample_start'] != (au-1)*2048 or row['errors'] != 0
                    or row['selected'] is not (au in case['selected_aus'])
                    or row['observed'] is not (au > 1)
                    or row['formatter_pending_samples'] != (31 if au > 1 else 0)
                    or row['formatter_tail_matches'] is not (au > 1)
                    or row['formatter_history_matches'] is not (au > 2)):
                raise ValueError('参考帧身份、完整历史或待输出尾部错误')
            if not row['selected']:
                continue
            d = directory/f'au{au:04}'
            raw = np.fromfile(d/'synthesis.f32', '<f4').reshape(21, 2048)
            api = np.fromfile(d/'api.f32', '<f4').reshape(21, 2048)
            prefix = np.fromfile(d/'format_prefix.f32', '<f4').reshape(8, 21, 32)
            contexts = np.fromfile(d/'format_context.bin', '<u4').reshape(9, 32)
            state = np.fromfile(d/'format_state.bin', '<f4').reshape(9, 128)
            state_words = state.view('<u4')
            # snapshot 0 在本帧 setter 之前，可保留启动帧的 10 路设置；
            # 本帧实际消费的是后一个快照所记录的设置，与下方 gain 同源。
            if (np.any(state_words[1:, 0] != 1) or np.any(state_words[1:, 3] != 1)
                    or np.any(state_words[1:, 4] != active) or np.any(state_words[1:, 14] != 32)):
                raise ValueError('参考格式转换分支或 32 采样粒度改变')
            offsets = np.fromfile(d/'format_offsets.u32', '<u4')
            destinations = np.fromfile(d/'format_return_delta.i64', '<i8')
            if (offsets.tolist() != list(range(0, 2048, 256)) or destinations.shape != (168,)
                    or np.any(destinations.reshape(8, 21)[:, :active])):
                raise ValueError('格式转换到返回缓冲发生裁剪或错位')
            if np.any(contexts[:, 15] != 31) or np.any(contexts[:, 18] != active) or np.any(contexts[:, 19] != 256):
                raise ValueError('原生保留数量或四时隙批次改变')
            observed_slots = np.fromfile(d/'descriptor_slots.u32', '<u4').reshape(21, 32)
            offsets_in_scratch = np.fromfile(d/'producer_offsets.i64', '<i8').reshape(21, 32)
            if (np.any(observed_slots[:active] != 4)
                    or np.any(offsets_in_scratch[:active] != np.tile(np.arange(4)*256, 8))
                    or np.any(observed_slots[active:]) or np.any(offsets_in_scratch[active:])):
                raise ValueError('原生暂存视图或时隙次序错误')
            exact_bytes(api, pcm[au-1].T, '返回缓冲与实际序列化 PCM 不一致')
            clipped = 0
            for group in range(8):
                limit, gain, start_gain = state[group+1, [1, 2, 29]]
                if gain.view('u4') != start_gain.view('u4') or not np.isfinite(state[group+1, [1, 2, 29]]).all():
                    raise ValueError('本批未覆盖变化中的格式增益')
                values = raw[:active, group*256:(group+1)*256]
                prediction, after, carry = format_block(prefix[group, :active], values, int(contexts[group, 15]), limit, gain)
                exact_bytes(prediction, api[:active, group*256:(group+1)*256], '原生格式转换／31 采样保留不符')
                if carry != int(contexts[group+1, 15]):
                    raise ValueError('格式保留游标错误')
                if group < 7:
                    exact_bytes(after[:, 1:], prefix[group+1, :active, 1:], '四时隙批次丢失尾部')
                clipped += int(np.count_nonzero(abs(np.concatenate((prefix[group, :active, 1:], values[:, :225]), axis=1)) > limit))
            if prior_au == au - 1:
                exact_bytes(prior_tail, prefix[0, :active, 1:], '跨 AU 格式历史不连续')
            prior_au, prior_tail = au, raw[:active, -31:].copy()
            selected.append(dict(au=au, pcm_samples=active*2048, pending_input_samples=31,
                                 format_delay_ticks=186, group_samples=256, clipped_values=clipped,
                                 clip_limit=float(state[-1, 1]), output_gain=float(state[-1, 2]),
                                 effective_gain=float(np.float32(state[-1, 2]*256)),
                                 format_state_source='next batch before setter; final batch after return'))
            for name in ('synthesis.f32', 'api.f32', 'qmf.f32', 'before.f32', 'after.f32', 'format_prefix.f32',
                         'producer_offsets.i64', 'descriptor_slots.u32', 'format_offsets.u32', 'format_return_delta.i64'):
                if name == 'format_return_delta.i64' and active != 21:
                    # 未合成的物理尾路没有目的地址观察；其地址槽不能用于重复性比较。
                    hashes[f'au{au:04}/{name}'] = hashlib.sha256(
                        destinations.reshape(8, 21)[:, :active].tobytes()).hexdigest()
                else:
                    hashes[f'au{au:04}/{name}'] = sha(d/name)
            # 内存快照只比较实际使用的标量；未使用位置和进程地址不冒充状态。
            hashes[f'au{au:04}/format_values'] = contexts[:, [14, 15, 18, 19, 22, 24]].tobytes().hex()+state[:, [1, 2, 29]].tobytes().hex()+state_words[:, [0, 3, 4, 14]].tobytes().hex()
        del pcm
        if [r['au'] for r in selected] != case['selected_aus']:
            raise ValueError('参考观察窗口遗漏')
        hashes['terminal.jsonl'] = sha(directory/'terminal.jsonl')
        hashes['oamd'] = sha(directory/'capture.oamd.bin')
        hashes['scene'] = sha(directory/'capture.scene.bin')
        for name in ('capture.status.json', 'capture.params.txt', 'capture.calls.csv'):
            hashes[name] = sha(directory/name)
        runs.append(dict(mode=mode, frames=count, pcm_sha256=accepted['pcm_sha256'],
                         adapter_sha256=receipt['adapter_sha256'], selected=selected, hashes=hashes))
    if runs[0]['adapter_sha256'] != runs[1]['adapter_sha256'] or runs[0]['hashes'] != runs[1]['hashes']:
        raise ValueError('独立进程记录不重复')
    return dict(id=case['id'], frames=count, independent_processes=2, pcm_sha256=runs[0]['pcm_sha256'],
                selected=runs[0]['selected'], repeated_files=len(runs[0]['hashes']),
                terminal_ledger_sha256=runs[0]['hashes']['terminal.jsonl'])


def read_rows(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines()]


def check_metadata_binding(root, cases, common, configurations):
    """把共同账本重新绑定到原始字段、实际 Scene、OAR 消费及新桥接捕获。"""
    root = Path(root)
    manifest = json.loads((root/'variants/manifest.json').read_text())
    if [c['configuration'] for c in manifest['cases']] != configurations:
        raise ValueError('元数据构造来源不完整')
    if sha(manifest['fields']) != manifest['fields_sha256'] or sha(manifest['seed']['input']) != manifest['seed']['input_sha256']:
        raise ValueError('原始字段或母流来源改变')
    source = Path(manifest['seed']['input']).read_bytes()
    fields = read_rows(manifest['fields'])
    results = []
    for previous, case in zip(manifest['cases'], cases):
        cfg = previous['configuration']
        if cfg['id'] != case['id']:
            raise ValueError('元数据错绑配置')
        proof = json.loads(Path(previous['proof']).read_text())
        verify_variant(source, Path(case['input']).read_bytes(), fields, proof)
        oar = root/'oar'/case['id']
        origin = json.loads((oar/'source.json').read_text())
        if sha(origin['pcm']) != origin['pcm_sha256'] or sha(origin['payload']) != origin['payload_sha256']:
            raise ValueError('复用 OAR 输入摘要变化')
        external = json.loads((oar/'external.json').read_text())
        payloads = read_payloads(origin['payload'], 400)
        scene_path = root/'scene'/case['id']/'scene.jsonl'
        scene = read_rows(scene_path)
        validate_scene(scene, cfg)
        trace = read_rows(oar/'r1/trace.jsonl')
        native = validate_native(trace, payloads, external)
        baseline = json.loads((oar/'baseline/receipt.json').read_text())
        for mode in ('baseline', 'confirm', 'r1', 'r2'):
            receipt = json.loads((oar/mode/'receipt.json').read_text())
            if (receipt['mode'] != mode or receipt['returncode'] != 0 or receipt['finite'] is not True
                    or receipt['pcm_sha256'] != baseline['pcm_sha256'] or receipt['bytes'] != 400*2048*16*4
                    or receipt['adapter_sha256'] != baseline['adapter_sha256']
                    or receipt['input_pcm_sha256'] != origin['pcm_sha256']
                    or receipt['input_payload_sha256'] != origin['payload_sha256']):
                raise ValueError('复用 OAR 参考门禁不符')
            if mode.startswith('r') and sha(oar/mode/'trace.jsonl') != receipt['trace_sha256']:
                raise ValueError('复用 OAR 消费记录改变')
        if (oar/'r1/trace.jsonl').read_bytes() != (oar/'r2/trace.jsonl').read_bytes():
            raise ValueError('复用 OAR 双进程记录不同')
        events = canonical_events(payloads, external)
        for mode in ('r7', 'r8'):
            receipt = json.loads((Path(case['directory'])/mode/'receipt.json').read_text())
            accepted = next(a for a in receipt['attempts'] if a['valid'])
            directory = Path(accepted['directory'])
            if (accepted['pcm_sha256'] != origin['pcm_sha256']
                    or sha(directory/'capture.oamd.bin') != origin['payload_sha256']):
                raise ValueError('新捕获不能复用已有 OAR 输入')
            validate_bridge(read_scene_export(directory/'capture.scene.bin'), events)
        selected = [r for r in common if r['id'] == cfg['id']]
        actual = [r for r in scene if r['kind'] == 'event' and r['object'] == 0]
        if len(selected) != len(actual):
            raise ValueError('共同记录未覆盖实际事件')
        for row, local, event in zip(selected, actual, events[1:]):
            if (row['consumer_au'] != local['au'] or row['source_au'] != local['source_au']
                    or row['scene_tick'] != ticks((local['au']-1)*2048+local['offset'])
                    or row['oar_input_tick'] != ticks(event['coordinate'])
                    or row['ramp_samples'] != event['ramp'] or row['objects'] != event['objects']):
                raise ValueError('共同账本与实际 Scene／OAR 来源不同')
        results.append(dict(id=cfg['id'], events=len(selected), native=native,
                            scene_sha256=sha(scene_path), trace_sha256=sha(oar/'r1/trace.jsonl'),
                            payload_sha256=origin['payload_sha256'], pcm_sha256=origin['pcm_sha256'],
                            native_consumer_reused=True))
    return results


def check_imdct(root):
    root = Path(root)
    lengths = [2048, 2048, 1024] + [128]*16 + [1024, 2048, 2048]
    expected = imdct_positions(lengths, 2048, 2048)
    rows = read_rows(root/'positions.jsonl')
    if len(rows) != 22:
        raise ValueError('混合窗口序列遗漏')
    for i, (row, wanted) in enumerate(zip(rows, expected)):
        if row != dict(wanted, previous=lengths[i-1] if i else 2048):
            raise ValueError('IMDCT 中心、读指针或历史写入区间错误')
    signal = np.fromfile(root/'signal.f64', '<f8')
    state = 0x8f4d3c2b1a097865
    owned = []
    for _ in range(len(signal)):
        state = (state*6364136223846793005 + 1442695040888963407) % 2**64
        owned.append(((state >> 40)-8388608)/8388608)
    if not np.array_equal(signal, owned):
        raise ValueError('独立时间标签的源波形改变')
    pcm = np.fromfile(root/'pcm.f32', '<f4')
    if pcm.shape != (sum(lengths),) or not np.isfinite(pcm).all():
        raise ValueError('IMDCT 输出形状错误')
    history, previous, cursor = np.zeros(2048), 2048, 0
    blocks = []
    for i, length in enumerate(lengths):
        lines = np.fromfile(root/f'block{i:02}.lines.f32', '<f4')
        window = np.fromfile(root/f'block{i:02}.window.f32', '<f4')
        predicted, history, previous = synthesize_block(lines, length, 64, history, previous, window)
        error = float(np.max(abs(predicted*131072 - pcm[cursor:cursor+length])))
        blocks.append(dict(maximum_absolute_residual=error,
                           window_sha256=sha(root/f'block{i:02}.window.f32'), **expected[i]))
        cursor += length
    reconstruction_error = float(np.max(abs(pcm[4096:-2048]-signal[8192:4096+len(pcm)-2048])))
    if reconstruction_error > 3e-5:
        raise ValueError('既有混合窗口重建回归失败')
    return dict(blocks=blocks, samples=len(pcm), exact_positions=True, reconstruction_error=reconstruction_error,
                existing_regression_budget=3e-5, new_numerical_conformance_claim=False,
                source_sha256=sha(root/'signal.f64'), pcm_sha256=sha(root/'pcm.f32'))


def check_owned_qmf(cases_path, inputs, outputs, imdct):
    cases = json.loads(Path(cases_path).read_text())
    base = [c for c in synthetic_cases() if c['kind'] == 'impulse']
    if cases[:512] != base or [c['id'] for c in cases[512:]] != ['imdctd0', 'imdctd352']:
        raise ValueError('块内相位或组合链遗漏')
    inputs, outputs = Path(inputs), Path(outputs)
    if sha(outputs/'qwin.f32') != WINDOW_SHA256:
        raise ValueError('QMF 窗身份错误')
    window = np.fromfile(outputs/'qwin.f32', '<f4').astype(float)
    source = np.fromfile(Path(imdct)/'pcm.f32', '<f4')
    results = []
    for c in cases:
        name, slots = c['id'], c['slots']
        x = np.fromfile(inputs/(name+'.input.f32'), '<f4')
        if c['kind'] == 'impulse':
            exact_bytes(x, synthetic_input(c), '脉冲位置改变')
        else:
            wanted = np.zeros(slots*64, '<f4')
            start = c['pcm_delay']
            wanted[start:start+len(source)] = source
            exact_bytes(x, wanted, 'IMDCT 到 QMF 输入交接改变')
        q = np.fromfile(outputs/(name+'.analysis.f32'), '<f4').reshape(slots, 64, 2)
        z = np.fromfile(outputs/(name+'.mapped.f32'), '<f4').reshape(q.shape)
        mapped = np.zeros_like(q)
        delay = c['delay']
        for a, b in c['routes']:
            mapped[delay:, b] = q[:slots-delay, a] if delay else q[:, a]
        exact_bytes(mapped, z, '时隙延迟或路由改变')
        cq, cz = q[..., 0]+1j*q[..., 1], z[..., 0]+1j*z[..., 1]
        a_bound, s_bound, _ = forward_bounds(x, cz, window, c['routes'])
        ae = require_bound(analyse_fir(x, window), cq, a_bound, '带完整尾部的分析')
        y = np.fromfile(outputs/(name+'.pcm.f32'), '<f4')
        se = require_bound(synthesise_fir(cz, window), y, s_bound, '带完整尾部的合成')
        if np.any(y[-64:]):
            raise ValueError('有限滤波尾部未排尽')
        results.append(dict(id=name, samples=len(x), analysis_error=ae, analysis_bound=a_bound,
                            synthesis_error=se, synthesis_bound=s_bound, artificial_tail_slots=slots-c['checkpoint']))
    return results


def check_container(rows):
    if not rows:
        raise ValueError('缺少容器记录')
    count = integer(rows[0].get('metadata_events'), 1, 100000)
    if rows[0] != dict(kind='timeline', aus=143, edit_samples=2048, raw_samples=292864,
                      presentation_samples=288000, payloads_equal=True, metadata_events=count):
        raise ValueError('已有 MP4 配对或 edit 事实改变')
    tracks = [r for r in rows if r['kind'] == 'track']
    events = [r for r in rows if r['kind'] == 'event']
    if len(tracks) != 21 or len(rows) != 1+len(tracks)+len(events) or len(events) != count:
        raise ValueError('容器观察不完整')
    for index, row in enumerate(tracks):
        if row != dict(kind='track', channel=index, raw_first=2048, presentation_first=0, samples=288000, bit_exact=True):
            raise ValueError('PCM edit 漏用、重复或裁剪错位')
    if len({r['order'] for r in events}) != len(events):
        raise ValueError('元数据事件重复')
    for row in events:
        if ticks(row['presentation_sample']) != apply_edit(ticks(row['raw_sample']), 2048) or row['state_identity'] is not True:
            raise ValueError('元数据 edit 漏用、重复或身份改变')
    return dict(aus=143, tracks=21, events=len(events), edit_samples=2048,
                raw_samples=292864, presented_samples=288000, excluded_suffix=2816, exact_payloads_and_pcm=True)


def check_time_candidates(root):
    root = Path(root)
    verified = check_synthetic(root, frames=7)
    window = np.fromfile(root/'window.f32', '<f4').astype(float)
    rows = []
    for result in verified:
        d = root/result['id']
        case = json.loads((d/'case.json').read_text())
        slots = case['slots']
        schedule = []
        for frame in range(7):
            starts, ramps, codes = {0: ([0], [1], [60]), 1: ([case['start']], [case['ramp']], [70]),
                                    3: ([0, slots-1], [64, 2], [40, 55])}.get(frame, ([], [], []))
            schedule.append(dict(slots=slots, starts=starts, ramps=ramps, targets=[(c-50)*205/2048 for c in codes]))
        early = scalar_schedule(schedule, immediate=True)
        x = np.concatenate([np.fromfile(d/f'au{i:04}.x.f32', '<f4').reshape(slots, 64, 2) for i in range(1, 8)])
        late_z = np.concatenate([np.fromfile(d/f'au{i:04}.z.f32', '<f4').reshape(slots, 64, 2) for i in range(1, 8)])
        early_z = (x.astype(float)*early[:, None, None].astype(float)).astype('<f4')
        delta = early_z.astype(float)-late_z.astype(float)
        changed = np.flatnonzero(np.any(delta != 0, axis=(1, 2)))
        pcm_delta = 2*synthesise_fir(delta[..., 0]+1j*delta[..., 1], window)
        support = None
        if len(changed):
            support = [synthesis_support(int(changed[0]))[0], synthesis_support(int(changed[-1]))[1]]
            first, last = support[0]//6, support[1]//6
            if np.any(pcm_delta[:first]) or np.any(pcm_delta[last+1:]):
                raise ValueError('日程差异越出独立 FIR 支持区间')
        elif np.any(pcm_delta):
            raise ValueError('无非零输入差异却改变 PCM')
        rows.append(dict(result, nonzero_signal_changed_slots=len(changed),
                         pcm_difference_support_ticks=support, pcm_delta_maximum=float(np.max(abs(pcm_delta))),
                         arithmetic_policy_held_fixed=True, selects_production_schedule=False))
    return rows


def validate_frontend_grid(rows):
    if len(rows) != 82 or len({r['id'] for r in rows}) != 82:
        raise ValueError('既有原生变换块遗漏或重复')
    for row in rows:
        width = 8 if row['channel'] == 0 else 48
        if row['width'] != width or row['reference_step_ticks'] != 384//width:
            raise ValueError('8／48 带的采样宽度换算错误')
        if (row['reference_grid_start_tick'] != translate(row['local_start_tick'], grid_tick(0, width))
                or row['pcm_delay_ticks'] != ticks(352)
                or row['pcm_delay_internal_samples'] != 352*width//64):
            raise ValueError('居中采样原点或前端延迟错误')
    return dict(blocks=82, widths=[8, 48], exact_grid_offsets_ticks=[21, 1], pcm_delay_ticks=2112)


def check_local(case):
    root = Path(case['directory'])
    if sha(case['input']) != case['input_sha256']:
        raise ValueError('本地原始流身份变化')
    frames, selected = case['expected_frames'], case['selected_aus']
    rows = read_rows(root / 'terminal.jsonl')
    if len(rows) != frames * 21:
        raise ValueError('终端帧／通道遗漏')
    window_path = root / 'window.f32'
    if sha(window_path) != WINDOW_SHA256:
        raise ValueError('原型窗身份不符')
    window = np.fromfile(window_path, '<f4').astype(float)
    pcm_path = root / 'objects.f32'
    if pcm_path.stat().st_size != frames * 2048 * 21 * 4:
        raise ValueError('完整对象 PCM 尺寸不符')
    exported = np.memmap(pcm_path, dtype='<f4', mode='r').reshape(frames, 2048, 21)
    summaries = []
    objects = None
    for index, row in enumerate(rows):
        au, channel = divmod(index, 21)
        au += 1
        if row['au'] != au or row['channel'] != channel or row['samples'] != 2048 or row['slots'] != 32:
            raise ValueError('终端身份或形状错误')
        if row['source_au'] != (au - 1 if au > 1 else None):
            raise ValueError('终端控制来源错误')
        if row['sample_start'] != (au - 1) * 2048 or row['tick_start'] != ticks(row['sample_start']):
            raise ValueError('终端整数时钟错误')
        if row['export_start_sample'] != row['sample_start'] or row['producer_return_identity'] is not True:
            raise ValueError('返回／序列化错位')
        if row['history_slots'] != (10 if au > 1 else 0) or row['selected'] is not (au in selected):
            raise ValueError('合成历史或窗口遗漏')
        if objects is None:
            objects = [r['object'] for r in rows[:21]]
            if sorted(objects) != list(range(-1, 20)):
                raise ValueError('对象或 LFE 身份不完整')
        if row['object'] != objects[channel]:
            raise ValueError('输出路由变化')
        if row['selected']:
            qpath = root / f'au{au:04}.terminal{channel}.qmf.f32'
            ppath = root / f'au{au:04}.terminal{channel}.pcm.f32'
            q = np.fromfile(qpath, '<f4').reshape(42, 128).astype(float)
            q = q[:, 0::2] + 1j*q[:, 1::2]
            actual = np.fromfile(ppath, '<f4')
            if actual.tobytes() != exported[au - 1, :, channel].tobytes():
                raise ValueError('实际合成输出与序列化字节不符')
            predicted = 2 * synthesise_fir(q, window)[640:]
            bound = 8*np.finfo(np.float32).eps*np.max(np.sum(abs(q), axis=1))*np.max(abs(prototype(window)).reshape(10, 64).sum(axis=0))/64
            error = require_bound(predicted, actual, float(bound), '终端独立 FIR')
            summaries.append(dict(au=au, channel=channel, object=row['object'], error=error,
                                  bound=float(bound), qmf_sha256=sha(qpath), pcm_sha256=sha(ppath),
                                  first_slot_support_tick=synthesis_support((au-1)*32)))
    del exported
    block_rows = read_rows(root/'imdct.jsonl')
    channel_count = read_rows(root/'frames.jsonl')[0]['channels']
    groups = {}
    for row in block_rows:
        groups.setdefault((row['au'], row['channel']), []).append(row)
    if set(groups) != {(au, ch) for au in range(1, frames+1) for ch in range(channel_count)}:
        raise ValueError('实际 IMDCT 帧或物理声道遗漏')
    previous = [2048]*channel_count
    centres = [None]*channel_count
    for au in range(1, frames+1):
        for channel in range(channel_count):
            cursor = 0
            for index, row in enumerate(groups[au, channel]):
                length = integer(row['length'], 128, 2048)
                if (row['block'] != index or row['frame_samples'] != 2048 or row['offset'] != cursor
                        or row['previous'] != previous[channel] or row['output_identity'] is not True
                        or row['read_tick'] != ticks((au-1)*2048+cursor)):
                    raise ValueError('实际 IMDCT 读写、前态或交接错误')
                centre = translate(row['read_tick'], 3*(2048+length))
                if centres[channel] is not None and centre-centres[channel] != 3*(previous[channel]+length):
                    raise ValueError('实际窗口中心不连续')
                previous[channel], centres[channel] = length, centre
                cursor += length
            if cursor != 2048:
                raise ValueError('实际 IMDCT 块未铺满帧')
    return dict(id=case['id'], frames=frames, terminal_channels=len(rows), selected_channels=len(summaries),
                imdct_blocks=len(block_rows), imdct_ledger_sha256=sha(root/'imdct.jsonl'),
                pcm_sha256=sha(pcm_path), ledger_sha256=sha(root/'terminal.jsonl'), windows=summaries)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text())
    if manifest.get('schema') != 'macinac4.audio-metadata-sync' or manifest.get('version') != 1:
        raise ValueError('未知共同时间轴协议')
    cases = manifest['local']
    if [c['id'] for c in cases] != ['tone', 'music', 'bed256']:
        raise ValueError('原始三份流遗漏或重复')
    if len(manifest['native']) != 19 or {c['id'] for c in manifest['native'][:3]} != {'tone', 'music', 'bed256'}:
        raise ValueError('参考原流或受控流覆盖不完整')
    try:
        from .check_oamd_consumer_timing import configurations
    except ImportError:
        from check_oamd_consumer_timing import configurations
    if [c['id'] for c in manifest['native'][3:]] != [c['id'] for c in configurations()]:
        raise ValueError('参考受控流身份错误')
    qmf = manifest['qmf']
    timing = manifest['native_timing']
    result = dict(schema='macinac4.audio-metadata-sync-comparison', version=1, ticks_per_48k_sample=6,
                  local=[check_local(c) for c in cases], native=[check_native(c) for c in manifest['native']],
                  imdct=check_imdct(manifest['imdct']),
                  qmf=check_owned_qmf(qmf['cases'], qmf['inputs'], qmf['outputs'], qmf['imdct']),
                  time_candidates=check_time_candidates(manifest['timing_tail']),
                  native_timing=check_reference_set(json.loads(Path(timing['manifest']).read_text()),
                                                    json.loads(Path(timing['records']).read_text())),
                  metadata=validate_common_events(read_rows(manifest['common_events']), configurations()),
                  metadata_sources=check_metadata_binding(manifest['metadata_sources'], manifest['native'][3:],
                                                          read_rows(manifest['common_events']), configurations()),
                  frontend_grid=validate_frontend_grid(read_rows(manifest['frontend_grid'])),
                  container=check_container(read_rows(manifest['container'])), fit_parameters=False)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n')
    print('three local streams, 19 double-process reference streams and owned/common clocks verified')


if __name__ == '__main__':
    main()
