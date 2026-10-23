#!/usr/bin/env python3
"""独立逐时隙核对 A-JOC all-pass 与 ducker；不搜索参考参数。"""
from __future__ import annotations

import argparse
from decimal import Decimal, localcontext
import hashlib
import json
from pathlib import Path

import numpy as np

try:
    from .check_ajoc_matrix import exact
    from .check_aspx_noise_replay import integer
except ImportError:
    from check_ajoc_matrix import exact
    from check_aspx_noise_replay import integer

F32 = np.float32
FRAMES = [24, 30, 32, 24, 30, 32]
PATTERNS = ('silence', 'impulse_re', 'impulse_im', 'burst', 'carrier',
            'random', 'low_energy', 'threshold')


def sha(path):
    with Path(path).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def array(path, shape, dtype='<f4'):
    p = Path(path)
    if p.stat().st_size != int(np.prod(shape)) * np.dtype(dtype).itemsize:
        raise ValueError(f'{p.name}: 数据长度不符')
    a = np.fromfile(p, dtype=dtype).reshape(shape)
    if not np.isfinite(a).all():
        raise ValueError(f'{p.name}: 非有限数')
    return a


def extract_tables():
    try:
        from .check_ajoc_tables import extract_pdf, BAND_COUNTS, PDF_PART1, PDF_PART2
    except ImportError:
        from check_ajoc_tables import extract_pdf, BAND_COUNTS, PDF_PART1, PDF_PART2
    maps, _, _, regions, tables, cycle = extract_pdf()
    return dict(schema='macinac4.ajoc-decorr-tables', version=1,
                pdf_sha256={p.name: sha(p) for p in (PDF_PART1, PDF_PART2)},
                mapping=maps[BAND_COUNTS.index(15)],
                regions=regions, cycle=cycle,
                coefficients=[[[str(v) for v in row] for row in tables[k]]
                              for k in ('TABLE_199', 'TABLE_200', 'TABLE_201')])


def validate_tables(t):
    if t.get('schema') != 'macinac4.ajoc-decorr-tables' or t.get('version') != 1:
        raise ValueError('未知系数协议')
    if t['regions'] != [[0, 6, 7, 7], [7, 22, 10, 4], [23, 63, 12, 2]]:
        raise ValueError('区域不完整')
    if t['cycle'] != [0, 2, 1, 0, 2, 1, 0]:
        raise ValueError('七路循环错误')
    mapping = t['mapping']
    if len(mapping) != 64 or sorted(mapping) != mapping or set(mapping) != set(range(15)):
        raise ValueError('15 带映射不完整')
    if len(t['coefficients']) != 3:
        raise ValueError('缺少系数组')
    for rows, n in zip(t['coefficients'], (8, 5, 3)):
        a = np.asarray(rows, float)
        if a.shape != (n, 3) or not np.isfinite(a).all() or not np.all(a[0] == 1):
            raise ValueError('all-pass 系数不完整')
    return t


def energy(u, mapping):
    accumulated = np.zeros((len(u), 15), float)
    for b, band in enumerate(mapping):
        accumulated[:, band] += u[:, b, 0].astype(float)**2 + u[:, b, 1].astype(float)**2
    result = accumulated.astype('<f4')
    if not np.isfinite(result).all():
        raise ValueError('输入能量溢出')
    return result


def duck(previous, e):
    previous = np.asarray(previous, '<f4')
    e = np.asarray(e, '<f4')
    if previous.shape != e.shape + (3,) or not np.isfinite(previous).all() or not np.isfinite(e).all() or np.any(e < 0):
        raise ValueError('ducker 状态或能量错误')
    peak = np.maximum(F32(.7659283) * previous[..., 0], e)
    smooth = F32(.75) * previous[..., 1] + F32(.25) * e
    difference = F32(.75) * previous[..., 2] + F32(.25) * (peak - e)
    gain = np.ones(e.shape, '<f4')
    active = F32(1.5) * difference > smooth
    gain[active] = smooth[active] / (F32(1.5) * (difference[active] + F32(1e-9)))
    return np.stack((peak, smooth, difference), axis=-1), gain


