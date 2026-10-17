#!/usr/bin/env python3
"""核对离线的分析 QMF 检查点、同状态重放和下游交接。

可变 N 的 P65 式只是诊断推广；生产仍使用 64 子带。实际窗和前／后旋转表
均由调用方提供，脚本不内置参考运行时数据，不拟合增益、相位或延迟。
"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import sys
import numpy as np
if __package__:
    from .check_aspx_noise_replay import floats, integer
    from .check_aspx_basis_replay import components, complex_values, metrics
else:
    from check_aspx_noise_replay import floats, integer
    from check_aspx_basis_replay import components, complex_values, metrics

WIDTHS = (8, 48, 64)
FREQUENCIES = ((600,1200,1800), (125,750,1750), (1500,6000,12000), (2250,9000,15000))
AMPLITUDES = (.12,.09,.05)
PHASES = (.31,-.77,1.4)


def ideal_phases(n):
    if n not in WIDTHS:
        raise ValueError("未支持的分析宽度")
    pre = 1j*np.exp(-1j*np.pi*np.arange(n)/(2*n))/(2*n)
    post = np.exp(1j*np.pi*(np.arange(n)+.5)*(1/n-1/64)/2)
    return pre, post


def fold_input(pcm, history, window):
    pcm, h, window = np.asarray(pcm, dtype=float), np.asarray(history, dtype=float).copy(), np.asarray(window, dtype=float)
    if pcm.ndim != 2 or pcm.shape[1] not in WIDTHS:
        raise ValueError("PCM 形状错误")
    n = pcm.shape[1]
    if h.shape != (9,n) or window.shape != (10*n,) or not len(pcm):
        raise ValueError("历史或窗形状错误")
    if not all(np.isfinite(x).all() for x in (pcm,h,window)):
        raise ValueError("非有限分析输入")
    result=[]
    for x in pcm:
        newest_first=np.vstack([x,h])[:,::-1].reshape(-1)
        result.append((newest_first*window).reshape(5,2*n).sum(axis=0))
        h=np.vstack([x,h[:8]])
    return np.array(result),h


def postprocess(fft_values, post):
    n=fft_values.shape[1]
    result=np.empty_like(fft_values)
    result[:,::2]=np.conj(fft_values[:,:n//2])
    result[:,1::2]=fft_values[:,:n//2-1:-1]
    return result*post


def fft_analysis(pcm, history, window, pre, post):
    if pcm.ndim!=2:
        raise ValueError("PCM 形状错误")
    n=pcm.shape[1]
    if pre.shape != (n,) or post.shape != (n,) or not np.isfinite(pre).all() or not np.isfinite(post).all():
        raise ValueError("旋转表形状或有限性错误")
    folded,after=fold_input(pcm,history,window)
    filtered=(folded[:,n:]+1j*folded[:,:n])*pre
    fft=np.fft.fft(filtered,axis=1)
    return dict(filtered=filtered,fft=fft,qmf=postprocess(fft,post),history=after)


def direct_analysis(pcm, history, window, rule="common_grid"):
    n=pcm.shape[1]
    folded,_=fold_input(pcm,history,window)
    omega=np.pi*(np.arange(n)+.5)/n
    basis=np.exp(1j*(np.arange(2*n)-.5)[:,None]*omega[None,:])
    if rule=="common_grid":rotation=-np.exp(1j*np.pi*(np.arange(n)+.5)*(1/n-1/128))
    elif rule=="local_width":rotation=-np.exp(1j*omega/2)
    elif rule=="fixed64":rotation=-np.exp(1j*np.pi*(np.arange(n)+.5)/128)
    else:raise ValueError("未知相位规则")
    return (folded@basis)*rotation/(2*n)


def score(reference, prediction):
    return metrics(reference,components(prediction))


def bit_equal(x,y):
    return x.shape==y.shape and np.array_equal(x.view('<u4'),y.view('<u4'))


def valid_id(value):
    if not isinstance(value,str) or re.fullmatch(r"[A-Za-z0-9]+",value) is None:
        raise ValueError("案例身份错误")
    return value


def find_owner(contexts, tokens, target_au):
    matches=[]
    for c in contexts:
        indices=[i for i,a in enumerate(c['aus']) if a==target_au]
        if [c['tokens'][i] for i in indices]==tokens:
            matches.append((c['id'],indices))
    if len(matches)!=1:
        raise ValueError("分配身份不能唯一确定上游所有者")
    return matches[0]


def validate_manifest(m):
    if m.get('schema')!='macinac4.normalized-analysis-checkpoints' or type(m.get('version')) is not int or m['version']!=1:
        raise ValueError("未支持的检查点格式")
    cases=m['cases']
    if not isinstance(cases,list) or not 1<=len(cases)<=4096:
        raise ValueError("案例数量错误")
    ids=set()
    for c in cases:
        name=valid_id(c['id'])
        if name in ids:raise ValueError("重复案例")
        ids.add(name)
        n=integer(c['n'],1,64)
        integer(c['mode'],8,8)
        if n not in WIDTHS or c['kind'] not in ('live','probe'):
            raise ValueError("分析配置错误")
        slots=integer(c['slots'],1,1024)
        if c['kind']=='live':
            if slots!=64 or n==64 or len(c['aus'])!=slots or len(c['tokens'])!=slots:
                raise ValueError("实际检查点时序形状错误")
            for x in c['aus']:integer(x,1,1000000)
            for x in c['tokens']:integer(x,0,1000000)
        else:
            if c['stimulus']=='impulse':
                integer(c['phase'],0,n-1)
                if slots!=24:raise ValueError("脉冲案例长度错误")
            elif c['stimulus']=='random':
                integer(c['seed'],0,2**32-1)
                if slots!=64:raise ValueError("随机案例长度错误")
            else:raise ValueError("未知探针")
    return {c['id']:c for c in cases}


def probe_input(c):
    n,slots=c['n'],c['slots']
    x=np.zeros((slots,n),dtype='<f4')
    if c['stimulus']=='impulse':x[2,c['phase']]=1
    else:
        state=c['seed']
        for t in range(slots):
            for k in range(n):
                state=(1664525*state+1013904223)&0xffffffff
                x[t,k]=((state>>8)/2**24-.5)*.25
    return x


def grid_input(n, pattern, centered, slots=76, warmup=12):
    if n not in WIDTHS or not 0<=pattern<4 or max(FREQUENCIES[pattern])>=n*750/2:
        raise ValueError("公共波形超出源采样网格")
    index=np.arange(slots*n,dtype=float)
    position=((index+.5)*64/n-.5) if centered else index*64/n
    position-=warmup*64
    x=np.zeros_like(position)
    for frequency,amplitude,phase in zip(FREQUENCIES[pattern],AMPLITUDES,PHASES,strict=True):
        x+=amplitude*np.cos(2*np.pi*frequency*position/48000+phase)
    return x.reshape(slots,n).astype('<f4')


def compare_grids(cases, data):
    ids=set();geometries=set();by_key={}
    for c in cases:
        name=valid_id(c['id']);n=integer(c['n'],1,64);pattern=integer(c['pattern'],0,3)
        if name in ids or n not in WIDTHS or type(c['centered']) is not bool:
            raise ValueError("公共波形案例身份或类型错误")
        integer(c['slots'],76,76)
        integer(c['warmup'],12,12)
        if c['slots']!=76 or c['warmup']!=12 or (n==64 and c['centered']):
            raise ValueError("公共波形窗口错误")
        key=n,pattern,c['centered']
        if key in geometries:raise ValueError("重复公共波形配置")
        ids.add(name);geometries.add(key)
        x=floats(data/f'{name}.input.f32',(76,n));expected=grid_input(n,pattern,c['centered'])
        if np.max(abs(x.astype(float)-expected.astype(float)))>1e-7:
            raise ValueError("公共波形输入不匹配声明的采样网格")
        q=floats(data/f'{name}.reference.f32',(76,n,2));repeat=floats(data/f'{name}.repeat.f32',(76,n,2))
        if not bit_equal(q,repeat):raise ValueError("公共波形参考不重复")
        by_key[key]=(c,q)
    output=[]
    for (n,pattern,centered),(c,q) in by_key.items():
        if (64,pattern,False) not in by_key:raise ValueError("缺失配对的 64 子带控制")
        control,q64=by_key[64,pattern,False]
        local=floats(data/(control['id']+'.local64.f32'),(76,64,2))
        rotation=-np.exp(1j*np.pi*(np.arange(n)+.5)/128)
        prediction=complex_values(local[:,:n])*rotation/128
        output.append(dict(id=c['id'],n=n,pattern=pattern,centered=centered,warmup=12,compared_slots=64,
            reference64=metrics(q[12:].copy(),q64[12:,:n].copy()),
            local64=score(q[12:].copy(),prediction[12:])))
    return output


def compare(manifest, data):
    m=json.loads(manifest.read_text());known=validate_manifest(m)
    parameters={}
    for n in sorted({c['n'] for c in known.values()}):
        parameters[n]=(floats(data/f'window{n}.f32',(10*n,)),
                       complex_values(floats(data/f'pre{n}.f32',(n,2))),
                       complex_values(floats(data/f'post{n}.f32',(n,2))))
    outputs={};results=[]
    for c in m['cases']:
        name,n,slots=c['id'],c['n'],c['slots']
        pcm=floats(data/f'{name}.input.f32',(slots,n));before=floats(data/f'{name}.before.f32',(9,n))
        q=floats(data/f'{name}.reference.f32',(slots,n,2));repeat=floats(data/f'{name}.repeat.f32',(slots,n,2))
        if not bit_equal(q,repeat):raise ValueError("独立进程输出不重复")
        if c['kind']=='live':outputs[name]=q
        window,pre,post=parameters[n]
        ff=fft_analysis(pcm,before,window,pre,post)
        row=dict(id=name,n=n,kind=c['kind'],nonzero=bool(np.any(q!=0)),
                 ideal_common_grid=score(q,direct_analysis(pcm,before,window)),
                 naive_local_width=score(q,direct_analysis(pcm,before,window,'local_width')),
                 naive_fixed64=score(q,direct_analysis(pcm,before,window,'fixed64')),
                 captured_tables=score(q,ff['qmf']))
        if c['kind']=='live':
            replay=floats(data/f'{name}.native_replay.f32',(slots,n,2))
            if not bit_equal(q,replay):raise ValueError("同状态参考重放不一致")
            after=floats(data/f'{name}.after.f32',(9,n))
            if not bit_equal(after,pcm[-9:][::-1].copy()):raise ValueError("逻辑历史不是最近九个输入块")
            filtered=floats(data/f'{name}.filtered.f32',(slots,n,2));fft=floats(data/f'{name}.fft.f32',(slots,n,2))
            row['filter_stage']=score(filtered,ff['filtered'])
            row['fft_stage']=score(fft,np.fft.fft(complex_values(filtered),axis=1))
            row['post_stage']=score(q,postprocess(complex_values(fft),post))
        else:
            if np.any(before!=0) or not bit_equal(pcm,probe_input(c)):
                raise ValueError("构造输入或初始状态不符")
        results.append(row)
    links=[];lfes=[];used_contexts=set()
    for scope in m['scopes']:
        first,last=scope['first_au'],scope['last_au']
        integer(first,1,1000000)
        integer(last,first+1,first+1)
        integer(scope['delay'],6,6)
        integer(scope['slots_per_au'],32,32)
        if last!=first+1 or scope['delay']!=6 or scope['slots_per_au']!=32:
            raise ValueError("实际区间或表 192 延迟错误")
        contexts=[known[x] for x in scope['contexts']]
        if sorted(c['n'] for c in contexts)!=[8]+[48]*9 or len(set(scope['contexts']))!=10:
            raise ValueError("实际上下文覆盖不完整")
        for c in contexts:
            if c['kind']!='live' or c['id'] in used_contexts or c['aus']!=[first]*32+[last]*32:
                raise ValueError("AU 时序或上下文归属错误")
            used_contexts.add(c['id'])
        if len(scope['links'])!=9 or sorted(x['call'] for x in scope['links'])!=list(range(9)):
            raise ValueError("缺失或重复的 A-SPX 调用")
        owners=set()
        for link in scope['links']:
            name=valid_id(link['id']);band=integer(link['bands'],1,48)
            if len(link['tokens'])!=38:raise ValueError("低带布局长度错误")
            for token in link['tokens']:integer(token,0,1000000)
            owner,indices=find_owner(contexts,link['tokens'][6:],last)
            if owner in owners or known[owner]['n']!=48 or indices!=list(range(32,64)):
                raise ValueError("低带所有者或当前 AU 区间错误")
            owners.add(owner)
            target=floats(data/f'{name}.low.f32',(38,band,2));source=outputs[owner][26:64,:band].copy()
            s=metrics(target,source)
            links.append(dict(scope=scope['id'],call=link['call'],owner=owner,delay=6,bands=band,metrics=s))
        source8=[c for c in contexts if c['n']==8]
        lfe=valid_id(scope['lfe_id']);target=floats(data/f'{lfe}.terminal.f32',(64,8,2))
        # The complete target AU is compared; the preceding AU supplies the six carried slots.
        s=metrics(target[32:].copy(),outputs[source8[0]['id']][26:58].copy())
        lfes.append(dict(scope=scope['id'],source=source8[0]['id'],delay=6,metrics=s))
    if used_contexts!={c['id'] for c in m['cases'] if c['kind']=='live'}:
        raise ValueError("遗漏实际上下文")
    tables=[]
    for n,(window,pre,post) in parameters.items():
        pi,po=ideal_phases(n)
        tables.append(dict(n=n,pre_max_abs_error=float(abs(pre-pi).max()),
            pre_max_phase_error_radians=float(abs(np.angle(pre/pi)).max()),
            post_max_abs_error=float(abs(post-po).max()),
            post_max_phase_error_radians=float(abs(np.angle(post/po)).max())))
    window_checks={}
    if all(n in parameters for n in WIDTHS):
        w64=parameters[64][0]
        window_checks=dict(n8_stride8_bit_equal=bit_equal(parameters[8][0],w64[::8].copy()),
                           n48_stride3_to64_stride4_bit_equal=bit_equal(parameters[48][0][::3].copy(),w64[::4].copy()))
    grids=compare_grids(m.get('grid_cases',[]),data)
    return dict(schema='macinac4.qmf-analysis-checkpoint-comparison',version=1,status='observation_only',
                phase_gain_or_delay_fitting=False,mode=8,rotation='C_N(k)=-exp(j*pi*(k+0.5)*(1/N-1/128))',
                analysis_scale='1/(2*N)',cases=results,aspx_links=links,lfe_links=lfes,
                phase_tables=tables,window_common_grid=window_checks,common_waveform_grid=grids)


def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('manifest',type=Path);p.add_argument('data',type=Path);p.add_argument('--output',required=True,type=Path);args=p.parse_args(argv)
    try:
        result=compare(args.manifest,args.data)
        with args.output.open('x') as f:json.dump(result,f,ensure_ascii=False,indent=2,allow_nan=False);f.write('\n')
    except (OSError,ValueError,KeyError,TypeError) as e:
        print(f'分析检查点校验失败：{e}',file=sys.stderr);return 2
    print(json.dumps({'cases':len(result['cases']),'aspx_links':len(result['aspx_links']),'lfe_links':len(result['lfe_links'])}));return 0


if __name__=='__main__':raise SystemExit(main())
