import copy
import hashlib
import json
from decimal import Decimal as D
from fractions import Fraction as F
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import numpy as np

from scripts import check_tna_stability as check


class OracleTests(unittest.TestCase):
    def test_augmented_qr_agrees_with_independent_lstsq_and_exact_arithmetic(self):
        for pattern in ('random','two','near4','near16','ramp','tone','exact4'):
            v=check.synthetic(pattern,42);o=check.qr_oracle(v)
            z=v[:,0].astype(float)+1j*v[:,1];t=np.arange(4,42,2);x=z[t-2];w=z[t-4];y=z[t]
            ridge=abs(np.vdot(x,w))**2/(np.vdot(x,x).real*1048577)
            matrix=np.vstack((np.column_stack((x,w)),[0,np.sqrt(ridge)]))
            independent=np.linalg.lstsq(matrix,np.r_[-y,0],rcond=None)[0]
            actual=np.array([complex(*map(float,a)) for a in o['alpha']])
            np.testing.assert_allclose(actual,independent,rtol=0,atol=1e-10)
            exact=check.exact_solution(v)
            # 有限精度 QR 的理论零可留下约 1e-92 的残差；阈值等号另由有理数裁决。
            np.testing.assert_allclose(actual,np.array([complex(*map(float,a)) for a in exact]),rtol=0,atol=1e-80)
            self.assertLessEqual(float(o['normalized_objective']),float(o['normalized_target_energy'])+1e-15)

    def test_exact_equality_is_not_decided_by_last_decimal_digit(self):
        # QR 的有限十进制精度会在 4 附近产生最后一位误差；有理数严格给出等号。
        for length in (19,22,34,42):
            a=check.exact_solution(check.synthetic('exact4',length))
            self.assertEqual(a[0],(F(-4),F(0)));self.assertEqual(a[1],(F(0),F(0)))
            self.assertEqual(check.norm2(a[0]),16)

    def test_frozen_boundary_input_is_above_four_and_projection_keeps_the_same_objective(self):
        c=next(c for c in check.synthetic_cases() if c['id']=='b217');v=check.values_for(c)
        a=check.exact_solution(v);margin=check.norm2(a[0])-16
        self.assertGreater(margin,0);self.assertLess(float(margin),1e-8)
        candidate=check.projected_candidate(v)
        self.assertGreater(candidate[0]**2+candidate[1]**2,16)
        self.assertLess(check.weighted_error(candidate,check.qr_oracle(v)),D('1e-13'))

    def test_ridge_penalty_is_observable_and_not_a_reference_fit(self):
        v=check.synthetic('near12',42);z=v[:,0].astype(float)+1j*v[:,1];t=np.arange(4,42,2)
        unregularized=np.linalg.lstsq(np.column_stack((z[t-2],z[t-4])),-z[t],rcond=None)[0]
        o=check.qr_oracle(v);raw=np.array([unregularized[0].real,unregularized[0].imag,unregularized[1].real,unregularized[1].imag])
        self.assertGreater(check.weighted_error(raw,o),D(o['weighted_error_bound']))

    def test_quarter_rotation_and_exact_power_scaling_leave_exact_solution_unchanged(self):
        v=check.synthetic('near8',42);expected=check.exact_solution(v)
        rotated=np.column_stack((-v[:,1],v[:,0]))
        self.assertEqual(check.exact_solution(rotated),expected)
        for exponent in (-100,100):
            scaled=np.ldexp(v.astype(float),exponent).astype('<f4')
            self.assertTrue(np.array_equal(scaled.astype(float),np.ldexp(v.astype(float),exponent)))
            self.assertEqual(check.exact_solution(scaled),expected)

    def test_subnormal_quantization_is_not_exact_rescaling(self):
        v=check.synthetic('near8',42);scaled=np.ldexp(v.astype(float),-149).astype('<f4')
        self.assertFalse(np.array_equal(scaled.astype(float),np.ldexp(v.astype(float),-149)))
        self.assertNotEqual(check.exact_solution(v),check.exact_solution(scaled))

    def test_silence_and_a_zero_first_column_remain_defined(self):
        for pattern in ('zero','first','last'):
            v=check.synthetic(pattern,42);exact=check.exact_solution(v);o=check.qr_oracle(v)
            self.assertEqual(exact,((F(0),F(0)),(F(0),F(0))))
            self.assertEqual([[D(x) for x in a] for a in o['alpha']],[[D(0),D(0)],[D(0),D(0)]])


