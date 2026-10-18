#!/usr/bin/env python3
"""固定 QMF、历史和控制状态的 HF/TNA 对照；不修改生产、不拟合参考输出。"""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import struct
import sys
import numpy as np
if __package__:
    from .check_aspx_basis_replay import metrics, complex_values
    from .check_aspx_noise_replay import floats, integer
    from .check_asf_imdct_checkpoints import identical, identifier
else:
    from check_aspx_basis_replay import metrics, complex_values
    from check_aspx_noise_replay import floats, integer
    from check_asf_imdct_checkpoints import identical, identifier

EXPONENTS = (-50, -30, -10, 0, 10, 20)
PREFLAT_PATTERNS = ('flat', 'rising', 'falling', 'alternating')
TNA_PATTERNS = ('zero', 'random', 'tone', 'two', 'near4', 'near8', 'near12', 'near16', 'impulse')


def parameter_bytes(r):
    pad = lambda values, size: list(values)+[0]*(size-len(values))
    words = [r[k] for k in ('first','last','ats','factor','preflat','master','start','stop','noise','xover',
                            'groups','patches','source_lo','source_hi','sbx','sbz')]
    words += pad(r['noise_borders'],6)+pad(r['targets'],6)+pad(r['sources'],5)
    return struct.pack('<33I4x2Q5f4x', *words, r['modes'], r['previous_modes'], *pad(r['previous_chirp'],5))


def validate_case(r):
    identifier(r['id'])
    first, last = integer(r['first'],0,37), integer(r['last'],1,38)
    if r['ats'] != 16 or r['factor'] != 2 or first >= last or first % 2 or last % 2:
        raise ValueError('区间或时间单位错误')
    integer(r['preflat'],0,1); integer(r['master'],0,1)
    integer(r['start'],0,7); integer(r['stop'],0,3); integer(r['noise'],0,3)
    integer(r['xover'],0,255)
    if r['xover'] not in (*range(8),255):
        raise ValueError('交叉偏移错误')
    groups, patches = integer(r['groups'],1,5), integer(r['patches'],1,5)
    sbx, sbz = integer(r['sbx'],4,40), integer(r['sbz'],5,64)
    integer(r['source_lo'],0,39); integer(r['source_hi'],1,40)
    if len(r['noise_borders']) != groups+1 or len(r['targets']) != patches+1 or len(r['sources']) != patches:
        raise ValueError('表长错误')
    for values in (r['noise_borders'],r['targets']):
        if values[0] != sbx or values[-1] != sbz:
            raise ValueError('频带端点错误')
        for a,b in zip(values,values[1:]):
            integer(a,0,64); integer(b,0,64)
            if a >= b:
                raise ValueError('频带不递增')
    for source,a,b in zip(r['sources'],r['targets'][:-1],r['targets'][1:],strict=True):
        integer(source,0,63)
        if source+b-a > sbx:
            raise ValueError('源越出低带')
    if r['source_lo'] != min(r['sources']) or r['source_hi'] != max(s+b-a for s,a,b in zip(r['sources'],r['targets'][:-1],r['targets'][1:])):
        raise ValueError('预测源区间错误')
    integer(r['modes'],0,2**64-1); integer(r['previous_modes'],0,2**64-1)
    if len(r['previous_chirp']) != groups or not all(np.isfinite(x) and 0 <= x <= 1 for x in r['previous_chirp']):
        raise ValueError('chirp 历史错误')


