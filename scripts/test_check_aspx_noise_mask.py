import copy
import unittest
import numpy as np
from scripts import check_aspx_noise_mask as check


def manifest():
    rows=[];iwc=[]
    for scope,count in [('tone',9),('music100',9),('music33',9),('odd2',2),('odd8',2),('even8',9)]:
        lo,n=(34,28) if scope.startswith('odd') else (36,26 if scope in ['tone','even8'] else 20)
        for call in range(count):
            times=([0,18,30,34] if scope=='odd2' else [0,16,32] if scope=='music33' else
                   [0,12,22,32] if scope=='music100' and call<5 else [0,16,32] if scope=='music100' else [0,32])
            for e,(a,b) in enumerate(zip(times,times[1:])):
                rows.append(dict(id=f'{scope}c{call}e{e}',scope=scope,call=call,env=e,first=a,last=b,sbx=lo,subbands=n,
                    gains=[[1.,1.,0.] for _ in range(n)],onset=False,noise_before=a*n%512,noise_after=b*n%512,tone_before=a%4,tone_after=b%4))
            iwc.append(dict(id=f'{scope}c{call}',scope=scope,call=call,first=times[0],last=times[-1],sbx=lo,subbands=n,fic=False,tic=False,perturbed_components=0))
    carried=[dict(id=f'carry{c}',call=c,from_au=2,to_au=3,sbx=34,subbands=28,source_slots=[32,34],destination_slots=[0,2],
                  replicates=[dict(run=r,tone_next=2,noise_last=440,destination_tone_next=2,destination_noise_last=440) for r in [1,2]]) for c in range(2)]
    controls=[dict(mode=m,sbx=36,subbands=20,fic_band_end=38,first=0,last=32,tic_first=14,tic_last=16) for m in range(4)]
    return dict(schema='macinac4.noise-mask-replay',version=1,cases=check.expected_cases(),reference_runs=[f'r{i}' for i in range(12)],live=rows,carry=carried,iwc_live=iwc,iwc_controls=controls)


class ManifestTests(unittest.TestCase):
    def test_complete_manifest_has_declared_coverage(self):
        m=manifest();check.validate_manifest(m)
        self.assertEqual(len(m['cases']),936);self.assertEqual(len(m['live']),67)
        self.assertEqual(len(check.case_lines(m['cases'])),936)

    def test_missing_case_run_or_envelope_is_rejected(self):
        for key in ['cases','reference_runs','live','iwc_live','iwc_controls']:
            m=manifest();m[key].pop()
            with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_case_recipe_cannot_silently_change(self):
        m=manifest();m['cases'][0]['noise_level']=0
        with self.assertRaises(ValueError):check.validate_manifest(m)
        m=manifest();m['cases'][0]['id']='../m0'
        with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_shifted_real_identity_and_time_are_rejected(self):
        for key,value in [('id','wrong'),('first',1),('last',33),('onset',1),('noise_after',0)]:
            m=manifest();m['live'][0][key]=value
            with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_invalid_gain_and_missing_carry_state_are_rejected(self):
        m=manifest();m['live'][0]['gains'][0][2]=float('nan')
        with self.assertRaises(ValueError):check.validate_manifest(m)
        m=manifest();m['carry'][0]['replicates'][0]['destination_noise_last']=439
        with self.assertRaises(ValueError):check.validate_manifest(m)
        m=manifest();m['carry'][0]['replicates'].pop()
        with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_unsupported_live_interleave_is_not_assumed_identity(self):
        m=manifest();m['iwc_live'][0]['tic']=True
        with self.assertRaises(ValueError):check.validate_manifest(m)

    def test_reference_clock_and_completion_are_checked(self):
        c=check.expected_cases()[0]
        row=dict(id=c['id'],finite=True,guards=True,save=0,envelopes=[dict(env=0,first=0,last=16,onset=0,present=0,noise_before=0,noise_after=320,tone_before=1,tone_after=1)])
        rows=[row,dict(complete=True,cases=1)]
        check.validate_states(rows,[c])
        for key,value in [('noise_after',0),('present',1),('last',8)]:
            changed=copy.deepcopy(rows);changed[0]['envelopes'][0][key]=value
            with self.assertRaises(ValueError):check.validate_states(changed,[c])
        with self.assertRaises(ValueError):check.validate_states(rows[:1],[c])


