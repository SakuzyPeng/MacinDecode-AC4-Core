"""矩阵检查器的自有输入、解析性质和故障注入；无需许可表或媒体。"""
import copy
import json
from pathlib import Path
import tempfile
import unittest
import numpy as np
try:
    from . import check_ajoc_matrix as check
except ImportError:
    import check_ajoc_matrix as check

def row(au=1):
    return dict(au=au,slots=4,dmx=2,decorr=1,enabled=[True],starts=[0],ramps=[3],
                selected=True,cursor_before=[0,0],objects=[dict(present=True,bands=1,coarse=True,sparse=False,
                  dry=[[dict(present=True,time=False,values=[30]),dict(present=True,time=False,values=[20])]],
                  wet=[[dict(present=True,time=False,values=[15])]])])

class MatrixChecks(unittest.TestCase):
    def test_nonbinary_ramp_uses_exact_declared_endpoint(self):
        r=dict(slots=5,starts=[0],ramps=[3]);m=check.Timeline(1)
        x=m.frame(r,np.array([[1]],'<f4'))[:,0]
        np.testing.assert_array_equal(x,[0,np.float32(1/3),np.float32(2/3),1,1])
        self.assertEqual([m.completed,m.length],[3,3])

    def test_cross_frame_64_slot_ramp_and_no_data_point(self):
        m=check.Timeline(1);a=m.frame(dict(slots=32,starts=[0],ramps=[64]),np.array([[1]],'<f4'))
        b=m.frame(dict(slots=32,starts=[],ramps=[]),np.empty((0,1),'<f4'))
        c=m.frame(dict(slots=2,starts=[],ramps=[]),np.empty((0,1),'<f4'))
        np.testing.assert_array_equal(np.concatenate((a,b,c))[:,0],np.minimum(np.arange(66)/64,1))

    def test_nonzero_start_and_interruption_keep_current_slot_value(self):
        m=check.Timeline(1);x=m.frame(dict(slots=6,starts=[1,3],ramps=[4,1]),np.array([[1],[-1]],'<f4'))[:,0]
        np.testing.assert_array_equal(x,[0,0,.25,.5,-1,-1])

    def test_immediate_reference_law_is_a_distinct_fixed_hypothesis(self):
        ref=check.immediate_ramp([0],[1],4,5)[:,0]
        np.testing.assert_array_equal(ref,[.25,.5,.75,1,1])
        local=check.Timeline(1).frame(dict(slots=6,starts=[0],ramps=[4]),np.array([[1]],'<f4'))[:,0]
        np.testing.assert_array_equal(ref,local[1:])

    def test_exact_quantization_and_signed_pre_target(self):
        r=row();t=check.targets(r,{}, {'1':[0]*64})[0];dry,wet,pre=check.split(t,r)
        gain=np.float32(2050/2048)
        np.testing.assert_array_equal(dry[0,0],np.full(64,gain))
        np.testing.assert_array_equal(dry[0,1],np.full(64,-gain))
        np.testing.assert_array_equal(pre[0,0],np.full(64,np.float32(float(gain)**2)))
        r['objects'][0]['wet'][0][0]['values']=[5]
        negative=check.targets(r,{}, {'1':[0]*64})[0]
        np.testing.assert_array_equal(check.split(negative,r)[2],pre)
        np.testing.assert_array_equal(check.split(negative,r)[1],-wet)

    def test_heterogeneous_object_grids_map_before_pre_sum(self):
        r=row();other=copy.deepcopy(r['objects'][0]);other['bands']=3
        other['dry'][0][0]['values']=[26,1,1];other['dry'][0][1]['values']=[25,0,0];other['wet'][0][0]['values']=[11,0,0];r['objects'].append(other)
        maps={'1':[0]*64,'3':[0]*20+[1]*20+[2]*24}
        target=check.targets(r,{},maps)[0];dry,wet,pre=check.split(target,r)
        expected=np.float32(np.float64(2050/2048)**2+np.float64(410/2048)**2)
        self.assertEqual(pre[0,0,0],expected)
        self.assertNotEqual(pre[0,0,0],pre[0,0,63])

    def test_sparse_omission_becomes_next_time_history(self):
        r=row();history={};r['objects'][0]['sparse']=True;r['objects'][0]['dry'][0][0]=dict(present=False,time=False,values=[])
        check.targets(r,history,{'1':[0]*64})
        r['objects'][0]['dry'][0][0]=dict(present=True,time=True,values=[1])
        dry,_,_=check.split(check.targets(r,history,{'1':[0]*64})[0],r)
        self.assertEqual(dry[0,0,0],np.float32(410/2048))

    def test_time_history_cannot_wrap_or_change_grid(self):
        r=row();history={};check.targets(r,history,{'1':[0]*64})
        for value in [-100,100]:
            r['objects'][0]['dry'][0][0]=dict(present=True,time=True,values=[value])
            with self.assertRaisesRegex(ValueError,'越界'):check.targets(r,copy.deepcopy(history),{'1':[0]*64})
        r['objects'][0]['dry'][0][0]['values']=[0];r['objects'][0]['coarse']=False
        with self.assertRaisesRegex(ValueError,'历史'):check.targets(r,history,{'1':[0]*64})

    def test_inactive_and_zero_points_preserve_quantized_history(self):
        r=row();h={};check.targets(r,h,{'1':[0]*64});snapshot=copy.deepcopy(h)
        r['objects'][0].update(present=False,dry=[],wet=[]);check.targets(r,h,{'1':[0]*64})
        for key in h:np.testing.assert_array_equal(h[key][2],snapshot[key][2])
        r['starts']=[];r['ramps']=[];self.assertEqual(check.targets(r,h,{'1':[0]*64}).shape[0],0)

    def test_interpolated_pre_is_not_pointwise_product(self):
        r=row();n=sum(check.sizes(r)[3]);target=np.full(n,2,'<f4');target[-128:]=4
        coeff=check.Timeline(n).frame(dict(slots=3,starts=[0],ramps=[2]),target[None])
        self.assertEqual(check.split(coeff[1],r)[2][0,0,0],2)
        self.assertEqual(check.pointwise_pre(coeff,r)[1,0,0,0],1)

    def test_matrix_product_retains_final_wet_sign_and_gate(self):
        r=row();r['slots']=1;n=sum(check.sizes(r)[3]);coeff=np.ones((1,n),'<f4');coeff[:,128:192]=-2
        x=np.ones((2,1,64,2),'<f4');y=np.full((1,1,64,2),3,'<f4')
        u,z=check.matrix_products(coeff,x,y,r);np.testing.assert_array_equal(u,np.full_like(u,2));np.testing.assert_array_equal(z,np.full_like(z,-4))
        r['enabled']=[False];_,z=check.matrix_products(coeff,x,y,r);np.testing.assert_array_equal(z,np.full_like(z,2))

    def test_permuting_coefficient_storage_is_equivalent(self):
        target=np.array([[1,2,-3],[4,-1,2]],'<f4');schedule=dict(slots=8,starts=[0,4],ramps=[7,3])
        original=check.Timeline(3).frame(schedule,target)
        permutation=[2,0,1];permuted=check.Timeline(3).frame(schedule,target[:,permutation])
        np.testing.assert_array_equal(permuted,original[:,permutation])

    def test_shape_invalid_parameters_and_nonfinite_are_rejected(self):
        for key,value in [('dmx',0),('decorr',8),('ramps',[0]),('starts',[4])]:
            r=row();r[key]=value
            with self.assertRaises(ValueError):check.targets(r,{}, {'1':[0]*64})
        with self.assertRaises(ValueError):check.exact([np.nan],[np.nan],'非有限')
        with self.assertRaises(ValueError):check.exact([0],[0,0],'形状')

    def test_file_manifest_rejects_missing_changed_and_duplicate_cases(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);data=root/'data';data.mkdir();maps=root/'maps.json';maps.write_text(json.dumps({'maps':{'1':[0]*64}}))
            marker=data/'frames.jsonl';marker.write_text('{}\n')
            case=dict(id='a',directory=str(data),files={marker.name:check.digest(marker)},aus=[1],selected_aus=[1])
            m=dict(schema='macinac4.ajoc-matrix-manifest',version=1,fit_parameters=False,maps=dict(path=str(maps),sha256=check.digest(maps)),cases=[case]);manifest=root/'manifest.json'
            for change in ['missing','digest','duplicate']:
                value=copy.deepcopy(m)
                if change=='missing':value['cases'][0]['files']['missing.f32']='x'
                elif change=='digest':value['cases'][0]['files'][marker.name]='x'
                else:value['cases'].append(case)
                manifest.write_text(json.dumps(value))
                with self.assertRaises(ValueError):check.check_manifest(manifest)

