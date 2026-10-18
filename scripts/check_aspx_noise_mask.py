#!/usr/bin/env python3
"""固定增益与游标核对注噪条件；参考原语、生产诊断和真实边界分别计分。"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import sys
import numpy as np
if __package__:
    from .check_aspx_basis_replay import metrics
    from .check_aspx_noise_replay import floats, integer
    from .check_asf_imdct_checkpoints import identical, identifier
else:
    from check_aspx_basis_replay import metrics
    from check_aspx_noise_replay import floats, integer
    from check_asf_imdct_checkpoints import identical, identifier

RULES=('literal','onset_only','sine_only','both','whole_envelope')
SINE=np.array([[1,0],[0,1],[-1,0],[0,-1]],dtype='<f4')


def expected_cases():
    cases=[]
    def add(kind,config,lo,n,factor,envs,mask,pattern,level,noise=1.,qmode=0):
        cases.append(dict(id=f'm{len(cases):04d}',kind=kind,config=config,sbx=lo,subbands=n,factor=factor,
                          envelopes=envs,onset_mask=mask,pattern=pattern,sine_level=level,noise_level=noise,
                          qmode=qmode,noise_seed=0,tone_seed=1,signal_gain=1.25))
    for config,lo,n in [([1,6,1,3,0],36,20),([1,6,0,3,5],47,15)]:
        for factor in [1,2]:
            for envs in [1,2,4]:
                for mask in sorted({0,1,1<<(envs-1),10&((1<<envs)-1),(1<<envs)-1}):
                    for pattern in range(7):
                        for level in [2**-24,1.,4.]:add('grid',config,lo,n,factor,envs,mask,pattern,level)
            for qmode,noise in [(0,0.),(1,0.),(1,1.)]:add('control',config,lo,n,factor,4,10,6,1.,noise,qmode)
    return cases


def case_lines(cases):
    return [' '.join(map(str,[c['id'],*c['config'],c['sbx'],c['subbands'],c['factor'],c['envelopes'],
             c['onset_mask'],c['pattern'],c['sine_level'],c['noise_level'],c['qmode']])) for c in cases]


def validate_manifest(m):
    if m.get('schema')!='macinac4.noise-mask-replay' or type(m.get('version')) is not int or m['version']!=1:
        raise ValueError('协议版本错误')
    if m['cases']!=expected_cases():raise ValueError('固定构造覆盖缺失或配方改变')
    runs=m['reference_runs']
    if len(runs)!=12 or len(set(runs))!=12:raise ValueError('独立进程数不完整')
    for name in runs:identifier(name)
    ids=set();groups={}
    for c in m['live']:
        identifier(c['id']);integer(c['call'],0,8);integer(c['env'],0,4)
        if c['id']!=f"{c['scope']}c{c['call']}e{c['env']}":raise ValueError('真实身份与声明不符')
        if c['id'] in ids:raise ValueError('重复真实包络')
        ids.add(c['id']);groups.setdefault((c['scope'],c['call']),[]).append(c)
        a,b=integer(c['first'],0,37),integer(c['last'],1,38)
        lo,n=integer(c['sbx'],0,63),integer(c['subbands'],1,44)
        g=np.asarray(c['gains'],dtype='<f4')
        if a>=b or a%2 or b%2 or lo+n>64 or type(c['onset']) is not bool or g.shape!=(n,3) or not np.isfinite(g).all() or np.any(g<0):
            raise ValueError('真实包络布局或增益错误')
        for k,limit in [('noise_before',511),('noise_after',511),('tone_before',3),('tone_after',3)]:integer(c[k],0,limit)
        if c['noise_after']!=(c['noise_before']+(b-a)*n)%512 or c['tone_after']!=(c['tone_before']+b-a)%4:
            raise ValueError('真实结束游标错误')
    counts={'tone':9,'music100':9,'music33':9,'odd2':2,'odd8':2,'even8':9}
    if set(groups)!={(s,c) for s,n in counts.items() for c in range(n)} or len(m['live'])!=67:
        raise ValueError('真实调用覆盖不完整')
    for rows in groups.values():
        if [x['env'] for x in rows]!=list(range(len(rows))):raise ValueError('真实包络错位')
        for a,b in zip(rows,rows[1:]):
            if a['last']!=b['first'] or a['noise_after']!=b['noise_before'] or a['tone_after']!=b['tone_before']:
                raise ValueError('真实包络或状态不连续')
    if len(m['carry'])!=2 or {c['call'] for c in m['carry']}!={0,1}:raise ValueError('携带检查覆盖缺失')
    for c in m['carry']:
        identifier(c['id'])
        if c['id']!=f"carry{c['call']}":raise ValueError('携带身份与调用不符')
        if (c['from_au'],c['to_au'],c['sbx'],c['subbands'],c['source_slots'],c['destination_slots'])!=(2,3,34,28,[32,34],[0,2]):
            raise ValueError('携带检查点错位')
        if [x['run'] for x in c['replicates']]!=[1,2]:raise ValueError('携带复捕不完整')
        for r in c['replicates']:
            integer(r['tone_next'],0,3);integer(r['noise_last'],0,511)
            source=groups['odd2',c['call']][-1]
            if (r['tone_next'],r['noise_last'])!=(source['tone_after'],source['noise_after']) or (r['destination_tone_next'],r['destination_noise_last'])!=(r['tone_next'],r['noise_last']):
                raise ValueError('跨帧携带游标不连续')
        if (c['replicates'][0]['tone_next'],c['replicates'][0]['noise_last'])!=(c['replicates'][1]['tone_next'],c['replicates'][1]['noise_last']):
            raise ValueError('携带游标不重复')
    if len(m['iwc_live'])!=40 or {(c['scope'],c['call']) for c in m['iwc_live']}!=set(groups):raise ValueError('交织出口缺失')
    for c in m['iwc_live']:
        identifier(c['id']);source=groups[c['scope'],c['call']]
        if c['id']!=f"{c['scope']}c{c['call']}":raise ValueError('交织身份与调用不符')
        if (c['first'],c['last'],c['sbx'],c['subbands'])!=(source[0]['first'],source[-1]['last'],source[0]['sbx'],source[0]['subbands']):
            raise ValueError('交织出口与包络来源不符')
        if type(c['fic']) is not bool or type(c['tic']) is not bool or c['fic'] or c['tic']:
            raise ValueError('本轮真实交织控制超出已验证范围')
    if [c['mode'] for c in m['iwc_controls']]!=[0,1,2,3]:raise ValueError('缺少交织正向控制')
    for c in m['iwc_controls']:
        if (c['first'],c['last'],c['tic_first'],c['tic_last'])!=(0,32,14,16) or not 0<=c['sbx']<c['fic_band_end']<=c['sbx']+c['subbands']<=64:
            raise ValueError('交织控制布局错误')


def sine_levels(c,e):
    b=np.arange(c['sbx'],c['sbx']+c['subbands']);p=c['pattern']
    active={0:np.zeros(len(b),bool),1:np.ones(len(b),bool),2:b%2==0,3:b%2==1,
            4:b==b[0],5:b==b[-1],6:(b-b[0]+e)%2==0}[p]
    return active.astype('<f4')*np.float32(c['sine_level'])


def noise_enabled(onset,sine,rule):
    if rule not in RULES:raise ValueError('未知注噪规则')
    if rule=='literal':return np.ones(sine.shape,bool)
    if rule=='onset_only':return np.full(sine.shape,not onset)
    if rule=='sine_only':return sine==0
    if rule=='whole_envelope':return np.full(sine.shape,not onset and not np.any(sine))
    return (sine==0)&(not onset)


def render(hf,gains,table,lo,noise_before,tone_before,onset,rule):
    if hf.ndim!=3 or hf.shape[-1]!=2 or not hf.size or not np.isfinite(hf).all():raise ValueError('HF 无效')
    L,n,_=hf.shape
    if gains.shape!=(n,3) or table.shape!=(512,2) or lo<0 or lo+n>64 or not np.isfinite(gains).all() or np.any(gains<0) or not np.isfinite(table).all():
        raise ValueError('生成电平或表无效')
    integer(noise_before,0,511);integer(tone_before,0,3)
    enabled=noise_enabled(onset,gains[:,2],rule)
    noise=table[(noise_before+np.arange(L*n)+1)%512].reshape(L,n,2)*gains[:,1][None,:,None]
    noise[:,~enabled]=0
    tone=SINE[(tone_before+np.arange(L))%4,None,:]*gains[:,2][None,:,None]
    tone[:,:,1]*=np.where(np.arange(lo,lo+n)%2,-1,1).astype('<f4')[None,:]
    out=hf*gains[:,0][None,:,None]
    out+=noise;out+=tone
    return out


def synthetic(c,table,rule):
    lo,n,L=c['sbx'],c['subbands'],16*c['factor'];b=np.arange(lo,lo+n);t=np.arange(L)[:,None]
    hf=np.zeros((L,n,2),'<f4');out=np.zeros_like(hf)
    if not c['qmode']:
        hf[:,:,0]=.125+(t%5-2)*.03125+b[None,:]*.00390625
        hf[:,:,1]=(b%3-1)[None,:]*.0625
    for e in range(c['envelopes']):
        a,z=L*e//c['envelopes'],L*(e+1)//c['envelopes']
        g=np.stack((np.full(n,c['signal_gain']),np.full(n,c['noise_level']),sine_levels(c,e)),axis=-1).astype('<f4')
        out[a:z]=render(hf[a:z],g,table,lo,(c['noise_seed']+a*n)%512,(c['tone_seed']+a)%4,bool(c['onset_mask']>>e&1),rule)
    return out


def validate_states(rows,cases):
    if len(rows)!=len(cases)+1 or rows[-1]!={'complete':True,'cases':len(cases)} or [r['id'] for r in rows[:-1]]!=[c['id'] for c in cases]:
        raise ValueError('参考完成状态或案例身份错误')
    for r,c in zip(rows[:-1],cases,strict=True):
        if not r['finite'] or not r['guards'] or r['save']!=0 or len(r['envelopes'])!=c['envelopes']:raise ValueError('参考原语未完整结束')
        L,n=16*c['factor'],c['subbands']
        for e,s in enumerate(r['envelopes']):
            a,b=L*e//c['envelopes'],L*(e+1)//c['envelopes']
            expected=dict(env=e,first=a,last=b,onset=(c['onset_mask']>>e)&1,present=int(np.any(sine_levels(c,e))),
                          noise_before=a*n%512,noise_after=b*n%512,tone_before=(1+a)%4,tone_after=(1+b)%4)
            if s!=expected:raise ValueError('参考包络、注噪控制或游标错位')


def iwc_control_output(c,q,wcc):
    mode=integer(c['mode'],0,3);lo=c['sbx'];hi=lo+c['subbands'];end=c['fic_band_end']
    if q.shape!=(40,64,2) or wcc.shape!=(40,44,2) or not np.isfinite(q).all() or not np.isfinite(wcc).all() or not 0<=lo<end<=hi<=64 or hi-lo>44:
        raise ValueError('交织控制输入无效')
    out=q.copy()
    if mode&1:out[:32,lo:end]+=wcc[:32,:end-lo]
    if mode&2:out[14:16,lo:hi]=wcc[14:16,:hi-lo]
    return out


def compare(manifest,parameters,references,local,table,real,carry,iwc):
    m=json.loads(manifest.read_text());validate_manifest(m);cases=m['cases'];runs=m['reference_runs']
    if parameters.read_text().splitlines()!=case_lines(cases):raise ValueError('重放输入参数与配方不符')
    table=floats(table,(512,2));log=(references/runs[0]/'states.json').read_bytes();validate_states(json.loads(log),cases)
    for run in runs[1:]:
        if (references/run/'states.json').read_bytes()!=log:raise ValueError('参考结束状态不重复')
    states={}
    for line in (local/'states.txt').read_text().splitlines():
        name,n,t=line.split()
        if name in states:raise ValueError('重复本地状态')
        states[name]=(int(n),int(t))
    if set(states)!={c['id'] for c in cases}:raise ValueError('本地状态缺失')
    results=[]
    for c in cases:
        name=c['id'];L,n,lo=16*c['factor'],c['subbands'],c['sbx']
        reference=floats(references/runs[0]/f'{name}.f32',(L,n,2))
        for run in runs[1:]:
            if not identical(reference,floats(references/run/f'{name}.f32',(L,n,2))):raise ValueError('参考波形不重复')
        hypotheses={rule:metrics(reference,synthetic(c,table,rule)) for rule in RULES}
        outputs={}
        for mode,rule in [('literal','literal'),('masked','both')]:
            data=floats(local/f'{name}.{mode}.f32',(L,64,2));valid=data[:,lo:lo+n].copy();data[:,lo:lo+n]=0
            if np.any(data):raise ValueError('生产重放写出了目标子带范围')
            if not identical(valid,synthetic(c,table,rule)):raise ValueError('生产原语重放与声明的诊断模式不符')
            outputs[mode]=metrics(reference,valid)
        if states[name]!=(L*n%512,(c['tone_seed']+L-1)%4):raise ValueError('本地结束游标错误')
        results.append(dict(id=name,kind=c['kind'],hypotheses=hypotheses,production=outputs))
    live=[]
    for c in m['live']:
        name=c['id'];shape=(c['last']-c['first'],c['subbands'],2)
        hf=floats(real/f'{name}.hf.f32',shape);reference=floats(real/f'{name}.reference.f32',shape);g=np.asarray(c['gains'],dtype='<f4')
        scores={rule:metrics(reference,render(hf,g,table,c['sbx'],c['noise_before'],c['tone_before'],c['onset'],rule)) for rule in RULES}
        live.append(dict(id=name,scope=c['scope'],call=c['call'],env=c['env'],first=c['first'],last=c['last'],onset=c['onset'],
                         sine_bands=(np.flatnonzero(g[:,2])+c['sbx']).tolist(),hypotheses=scores))
    carried=[]
    for c in m['carry']:
        for run in [1,2]:
            name=f"{c['id']}r{run}";a=floats(carry/f'{name}.tail.f32',(2,28,2));b=floats(carry/f'{name}.head.f32',(2,28,2))
            if not identical(a,b):raise ValueError('真实跨帧携带不匹配')
            carried.append(dict(id=name,metrics=metrics(a,b)))
    interleaved=[]
    for c in m['iwc_live']:
        data={key:floats(iwc/'live'/f"{c['id']}.{key}.f32",(38,64,2)) for key in ['before','after','replay','perturbed_input','perturbed_output']}
        if not identical(data['after'],data['replay']):raise ValueError('交织原语未复现实际出口')
        expected=data['before'].copy();lo,hi=c['sbx'],c['sbx']+c['subbands']
        for r in m['live']:
            if (r['scope'],r['call'])==(c['scope'],c['call']) and r['onset']:
                expected[r['first']:r['last'],lo:hi,0]+=np.float32(1)
                expected[r['first']:r['last'],lo:hi,1]-=np.float32(.5)
        if not identical(expected,data['perturbed_input']):raise ValueError('交织扰动输入不符合声明')
        changed=int(np.count_nonzero(expected!=data['before']))
        if changed!=c['perturbed_components']:raise ValueError('交织扰动不可观察或数量不符')
        if not identical(data['before'],data['after']) or not identical(data['perturbed_input'],data['perturbed_output']):
            raise ValueError('交织出口改变了声明为保留的输入')
        interleaved.append(dict(id=c['id'],identity=metrics(data['before'],data['after']),perturbed_components=changed,
                               perturbation_preserved=metrics(data['perturbed_input'],data['perturbed_output'])))
    controls=[];fixed_input=None;fixed_wcc=None
    for c in m['iwc_controls']:
        mode=c['mode'];name=f'control{mode}'
        data={key:floats(iwc/'control1'/f'{name}.{key}.f32',(40,width,2)) for key,width in [('input',64),('output',64),('wcc',44)]}
        for key,width in [('input',64),('output',64),('wcc',44)]:
            if not identical(data[key],floats(iwc/'control2'/f'{name}.{key}.f32',(40,width,2))):raise ValueError('交织正向控制不重复')
        if fixed_input is None:fixed_input=data['input'];fixed_wcc=data['wcc']
        if not identical(fixed_input,data['input']) or not identical(fixed_wcc,data['wcc']):raise ValueError('切换交织控制时改变了输入')
        expected=iwc_control_output(c,data['input'],data['wcc'])
        if not identical(expected,data['output']) or (identical(data['input'],data['output'])!=(mode==0)):
            raise ValueError('交织正向控制没有验证加法与替换')
        controls.append(dict(mode=mode,expected_output=metrics(expected,data['output']),changed_components=int(np.count_nonzero(data['input']!=data['output']))))
    return dict(schema='macinac4.noise-mask-comparison',version=1,status='observation_only',phase_gain_or_delay_fitting=False,
                reference_processes=len(runs),cases=results,live=live,carry=carried,iwc=interleaved,iwc_controls=controls)


def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['manifest','parameters','references','local','table','real','carry','iwc']:p.add_argument(name,type=Path)
    p.add_argument('--output',type=Path,required=True);a=p.parse_args(argv)
    try:
        r=compare(a.manifest,a.parameters,a.references,a.local,a.table,a.real,a.carry,a.iwc)
        with a.output.open('x') as f:json.dump(r,f,ensure_ascii=False,indent=2,allow_nan=False);f.write('\n')
    except (OSError,ValueError,KeyError,TypeError) as e:
        print(f'注噪对照失败：{e}',file=sys.stderr);return 2
    print(json.dumps(dict(cases=len(r['cases']),live_envelopes=len(r['live']),reference_processes=r['reference_processes'])));return 0


if __name__=='__main__':raise SystemExit(main())