def observed_reference_duck(previous, e):
    """已测参考原语的状态模型；不是生产策略，也不作为规范正确性判据。

    e 位于已冻结的参考 QMF 接口量级。中间双精度用于模拟这些 f32
    操作数的融合乘加，最终逐位检查捕获状态，不凭残差拟合常量。
    """
    e = np.minimum(np.asarray(e, '<f4'), F32(1))
    peak = np.maximum(F32(.7659283) * previous[..., 0], e)
    smooth = ((e * F32(.25)).astype(float) + previous[..., 1].astype(float) * .75).astype('<f4')
    difference = ((previous[..., 2] * F32(.75)).astype(float) + (peak-e).astype(float) * .25).astype('<f4')
    numerator = F32(2/3) * smooth
    gain = np.ones(e.shape, '<f4')
    active = numerator < difference
    gain[active] = numerator[active] / difference[active]
    return np.stack((peak, smooth, difference), axis=-1), gain


class ReferenceTransfer:
    """由观察到的只读系数推导传递函数，另用输入／输出历史直接递推。"""
    def __init__(self, coefficients, count):
        self.x = np.zeros((count, 64, 14, 2), float)
        self.y = np.zeros((count, 64, 7, 2), float)
        self.polynomials = []
        cycle = [0, 2, 1, 0, 2, 1, 0][:count]
        for offset, order in ((0, 7), (14, 4), (22, 2)):
            reverse = coefficients[cycle, offset:offset+order].astype(float)
            combined = coefficients[cycle, offset+order:offset+2*order].astype(float)
            a = np.concatenate((np.ones((count,1)), reverse[:,::-1]), axis=1)
            b = np.concatenate((reverse[:,:1], reverse[:,:1]*a[:,1:]+combined[:,::-1]), axis=1)
            self.polynomials.append((a,b,combined))

    def advance(self, u):
        result = np.zeros(u.shape, float)
        for r,(first,last,delay,order) in enumerate(((0,6,7,7),(7,22,10,4),(23,47,12,2))):
            s=slice(first,last+1);a,b,_=self.polynomials[r]
            x,y=self.x[:,s],self.y[:,s]
            v=x[:,:,delay-1]*b[:,0,None,None]
            for i in range(1,order+1):
                v+=x[:,:,delay+i-1]*b[:,i,None,None]-y[:,:,i-1]*a[:,i,None,None]
            result[:,s]=v
            x[:,:,1:]=x[:,:,:-1].copy();x[:,:,0]=u[:,s]
            y[:,:,1:]=y[:,:,:-1].copy();y[:,:,0]=v
        return result

    def bounds(self, magnitudes):
        result=np.zeros(magnitudes.shape)
        for r,(a,b,c) in enumerate(self.polynomials):
            for de in range(len(a)):
                k=amplification(a[de]);m=magnitudes[de,r]
                feedback=sum(abs(a[de,1:]));combined=sum(abs(c[de]))
                eps=np.finfo(np.float32).eps/2
                gamma=4*len(a[de])*eps/(1-4*len(a[de])*eps)
                denom=1-k*gamma*feedback
                if denom<=0:raise ValueError('参考递推误差界不收敛')
                ew=k*gamma*(m+feedback*k*m)/denom
                # 原语融合累加的界用非融合操作数上界，另计最终 f32 增益乘法。
                result[de,r]=(combined*ew+gamma*(abs(b[de,0])*m+combined*(k*m+ew))
                               +filter_bound(a[de],m)+eps*sum(abs(b[de]))*k*m+np.finfo(float).tiny)
        return result


def verify_reference_states(u, actual, initial, mapping):
    state=np.array(initial,'<f4',copy=True);gains=[];clipped=0
    for i,slot in enumerate(u):
        e=energy(slot,mapping);clipped+=int(np.count_nonzero(e>1))
        state,gain=observed_reference_duck(state,e)
        exact(actual[i],state,'参考 ducker 实际状态')
        gains.append(gain)
    return np.asarray(gains),clipped


