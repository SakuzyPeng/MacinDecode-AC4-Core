"""基底比较器的独立小矩阵门禁，不加载参考运行时或许可表。"""
import json
from pathlib import Path
import tempfile
import unittest
import numpy as np
from scripts.check_aspx_basis_replay import HF_MODES,TONE_MODES,ROTATION,components,complex_values,compare,metrics


class BasisReplayTests(unittest.TestCase):
    def setUp(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup)
        self.root=Path(t.name);self.inputs=self.root/'inputs';self.local=self.root/'local'
        self.inputs.mkdir();self.local.mkdir();self.path=self.inputs/'cases.json'
        self.cases=[dict(kind='H',id='h',config=[1,6,0,3,255],first=1,last=3,targets=[36,38],sources=[28]),
                    dict(kind='S',id='s',slots=16,band=36,seed=0,first=2,last=4)]
        q=np.zeros((40,64,2),dtype='<f4');q[:,28:30]=[1,.5]
        converted=components(complex_values(q)/ROTATION)
        ref=q.copy();ref[1:3,36:38]=q[1:3,28:30]/4
        rot=converted.copy();rot[1:3,36:38]=converted[1:3,28:30]/4
        for name,values in [('native',q),('converted',converted),('reference',ref),('rotated_reference',rot)]:values.tofile(self.inputs/f'h.{name}.f32')
        for mode in HF_MODES:
            values=np.zeros((2,64,2),dtype='<f4')
            values[:,36:38]=q[1:3,28:30] if mode=='native' else converted[1:3,28:30]
            if mode=='operator_basis':
                values[:,36:38]=components(complex_values(values[:,36:38])*np.exp(-1j*np.pi*8/128))
            values.tofile(self.local/f'h.{mode}.f32')
        q=np.zeros((16,64,2),dtype='<f4');q[2,36]=[1,0];q[3,36]=[0,1];q.tofile(self.inputs/'s.native.f32')
        pcm=np.arange(16*64,dtype='<f4').reshape(16,64)/1024
        (pcm/2**17).astype('<f4').tofile(self.inputs/'s.reference.f32')
        for mode in TONE_MODES:
            values=pcm.copy()
            if mode=='native':values[0,0]=1
            values.tofile(self.local/f's.{mode}.f32')
        self.rows=[f'H h {mode} 1 3 36' for mode in HF_MODES]+[f'S s {mode} 16' for mode in TONE_MODES]
        self.save()

    def save(self):
        self.path.write_text(json.dumps(self.cases))
        (self.local/'results.txt').write_text('\n'.join(self.rows)+'\n')

    def report(self):return compare(self.path,self.inputs,self.local)

    def test_distinguishes_coordinate_conversion_from_unchanged_copy(self):
        r=self.report();h=r['hf'][0]
        self.assertEqual(h['native_quarter_copy']['differing_components'],0)
        self.assertEqual(h['same_basis_local']['differing_components'],0)
        self.assertGreater(h['unchanged_operator']['max_abs_error'],.1)
        self.assertLess(h['transformed_operator']['max_abs_error'],2e-7)
        self.assertAlmostEqual(h['patches'][0]['predicted_operator_phase_deg'],-11.25)
        self.assertAlmostEqual(h['patches'][0]['observed_reference_over_plain_copy_phase_deg'],-11.25,places=5)
        self.assertEqual(r['tones'][0]['modes']['native']['differing_components'],1)
        self.assertEqual(r['tones'][0]['modes']['basis']['differing_components'],0)

    def test_rejects_missing_duplicate_and_bad_local_identity(self):
        rows=self.rows
        for bad in [rows[:-1],[*rows,rows[0]],[rows[0].replace('H h ','H absent '),*rows[1:]]]:
            self.rows=bad;self.save()
            with self.assertRaises(ValueError):self.report()

    def test_rejects_shifted_metadata_and_invalid_source_range(self):
        original=dict(self.cases[0])
        for patch in [dict(first=2),dict(sources=[35]),dict(targets=[36,38,39]),dict(config=[2,6,0,3,255])]:
            self.cases[0]={**original,**patch};self.save()
            with self.assertRaises(ValueError):self.report()

    def test_rejects_wrong_tone_phase_or_subband(self):
        original=dict(self.cases[1])
        for patch in [dict(seed=1),dict(band=37),dict(first=1)]:
            self.cases[1]={**original,**patch};self.save()
            with self.assertRaises(ValueError):self.report()

    def test_rejects_duplicate_manifest_id_and_path_traversal(self):
        original=list(self.cases)
        for rows in [[*original,original[0]],[dict(original[0],id='../h'),original[1]]]:
            self.cases=rows;self.save()
            with self.assertRaises(ValueError):self.report()

    def test_rejects_nonfinite_truncated_and_outside_reference_data(self):
        p=self.inputs/'h.reference.f32';original=p.read_bytes()
        for bad in ['nan','short','outside']:
            q=np.frombuffer(original,dtype='<f4').copy()
            if bad=='short':p.write_bytes(original[:-4])
            else:q[0]=np.nan if bad=='nan' else 1;q.tofile(p)
            with self.assertRaises(ValueError):self.report()

    def test_rejects_local_write_outside_patch_targets(self):
        p=self.local/'h.operator_basis.f32';q=np.fromfile(p,'<f4');q[0]=1;q.tofile(p)
        with self.assertRaises(ValueError):self.report()

    def test_reports_corrupt_basis_input_without_fitting_it_away(self):
        p=self.inputs/'h.converted.f32';q=np.fromfile(p,'<f4').reshape(40,64,2)
        q[0,0,0]=1;q.tofile(p)
        # Keep the declared unaffected region equal to this corrupted input.
        p=self.inputs/'h.rotated_reference.f32';q=np.fromfile(p,'<f4').reshape(40,64,2)
        q[0,0,0]=1;q.tofile(p)
        self.assertEqual(self.report()['hf'][0]['input_basis_transform']['differing_components'],1)

    def test_metrics_separate_signed_zero_and_zero_signal(self):
        r=metrics(np.array([0.,0.],dtype='<f4'),np.array([-0.,0.],dtype='<f4'))
        self.assertEqual(r['max_ulp'],0);self.assertEqual(r['signed_zero_only_differences'],1)
        self.assertEqual(r['signal_energy'],0);self.assertIsNone(r['snr_db'])


if __name__=='__main__':unittest.main()
