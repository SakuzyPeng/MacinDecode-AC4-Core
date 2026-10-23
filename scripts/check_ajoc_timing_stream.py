#!/usr/bin/env python3
"""等长时间字段构造流的生成、保护范围及连续消费检查。"""
from __future__ import annotations
import argparse
import hashlib
import json
import struct
from pathlib import Path
import numpy as np
try:
    from .check_aspx_noise_replay import integer
    from .check_ajoc_matrix import check_directory, exact, sizes, split, matrix_products, native_output_bound
    from .check_ajoc_decorrelator import array
    from .check_ajoc_timing import validate_timeline
except ImportError:
    from check_aspx_noise_replay import integer
    from check_ajoc_matrix import check_directory, exact, sizes, split, matrix_products, native_output_bound
    from check_ajoc_decorrelator import array
    from check_ajoc_timing import validate_timeline

WIDTHS={'ajoc_start_pos':5,'ajoc_ramp_len_minus1':6,'block_offset_factor':6,'ramp_duration_table_index':4}
RAMP_INDEX={1:1,2:2,32:15}


def sha(path):
    with Path(path).open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()


def crc16(data):
    register=0
    for byte in data:
        register^=byte<<8
        for _ in range(8):register=((register<<1)^(0x8005 if register&0x8000 else 0))&0xffff
    return register


def sync_frames(data):
    rows=[];offset=0
    while offset<len(data):
        if len(data)-offset<4:raise ValueError('sync frame 头截断')
        magic=int.from_bytes(data[offset:offset+2],'big')
        if magic not in (0xac40,0xac41):raise ValueError('sync word 错误')
        n=int.from_bytes(data[offset+2:offset+4],'big');header=4
        if n==65535:
            if len(data)-offset<7:raise ValueError('扩展长度截断')
            n=int.from_bytes(data[offset+4:offset+7],'big');header=7
        if not n:raise ValueError('空 sync frame')
        end=offset+header+n;total=end+(2 if magic==0xac41 else 0)
        if total>len(data):raise ValueError('sync frame 载荷截断')
        if magic==0xac41 and crc16(data[offset+2:total]):raise ValueError('CRC 错误')
        rows.append(dict(au=len(rows)+1,offset=offset,raw_start=offset+header,raw_end=end,end=total,crc=magic==0xac41))
        offset=total
    if not rows:raise ValueError('空输入')
    return rows