def validate_manifest(m):
    if m.get('schema') != 'macinac4.normalized-hf-replay' or type(m.get('version')) is not int or m['version'] != 1:
        raise ValueError('协议版本错误')
    ids, coverage = set(), set()
    for r in m['cases']:
        validate_case(r)
        if r['id'] in ids:
            raise ValueError('重复案例')
        ids.add(r['id'])
        if r['kind'] == 'live':
            integer(r['call'],0,8)
            key = r['kind'],r['scope'],r['call']
        else:
            if r['amplitude_exp'] not in EXPONENTS:
                raise ValueError('未声明的幅度')
            if r['kind'] == 'preflat_probe':
                key = r['kind'],r['pattern'],r['amplitude_exp'],r['first'],r['last']
                if r['modes'] or r['previous_modes'] or any(r['previous_chirp']):
                    raise ValueError('预平坦化探针没有隔离 TNA')
            elif r['kind'] == 'tna_probe' and (r['first'],r['last']) == (0,38):
                key = r['kind'],r['pattern'],r['amplitude_exp']
                pack=lambda value:sum(value << (62-2*g) for g in range(r['groups']))
                if r['modes']!=pack(2) or r['previous_modes']!=pack(1) or not np.array_equal(np.asarray(r['previous_chirp'],dtype='<f4'),np.full(r['groups'],.61,dtype='<f4')):
                    raise ValueError('TNA 探针控制状态不符')
            else:
                raise ValueError('未知构造类型')
        if key in coverage:
            raise ValueError('重复配置')
        coverage.add(key)
    expected = {('live',s,c) for s in ('tone','music100','music33') for c in range(9)}
    expected |= {('preflat_probe',p,e,f,l) for p in PREFLAT_PATTERNS for e in EXPONENTS for f,l in ((0,32),(2,38))}
    expected |= {('tna_probe',p,e) for p in TNA_PATTERNS for e in EXPONENTS}
    if coverage != expected:
        raise ValueError('缺失实验覆盖')
    runs = m['reference_runs']
    if len(runs) != 4 or len(set(runs)) != 4:
        raise ValueError('参考进程覆盖错误')
    for name in runs:
        identifier(name)
    levels = [np.float32(0), np.nextafter(np.float32(1/16),np.float32(0)), np.float32(1/16),
              np.nextafter(np.float32(1/16),np.float32(1)), np.float32(.6), np.float32(.75), np.float32(.98)]
    expected_chirp = {(c,p,int(v.view('u4'))) for c in range(4) for p in range(4) for v in levels}
    actual_chirp = set()
    for r in m['chirp_cases']:
        name=identifier(r['id'])
        if name in ids:
            raise ValueError('重复原语身份')
        ids.add(name)
        key=(integer(r['current'],0,3),integer(r['previous_mode'],0,3),integer(r['previous_bits'],0,2**32-1))
        if key in actual_chirp:
            raise ValueError('重复 chirp 条件')
        actual_chirp.add(key)
    if actual_chirp != expected_chirp:
        raise ValueError('chirp 状态覆盖不完整')
    expected_solver = {f'srank{i}' for i in range(6)} | {f'sclamp{i}' for i in range(5)}
    expected_solver |= {f'sden{i}' for i in range(3)} | {f'sdiag{i}' for i in range(3)} | {'szero'}
    actual_solver=set()
    for r in m['solver_cases']:
        name=identifier(r['id'])
        if name in ids:
            raise ValueError('重复原语身份')
        ids.add(name);actual_solver.add(name)
        if len(r['covariance'])!=9 or not np.isfinite(r['covariance']).all():
            raise ValueError('构造协方差无效')
    if actual_solver != expected_solver:
        raise ValueError('解算器边界覆盖不完整')


def synthetic(r):
    n = r['sbx'];t = np.arange(42)[:,None];b = np.arange(n)[None,:];p = r['pattern']
    if r['kind'] == 'preflat_probe':
        amplitudes = {'flat':np.ones((1,n)), 'rising':2.**((b-(n-1)/2)/3),
                      'falling':2.**(-(b-(n-1)/2)/3), 'alternating':2.**np.where(b%2,5,-5)}
        z = amplitudes[p]*np.exp(1j*(t+2)*(b+1)*.073)
    elif p == 'zero':
        z = np.zeros((42,n),complex)
    elif p == 'random':
        random = np.empty((42,n,2));state = 847012
        for index in np.ndindex(random.shape):
            state = (1664525*state+1013904223) & 0xffffffff
            random[index] = ((state >> 8)/2**24-.5)*.25
        z = random[:,:,0]+1j*random[:,:,1]
    elif p == 'impulse':
        z = np.zeros((42,n),complex)
        for b in range(n):
            z[8+b%11,b] = 1+.5j
    else:
        ratio = 1 if p == 'two' else (2.**(-int(p[4:])) if p.startswith('near') else 0)
        z = (1+.02*b)*np.exp(1j*(.21*t+.03*b))+ratio*(1+.02*b)*np.exp(1j*(.83*t-.05*b))
    z *= 2.**r['amplitude_exp']
    q = np.zeros((40,64,2),'<f4');h = np.zeros((4,40,2),'<f4')
    q[:38,:n,0],q[:38,:n,1] = z[4:].real,z[4:].imag
    h[:,:n,0],h[:,:n,1] = z[:4].real,z[:4].imag
    return q,h