def check_reference_manifest(manifest):
    if manifest.get('schema')!='macinac4.ajoc-decorr-reference' or manifest.get('version')!=1 or manifest.get('fit_parameters') is not False:
        raise ValueError('参考协议错误')
    t=manifest['tables']
    if sha(t['path'])!=t['sha256']:raise ValueError('表摘要改变')
    tables=validate_tables(json.loads(Path(t['path']).read_text()))
    results=[]
    for case in manifest['cases']:
        root=Path(case['directory']);n,count,bands,components=case['shape']
        if bands!=64 or components!=2 or case['valid_subbands']!=48:
            raise ValueError('参考有效宽度不符')
        integer(n,1,50000);integer(count,1,7)
        files=case['files']
        if set(files)!={p.name for p in root.iterdir()} or any(sha(root/k)!=v for k,v in files.items()):
            raise ValueError('参考完整文件清单或摘要改变')
        u=array(root/'u.f32',(n,count,64,2));y=array(root/'y.f32',u.shape)
        exact(y,array(root/'repeat_y.f32',y.shape),'独立进程参考重复')
        states=array(root/'states.f32',(n,count,15,3))
        exact(states,array(root/'repeat_states.f32',states.shape),'独立进程状态重复')
        initial=array(root/'initial.f32',(count,15,3))
        gains,clipped=verify_reference_states(u,states,initial,tables['mapping'])
        result=dict(id=case['id'],slots=n,instances=count,valid_subbands=48,
                    states_bit_exact=True,repeated_bit_exact=True,clipped_band_slots=clipped,
                    ducked_band_slots=int(np.count_nonzero(gains<1)))
        if case['zero_start']:
            if np.any(initial):raise ValueError('零状态重放带有非零初值')
            local_y=array(root/'local_y.f32',u.shape)
            local=Model(tables,count)
            coefficients=array(root/'coefficients.f32',(3,26))
            transfer=ReferenceTransfer(coefficients,count)
            max_error=np.zeros((count,3));magnitudes=max_error.copy();diff_energy=signal_energy=0.
            diagnostic_errors=np.zeros(3);gain_difference=0.
            for i,slot in enumerate(u):
                _,local_gain,_,local_filtered,local_output=local.advance(slot*F32(2**23))
                exact(local_y[i],local_output/F32(2**23),'相同物理输入的实际生产重放')
                reference_filtered=transfer.advance(slot)
                expected=(reference_filtered*gains[i][:,tables['mapping'],None]).astype('<f4')
                gain_difference=max(gain_difference,float(np.max(abs(local_gain-gains[i]))))
                variants=[local_filtered/2**23*gains[i][:,tables['mapping'],None],
                          reference_filtered*local_gain[:,tables['mapping'],None],expected]
                for index,value in enumerate(variants):
                    diagnostic_errors[index]+=float(np.sum((value[:,:48].astype(float)-y[i,:,:48].astype(float))**2))
                for r,(first,last) in enumerate(((0,6),(7,22),(23,47))):
                    s=slice(first,last+1)
                    max_error[:,r]=np.maximum(max_error[:,r],np.max(abs(y[i,:,s].astype(float)-expected[:,s].astype(float)),axis=(1,2)))
                    magnitudes[:,r]=np.maximum(magnitudes[:,r],np.max(abs(slot[:,s]),axis=(1,2)))
                diff_energy+=float(np.sum((y[i,:,:48].astype(float)-local_y[i,:,:48].astype(float))**2))
                signal_energy+=float(np.sum(y[i,:,:48].astype(float)**2))
            bounds=transfer.bounds(magnitudes)
            if np.any(max_error>bounds):raise ValueError('参考传递函数重放超出预先算子界')
            result.update(transfer_max_error=max_error.tolist(),forward_bounds=bounds.tolist(),
                          reference_coefficient_grid_2_neg14=bool(np.all(coefficients*2**14==np.floor(coefficients*2**14))),
                          local_difference_energy=diff_energy,reference_energy=signal_energy,
                          local_snr_db=10*np.log10(signal_energy/diff_energy) if signal_energy and diff_energy else None,
                          max_gain_difference=gain_difference,
                          diagnostic_snr_db={name:10*np.log10(signal_energy/e) if signal_energy and e else None
                              for name,e in zip(('nominal_filter_reference_gain','reference_transfer_local_gain','reference_transfer_reference_gain'),diagnostic_errors)})
        else:
            if case.get('native_state_replay_matches')!=n*count:
                raise ValueError('真实窗口缺少完整历史恢复及逐位重放')
            result['native_state_replay_matches']=n*count
        results.append(result)
    if not results:raise ValueError('空参考比较')
    return dict(schema='macinac4.ajoc-decorr-reference-comparison',version=1,cases=results)


