"""用自有窗、信号和匿名分配身份检查分析 QMF 报告器，不加载许可表或运行时。"""
import json
from pathlib import Path
import tempfile
import unittest
import numpy as np
from scripts.check_aspx_basis_replay import components
from scripts.check_qmf_analysis_checkpoints import compare,direct_analysis,fft_analysis,ideal_phases,probe_input,grid_input,compare_grids


class AnalysisModelTests(unittest.TestCase):
    def test_fft_factorization_matches_independent_dense_formula(self):
        rng=np.random.default_rng(8417)
        for n in [8,48,64]:
            x=rng.normal(size=(13,n));h=rng.normal(size=(9,n));w=np.hanning(10*n)
            pre,post=ideal_phases(n)
            a=direct_analysis(x,h,w);b=fft_analysis(x,h,w,pre,post)['qmf']
            self.assertLess(np.max(abs(a-b)),2e-13)

    def test_latest_sample_and_window_origin_are_explicit(self):
        n=8;x=np.zeros((1,n));x[0,-1]=1;w=np.zeros(10*n);w[0]=1
        pre,post=ideal_phases(n)
        q=fft_analysis(x,np.zeros((9,n)),w,pre,post)['qmf'][0]
        expected=-np.exp(1j*np.pi*(np.arange(n)+.5)*7/128)/16
        np.testing.assert_allclose(q,expected,rtol=0,atol=1e-16)

    def test_splitting_preserves_history_and_complete_output(self):
        rng=np.random.default_rng(17);n=48;x=rng.normal(size=(23,n));h=rng.normal(size=(9,n));w=np.hanning(10*n)
        pre,post=ideal_phases(n);whole=fft_analysis(x,h,w,pre,post)
        a=fft_analysis(x[:7],h,w,pre,post);b=fft_analysis(x[7:],a['history'],w,pre,post)
        self.assertTrue(np.array_equal(np.concatenate([a['qmf'],b['qmf']]),whole['qmf']))
        self.assertTrue(np.array_equal(b['history'],x[-9:][::-1]))

    def test_rejects_nonfinite_and_bad_parameter_shapes(self):
        pre,post=ideal_phases(8)
        with self.assertRaises(ValueError):fft_analysis(np.zeros(8),np.zeros((9,8)),np.zeros(80),pre,post)
        with self.assertRaises(ValueError):fft_analysis(np.zeros((1,8)),np.zeros((8,8)),np.zeros(80),pre,post)
        with self.assertRaises(ValueError):fft_analysis(np.full((1,8),np.nan),np.zeros((9,8)),np.zeros(80),pre,post)

    def test_sample_grid_choice_is_explicit_and_rejects_aliasing_probe(self):
        self.assertTrue(np.array_equal(grid_input(64,0,False),grid_input(64,0,True)))
        self.assertGreater(np.max(abs(grid_input(8,0,False)-grid_input(8,0,True))),.01)
        with self.assertRaises(ValueError):grid_input(8,2,False)

    def test_grid_comparison_requires_control_and_matching_input(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c=dict(id='g',n=8,pattern=0,centered=True,slots=76,warmup=12)
            grid_input(8,0,True).tofile(root/'g.input.f32')
            for suffix in ['reference','repeat']:np.zeros((76,8,2),dtype='<f4').tofile(root/f'g.{suffix}.f32')
            with self.assertRaisesRegex(ValueError,'控制'):compare_grids([c],root)
            grid_input(8,0,False).tofile(root/'g.input.f32')
            with self.assertRaisesRegex(ValueError,'网格'):compare_grids([c],root)


class AnalysisCheckpointTests(unittest.TestCase):
    def setUp(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);self.root=Path(t.name);self.path=self.root/'manifest.json'
        rng=np.random.default_rng(19)
        parameters={}
        for n in [8,48]:
            w=np.hanning(10*n).astype('<f4');pre,post=ideal_phases(n)
            pre=components(pre);post=components(post)
            self.save_array(f'window{n}.f32',w);self.save_array(f'pre{n}.f32',pre);self.save_array(f'post{n}.f32',post)
            parameters[n]=(w,pre[:,0].astype(float)+1j*pre[:,1],post[:,0].astype(float)+1j*post[:,1])
        self.cases=[];self.outputs={}
        for index in range(10):
            n=8 if index==0 else 48;name=f'a{index}';x=rng.normal(0,.1,(64,n)).astype('<f4');h=rng.normal(0,.1,(9,n)).astype('<f4')
            self.create_record(name,n,x,h,parameters[n])
            self.cases.append(dict(id=name,n=n,kind='live',mode=8,slots=64,aus=[7]*32+[8]*32,tokens=list(range(index*100,index*100+32))*2))
        links=[]
        for index in range(9):
            name=f'low{index}';owner=f'a{index+1}';self.save_array(name+'.low.f32',self.outputs[owner][26:64,:36])
            links.append(dict(id=name,call=index,bands=36,tokens=list(range(10000+index*6,10006+index*6))+self.cases[index+1]['tokens'][32:]))
        terminal=np.zeros((64,8,2),dtype='<f4');terminal[32:]=self.outputs['a0'][26:58];self.save_array('lfe.terminal.f32',terminal)
        self.scope=dict(id='s',first_au=7,last_au=8,slots_per_au=32,delay=6,contexts=[c['id'] for c in self.cases],links=links,lfe_id='lfe')
        probe=dict(id='probe',n=8,mode=8,kind='probe',slots=64,stimulus='random',seed=847012,phase=0)
        x=probe_input(probe);self.create_record('probe',8,x,np.zeros((9,8),dtype='<f4'),parameters[8]);self.cases.append(probe)
        self.manifest=dict(schema='macinac4.normalized-analysis-checkpoints',version=1,cases=self.cases,scopes=[self.scope]);self.save()

    def save_array(self,name,x):np.asarray(x,dtype='<f4').tofile(self.root/name)

    def create_record(self,name,n,x,h,parameters):
        w,pre,post=parameters;f=fft_analysis(x,h,w,pre,post);q=components(f['qmf']);self.outputs[name]=q
        for suffix,value in [('input',x),('before',h),('after',x[-9:][::-1]),('reference',q),('repeat',q),('native_replay',q),('filtered',components(f['filtered'])),('fft',components(f['fft']))]:
            self.save_array(name+'.'+suffix+'.f32',value)

    def save(self):self.path.write_text(json.dumps(self.manifest))
    def report(self):return compare(self.path,self.root)

    def test_valid_replay_ownership_and_six_slot_handoffs(self):
        r=self.report();self.assertEqual(len(r['cases']),11);self.assertEqual(len(r['aspx_links']),9)
        self.assertEqual([x['owner'] for x in r['aspx_links']],[f'a{i}' for i in range(1,10)])
        self.assertTrue(all(x['metrics']['bit_differing_components']==0 for x in r['aspx_links']+r['lfe_links']))

    def test_rejects_ambiguous_owner_without_waveform_matching(self):
        self.cases[2]['tokens']=self.cases[1]['tokens'].copy();self.save()
        with self.assertRaisesRegex(ValueError,'所有者'):self.report()

    def test_rejects_missing_link_or_context(self):
        self.scope['links'].pop();self.save()
        with self.assertRaises(ValueError):self.report()

    def test_rejects_shifted_au_and_wrong_delay(self):
        self.cases[1]['aus'][32]=7;self.save()
        with self.assertRaises(ValueError):self.report()
        self.cases[1]['aus'][32]=8;self.scope['delay']=5;self.save()
        with self.assertRaises(ValueError):self.report()

    def test_rejects_duplicate_and_unsafe_identity(self):
        self.cases[1]['id']='a0';self.save()
        with self.assertRaises(ValueError):self.report()
        self.cases[1]['id']='../a1';self.save()
        with self.assertRaises(ValueError):self.report()

    def test_rejects_nonfinite_truncated_and_mismatching_native_replay(self):
        p=self.root/'a0.native_replay.f32';original=p.read_bytes()
        for kind in ['nan','short','different']:
            q=np.frombuffer(original,dtype='<f4').copy()
            if kind=='short':p.write_bytes(original[:-4])
            else:q[0]=np.nan if kind=='nan' else 1;q.tofile(p)
            with self.assertRaises(ValueError):self.report()

    def test_rejects_wrong_logical_history_and_probe_input(self):
        p=self.root/'a0.after.f32';original=p.read_bytes();x=np.frombuffer(original,dtype='<f4').copy();x[0]+=1;x.tofile(p)
        with self.assertRaises(ValueError):self.report()
        p.write_bytes(original)
        p=self.root/'probe.input.f32';x=np.fromfile(p,'<f4');x[0]+=1;x.tofile(p)
        with self.assertRaises(ValueError):self.report()

    def test_reports_handoff_residual_instead_of_fitting_delay_or_scale(self):
        p=self.root/'low0.low.f32';x=np.fromfile(p,'<f4');x[0]+=1;x.tofile(p)
        r=self.report()['aspx_links'][0]['metrics'];self.assertEqual(r['differing_components'],1);self.assertGreater(r['max_abs_error'],.99)


if __name__=='__main__':unittest.main()
