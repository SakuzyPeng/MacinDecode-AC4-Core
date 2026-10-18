import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

from scripts import check_qmf_phase_chain as check


def toy_window():
    a = np.hanning(641)[:640]
    return a*np.where((np.arange(640)//128)%2,-1,1)


def manifest():
    cases = check.synthetic_cases()
    for c in cases:
        c['input_sha256'] = '0'*64
    for scope in ('tone','music100','music33'):
        for owner in range(-1,9):
            for layout in (('full',) if owner==-1 else ('full','patch')):
                c = dict(id=f'r{scope}{owner+1}{layout[0]}',kind='real',scope=scope,owner=owner,
                         layout=layout,slots=106,checkpoint=74,delay=6,source_start_slot=128)
                if layout == 'patch':
                    c['config'] = list(check.PATCH_CONFIGS[0])
                    c['routes'] = check.routes_for(c['config'])
                else:
                    c['routes'] = [[b,b] for b in range(64)]
                cases.append(c)
    return dict(schema='macinac4.qmf-phase-chain',version=1,cases=cases)


class TimeDomainTests(unittest.TestCase):
    def test_unfolded_analysis_uses_the_correct_sample_and_modulation_time(self):
        x = np.arange(24*64,dtype=float)/1024
        for lag in (0,63,64,127,128,319,320,639):
            w = np.zeros(640)
            w[lag] = .75
            q = check.analyse_fir(x,w)
            for slot in (0,9,15,23):
                position = slot*64+63-lag
                value = x[position] if position>=0 else 0
                expected = .75*value*np.exp(1j*check.OMEGA*((lag%128)-.5))
                np.testing.assert_allclose(q[slot],expected,atol=2e-13,rtol=0)

    def test_unfolded_synthesis_matches_individual_polyphase_positions(self):
        q = np.zeros((16,64),complex)
        q[2,7] = 1+2j
        for lag in (0,63,64,127,128,319,320,639):
            w = np.zeros(640)
            w[lag] = -.75
            y = check.synthesise_fir(q,w)
            expected = np.zeros_like(y)
            angle = check.OMEGA[7]*((lag%128)-127.5)
            expected[2*64+lag] = -.75*np.real((1+2j)*np.exp(1j*angle))/64
            np.testing.assert_allclose(y,expected,atol=2e-15,rtol=0)

    def test_analytic_carrier_predicts_the_entire_steady_response_without_fitting(self):
        w = toy_window()
        source,target = 18,44
        omega = check.OMEGA[source]+np.pi/256
        nu = check.OMEGA[target]+np.pi/256
        x = np.cos(omega*np.arange(128*64)+.37)
        q = check.analyse_fir(x,w)
        y = check.synthesise_fir(check.map_bands(q,[(source,target)],6),w)
        actual = check.measured_phasor(y,nu)
        expected = check.carrier_phasor(w,source,target,omega,.37,nu,6)
        self.assertLess(abs(actual-expected),1e-9)
        wrong = check.carrier_phasor(w,source,target,omega,.37,nu,0)
        self.assertGreater(abs(actual-wrong),.1)

    def test_odd_distance_does_not_realise_the_requested_quarter_bin_translation(self):
        w = toy_window()
        source,target = 18,19
        omega = check.OMEGA[source]+np.pi/256
        nu = check.OMEGA[target]+np.pi/256
        self.assertEqual(check.carrier_phasor(w,source,target,omega,.37,nu),0j)
        x = np.cos(omega*np.arange(128*64)+.37)
        q = check.analyse_fir(x,w)
        y = check.synthesise_fir(check.map_bands(q,[(source,target)],0),w)
        requested = check.measured_phasor(y,nu)
        wrong_frequency = check.carrier_phasor(w,source,target,omega,.37,omega)
        self.assertNotEqual(wrong_frequency,0j)
        self.assertLess(abs(check.measured_phasor(y,omega)-wrong_frequency),1e-9)
        self.assertGreater(abs(wrong_frequency),10000*abs(requested))
        self.assertLess(abs(requested),1e-9)

    def test_fixed_delay_is_not_an_adjustable_phase_fit(self):
        q = np.arange(12*64,dtype=float).reshape(12,64)+1j
        out = check.map_bands(q,[(28,36),(29,37)],6)
        self.assertFalse(np.any(out[:6]))
        np.testing.assert_array_equal(out[6:,36],q[:-6,28])
        with self.assertRaises(ValueError):
            check.map_bands(q,[(28,36)],1)

    def test_duplicate_targets_and_nonfinite_inputs_are_rejected(self):
        with self.assertRaises(ValueError):
            check.map_bands(np.ones((16,64)),[(28,36),(29,36)],0)
        with self.assertRaises(ValueError):
            check.analyse_fir(np.full(64,np.nan),np.zeros(640))
        with self.assertRaises(ValueError):
            check.synthesise_fir(np.full((16,64),np.inf),np.zeros(640))

    def test_bound_rejects_mutation_and_does_not_accept_nan(self):
        expected = np.ones(10)
        check.require_bound(expected,expected.copy(),0,'test')
        for actual,bound in [(expected*1.01,1e-4),(expected,np.nan),(expected,-1),
                             (np.full(10,np.nan),1)]:
            with self.assertRaises(ValueError):
                check.require_bound(expected,actual,bound,'test')


class CoverageTests(unittest.TestCase):
    def test_all_declared_cases_are_present(self):
        m = manifest()
        self.assertEqual(len(check.validate_manifest(m)),1025)
        self.assertEqual(len(check.synthetic_cases()),968)

    def test_missing_or_duplicate_cases_are_rejected(self):
        m = manifest()
        for cases in [m['cases'][1:],m['cases']+[m['cases'][0]]]:
            with self.assertRaises(ValueError):
                check.validate_manifest(dict(m,cases=cases))

    def test_changed_frequency_or_mapping_recipe_is_rejected(self):
        for key,value in [('phase',0.99),('offset_bins',.1),('routes',[[0,9]])]:
            m = manifest()
            next(c for c in m['cases'] if c['kind']=='tone')[key] = value
            with self.assertRaises(ValueError):
                check.validate_manifest(m)

    def test_missing_real_owner_and_shortened_history_are_rejected(self):
        m = manifest()
        with self.assertRaises(ValueError):
            check.validate_manifest(dict(m,cases=m['cases'][:-1]))
        m = copy.deepcopy(m)
        m['cases'][-1]['checkpoint'] = 64
        with self.assertRaises(ValueError):
            check.validate_manifest(m)

    def test_constructed_impulse_and_complete_flush_are_fixed(self):
        c = check.synthetic_cases()[0]
        x = check.synthetic_input(c)
        self.assertEqual(np.count_nonzero(x),1)
        self.assertEqual(x[128],1)
        self.assertFalse(np.any(x[c['checkpoint']*64:]))


class ArtifactTests(unittest.TestCase):
    """独立 FIR 的数学性质由前面的稀疏抽头验证；此处验证文件门禁实际连通。"""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.data = self.root/'data'
        self.output = self.root/'output'
        self.data.mkdir()
        self.output.mkdir()
        self.case = check.synthetic_cases()[0]
        x = check.synthetic_input(self.case)
        self.case['input_sha256'] = hashlib.sha256(x.tobytes()).hexdigest()
        w = toy_window().astype('<f4')
        self.window_hash = hashlib.sha256(w.tobytes()).hexdigest()
        q = check.components(check.analyse_fir(x,w))
        values = check.complex_values(q)
        y = check.synthesise_fir(values,w).astype('<f4')
        point = self.case['checkpoint']
        history = np.zeros(640,dtype='<f4')
        history[:point*64] = x[:point*64][::-1]
        matrix = np.exp(1j*(np.arange(128)[:,None]-127.5)*check.OMEGA)/64
        synthesis_history = np.zeros((10,128),dtype='<f4')
        for lag in range(point):
            synthesis_history[lag] = np.real(matrix@values[point-1-lag])
        self.manifest = self.root/'manifest.json'
        self.manifest.write_text(json.dumps({'cases':[self.case]}))
        (self.data/'cases.txt').write_text(check.case_line(self.case)+'\n')
        x.tofile(self.data/f"{self.case['id']}.input.f32")
        for suffix,array in [('analysis',q),('mapped',q),('pcm',y),
                             ('analysis_state',history),('synthesis_state',synthesis_history)]:
            array.tofile(self.output/f"{self.case['id']}.{suffix}.f32")
        w.tofile(self.output/'qwin.f32')
        routes = ' '.join(f'{p} {k}' for p,k in self.case['routes'])
        (self.output/'results.txt').write_text(f"{self.case['id']} 40 8 0 64 {routes}\n")

    def compare(self):
        # 只缩小本测试的覆盖清单；完整清单的缺失/重复拒绝由 CoverageTests 独立验证。
        with patch.object(check,'validate_manifest',return_value=[self.case]), \
                patch.object(check,'WINDOW_SHA256',self.window_hash):
            return check.compare(self.manifest,self.data,self.output)

    def test_unchanged_artifacts_pass(self):
        self.assertEqual(len(self.compare()['cases']),1)

    def test_corrupted_artifacts_do_not_pass_the_file_checker(self):
        name = self.case['id']
        changes = [
            (self.output/f'{name}.analysis.f32',4*128,np.float32(.1).tobytes()),
            (self.output/f'{name}.mapped.f32',4*128,np.float32(.1).tobytes()),
            (self.output/f'{name}.pcm.f32',0,np.float32(.1).tobytes()),
            (self.output/f'{name}.analysis_state.f32',0,np.float32(.1).tobytes()),
            (self.output/f'{name}.synthesis_state.f32',0,np.float32(.1).tobytes()),
            (self.output/f'{name}.pcm.f32',0,np.float32(np.nan).tobytes()),
            (self.data/f'{name}.input.f32',0,np.float32(.1).tobytes()),
        ]
        for path,offset,replacement in changes:
            original = path.read_bytes()
            with self.subTest(file=path.name,replacement=replacement):
                path.write_bytes(original[:offset]+replacement+original[offset+len(replacement):])
                try:
                    with self.assertRaises(ValueError):
                        self.compare()
                finally:
                    path.write_bytes(original)
        for path in (self.output/'results.txt',self.output/f'{name}.pcm.f32'):
            original = path.read_bytes()
            path.write_bytes(b'')
            try:
                with self.assertRaises(ValueError):
                    self.compare()
            finally:
                path.write_bytes(original)
        self.assertEqual(len(self.compare()['cases']),1)


if __name__=='__main__':
    unittest.main()
