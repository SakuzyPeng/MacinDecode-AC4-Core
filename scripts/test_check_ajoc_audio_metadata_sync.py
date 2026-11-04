"""无媒体的共同时间格、格式 FIFO、交接和容器反控制。"""
import copy
import unittest

import numpy as np

from scripts import check_ajoc_audio_metadata_sync as sync
from scripts.check_oamd_consumer_timing import configurations


def transfer():
    return dict(samples=2048, channels=21, sample_bytes=4, returned_bytes=172032,
                channel_stride_bytes=8920, allocation_bytes=187320, sample_start=202752,
                export_sample_start=202752, export_samples=2048, tick_start=1216512)


def stream(parts, data, *, carry=31, limit=.25, gain=1/64):
    state = np.zeros((data.shape[0], 32), '<f4')
    result = []
    cursor = 0
    for n in parts:
        output, state, carry = sync.format_block(state, data[:, cursor:cursor+n], carry, limit, gain)
        result.append(output)
        cursor += n
    if cursor != data.shape[1]:
        raise ValueError('test partition does not cover input')
    return np.concatenate(result, axis=1), state, carry


class CommonClockTests(unittest.TestCase):
    def test_exact_width_grids_include_fractional_origins(self):
        self.assertEqual([sync.grid_tick(i, 64) for i in (-1, 0, 1)], [-6, 0, 6])
        self.assertEqual([sync.grid_tick(i, 48) for i in (-1, 0, 1)], [-7, 1, 9])
        self.assertEqual([sync.grid_tick(i, 8) for i in (-1, 0, 1)], [-27, 21, 69])

    def test_exact_sampling_rejects_rounding(self):
        self.assertEqual(sync.exact_samples(-12), -2)
        for n in (1, 21, -7):
            with self.assertRaises(ValueError):
                sync.exact_samples(n)

    def test_352_has_exact_internal_sample_counts(self):
        for width, count in [(8, 44), (48, 264), (64, 352)]:
            self.assertEqual(sync.grid_tick(0, width)-sync.grid_tick(-count, width), 2112)

    def test_reference_grid_width_and_delay_faults_fail(self):
        rows = [dict(id=str(i), channel=0, width=8, reference_step_ticks=48,
                     local_start_tick=0, reference_grid_start_tick=21,
                     pcm_delay_ticks=2112, pcm_delay_internal_samples=44) for i in range(82)]
        sync.validate_frontend_grid(rows)
        for field, value in [('width', 48), ('reference_grid_start_tick', 0), ('pcm_delay_internal_samples', 352)]:
            bad = copy.deepcopy(rows)
            bad[0][field] = value
            with self.assertRaises(ValueError):
                sync.validate_frontend_grid(bad)

    def test_time_rejects_float_bool_and_unknown_width(self):
        for value in (1.0, True, float('nan')):
            with self.assertRaises(ValueError):
                sync.ticks(value)
        for width in (12, 32, 48.0, True):
            with self.assertRaises(ValueError):
                sync.grid_tick(0, width)

    def test_signed_tick_overflow_is_rejected(self):
        for fn in (lambda: sync.ticks(sync.I64_MAX), lambda: sync.translate(sync.I64_MIN, -1),
                   lambda: sync.translate(sync.I64_MAX, 1), lambda: sync.grid_tick(sync.I64_MAX, 8)):
            with self.assertRaises(ValueError):
                fn()

    def test_analysis_support_keeps_zero_history_and_352_separate(self):
        self.assertEqual(sync.analysis_support(6, 0), [-3456, 378])
        self.assertEqual(sync.analysis_support(6, 352), [-5568, -1734])
        self.assertEqual(sync.analysis_support(7, 352), [-5184, -1350])

    def test_synthesis_support_is_an_interval_not_a_latency(self):
        self.assertEqual(sync.synthesis_support(0), [0, 3834])
        self.assertEqual(sync.synthesis_support(1), [384, 4218])
        self.assertEqual(sync.synthesis_support(-1), [-384, 3450])

    def test_mixed_window_read_clock_is_uniform(self):
        lengths = [2048, 2048, 1024] + [128]*16 + [1024, 2048, 2048]
        rows = sync.imdct_positions(lengths, 2048, 2048)
        self.assertEqual(len(rows), 22)
        offset = 0
        for row, length in zip(rows, lengths):
            self.assertEqual(row['read_start_tick'], offset*6)
            offset += length
            self.assertEqual(row['read_end_tick'], offset*6)
        self.assertEqual(offset, 12288)
        self.assertEqual(rows[3]['new_history_start'], 960)

    def test_initial_short_block_has_declared_read_origin(self):
        rows = sync.imdct_positions([128, 128, 1024], 2048, 128)
        self.assertEqual(rows[0]['read_start_tick'], -5760)
        self.assertEqual(rows[1]['read_start_tick'], -4992)

    def test_invalid_window_or_origin_is_rejected(self):
        for args in [([], 2048, 0), ([127], 2048, 0), ([4096], 2048, 0), ([2048, 2048], 2048, sync.I64_MAX)]:
            with self.assertRaises(ValueError):
                sync.imdct_positions(*args)

    def test_storage_stride_is_not_a_sample_delay(self):
        sync.validate_transfer(transfer())
        changed = dict(transfer(), channel_stride_bytes=9000, allocation_bytes=189000)
        sync.validate_transfer(changed)

    def test_handoff_catches_one_sample_one_slot_and_truncation(self):
        for field, delta in [('export_sample_start', 1), ('export_sample_start', 64),
                             ('tick_start', 1), ('returned_bytes', -4), ('export_samples', -1),
                             ('channel_stride_bytes', -1000), ('allocation_bytes', -10000)]:
            bad = transfer()
            bad[field] += delta
            with self.assertRaises(ValueError, msg=field):
                sync.validate_transfer(bad)

    def test_handoff_rejects_float_time_even_if_integral(self):
        for field in ('sample_start', 'export_sample_start', 'tick_start'):
            bad = transfer()
            bad[field] = float(bad[field])
            with self.assertRaises(ValueError):
                sync.validate_transfer(bad)

    def test_metadata_domains_have_explicit_origins(self):
        self.assertEqual(sync.map_metadata(200704, 2048, 0, 2, 352),
                         dict(source_tick=1204608, scene_tick=1216896, oar_input_tick=1219008))
        a = sync.map_metadata(0, 2048, 31, 63, 352)
        self.assertEqual(a['source_tick'], 2047*6)
        self.assertEqual(a['oar_input_tick']-a['scene_tick'], 352*6)

    def test_metadata_does_not_absorb_the_formatter_delay(self):
        a = sync.map_metadata(0, 2048, 0, 0, 352)
        preformat = sync.translate(a['oar_input_tick'], -sync.ticks(31))
        self.assertEqual(preformat-a['scene_tick'], 321*6)

    def test_edit_is_once_and_keeps_preroll_signed(self):
        self.assertEqual(sync.apply_edit(sync.ticks(100), 2048), -11688)
        self.assertEqual(sync.apply_edit(sync.ticks(2048), 2048), 0)
        self.assertNotEqual(sync.apply_edit(sync.apply_edit(sync.ticks(4096), 2048), 2048), sync.ticks(2048))


