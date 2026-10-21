#!/usr/bin/env python3
"""从原始差分符号独立核对 A-JOC 目标、连续插值和矩阵乘法；不拟合参考。"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import numpy as np
try:
    from .check_aspx_noise_replay import floats, integer
except ImportError:
    from check_aspx_noise_replay import floats, integer

def exact(actual, expected, label):
    a, b = np.asarray(actual, '<f4'), np.asarray(expected, '<f4')
    if a.shape != b.shape or not np.isfinite(a).all() or not np.isfinite(b).all() or a.tobytes() != b.tobytes():
        error=float(np.max(abs(a.astype(float)-b.astype(float)))) if a.shape==b.shape and a.size else None
        raise ValueError(f'{label} 不逐位一致，最大绝对差 {error}')

def sizes(row):
    o=integer(len(row['objects']),1,20);i=integer(row['dmx'],1,16);d=integer(row['decorr'],0,7)
    return o,i,d,(o*i*64,o*d*64,d*i*64)

def split(values,row):
    o,i,d,lens=sizes(row);a,b=np.cumsum(lens)[:2]
    return values[:a].reshape(o,i,64),values[a:b].reshape(o,d,64),values[b:].reshape(d,i,64)

def targets(row, history, maps):
    o,i,d,lengths=sizes(row);points=len(row['starts'])
    if points!=len(row['ramps']) or points>2 or len(row['enabled'])!=d:
        raise ValueError('数据点或去相关形状错误')
    for start,ramp in zip(row['starts'],row['ramps']):
        integer(start,0,row['slots']-1);integer(ramp,1,64)
    if any(type(x) is not bool for x in row['enabled']):raise ValueError('门控必须为布尔值')
    dry=np.zeros((points,o,i,64),'<f4');wet=np.zeros((points,o,d,64),'<f4')
    for obj,c in enumerate(row['objects']):
        if type(c['present']) is not bool or type(c['coarse']) is not bool or type(c['sparse']) is not bool:
            raise ValueError('对象控制类型错误')
        if not c['present']:
            if c['dry'] or c['wet']:raise ValueError('inactive 对象不应附加差分数据')
            continue
        bands=c['bands'];mapping=maps.get(str(bands))
        if mapping is None or len(mapping)!=64 or set(mapping)!=set(range(bands)) or sorted(mapping)!=mapping:
            raise ValueError('参数频带映射不完整')
        for kind,nrows,dest,mid in [('dry',i,dry,25 if c['coarse'] else 50),('wet',d,wet,10 if c['coarse'] else 20)]:
            nquant=2*mid+1
            if len(c[kind])!=points:raise ValueError('对象数据点数量不符')
            for dp,rows in enumerate(c[kind]):
                if len(rows)!=nrows:raise ValueError('系数行数不符')
                for ch,r in enumerate(rows):
                    key=(obj,kind,ch)
                    if not r['present']:
                        if r['values'] or r['time']:raise ValueError('省略行不应含数值')
                        values=np.full(bands,mid,dtype=np.int64)
                    else:
                        if len(r['values'])!=bands:raise ValueError('系数频带数不符')
                        raw=np.array([integer(v,-2*nquant,2*nquant) for v in r['values']],np.int64)
                        if r['time']:
                            if key not in history or history[key][:2]!=(bands,c['coarse']):raise ValueError('时间差分历史不兼容')
                            values=history[key][2]+raw
                        else:
                            if not 0<=raw[0]<nquant:raise ValueError('F0 越界')
                            values=np.cumsum(raw)%nquant
                        if np.any(values<0) or np.any(values>=nquant):raise ValueError('差分索引越界')
                    history[key]=(bands,c['coarse'],values.copy())
                    dq=((values-mid)*(410 if c['coarse'] else 205)/2048).astype('<f4')
                    dest[dp,obj,ch]=dq[mapping]
    result=[]
    for dp in range(points):
        pre=np.zeros((d,i,64),np.float64)
        for obj in range(o):
            pre+=abs(wet[dp,obj].astype(float))[:,None,:]*dry[dp,obj].astype(float)[None,:,:]
        result.append(np.concatenate((dry[dp].ravel(),wet[dp].ravel(),pre.astype('<f4').ravel())))
    return np.array(result,dtype='<f4').reshape(points,sum(lengths))

class Timeline:
    def __init__(self,size):
        self.current=np.zeros(size,'<f4');self.target=self.current.copy();self.delta=self.current.copy()
        self.completed=self.length=0
    def state(self):return np.stack((self.current,self.target,self.delta),axis=-1)
    def frame(self,row,target):
        out=[]
        for ts in range(row['slots']):
            if self.completed<self.length:
                self.current=(self.current+self.delta).astype('<f4');self.completed+=1
                if self.completed==self.length:self.current=self.target.copy()
            for dp,(start,ramp) in enumerate(zip(row['starts'],row['ramps'])):
                if ts==start:
                    self.target=target[dp].copy();self.delta=((self.target-self.current)/np.float32(ramp)).astype('<f4')
                    self.completed=0;self.length=ramp
            out.append(self.current.copy())
        return np.array(out,'<f4')

def matrix_products(coeff,x,y,row):
    o,i,d,_=sizes(row);all_u=[];all_z=[]
    for ts,c in enumerate(coeff):
        dry,wet,pre=split(c,row)
        u=np.zeros((d,64,2),float);z=np.zeros((o,64,2),float)
        for ch in range(i):
            u+=pre[:,ch,:,None].astype(float)*x[ch,ts][None,:,:].astype(float)
            z+=dry[:,ch,:,None].astype(float)*x[ch,ts][None,:,:].astype(float)
        for de in range(d):
            if row['enabled'][de]:z+=wet[:,de,:,None].astype(float)*y[ts,de][None,:,:].astype(float)
        all_u.append(u.astype('<f4'));all_z.append(z.astype('<f4'))
    return np.array(all_u,'<f4'),np.array(all_z,'<f4')

def immediate_ramp(initial,target,length,slots):
    """固定初值和目标，在命中时隙执行第一步；用于独立检查参考观察。"""
    integer(length,1,64);integer(slots,1,64)
    current=np.asarray(initial,'<f4').copy();target=np.asarray(target,'<f4')
    delta=((target-current)*np.float32(1/length)).astype('<f4')
    result=[]
    for step in range(slots):
        if step<length:current=(current+delta).astype('<f4')
        result.append(current.copy())
    return np.array(result,'<f4')

def pointwise_pre(coeff,row):
    o,i,d,_=sizes(row);result=[]
    for c in coeff:
        dry,wet,_=split(c,row);pre=np.zeros((d,i,64),float)
        for obj in range(o):pre+=abs(wet[obj].astype(float))[:,None,:]*dry[obj].astype(float)[None,:,:]
        result.append(pre.astype('<f4'))
    return np.array(result,'<f4')

def native_output_bound(coeff,x,y,row):
    o,i,d,_=sizes(row);bounds=[]
    for ts,c in enumerate(coeff):
        dry,wet,_=split(c,row);absolute=np.zeros((o,64,2),float)
        for ch in range(i):absolute+=abs(dry[:,ch,:,None].astype(float)*x[ch,ts][None,:,:].astype(float))
        for de in range(d):
            if row['enabled'][de]:absolute+=abs(wet[:,de,:,None].astype(float)*y[ts,de][None,:,:].astype(float))
        bounds.append((2*(i+d)+2)*np.finfo(np.float32).eps*absolute)
    return np.array(bounds)

def check_directory(directory,maps):
    rows=[json.loads(line) for line in (directory/'frames.jsonl').read_text().splitlines()]
    if not rows:raise ValueError('空矩阵序列')
    shape=sizes(rows[0]);model=Timeline(sum(shape[3]));history={};selected=[];counts=dict(frames=0,points=0,dt_rows=0,df_rows=0,sparse_omitted_rows=0,mixed_active_grids=0)
    previous=rows[0]['au']-1
    for row in rows:
        if sizes(row)!=shape or row['au']!=previous+1:raise ValueError('连续矩阵拓扑或 AU 顺序改变')
        previous=row['au'];integer(row['slots'],1,32)
        if row['cursor_before']!=[model.completed,model.length]:raise ValueError('跨帧 ramp 游标不是独立时序的延续')
        before=model.state();t=targets(row,history,maps);coeff=model.frame(row,t)
        counts['frames']+=1;counts['points']+=len(row['starts'])
        counts['mixed_active_grids']+=len({c['bands'] for c in row['objects'] if c['present']})>1
        for c in row['objects']:
            for kind in ['dry','wet']:
                for dp in c[kind]:
                    for v in dp:counts['sparse_omitted_rows' if not v['present'] else 'dt_rows' if v['time'] else 'df_rows']+=1
        if not row['selected']:continue
        stem=f"au{row['au']:04}";o,i,d,lens=shape;n=sum(lens);slots=row['slots']
        exact(floats(directory/f'{stem}.before.f32',(n,3)),before,'开始状态')
        exact(floats(directory/f'{stem}.targets.f32',t.shape),t,'反量化／逐对象网格／pre 目标')
        exact(floats(directory/f'{stem}.coeff.f32',coeff.shape),coeff,'逐时隙插值系数')
        exact(floats(directory/f'{stem}.after.f32',(n,3)),model.state(),'结束状态')
        if json.loads((directory/f'{stem}.cursor_after.json').read_text())!=[model.completed,model.length]:raise ValueError('结束游标不符')
        x=floats(directory/f'{stem}.x.f32',(i,slots,64,2));y=floats(directory/f'{stem}.y.f32',(slots,d,64,2))
        u,z=matrix_products(coeff,x,y,row)
        exact(floats(directory/f'{stem}.u.f32',u.shape),u,'pre 输入乘法')
        exact(floats(directory/f'{stem}.z.f32',z.shape),z,'dry/wet 输出乘法')
        selected.append(dict(au=row['au'],slots=slots,coefficients=coeff.size,qmf_output_components=z.size,bit_exact=True))
    if not selected:raise ValueError('没有观察窗口')
    return dict(status='independent_local_model_pass',counts=counts,selected=selected)

def digest(path):
    with path.open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()

def frozen_files(record):
    directory=Path(record['directory']);files=record['files']
    if {p.name for p in directory.iterdir()}!=set(files):raise ValueError('检查点遗漏或额外文件')
    for name,expected in files.items():
        if Path(name).name!=name or digest(directory/name)!=expected:raise ValueError('检查点摘要或路径改变')
    return directory

def check_reference(reference,cases):
    directory=frozen_files(reference);case=cases[reference['case_id']]
    if reference['input_sha256']!=case['input']['sha256'] or reference['valid_subbands']!=48:
        raise ValueError('参考媒体身份或已观测的有效宽度改变')
    row=json.loads((directory/'frame.json').read_text());au=row['au'];o,i,d,lens=sizes(row);n=sum(lens);dry_wet=sum(lens[:2])
    if au!=reference['au'] or row['starts']!=[0] or row['ramps']!=[32] or row['slots']!=32 or au not in case['selected_aus'] or au+1 not in case['selected_aus']:
        raise ValueError('参考窗口超出固定的调度与跨帧覆盖')
    initial=floats(directory/'initial.f32',(n,));target=floats(directory/'target.f32',(n,));coeff=floats(directory/'coeff.f32',(32,n))
    exact(floats(directory/'repeat_coeff.f32',coeff.shape),coeff,'参考系数独立进程重复')
    exact(floats(directory/'used_drywet.f32',(32,dry_wet)),coeff[:,:dry_wet],'参考实际消费系数')
    exact(coeff,immediate_ramp(initial,target,32,32),'参考时隙状态转移')
    identity=json.loads((directory/'identity.json').read_text())
    if identity['entry']!=identity['consumer'] or len(identity['entry'])!=i or any(len(r)!=32 for r in identity['entry']) or len({v for r in identity['entry'] for v in r})!=i*32:
        raise ValueError('参考矩阵的真实输入身份不符')
    local=Path(case['directory']);t=floats(local/f'au{au:04}.targets.f32',(1,n))[0];c=floats(local/f'au{au:04}.coeff.f32',(32,n));next_c=floats(local/f'au{au+1:04}.coeff.f32',(32,n))
    exact(target,t,'同源反量化与 pre 目标')
    exact(initial[:dry_wet],c[0,:dry_wet],'参考初值与本地首时隙')
    shifted=np.concatenate((c[1:],next_c[:1]))
    exact(coeff[:,:dry_wet],shifted[:,:dry_wet],'调度推导的下一时隙关系')
    x=floats(directory/'x.f32',(i,32,64,2));y=floats(directory/'y.f32',(32,d,64,2));z=floats(directory/'z.f32',(32,o,64,2))
    exact(floats(directory/'repeat_z.f32',z.shape)[:,:,:48],z[:,:,:48],'参考有效输出独立进程重复')
    _,expected=matrix_products(coeff,x,y,row);bound=native_output_bound(coeff,x,y,row)[:,:,:48];error=abs(expected[:,:,:48].astype(float)-z[:,:,:48].astype(float))
    if np.any(error>bound+1e-38):raise ValueError('参考输出乘法超出固定浮点前向界')
    pre=np.stack([split(v,row)[2] for v in coeff]);pointwise=pointwise_pre(coeff,row)
    return dict(case_id=reference['case_id'],au=au,status='observation_only',targets_bit_exact=True,consumer_verified=True,
                immediate_ramp_bit_exact=True,dry_wet_next_tick_bit_exact=True,valid_output_subbands=48,
                changing_dry_wet_coefficients=int(np.count_nonzero(target[:dry_wet]!=initial[:dry_wet])),
                same_tick_dry_wet_max_abs_difference=float(np.max(abs(coeff[:,:dry_wet].astype(float)-c[:,:dry_wet].astype(float)))),
                pointwise_pre_max_abs_difference=float(np.max(abs(pre.astype(float)-pointwise.astype(float)))),
                pre_next_tick_max_abs_error=float(np.max(abs(coeff[:,dry_wet:].astype(float)-shifted[:,dry_wet:].astype(float)))),
                output_max_abs_error=float(np.max(error)),output_max_error_over_bound=float(np.max(np.divide(error,bound,out=np.zeros_like(error),where=bound!=0))))

def check_mixed_pre(record,maps):
    directory=frozen_files(record)
    dry=floats(directory/'dry.f32',(16,2,3,24));wet=floats(directory/'wet.f32',(16,2,2,24));pre=floats(directory/'pre.f32',(16,2,23,3))
    exact(floats(directory/'repeat_pre.f32',pre.shape),pre,'混合网格参考重复')
    counts=[23,15,12,9,7,5,3,1]
    for v in range(16):
        recipe_d=np.zeros((2,3,24),'<f4');recipe_w=np.zeros((2,2,24),'<f4');expected=np.zeros((2,3,64),float)
        for o,n in enumerate([counts[v%8],counts[(v+3)%8]]):
            for b in range(n):
                for c in range(3):recipe_d[o,c,b]=((o*5+c*3+b*2)%9-4)*.25
                for d in range(2):recipe_w[o,d,b]=((o*3+d*2+b)%7-3)*.125*(-1 if v>=8 else 1)
            expected+=abs(recipe_w[o,:,:n][:,maps[str(n)]].astype(float))[:,None,:]*recipe_d[o,:,:n][:,maps[str(n)]].astype(float)[None,:,:]
        exact(dry[v],recipe_d,'自有 dry 探针配方');exact(wet[v],recipe_w,'自有 wet 探针配方')
        exact(pre[v][:,maps['23'],:].transpose(0,2,1),expected,'按各对象网格求 pre')
    exact(pre[:8],pre[8:],'wet 符号反转的 pre 不变性')
    return dict(cases=16,bit_exact=True,all_eight_parameter_grids=True)

def check_manifest(path):
    manifest=json.loads(path.read_text())
    if (manifest.get('schema'),manifest.get('version'))!=('macinac4.ajoc-matrix-manifest',1) or manifest.get('fit_parameters') is not False:
        raise ValueError('未知矩阵协议或允许拟合')
    maps_path=Path(manifest['maps']['path'])
    if digest(maps_path)!=manifest['maps']['sha256']:raise ValueError('独立频带表摘要改变')
    maps=json.loads(maps_path.read_text())['maps'];cases=manifest['cases']
    if not cases or len({c['id'] for c in cases})!=len(cases):raise ValueError('空或重复案例')
    reports=[]
    for case in cases:
        directory=frozen_files(case)
        if case.get('input'):
            if digest(Path(case['input']['path']))!=case['input']['sha256']:raise ValueError('媒体身份改变')
        rows=[json.loads(s) for s in (directory/'frames.jsonl').read_text().splitlines()]
        if [r['au'] for r in rows]!=case['aus'] or [r['au'] for r in rows if r['selected']]!=case['selected_aus']:
            raise ValueError('完整 AU 或观察窗口覆盖改变')
        report=check_directory(directory,maps);reports.append(dict(id=case['id'],**report))
    refs=manifest.get('references',[])
    if len({(r['case_id'],r['au']) for r in refs})!=len(refs):raise ValueError('重复参考窗口')
    references=[check_reference(r,{c['id']:c for c in cases}) for r in refs]
    mixed=check_mixed_pre(manifest['mixed_pre'],maps) if 'mixed_pre' in manifest else None
    return dict(schema='macinac4.ajoc-matrix-comparison',version=1,production_changed=False,fit_parameters=False,cases=reports,references=references,mixed_pre=mixed)

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('manifest',type=Path);p.add_argument('--output',required=True,type=Path);a=p.parse_args()
    try:
        result=check_manifest(a.manifest)
        with a.output.open('x') as f:json.dump(result,f,ensure_ascii=False,indent=2,allow_nan=False);f.write('\n')
    except (OSError,ValueError,KeyError,TypeError,IndexError) as e:print(f'A-JOC 矩阵检查失败：{e}');return 2
    print(json.dumps(dict(cases=len(result['cases']),frames=sum(c['counts']['frames'] for c in result['cases']))));return 0
if __name__=='__main__':raise SystemExit(main())
