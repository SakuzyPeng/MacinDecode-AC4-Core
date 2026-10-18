import unittest
import numpy as np
from scripts import check_aspx_downstream_replay as check


def manifest():
    cases=[]
    for scope in ['tone','music100','music33']:
        for c in range(9):
            cases.append(dict(id=f'{scope}c{c}',scope=scope,call=c,master=0,start=0,stop=0,noise=0,xover=0,
                sbx=4,sbz=6,mapped_preflat_gain=[1.,2.],low=[4,6],high=[4,5,6],
                signal_borders=[0,16],freq_res=[0],envelopes=[[0,32]],first=0,last=32))
    return dict(schema='macinac4.hf-downstream-replay',version=1,modes=check.MODES,cases=cases)


class ManifestTests(unittest.TestCase):
    def test_complete_manifest_and_mode_count(self):
        m=manifest();check.validate_manifest(m)
        self.assertEqual(len(check.expected_keys(m)),864)
        self.assertEqual(len(check.parameter_lines(m)),432)

    def test_missing_call_and_duplicate_identity(self):
        m=manifest();m['cases'].pop()
        with self.assertRaises(ValueError):check.validate_manifest(m)
        m=manifest();m['cases'][1]=dict(m['cases'][0])
        with self.assertRaises(ValueError):check.validate_manifest(m)
        m=manifest();m['cases'][1]['id']=m['cases'][0]['id']
        with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_time_unit_and_frequency_source_errors(self):
        for key,value in [('first',1),('last',16),('envelopes',[[0,16]]),('low',[5,6]),('freq_res',[2])]:
            m=manifest();m['cases'][0][key]=value
            with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_invalid_gain_and_path_identity(self):
        for gain in [[0,1],[float('nan'),1],[float('inf'),1],[1]]:
            m=manifest();m['cases'][0]['mapped_preflat_gain']=gain
            with self.assertRaises(ValueError):check.validate_manifest(m)
        m=manifest();m['cases'][0]['id']='../x'
        with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_incomplete_local_estimates_and_nonfinite_values(self):
        for text in ['', 'tonec0v0i0 0 0 7fc00000', 'tonec0v0i0 1 0 3f800000',
                     'tonec0v0i0 0 0 bf800000']:
            with self.assertRaises(ValueError):check.parse_estimates(text,manifest())

    def test_complete_estimates_then_mutated_identity(self):
        m=manifest();lines=[]
        for c in m['cases']:
            for v in range(8):
                for i in range(2):
                    for b in range(2):lines.append(f"{c['id']}v{v}i{i} 0 {b} 3f800000")
        self.assertEqual(len(check.parse_estimates('\n'.join(lines),m)),864)
        with self.assertRaises(ValueError):check.parse_estimates('\n'.join(lines+[lines[0]]),m)
        changed=lines.copy();changed[0]=changed[0].replace('tonec0','tonec1')
        with self.assertRaises(ValueError):check.parse_estimates('\n'.join(changed),m)


class EnergyTests(unittest.TestCase):
    def test_complex_power_is_averaged_over_qmf_slots(self):
        q=np.array([[[3.,4.]],[[0.,5.]]],dtype='<f4')
        np.testing.assert_array_equal(check.estimate_power(q,[0,1],True),[25])

    def test_group_mean_is_distinct_from_per_band_estimate(self):
        q=np.zeros((4,2,2),'<f4');q[:,0,0]=2;q[:,1,0]=4
        np.testing.assert_array_equal(check.estimate_power(q,[0,2],True),[4,16])
        np.testing.assert_array_equal(check.estimate_power(q,[0,2],False),[10,10])

    def test_repeating_samples_preserves_average(self):
        q=np.arange(24,dtype='<f4').reshape(4,3,2)
        np.testing.assert_array_equal(check.estimate_power(q,[0,3],True),check.estimate_power(np.repeat(q,2,axis=0),[0,3],True))

    def test_zero_and_invalid_matrix(self):
        np.testing.assert_array_equal(check.estimate_power(np.zeros((2,2,2),'<f4'),[0,2],True),[0,0])
        for q in [np.zeros((0,2,2),'<f4'),np.full((2,2,2),np.nan),np.zeros((2,2))]:
            with self.assertRaises(ValueError):check.estimate_power(q,[0,2],True)
        with self.assertRaises(ValueError):check.estimate_power(np.zeros((2,2,2)),[0,0,2],False)

    def test_compensation_cancels_positive_scale_without_floor_or_limit(self):
        q=np.array([[[3.,4.],[1.,2.]],[[4.,3.],[2.,1.]]],dtype='<f4')
        scale=np.array([.25,4.],dtype='<f4');target=np.array([8.,2.])
        base=q*np.sqrt(target/check.estimate_power(q,[0,2],True))[None,:,None]
        changed=q*scale[None,:,None]
        adjusted=changed*np.sqrt(target/check.estimate_power(changed,[0,2],True))[None,:,None]
        np.testing.assert_array_equal(base,adjusted)
        wrong=changed*np.sqrt(target/check.estimate_power(q,[0,2],True))[None,:,None]
        self.assertGreater(np.max(abs(base-wrong)),1)


class AssemblyTests(unittest.TestCase):
    def test_noise_index_uses_full_interval_origin(self):
        table=np.zeros((512,2),'<f4');table[:,0]=np.arange(512)
        q=np.zeros((2,2,2),'<f4');g=np.array([[0,1,0],[0,1,0]],'<f4')
        out,_,_=check.assemble_model(q,g,table,510,False)
        np.testing.assert_array_equal(out[:,:,0],[[511,0],[1,2]])

    def test_onset_suppression_keeps_signal(self):
        q=np.ones((2,2,2),'<f4');g=np.array([[2,1,0],[2,1,0]],'<f4');table=np.ones((512,2),'<f4')
        masked,signal,noise=check.assemble_model(q,g,table,0,True)
        plain,_,_=check.assemble_model(q,g,table,0,True,False)
        np.testing.assert_array_equal(masked,np.full_like(q,2));np.testing.assert_array_equal(signal,masked)
        np.testing.assert_array_equal(noise,np.zeros_like(q));np.testing.assert_array_equal(plain,np.full_like(q,3))

    def test_non_onset_does_not_change_with_mask_mode(self):
        q=np.ones((2,2,2),'<f4');g=np.array([[2,1,0],[2,1,0]],'<f4');table=np.ones((512,2),'<f4')
        a,*_=check.assemble_model(q,g,table,0,False);b,*_=check.assemble_model(q,g,table,0,False,False)
        self.assertTrue(check.identical(a,b))

    def test_nonzero_tone_and_invalid_noise_cursor_are_rejected(self):
        q=np.ones((2,2,2),'<f4');g=np.ones((2,3),'<f4');table=np.ones((512,2),'<f4')
        with self.assertRaises(ValueError):check.assemble_model(q,g,table,0,False)
        g[:,2]=0
        with self.assertRaises(ValueError):check.assemble_model(q,g,table,512,False)


if __name__=='__main__':unittest.main()