class ProtocolTests(unittest.TestCase):
    def test_solver_metadata_never_reinterprets_unmarked_history(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);self.assertEqual(check.read_solver(root),'legacy_covariance')
            valid=dict(schema='macinac4.tna-stability-replay',version=1,solver='orthogonal_residual')
            path=root/'replay_metadata.json';path.write_text(json.dumps(valid))
            self.assertEqual(check.read_solver(root),'orthogonal_residual')
            for value in [dict(valid,version=True),dict(valid,solver='unknown'),dict(valid,extra=1)]:
                path.write_text(json.dumps(value))
                with self.assertRaises(ValueError):check.read_solver(root)

    def manifest(self):
        rows=check.synthetic_cases()
        for c in rows:c['hf_shift']=0
        return dict(schema='macinac4.tna-stability-input',version=1,cases=rows,source_manifest_sha256=None)

    def test_constructed_coverage_is_complete(self):
        self.assertEqual(len(check.validate_manifest(self.manifest())),1472)

    def test_missing_duplicate_changed_phase_or_layout_is_rejected(self):
        original=self.manifest()
        for change in ('missing','duplicate','phase','layout'):
            m=copy.deepcopy(original)
            if change=='missing':m['cases'].pop()
            elif change=='duplicate':m['cases'].append(m['cases'][0])
            elif change=='phase':m['cases'][-1]['phase']=.123
            else:m['cases'][0]['n']=7
            with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_declared_live_source_cannot_silently_lose_all_rows(self):
        m=self.manifest();m['source_manifest_sha256']='1'*64
        with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_reader_rejects_nonfinite_or_truncated_results(self):
        with tempfile.TemporaryDirectory() as directory:
            p=Path(directory)/'values.f32'
            np.ones(4,dtype='<f4').tofile(p);np.testing.assert_array_equal(check.read_array(p,'<f4',(4,)),1)
            for v in [np.ones(3,dtype='<f4'),np.full(4,np.nan,dtype='<f4'),np.full(4,np.inf,dtype='<f4')]:
                v.tofile(p)
                with self.assertRaises(ValueError):check.read_array(p,'<f4',(4,))


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.output=self.root/'out';self.output.mkdir();(self.root/'inputs').mkdir()
        c=next(copy.deepcopy(c) for c in check.synthetic_cases() if c['n']==6 and c['pattern']=='constant' and c['exponent']==0)
        c['hf_shift']=-5;c['exact_power_scale']=True;v=check.values_for(c);self.case=c
        p=self.root/'inputs'/f"{c['id']}.f32";v.tofile(p);c['input_sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
        self.manifest=dict(cases=[c]);self.write_manifest()
        line=f"{c['id']} {c['n']} {c['offset']} {c['hf_shift']}\n"
        (self.root/'inputs/cases.txt').write_text(line);(self.output/'results.txt').write_text(line)
        o=check.qr_oracle(v);raw=np.array([*map(float,o['covariance']),-1,0,0,0],dtype='<f8');raw.tofile(self.output/f"{c['id']}.f64")
        np.array([-1,0,0,0],dtype='<f4').tofile(self.output/f"{c['id']}.coeff.f32")
        scaled=np.ldexp(v.astype(float),-5).astype('<f4')[4:]
        for i,gamma in enumerate(np.array([0,.6,.9,.98,1],dtype='<f4')):
            (scaled.astype(float)*(1-float(gamma))).astype('<f4').tofile(self.output/f"{c['id']}.hf{i}.f32")

    def write_manifest(self):
        (self.root/'manifest.json').write_text(json.dumps(self.manifest))

    def compare(self):
        with patch.object(check,'validate_manifest',return_value=self.manifest['cases']):
            return check.compare(self.root,self.output)

    def test_untouched_file_fixture_passes(self):
        self.assertEqual(len(self.compare()['cases']),1)

    def test_corruption_is_rejected_by_the_complete_checker(self):
        name=self.case['id']
        for suffix,offset,value in [('coeff.f32',0,np.float32(.25).tobytes()),
                                    ('f64',64,np.float64(.25).tobytes()),
                                    ('hf1.f32',0,np.float32(.25).tobytes())]:
            path=self.output/f'{name}.{suffix}';before=path.read_bytes()
            path.write_bytes(before[:offset]+value+before[offset+len(value):])
            try:
                with self.assertRaises(ValueError):self.compare()
            finally:path.write_bytes(before)
        self.case['exact_power_scale']=False;self.write_manifest()
        try:
            with self.assertRaises(ValueError):self.compare()
        finally:self.case['exact_power_scale']=True;self.write_manifest()
        self.assertEqual(len(self.compare()['cases']),1)

    def test_historical_boundary_output_is_reported_as_a_mismatch(self):
        c=next(copy.deepcopy(c) for c in check.synthetic_cases() if c['id']=='b217');c['hf_shift']=-5
        v=check.values_for(c);p=self.root/'inputs/b217.f32';v.tofile(p);c['input_sha256']=check.sha(p)
        self.manifest=dict(cases=[c]);self.write_manifest()
        for path in [self.root/'inputs/cases.txt',self.output/'results.txt']:path.write_text('b217 32 6 -5\n')
        # 冻结调查时的原始输出，只测试报告器能识别历史缺陷，不锁定生产继续出错。
        raw=np.array([-.9480944404281032,-3.886015559922867,3.1041712588232267e-9,-1.5520856294116133e-9])
        cov=list(map(float,check.qr_oracle(v)['covariance']))
        np.array([*cov,*raw],dtype='<f8').tofile(self.output/'b217.f64')
        stored=raw.astype('<f4');stored.tofile(self.output/'b217.coeff.f32')
        z=np.ldexp(v.astype(float),-5).astype('<f4');z=z[:,0].astype(float)+1j*z[:,1]
        for i,g in enumerate(np.array([0,.6,.9,.98,1],dtype='<f4')):
            ideal=z[4:]+complex(*stored[:2])*float(g)*z[2:-2]+complex(*stored[2:])*float(g)**2*z[:-4]
            np.column_stack((ideal.real,ideal.imag)).astype('<f4').tofile(self.output/f'b217.hf{i}.f32')
        report=self.compare()
        self.assertEqual(report['production_clamp_mismatches'],1)
        self.assertEqual(report['projected_clamp_mismatches'],0)


if __name__=='__main__':unittest.main()