def covariance(low, history):
    z = np.concatenate((complex_values(history),complex_values(low[:38,:40])))
    ts = np.arange(4,42,2)
    get = lambda i,j: np.sum(z[ts-2*i]*np.conj(z[ts-2*j]),axis=0)
    c01,c02,c11,c12,c22 = get(0,1),get(0,2),get(1,1),get(1,2),get(2,2)
    return np.stack((c01.real,c01.imag,c02.real,c02.imag,c11.real,c12.real,c12.imag,c22.real),axis=-1)


GAIN_CONVENTIONS = ('mean_minus_slope', 'slope_minus_mean')


def read_gain_convention(local):
    path = local / 'replay_metadata.json'
    if not path.exists():
        return 'mean_minus_slope'
    value = json.loads(path.read_text())
    if (not isinstance(value, dict)
            or set(value) != {'schema', 'version', 'preflatten_gain_convention'}
            or value['schema'] != 'macinac4.hf-replay'
            or type(value['version']) is not int or value['version'] != 1
            or value['preflatten_gain_convention'] not in GAIN_CONVENTIONS):
        raise ValueError('无效 HF 重放增益约定')
    return value['preflatten_gain_convention']


def gain_model(low, sbx, first, last, scale, convention='mean_minus_slope'):
    if convention not in GAIN_CONVENTIONS:
        raise ValueError('未知预平坦化增益约定')
    power = np.mean(np.sum(low[first:last,:sbx].astype(float)**2,axis=-1),axis=0)*scale**2
    energy = 10*np.log10(power+1)
    x = np.linspace(-1,1,sbx)
    design = np.vander(x,4,increasing=True)
    fit = design @ np.linalg.lstsq(design,energy,rcond=None)[0]
    difference = energy.mean()-fit if convention == 'mean_minus_slope' else fit-energy.mean()
    return (10**(difference/20)).astype('<f4')


def checked_gain_metrics(actual, expected):
    # 保留历史报告的指标方向；约定标签改变时也不能重释旧数据。
    result = metrics(actual, expected)
    if np.any(actual <= 0) or not np.allclose(actual, expected, rtol=2e-6, atol=0):
        raise ValueError('生产增益与声明的约定不符')
    return result


def regularized_solver(values):
    c01r,c01i,c02r,c02i,c11,c12r,c12i,c22 = map(float,values[:8])
    d = c22*c11-(c12r*c12r+c12i*c12i)*(1/(1+2**-20))
    a1r = ((c01r*c12r-c01i*c12i)-c02r*c11)/d if d else 0.
    a1i = ((c01i*c12r+c01r*c12i)-c02i*c11)/d if d else 0.
    a0r = ((0.-c01r)-(a1r*c12r+a1i*c12i))/c11 if c11 else 0.
    a0i = ((0.-c01i)-(a1i*c12r-a1r*c12i))/c11 if c11 else 0.
    if a0r*a0r+a0i*a0i >= 16 or a1r*a1r+a1i*a1i >= 16:
        a0r=a0i=a1r=a1i=0.
    return np.array([a0r,a0i,a1r,a1i],dtype='<f4')


def slope(values, bands):
    power = np.mean(np.sum(values.astype(float)**2,axis=-1),axis=0)
    if np.any(power <= 0):
        raise ValueError('斜率探针没有完整激励')
    return float(np.polyfit(bands,10*np.log10(power),1)[0])


def completed_rows(rows, ids):
    if not rows or rows[-1] != {'complete':True,'cases':len(ids)} or [x['id'] for x in rows[:-1]] != ids:
        raise ValueError('参考重放未完整结束或案例错位')
    if not all(x['finite'] is True and x['save']==0 for x in rows[:-1]):
        raise ValueError('参考重放数据无效或未保存')
    return rows[:-1]