def amplification(a):
    """80 位算 companion 矩阵幂的稳定块上界；只依赖系数。"""
    with localcontext() as ctx:
        ctx.prec = 80
        n = len(a) - 1
        A = [[Decimal(0) for _ in range(n)] for _ in range(n)]
        A[0] = [-Decimal.from_float(float(v)) for v in a[1:]]
        for i in range(1, n):
            A[i][i-1] = Decimal(1)
        power = [[Decimal(int(i == j)) for j in range(n)] for i in range(n)]
        total = Decimal(0)
        for _ in range(1024):
            total += max(sum(abs(x) for x in row) for row in power)
            power = [[sum(power[i][k] * A[k][j] for k in range(n)) for j in range(n)] for i in range(n)]
            norm = max(sum(abs(x) for x in row) for row in power)
            if norm < Decimal('.5'):
                # 余量远大于 80 位矩阵运算的舍入；不由被测输出确定。
                return float(total / (1 - norm)) * (1 + 1e-12)
    raise ValueError('无法建立递归稳定误差界')


def filter_bound(a, magnitude):
    """canonical 与直接递推相对同一精确传递函数的前向误差界之和。"""
    k = amplification(a)
    feedback = sum(abs(a[1:]))
    feedforward = sum(abs(a))
    eps = np.finfo(float).eps / 2
    gamma = (4 * len(a) * eps) / (1 - 4 * len(a) * eps)
    denominator = 1 - k * gamma * feedback
    if denominator <= 0:
        raise ValueError('舍入界不收敛')
    ew = k * gamma * (magnitude + feedback * k * magnitude) / denominator
    canonical = feedforward * ew + gamma * feedforward * (k * magnitude + ew)
    direct = k * gamma * (feedforward * magnitude + feedback * feedforward * k * magnitude) / denominator
    return float(canonical + direct + np.finfo(float).tiny)


class Model:
    def __init__(self, tables, count):
        self.t = validate_tables(tables)
        self.count = integer(count, 1, 7)
        self.canonical = np.zeros((count, 64, 14, 2), float)
        self.x = self.canonical.copy()
        self.y = np.zeros((count, 64, 7, 2), float)
        self.ducker = np.zeros((count, 15, 3), '<f4')
        self.coefficients = [np.asarray(c, float)[:, self.t['cycle'][:count]] for c in self.t['coefficients']]

    def advance(self, u):
        u = np.asarray(u, '<f4')
        if u.shape != (self.count, 64, 2) or not np.isfinite(u).all():
            raise ValueError('非法去相关输入')
        e = energy(u, self.t['mapping'])
        after, gain = duck(self.ducker, e)
        direct = np.empty(u.shape, float)
        canonical = np.empty(u.shape, float)
        for r, (first, last, delay, order) in enumerate(self.t['regions']):
            s = slice(first, last + 1)
            a = self.coefficients[r]
            x, y, cells = self.x[:, s], self.y[:, s], self.canonical[:, s]
            v = x[:, :, delay-1] * a[order, :, None, None]
            for i in range(1, order + 1):
                v += x[:, :, delay+i-1] * a[order-i, :, None, None] - y[:, :, i-1] * a[i, :, None, None]
            direct[:, s] = v
            x[:, :, 1:] = x[:, :, :-1].copy()
            x[:, :, 0] = u[:, s]
            y[:, :, 1:] = y[:, :, :-1].copy()
            y[:, :, 0] = v
            w = cells[:, :, 0].copy()
            cells[:, :, :delay-1] = cells[:, :, 1:delay].copy()
            cells[:, :, delay-1] = u[:, s]
            for i in range(1, order + 1):
                w -= cells[:, :, delay+i-1] * a[i, :, None, None]
            out = w * a[order, :, None, None]
            for i in range(1, order + 1):
                out += cells[:, :, delay+i-1] * a[order-i, :, None, None]
            canonical[:, s] = out
            cells[:, :, delay+1:delay+order] = cells[:, :, delay:delay+order-1].copy()
            cells[:, :, delay] = w
        self.ducker = after
        output = (canonical * gain[:, self.t['mapping'], None].astype(float)).astype('<f4')
        return e, gain, direct, canonical, output


def decimal_filter(values, coefficients, delay):
    """P111 的独立 80 位输入／输出历史递推；保持精确的 f64 系数值。"""
    with localcontext() as ctx:
        ctx.prec = 80
        a = [Decimal.from_float(float(v)) for v in coefficients]
        order = len(a) - 1
        x = [Decimal(0)] * (delay + order)
        y = [Decimal(0)] * order
        result = []
        for sample in values:
            v = a[order] * x[delay-1]
            for i in range(1, order + 1):
                v += a[order-i] * x[delay+i-1] - a[i] * y[i-1]
            result.append(float(v))
            x = [Decimal.from_float(float(sample))] + x[:-1]
            y = [v] + y[:-1]
        return np.asarray(result)