class ReferenceChecks(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        root=Path(self.temp.name);self.native=root/'native';self.local=root/'local';self.native.mkdir();self.local.mkdir()
        self.row=row(5);self.row.update(slots=32,starts=[0],ramps=[32]);n=sum(check.sizes(self.row)[3]);dry_wet=sum(check.sizes(self.row)[3][:2])
        target=check.targets(self.row,{}, {'1':[0]*64})[0];initial=np.zeros(n,'<f4');model=check.Timeline(n)
        current=model.frame(self.row,target[None]);next_values=model.frame(dict(slots=32,starts=[],ramps=[]),np.empty((0,n),'<f4'))
        native=check.immediate_ramp(initial,target,32,32)
        target.tofile(self.local/'au0005.targets.f32');current.tofile(self.local/'au0005.coeff.f32');next_values.tofile(self.local/'au0006.coeff.f32')
        rng=np.random.default_rng(719);x=rng.normal(size=(2,32,64,2)).astype('<f4');y=rng.normal(size=(32,1,64,2)).astype('<f4');_,z=check.matrix_products(native,x,y,self.row)
        for name,value in [('initial',initial),('target',target),('coeff',native),('repeat_coeff',native),('used_drywet',native[:,:dry_wet]),('x',x),('y',y),('z',z),('repeat_z',z)]:value.tofile(self.native/(name+'.f32'))
        (self.native/'frame.json').write_text(json.dumps(self.row));tokens=np.arange(64).reshape(2,32).tolist();(self.native/'identity.json').write_text(json.dumps(dict(entry=tokens,consumer=tokens)))
        self.cases={'own':dict(directory=str(self.local),selected_aus=[5,6],input=dict(sha256='own'))}
    def run_check(self):
        reference=dict(case_id='own',au=5,input_sha256='own',valid_subbands=48,directory=str(self.native),files={p.name:check.digest(p) for p in self.native.iterdir()})
        return check.check_reference(reference,self.cases)
    def test_complete_positive_reference(self):
        self.assertEqual(self.run_check()['output_max_abs_error'],0)
    def test_actual_consumer_is_not_replaced_by_named_state(self):
        p=self.native/'used_drywet.f32';x=np.fromfile(p,'<f4');x[0]+=1;x.tofile(p)
        with self.assertRaisesRegex(ValueError,'实际消费'):self.run_check()
    def test_reference_output_perturbation_is_detected_after_valid_repeat(self):
        p=self.native/'z.f32';x=np.fromfile(p,'<f4');x[10]+=1;x.tofile(p);x.tofile(self.native/'repeat_z.f32')
        with self.assertRaisesRegex(ValueError,'前向界'):self.run_check()
    def test_cross_frame_endpoint_is_not_cropped(self):
        p=self.local/'au0006.coeff.f32';x=np.fromfile(p,'<f4');x[0]+=1;x.tofile(p)
        with self.assertRaisesRegex(ValueError,'下一时隙'):self.run_check()
    def test_identity_failure_even_when_values_match(self):
        p=self.native/'identity.json';value=json.loads(p.read_text());value['consumer'][0],value['consumer'][1]=value['consumer'][1],value['consumer'][0];p.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError,'身份'):self.run_check()

if __name__=='__main__':unittest.main()