class MaskTests(unittest.TestCase):
    def setUp(self):
        self.table=np.ones((512,2),'<f4')

    def test_onset_removes_noise_but_preserves_signal(self):
        q=np.ones((2,2,2),'<f4');g=np.array([[2,1,0],[2,1,0]],'<f4')
        actual=check.render(q,g,self.table,36,0,1,True,'both')
        np.testing.assert_array_equal(actual,np.full_like(q,2))
        literal=check.render(q,g,self.table,36,0,1,True,'literal')
        np.testing.assert_array_equal(literal,np.full_like(q,3))

    def test_sine_masks_only_its_own_subband(self):
        q=np.ones((1,2,2),'<f4');g=np.array([[2,1,0],[2,1,4]],'<f4')
        actual=check.render(q,g,self.table,36,0,1,False,'both')
        np.testing.assert_array_equal(actual,[[[3,3],[2,-2]]])
        too_broad=check.render(q,g,self.table,36,0,1,False,'whole_envelope')
        self.assertFalse(check.identical(actual,too_broad))

    def test_tiny_nonzero_sine_still_selects_noise_exclusion(self):
        q=np.zeros((1,1,2),'<f4');g=np.array([[1,1,2**-24]],'<f4')
        out=check.render(q,g,self.table,36,0,1,False,'both')
        np.testing.assert_array_equal(out,[[[0,2**-24]]])
        g[:,2]=0
        np.testing.assert_array_equal(check.render(q,g,self.table,36,0,1,False,'both'),[[[1,1]]])

    def test_no_noise_is_a_noninformative_equivalent_control(self):
        q=np.ones((2,2,2),'<f4');g=np.array([[1,0,1],[1,0,0]],'<f4')
        outputs=[check.render(q,g,self.table,47,0,1,True,rule) for rule in check.RULES]
        self.assertTrue(all(check.identical(outputs[0],x) for x in outputs[1:]))

    def test_absolute_parity_and_noise_wrap_are_observable(self):
        q=np.zeros((1,2,2),'<f4');g=np.array([[1,0,1],[1,0,1]],'<f4')
        out=check.render(q,g,self.table,47,510,1,False,'both')
        np.testing.assert_array_equal(out,[[[0,-1],[0,1]]])
        table=np.zeros((512,2),'<f4');table[:,0]=np.arange(512);g[:,1]=1;g[:,2]=0
        np.testing.assert_array_equal(check.render(q,g,table,47,510,1,False,'both'),[[[511,0],[0,0]]])

    def test_invalid_levels_and_rule_are_rejected(self):
        q=np.zeros((1,1,2),'<f4');g=np.ones((1,3),'<f4')
        with self.assertRaises(ValueError):check.render(q,g,self.table,0,0,1,False,'unknown')
        g[0,1]=np.nan
        with self.assertRaises(ValueError):check.render(q,g,self.table,0,0,1,False,'both')


class InterleaveTests(unittest.TestCase):
    def test_frequency_addition_and_time_replacement_are_distinct(self):
        q=np.ones((40,64,2),'<f4');w=np.full((40,44,2),3,dtype='<f4')
        c=dict(mode=1,sbx=36,subbands=20,fic_band_end=38)
        added=check.iwc_control_output(c,q,w)
        self.assertEqual(added[0,36,0],4);self.assertEqual(added[0,38,0],1)
        c['mode']=2;replaced=check.iwc_control_output(c,q,w)
        self.assertEqual(replaced[14,40,0],3);self.assertEqual(replaced[13,40,0],1)
        c['mode']=3;combined=check.iwc_control_output(c,q,w)
        self.assertEqual(combined[14,36,0],3);self.assertEqual(combined[13,36,0],4)
        c['mode']=0;self.assertTrue(check.identical(check.iwc_control_output(c,q,w),q))

    def test_invalid_interleave_shapes_are_rejected(self):
        c=dict(mode=0,sbx=36,subbands=20,fic_band_end=38)
        with self.assertRaises(ValueError):check.iwc_control_output(c,np.zeros((32,64,2)),np.zeros((40,44,2)))
        c['mode']=4
        with self.assertRaises(ValueError):check.iwc_control_output(c,np.zeros((40,64,2)),np.zeros((40,44,2)))


if __name__=='__main__':unittest.main()