class FormatterTests(unittest.TestCase):
    def test_31_samples_follow_from_fifo_for_all_input_positions(self):
        for index in range(64):
            data = np.zeros((1, 128), '<f4')
            data[0, index] = .125
            result, _, _ = stream([64, 64], data)
            expected = np.zeros_like(data)
            expected[0, index+31] = .5
            sync.exact_bytes(result, expected, 'owned impulse moved')

    def test_clipping_boundary_uses_captured_scale(self):
        data = np.tile(np.array([-.5, -.25, 0, .25, .5], '<f4'), 32)[None, :]
        y, _, _ = stream([160], data)
        expected = np.concatenate((np.zeros((1, 31), '<f4'), np.clip(data, -.25, .25)*4), axis=1)[:, :160]
        sync.exact_bytes(y, expected, 'clip boundary changed')

    def test_split_equivalence_includes_non_multiple_of_32(self):
        data = np.linspace(-.5, .5, 513, dtype='<f4')[None, :]
        whole = stream([513], data)
        split = stream([1, 31, 7, 25, 256, 193], data)
        sync.exact_bytes(whole[0], split[0], 'split output changed')
        sync.exact_bytes(whole[1], split[1], 'split state changed')
        self.assertEqual(whole[2], split[2])

    def test_equivalent_zero_state_representations(self):
        data = np.full((1, 256), .125, '<f4')
        sync.exact_bytes(stream([256], data, carry=0)[0], stream([256], data, carry=31)[0], 'zero representation changed output')

    def test_cross_frame_history_and_reset(self):
        x = np.ones((1, 2048), '<f4')/8
        _, state, carry = sync.format_block(np.zeros((1, 32), '<f4'), x, 31, .25, 1/64)
        continued, _, _ = sync.format_block(state, np.zeros_like(x), carry, .25, 1/64)
        self.assertTrue(np.all(continued[:, :31] == .5))
        reset, _, _ = sync.format_block(np.zeros_like(state), np.zeros_like(x), 31, .25, 1/64)
        self.assertTrue(np.all(reset == 0))

    def test_state_is_independent_per_channel(self):
        x = np.zeros((2, 256), '<f4')
        x[1] = .125
        y, state, _ = stream([256], x)
        self.assertTrue(np.all(y[0] == 0))
        self.assertTrue(np.all(state[0] == 0))
        self.assertTrue(np.all(y[1, 31:] == .5))

    def test_unused_state_cell_is_not_claimed_as_valid_history(self):
        state = np.zeros((1, 32), '<f4')
        state[0, 0] = np.nan
        y, _, _ = sync.format_block(state, np.zeros((1, 256), '<f4'), 31, .25, 1/64)
        self.assertTrue(np.isfinite(y).all())

    def test_rejects_nonfinite_live_history_and_invalid_shape(self):
        for state, data, carry in [(np.ones((1, 31)), np.ones((1, 32)), 31),
                                   (np.ones((1, 32)), np.ones((2, 32)), 31),
                                   (np.ones((1, 32)), np.full((1, 32), np.nan), 31),
                                   (np.full((1, 32), np.inf), np.ones((1, 32)), 31),
                                   (np.ones((1, 32)), np.ones((1, 32)), 32)]:
            with self.assertRaises(ValueError):
                sync.format_block(state, data, carry, .25, 1/64)

    def test_wrong_delay_gain_or_cleared_history_is_detectable(self):
        x = np.arange(512, dtype='<f4')[None, :]/2048
        correct, state, carry = stream([256], x[:, :256])
        next_output, _, _ = sync.format_block(state, x[:, 256:], carry, .25, 1/64)
        cleared, _, _ = sync.format_block(np.zeros_like(state), x[:, 256:], carry, .25, 1/64)
        wrong_gain, _, _ = sync.format_block(state, x[:, 256:], carry, .25, 1/32)
        for bad in (np.roll(correct, 1, axis=1), np.roll(correct, 64, axis=1)):
            with self.assertRaises(ValueError):
                sync.exact_bytes(correct, bad, 'time fault')
        for bad in (cleared, wrong_gain):
            with self.assertRaises(ValueError):
                sync.exact_bytes(next_output, bad, 'state fault')


