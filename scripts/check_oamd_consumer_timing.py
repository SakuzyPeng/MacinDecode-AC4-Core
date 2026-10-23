#!/usr/bin/env python3
"""OAMD 等长构造、严格捕获协议与独立采样时间核对；不依赖私有参考库。"""
from __future__ import annotations

import argparse
import hashlib
import json
import struct
from pathlib import Path

try:
    from .check_ajoc_timing_stream import crc16, read_bits, read_scene_export, sha, sync_frames, validate_fields
except ImportError:
    from check_ajoc_timing_stream import crc16, read_bits, read_scene_export, sha, sync_frames, validate_fields

# TS103190-2:v1.3.1:6.3.9.3.11，独立抄录表 95。
RAMP_TABLE = (32, 64, 128, 256, 320, 480, 1000, 1001, 1024, 1600, 1601, 1602, 1920, 2000, 2002, 2048)
I64_MIN, I64_MAX = -(1 << 63), (1 << 63) - 1


def integer(value, low=I64_MIN, high=I64_MAX):
    if type(value) is not int or not low <= value <= high:
        raise ValueError(f'整数范围错误: {value}')
    return value


def sample_offset(source, value):
    integer(value, 0, 31)
    if source == 'Implicit' and value == 0:
        return 0
    if source == 'Code' and type(value) is int and value in (8, 16, 24):
        return value
    if source == 'Explicit':
        return integer(value, 0, 31)
    raise ValueError('sample_offset 编码错误')


def ramp_samples(kind, value=0):
    integer(value, 0, 2047)
    if kind == 'Zero' and value == 0:
        return 0
    if kind in ('Fixed512', 'Fixed1536') and value == 0:
        return 512 if kind == 'Fixed512' else 1536
    if kind == 'Table':
        return RAMP_TABLE[integer(value, 0, 15)]
    if kind == 'Explicit':
        return integer(value, 0, 2047)
    raise ValueError('ramp 编码错误')


def event_sample(origin, offset, factor):
    """TS103190-2:v1.3.1:5.9.2；所有加法在有符号 64 位范围内核对。"""
    return integer(integer(origin) + integer(offset, 0, 31) + 32 * integer(factor, 0, 63))


def configurations():
    rows = [dict(id='music_99_ajoc_s00_r32', factor=0, duration=2048, changes={})]
    for start, ramp in ((1, 1), (15, 2), (31, 32)):
        for mode in ('ajoc', 'oamd', 'both'):
            changes = {}
            if mode != 'oamd':
                changes.update(ajoc_start_pos=start, ajoc_ramp_len_minus1=ramp - 1)
            if mode != 'ajoc':
                changes.update(block_offset_factor=2 * start, ramp_duration_table_index=RAMP_TABLE.index(64 * ramp))
            rows.append(dict(id=f'music_99_{mode}_s{start:02}_r{ramp:02}',
                             factor=0 if mode == 'ajoc' else 2 * start,
                             duration=2048 if mode == 'ajoc' else 64 * ramp, changes=changes))
    for factor in (1, 31, 63):
        for duration in (32, 1001):
            rows.append(dict(id=f'music_99_oamd_f{factor:02}_d{duration:04}', factor=factor, duration=duration,
                             changes=dict(block_offset_factor=factor, ramp_duration_table_index=RAMP_TABLE.index(duration))))
    return [dict(c, raw_au=99, consumer_au=100, reused=i < 10) for i, c in enumerate(rows)]


