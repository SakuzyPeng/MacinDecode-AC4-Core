#!/usr/bin/env python3
"""固定 HF 输入的包络、增益和组装对照；只读归一化数据，不调用参考库。"""
from __future__ import annotations
import argparse
import json
import math
from pathlib import Path
import struct
import sys
import numpy as np

if __package__:
    from .check_aspx_basis_replay import metrics
    from .check_aspx_noise_replay import floats, integer
    from .check_asf_imdct_checkpoints import identical, identifier
    from .check_aspx_gain_replay import validate_records, parse_local
else:
    from check_aspx_basis_replay import metrics
    from check_aspx_noise_replay import floats, integer
    from check_asf_imdct_checkpoints import identical, identifier
    from check_aspx_gain_replay import validate_records, parse_local

MODES = ['captured', 'local_off', 'local_literal', 'reference_divide',
         'reference_multiply', 'half', 'double', 'alternating']


def validate_manifest(m):
    if m.get('schema') != 'macinac4.hf-downstream-replay' or type(m.get('version')) is not int or m['version'] != 1 or m['modes'] != MODES:
        raise ValueError('协议或实验模式错误')
    seen=set();ids=set()
    for c in m['cases']:
        name=identifier(c['id']);call=integer(c['call'],0,8)
        key=c['scope'],call
        if key in seen or name in ids:
            raise ValueError('重复调用')
        seen.add(key);ids.add(name)
        for key,bound in [('master',1),('start',7),('stop',3),('noise',3),('xover',7)]:
            integer(c[key],0,bound)
        lo,hi=integer(c['sbx'],4,63),integer(c['sbz'],5,64)
        if hi<=lo or len(c['mapped_preflat_gain'])!=hi-lo or not all(np.isfinite(x) and x>0 for x in c['mapped_preflat_gain']):
            raise ValueError('频带或预平坦化增益错误')
        for borders in [c['low'],c['high']]:
            if len(borders)<2 or borders[0]!=lo or borders[-1]!=hi:
                raise ValueError('频带端点不符')
            for a,b in zip(borders,borders[1:]):
                integer(a,0,63);integer(b,1,64)
                if a>=b:raise ValueError('频带不递增')
        times=c['signal_borders'];n=len(times)-1
        if not 1<=n<=4 or len(c['freq_res'])!=n or c['envelopes']!=[[2*a,2*b] for a,b in zip(times,times[1:])]:
            raise ValueError('包络数量或时间单位错位')
        if c['first']!=2*times[0] or c['last']!=2*times[-1] or not 0<=times[0]<=3 or not 16<=times[-1]<=19:
            raise ValueError('调用端点错误')
        for a,b in zip(times,times[1:]):
            integer(a,0,18);integer(b,1,19)
            if a>=b:raise ValueError('时间不递增')
        for x in c['freq_res']:integer(x,0,1)
    if seen!={(s,c) for s in ['tone','music100','music33'] for c in range(9)}:
        raise ValueError('实际调用覆盖不完整')


def expected_keys(m):
    return [f"{c['id']}v{v}i{i}l{l}e{e}" for c in m['cases'] for v in range(8)
            for i in range(2) for l in range(2) for e in range(len(c['envelopes']))]


def parameter_lines(m):
    lines=[]
    for c in m['cases']:
        for v in range(8):
            for i in range(2):
                values=[f"{c['id']}v{v}i{i}",c['master'],c['start'],c['stop'],c['noise'],c['xover'],
                        c['sbx'],c['sbz'],i,len(c['envelopes']),*c['signal_borders'],*c['freq_res'],
                        len(c['low']),*c['low'],len(c['high']),*c['high']]
                lines.append(' '.join(map(str,values)))
    return lines


def estimate_power(q, borders, interpolation):
    if q.ndim!=3 or q.shape[-1]!=2 or not q.size or not np.isfinite(q).all():
        raise ValueError('HF 矩阵无效')
    power=np.mean(np.sum(q.astype(float)**2,axis=-1),axis=0)
    if not interpolation:
        if borders[0]!=0 or borders[-1]!=len(power) or any(a>=b for a,b in zip(borders,borders[1:])):
            raise ValueError('相对子带组错误')
        for a,b in zip(borders,borders[1:]):power[a:b]=np.mean(power[a:b])
    return power.astype('<f4')


