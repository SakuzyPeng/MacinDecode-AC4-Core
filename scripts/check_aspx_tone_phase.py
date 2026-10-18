#!/usr/bin/env python3
"""固定 QMF 定义，用实包络卷积的闭式核对音调相位；不搜索相位、尺度或延迟。"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np

try:
    from .check_qmf_phase_chain import OMEGA, WINDOW_SHA256, prototype, require_bound
    from .check_aspx_basis_replay import components, complex_values, metrics
    from .check_aspx_noise_replay import floats
except ImportError:
    from check_qmf_phase_chain import OMEGA, WINDOW_SHA256, prototype, require_bound
    from check_aspx_basis_replay import components, complex_values, metrics
    from check_aspx_noise_replay import floats


def cases():
    return [dict(id=f'b{b:02}s{s}f{f}c{c}p{p}',band=b,seed=s,factor=f,carry=c,profile=p)
            for b in range(10,62) for s in (0,1) for f in (1,2) for c in (0,3) for p in (0,1)]


def envelope(c):
    frame=15*c['factor']
    out=np.zeros(4*frame+16,dtype='<f4')
    for i in range(4):
        first=i*frame+(c['carry']*c['factor'] if i else 0)
        last=(i+1)*frame+(c['carry']*c['factor'] if i<3 else 0)
        out[first:last]=1 if c['profile']==0 else (0,1,0,.25)[i]
    return out


def phase_sequence(level,band,seed,origin=0):
    """用绝对时间的四分之一周坐标定义 QMF；不复制区间 previous 递推。"""
    level=np.asarray(level,dtype=float)
    if (level.ndim!=1 or not len(level) or not np.isfinite(level).all()
            or not 0<=band<64 or not 0<=seed<4 or not isinstance(origin,int)):
        raise ValueError('音调时间轴或相位非法')
    phase=(seed+np.arange(len(level))-origin)%4
    real=np.where(phase%2==0,np.where(phase==0,1,-1),0)
    imag=np.where(phase%2==1,np.where(phase==1,1,-1),0)*(-1 if band%2 else 1)
    return level*(real+1j*imag)


def filtered_envelope(level,window):
    """每 64 样本放置一次实幅度，再用去折叠符号后的原型做线性卷积。"""
    level=np.asarray(level,dtype=float)
    if level.ndim!=1 or not len(level) or not np.isfinite(level).all():
        raise ValueError('包络必须非空且有限')
    impulse=np.zeros(len(level)*64)
    impulse[::64]=level
    return np.convolve(impulse,prototype(window))[:len(impulse)]/64


def tone_pcm(level,window,band,seed,origin=0,phase_offset=0.):
    phase_sequence(level,band,seed,origin)  # 同一入口范围检查，不用于生成 PCM 期望值。
    if not np.isfinite(phase_offset):
        raise ValueError('相位偏移必须有限')
    time=np.arange(len(level)*64)
    sign=-1 if band%2 else 1
    phase=OMEGA[band]*(time-127.5)+sign*np.pi/2*(seed-origin)+phase_offset
    return filtered_envelope(level,window)*np.cos(phase)


def synthesis_bound(window,level):
    return float(4*np.finfo(np.float32).eps*np.max(abs(level))*
                 np.max(abs(prototype(window)).reshape(10,64).sum(axis=0))/64)


def phasor(pcm,band,first=16*64,last=32*64):
    if not 0<=first<last<=len(pcm):
        raise ValueError('固定稳态窗口越界')
    return 2*np.mean(pcm[first:last]*np.exp(-1j*OMEGA[band]*np.arange(first,last)))


def validate_rows(text):
    known={c['id']:c for c in cases()}
    rows={}
    for line in text.splitlines():
        p=line.split()
        if len(p)!=11 or p[0] not in known or p[0] in rows:
            raise ValueError('身份遗漏、重复或格式不符')
        c=known[p[0]]
        if list(map(int,p[1:6]))!=[c[k] for k in ('band','seed','factor','carry','profile')]:
            raise ValueError('案例参数不符')
        last=[(c['seed']+(i+1)*15*c['factor']+(c['carry']*c['factor'] if i<3 else 0)-1)%4 for i in range(4)]
        if list(map(int,p[6:10]))!=last:
            raise ValueError('结束游标错位')
        error=float(p[10])
        if not np.isfinite(error) or error<0:
            raise ValueError('无效数值检查记录')
        rows[p[0]]=dict(c,last=last,rust_max_abs_error=error)
    if set(rows)!=set(known):
        raise ValueError('覆盖不完整')
    return list(rows.values())


def compare(root):
    declared=validate_rows((root/'cases.txt').read_text())
    w=floats(root/'qwin.f32',(640,))
    if hashlib.sha256(w.tobytes()).hexdigest()!=WINDOW_SHA256:
        raise ValueError('锁定窗表不符')
    records=[];waveforms={}
    for c in declared:
        level=envelope(c);n=len(level);name=c['id'];band=c['band']
        q=floats(root/f'{name}.qmf.f32',(n,64,2));y=floats(root/f'{name}.pcm.f32',(n*64,))
        expected_q=np.zeros((n,64),complex)
        expected_q[:,band]=phase_sequence(level,band,c['seed'])
        if not np.array_equal(complex_values(q),expected_q):
            raise ValueError(f'{name}: 发生器、静音时钟或跨帧携带错误')
        expected=tone_pcm(level,w,band,c['seed'])
        bound=synthesis_bound(w,level)
        error=require_bound(expected,y,bound,'独立音调闭式')
        if c['rust_max_abs_error']>bound:
            raise ValueError('Rust 的独立前向误差超界')
        row=dict(c,samples=len(y),qmf=metrics(components(expected_q),q),pcm=metrics(expected.astype('<f4'),y),
                 forward_max_abs_error=error,forward_bound=bound,peak=float(np.max(abs(y))))
        if c['profile']==0:
            sign=-1 if band%2 else 1
            expected_carrier=np.sum(prototype(w))/4096*np.exp(1j*(sign*np.pi/2*c['seed']-127.5*OMEGA[band]))
            actual=phasor(y,band)
            if abs(actual-expected_carrier)>2*bound:
                raise ValueError('固定载波相位或幅度不符')
            row['carrier']=dict(expected=[float(expected_carrier.real),float(expected_carrier.imag)],actual=[float(actual.real),float(actual.imag)],absolute_error=float(abs(actual-expected_carrier)),phase_error_rad=float(np.angle(actual/expected_carrier)))
        records.append(row);waveforms[name]=y
    pairs=[]
    for c in declared:
        if c['seed']!=0:continue
        name=c['id'];name1=name.replace('s0','s1')
        zero,one=waveforms[name],waveforms[name1]
        wrong_shift=one[:-64]-zero[64:]
        row=dict(band=c['band'],factor=c['factor'],carry=c['carry'],profile=c['profile'],
                 phase_zero_vs_one=metrics(zero,one),
                 phase_only_vs_full_shift_max_abs=float(np.max(abs(wrong_shift))),
                 phase_only_vs_full_shift_error_energy=float(np.sum(wrong_shift.astype(float)**2)),
                 zero_peak=float(np.max(abs(zero))),one_peak=float(np.max(abs(one))))
        if c['profile']==0:
            same_time=one[16*64:32*64].astype(float)-zero[16*64:32*64]
            row['steady_phase_change_relative_rms']=float(np.sqrt(np.sum(same_time**2)/np.sum(zero[16*64:32*64].astype(float)**2)))
            steady=one[16*64:32*64]-zero[17*64:33*64]
            row['steady_64_sample_relation_max_abs']=float(np.max(abs(steady)))
            if row['steady_64_sample_relation_max_abs']>2*synthesis_bound(w,envelope(c)):
                raise ValueError('载波的一时隙关系不成立')
            # 起音／停音的固定反例不能被纯稳态判据掩盖。
            if row['phase_only_vs_full_shift_max_abs']<=100*synthesis_bound(w,envelope(c)):
                raise ValueError('边界反例不可辨')
        pairs.append(row)
    for c in declared:
        if c['profile']==0 and c['carry']==0:
            a=waveforms[c['id']];b=waveforms[c['id'].replace('c0','c3')]
            if not np.array_equal(a.view('<u4'),b.view('<u4')):
                raise ValueError('恒定包络因分界产生相位跳变')
    return dict(schema='macinac4.aspx-tone-phase-comparison',version=1,production_changed=False,
                fitting=False,cases=records,initial_phase_pairs=pairs,constant_envelope_split_bit_exact=True)


def compare_reference(inputs,window):
    records=json.loads((inputs/'cases.json').read_text());tones=[c for c in records if c['kind']=='S']
    if len(tones)!=128 or {(c['band'],c['seed']) for c in tones}!={(b,s) for b in range(64) for s in (0,1)}:
        raise ValueError('既有参考原语覆盖错误')
    rows=[]
    for c in tones:
        if (c['slots'],c['first'],c['last'])!=(48,3,32):
            raise ValueError('既有参考时间轴不符')
        level=np.zeros(48);level[3:32]=1
        q=floats(inputs/f"{c['id']}.native.f32",(48,64,2))
        expected_q=np.zeros((48,64),complex);expected_q[:,c['band']]=phase_sequence(level,c['band'],c['seed'],3)
        if not np.array_equal(complex_values(q),expected_q):raise ValueError('参考构造输入不符')
        path=inputs/f"{c['id']}.reference.f32"
        actual=floats(path,(48*64,))*np.float32(2**17)
        # 先前独立固定的 1/R(k) = exp(i*(pi-omega_k/2))，不拟合本批输出。
        expected=tone_pcm(level,window,c['band'],c['seed'],3,np.pi-OMEGA[c['band']]/2)
        bound=synthesis_bound(window,level)
        error=require_bound(expected,actual,bound,'参考原语的固定表示换算')
        rows.append(dict(id=c['id'],band=c['band'],seed=c['seed'],pcm=metrics(expected.astype('<f4'),actual),
                         forward_max_abs_error=error,forward_bound=bound,reference_sha256=hashlib.sha256(path.read_bytes()).hexdigest()))
    return dict(new_capture=False,reference_pcm_scale=2**17,phase_offset='pi-omega_k/2, fixed inverse R(k)',cases=rows)


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('input',type=Path);p.add_argument('--report',required=True,type=Path);p.add_argument('--reference-inputs',type=Path)
    a=p.parse_args();r=compare(a.input)
    if a.reference_inputs:r['reference']=compare_reference(a.reference_inputs,floats(a.input/'qwin.f32',(640,)))
    with a.report.open('x') as f:json.dump(r,f,ensure_ascii=False,indent=2,allow_nan=False);f.write('\n')
    print(f"verified {len(r['cases'])} production tone chains")


if __name__=='__main__':main()
