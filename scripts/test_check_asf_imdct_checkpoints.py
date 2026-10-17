import copy
import unittest

import numpy as np

from scripts import check_asf_imdct_checkpoints as check


def frames():
    return [dict(id=f'a{au}t{track}', dataset='x', au=au, track=track,
                 element=track, channel=0, ajoc_input=-1 if track == 0 else (track+2) % 9,
                 lengths=[1024, 1024] if track else [2048])
            for au in range(1, 4) for track in range(10)]


def blocks():
    result = []
    for au in (2, 3):
        for track in range(10):
            width = 48 if track else 8
            for offset in ([0, 1024] if track else [0]):
                n = 768 if track else 256
                result.append(dict(id=f'a{au}t{track}b{offset}', scope='s', dataset='x',
                                   inactive_workspace_zeroed=True,
                                   au=au, ajoc_input=-1 if track == 0 else (track+2) % 9, n=n, previous=n,
                                   width=width, offset=offset, prefix=0, span=n))
    return result


class TransformTests(unittest.TestCase):
    def test_fft_dct_matches_independent_dense_cosine(self):
        rng = np.random.default_rng(661)
        for n in (16, 96, 256, 384):
            x = rng.normal(size=n)
            k = np.arange(n)+.5
            expected = np.cos(np.pi/n*k[:, None]*k[None, :]) @ x
            np.testing.assert_allclose(check.dct_iv(x), expected, atol=4e-12, rtol=2e-11)

    def test_unfold_matches_full_imdct_definition(self):
        n = 96
        x = np.sin(np.arange(n)*.313)
        t = np.arange(2*n)+.5+n/2
        expected = np.cos(np.pi/n*t[:, None]*(np.arange(n)+.5)) @ x
        np.testing.assert_allclose(check.unfold(check.dct_iv(x)), expected, atol=2e-12)

    def test_half_length_fft_factorization_is_the_same_cosine_transform(self):
        for n in (16, 96, 256, 384):
            x = np.random.default_rng(n).normal(size=n)
            actual = check.paired_post_rotation(np.fft.fft(check.paired_pre_rotation(x)))
            np.testing.assert_allclose(actual, check.dct_iv(x), atol=2e-13)

    def test_zero_padding_has_exact_centered_sample_coordinates(self):
        original, reduced = 128, 96
        x = np.zeros(original)
        x[[1, 7, 80]] = [.5, -.2, .9]
        position = (np.arange(2*reduced)+.5)*original/reduced-.5
        direct = np.cos(np.pi/original*(position+.5+original/2)[:, None]
                        *(np.arange(original)+.5)) @ x
        np.testing.assert_allclose(check.unfold(check.dct_iv(x[:reduced])), direct, atol=1e-13)

    def test_window_positions_use_centered_grid(self):
        window = np.arange(128, dtype='<f4')
        expected = (np.arange(96)+.5)*128/96-.5
        np.testing.assert_allclose(check.centered_window(window, 96), expected, atol=4e-6)

    def test_overlap_retains_unwindowed_history_and_fixed_scale(self):
        n = 128
        lines = np.zeros(n, dtype='<f4')
        lines[7] = 2
        window = np.linspace(.01, 1, n)
        raw = check.unfold(check.dct_iv(lines))/(n*65536)
        pcm, history, previous = check.synthesize_block(lines, n, 64, np.zeros(n), n, window)
        np.testing.assert_allclose(pcm, raw[:n]*window, atol=1e-20)
        np.testing.assert_allclose(history, raw[n:], atol=1e-20)
        zero, _, _ = check.synthesize_block(np.zeros(n), n, 64, history, previous, window)
        np.testing.assert_allclose(zero, raw[n:]*window[::-1], atol=1e-20)

    def test_nonfinite_and_invalid_transform_input_rejected(self):
        for x in ([1, 2, 3], [1, 0, float('nan'), 0], np.zeros((2, 2))):
            with self.assertRaises(ValueError):
                check.dct_iv(x)
        with self.assertRaises(ValueError):
            check.synthesize_block(np.ones(128), 128, 64, np.zeros(128), 128, np.ones(127))

    def test_probe_recipe_has_distinct_deterministic_excitation(self):
        self.assertFalse(np.any(check.probe_input(96, 0)))
        self.assertEqual(np.flatnonzero(check.probe_input(96, 1)).tolist(), [0])
        self.assertEqual(np.flatnonzero(check.probe_input(96, 2)).tolist(), [95])
        self.assertTrue(check.identical(check.probe_input(96, 3), check.probe_input(96, 3)))
        self.assertFalse(check.identical(check.probe_input(96, 3), check.probe_input(96, 4)))


class CoverageTests(unittest.TestCase):
    def setUp(self):
        self.rows = frames()
        self.known = check.validate_frames(self.rows)
        self.blocks = blocks()

    def test_complete_ownership_and_blocks_accepted(self):
        self.assertEqual(len(check.validate_blocks(self.blocks, self.known)), 20)

    def test_missing_duplicate_or_ambiguous_tracks_rejected(self):
        for rows in (self.rows[:-1], self.rows+self.rows[:1]):
            with self.assertRaises(ValueError):
                check.validate_frames(rows)
        rows = copy.deepcopy(self.rows)
        rows[1]['track'] = rows[0]['track']
        with self.assertRaises(ValueError):
            check.validate_frames(rows)

    def test_fitted_channel_permutation_is_rejected(self):
        rows = copy.deepcopy(self.rows)
        rows[1]['ajoc_input'], rows[2]['ajoc_input'] = rows[2]['ajoc_input'], rows[1]['ajoc_input']
        with self.assertRaisesRegex(ValueError, '置换'):
            check.validate_frames(rows)

    def test_missing_misaligned_or_reordered_blocks_rejected(self):
        for mutation in ('missing', 'offset', 'reorder'):
            rows = copy.deepcopy(self.blocks)
            if mutation == 'missing':
                del rows[2]
            elif mutation == 'offset':
                rows[2]['offset'] += 1
            else:
                rows[1], rows[2] = rows[2], rows[1]
            with self.assertRaises(ValueError):
                check.validate_blocks(rows, self.known)

    def test_history_prefix_and_previous_length_checked(self):
        rows = copy.deepcopy(self.blocks)
        rows[2].update(previous=1536, prefix=384, span=1152)
        with self.assertRaisesRegex(ValueError, '上一块'):
            check.validate_blocks(rows, self.known)
        rows[2]['prefix'] = 0
        with self.assertRaisesRegex(ValueError, '重叠范围'):
            check.validate_blocks(rows, self.known)

    def test_width_and_frame_extent_checked(self):
        rows = copy.deepcopy(self.blocks)
        rows[0]['width'] = 48
        with self.assertRaises(ValueError):
            check.validate_blocks(rows, self.known)
        rows = copy.deepcopy(self.rows)
        rows[0]['lengths'] = [1024]
        with self.assertRaises(ValueError):
            check.validate_frames(rows)

    def test_identity_rejects_path_traversal_and_noninteger_au(self):
        for value in ('../abc', 'x/y', '', 12):
            with self.assertRaises(ValueError):
                check.identifier(value)
        rows = copy.deepcopy(self.rows)
        rows[0]['au'] = True
        with self.assertRaises(ValueError):
            check.validate_frames(rows)


if __name__ == '__main__':
    unittest.main()
