#!/usr/bin/env python3
"""整数音频／OAMD 时间锚点与两种 ramp 日程的独立对照。"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import numpy as np
try:
    from .check_ajoc_decorrelator import array, sha
    from .check_ajoc_matrix import exact
    from .check_aspx_noise_replay import integer
    from .check_qmf_phase_chain import analyse_fir, synthesise_fir, prototype, WINDOW_SHA256, require_bound
except ImportError:
    from check_ajoc_decorrelator import array, sha
    from check_ajoc_matrix import exact
    from check_aspx_noise_replay import integer
    from check_qmf_phase_chain import analyse_fir, synthesise_fir, prototype, WINDOW_SHA256, require_bound


def metadata_events(timing, start):
    if timing is None:
        return []
    integer(start, -(2**63), 2**63-1)
    offset = integer(timing['sample_offset'], 0, 2048)
    blocks = timing['blocks']
    if not blocks or len(blocks) > 8:
        raise ValueError('OAMD 块数非法')
    result = []
    for b in blocks:
        factor = integer(b['factor'], 0, 63)
        integer(b['ramp'], 0, 2048)
        integer(b['code'], 0, 3)
        result.append(integer(start + offset + 32 * factor, -(2**63), 2**63-1))
    return result


def validate_timeline(rows, matrix):
    if not rows or not matrix:
        raise ValueError('时间或矩阵序列为空')
    consumed = {r['au']: r for r in matrix}
    if len(consumed) != len(matrix) or set(consumed) != set(range(2, len(rows)+1)):
        raise ValueError('矩阵消费序列不完整')
    previous_dmx = previous_umx = None
    records = []
    start = 0
    for i, r in enumerate(rows):
        n = integer(r['samples'], 1536, 2048)
        if n not in (1536, 1920, 2048) or n != rows[0]['samples']:
            raise ValueError('未覆盖的帧长或拓扑改变')
        delay = {1536:96, 1920:288, 2048:352}[n]
        if r['au'] != i+1 or r['input_sample_start'] != start or r['consumer_qmf_base'] != start//64 or r['pcm_delay'] != delay or r['aspx_delay'] != 6:
            raise ValueError('音频绝对样本／时隙锚点错误')
        if i == 0:
            if r['source_sample_start'] is not None or r['applied_dmx'] is not None or r['applied_umx'] is not None or r['oamd_events']:
                raise ValueError('预热消费了未到期控制')
        else:
            origin = rows[i-1]
            source = origin['input_sample_start']
            if r['source_sample_start'] != source:
                raise ValueError('控制 FIFO 的源样本起点错误')
            dmx = origin['raw_dmx'] or origin['group_timing'] or previous_dmx
            umx = origin['raw_umx'] or (dmx if origin['derive'] is True else origin['group_timing'] or previous_umx)
            if r['applied_dmx'] != dmx or r['applied_umx'] != umx:
                raise ValueError('原始 OAMD timing 继承或控制交接错误')
            events = metadata_events(umx, source)
            if events != r['oamd_events']:
                raise ValueError('OAMD 原始字段的整数事件时间不符')
            m = consumed[i+1]
            if len(m['starts']) != len(m['ramps']) or len(m['starts']) > 2:
                raise ValueError('实际消费数据点形状错误')
            if m['slots'] != n//64 or m['starts'] != origin['raw_starts'] or m['ramps'] != origin['raw_ramps']:
                raise ValueError('A-JOC 实际消费的日程不是同源 raw AU')
            for p, length in zip(m['starts'], m['ramps']):
                integer(p, 0, n//64-1)
                integer(length, 1, 64)
            if m['selected']:
                records.append(dict(au=i+1, source_au=i, source_sample_start=source,
                    consumer_qmf_base=start//64, oamd_source_sample_events=events,
                    oamd_ramp_samples=[b['ramp'] for b in umx['blocks']] if umx else [],
                    matrix_start_source_samples=[source+64*p for p in m['starts']],
                    immediate_target_source_samples=[source+64*(p+length-1) for p,length in zip(m['starts'],m['ramps'])],
                    deferred_target_source_samples=[source+64*(p+length) for p,length in zip(m['starts'],m['ramps'])],
                    starts=m['starts'], ramps=m['ramps'],
                    first_slot_fir_source_support=[start-6*64+63-delay-639, start-6*64+63-delay],
                    immediate_first_global_slots=[start//64+p for p in m['starts']],
                    deferred_first_global_slots=[start//64+p+1 for p in m['starts']]))
            previous_dmx, previous_umx = dmx, umx
        start += n
    return dict(frames=len(rows), reconstructed_frames=len(matrix), selected=records,
                integer_timeline=True, independent_normative_schedule_decision=False)


def scalar_schedule(rows, *, immediate=False):
    """单系数的事件日程；两种解释仅在第一步发生时刻不同。"""
    current = delta = target = np.float32(0)
    remaining = 0
    values = []
    for row in rows:
        n = integer(row['slots'], 1, 32)
        if len(row['starts']) != len(row['ramps']) or len(row['targets']) != len(row['starts']):
            raise ValueError('时间点形状错误')
        if not np.isfinite(np.asarray(row['targets'], float)).all():
            raise ValueError('目标不是有限数')
        for p, ramp in zip(row['starts'], row['ramps']):
            integer(p, 0, n-1)
            integer(ramp, 1, 64)
        for ts in range(n):
            hits = [i for i,p in enumerate(row['starts']) if p == ts]
            if immediate:
                for i in hits:
                    target = np.float32(row['targets'][i])
                    remaining = row['ramps'][i]
                    delta = np.float32(np.float32(target-current)/np.float32(remaining))
            if remaining:
                current = np.float32(current + delta)
                remaining -= 1
                if remaining == 0:
                    current = target
            values.append(current)
            if not immediate:
                for i in hits:
                    target = np.float32(row['targets'][i])
                    remaining = row['ramps'][i]
                    delta = np.float32(np.float32(target-current)/np.float32(remaining))
    return np.asarray(values, '<f4')


def check_synthetic(root):
    root = Path(root)
    if sha(root/'window.f32') != WINDOW_SHA256:
        raise ValueError('独立 FIR 原型窗身份错误')
    window = array(root/'window.f32', (640,))
    expected_names = {f's{s}p{p}r{r}{kind}' for s in (24,30,32) for p in (0,s//2,s-1)
                      for r in (1,2,31,32,33,64) for kind in ('impulse','step')}
    if {p.name for p in root.iterdir() if p.is_dir()} != expected_names:
        raise ValueError('时间探针覆盖不完整')
    results = []
    for name in sorted(expected_names):
        d = root/name
        case = json.loads((d/'case.json').read_text())
        slots = integer(case['slots'],24,32)
        n = slots*64
        length = 5*n
        if case['frames'] != 5 or case['pcm_delay'] != {24:96,30:288,32:352}[slots]:
            raise ValueError('构造链帧数或延迟错误')
        pcm = array(d/'pcm_input.f32',(length,))
        expected_pcm = np.zeros(length,'<f4')
        if case['pattern']=='impulse':
            expected_pcm[[n//2,n+63,2*n-1,3*n+32]]=1
        elif case['pattern']=='step':
            expected_pcm[n+3:]=.25
        else:
            raise ValueError('未知波形')
        exact(pcm,expected_pcm,'带样本标签的构造输入')
        delayed = np.zeros(length)
        delay = case['pcm_delay']
        delayed[delay:] = pcm[:-delay].astype(float)/2
        analysed = analyse_fir(delayed,window)
        expected_x = np.zeros(analysed.shape,complex)
        expected_x[6:] = analysed[:-6]
        rows = [json.loads(line) for line in (d/'frames.jsonl').read_text().splitlines()]
        if len(rows)!=5 or [r['au'] for r in rows]!=list(range(1,6)):
            raise ValueError('构造矩阵帧序列不完整')
        schedule=[]; xs=[]; zs=[]; coefficients=[]
        for i,r in enumerate(rows):
            expected_points = {0:([0],[1],[60]),1:([case['start']],[case['ramp']],[70]),
                               3:([0,slots-1],[64,2],[40,55])}.get(i,([],[],[]))
            starts,ramps,codes=expected_points
            if r['starts']!=starts or r['ramps']!=ramps or r['slots']!=slots or r['dmx']!=1 or r['decorr']!=0:
                raise ValueError('构造事件日程改变')
            schedule.append(dict(slots=slots,starts=starts,ramps=ramps,
                                 targets=[(c-50)*205/2048 for c in codes]))
            prefix=d/f'au{i+1:04}'
            x=array(str(prefix)+'.x.f32',(slots,64,2));z=array(str(prefix)+'.z.f32',x.shape)
            c=array(str(prefix)+'.coeff.f32',(slots,64))
            if not np.all(c==c[:,0,None]):raise ValueError('单参数带展开错误')
            xs.append(x);zs.append(z);coefficients.extend(c[:,0])
        x=np.concatenate(xs);z=np.concatenate(zs)
        bound=4*np.finfo(np.float32).eps*np.max(abs(delayed))*sum(abs(prototype(window)))
        qmf_error=require_bound(expected_x,x[...,0]+1j*x[...,1],bound,'绝对样本 FIR 锚点')
        local=scalar_schedule(schedule);early=scalar_schedule(schedule,immediate=True)
        exact(coefficients,local,'当前实际消费系数的时隙')
        exact(z,(x.astype(float)*local[:,None,None].astype(float)).astype('<f4'),'dry 矩阵乘法')
        expected_output=2*synthesise_fir(z[...,0]+1j*z[...,1],window)
        output=array(d/'pcm_output.f32',(length,))
        synthesis_bound=8*np.finfo(np.float32).eps*np.max(abs(z))*sum(abs(prototype(window)))
        pcm_error=require_bound(expected_output,output,synthesis_bound,'完整合成时间轴')
        different=np.flatnonzero(local.view('u4')!=early.view('u4'))
        if not len(different):raise ValueError('构造链没有区分两种日程')
        results.append(dict(id=name,slots=5*slots,qmf_max_error=qmf_error,qmf_bound=bound,
                            pcm_max_error=pcm_error,pcm_bound=synthesis_bound,
                            schedule_different_slots=len(different),first_different_slot=int(different[0])))
    return results


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('root',type=Path)
    p.add_argument('--output',type=Path,required=True)
    args=p.parse_args()
    result=dict(schema='macinac4.ajoc-timing-comparison',version=1,production_dsp_changed=False,
                synthetic=check_synthetic(args.root/'timing'),real={})
    for name in ('tone','music','bed256'):
        d=args.root/('local_'+name)
        read=lambda p:[json.loads(line) for line in p.read_text().splitlines()]
        result['real'][name]=validate_timeline(read(d/'timeline.jsonl'),read(d/'matrix/frames.jsonl'))
    args.output.write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(f"通过 {len(result['synthetic'])} 条构造链与三份完整流的时间锚点")


if __name__=='__main__':
    main()