def parse_estimates(text,m):
    expected={(f"{c['id']}v{v}i{i}",e,b) for c in m['cases'] for v in range(8) for i in range(2)
              for e in range(len(c['envelopes'])) for b in range(c['sbz']-c['sbx'])}
    result={}
    for line in text.splitlines():
        p=line.split()
        if len(p)!=4:raise ValueError('估计行形状错误')
        key=(p[0],int(p[1]),int(p[2]));value=struct.unpack('<f',struct.pack('<I',int(p[3],16)))[0]
        if key not in expected or key in result or not math.isfinite(value) or value<0:
            raise ValueError('估计身份、数量或有限性错误')
        result[key]=value
    if result.keys()!=expected:raise ValueError('遗漏估计包络或子带')
    return result


def assemble_model(q,gains,table,noise_start,onset,masked=True):
    slots,n,_=q.shape
    if gains.shape!=(n,3) or table.shape!=(512,2) or not np.isfinite(gains).all() or np.any(gains<0):
        raise ValueError('组装输入无效')
    if np.any(gains[:,2]):raise ValueError('本轮真实案例不覆盖非零正弦组装')
    index=(integer(noise_start,0,511)+np.arange(slots*n)+1)%512
    signal=q.astype(float)*gains[:,0][None,:,None]
    noise=table[index].reshape(slots,n,2).astype(float)*gains[:,1][None,:,None]
    if masked and onset:noise[:]=0
    return (signal+noise).astype('<f4'),signal.astype('<f4'),noise.astype('<f4')


