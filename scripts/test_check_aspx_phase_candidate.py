"""相位候选的固定尺度、包络、相关性及完整尾部诊断。"""
import unittest

import numpy as np

from scripts.check_aspx_phase_candidate import correlation_metrics, diagnostics, fixed_noise_signal_variants


class PhaseCandidateMetricTests(unittest.TestCase):
    def signal(self, samples=8197):
        t=np.arange(samples)
        return np.stack([np.sin(t*.17)+.25*np.cos(t*1.9), .3*np.sin(t*.23)+.1*np.cos(t*2.1)],axis=1)

    def test_signal_factorial_keeps_noise_and_production_addition_order(self):
        high=np.full((2,1,2),np.float32(1e8),'<f4');gain=np.ones(1,'<f4')
        noise=-high;sine=np.ones_like(high)
        result=fixed_noise_signal_variants(high,high*.5,gain,gain*2,noise,sine,np.ones(1,bool))
        np.testing.assert_array_equal(result['original'],1)
        np.testing.assert_array_equal(result['both_fixed_noise'],1)
        self.assertTrue(np.any(result['hf_only']!=result['original']))
        self.assertTrue(np.any(result['gain_only']!=result['original']))

    def test_signal_factorial_rejects_wrong_shapes_and_nonfinite_data(self):
        high=np.ones((2,1,2),'<f4');gain=np.ones(1,'<f4')
        for bad in (np.ones((2,2,2),'<f4'),np.full((2,1,2),np.nan,'<f4')):
            with self.assertRaises(ValueError):fixed_noise_signal_variants(high,bad,gain,gain,high,high,np.ones(1,bool))
        with self.assertRaises(ValueError):fixed_noise_signal_variants(high,high,-gain,gain,high,high,np.ones(1,bool))

    def test_exact_identity_has_zero_error_and_accounts_for_tail(self):
        x=self.signal();r=diagnostics(x,x)
        self.assertEqual(r['samples'],len(x));self.assertEqual(r['spectral_blocks'],2)
        self.assertEqual(r['spectral_tail_samples'],5)
        for entry in r['envelope_windows'].values():self.assertEqual(entry['absolute_energy_error'],0)
        self.assertEqual(r['whole_zero_lag_correlation']['max_correlation_difference'],0)
        self.assertEqual(r['band_12_18_complex_coherence']['relative_matrix_frobenius_error'],0)

    def test_tail_pulse_is_visible_when_stft_has_no_tail_window(self):
        x=np.zeros((4103,2));y=x.copy();y[-1,1]=1
        r=diagnostics(x,y)
        self.assertEqual(r['spectral_tail_samples'],7)
        self.assertEqual(r['segments']['tail']['error_energy'],1)
        self.assertEqual(r['segments']['tail']['max_abs_error'],1)
        self.assertEqual(r['reference_max_sample_step'],1)
        for entry in r['envelope_windows'].values():self.assertEqual(entry['absolute_energy_error'],1)

    def test_sample_step_across_chunk_boundary_is_counted(self):
        x=np.zeros((8192,1));x[4096:]=1
        r=diagnostics(x,x)
        self.assertEqual(r['local_max_sample_step'],1)

    def test_global_polarity_can_preserve_correlation_but_change_pcm(self):
        x=self.signal();r=diagnostics(-x,x)
        self.assertEqual(r['whole_zero_lag_correlation']['relative_matrix_frobenius_error'],0)
        self.assertGreater(r['segments']['startup']['error_energy'],0)
        self.assertEqual(r['envelope_windows']['256']['absolute_energy_error'],0)

    def test_channel_specific_polarity_is_visible_in_correlation(self):
        x=self.signal();x[:,1]=x[:,0];y=x.copy();y[:,1]*=-1
        r=diagnostics(x,y)
        self.assertAlmostEqual(r['whole_zero_lag_correlation']['max_correlation_difference'],2)
        self.assertAlmostEqual(r['band_12_18_complex_coherence']['max_correlation_difference'],2)

    def test_band_envelope_separates_energy_above_reference_bandwidth(self):
        t=np.arange(8192)
        reference=np.sin(2*np.pi*1280*t/4096)[:,None]
        local=reference+np.sin(2*np.pi*1800*t/4096)[:,None]
        r=diagnostics(local,reference)
        self.assertGreater(r['envelope_windows']['2048']['relative_absolute_energy_error'],.99)
        self.assertLess(r['band_12_18_energy_envelope']['relative_absolute_energy_error'],1e-6)

    def test_silent_and_undefined_pairs_are_not_reported_as_matched(self):
        a=np.array([[1.,1.],[1.,1.]]);b=np.zeros((2,2))
        r=correlation_metrics(a,b)
        self.assertEqual(r['valid_directed_pairs'],0)
        self.assertEqual(r['undefined_directed_pairs'],2)
        self.assertIsNone(r['max_correlation_difference'])
        r=correlation_metrics(b,b)
        self.assertEqual(r['both_zero_normalizer_pairs'],2)

    def test_fixed_scale_is_applied_without_gain_fitting(self):
        x=self.signal();matched=diagnostics(x*2,x,.5);wrong=diagnostics(x*2,x)
        self.assertEqual(matched['segments']['startup']['error_energy'],0)
        self.assertGreater(wrong['envelope_windows']['2048']['relative_absolute_energy_error'],2.99)

    def test_scene_roundtrip_is_explicit_for_subnormal_values(self):
        x=np.full((17,1),np.float32(1e-34),'<f4')
        expected=(x*np.float32(2**-15))*np.float32(2**15)
        r=diagnostics(x,expected,scene_roundtrip=True)
        self.assertEqual(r['segments']['tail']['error_energy'],0)

    def test_rejects_nonfinite_and_invalid_geometry(self):
        x=self.signal(32)
        for a,b,scale in [(x[:0],x[:0],1), (x,x[:,:1],1), (x,x,True), (x,x,float('inf')), (x,x,0)]:
            with self.assertRaises(ValueError):diagnostics(a,b,scale)
        x[-1,0]=np.nan
        with self.assertRaises(ValueError):diagnostics(x,np.zeros_like(x))
        with self.assertRaises(ValueError):correlation_metrics(np.zeros((2,3)),np.zeros((2,3)))


if __name__=='__main__':unittest.main()
