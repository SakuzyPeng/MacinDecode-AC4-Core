import copy
import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

from scripts import check_aspx_tone_phase as check
from scripts.check_qmf_phase_chain import synthesise_fir


def window():
    return np.hanning(641)[:640]*np.where((np.arange(640)//128)%2,-1,1)


def case_rows():
    rows=[]
    for c in check.cases():
        last=[(c['seed']+(i+1)*15*c['factor']+(c['carry']*c['factor'] if i<3 else 0)-1)%4 for i in range(4)]
        rows.append(' '.join(map(str,[c['id'],*[c[k] for k in ('band','seed','factor','carry','profile')],*last,0])))
    return rows


class TonePhaseTests(unittest.TestCase):
    def test_closed_form_agrees_with_full_complex_fir_at_every_band(self):
        w=window();level=np.zeros(64);level[3:19]=.5;level[27:43]=1
        for band in range(64):
            seed=band%4
            q=np.zeros((64,64),complex);q[:,band]=check.phase_sequence(level,band,seed,3)
            expected=synthesise_fir(q,w)
            actual=check.tone_pcm(level,w,band,seed,3)
            np.testing.assert_allclose(actual,expected,rtol=0,atol=3e-13)

    def test_absolute_band_parity_determines_the_direction(self):
        a=check.phase_sequence(np.ones(4),18,0)
        b=check.phase_sequence(np.ones(4),19,0)
        np.testing.assert_array_equal(a,[1,1j,-1,-1j])
        np.testing.assert_array_equal(b,np.conj(a))

    def test_one_index_changes_carrier_phase_without_moving_the_envelope(self):
        w=window();level=np.ones(80);level[:8]=0;level[60:]=0
        for band in (0,1,18,19,60,61,62,63):
            a=check.tone_pcm(level,w,band,0);b=check.tone_pcm(level,w,band,1)
            np.testing.assert_allclose(b[20*64:40*64],a[21*64:41*64],rtol=0,atol=3e-13)
            self.assertGreater(np.max(abs(b[:-64]-a[64:])),1e-4)
            # 让包络一起延迟一时隙，才补足全区间平移所需的条件。
            delayed=np.r_[0,level[:-1]]
            shifted=check.tone_pcm(delayed,w,band,0)
            np.testing.assert_allclose(b[:-64],shifted[64:],rtol=0,atol=3e-13)

    def test_silence_does_not_reset_phase_and_explicit_initialization_does(self):
        level=np.r_[np.zeros(3),np.ones(8)]
        actual=check.phase_sequence(level,19,1)
        self.assertEqual(actual[3],1)
        restarted=check.phase_sequence(level,19,1,3)
        self.assertEqual(restarted[3],-1j)
        self.assertGreater(abs(actual[3]-restarted[3]),1)

    def test_fixed_reference_basis_is_not_an_initial_index_change(self):
        w=window();level=np.r_[np.zeros(3),np.ones(29),np.zeros(16)]
        for band in (0,1,36,37,62,63):
            reference=check.tone_pcm(level,w,band,0,3,np.pi-check.OMEGA[band]/2)
            local_advance=check.tone_pcm(level,w,band,1,3)
            self.assertGreater(np.max(abs(reference-local_advance)),1e-4)

    def test_bound_rejects_nonfinite_and_wrong_physical_phase(self):
        w=window();level=np.ones(64)
        expected=check.tone_pcm(level,w,36,1)
        bound=check.synthesis_bound(w,level)
        check.require_bound(expected,expected.copy(),bound,'positive control')
        with self.assertRaises(ValueError):
            check.require_bound(expected,check.tone_pcm(level,w,36,0),bound,'phase mutation')
        for invalid in (np.full(64,np.nan),np.full(64,np.inf),np.empty(0)):
            with self.assertRaises(ValueError):check.tone_pcm(invalid,w,36,1)


class CoverageTests(unittest.TestCase):
    def test_complete_rows_cover_legal_tone_bands_and_both_phases(self):
        rows=check.validate_rows('\n'.join(case_rows()))
        self.assertEqual(len(rows),832)
        self.assertEqual({c['band'] for c in rows},set(range(10,62)))
        self.assertEqual({c['seed'] for c in rows},{0,1})

    def test_missing_duplicate_or_mislabelled_rows_are_rejected(self):
        original=case_rows()
        for rows in [original[:-1],original+[original[0]],[original[0].replace('b10','b09')]+original[1:]]:
            with self.assertRaises(ValueError):check.validate_rows('\n'.join(rows))

    def test_wrong_parameters_state_or_nonfinite_score_are_rejected(self):
        for index,value in [(1,'11'),(3,'3'),(6,'0'),(10,'NaN')]:
            rows=case_rows();p=rows[0].split();p[index]=value;rows[0]=' '.join(p)
            with self.assertRaises(ValueError):check.validate_rows('\n'.join(rows))

    def test_envelope_profiles_preserve_the_declared_carry_time(self):
        c=dict(band=36,seed=1,factor=2,carry=3,profile=1)
        e=check.envelope(c)
        np.testing.assert_array_equal(e[:36],0)
        np.testing.assert_array_equal(e[36:66],1)
        np.testing.assert_array_equal(e[66:96],0)
        np.testing.assert_array_equal(e[96:120],.25)
        self.assertFalse(np.any(e[120:]))
        c['profile']=0
        np.testing.assert_array_equal(check.envelope(c)[:120],1)
        other=copy.deepcopy(c);other['carry']=0
        np.testing.assert_array_equal(check.envelope(c),check.envelope(other))


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)
        self.cases=[c for c in check.cases() if c['band']==10 and c['factor']==1 and c['profile']==0]
        selected={c['id'] for c in self.cases}
        rows=[row for row in case_rows() if row.split()[0] in selected]
        (self.root/'cases.txt').write_text('\n'.join(rows)+'\n')
        self.w=window().astype('<f4');self.w.tofile(self.root/'qwin.f32')
        for c in self.cases:
            level=check.envelope(c)
            q=np.zeros((len(level),64),complex);q[:,10]=check.phase_sequence(level,10,c['seed'])
            check.components(q).tofile(self.root/f"{c['id']}.qmf.f32")
            check.tone_pcm(level,self.w,10,c['seed']).astype('<f4').tofile(self.root/f"{c['id']}.pcm.f32")

    def compare(self):
        with patch.object(check,'cases',return_value=self.cases), \
                patch.object(check,'WINDOW_SHA256',hashlib.sha256(self.w.tobytes()).hexdigest()):
            return check.compare(self.root)

    def test_unchanged_file_fixture_passes(self):
        self.assertEqual(len(self.compare()['cases']),4)

    def test_wrong_qmf_pcm_nan_and_truncation_fail_the_actual_checker(self):
        name=self.cases[0]['id']
        for suffix,value in [('qmf',.1),('pcm',.1),('pcm',np.nan)]:
            p=self.root/f'{name}.{suffix}.f32';original=p.read_bytes()
            p.write_bytes(np.float32(value).tobytes()+original[4:])
            try:
                with self.assertRaises(ValueError):self.compare()
            finally:p.write_bytes(original)
        p=self.root/f'{name}.pcm.f32';original=p.read_bytes();p.write_bytes(original[:-4])
        try:
            with self.assertRaises(ValueError):self.compare()
        finally:p.write_bytes(original)
        self.assertEqual(len(self.compare()['cases']),4)


if __name__=='__main__':unittest.main()
