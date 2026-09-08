import unittest

from scripts.check_damf_headphone import continuous_commands, headphone_timeline, verify_roundtrip


BASE = """sampleRate: 48000
events:
  - ID: 10
    samplePos: 0
    pos: [-1, 0, 0]
    gain: -28
    rampLength: 0
    headTrackMode: scene relative
    binauralRenderMode: near
  - samplePos: 1000
    pos: [1, 0, 0]
    gain: -16
    rampLength: 4000
"""
PATCH = """  - samplePos: 3000
    headTrackMode: head relative
    binauralRenderMode: far
"""


class DamfHeadphoneCheckTests(unittest.TestCase):
    def test_native_partial_events_preserve_commands_and_identity(self):
        self.assertEqual(continuous_commands(BASE, 10), continuous_commands(BASE + PATCH, 10))
        self.assertEqual(headphone_timeline(BASE + PATCH, 10), [(0, "scene relative", "near"), (3000, "head relative", "far")])
        verify_roundtrip(BASE, BASE + PATCH, BASE + PATCH, 10)

    def test_restarting_ramp_is_detected(self):
        for injected in ("    rampLength: 0\n", "    pos: [1, 0, 0]\n"):
            with self.assertRaisesRegex(ValueError, "连续字段"):
                verify_roundtrip(BASE, BASE + PATCH + injected, BASE + PATCH, 10)

    def test_wrong_policy_or_time_is_detected(self):
        for changed in (PATCH.replace("3000", "3001"), PATCH.replace("head relative", "scene relative")):
            with self.assertRaisesRegex(ValueError, "采样时刻"):
                verify_roundtrip(BASE, BASE + changed, BASE + PATCH, 10)

    def test_redundant_unchanged_policy_is_semantically_equivalent(self):
        duplicate = "  - samplePos: 4000\n    headTrackMode: head relative\n"
        verify_roundtrip(BASE, BASE + PATCH + duplicate, BASE + PATCH, 10)

    def test_same_sample_changes_resolve_to_the_final_policy(self):
        reverted = "  - samplePos: 3000\n    headTrackMode: scene relative\n    binauralRenderMode: near\n"
        verify_roundtrip(BASE, BASE + PATCH + reverted, BASE, 10)


if __name__ == "__main__":
    unittest.main()