def compare(manifest,data,references,estimates,gain_inputs,gain_output,onset):
    m=json.loads(manifest.read_text());validate_manifest(m)
    if (data/'cases.txt').read_text().splitlines()!=parameter_lines(m):
        raise ValueError('生产估计参数与清单不符')
    records=json.loads((references/'r0/envelopes.json').read_text())
    known=validate_records(records)
    if list(known)!=expected_keys(m):raise ValueError('参考包络缺失、错位或重复')
    for i in range(1,4):
        if (references/f'r{i}/envelopes.json').read_bytes()!=(references/'r0/envelopes.json').read_bytes():
            raise ValueError('独立进程的包络记录不重复')
    local_est=parse_estimates(estimates.read_text(),m)
    gi=validate_records(json.loads(gain_inputs.read_text()));lg=parse_local(gain_output.read_text(),gi)
    if set(gi)!={r['id']+kind for r in records for kind in ['native','production']}:
        raise ValueError('增益重放来源集合错误')
    table=floats(data/'table.f32',(512,2));results=[]
    for c in m['cases']:
        id=c['id'];lo,hi=c['sbx'],c['sbz'];n=hi-lo
        base=floats(data/f'{id}v0i1.f32',(40,64,2))
        for v in range(8):
            q=floats(data/f'{id}v{v}i0.f32',(40,64,2))
            if not identical(q,floats(data/f'{id}v{v}i1.f32',(40,64,2))):raise ValueError('插值对照没有固定 HF 输入')
            if v>=3:
                expected=base.copy();part=expected[c['first']:c['last'],lo:hi]
                g=np.array(c['mapped_preflat_gain'],dtype='<f4')[None,:,None]
                if v==3:part/=g
                elif v==4:part*=g
                elif v==5:part*=np.float32(.5)
                elif v==6:part*=np.float32(2)
                else:part*=np.where(np.arange(n)%2,4,.25).astype('<f4')[None,:,None]
                if not identical(q,expected):raise ValueError('构造 HF 不符合固定配方')
            for i in range(2):
                for l in range(2):
                    for e,(a,b) in enumerate(c['envelopes']):
                        name=f'{id}v{v}i{i}l{l}e{e}';r=known[name]
                        if (r['base'],r['mode'],r['interpolation'],r['bitstream_limiter'],r['env'],r['first'],r['last'])!=(id,v,i,l,e,a,b):
                            raise ValueError('参考行身份与布局不符')
                        expected_borders=c['high'] if c['freq_res'][e] else c['low']
                        power=estimate_power(q[a:b,lo:hi],[x-lo for x in expected_borders],bool(i))
                        local=np.array([local_est[(f'{id}v{v}i{i}',e,s)] for s in range(n)],'<f4')
                        if not np.allclose(local,power,rtol=8*np.finfo('f4').eps,atol=np.finfo('f4').smallest_subnormal):
                            raise ValueError('生产包络估计与独立时频平均不符')
                        energy=np.array(r['estimated'],dtype='<f4')
                        if energy.shape!=(n,):raise ValueError('估计带数错误')
                        if r['estimated']!=known[f'{id}v{v}i{i}l0e{e}']['estimated']:
                            raise ValueError('只切 limiter 却改变了估计输入')
                        fixed=known[f'{id}v0i1l1e{e}']
                        for key in ['target','noise_ratio','sine_index','sine_area','onset','low','high','noise_borders','limiter','noise_before','tone_before']:
                            if r[key]!=fixed[key]:raise ValueError('反事实改变了固定控制')
                        integer(r['noise_before'],0,511);integer(r['tone_before'],0,3)
                        if r['noise_after']!=(r['noise_before']+(b-c['first'])*n)%512 or r['tone_after']!=(r['tone_before']+b-c['first'])%4:
                            raise ValueError('组装结束游标错误')
                        reference=floats(references/'r0'/f'{name}.f32',(b-a,n,2))
                        for run in range(1,4):
                            if not identical(reference,floats(references/f'r{run}'/f'{name}.f32',(b-a,n,2))):
                                raise ValueError('独立进程的组装输出不重复')
                        g=np.array(r['reference'],dtype='<f4')
                        noise_start=(r['noise_before']+(a-c['first'])*n)%512
                        assembled,signal,noise=assemble_model(q[a:b,lo:hi],g,table,noise_start,r['onset'])
                        unmasked,_,_=assemble_model(q[a:b,lo:hi],g,table,noise_start,r['onset'],False)
                        base_name=f'{id}v0i{i}l{l}e{e}';base_output=floats(references/'r0'/f'{base_name}.f32',(b-a,n,2))
                        scores={}
                        for kind in ['native','production']:
                            record=gi[name+kind]
                            expected_est=energy if kind=='native' else local
                            if not identical(np.array(record['estimated'],dtype='<f4'),expected_est):raise ValueError('增益重放接错包络估计')
                            for key in ['target','noise_ratio','sine_index','sine_area','noise_level','sine_level','onset','config','low','high','noise_borders','limiter']:
                                if record[key]!=r[key]:raise ValueError('增益重放接错固定控制')
                            for mode in (['on'] if l else ['off','boost']):
                                got=np.array([lg[name+kind,mode,s][:3] for s in range(n)],'<f4')
                                scores[f'{kind}_{mode}']=metrics(g,got)
                        results.append(dict(id=name,scope=c['scope'],base=id,env=e,mode=MODES[v],interpolation=i,limiter=l,onset=r['onset'],
                            estimate=metrics(energy,local),production_vs_math=metrics(power,local),
                            assembly_model=metrics(reference,assembled),unmasked_reference_gains=metrics(reference,unmasked),
                            hf_change=metrics(base[a:b,lo:hi].copy(),q[a:b,lo:hi].copy()),
                            assembled_change=metrics(base_output,reference),local_gains=scores))
    o0=floats(onset/'onset0.f32',(32,64,2));o1=floats(onset/'onset1.f32',(32,64,2))
    expected=np.zeros_like(o0);expected[:,36:56]=table[(np.arange(32*20)+1)%512].reshape(32,20,2)
    observation=dict(ordinary_matches_unit_noise=identical(o0,expected),onset_matches_unit_noise=identical(o1,expected),onset_output_energy=float(np.sum(o1.astype(float)**2)),between=metrics(o0,o1))
    return dict(schema='macinac4.hf-downstream-comparison',version=1,status='observation_only',output_fitting=False,
                reference_processes=4,envelope_variants=len(results),results=results,onset_production_observation=observation)


def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['manifest','data','references','estimates','gain_inputs','gain_output','onset']:p.add_argument(name,type=Path)
    p.add_argument('--output',type=Path,required=True);a=p.parse_args(argv)
    try:
        result=compare(a.manifest,a.data,a.references,a.estimates,a.gain_inputs,a.gain_output,a.onset)
        with a.output.open('x') as f:json.dump(result,f,ensure_ascii=False,indent=2,allow_nan=False);f.write('\n')
    except (OSError,ValueError,KeyError,TypeError,struct.error) as error:
        print(f'下游重放检查失败：{error}',file=sys.stderr);return 2
    print(json.dumps({k:result[k] for k in ['status','reference_processes','envelope_variants']}));return 0


if __name__=='__main__':raise SystemExit(main())
