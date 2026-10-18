import json
from pathlib import Path
import tempfile
import unittest
import numpy as np
from scripts import check_aspx_hf_replay as check


def pack(value):
    return (value << 62) | (value << 60)


def case():
    return dict(id='probe',kind='tna_probe',scope='synthetic',call=-1,
                first=0,last=38,ats=16,factor=2,preflat=1,master=1,start=6,stop=1,
                noise=3,xover=255,groups=2,patches=2,source_lo=24,source_hi=36,
                sbx=36,sbz=56,noise_borders=[36,44,56],targets=[36,44,56],sources=[28,24],
                modes=pack(2),previous_modes=pack(1),previous_chirp=[.61,.61],
                amplitude_exp=0,pattern='random')


class GainConventionTests(unittest.TestCase):
    def test_unmarked_history_is_literal_and_new_metadata_is_explicit(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            self.assertEqual(check.read_gain_convention(root), 'mean_minus_slope')
            marker = root / 'replay_metadata.json'
            for convention in check.GAIN_CONVENTIONS:
                marker.write_text(json.dumps(dict(schema='macinac4.hf-replay', version=1,
                                                 preflatten_gain_convention=convention)))
                self.assertEqual(check.read_gain_convention(root), convention)

    def test_invalid_metadata_does_not_silently_fall_back_to_history(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            valid = dict(schema='macinac4.hf-replay', version=1,
                         preflatten_gain_convention='slope_minus_mean')
            for content in [dict(valid, version=True), dict(valid, version=2),
                            dict(valid, schema='other'), dict(valid, extra=0),
                            dict(valid, preflatten_gain_convention='unknown'), {}]:
                (root/'replay_metadata.json').write_text(json.dumps(content))
                with self.assertRaises(ValueError):
                    check.read_gain_convention(root)

    def test_a_nonflat_gain_cannot_be_relabelled_as_the_other_convention(self):
        r=case();r.update(kind='preflat_probe',pattern='rising')
        q,_=check.synthetic(r)
        old=check.gain_model(q,36,0,32,2**23)
        new=check.gain_model(q,36,0,32,2**23,'slope_minus_mean')
        check.checked_gain_metrics(old,old.copy())
        check.checked_gain_metrics(new,new.copy())
        np.testing.assert_allclose(old*new,np.ones(36),rtol=2e-7,atol=0)
        with self.assertRaisesRegex(ValueError,'声明'):
            check.checked_gain_metrics(old,new)

    def test_flat_gains_remain_equivalent_under_either_convention(self):
        q=np.ones((40,64,2),dtype='<f4')
        a=check.gain_model(q,36,0,32,1)
        b=check.gain_model(q,36,0,32,1,'slope_minus_mean')
        check.checked_gain_metrics(a,b)


class ParameterTests(unittest.TestCase):
    def test_binary_parameters_are_fixed_and_finite(self):
        r=case();check.validate_case(r)
        self.assertEqual(len(check.parameter_bytes(r)),176)
        self.assertEqual(check.parameter_bytes(r)[:4],b'\0'*4)

    def test_bad_time_units_and_intervals_rejected(self):
        for key,value in [('factor',1),('ats',32),('first',1),('last',40),('first',38)]:
            r=case();r[key]=value
            with self.assertRaises(ValueError):check.validate_case(r)

    def test_bad_tables_and_sources_rejected(self):
        for key,value in [('sources',[35,24]),('targets',[36,36,56]),('noise_borders',[35,44,56]),
                          ('source_lo',23),('source_hi',37),('xover',8)]:
            r=case();r[key]=value
            with self.assertRaises(ValueError):check.validate_case(r)

    def test_nonfinite_history_and_invalid_identity_rejected(self):
        for value in (float('nan'),float('inf'),-.1,1.1):
            r=case();r['previous_chirp']=[value,.61]
            with self.assertRaises(ValueError):check.validate_case(r)
        r=case();r['id']='../probe'
        with self.assertRaises(ValueError):check.validate_case(r)

    def test_incomplete_manifest_is_rejected(self):
        m=dict(schema='macinac4.normalized-hf-replay',version=1,cases=[case()],reference_runs=['r0','r1','r2','r3'])
        with self.assertRaisesRegex(ValueError,'覆盖'):check.validate_manifest(m)

    def test_preflat_probe_must_disable_predictor(self):
        r=case();r['kind']='preflat_probe';r['pattern']='rising'
        m=dict(schema='macinac4.normalized-hf-replay',version=1,cases=[r])
        with self.assertRaisesRegex(ValueError,'隔离 TNA'):check.validate_manifest(m)


class CompletionTests(unittest.TestCase):
    def test_complete_ordered_rows_are_accepted(self):
        row=dict(id='a',finite=True,save=0)
        self.assertEqual(check.completed_rows([row,dict(complete=True,cases=1)],['a']),[row])

    def test_missing_completion_or_wrong_identity_is_rejected(self):
        row=dict(id='a',finite=True,save=0)
        for rows in ([],[row],[dict(complete=True,cases=1)],
                     [row,dict(complete=True,cases=2)],
                     [row,dict(id='b',finite=True,save=0),dict(complete=True,cases=2)]):
            with self.assertRaises(ValueError):check.completed_rows(rows,['a'])
        with self.assertRaises(ValueError):
            check.completed_rows([row,dict(complete=True,cases=1)],['b'])

    def test_nonfinite_or_unsaved_row_is_rejected(self):
        for row in (dict(id='a',finite=False,save=0),dict(id='a',finite=True,save=1)):
            with self.assertRaises(ValueError):
                check.completed_rows([row,dict(complete=True,cases=1)],['a'])


class NumericalTests(unittest.TestCase):
    def test_covariance_has_correct_history_stride_and_conjugate(self):
        q=np.zeros((40,64,2),'<f4');h=np.zeros((4,40,2),'<f4')
        t=np.arange(42);z=(t+1)+1j*np.cos(t*.2)
        h[:,0,0],h[:,0,1]=z[:4].real,z[:4].imag
        q[:38,0,0],q[:38,0,1]=z[4:].real,z[4:].imag
        c=check.covariance(q,h)[0]
        actual=np.concatenate((check.complex_values(h)[:,0],check.complex_values(q[:38,:40])[:,0]))
        expected=sum(actual[t]*actual[t-2].conjugate() for t in range(4,42,2))
        self.assertAlmostEqual(c[0],expected.real,places=10)
        self.assertAlmostEqual(c[1],expected.imag,places=10)
        self.assertGreater(abs(c[1]),1)

    def test_solver_decoupled_covariance_has_known_coefficients(self):
        c=[-1,-.5,.25,-.125,1,0,0,1]
        np.testing.assert_array_equal(check.regularized_solver(c),[1,.5,-.25,.125])

    def test_regularization_changes_near_singular_selection(self):
        c11=c22=.5;c12=.5*(1-2**-20);a0=-.5+.125j;a1=.25+.0625j
        c01=-a0*c11-a1*c12;c02=-a0*c12-a1*c22
        c=[c01.real,c01.imag,c02.real,c02.imag,c11,c12,0,c22]
        out=check.regularized_solver(c)
        self.assertGreater(out[2],.15);self.assertLess(out[2],.18)
        residual=c01+complex(*out[:2])*c11+complex(*out[2:])*c12
        self.assertLess(abs(residual),3e-8)

    def test_solver_zero_and_unstable_cases_are_bounded(self):
        np.testing.assert_array_equal(check.regularized_solver([0]*8),[0]*4)
        np.testing.assert_array_equal(check.regularized_solver([-4,0,0,0,1,0,0,1]),[0]*4)

    def test_synthetic_inputs_are_deterministic_and_scale_by_power_of_two(self):
        r=case();q,h=check.synthetic(r);q2,h2=check.synthetic(r)
        self.assertTrue(check.identical(q,q2));self.assertTrue(check.identical(h,h2))
        r['amplitude_exp']=10;high,high_h=check.synthetic(r)
        self.assertTrue(check.identical(high,q*1024));self.assertTrue(check.identical(high_h,h*1024))

    def test_flat_envelope_produces_unit_gain(self):
        r=case();r.update(kind='preflat_probe',pattern='flat',amplitude_exp=20)
        q,_=check.synthetic(r);g=check.gain_model(q,36,0,32,2**23)
        np.testing.assert_allclose(g,np.ones(36),atol=2e-7)

    def test_gain_direction_is_independent_of_reference_output(self):
        for pattern in ('rising','falling'):
            r=case();r.update(kind='preflat_probe',pattern=pattern,amplitude_exp=20)
            q,_=check.synthetic(r);g=check.gain_model(q,36,0,32,1)
            x=q[:32,:36];bands=list(range(36));base=check.slope(x,bands)
            divided=check.slope(x/g[None,:,None],bands)
            multiplied=check.slope(x*g[None,:,None],bands)
            self.assertAlmostEqual(divided/base,2,places=6)
            self.assertLess(abs(multiplied),1e-6)

    def test_floor_effect_depends_on_declared_input_units(self):
        r=case();r.update(kind='preflat_probe',pattern='rising',amplitude_exp=-10)
        q,_=check.synthetic(r)
        small=check.gain_model(q,36,0,32,1);scaled=check.gain_model(q,36,0,32,2**23)
        self.assertLess(np.max(abs(small-1)),.001)
        self.assertGreater(np.max(abs(scaled-1)),10)


if __name__=='__main__':unittest.main()