class ReferenceGateTests(unittest.TestCase):
    def fixture(self):
        status = dict(backend='drp-runtime', runtime_profile='DRP 4.3.0.19350 arm64', layout='9.1.6',
                      success=True, error=None, frames=2, blocks=2, sample_rate=48000,
                      decoder_raw_bytes=344064, oar_raw_bytes=0)
        calls = [[0, 0, 172032, 1], [4096, 0, 172032, 1]]
        params = [f'{key}={value} result=0' for key, value in sync.REFERENCE_PARAMETERS.items()]
        return status, calls, params, [0, 4096]

    def test_each_call_must_match_the_complete_input(self):
        sync.validate_reference_gate(*self.fixture())
        for calls in ([[0, 0, 172032, 1]], [[0, 0, 172032, 1]]*2,
                      [[0, 0, 172032, 1], [4096, 1, 172032, 1]],
                      [[0, 0, 172032, 1], [4096, 0, 172028, 1]],
                      [[4096, 0, 172032, 1], [0, 0, 172032, 1]]):
            status, _, params, offsets = self.fixture()
            with self.assertRaises(ValueError):
                sync.validate_reference_gate(status, calls, params, offsets)

    def test_parameter_success_alone_cannot_accept_wrong_profile(self):
        for mutation in ('omit', 'duplicate', 'failure', 'wrong_level'):
            status, calls, params, offsets = self.fixture()
            if mutation == 'omit':
                params.pop()
            elif mutation == 'duplicate':
                params.append(params[0])
            elif mutation == 'failure':
                params[1] = 'target_reference_level=0 result=1'
            else:
                params[1] = 'target_reference_level=-23 result=0'
            with self.assertRaises(ValueError):
                sync.validate_reference_gate(status, calls, params, offsets)

    def test_outer_success_does_not_hide_partial_processing(self):
        for field, value in [('blocks', 1), ('error', 'decode failure'), ('sample_rate', 44100), ('success', False)]:
            status, calls, params, offsets = self.fixture()
            status[field] = value
            with self.assertRaises(ValueError):
                sync.validate_reference_gate(status, calls, params, offsets)


