#!/usr/bin/env python3
"""以 100 位十进制增广 QR 核对 TNA；不采用参考解算器作为正确性判据。"""
from __future__ import annotations
import argparse
from decimal import Decimal as D, localcontext
from fractions import Fraction as F
import hashlib
import json
from pathlib import Path
import numpy as np
try:
    from .check_aspx_basis_replay import metrics
except ImportError:
    from check_aspx_basis_replay import metrics

LAYOUTS=((6,3),(8,3),(12,3),(15,3),(16,3),(24,6),(30,6),(32,6))
EXPONENTS=(-149,-130,-100,-50,0,50,100,125)
PATTERNS=('zero','constant','tone','two','near4','near8','near12','near16','near20','near24',
          'random','ramp','first','last','grow','decay','below4','exact4','above4')

# 自有构造反例的 42 个复样本（u32 位型）。固定输入，避免宿主 sin/cos 版本改变
# 这个距清零阈值极近的夹具；前四个样本为历史，其余 38 个为本区间。
BOUNDARY_BITS=(
    723141345,0,728933277,725520095,722616597,739630475,2885630915,747401760,
    2903043499,747509288,2912391511,2892340186,2915758778,2917687300,2894113697,2929331502,
    783394392,2935766601,798212456,2930855530,806327502,794925444,807152515,814025185,
    2946897327,823712670,2975693475,827771211,2988010555,813727447,2994902188,2988720674,
    2991579214,3004251751,851663412,3012711301,872466808,3014251555,882490232,852244422,
    887229010,886176528,876629607,899178615,3046534033,906407754,3062781511,904782200,
    3071585373,3055850210,3073836679,3078119114,899116031,3088724043,944105804,3094130171,
    957803165,3086981141,965372963,956836935,965058179,973802255,3112524029,982949299,
    3136191929,985905535,3147446726,953418224,3153505500,3149483455,3147765736,3163884465,
    1014176862,3171797149,1032281865,3172200258,1041770028,1019970691,1045423155,1046738590,
    1026920977,1058658272,3207344431,1065353216,
)


def synthetic(pattern,length,phase=0.):
    t=np.arange(length,dtype=float)
    if pattern=='zero':z=np.zeros(length,complex)
    elif pattern=='constant':z=np.full(length,1+.5j)
    elif pattern=='random':
        state=847012;values=[]
        for _ in range(length*2):
            state=(1664525*state+1013904223)%2**32;values.append(((state>>8)/2**24-.5)*.25)
        v=np.array(values).reshape(length,2);z=v[:,0]+1j*v[:,1]
    elif pattern=='ramp':z=1+.1j*t+.003*t*t
    elif pattern in ('first','last'):
        z=np.zeros(length,complex);z[0 if pattern=='first' else -1]=1+.5j
    elif pattern in ('grow','decay','below4','exact4','above4','threshold'):
        ratio={'grow':1.9,'decay':.5,'below4':4-2**-12,'exact4':4,'above4':4+2**-12,'threshold':4}[pattern]
        z=ratio**((t-(length-1))/2)*np.exp(1j*phase*t/2)
    else:
        ratio=1 if pattern=='two' else 2.**(-int(pattern[4:])) if pattern.startswith('near') else 0
        z=np.exp(.21j*t)+ratio*(.75-.5j)*np.exp(.83j*t)
    data=np.column_stack((z.real,z.imag));peak=float(np.max(abs(data)))
    return (data/peak if peak else data).astype('<f4')


def synthetic_cases():
    rows=[]
    for n,offset in LAYOUTS:
        for pattern in PATTERNS:
            for exponent in EXPONENTS:
                rows.append(dict(id=f's{len(rows):04}',kind='synthetic',n=n,offset=offset,
                                 pattern=pattern,exponent=exponent,scale_group=f'n{n}{pattern}'))
    for i in range(256):
        rows.append(dict(id=f'b{i:03}',kind='threshold',n=32,offset=6,pattern='threshold',
                         phase=i*np.pi/512,exponent=0,scale_group=f'b{i:03}'))
    return rows


def values_for(c):
    if c['id']=='b217':return np.array(BOUNDARY_BITS,dtype='<u4').view('<f4').reshape(42,2)
    base=synthetic(c['pattern'],c['n']+c['offset']+4,c.get('phase',0))
    return np.ldexp(base.astype(float),c['exponent']).astype('<f4')


