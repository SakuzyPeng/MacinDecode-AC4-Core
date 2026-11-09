#!/usr/bin/env python3
"""固定身份、尺度和 31 样本对齐，评价仅测试的 A-SPX 相位候选。"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path

import numpy as np

try:
    from .compare_object_pcm import compare_track, finite_pcm
    from .check_aspx_noise_replay import integer
    from .check_ajoc_timing_stream import sync_frames
except ImportError:
    from compare_object_pcm import compare_track, finite_pcm
    from check_aspx_noise_replay import integer
    from check_ajoc_timing_stream import sync_frames

MODES = ('baseline', 'noise_inverse_r', 'patch_ratio', 'noise_and_patch')
BLOCK = 4096


def fixed_noise_signal_variants(high_a, high_b, gain_a, gain_b, noise, sine, noise_enabled):
    """固定同一噪声和音调，只交换实际 HF 与增益；保留生产的 f32 加法顺序。"""
    if high_a.ndim != 3 or not high_a.size or high_a.shape[-1] != 2:
        raise ValueError('HF 形状无效')
    for value in (high_a, high_b, noise, sine):
        if value.shape != high_a.shape or value.dtype != np.dtype('<f4') or not np.isfinite(value).all():
            raise ValueError('HF/生成分量形状、类型或有限性错误')
    for gain in (gain_a,gain_b):
        if gain.shape != (high_a.shape[1],) or gain.dtype != np.dtype('<f4') or not np.isfinite(gain).all() or np.any(gain<0):
            raise ValueError('信号增益无效')
    if noise_enabled.shape!=(high_a.shape[1],) or noise_enabled.dtype!=np.dtype(bool):
        raise ValueError('注噪条件无效')
    result={}
    for name,high,gain in [('original',high_a,gain_a),('hf_only',high_b,gain_a),
                           ('gain_only',high_a,gain_b),('both_fixed_noise',high_b,gain_b)]:
        output=high*gain[None,:,None];output[:,noise_enabled]+=noise[:,noise_enabled];output+=sine
        if not np.isfinite(output).all():raise ValueError('信号交换导致非有限输出')
        result[name]=output
    return result


def correlation_metrics(local, reference):
    """归一化零延迟互相关／频带复相干性；另保留未归一化矩阵差。"""
    if (local.shape != reference.shape or local.ndim != 2 or local.shape[0] != local.shape[1]
            or not np.isfinite(local).all() or not np.isfinite(reference).all()):
        raise ValueError('互相关矩阵形状或有限性错误')
    x = np.maximum(local.diagonal().real, 0)
    y = np.maximum(reference.diagonal().real, 0)
    dx, dy = np.sqrt(x[:, None]*x[None, :]), np.sqrt(y[:, None]*y[None, :])
    off_diagonal = ~np.eye(len(x), dtype=bool)
    usable = (dx > 0) & (dy > 0) & off_diagonal
    cx = np.divide(local, dx, out=np.zeros_like(local), where=dx > 0)
    cy = np.divide(reference, dy, out=np.zeros_like(reference), where=dy > 0)
    difference = abs(cx-cy)
    weights = np.where(usable, dy, 0)
    maximum = float(np.max(difference[usable])) if np.any(usable) else None
    pair = list(map(int, np.unravel_index(np.argmax(np.where(usable, difference, -1)), difference.shape))) if maximum is not None else None
    reference_norm = float(np.linalg.norm(reference))
    return dict(valid_directed_pairs=int(np.count_nonzero(usable)),
                undefined_directed_pairs=int(np.count_nonzero(((dx > 0) | (dy > 0)) & ~usable & off_diagonal)),
                both_zero_normalizer_pairs=int(np.count_nonzero((dx == 0) & (dy == 0) & off_diagonal)),
                max_correlation_difference=maximum, max_pair=pair,
                reference_energy_weighted_rms_difference=float(np.sqrt(np.sum(weights*difference**2)/np.sum(weights))) if np.sum(weights) else None,
                relative_matrix_frobenius_error=float(np.linalg.norm(local-reference)/reference_norm) if reference_norm else None)


def diagnostics(local, reference, scale=1., scene_roundtrip=False):
    """全部共同 PCM 样本进入包络、首尾和导数统计；STFT 单独标记不足一窗的尾部。"""
    if (local.ndim != 2 or local.shape != reference.shape or not len(local) or not local.shape[1]
            or type(scale) not in (int,float) or not math.isfinite(scale) or scale <= 0):
        raise ValueError('PCM 形状或尺度无效')
    count, channels = local.shape
    cov_x = np.zeros((channels, channels));cov_y = np.zeros_like(cov_x)
    spectral_x = np.zeros((channels, channels), complex);spectral_y = np.zeros_like(spectral_x)
    envelopes = {n:dict(blocks=0,reference_energy=0.,absolute_energy_error=0.,max_block_absolute_energy_error=0.) for n in (256, 2048)}
    ranges = {'startup':(0,min(count,6144)), 'tail':(max(0,count-6144),count)}
    segments = {key:dict(samples=0,error_energy=0.,reference_energy=0.,max_abs_error=0.,local_peak=0.,reference_peak=0.) for key in ranges}
    spectral_blocks = 0;previous_x = previous_y = None;peak_dx = peak_dy = 0.
    spectral_envelope = dict(blocks=0,reference_energy=0.,absolute_energy_error=0.,max_block_absolute_energy_error=0.)
    window = np.hanning(BLOCK);frequency = np.fft.rfftfreq(BLOCK,1/48000)
    high = (frequency >= 12000) & (frequency < 18000)
    for start in range(0,count,BLOCK):
        raw = local[start:start+BLOCK]
        if scene_roundtrip:raw=(np.asarray(raw,'<f4')*np.float32(2**-15))*np.float32(2**15)
        x=np.asarray(raw,float)*scale;y=np.asarray(reference[start:start+BLOCK],float)
        if not np.isfinite(x).all() or not np.isfinite(y).all():raise ValueError('PCM 含非有限值')
        cov_x+=x.T@x;cov_y+=y.T@y
        if previous_x is not None:
            peak_dx=max(peak_dx,float(np.max(abs(x[0]-previous_x))));peak_dy=max(peak_dy,float(np.max(abs(y[0]-previous_y))))
        if len(x)>1:
            peak_dx=max(peak_dx,float(np.max(abs(np.diff(x,axis=0)))));peak_dy=max(peak_dy,float(np.max(abs(np.diff(y,axis=0)))))
        previous_x,previous_y=x[-1],y[-1]
        for size,entry in envelopes.items():
            for offset in range(0,len(x),size):
                ex=np.sum(x[offset:offset+size]**2,axis=0);ey=np.sum(y[offset:offset+size]**2,axis=0)
                error=float(np.sum(abs(ex-ey)));entry['blocks']+=1;entry['reference_energy']+=float(np.sum(ey));entry['absolute_energy_error']+=error;entry['max_block_absolute_energy_error']=max(entry['max_block_absolute_energy_error'],error)
        for key,(a,b) in ranges.items():
            left,right=max(a,start),min(b,start+len(x))
            if left>=right:continue
            xs=x[left-start:right-start];ys=y[left-start:right-start];entry=segments[key]
            entry['samples']+=len(xs);entry['error_energy']+=float(np.sum((xs-ys)**2));entry['reference_energy']+=float(np.sum(ys**2));entry['max_abs_error']=max(entry['max_abs_error'],float(np.max(abs(xs-ys))));entry['local_peak']=max(entry['local_peak'],float(np.max(abs(xs))));entry['reference_peak']=max(entry['reference_peak'],float(np.max(abs(ys))))
        if len(x)==BLOCK:
            a=np.fft.rfft(x*window[:,None],axis=0)[high];b=np.fft.rfft(y*window[:,None],axis=0)[high]
            spectral_x+=a.conj().T@a;spectral_y+=b.conj().T@b;spectral_blocks+=1
            ex=np.sum(abs(a)**2,axis=0);ey=np.sum(abs(b)**2,axis=0)
            error=float(np.sum(abs(ex-ey)));spectral_envelope['blocks']+=1;spectral_envelope['reference_energy']+=float(np.sum(ey));spectral_envelope['absolute_energy_error']+=error;spectral_envelope['max_block_absolute_energy_error']=max(spectral_envelope['max_block_absolute_energy_error'],error)
    for entry in envelopes.values():
        entry['relative_absolute_energy_error']=entry['absolute_energy_error']/entry['reference_energy'] if entry['reference_energy'] else None
    spectral_envelope['relative_absolute_energy_error']=spectral_envelope['absolute_energy_error']/spectral_envelope['reference_energy'] if spectral_envelope['reference_energy'] else None
    return dict(samples=count,envelope_windows={str(k):v for k,v in envelopes.items()},
                whole_zero_lag_correlation=correlation_metrics(cov_x,cov_y),
                band_12_18_complex_coherence=correlation_metrics(spectral_x,spectral_y) if spectral_blocks else None,
                band_12_18_energy_envelope=spectral_envelope,
                spectral_blocks=spectral_blocks,spectral_tail_samples=count%BLOCK,
                local_max_sample_step=peak_dx,reference_max_sample_step=peak_dy,segments=segments)


def checked(root, entry):
    path=root/entry['path']
    with path.open('rb') as f:digest=hashlib.file_digest(f,'sha256').hexdigest()
    if digest!=entry['sha256']:raise ValueError('文件摘要不符')
    return path


def compare_case(case, root):
    frames=integer(case['frames'],2,2**31-1);channels=integer(case['channels'],1,21)
    source=checked(root,case['input'])
    if len(sync_frames(source.read_bytes()))!=frames:raise ValueError('完整 AU 数量不符')
    reference_path=checked(root,case['reference'])
    if reference_path.stat().st_size!=frames*2048*21*4:raise ValueError('参考 PCM 形状错误')
    reference=np.memmap(reference_path,dtype='<f4',mode='r').reshape(-1,21);finite_pcm(reference,'reference')
    if np.any(reference[:,channels:]!=0):raise ValueError('未描述物理尾路非零')
    if set(case['variants'])!=set(MODES):raise ValueError('四组候选覆盖不完整')
    count=len(reference)-31;baseline=None;results={}
    for mode in MODES:
        record=case['variants'][mode];path=checked(root,record['pcm']);state=json.loads(checked(root,record['phase']).read_text())
        if state['mode']!=mode or not 0<=state['max_energy_error_over_bound']<=1:raise ValueError('候选身份或单位旋转能量约束错误')
        if (state['noise_components']>0)!=(mode in ('noise_inverse_r','noise_and_patch')) or (state['patch_components']>0)!=(mode in ('patch_ratio','noise_and_patch')):raise ValueError('候选未按声明执行')
        if path.stat().st_size!=frames*2048*channels*4:raise ValueError('本地 PCM 尺寸错误')
        local=np.memmap(path,dtype='<f4',mode='r').reshape(-1,channels);finite_pcm(local,'local')
        if mode=='baseline':baseline=local
        elif local[:,0].tobytes()!=baseline[:,0].tobytes():raise ValueError('候选改变 LFE')
        tracks=[]
        for channel in range(channels):
            x=(local[:count,channel]*np.float32(2**-15))*np.float32(2**15)
            tracks.append(compare_track(x,reference[31:,channel],2**-16,48000))
        error=sum(t['rmse']**2 for t in tracks);bands=[sum(t['bands'][b]['rmse']**2 for t in tracks) for b in range(3)]
        energy=[sum(t['bands'][b]['reference_rms']**2 for t in tracks) for b in range(3)]
        results[mode]=dict(whole_error_energy_per_sample=error,band_error_energy_per_sample=bands,
                           band_reference_energy_per_sample=energy,band_snr_db=[10*math.log10(e/v) if e and v else None for e,v in zip(energy,bands)],
                           max_abs_error=max(t['max_abs_error'] for t in tracks),phase=state,
                           diagnostics=diagnostics(local[:count],reference[31:,:channels],2**-16,True))
    base=results['baseline']
    for mode,result in results.items():
        result['whole_improvement_db']=10*math.log10(base['whole_error_energy_per_sample']/result['whole_error_energy_per_sample']) if base['whole_error_energy_per_sample'] and result['whole_error_energy_per_sample'] else None
        result['band_improvement_db']=[10*math.log10(a/b) if a and b else None for a,b in zip(base['band_error_energy_per_sample'],result['band_error_energy_per_sample'])]
    return dict(id=case['id'],group=case['group'],frames=frames,channels=channels,compared_samples=count,variants=results)


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('manifest',type=Path);parser.add_argument('--output',required=True,type=Path);args=parser.parse_args()
    m=json.loads(args.manifest.read_text())
    if (m.get('schema'),m.get('version'))!=('macinac4.aspx-phase-candidate',1) or m.get('fit_parameters') is not False:raise ValueError('未知协议或允许拟合')
    if not m['cases'] or len({c['id'] for c in m['cases']})!=len(m['cases']):raise ValueError('案例缺失或重复')
    results=[]
    for case in m['cases']:
        results.append(compare_case(case,args.manifest.parent));print(case['id'],'compared',flush=True)
    report=dict(schema='macinac4.aspx-phase-candidate-comparison',version=1,status='observation_only',local_scale=2**-16,reference_lag_samples=31,fit_parameters=False,cases=results,
                limitations=['波形、包络和相关性分数不单独裁决规范正确性。','HF 相干性仅统计完整 Hann 窗；全部 PCM 尾部仍进入时域统计。','相关性未定义的静音路对单独计数，不能充当相位吻合。'])
    with args.output.open('x') as stream:json.dump(report,stream,ensure_ascii=False,indent=2,allow_nan=False);stream.write('\n')


if __name__=='__main__':main()