class ContainerTests(unittest.TestCase):
    def fixture(self):
        return [dict(kind='timeline', aus=143, edit_samples=2048, raw_samples=292864, presentation_samples=288000, payloads_equal=True, metadata_events=3)] + [
            dict(kind='track', channel=i, raw_first=2048, presentation_first=0, samples=288000, bit_exact=True) for i in range(21)] + [
            dict(kind='event', order=i, raw_sample=2048*i, presentation_sample=2048*i-2048, ramp=2048, state_identity=True) for i in range(3)]

    def test_owned_pair_preserves_signed_events(self):
        self.assertEqual(sync.check_container(self.fixture())['edit_samples'], 2048)

    def test_pcm_metadata_and_duplicate_edit_faults(self):
        for delta in (-2048, 1, 2048):
            rows = self.fixture()
            rows[-1]['presentation_sample'] += delta
            with self.assertRaises(ValueError):
                sync.check_container(rows)
        rows = self.fixture()
        rows[1]['raw_first'] += 2048
        with self.assertRaises(ValueError):
            sync.check_container(rows)

    def test_missing_track_or_event_identity_is_rejected(self):
        rows = self.fixture()
        with self.assertRaises(ValueError):
            sync.check_container(rows[:-1])
        with self.assertRaises(ValueError):
            sync.check_container(rows[:1]+rows[2:])
        rows[-1]['state_identity'] = False
        with self.assertRaises(ValueError):
            sync.check_container(rows)


class CommonEventTests(unittest.TestCase):
    def fixture(self):
        cases = configurations()
        rows = []
        for cfg in cases:
            for au in range(2, 401):
                factor = cfg['factor'] if au == 100 else 0
                ramp = cfg['duration'] if au == 100 else 2048
                start = cfg['changes'].get('ajoc_start_pos', 0) if au == 100 else 0
                slot = (au-1)*32+start
                source = (au-2)*2048+32*factor
                scene = source+2048
                api = scene+352
                rows.append(dict(id=cfg['id'], consumer_au=au, source_au=au-1, objects=21,
                                 sample_offset=0, block_factor=factor, api_offset=352, ramp_samples=ramp,
                                 source_tick=source*6, scene_tick=scene*6, oar_input_tick=api*6,
                                 oar_preformat_tick=(api-31)*6,
                                 reference_first_step_support=[slot*384, slot*384+3834],
                                 local_first_step_support=[slot*384+384, slot*384+4218]))
        return rows, cases

    def test_all_pairs_keep_their_own_clock_origins(self):
        rows, cases = self.fixture()
        result = sync.validate_common_events(rows, cases)
        self.assertEqual(len(result), 16)
        self.assertTrue(all(r['coordinate_difference_after_formatter_ticks'] == 1926 for r in result))

    def test_missing_duplicated_and_wrong_source_events_fail(self):
        rows, cases = self.fixture()
        for bad in (rows[:-1], rows+[rows[0]]):
            with self.assertRaises(ValueError):
                sync.validate_common_events(bad, cases)
        rows[98]['source_au'] -= 1
        with self.assertRaises(ValueError):
            sync.validate_common_events(rows, cases)

    def test_352_and_formatter_delay_cannot_be_reapplied(self):
        rows, cases = self.fixture()
        for field, delta in [('oar_input_tick', 2112), ('oar_input_tick', -2112),
                             ('scene_tick', 186), ('oar_preformat_tick', -186), ('source_tick', 1)]:
            bad = copy.deepcopy(rows)
            bad[98][field] += delta
            with self.assertRaises(ValueError):
                sync.validate_common_events(bad, cases)

    def test_first_step_cannot_be_bound_to_an_adjacent_slot(self):
        rows, cases = self.fixture()
        rows[98]['reference_first_step_support'] = rows[98]['local_first_step_support']
        with self.assertRaises(ValueError):
            sync.validate_common_events(rows, cases)

if __name__ == '__main__':
    unittest.main()