def make_variant(source, fields, configuration):
    frames, located = validate_fields(source, fields)
    if configuration not in configurations():
        raise ValueError('配置不在固定 16 项清单中')
    out = bytearray(source)
    patches = []
    for name, value in configuration['changes'].items():
        f = located[configuration['raw_au'], name]
        for i in range(f['width']):
            bit = f['bit'] + i
            mask = 1 << (7 - bit % 8)
            out[bit // 8] = (out[bit // 8] & ~mask) | (((value >> (f['width'] - 1 - i)) & 1) * mask)
        patches.append(dict(f, old=f['value'], new=value))
    frame = frames[configuration['raw_au'] - 1]
    if frame['crc']:
        out[frame['raw_end']:frame['end']] = crc16(out[frame['offset'] + 2:frame['raw_end']]).to_bytes(2, 'big')
    proof = dict(schema='macinac4.oamd-consumer-patch', version=1, configuration=configuration,
                 source_sha256=hashlib.sha256(source).hexdigest(), variant_sha256=hashlib.sha256(out).hexdigest(), patches=patches)
    verify_variant(source, bytes(out), fields, proof)
    return bytes(out), proof


def verify_variant(source, variant, fields, proof):
    if proof.get('schema') != 'macinac4.oamd-consumer-patch' or proof.get('version') != 1:
        raise ValueError('未知构造协议')
    cfg = proof['configuration']
    if cfg not in configurations():
        raise ValueError('未知配置')
    before, located = validate_fields(source, fields)
    if sync_frames(variant) != before or len(variant) != len(source):
        raise ValueError('帧边界或长度变化')
    if sha_bytes(source) != proof['source_sha256'] or sha_bytes(variant) != proof['variant_sha256']:
        raise ValueError('源或变体摘要不符')
    patches = proof['patches']
    if len(patches) != len(cfg['changes']) or {p['field'] for p in patches} != set(cfg['changes']):
        raise ValueError('字段清单不符')
    allowed = bytearray(len(source))
    for p in patches:
        f = located[cfg['raw_au'], p['field']]
        if p != dict(f, old=f['value'], new=cfg['changes'][p['field']]):
            raise ValueError('字段位置、旧值或来源错误')
        if read_bits(variant, f['bit'], f['width']) != p['new']:
            raise ValueError('新值未写入')
        for bit in range(f['bit'], f['bit'] + f['width']):
            allowed[bit // 8] |= 1 << (7 - bit % 8)
    frame = before[cfg['raw_au'] - 1]
    if frame['crc']:
        allowed[frame['raw_end']:frame['end']] = b'\xff\xff'
    if any((a ^ b) & ~mask for a, b, mask in zip(source, variant, allowed)):
        raise ValueError('白名单之外的比特变化')


def sha_bytes(data):
    return hashlib.sha256(data).hexdigest()


def read_payloads(path, expected_calls):
    """原始 get_oamd 包络；拒绝旧 read_meta 会静默接受的重复和尾部截断。"""
    data = Path(path).read_bytes()
    offset = 0
    rows = []
    for call in range(1, integer(expected_calls, 1, 1_000_000) + 1):
        if len(data) - offset < 40:
            raise ValueError('遗漏 AU 或包络截断')
        magic, version, actual_call, index, api_offset, samples, size = struct.unpack_from('<IIQIiQQ', data, offset)
        offset += 40
        if magic != 0x444d414f or version != 2 or actual_call != call or index != 0:
            raise ValueError('包络版本、顺序、唯一性或来源错误')
        if samples != 2048 or not 0 < size <= 1024 * 1024 or offset + size > len(data):
            raise ValueError('payload 尺寸错误')
        payload = data[offset:offset + size]
        offset += size
        rows.append(dict(call_index=call, pcm_start=(call - 1) * samples, samples=samples,
                         api_offset=api_offset, payload=payload, payload_sha256=sha_bytes(payload)))
    if offset != len(data):
        raise ValueError('多余或重复记录／尾部字节')
    return rows


def validate_acceptance(rows, expected_objects):
    expected_calls = len(expected_objects)
    if len(rows) != expected_calls:
        raise ValueError('调用数错误')
    for call, row in enumerate(rows, 1):
        if row['call_index'] != call or row['parse_rc'] != 0 or row['render_completed'] is not True:
            raise ValueError('解析或渲染失败／错源')
        if row['parse_calls'] != 1 or row['errors'] != 0 or row['accepted_payloads'] != 1 or row['objects'] != expected_objects[call - 1]:
            raise ValueError('payload 未被实际接受')


def validate_native(rows, payloads, external):
    """预先由实际消费者指令确定的整数模型；不从残差搜索尺度或偏移。"""
    count = len(payloads)
    if len(external) != count:
        raise ValueError('外部 OAMDI 记录遗漏')
    parsed = [r for r in rows if r['kind'] == 'parse']
    queued = [r for r in rows if r['kind'] == 'enqueue']
    accepted = [r for r in rows if r['kind'] == 'acceptance']
    queries = [r for r in rows if r['kind'] == 'query']
    if len(parsed) != count or len(queued) != count or not queries:
        raise ValueError('原生观察边界遗漏')
    if len(rows) != len(parsed) + len(queued) + len(accepted) + len(queries):
        raise ValueError('未知原生记录')
    validate_acceptance(accepted, [r['objects'] for r in external])
    for i, (p, q, ex, payload) in enumerate(zip(parsed, queued, external, payloads), 1):
        if p['au'] != i or q['au'] != i or p['rc'] != 0 or ex['call_index'] != i:
            raise ValueError('解析或入队错源')
        if p['objects'] != ex['objects'] or ex['payload_sha256'] != payload['payload_sha256']:
            raise ValueError('对象身份或 payload 摘要错误')
        for record in (p, q):
            if record['internal_offset'] != ex['internal_offset'] or record['blocks'] != ex['blocks']:
                raise ValueError('外部与内部 OAMDI 不一致')
        if q['api_offset'] != payload['api_offset'] or q['queue_time'] != q['queue_origin'] + q['api_offset'] - 1:
            raise ValueError('API 偏移或队列坐标错误')
        if not 1 <= q['count'] <= 32:
            raise ValueError('队列形状错误')
    changed = []
    active_centres = {au: [] for au in range(99, 103)}
    for q in queries:
        au = integer(q['au'], 99, 102)
        source = integer(q['source_call'], 1, count)
        ex, payload = external[source - 1], payloads[source - 1]
        if q['internal_offset'] != ex['internal_offset'] or q['blocks'] != ex['blocks']:
            raise ValueError('查询绑定了错误的元数据来源')
        absolute_query = payloads[au - 1]['pcm_start'] + integer(q['parent_query'], 0, 2048)
        relative_query = absolute_query - payload['pcm_start'] - payload['api_offset']
        if q['query'] != relative_query:
            raise ValueError('实际消费游标或 API 偏移错误')
        if len(ex['blocks']) != 1:
            raise ValueError('当前参考协议仅覆盖单块')
        factor, duration = ex['blocks'][0]
        start = event_sample(0, ex['internal_offset'], factor)
        active = relative_query >= start
        if q['rc'] != (ex['objects'] if active else 0):
            raise ValueError('第一步消费边界错误')
        if active and (q['duration'] != duration or q['value'] != max(0, duration - (relative_query - start))):
            raise ValueError('ramp 剩余时长或终点错误')
        if active:
            active_centres[au].append(q['parent_query'])
            eligible = [j + 1 for j in range(au)
                        if payloads[j]['pcm_start'] + payloads[j]['api_offset']
                        + event_sample(0, external[j]['internal_offset'], external[j]['blocks'][0][0]) <= absolute_query]
            if not eligible or source != eligible[-1]:
                raise ValueError('中断或跨块历史来源错误')
        if source == 100:
            changed.append(dict(au=au, query_sample=absolute_query, relative_query=relative_query,
                                active=active, remaining=q.get('value'),
                                scheduled_sample=payload['pcm_start'] + payload['api_offset'] + start))
    if any(values != list(range(16, 2048, 32)) for values in active_centres.values()):
        raise ValueError('实际消费窗口遗漏、重复或错一时隙')
    if not changed or not any(r['active'] for r in changed):
        raise ValueError('目标事件没有实际消费观察')
    return dict(calls=count, queries=len(queries), target_queries=changed,
                first_active_query=next(r for r in changed if r['active']),
                last_active_query=next(r for r in reversed(changed) if r['active']))


def validate_scene(rows, cfg, expected_calls=400):
    if not rows or rows[-1] != dict(kind='eof', aus=expected_calls, source_samples=expected_calls * 2048):
        raise ValueError('Scene 未完整到达 EOF')
    frames = [r for r in rows if r['kind'] == 'frame']
    events = [r for r in rows if r['kind'] == 'event']
    if len(frames) != expected_calls or len(events) != (expected_calls - 1) * 21 or len(rows) != len(frames) + len(events) + 1:
        raise ValueError('Scene 帧或事件遗漏')
    grouped = {}
    for event in events:
        grouped.setdefault(event['au'], []).append(event)
    selected = []
    for au, frame in enumerate(frames, 1):
        start = (au - 1) * 2048
        if frame != dict(kind='frame', au=au, source_start=start, codec_start=start, duration=2048,
                         objects=20, updates=0 if au == 1 else 21):
            raise ValueError('Scene 帧坐标或预热错误')
        current = grouped.get(au, [])
        if au == 1 and current:
            raise ValueError('提前消费')
        for obj, event in enumerate(current):
            source_au = au - 1
            factor = cfg['factor'] if source_au == 99 else 0
            duration = cfg['duration'] if source_au == 99 else 2048
            expected = dict(kind='event', au=au, source_au=source_au, object=obj, block=0,
                            offset=32 * factor, ramp=duration, sample_offset=0, offset_source='Implicit',
                            factor=factor, ramp_code=3, ramp_encoding=f'Table {{ index: {RAMP_TABLE.index(duration)} }}',
                            timing_updated=True, order=(au - 2) * 21 + obj)
            if event != expected:
                raise ValueError('Scene 时间、对象或控制来源错误')
            if source_au == 99:
                selected.append(dict(object=obj, source_sample=event_sample((source_au - 1) * 2048, 0, factor),
                                     scene_sample=start + event['offset'], ramp=duration))
    return dict(aus=len(frames), events=len(events), target_events=selected)


def canonical_events(payloads, external):
    """只在原生消费者已验证后使用；坐标为 OAR 输入 PCM，不是源节目时间。"""
    if len(payloads) != len(external):
        raise ValueError('规范化输入缺失')
    events = []
    for payload, ex in zip(payloads, external):
        for factor, duration in ex['blocks']:
            events.append(dict(coordinate=event_sample(payload['pcm_start'] + payload['api_offset'], ex['internal_offset'], factor),
                               ramp=duration, objects=ex['objects']))
    return events


def validate_bridge(legacy, expected):
    if len(legacy) != len(expected):
        raise ValueError('桥接事件缺失')
    for call, (a, b) in enumerate(zip(legacy, expected), 1):
        if any(a[k] != b[k] for k in ('coordinate', 'ramp', 'objects')):
            raise ValueError(f'桥接反例 call={call}: coordinate={a["coordinate"]}, consumer_sample={b["coordinate"]}')


class EventQueue:
    """独立的本地事件分帧模型；边界事件属于下一帧，reset 清空历史。"""
    def __init__(self):
        self.pending = []
        self.last = {}
        self.cursor = 0

    def reset(self):
        self.__init__()

    def frame(self, duration, new):
        end = integer(self.cursor + integer(duration, 1, 4096))
        for event in new:
            time = integer(event['sample'])
            obj = integer(event['object'], 0, 158)
            if time < self.cursor or time < self.last.get(obj, I64_MIN):
                raise ValueError('事件倒序')
            self.last[obj] = time
            self.pending.append(dict(event))
        current = [e for e in self.pending if e['sample'] < end]
        self.pending = [e for e in self.pending if e['sample'] >= end]
        current.sort(key=lambda e: (e['sample'], e['order']))
        self.cursor = end
        return current


def prepare(old_manifest, destination):
    old = json.loads(Path(old_manifest).read_text())
    seed = old['seeds']['music']
    source = Path(seed['input']).read_bytes()
    if sha_bytes(source) != seed['input_sha256']:
        raise ValueError('母流摘要错误')
    fields_path = Path(seed['directory']) / 'fields.jsonl'
    fields = [json.loads(line) for line in fields_path.read_text().splitlines()]
    root = Path(destination)
    root.mkdir()
    cases = []
    for cfg in configurations():
        variant, proof = make_variant(source, fields, cfg)
        path, evidence = root / (cfg['id'] + '.ac4'), root / (cfg['id'] + '.json')
        if cfg['reused']:
            previous = next(c for c in old['cases'] if c['configuration']['id'] == cfg['id'])
            if Path(previous['input']).read_bytes() != variant:
                raise ValueError('旧配置不可逐位复现')
        path.write_bytes(variant)
        evidence.write_text(json.dumps(proof, indent=2) + '\n')
        cases.append(dict(configuration=cfg, input=str(path), proof=str(evidence)))
    manifest = dict(schema='macinac4.oamd-consumer', version=1, seed=seed, fields=str(fields_path),
                    fields_sha256=sha(fields_path), predecessor_manifest=str(old_manifest), cases=cases)
    (root / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--prepare', type=Path)
    parser.add_argument('--captures', type=Path, help='含各配置 external.json、四份 receipt 与双进程 trace 的目录')
    parser.add_argument('--scene', type=Path, help='含各配置 scene.jsonl 的目录')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--legacy-scene', type=Path, help='显式复现旧桥接非零偏移失败；不自动修正旧文件')
    parser.add_argument('--legacy-case', help='旧桥接文件所属的固定配置 ID')
    args = parser.parse_args()
    if args.prepare:
        result = prepare(args.manifest, args.prepare)
        print(json.dumps(dict(configurations=len(result['cases'])), ensure_ascii=False))
    else:
        manifest = json.loads(args.manifest.read_text())
        source = Path(manifest['seed']['input']).read_bytes()
        if sha(manifest['fields']) != manifest['fields_sha256']:
            raise ValueError('字段记录摘要错误')
        fields = [json.loads(s) for s in Path(manifest['fields']).read_text().splitlines()]
        for case in manifest['cases']:
            verify_variant(source, Path(case['input']).read_bytes(), fields, json.loads(Path(case['proof']).read_text()))
        if [c['configuration'] for c in manifest['cases']] != configurations():
            raise ValueError('配置遗漏或重复')
        result = dict(schema='macinac4.oamd-consumer-comparison', version=1,
                      manifest_sha256=sha(args.manifest), cases=[])
        if bool(args.captures) != bool(args.scene):
            raise ValueError('参考与 Scene 捕获必须配对提供')
        if args.captures:
            import numpy as np
            for case in manifest['cases']:
                cfg = case['configuration']
                root = args.captures / cfg['id']
                source = json.loads((root / 'source.json').read_text())
                proof = json.loads(Path(case['proof']).read_text())
                decoder_receipt = Path(source['receipt'])
                for receipt_path in (decoder_receipt, decoder_receipt.parent.parent / 'baseline_confirm/receipt.json'):
                    decoder = json.loads(receipt_path.read_text())
                    good = [a for a in decoder['attempts'] if a['valid']]
                    if (decoder['id'] != cfg['id'] or decoder['input_sha256'] != proof['variant_sha256']
                            or decoder['expected_frames'] != 400 or not good
                            or any(a['pcm_sha256'] != source['pcm_sha256'] or a['returncode'] != 0
                                   or a['calls'] != 400 or a['unchanged_prefix_sha256'] != a['expected_prefix_sha256'] for a in good)):
                        raise ValueError('完整解码来源或独立确认错误')
                payloads = read_payloads(source['payload'], 400)
                external = json.loads((root / 'external.json').read_text())
                if sha(source['pcm']) != source['pcm_sha256'] or sha(source['payload']) != source['payload_sha256']:
                    raise ValueError('参考输入身份变化')
                for i, ex in enumerate(external, 1):
                    expected = [cfg['factor'], cfg['duration']] if i == 100 else [0, 2048]
                    if ex['blocks'] != [expected] or ex['internal_offset'] != 0:
                        raise ValueError('参考元数据未对应配置')
                baseline = root / 'baseline' / 'output.raw'
                digest = sha(baseline)
                if baseline.stat().st_size != 400 * 2048 * 16 * 4:
                    raise ValueError('OAR PCM 长度错误')
                samples = np.memmap(baseline, dtype='<f4', mode='r')
                if not np.isfinite(samples).all() or not np.any(samples):
                    raise ValueError('OAR PCM 非有限或全零')
                del samples
                adapter_digest = json.loads((root / 'baseline/receipt.json').read_text())['adapter_sha256']
                for mode in ('baseline', 'confirm', 'r1', 'r2'):
                    receipt = json.loads((root / mode / 'receipt.json').read_text())
                    if (receipt['mode'] != mode or receipt['returncode'] != 0 or receipt['pcm_sha256'] != digest
                            or receipt['bytes'] != baseline.stat().st_size or receipt['finite'] is not True
                            or receipt['adapter_sha256'] != adapter_digest
                            or receipt['input_pcm_sha256'] != source['pcm_sha256']
                            or receipt['input_payload_sha256'] != source['payload_sha256']):
                        raise ValueError('参考门禁或输入身份错误')
                    if mode.startswith('r') and sha(root / mode / 'trace.jsonl') != receipt['trace_sha256']:
                        raise ValueError('捕获记录摘要错误')
                if (root / 'r1/trace.jsonl').read_bytes() != (root / 'r2/trace.jsonl').read_bytes():
                    raise ValueError('独立进程捕获不一致')
                native = validate_native([json.loads(s) for s in (root / 'r1/trace.jsonl').read_text().splitlines()], payloads, external)
                scene_file = args.scene / cfg['id'] / 'scene.jsonl'
                scene = validate_scene([json.loads(s) for s in scene_file.read_text().splitlines()], cfg)
                if args.legacy_case == cfg['id']:
                    if not args.legacy_scene:
                        raise ValueError('缺少旧桥接文件')
                    validate_bridge(read_scene_export(args.legacy_scene), canonical_events(payloads, external))
                result['cases'].append(dict(configuration=cfg, native=native, scene=scene,
                                            reference_pcm_sha256=digest, trace_sha256=sha(root / 'r1/trace.jsonl'),
                                            scene_sha256=sha(scene_file), input_pcm_sha256=source['pcm_sha256'],
                                            input_payload_sha256=source['payload_sha256']))
        if (args.legacy_scene or args.legacy_case) and (not args.captures or args.legacy_case not in {c['id'] for c in configurations()}):
            raise ValueError('旧桥接反例必须具名绑定已验证捕获')
        if args.output:
            args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
        print(f'16 configurations verified; {len(result["cases"])} complete consumer/Scene comparisons')


if __name__ == '__main__':
    main()
