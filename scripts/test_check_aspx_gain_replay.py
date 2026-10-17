"""增益检查器的离线判据，不需要媒体、规范表或参考解码器。"""

import copy
import unittest

from scripts.check_aspx_gain_replay import MODES, compare, f32_bits


def fixture():
    records = [{"id": "a", "case": "constructed", "call": 0, "env": 0, "bitstream_limiter": 0,
                "estimated": [1, 2], "reference": [[1, 0, 2], [4, 2, 0]]}]
    lines = [f"a {mode} {sb} " + " ".join(f"{f32_bits(float(v)):08x}" for v in [*ref, 1])
             for mode in MODES for sb, ref in enumerate(records[0]["reference"])]
    return records, lines


class GainReplayTests(unittest.TestCase):
    def test_exact_values_and_row_permutation(self):
        rows, lines = fixture()
        a = compare(rows, "\n".join(lines))
        self.assertEqual(a, compare(rows, "\n".join(reversed(lines))))
        self.assertEqual(a["subband_envelopes"], 2)
        self.assertEqual(a["groups"][0]["fields"]["signal_gain"]["max_ulp"], 0)

    def test_detects_one_ulp_and_nonzero_at_reference_zero(self):
        rows, lines = fixture()
        lines[0] = "a off 0 3f800001 3f800000 40000000 3f800000"
        result = compare(rows, "\n".join(lines))
        fields = next(g["fields"] for g in result["groups"] if g["mode"] == "off")
        self.assertEqual(fields["signal_gain"]["max_ulp"], 1)
        self.assertEqual(fields["noise_level"]["max_zero_reference_abs_error"], 1)

    def test_rejects_missing_duplicate_unknown_or_shifted_rows(self):
        rows, lines = fixture()
        variants = [lines[:-1], lines + [lines[0]], [lines[0].replace("a off", "b off"), *lines[1:]],
                    [lines[0].replace("off 0", "off 2"), *lines[1:]],
                    [lines[0].replace("off", "invalid"), *lines[1:]]]
        for variant in variants:
            with self.subTest(variant=variant), self.assertRaises(ValueError):
                compare(rows, "\n".join(variant))

    def test_rejects_nonfinite_and_negative_local_values(self):
        rows, lines = fixture()
        for bits in ("7fc00000", "7f800000", "bf800000"):
            with self.subTest(bits=bits), self.assertRaises(ValueError):
                compare(rows, "\n".join([lines[0].replace("3f800000", bits, 1), *lines[1:]]))

    def test_rejects_invalid_or_duplicated_checkpoint_identity(self):
        rows, lines = fixture()
        for altered in (rows + rows, [dict(rows[0], reference=[[1, 0, 2]])],
                        [dict(rows[0], bitstream_limiter=2)], [dict(rows[0], env=5)]):
            with self.subTest(altered=altered), self.assertRaises(ValueError):
                compare(altered, "\n".join(lines))
        altered = copy.deepcopy(rows)
        altered[0]["reference"][0][0] = float("nan")
        with self.assertRaises(ValueError):
            compare(altered, "\n".join(lines))


if __name__ == "__main__":
    unittest.main()