def read_bits(data,position,width):
    integer(position,0,len(data)*8);integer(width,1,32)
    if position+width>len(data)*8:raise ValueError('字段越界')
    value=0
    for bit in range(position,position+width):value=(value<<1)|((data[bit//8]>>(7-bit%8))&1)
    return value


def validate_fields(data,fields):
    frames=sync_frames(data);seen={};occupied=set()
    for f in fields:
        au=integer(f['au'],1,len(frames));name=f['field'];width=WIDTHS.get(name)
        if width is None or f['width']!=width or f['point']!=0:raise ValueError('字段不在等长单数据点协议中')
        key=(au,name)
        if key in seen:raise ValueError('重复字段')
        bounds=f['region'];frame=frames[au-1]
        if len(bounds)!=2 or not frame['raw_start']*8<=bounds[0]<=f['bit'] or not f['bit']+width<=bounds[1]<=frame['raw_end']*8:
            raise ValueError('字段位置越出已定界载荷')
        value=integer(f['value'],0,(1<<width)-1)
        if read_bits(data,f['bit'],width)!=value:raise ValueError('字段旧值与源比特不符')
        bits=set(range(f['bit'],f['bit']+width))
        if occupied&bits:raise ValueError('字段重叠')
        occupied|=bits;seen[key]=f
    if set(seen)!={(au,name) for au in range(1,len(frames)+1) for name in WIDTHS}:raise ValueError('字段序列不完整')
    for au in range(1,len(frames)+1):
        start=seen[au,'ajoc_start_pos'];ramp=seen[au,'ajoc_ramp_len_minus1']
        factor=seen[au,'block_offset_factor'];index=seen[au,'ramp_duration_table_index']
        if ramp['bit']!=start['bit']+5 or read_bits(data,start['bit']-2,2)!=1:raise ValueError('A-JOC 数据点边界不符')
        if index['bit']!=factor['bit']+9 or read_bits(data,factor['bit']-4,4)!=1 or read_bits(data,factor['bit']+6,3)!=7:
            raise ValueError('OAMD 不是 implicit offset、单块、table ramp 路径')
    return frames,seen


def configurations():
    result=[]
    def add(source,au,mode,start,ramp):
        result.append(dict(id=f'{source}_{au}_{mode}_s{start:02}_r{ramp:02}',source=source,raw_au=au,consumer_au=au+1,mode=mode,start=start,ramp=ramp))
    for start in (0,1,15,31):
        for ramp in (1,2,31,32,33,64):add('music',99,'ajoc',start,ramp)
    for start in (1,31):
        for ramp in (1,33,64):add('music',32,'ajoc',start,ramp)
    for start,ramp in ((1,1),(15,2),(31,32)):
        for mode in ('oamd','both'):add('music',99,mode,start,ramp)
    for start,ramp in ((1,2),(31,64)):add('tone',7,'ajoc',start,ramp)
    assert len(result)==38
    return result


def wanted_changes(c):
    integer(c['start'],0,31);integer(c['ramp'],1,64)
    if c['mode'] not in ('ajoc','oamd','both'):raise ValueError('未知修改模式')
    out={}
    if c['mode'] in ('ajoc','both'):out.update(ajoc_start_pos=c['start'],ajoc_ramp_len_minus1=c['ramp']-1)
    if c['mode'] in ('oamd','both'):
        if c['ramp'] not in RAMP_INDEX:raise ValueError('OAMD 不能等长表达指定 ramp')
        out.update(block_offset_factor=2*c['start'],ramp_duration_table_index=RAMP_INDEX[c['ramp']])
    return out


def make_variant(source,fields,configuration):
    frames,seen=validate_fields(source,fields);au=integer(configuration['raw_au'],1,len(frames))
    out=bytearray(source);patches=[]
    for name,value in wanted_changes(configuration).items():
        f=seen[au,name]
        for i in range(f['width']):
            bit=f['bit']+i;mask=1<<(7-bit%8)
            out[bit//8]=(out[bit//8]&~mask)|(((value>>(f['width']-1-i))&1)<<(7-bit%8))
        patches.append(dict(f,old=f['value'],new=value))
    frame=frames[au-1]
    if frame['crc']:out[frame['raw_end']:frame['end']]=crc16(out[frame['offset']+2:frame['raw_end']]).to_bytes(2,'big')
    proof=dict(schema='macinac4.ajoc-timing-patch',version=1,configuration=configuration,
               source_sha256=hashlib.sha256(source).hexdigest(),variant_sha256=hashlib.sha256(out).hexdigest(),patches=patches,
               crc_aus=[au] if frame['crc'] else [])
    verify_variant(source,bytes(out),proof)
    return bytes(out),proof


def verify_variant(source,variant,proof):
    if proof.get('schema')!='macinac4.ajoc-timing-patch' or proof.get('version')!=1:raise ValueError('未知构造协议')
    if hashlib.sha256(source).hexdigest()!=proof['source_sha256'] or hashlib.sha256(variant).hexdigest()!=proof['variant_sha256']:raise ValueError('源或变体摘要不符')
    before=sync_frames(source);after=sync_frames(variant)
    if before!=after or len(source)!=len(variant):raise ValueError('帧边界或长度改变')
    c=proof['configuration'];au=integer(c['raw_au'],1,len(before));wanted=wanted_changes(c);patches=proof['patches']
    if len(patches)!=len(wanted) or {p['field'] for p in patches}!=set(wanted):raise ValueError('修改字段清单不完整')
    allowed=bytearray(len(source));seen=set();frame=before[au-1]
    for p in patches:
        name=p['field'];width=WIDTHS[name];bit=integer(p['bit'],frame['raw_start']*8,frame['raw_end']*8-width)
        if p['width']!=width or p['au']!=au or p['point']!=0 or p['new']!=wanted[name]:raise ValueError('修改字段与配置不一致')
        if not frame['raw_start']*8<=p['region'][0]<=bit or not bit+width<=p['region'][1]<=frame['raw_end']*8:raise ValueError('修改字段的载荷边界错误')
        if read_bits(source,bit,width)!=p['old'] or read_bits(variant,bit,width)!=p['new']:raise ValueError('字段值改写错误')
        for pos in range(bit,bit+width):
            if pos in seen:raise ValueError('重复修改比特')
            seen.add(pos);allowed[pos//8]|=1<<(7-pos%8)
    expected_crc=[au] if frame['crc'] else []
    if proof['crc_aus']!=expected_crc:raise ValueError('CRC 修改声明错误')
    if frame['crc']:allowed[frame['raw_end']:frame['end']]=b'\xff\xff'
    if any((a^b)&(~mask&255) for a,b,mask in zip(source,variant,allowed)):raise ValueError('非白名单位被改变')
    return dict(frames=len(before),bytes=len(source),changed_bits=sum((a^b).bit_count() for a,b in zip(source,variant)),protected_bits_unchanged=True)


def load_rows(path):return [json.loads(s) for s in Path(path).read_text().splitlines()]


def prefix_digest(path,byte_count):
    integer(byte_count,0,2**63-1)
    digest=hashlib.sha256()
    with Path(path).open('rb') as f:
        remaining=byte_count
        while remaining:
            block=f.read(min(remaining,1024*1024))
            if not block:raise ValueError('改动前 PCM 前缀截断')
            digest.update(block);remaining-=len(block)
    return digest.hexdigest()


def read_scene_export(path):
    """读取既有 MASC 导出格式；coordinate 不是已验证的绝对 PCM 时间。"""
    data=Path(path).read_bytes()
    if len(data)<24:raise ValueError('参考 scene 头截断')
    magic,version,rate,channels,count=struct.unpack_from('<4sIIIQ',data)
    if (magic,version,rate,channels)!=(b'MASC',1,48000,21):raise ValueError('参考 scene 格式错误')
    position=24;rows=[]
    for _ in range(count):
        if position+16>len(data):raise ValueError('参考 scene 事件截断')
        coordinate,ramp,objects,reserved=struct.unpack_from('<qIHH',data,position);position+=16
        if not 1<=objects<=159 or reserved or position+16*objects>len(data):raise ValueError('参考 scene 对象边界错误')
        payload=data[position:position+16*objects];position+=16*objects
        rows.append(dict(coordinate=coordinate,ramp=ramp,objects=objects,objects_sha256=hashlib.sha256(payload).hexdigest()))
    if position!=len(data):raise ValueError('参考 scene 有额外后缀')
    return rows


class NativeTimeline:
    """已核验消费者：命中时隙安装增量、随后推进，达到 N 次即停止。"""
    def __init__(self,current,delta,completed,length):
        self.current=np.asarray(current,'<f4').copy();self.delta=np.asarray(delta,'<f4').copy()
        if self.current.shape!=self.delta.shape or not np.isfinite(self.current).all() or not np.isfinite(self.delta).all():raise ValueError('参考初值无效')
        self.completed=integer(completed,0,64);self.length=integer(length,0,64)
        if self.completed>self.length:raise ValueError('参考游标超过长度')

    def slot(self,target=None,length=None):
        if target is not None:
            target=np.asarray(target,'<f4');integer(length,1,64)
            if target.shape!=self.current.shape or not np.isfinite(target).all():raise ValueError('目标形状或有限性错误')
            self.delta=((target-self.current)*np.float32(1/length)).astype('<f4')
            self.completed=0;self.length=length
        if self.completed<self.length:
            self.current=(self.current+self.delta).astype('<f4');self.completed+=1
        return self.current.copy()


def check_native_timing(directory,local_directory):
    directory=Path(directory);local_directory=Path(local_directory)
    rows=json.loads((directory/'frames.json').read_text());model=None;results=[]
    if len(rows)!=3 or [r['au'] for r in rows]!=list(range(rows[0]['au'],rows[0]['au']+3)):raise ValueError('参考连续三帧不完整')
    for row in rows:
        n=row['slots'];au=row['au'];point=row['starts'][0];length=row['ramps'][0]
        if n!=32 or len(row['starts'])!=1 or len(row['ramps'])!=1:raise ValueError('超出本轮单点长帧范围')
        integer(point,0,n-1);integer(length,1,64)
        d=directory/f'au{au:04}';o,i,de,lens=sizes(row);size=sum(lens)
        current=array(d/'current.f32',(n,size));delta=array(d/'delta.f32',current.shape)
        target=array(d/'target.f32',(size,));before=array(d/'install_current.f32',(size,));before_delta=array(d/'install_delta.f32',(size,))
        cursor=array(d/'cursor.u32',(n,2),'<u4');install_cursor=array(d/'install_cursor.u32',(2,),'<u4')
        if model is None:
            if point!=0:raise ValueError('首个参考窗口缺少帧首状态')
            model=NativeTimeline(before,before_delta,int(install_cursor[0]),int(install_cursor[1]))
        endpoint_error=None;endpoint_slot=None;endpoint_differences=None
        divided=((target-before)/np.float32(length)).astype('<f4')
        reciprocal=((target-before)*np.float32(1/length)).astype('<f4')
        for ts in range(n):
            if ts==point:
                exact(model.current,before,'真实目标安装前的 current')
                exact(model.delta,before_delta,'真实目标安装前的 delta')
                if [model.completed,model.length]!=install_cursor.tolist():raise ValueError('安装前跨帧游标不符')
            expected=model.slot(target if ts==point else None,length if ts==point else None)
            exact(current[ts],expected,'实际消费系数的事件日程')
            exact(delta[ts],model.delta,'实际 delta 的倒数乘法')
            if cursor[ts].tolist()!=[model.completed,model.length]:raise ValueError('实际增量次数错误')
            if ts>=point and model.completed==length and endpoint_slot is None:
                endpoint_slot=ts;endpoint_error=float(np.max(abs(model.current.astype(float)-target.astype(float))))
                endpoint_differences=int(np.count_nonzero(model.current.view('u4')!=target.view('u4')))
        local_target=array(local_directory/f'au{au:04}.targets.f32',(size,))
        exact(target[:sum(lens[:2])],local_target[:sum(lens[:2])],'同源 dry/wet 目标')
        dry,wet,pre=split(target,row)
        expected_pre=np.zeros(pre.shape,float);absolute=np.zeros(pre.shape,float)
        for obj in range(o):
            term=abs(wet[obj].astype(float))[:,None,:]*dry[obj].astype(float)[None,:,:]
            expected_pre+=term;absolute+=abs(term)
        eps=np.finfo(np.float32).eps/2;gamma=(3*o*eps)/(1-3*o*eps)
        pre_bound=gamma*absolute+np.finfo(np.float32).tiny
        if np.any(abs(pre.astype(float)-expected_pre)>pre_bound):raise ValueError('参考 pre 目标超过输入与运算量误差界')
        x=array(d/'x.f32',(i,n,64,2));y=array(d/'y.f32',(n,de,64,2));z=array(d/'z.f32',(n,o,64,2))
        _,predicted=matrix_products(current,x,y,row);bound=native_output_bound(current,x,y,row)
        error=abs(predicted[:,:,:48].astype(float)-z[:,:,:48].astype(float));bound=bound[:,:,:48]
        if np.any(error>bound+1e-38):raise ValueError('参考实际输出乘法不符')
        results.append(dict(au=au,start=point,ramp=length,coefficients_bit_exact=True,delta_bit_exact=True,
                            endpoint_slot=endpoint_slot,endpoint_max_error=endpoint_error,
                            endpoint_differing_components=endpoint_differences,
                            reciprocal_vs_division_differences=int(np.count_nonzero(divided.view('u4')!=reciprocal.view('u4'))),
                            end_cursor=cursor[-1].tolist(),valid_subbands=48,
                            matrix_max_error=float(np.max(error))))
    return results


def check_reference_set(manifest,records):
    lookup={r['id']:r for r in records}
    if len(records)!=38 or set(lookup)!={c['id'] for c in configurations()}:raise ValueError('参考配置覆盖不完整')
    results=[]
    for case in manifest['cases']:
        cfg=case['configuration'];record=lookup[cfg['id']];root=Path(record['directory'])
        files={str(p.relative_to(root)):sha(p) for p in root.rglob('*') if p.is_file()}
        if files!=record['files']:raise ValueError('参考完整捕获清单或摘要改变')
        for p in (root/'r1').rglob('*'):
            if p.is_file() and p.read_bytes()!=(root/'r2'/p.relative_to(root/'r1')).read_bytes():raise ValueError('两个独立进程不逐位一致')
        actual={r['au']:r for r in load_rows(Path(case['directory'])/'matrix/frames.jsonl')}
        for row in json.loads((root/'r1/frames.json').read_text()):
            for key in ['starts','ramps','objects','slots','dmx','decorr','enabled']:
                if row[key]!=actual[row['au']][key]:raise ValueError('参考控制没有绑定实际同源消费者')
        results.append(dict(id=cfg['id'],frames=check_native_timing(root/'r1',Path(case['directory'])/'matrix'),repeated_bit_exact=True))
    return results


def check_local(root,seed,c,source,variant,proof,maps):
    result=verify_variant(source,variant,proof)
    frames,fields=validate_fields(variant,load_rows(root/'fields.jsonl'))
    if len(frames)!=(400 if c['source']=='music' else 143):raise ValueError('完整母流 AU 数量改变')
    for p in proof['patches']:
        if fields[p['au'],p['field']]['bit']!=p['bit'] or fields[p['au'],p['field']]['value']!=p['new']:raise ValueError('重新解析未收到指定字段')
    for name in ['core.f32','sources.f32']:
        if sha(root/name)!=sha(seed/name):raise ValueError('完整前端音频／A-SPX 输出改变')
    current=load_rows(root/'matrix/frames.jsonl');original=load_rows(seed/'matrix/frames.jsonl')
    if len(current)!=len(original):raise ValueError('矩阵帧数改变')
    for a,b in zip(current,original):
        if {k:v for k,v in a.items() if k not in ('selected','cursor_before','starts','ramps')}!={k:v for k,v in b.items() if k not in ('selected','cursor_before','starts','ramps')}:raise ValueError('矩阵参数或对象控制被改变')
        expected_starts=[c['start']] if a['au']==c['consumer_au'] and c['mode']!='oamd' else b['starts']
        expected_ramps=[c['ramp']] if a['au']==c['consumer_au'] and c['mode']!='oamd' else b['ramps']
        if a['starts']!=expected_starts or a['ramps']!=expected_ramps:raise ValueError('实际消费者错绑时间字段')
        if a['selected']:
            for tag in ['x.f32','targets.f32']:
                filename=f"au{a['au']:04}.{tag}"
                if sha(root/'matrix'/filename)!=sha(seed/'matrix'/filename):raise ValueError('固定 X 或矩阵目标改变')
    result['matrix']=check_directory(root/'matrix',maps)
    result['timeline']=validate_timeline(load_rows(root/'timeline.jsonl'),current)
    result['core_sha256']=sha(root/'core.f32');result['aspx_qmf_sha256']=sha(root/'sources.f32')
    result['objects_sha256']=sha(root/'objects.f32')
    if c['mode']=='oamd' or (c['start'],c['ramp'])==(0,32):
        if result['objects_sha256']!=sha(seed/'objects.f32'):raise ValueError('OAMD 单改或原值对照改变本地对象 PCM')
    return result


def prepare_manifest(template,destination):
    """从已捕获的母流字段记录生成固定配置；新目录不得存在。"""
    if template.get('schema')!='macinac4.ajoc-timing-stream-manifest' or template.get('version')!=1:
        raise ValueError('未知实验协议')
    if set(template['seeds'])!={'tone','music'}:raise ValueError('缺少具名母流')
    maps=template['maps']
    if sha(maps['path'])!=maps['sha256']:raise ValueError('参数频带表摘要改变')
    sources={};fields={}
    for name,seed in template['seeds'].items():
        if sha(seed['input'])!=seed['input_sha256']:raise ValueError('母流摘要改变')
        sources[name]=Path(seed['input']).read_bytes();fields[name]=load_rows(Path(seed['directory'])/'fields.jsonl')
        frames,_=validate_fields(sources[name],fields[name])
        if len(frames)!={'music':400,'tone':143}[name]:raise ValueError('母流完整 AU 数量不符')
    destination=Path(destination);destination.mkdir();(destination/'variants').mkdir();(destination/'local').mkdir();cases=[]
    for cfg in configurations():
        data,proof=make_variant(sources[cfg['source']],fields[cfg['source']],cfg)
        path=destination/'variants'/(cfg['id']+'.ac4');path.write_bytes(data);receipt=path.with_suffix('.json')
        receipt.write_text(json.dumps(proof,indent=2)+'\n')
        cases.append(dict(configuration=cfg,input=str(path),proof=str(receipt),directory=str(destination/'local'/cfg['id'])))
    result=dict(template,cases=cases);path=destination/'manifest.json';path.write_text(json.dumps(result,indent=2)+'\n')
    return path


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('manifest',type=Path)
    operation=p.add_mutually_exclusive_group(required=True);operation.add_argument('--output',type=Path);operation.add_argument('--prepare',type=Path,help='从 manifest 的母流与字段记录生成到新目录')
    p.add_argument('--reference',type=Path);args=p.parse_args()
    m=json.loads(args.manifest.read_text())
    if args.prepare:
        print(prepare_manifest(m,args.prepare));return
    if m.get('schema')!='macinac4.ajoc-timing-stream-manifest' or m.get('version')!=1:raise ValueError('未知实验协议')
    if [c['configuration'] for c in m['cases']]!=configurations():raise ValueError('38 个固定配置不完整或改变')
    if sha(m['maps']['path'])!=m['maps']['sha256']:raise ValueError('参数频带表摘要改变')
    maps=json.loads(Path(m['maps']['path']).read_text())['maps'];results=[]
    for c in m['cases']:
        conf=c['configuration'];seed=m['seeds'][conf['source']];source=Path(seed['input']).read_bytes();variant=Path(c['input']).read_bytes();proof=json.loads(Path(c['proof']).read_text())
        if hashlib.sha256(source).hexdigest()!=seed['input_sha256'] or proof['configuration']!=conf:raise ValueError('母流或配置身份错误')
        results.append(dict(id=conf['id'],configuration=conf,local=check_local(Path(c['directory']),Path(seed['directory']),conf,source,variant,proof,maps)))
    result=dict(schema='macinac4.ajoc-timing-stream-comparison',version=1,cases=results)
    if args.reference:result['reference']=check_reference_set(m,json.loads(args.reference.read_text()))
    args.output.write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n');print('38 configurations passed')


if __name__=='__main__':main()
