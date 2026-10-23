"""不携带媒体或规范表的检查器判据测试。"""
import copy
import unittest
import tempfile
from pathlib import Path
import numpy as np
from scripts.check_ajoc_decorrelator import Model, duck, energy, decimal_filter, filter_bound, validate_tables, synthetic_input
from scripts.check_ajoc_decorrelator import observed_reference_duck, array


def owned_tables():
    # 自有稳定多项式；不是规范表副本。
    return dict(schema='macinac4.ajoc-decorr-tables', version=1,
                regions=[[0,6,7,7],[7,22,10,4],[23,63,12,2]],
                mapping=[min(b*15//64,14) for b in range(64)], cycle=[0,2,1,0,2,1,0],
                coefficients=[[[1,1,1],[.25,-.125,.5]]+[[0,0,0] for _ in range(n-2)] for n in (8,5,3)])


class DecorrelatorCheckerTests(unittest.TestCase):
    def test_complex_energy_uses_input_and_adds_whole_band(self):
        u=np.zeros((1,64,2),'<f4');u[0,0]=[3,4];u[0,1]=[0,2]
        mapping=[0]*2+[min(1+(b-2)//5,14) for b in range(2,64)]
        self.assertEqual(float(energy(u,mapping)[0,0]),29)

    def test_impulse_is_not_ducked_but_its_tail_is(self):
        state,gain=duck(np.zeros((1,1,3),'<f4'),np.ones((1,1),'<f4'))
        np.testing.assert_array_equal(state,[[[1,.25,0]]]);self.assertEqual(gain[0,0],1)
        state,gain=duck(state,np.zeros((1,1),'<f4'))
        self.assertGreater(gain[0,0],0);self.assertLess(gain[0,0],1)
        self.assertEqual(state[0,0,1],np.float32(.1875))

    def test_threshold_is_strict_and_one_ulp_distinguishes_it(self):
        previous=np.array([[[0,.1875,.125]]],'<f4');e=np.zeros((1,1),'<f4')
        self.assertEqual(duck(previous,e)[1][0,0],1)
        previous[...,2]=np.nextafter(previous[...,2],np.float32(np.inf))
        self.assertLess(duck(previous,e)[1][0,0],1)

    def test_epsilon_is_not_scale_invariant(self):
        s=np.array([[[1,.25,.5]]],'<f4');e=np.zeros((1,1),'<f4')
        self.assertLess(duck(s*np.float32(2**-40),e)[1][0,0],duck(s,e)[1][0,0])

    def test_rejects_bad_state_and_energy(self):
        for e in [np.array([[-1]],'<f4'),np.array([[np.nan]],'<f4')]:
            with self.assertRaises(ValueError):duck(np.zeros((1,1,3),'<f4'),e)
        with self.assertRaises(ValueError):duck(np.zeros((1,1,2),'<f4'),np.zeros((1,1),'<f4'))

    def test_all_seven_instances_and_direct_high_precision(self):
        t=owned_tables();model=Model(t,7);u=synthetic_input('burst');out=[]
        for slot in u:
            _,_,direct,canonical,_=model.advance(slot);out.append(canonical)
            np.testing.assert_allclose(direct,canonical,rtol=0,atol=1e-14)
        actual=np.array(out)
        for de in range(7):
            for r,b in enumerate((0,7,23)):
                a=np.asarray(t['coefficients'][r],float)[:,t['cycle'][de]]
                expected=decimal_filter(u[:,de,b,0],a,t['regions'][r][2])
                self.assertLessEqual(np.max(abs(actual[:,de,b,0]-expected)),filter_bound(a,np.max(abs(u[:,de,b,0]))))

    def test_causal_delay_and_independent_instances(self):
        m=Model(owned_tables(),2);u=np.zeros((2,64,2),'<f4');u[0,23,0]=1;values=[]
        for i in range(20):
            _,_,direct,_,_=m.advance(u if i==0 else np.zeros_like(u));values.append(direct)
        values=np.array(values)
        self.assertFalse(np.any(values[:13]));self.assertNotEqual(values[13,0,23,0],0)
        self.assertFalse(np.any(values[:,1]))

    def test_invalid_input_does_not_advance_model(self):
        m=Model(owned_tables(),1);before=copy.deepcopy(m.__dict__)
        with self.assertRaises(ValueError):m.advance(np.full((1,64,2),np.inf,'<f4'))
        np.testing.assert_array_equal(m.canonical,before['canonical'])
        np.testing.assert_array_equal(m.ducker,before['ducker'])

    def test_rejects_missing_tables_wrong_cycle_and_mapping(self):
        for key,value in [('cycle',[0]*7),('mapping',[0]*64),('coefficients',[[[1]]]*3),('regions',[])]:
            t=owned_tables();t[key]=value
            with self.assertRaises(ValueError):validate_tables(t)

    def test_reference_energy_ceiling_is_a_separate_observation(self):
        state=np.zeros((1,1,3),'<f4');e=np.array([[2]],'<f4')
        self.assertEqual(observed_reference_duck(state,e)[0][0,0,0],1)
        self.assertEqual(duck(state,e)[0][0,0,0],2)

    def test_reference_no_epsilon_does_not_replace_production_floor(self):
        state=np.array([[[1,.25,.5]]],'<f4')*np.float32(2**-40);e=np.zeros((1,1),'<f4')
        self.assertGreater(observed_reference_duck(state,e)[1][0,0],duck(state,e)[1][0,0])

    def test_rejects_truncation_extra_bytes_and_nonfinite_capture(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'capture.f32'
            for values in [np.array([],dtype='<f4'),np.zeros(2,'<f4'),np.array([np.inf],'<f4')]:
                values.tofile(p)
                with self.assertRaises(ValueError):array(p,(1,))


if __name__=='__main__':unittest.main()