def compare(manifest, data, references, local):
    m=json.loads(manifest.read_text());validate_manifest(m)
    gain_convention = read_gain_convention(local)
    dirs=[references/identifier(x) for x in m['reference_runs']]
    ids=[r['id'] for r in m['cases']]
    if (data/'hf_cases.txt').read_text().split() != ids:
        raise ValueError('实际重放列表不符')
    chirp_lines=[f"{r['id']} {r['current']} {r['previous_mode']} {r['previous_bits']}" for r in m['chirp_cases']]
    if (data/'chirp_cases.txt').read_text().splitlines()!=chirp_lines or (data/'solver_cases.txt').read_text().split()!=[r['id'] for r in m['solver_cases']]:
        raise ValueError('实际原语控制列表不符')
    for directory in dirs:
        rows=[json.loads(s) for s in (directory/'hf_probe_results.jsonl').read_text().splitlines()]
        rows=completed_rows(rows,ids)
        if not all(x['captured_bit_differences']==x['history_bit_differences']==x['state_bit_differences']==x['normalization_bit_differences']==0 for x in rows):
            raise ValueError('参考重放门禁失败')
        for filename,key in [('chirp_probes.jsonl','chirp_cases'),('solver_probes.jsonl','solver_cases')]:
            rows=[json.loads(s) for s in (directory/filename).read_text().splitlines()]
            completed_rows(rows,[r['id'] for r in m[key]])
    result=[];band_results=[]
    for r in m['cases']:
        name=r['id'];first,last,lo,hi=r['first'],r['last'],r['sbx'],r['sbz'];sources=slice(r['source_lo'],r['source_hi'])
        if (data/f'{name}.param').read_bytes() != parameter_bytes(r):
            raise ValueError('参数与清单不一致')
        q=floats(data/f'{name}.qmf.f32',(40,64,2));h=floats(data/f'{name}.history.f32',(4,40,2))
        if r['kind'] != 'live':
            expected_q,expected_h=synthetic(r)
            if not identical(q,expected_q) or not identical(h,expected_h):
                raise ValueError('构造输入不符')
        arrays={}
        shapes={'reference0':(40,64,2),'reference1':(40,64,2),'alpha':(64,4),'cov':(64,9),
                'cov_math':(64,9),'alpha_math_cov':(64,4),'chirp':(5,),
                'history_after':(4,40,2),'chirp_after':(5,)}
        for suffix,shape in shapes.items():
            a=floats(dirs[0]/f'{name}.{suffix}.f32',shape)
            for directory in dirs[1:]:
                if not identical(a,floats(directory/f'{name}.{suffix}.f32',shape)):
                    raise ValueError(f'独立参考进程不重复：{name}/{suffix}')
            arrays[suffix]=a
        if not identical(arrays['history_after'][:,:lo].copy(),q[28:32,:lo].copy()):
            raise ValueError('参考低带历史更新错误')
        groups=r['groups'];current=np.array([(r['modes']>>(62-2*g))&3 for g in range(groups)],dtype='u1')
        for directory in dirs:
            if (directory/f'{name}.modes_after.u64').read_bytes() != struct.pack('<Q',r['modes']):
                raise ValueError('参考模式更新错误')
        local_chirp=floats(local/f'{name}.chirp.f32',(groups,));local_chirp_after=floats(local/f'{name}.chirp_after.f32',(5,))
        local_modes=(local/f'{name}.modes_after.u8').read_bytes()
        if len(local_modes)!=5 or not identical(local_chirp_after[:groups].copy(),local_chirp) or local_modes[:groups] != current.tobytes():
            raise ValueError('本地 chirp 末态错误')
        if not identical(arrays['chirp_after'][:groups].copy(),arrays['chirp'][:groups].copy()):
            raise ValueError('参考 chirp 末态错误')
        for mode in ('raw','scaled'):
            lh=floats(local/f'{name}.{mode}.history.f32',(4,64,2))
            if not identical(lh[:,:lo].copy(),q[28:32,:lo].copy()):
                raise ValueError('本地低带历史更新错误')
        cov_path=local/f'{name}.cov.f64'
        if cov_path.stat().st_size != 64*8*8:
            raise ValueError('本地协方差矩阵长度错误')
        raw_cov=np.fromfile(cov_path,dtype='<f8')
        if raw_cov.size != 64*8 or not np.isfinite(raw_cov).all():
            raise ValueError('本地协方差矩阵无效')
        raw_cov=raw_cov.reshape(64,8)
        independent=covariance(q,h)
        extent=max(float(abs(independent[sources]).max()),np.finfo(float).tiny)
        if np.max(abs(raw_cov[sources]-independent[sources])) > extent*2e-14:
            raise ValueError('协方差的时间或共轭定义不符')
        exponent_bytes=(dirs[0]/f'{name}.normalization.i32').read_bytes()
        if len(exponent_bytes)!=64*4:
            raise ValueError('协方差尺度长度错误')
        exponents=np.frombuffer(exponent_bytes,dtype='<i4')
        if exponents.shape != (64,) or np.any(exponents<0) or np.any(exponents>60):
            raise ValueError('协方差归一化范围错误')
        for directory in dirs[1:]:
            if (directory/f'{name}.normalization.i32').read_bytes() != exponents.tobytes():
                raise ValueError('协方差尺度不重复')
        normalized=np.ldexp(raw_cov[sources],exponents[sources,None]).astype('<f4')
        a=floats(local/f'{name}.raw.alpha.f32',(64,4));scaled_a=floats(local/f'{name}.scaled.alpha.f32',(64,4))
        if not identical(a,scaled_a):
            raise ValueError('生产预测器不满足此次两次幂尺度控制')
        expected=np.array([regularized_solver(v) for v in raw_cov[sources]],dtype='<f4')
        if not np.array_equal(a[sources],expected):
            raise ValueError('本地系数不符合声明的正则化规则')
        off=arrays['reference0'][first:last,lo:hi].copy()
        if r['kind']=='live':
            captured=floats(data/f'{name}.reference.f32',(40,64,2))[first:last,lo:hi].copy()
            if not identical(off,captured):
                raise ValueError('HF 原语没有复现实际检查点')
        if np.all(arrays['chirp'][:groups]==0):
            copied=[]
            for s,start,stop in zip(r['sources'],r['targets'][:-1],r['targets'][1:]):
                copied.append(q[first:last,s:s+stop-start]*np.float32(.25))
            if not identical(off,np.concatenate(copied,axis=1)):
                raise ValueError('零 chirp 搬运的尺度或子带映射错误')
        ref=off*np.float32(4)
        outputs={};gains={}
        for mode in ('raw','scaled'):
            for flag in ('off','preflat','reference_controls'):
                outputs[f'{mode}_{flag}']=floats(local/f'{name}.{mode}.{flag}.f32',(last-first,64,2))[:,lo:hi].copy()
            gains[mode]=floats(local/f'{name}.{mode}.gain.f32',(64,))[:lo].copy()
            if np.any(gains[mode]<=0):
                raise ValueError('非正预平坦化增益')
        if not identical(outputs['raw_off'],outputs['scaled_off']):
            raise ValueError('无预平坦化的 HF 尺度控制不一致')
        row=dict(id=name,kind=r['kind'],scope=r['scope'],nonzero=bool(np.any(ref)),
                 reference_preflat=metrics(arrays['reference0'][first:last,lo:hi].copy(),arrays['reference1'][first:last,lo:hi].copy()),
                 covariance=metrics(arrays['cov'][sources,:8].copy(),normalized),
                 coefficients=metrics(arrays['alpha'][sources].copy(),a[sources].copy()),
                 native_coefficients_with_math_cov=metrics(arrays['alpha'][sources].copy(),arrays['alpha_math_cov'][sources].copy()),
                 local_coefficients_with_native_cov=metrics(arrays['alpha'][sources].copy(),floats(local/f'{name}.alpha_native_cov.f32',(64,4))[sources].copy()),
                 chirp=metrics(arrays['chirp'][:groups].copy(),local_chirp),
                 gain_model={mode:checked_gain_metrics(gains[mode],gain_model(q,lo,first,last,scale,gain_convention)) for mode,scale in [('raw',1),('scaled',2**23)]},
                 hf={mode:metrics(ref,v) for mode,v in outputs.items()},
                 native_negative_determinants=int(np.count_nonzero(arrays['cov'][sources,8]<0)))
        if r['kind']=='live':
            for b in range(hi-lo):
                band_results.append(dict(id=name,band=lo+b,without_preflat=metrics(ref[:,b].copy(),outputs['scaled_off'][:,b].copy()),
                                         with_preflat=metrics(ref[:,b].copy(),outputs['scaled_preflat'][:,b].copy())))
        else:
            row.update(pattern=r['pattern'],amplitude_exp=r['amplitude_exp'],first=first,last=last)
        if r['kind']=='preflat_probe' and r['pattern'] in ('rising','falling'):
            mapping={}
            for s,start,stop in zip(r['sources'],r['targets'][:-1],r['targets'][1:]):
                for b in range(stop-start):
                    mapping.setdefault(s+b,start+b-lo)
            bands=sorted(mapping);targets=[mapping[b] for b in bands]
            original=q[first:last,bands].copy();input_slope=slope(original,bands)
            output_slope=slope(outputs['scaled_preflat'][:,targets],bands)
            multiplied=original*gains['scaled'][bands][None,:,None]
            row['slope_direction']=dict(input_db_per_band=input_slope,production_db_per_band=output_slope,
                                        production_ratio=output_slope/input_slope,
                                        native_ratio=slope(ref[:,targets],bands)/input_slope,
                                        mathematical_multiply_db_per_band=slope(multiplied,bands))
        result.append(row)
    chirps=[];solvers=[]
    for r in m['chirp_cases']:
        name=identifier(r['id']);integer(r['current'],0,3);integer(r['previous_mode'],0,3);integer(r['previous_bits'],0,2**32-1)
        a=floats(dirs[0]/f'{name}.reference_chirp.f32',(1,));b=floats(local/f'{name}.local_chirp.f32',(1,))
        for directory in dirs[1:]:
            if not identical(a,floats(directory/f'{name}.reference_chirp.f32',(1,))):
                raise ValueError('chirp 探针不重复')
        chirps.append(dict(id=name,metrics=metrics(a,b),zero_decision_equal=bool((a[0]==0)==(b[0]==0))))
    for r in m['solver_cases']:
        name=identifier(r['id']);c=floats(data/f'{name}.solver_cov.f32',(9,));a=floats(dirs[0]/f'{name}.reference_alpha.f32',(4,));b=floats(local/f'{name}.local_alpha.f32',(4,))
        if not identical(c,np.asarray(r['covariance'],dtype='<f4')):
            raise ValueError('解算器输入与声明不符')
        if not np.array_equal(b,regularized_solver(c)):
            raise ValueError('协方差控制的本地结果不符')
        for directory in dirs[1:]:
            if not identical(a,floats(directory/f'{name}.reference_alpha.f32',(4,))):
                raise ValueError('解算器探针不重复')
        solvers.append(dict(id=name,covariance=c.tolist(),reference=a.tolist(),local=b.tolist(),metrics=metrics(a,b)))
    return dict(schema='macinac4.hf-tna-preflat-comparison',version=1,status='observation_only',
                preflatten_gain_convention=gain_convention,
                output_gain_phase_or_delay_fitting=False,cases=result,live_bands=band_results,chirp_cases=chirps,solver_cases=solvers)


def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('manifest','data','references','local'):
        p.add_argument(name,type=Path)
    p.add_argument('--output',required=True,type=Path);args=p.parse_args(argv)
    try:
        result=compare(args.manifest,args.data,args.references,args.local)
        with args.output.open('x') as target:
            json.dump(result,target,ensure_ascii=False,indent=2,allow_nan=False);target.write('\n')
    except (OSError,ValueError,KeyError,TypeError,struct.error) as error:
        print(f'HF 重放检查失败：{error}',file=sys.stderr);return 2
    print(json.dumps({'cases':len(result['cases']),'chirp':len(result['chirp_cases']),'solver':len(result['solver_cases'])}));return 0


if __name__=='__main__':raise SystemExit(main())
