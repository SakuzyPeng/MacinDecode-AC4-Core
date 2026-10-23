"""自有整数时间与插值事件判据，不依赖媒体或 PDF。"""
import copy
import unittest
import numpy as np
from scripts.check_ajoc_timing import metadata_events, scalar_schedule, validate_timeline


def fixture():
    timing=dict(sample_offset=3,blocks=[dict(factor=2,ramp=512,code=1)])
    rows=[]
    for i in range(3):
        rows.append(dict(au=i+1,samples=2048,input_sample_start=i*2048,consumer_qmf_base=i*32,
                         pcm_delay=352,aspx_delay=6,source_sample_start=(i-1)*2048 if i else None,
                         group_timing=timing,raw_dmx=None,raw_umx=None,derive=False,
                         applied_dmx=timing if i else None,applied_umx=timing if i else None,
                         oamd_events=[(i-1)*2048+67] if i else [],raw_starts=[4],raw_ramps=[32]))
    matrix=[dict(au=i,slots=32,starts=[4],ramps=[32],selected=True) for i in [2,3]]
    return rows,matrix


class TimingCheckerTests(unittest.TestCase):
    def test_metadata_uses_sample_units_and_does_not_equal_ajoc_start(self):
        rows,matrix=fixture();result=validate_timeline(rows,matrix)
        self.assertEqual(result['selected'][0]['oamd_source_sample_events'],[67])
        self.assertEqual(result['selected'][0]['immediate_first_global_slots'],[36])
        self.assertFalse(result['independent_normative_schedule_decision'])

    def test_zero_and_negative_integer_origins(self):
        t=dict(sample_offset=0,blocks=[dict(factor=0,ramp=0,code=0)])
        self.assertEqual(metadata_events(t,-2048),[-2048])
        self.assertEqual(metadata_events(None,0),[])
        self.assertEqual(metadata_events(t,2**63-1),[2**63-1])
        t['sample_offset']=1
        with self.assertRaises(ValueError):metadata_events(t,2**63-1)

    def test_rejects_noninteger_and_bad_metadata(self):
        for value in [-1,64,1.5,True]:
            with self.assertRaises(ValueError):metadata_events(dict(sample_offset=0,blocks=[dict(factor=value,ramp=0,code=0)]),0)
        with self.assertRaises(ValueError):metadata_events(dict(sample_offset=0,blocks=[]),0)

    def test_one_slot_shift_and_wrong_control_source_are_detected(self):
        for key in ('consumer_qmf_base','source_sample_start','pcm_delay','aspx_delay'):
            rows,matrix=fixture();rows[1][key]+=1
            with self.assertRaises(ValueError):validate_timeline(rows,matrix)

    def test_missing_duplicate_or_warmup_matrix_is_rejected(self):
        rows,matrix=fixture()
        for bad in [[],matrix[:1],matrix+matrix[:1],[dict(matrix[0],au=1),matrix[1]]]:
            with self.assertRaises(ValueError):validate_timeline(rows,bad)

    def test_raw_timing_and_event_corruption_is_detected(self):
        for key,value in [('applied_umx',None),('oamd_events',[68])]:
            rows,matrix=fixture();rows[1][key]=value
            with self.assertRaises(ValueError):validate_timeline(rows,matrix)

    def test_two_interpretations_keep_input_coordinates_fixed(self):
        row=dict(slots=4,starts=[0],ramps=[2],targets=[2])
        np.testing.assert_array_equal(scalar_schedule([row]),[0,1,2,2])
        np.testing.assert_array_equal(scalar_schedule([row],immediate=True),[1,2,2,2])

    def test_cross_frame_completion_and_interruption(self):
        a=dict(slots=3,starts=[2],ramps=[2],targets=[2]);b=dict(slots=3,starts=[],ramps=[],targets=[])
        np.testing.assert_array_equal(scalar_schedule([a,b]),[0,0,0,1,2,2])
        b=dict(slots=3,starts=[0],ramps=[1],targets=[-1])
        np.testing.assert_array_equal(scalar_schedule([a,b]),[0,0,0,1,-1,-1])

    def test_ramp_boundary_validation(self):
        base=dict(slots=24,starts=[23],ramps=[64],targets=[1])
        for key,value in [('slots',0),('starts',[24]),('ramps',[0]),('ramps',[65]),('targets',[]),('targets',[np.nan])]:
            row=copy.deepcopy(base);row[key]=value
            with self.assertRaises(ValueError):scalar_schedule([row])


if __name__=='__main__':unittest.main()