def synthetic_input(pattern):
    if pattern not in PATTERNS:
        raise ValueError('未知自有配方')
    n = sum(FRAMES)
    u = np.zeros((n, 7, 64, 2), '<f4')
    if pattern.startswith('impulse'):
        for de in range(7):
            for b in range(64):
                u[(b + 3*de) % 24, de, b, int(pattern == 'impulse_im')] = F32((de+1)/8)
    elif pattern in ('random', 'low_energy', 'burst'):
        rng = np.random.Generator(np.random.PCG64(871923))
        u[:] = rng.integers(-64, 65, size=u.shape).astype('<f4') / F32(256)
        if pattern == 'low_energy':
            u *= F32(2**-20)
        if pattern == 'burst':
            u[8:] = 0
    elif pattern == 'carrier':
        for t in range(n):
            u[t, :, :, t % 2] = F32(.125 if t % 4 < 2 else -.125)
    elif pattern == 'threshold':
        # 突发后的多个能级及精确二次幂缩放，跨越状态判定和 epsilon 区域。
        for t in range(n):
            u[t, :, :, 0] = F32([1, 0, .25, .5, 2**-16, 2**-24][t // 8 % 6])
    return u


def prepare(root):
    root = Path(root)
    root.mkdir()
    for pattern in PATTERNS:
        directory = root / pattern
        directory.mkdir()
        (directory / 'frames.txt').write_text(' '.join(map(str, FRAMES)) + '\n')
        synthetic_input(pattern).tofile(directory / 'input.f32')


def check_case(directory, tables, *, real=False, high_precision=True):
    directory = Path(directory)
    rows = [json.loads(s) for s in (directory / 'frames.jsonl').read_text().splitlines()]
    if not rows:
        raise ValueError('空去相关序列')
    count = integer(rows[0]['instances'], 1, 7)
    model = Model(tables, count)
    inputs, filtered_all, selected_indices = [], [], []
    errors = np.zeros((count, 3))
    magnitudes = np.zeros((count, 3))
    selected, total = 0, 0
    last_au = 1 if real else 0
    for row in rows:
        au = integer(row['au'], 1, 10**7)
        n = integer(row['slots'], 1, 32)
        if au != last_au+1 or row['instances'] != count or len(row['enabled']) != count or any(type(v) is not bool for v in row['enabled']) or type(row['selected']) is not bool:
            raise ValueError('时隙、实例或完整连续 AU 序列错误')
        last_au = au
        prefix = directory / f'au{au:04}'
        def read(tag, shape, dtype='<f4'):
            return array(str(prefix)+'.'+tag, shape, dtype)
        u = read('u.f32', (n, count, 64, 2))
        actual_y = read('y.f32', u.shape)
        kinds = read('kinds.u8', (n, count), 'u1')
        if not np.all(kinds == np.array(tables['cycle'][:count])):
            raise ValueError('实例循环错序')
        inputs.append(u)
        if row['selected']:
            selected += 1
            captures = {tag: read(tag+'.f32', shape) for tag, shape in (
                ('energy', (n, count, 15)), ('gain', (n, count, 15)),
                ('ducker_before', (n, count, 15, 3)), ('ducker_after', (n, count, 15, 3)))}
            captured_filter = read('filtered.f64', u.shape, '<f8')
            for tag, value in [('filter_before', model.canonical)]:
                if read(tag+'.f64', value.shape, '<f8').tobytes() != value.tobytes():
                    raise ValueError('all-pass 帧入口状态不逐位一致')
        frame_filtered = []
        for ts in range(n):
            before = model.ducker.copy()
            e, gain, direct, canonical, y = model.advance(u[ts])
            exact(actual_y[ts], y, '生产去相关同输入重放')
            for r, (first, last, _, _) in enumerate(tables['regions']):
                s = slice(first, last+1)
                errors[:, r] = np.maximum(errors[:, r], np.max(abs(canonical[:, s]-direct[:, s]), axis=(1, 2)))
                magnitudes[:, r] = np.maximum(magnitudes[:, r], np.max(abs(u[ts, :, s]), axis=(1, 2)))
            frame_filtered.append(canonical)
            if row['selected']:
                for tag, value in [('energy', e), ('gain', gain), ('ducker_before', before), ('ducker_after', model.ducker)]:
                    exact(captures[tag][ts], value, tag)
                if captured_filter[ts].tobytes() != canonical.tobytes():
                    raise ValueError('生产 all-pass 观察与独立 canonical 重放不一致')
                selected_indices.append(total+ts)
        if row['selected'] and read('filter_after.f64', model.canonical.shape, '<f8').tobytes() != model.canonical.tobytes():
            raise ValueError('all-pass 帧末状态错误')
        total += n
        filtered_all.extend(frame_filtered)
    if not selected:
        raise ValueError('没有详细窗口')
    bounds = np.array([[filter_bound(np.asarray(tables['coefficients'][r], float)[:, tables['cycle'][de]], magnitudes[de, r]) for r in range(3)] for de in range(count)])
    if np.any(errors > bounds):
        raise ValueError('直接递推与 canonical 超出预先算子界')
    anchors = []
    all_u = np.concatenate(inputs)
    all_filtered = np.asarray(filtered_all)
    if high_precision:
        for de in range(count):
            for b in (0, 6, 7, 22, 23, 47, 48, 63):
                r = next(i for i, (first, last, _, _) in enumerate(tables['regions']) if first <= b <= last)
                a = np.asarray(tables['coefficients'][r], float)[:, tables['cycle'][de]]
                for component in (0, 1):
                    expected = decimal_filter(all_u[:, de, b, component], a, tables['regions'][r][2])
                    error = float(np.max(abs(expected - all_filtered[:, de, b, component])))
                    if error > bounds[de, r]:
                        raise ValueError('80 位直接递推锚点超出算子界')
                    anchors.append(dict(instance=de, subband=b, component=component, samples=total, max_error=error, bound=bounds[de, r]))
    return dict(frames=len(rows), slots=total, instances=count, detailed_frames=selected,
                checked_output_components=total*count*64*2, canonical_replay_bit_exact=True,
                ducker_bit_exact=True, direct_max_error=errors.tolist(), forward_bounds=bounds.tolist(),
                decimal_precision=80 if high_precision else None, decimal_anchors=anchors)


def check_manifest(manifest):
    if manifest.get('schema') != 'macinac4.ajoc-decorr-manifest' or manifest.get('version') != 1 or manifest.get('fit_parameters') is not False:
        raise ValueError('未知协议或启用了拟合')
    tables = manifest['tables']
    if sha(tables['path']) != tables['sha256']:
        raise ValueError('PDF 派生系数摘要错误')
    t = validate_tables(json.loads(Path(tables['path']).read_text()))
    cases = manifest['cases']
    if not cases or len({c['id'] for c in cases}) != len(cases):
        raise ValueError('案例缺失或重复')
    results = {}
    for c in cases:
        if type(c['real']) is not bool:
            raise ValueError('真实输入标志必须为布尔值')
        root = Path(c['directory'])
        files = c['files']
        if not files or {p.name for p in root.iterdir() if p.is_file()} != set(files):
            raise ValueError('完整文件清单不符')
        if any(sha(root / name) != value for name, value in files.items()):
            raise ValueError('捕获摘要改变')
        if not c['real']:
            expected = synthetic_input(c['id'])
            actual = np.concatenate([array(root/f'au{i+1:04}.u.f32', (n, 7, 64, 2)) for i,n in enumerate(FRAMES)])
            exact(actual, expected, '自有夹具输入配方')
        results[c['id']] = check_case(root, t, real=c['real'])
        if results[c['id']]['frames'] != integer(c['expected_frames'],1,10**7):
            raise ValueError('缺少连续输入的末帧')
    return dict(schema='macinac4.ajoc-decorr-comparison', version=1, production_dsp_changed=False, cases=results)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('manifest', nargs='?', type=Path)
    p.add_argument('--output', type=Path)
    p.add_argument('--extract-tables', type=Path)
    p.add_argument('--prepare', type=Path)
    args = p.parse_args()
    if args.prepare:
        prepare(args.prepare)
    elif args.extract_tables:
        args.extract_tables.write_text(json.dumps(extract_tables(), indent=2)+'\n')
    elif args.manifest and args.output:
        manifest = json.loads(args.manifest.read_text())
        result = (check_reference_manifest(manifest) if manifest.get('schema') == 'macinac4.ajoc-decorr-reference'
                  else check_manifest(manifest))
        args.output.write_text(json.dumps(result, indent=2)+'\n')
        print(f"通过 {len(result['cases'])} 组去相关检查")
    else:
        p.error('需要 manifest --output，或 --extract-tables / --prepare')


if __name__ == '__main__':
    main()