def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare(root,live_root=None):
    root.mkdir();data=root/'inputs';data.mkdir();rows=synthetic_cases()
    values={c['id']:values_for(c) for c in rows}
    if live_root:
        prior=json.loads((live_root/'manifest.json').read_text())
        for c in prior['cases']:
            if c['kind']!='live':continue
            low=np.fromfile(live_root/'normalized_inputs'/f"{c['id']}.qmf.f32",dtype='<f4').reshape(40,64,2)
            history=np.fromfile(live_root/'normalized_inputs'/f"{c['id']}.history.f32",dtype='<f4').reshape(4,40,2)
            for band in sorted({s+j for s,a,b in zip(c['sources'],c['targets'][:-1],c['targets'][1:]) for j in range(b-a)}):
                name=f"r{c['scope']}{c['call']}b{band}"
                row=dict(id=name,kind='live',n=32,offset=6,scope=c['scope'],call=c['call'],band=band,
                         exponent=0,scale_group=name,source_case=c['id'])
                rows.append(row);values[name]=np.concatenate((history[:,band],low[:38,band]))*np.float32(2**23)
    for c in rows:
        v=values[c['id']];assert v.shape==(c['n']+c['offset']+4,2) and np.isfinite(v).all()
        peak=float(np.max(abs(v)));c['hf_shift']=-int(np.frexp(peak)[1])-4 if peak else 0
        path=data/f"{c['id']}.f32";v.tofile(path);c['input_sha256']=sha(path)
        if c['kind']=='synthetic':
            base=synthetic(c['pattern'],len(v));exact=np.ldexp(base.astype(float),c['exponent'])
            c['exact_power_scale']=bool(np.array_equal(v.astype(float),exact))
    (data/'cases.txt').write_text(''.join(f"{c['id']} {c['n']} {c['offset']} {c['hf_shift']}\n" for c in rows))
    manifest=dict(schema='macinac4.tna-stability-input',version=1,cases=rows,
                  source_manifest_sha256=sha(live_root/'manifest.json') if live_root else None)
    (root/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    return manifest


def add(a,b):return a[0]+b[0],a[1]+b[1]
def sub(a,b):return a[0]-b[0],a[1]-b[1]
def mul(a,b):return a[0]*b[0]-a[1]*b[1],a[0]*b[1]+a[1]*b[0]
def div(a,b):return a[0]/b,a[1]/b
def norm2(a):return a[0]*a[0]+a[1]*a[1]
def dot(a,b):
    return (sum((x[0]*y[0]+x[1]*y[1] for x,y in zip(a,b)),0),
            sum((x[0]*y[1]-x[1]*y[0] for x,y in zip(a,b)),0))


def exact_solution(values=None,covariance=None):
    """f32/f64 输入都是有理数；精确正规方程只用于阈值裁决及 QR 独立复核。

    不能把有限精度 QR 在精确等号附近的最后一位当作严格小于/大于的证明。
    """
    if values is not None:
        z=[tuple(F(float(x)) for x in v) for v in values];t=range(4,len(z),2)
        y=[z[k] for k in t];x=[z[k-2] for k in t];w=[z[k-4] for k in t]
        c01=dot(x,y);c02=dot(w,y);c11=dot(x,x)[0];c12=dot(w,x);c22=dot(w,w)[0]
    else:
        a=list(map(lambda x:F(float(x)),covariance));c01=a[:2];c02=a[2:4];c11=a[4];c12=a[5:7];c22=a[7]
    denominator=c11*c22-norm2(c12)*F(1048576,1048577)
    a1=div(sub(mul(c01,c12),(c02[0]*c11,c02[1]*c11)),denominator) if denominator else (F(0),F(0))
    a0=div(sub((-c01[0],-c01[1]),mul(a1,(c12[0],-c12[1]))),c11) if c11 else (F(0),F(0))
    return (a0,a1)


def projected_candidate(values):
    """仅在宿主调查中检验等价的正交残差算法；不接入生产路径。"""
    z=values[:,0].astype(float)+1j*values[:,1]
    peak=float(np.max(abs(z)))
    if peak:z=z/peak
    t=np.arange(4,len(z),2);y=z[t];x=z[t-2];w=z[t-4]
    norm=float(np.vdot(x,x).real)
    if not norm:return np.zeros(4)
    p1=np.vdot(x,y)/norm;p2=np.vdot(x,w)/norm
    yp=y-p1*x;wp=w-p2*x
    ridge=abs(np.vdot(x,w))**2/(norm*1048577)
    denom=float(np.vdot(wp,wp).real)+ridge
    a1=-np.vdot(wp,yp)/denom if denom else 0j
    a0=-p1-a1*p2
    return np.array([a0.real,a0.imag,a1.real,a1.imag])


def qr_oracle(values,precision=100):
    """增广矩阵上做两列 QR，不形成 P87 行列式或复写两抽头闭式解。"""
    with localcontext() as ctx:
        ctx.prec=precision
        z=[tuple(D.from_float(float(v)) for v in pair) for pair in values]
        peak=max((abs(v) for pair in z for v in pair),default=D(0))
        if peak:z=[div(pair,peak) for pair in z]
        times=range(4,len(z),2);y=[z[t] for t in times];x=[z[t-2] for t in times];w=[z[t-4] for t in times]
        n1=dot(x,x)[0];n2=dot(w,w)[0];cross=dot(x,w);ny=dot(y,y)[0]
        ridge=norm2(cross)/(n1*D(1048577)) if n1 else D(0)
        zero=(D(0),D(0));target=[(-a,-b) for a,b in y]+[zero]
        col1=x+[zero];col2=w+[(ridge.sqrt(),D(0))]
        if n1:
            r11=n1.sqrt();q1=[div(v,r11) for v in col1];r12=dot(q1,col2)
            v2=[sub(v,mul(q,r12)) for v,q in zip(col2,q1)];r22=dot(v2,v2)[0].sqrt()
            if r22:
                q2=[div(v,r22) for v in v2];a1=div(dot(q2,target),r22)
            else:a1=zero
            a0=div(sub(dot(q1,target),mul(r12,a1)),r11)
        elif n2:
            a0=zero;a1=div(dot(w,target[:-1]),n2)
        else:a0=a1=zero
        covariance=[dot(x,y),dot(w,y),(n1,D(0)),dot(w,x),(n2,D(0))]
        cov=[*covariance[0],*covariance[1],n1,*covariance[3],n2]
        norms=[n1.sqrt(),n2.sqrt()];ynorm=ny.sqrt() if ny else D(1)
        alpha=[a0,a1];beta=[norm2(a).sqrt()*n/ynorm for a,n in zip(alpha,norms)]
        # 列归一化后，相关系数不超过 1；ridge 将最小特征值推离零约 epsilon/2。
        # 用保守的操作数预算定界，不按观测输出误差选择容差。
        bound=D(64)*D.from_float(float(np.finfo(float).eps))*len(y)*D(1048576)*(1+max(beta))
        residual=[add(v,add(mul(a0,p),mul(a1,q))) for v,p,q in zip(y,x,w)]
        objective=sum((norm2(v) for v in residual),D(0))+ridge*norm2(a1)
        return dict(alpha=[[str(v) for v in a] for a in alpha],covariance=[str(v*peak*peak) for v in cov],
                    norms=[str(v) for v in norms],ynorm=str(ynorm),weighted_error_bound=str(bound),
                    magnitudes=[str(norm2(a).sqrt()) for a in alpha],ridge=str(ridge),
                    normalized_target_energy=str(ny),normalized_objective=str(objective),peak=str(peak))


def weighted_error(raw,oracle):
    with localcontext() as ctx:
        ctx.prec=100
        return max(norm2(sub(tuple(D.from_float(float(v)) for v in raw[2*i:2*i+2]),tuple(map(D,oracle['alpha'][i])))).sqrt()*D(oracle['norms'][i])/D(oracle['ynorm']) for i in range(2))


def read_array(path,dtype,shape):
    a=np.fromfile(path,dtype=dtype)
    if a.size!=int(np.prod(shape)) or not np.isfinite(a).all():raise ValueError(f'{path.name}: 非有限值或截断')
    return a.reshape(shape)


def read_solver(output):
    path=output/'replay_metadata.json'
    if not path.exists():return 'legacy_covariance'
    value=json.loads(path.read_text())
    if (not isinstance(value,dict) or set(value)!={'schema','version','solver'}
            or value['schema']!='macinac4.tna-stability-replay' or type(value['version']) is not int
            or value['version']!=1 or value['solver'] not in ('legacy_covariance','orthogonal_residual')):
        raise ValueError('无效的 TNA 求解器标记')
    return value['solver']


def validate_manifest(m):
    if m.get('schema')!='macinac4.tna-stability-input' or m.get('version')!=1:raise ValueError('协议不符')
    rows=m['cases'];ids=set()
    for c in rows:
        name=c['id']
        if not name.isascii() or not name.isalnum() or name in ids or (c['n'],c['offset']) not in LAYOUTS or not -130<=c['hf_shift']<=155:
            raise ValueError('身份或几何错误')
        ids.add(name)
    expected={c['id']:c for c in synthetic_cases()}
    actual={c['id']:{k:c[k] for k in expected[c['id']]} for c in rows if c['kind']!='live'}
    if actual!=expected:raise ValueError('构造覆盖不完整')
    live=[c for c in rows if c['kind']=='live']
    expected_live={(scope,call,b) for scope in ('tone','music100','music33') for call in range(9)
                   for b in range(18 if scope=='tone' else 24,36)} if m.get('source_manifest_sha256') else set()
    if len(live)!=len(expected_live) or {(c['scope'],c['call'],c['band']) for c in live}!=expected_live:
        raise ValueError('真实源子带覆盖不完整')
    return rows


def compare(root,output):
    m=json.loads((root/'manifest.json').read_text());cases=validate_manifest(m)
    solver=read_solver(output)
    lines=[f"{c['id']} {c['n']} {c['offset']} {c['hf_shift']}" for c in cases]
    if (root/'inputs/cases.txt').read_text().splitlines()!=lines or (output/'results.txt').read_text().splitlines()!=lines:
        raise ValueError('生产重放身份或数量不符')
    results=[];scales={}
    for c in cases:
        name=c['id'];path=root/'inputs'/f'{name}.f32';v=read_array(path,'<f4',(c['n']+c['offset']+4,2))
        if sha(path)!=c['input_sha256']:raise ValueError('输入摘要不符')
        if c['kind']!='live' and not np.array_equal(v.view('<u4'),values_for(c).view('<u4')):raise ValueError('输入不符独立构造配方')
        peak=float(np.max(abs(v)));expected_shift=-int(np.frexp(peak)[1])-4 if peak else 0
        if c['hf_shift']!=expected_shift:raise ValueError('HF 的预声明二次幂归一化不符输入')
        if c['kind']=='synthetic':
            exact=bool(np.array_equal(v.astype(float),np.ldexp(synthetic(c['pattern'],len(v)).astype(float),c['exponent'])))
            if type(c.get('exact_power_scale')) is not bool or c['exact_power_scale']!=exact:
                raise ValueError('精确缩放与输入量化的分类不符')
        data=read_array(output/f'{name}.f64','<f8',(12,));stored=read_array(output/f'{name}.coeff.f32','<f4',(4,))
        cov,raw=data[:8],data[8:];o=qr_oracle(v)
        weighted=weighted_error(raw,o);bound=D(o['weighted_error_bound'])
        if weighted>bound:raise ValueError(f'{name}: 生产原始解偏离增广 QR：{weighted} > {bound}')
        exact_cov=np.array(list(map(float,o['covariance'])))
        cov_bound=16*np.finfo(float).eps*len(range(4,len(v),2))*float(np.max(abs(v)))**2
        if np.max(abs(cov-exact_cov))>cov_bound:raise ValueError('协方差独立复算不符')
        clamp=bool(raw[0]*raw[0]+raw[1]*raw[1]>=16 or raw[2]*raw[2]+raw[3]*raw[3]>=16)
        expected=np.zeros(4,dtype='<f4') if clamp else raw.astype('<f4')
        if not np.array_equal(stored.view('<u4'),expected.view('<u4')):raise ValueError('实际存储未按原始解执行边界或舍入')
        rational=exact_solution(v)
        hp_clamp=any(norm2(x)>=16 for x in rational)
        # 精确分类独立于 QR 的最终十进制舍入；严格等号不能加任意容差来判。
        exact_margin=[norm2(x)-16 for x in rational]
        with localcontext() as ctx:
            ctx.prec=100
            qr_exact_error=max(norm2(sub(tuple(map(D,o['alpha'][i])),tuple(D(x.numerator)/D(x.denominator) for x in rational[i]))).sqrt()*D(o['norms'][i])/D(o['ynorm']) for i in range(2))
        if qr_exact_error>D('1e-80'):raise ValueError('高精度 QR 与精确正规方程不一致')
        expected_stored=np.zeros(4,dtype='<f4') if hp_clamp else np.array([float(x) for a in rational for x in a],dtype='<f4')
        margin=min(abs(D(x)-4) for x in o['magnitudes'])
        with localcontext() as ctx:
            ctx.prec=100
            absolute_error=max(norm2(sub(tuple(D.from_float(float(v)) for v in raw[2*i:2*i+2]),tuple(map(D,o['alpha'][i])))).sqrt() for i in range(2))
        norm_stored=max(np.hypot(*stored[:2].astype(float)),np.hypot(*stored[2:].astype(float)))
        projected=projected_candidate(v);projected_error=weighted_error(projected,o)
        projected_clamp=bool(projected[0]**2+projected[1]**2>=16 or projected[2]**2+projected[3]**2>=16)
        if projected_error>bound:raise ValueError('投影候选偏离同一正则化目标')
        hf=[];scaled=np.ldexp(v.astype(float),c['hf_shift']).astype('<f4');z=scaled[:,0].astype(float)+1j*scaled[:,1]
        a=stored[:2].astype(float);b=stored[2:].astype(float)
        for i,gamma in enumerate(np.array([0,.6,.9,.98,1],dtype='<f4')):
            y=read_array(output/f'{name}.hf{i}.f32','<f4',(len(v)-4,2))
            g=float(gamma);ideal=z[4:]+complex(*a)*g*z[2:-2]+complex(*b)*g*g*z[:-4]
            err=float(np.max(abs((y[:,0].astype(float)+1j*y[:,1])-ideal)))
            limit=16*np.finfo(np.float32).eps*np.max(abs(z))*(1+abs(complex(*a))*g+abs(complex(*b))*g*g)
            if err>limit:raise ValueError('给定系数的 HF 消费偏离独立复数滤波')
            hf.append(dict(chirp=g,max_abs_error=err,forward_bound=float(limit)))
        if c.get('exact_power_scale'):
            key=c['scale_group'];bits=stored.view('<u4').tolist()
            if key in scales and scales[key]!=bits:raise ValueError('精确二次幂缩放改变系数')
            scales[key]=bits
        results.append(dict(id=name,kind=c['kind'],pattern=c.get('pattern'),scope=c.get('scope'),exponent=c['exponent'],
                            weighted_error=float(weighted),weighted_error_bound=float(bound),max_abs_raw_alpha_error=float(absolute_error),
                            clamped=clamp,high_precision_clamped=hp_clamp,clamp_disagreement=clamp!=hp_clamp,
                            exact_squared_margin=[str(x) for x in exact_margin],
                            projected_weighted_error=float(projected_error),projected_clamped=projected_clamp,
                            projected_clamp_disagreement=projected_clamp!=hp_clamp,
                            high_precision_distance_to_four=float(margin),stored_max_magnitude=float(norm_stored),
                            exact_power_scale=c.get('exact_power_scale'),raw_alpha=raw.tolist(),stored_alpha=stored.tolist(),
                            stored_metrics=metrics(expected_stored,stored),oracle=o,hf=hf))
    return dict(schema='macinac4.tna-stability-comparison',version=1,status='observation_only',solver=solver,
                production_changed=solver=='orthogonal_residual',
                reference_used_as_oracle=False,decimal_precision=100,cases=results,exact_scale_groups=len(scales),
                production_clamp_mismatches=sum(c['clamp_disagreement'] for c in results),
                projected_clamp_mismatches=sum(c['projected_clamp_disagreement'] for c in results))


def main():
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='command',required=True)
    q=sub.add_parser('prepare');q.add_argument('root',type=Path);q.add_argument('--live-root',type=Path)
    q=sub.add_parser('compare');q.add_argument('root',type=Path);q.add_argument('output',type=Path);q.add_argument('--report',required=True,type=Path)
    a=p.parse_args()
    if a.command=='prepare':print(len(prepare(a.root,a.live_root)['cases']),'cases');return
    result=compare(a.root,a.output)
    with a.report.open('x') as f:json.dump(result,f,ensure_ascii=False,indent=2,allow_nan=False);f.write('\n')
    print('verified',len(result['cases']),'TNA cases')


if __name__=='__main__':main()
