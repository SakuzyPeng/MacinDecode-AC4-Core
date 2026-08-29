#!/usr/bin/env python3
"""DME channel-based / native IMS 作业与生产链回归测试。"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import textwrap
import unittest
import wave
from pathlib import Path

from scripts.dme_native import (
    DmeChannelJob,
    DmeImsJob,
    ims_track_options,
    parse_channel_jobs,
    parse_ims_jobs,
    prepare_wave,
)
from scripts import dme_native_check

REPO_ROOT = Path(__file__).resolve().parent.parent


def pure_bed_case() -> dict:
    return {
        "case_id": "dme_native_smoke",
        "sample_rate": 48000,
        "duration_samples": 32,
        "frame_rate": "24",
        "offset_frames": 0,
        "bed": {
            "layout": "7.1.2",
            "channels": [
                "L",
                "R",
                "C",
                "LFE",
                "Lss",
                "Rss",
                "Lrs",
                "Rrs",
                "Lts",
                "Rts",
            ],
            "signal": {
                "kind": "per_channel_sine",
                "level_dbfs": -18.0,
                "base_hz": 200.0,
                "step_hz": 150.0,
                "lfe_hz": 40.0,
                "fade_samples": 0,
            },
        },
        "objects": [],
        "encodes": [],
    }


class DmeNativeJobsTests(unittest.TestCase):
    def test_channel_layouts_rates_and_names_are_stable(self) -> None:
        case = pure_bed_case()
        case["dme_channel"] = [
            {"layout": "stereo", "bitrate": 64},
            {"layout": "5.1", "bitrate": 128},
            {"layout": "5.1.4", "bitrate": 192},
        ]
        jobs = parse_channel_jobs(case)
        self.assertEqual(jobs[0], DmeChannelJob("stereo", 64))
        self.assertEqual(
            jobs[0].output_filename,
            "master_ac4_dme_channel_stereo_64K.m4a",
        )
        self.assertEqual(jobs[1].input_format, "wav")
        self.assertEqual(jobs[2].input_format, "cbi_wav")
        self.assertEqual(
            jobs[2].output_filename,
            "master_ac4_dme_channel_5_1_4_192K.m4a",
        )

    def test_ims_modes_inputs_and_names_are_stable(self) -> None:
        case = pure_bed_case()
        case["dme_ims"] = [
            {"input": "wav_5_1", "mode": "general", "bitrate": 256},
            {"input": "wav_5_1", "mode": "music", "bitrate": 256},
            {"input": "damf", "mode": "general", "bitrate": 320},
        ]
        jobs = parse_ims_jobs(case)
        self.assertEqual(jobs[0], DmeImsJob("wav_5_1", "general", 256))
        self.assertEqual(jobs[0].drc_profile, "film_light")
        self.assertEqual(jobs[1].drc_profile, "music_light")
        self.assertEqual(jobs[0].target_fps, "24")
        self.assertEqual(jobs[1].target_fps, "native")
        self.assertEqual(
            jobs[1].loudness_management,
            "measure_only:preset=manual:dialogue_intelligence=0",
        )
        self.assertEqual(
            jobs[1].output_filename,
            "master_ac4_dme_ims_music_wav_256K.m4a",
        )
        self.assertEqual(jobs[2].input_format, "atmos_mezz")

    def test_rejects_unsupported_or_ambiguous_jobs(self) -> None:
        base = pure_bed_case()
        invalid_channel = (
            {**base, "dme_channel": {}},
            {**base, "sample_rate": 44100, "dme_channel": [{"layout": "stereo", "bitrate": 64}]},
            {**base, "dme_channel": [{"layout": "7.1", "bitrate": 128}]},
            {**base, "dme_channel": [{"layout": "5.1.4", "bitrate": 128}]},
            {**base, "dme_channel": [{"layout": "stereo", "bitrate": True}]},
            {**base, "dme_channel": [{"layout": "stereo", "bitrate": 64, "mode": "x"}]},
            {**base, "dme_channel": [{"layout": "stereo", "bitrate": 64}] * 2},
        )
        for case in invalid_channel:
            with self.subTest(case=json.dumps(case, sort_keys=True)):
                with self.assertRaises(ValueError):
                    parse_channel_jobs(case)

        invalid_ims = (
            {**base, "dme_ims": {}},
            {**base, "sample_rate": 44100, "dme_ims": [{"input": "damf", "mode": "general", "bitrate": 256}]},
            {**base, "dme_ims": [{"input": "adm", "mode": "general", "bitrate": 256}]},
            {**base, "dme_ims": [{"input": "damf", "mode": "speech", "bitrate": 256}]},
            {**base, "dme_ims": [{"input": "damf", "mode": "general", "bitrate": 768}]},
            {**base, "dme_ims": [{"input": "damf", "mode": "general", "bitrate": 256, "legacy": False}]},
            {**base, "dme_ims": [{"input": "damf", "mode": "general", "bitrate": 256}] * 2},
        )
        for case in invalid_ims:
            with self.subTest(case=json.dumps(case, sort_keys=True)):
                with self.assertRaises(ValueError):
                    parse_ims_jobs(case)

    def test_speaker_wave_jobs_reject_objects_but_damf_ims_accepts_them(self) -> None:
        case = pure_bed_case()
        case["objects"] = [{"source_id": 10}]
        case["dme_channel"] = [{"layout": "stereo", "bitrate": 64}]
        with self.assertRaisesRegex(ValueError, "objects 为空"):
            parse_channel_jobs(case)

        case["dme_channel"] = []
        case["dme_ims"] = [
            {"input": "wav_5_1", "mode": "general", "bitrate": 256}
        ]
        with self.assertRaisesRegex(ValueError, "objects 为空"):
            parse_ims_jobs(case)

        case["dme_ims"] = [
            {"input": "damf", "mode": "general", "bitrate": 256}
        ]
        self.assertEqual(len(parse_ims_jobs(case)), 1)

    def test_prepares_exact_pcm24_speaker_wave_shape(self) -> None:
        case = pure_bed_case()
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "input.wav"
            prepare_wave(case, output, "5.1.4")
            with wave.open(str(output), "rb") as reader:
                self.assertEqual(reader.getnchannels(), 10)
                self.assertEqual(reader.getsampwidth(), 3)
                self.assertEqual(reader.getframerate(), 48000)
                self.assertEqual(reader.getnframes(), 32)
                payload = reader.readframes(32)
            self.assertEqual(len(payload), 32 * 10 * 3)
            self.assertNotEqual(payload, bytes(len(payload)))

    def test_native_ims_track_options_freeze_delay_and_duration(self) -> None:
        self.assertEqual(
            ims_track_options(48000, "general"), "offset=-2000:duration=48000"
        )
        self.assertEqual(
            ims_track_options(48000, "music"), "offset=0:duration=48000"
        )
        for value in (0, -1, True):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    ims_track_options(value, "general")


class DmeNativeCheckerTests(unittest.TestCase):
    @staticmethod
    def observation(frames: int, ch_mode: int, mode: str) -> dict:
        music = mode == "music"
        new_config = 0 if music else 3
        emdf = {
            "routed_infos": 0,
            "routed_frames": 0,
            "located_substreams": 0,
            "parsed_substreams": 0,
            "nonempty_substreams": 0,
            "empty_substreams": 0,
            "payloads": 0,
            "payload_bytes": 0,
            "failures": 0,
            "routes": [],
            "signatures": [],
        }
        if mode == "channel":
            emdf.update(
                {
                    "routed_infos": new_config,
                    "routed_frames": new_config,
                    "located_substreams": new_config,
                    "parsed_substreams": new_config,
                    "nonempty_substreams": new_config,
                    "payloads": new_config,
                    "payload_bytes": new_config,
                    "routes": [
                        {
                            "kind": "primary",
                            "emdf_version": 0,
                            "key_id": 0,
                            "substream_index": 2,
                            "count": new_config,
                        }
                    ],
                    "signatures": [
                        {
                            "id": 20,
                            "count": new_config,
                            "size_bytes": 1,
                            "fnv1a64": "af63bd4c8601b7df",
                            "opaque_prefix_hex": "00",
                            "opaque_prefix_truncated": False,
                            "config": {
                                "sample_offset": None,
                                "duration": None,
                                "group_id": None,
                                "codec_data": None,
                                "discard_unknown_payload": True,
                                "payload_frame_aligned": False,
                                "create_duplicate": False,
                                "remove_duplicate": False,
                                "priority": None,
                                "processing_allowed": None,
                            },
                        }
                    ],
                }
            )
        return {
            "frames": frames,
            "parse_failures": 0,
            "scene_path": "channel_based",
            "channel_modes": [ch_mode],
            "topology_integrity": {
                "substream_size_overruns": 0,
                "dangling_group_references": 0,
                "substream_reference_failures": 0,
                "frames_differing_from_first": 0,
                "config_generations": 1,
            },
            "audio": {
                "located": frames,
                "parsed": frames,
                "failures": 0,
                "first_error": None,
            },
            "dialogue_enhancement": {
                "absent": frames if music else 0,
                "present": 0 if music else frames,
                "new_config": new_config,
                "keep_previous": 0 if music else frames - new_config,
                "body_bits": 0,
                "max_body_bits": 0,
                "configurations": []
                if music
                else [
                    {
                        "method": 0,
                        "max_gain": 2,
                        "channel_config": 0,
                        "count": new_config,
                    }
                ],
            },
            "emdf": emdf,
        }

    def test_expected_frame_count_distinguishes_video_and_native_rates(self) -> None:
        self.assertEqual(dme_native_check.expected_frames(96000, "channel"), 49)
        self.assertEqual(dme_native_check.expected_frames(96000, "general"), 49)
        self.assertEqual(dme_native_check.expected_frames(96000, "music"), 47)

    def test_accepts_active_and_music_dialogue_enhancement_shapes(self) -> None:
        active = self.observation(49, 5, "general")
        self.assertEqual(
            dme_native_check.validate_observation(
                active,
                expected_ch_mode=5,
                expected_frame_count=49,
                mode="general",
            ),
            [],
        )
        music = self.observation(47, 5, "music")
        self.assertEqual(
            dme_native_check.validate_observation(
                music,
                expected_ch_mode=5,
                expected_frame_count=47,
                mode="music",
            ),
            [],
        )
        channel = self.observation(49, 12, "channel")
        self.assertEqual(
            dme_native_check.validate_observation(
                channel,
                expected_ch_mode=12,
                expected_frame_count=49,
                mode="channel",
            ),
            [],
        )

    def test_rejects_wrong_type_nonzero_body_and_unexpected_emdf(self) -> None:
        actual = self.observation(49, 5, "general")
        actual["channel_modes"] = [6]
        actual["dialogue_enhancement"]["body_bits"] = 1
        actual["dialogue_enhancement"]["max_body_bits"] = 1
        actual["emdf"]["routed_infos"] = 1
        problems = dme_native_check.validate_observation(
            actual,
            expected_ch_mode=5,
            expected_frame_count=49,
            mode="general",
        )
        self.assertTrue(any("ch_mode" in item for item in problems))
        self.assertTrue(any("body" in item for item in problems))
        self.assertTrue(any("emdf.routed_infos" in item for item in problems))

    def test_rejects_reference_failures_and_midstream_topology_changes(self) -> None:
        actual = self.observation(49, 5, "general")
        actual["topology_integrity"]["dangling_group_references"] = 1
        actual["topology_integrity"]["frames_differing_from_first"] = 12
        actual["topology_integrity"]["config_generations"] = 2

        problems = dme_native_check.validate_observation(
            actual,
            expected_ch_mode=5,
            expected_frame_count=49,
            mode="general",
        )

        self.assertTrue(any("引用不完整" in item for item in problems))
        self.assertTrue(any("配置不稳定" in item for item in problems))


class DmeNativeToolBoundaryTests(unittest.TestCase):
    @staticmethod
    def write_executable(path: Path, source: str = "#!/bin/sh\nexit 0\n") -> None:
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    def test_native_profile_requires_only_two_encoders_and_shared_muxer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            channel = root / "channel"
            ims = root / "ims"
            muxer = root / "muxer"
            for executable in (channel, ims, muxer):
                self.write_executable(executable)
            env_file = root / "tools.env"
            env_file.write_text(
                "\n".join(
                    (
                        f'DME_AC4_ENCODER="{channel}"',
                        f'DME_AC4_IMS_ENCODER="{ims}"',
                        f'DME_MP4MUXER="{muxer}"',
                    )
                )
                + "\n",
                encoding="utf-8",
            )
            environment = os.environ.copy()
            environment["MACINAC4_ENV_FILE"] = str(env_file)
            checked = subprocess.run(
                [
                    str(REPO_ROOT / "scripts" / "check_tools.sh"),
                    "--json",
                    "--profile",
                    "dme_native",
                ],
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
            tools = json.loads(checked.stdout)["tools"]
            native = tools["dme_native"]
            self.assertNotEqual(native["channel_encoder_sha256"], "")
            self.assertNotEqual(native["ims_encoder_sha256"], "")
            self.assertNotEqual(native["muxer_sha256"], "")
            self.assertEqual(tools["dme_ac4"]["encoder_sha256"], "")
            self.assertEqual(tools["dme_ac4"]["muxer_sha256"], "")
            self.assertEqual(tools["adm_normalizer"]["sha256"], "")


class BuildVectorDmeNativeTests(unittest.TestCase):
    @staticmethod
    def write_executable(path: Path, source: str) -> None:
        path.write_text(textwrap.dedent(source).lstrip(), encoding="utf-8")
        path.chmod(0o755)

    def test_native_profile_encodes_muxes_skips_and_cleans(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case_dir = root / "case"
            tools = root / "tools"
            case_dir.mkdir()
            tools.mkdir()

            case = pure_bed_case()
            case["duration_samples"] = 16
            case["dme_channel"] = [
                {"layout": "stereo", "bitrate": 64},
                {"layout": "5.1.4", "bitrate": 192},
            ]
            case["dme_ims"] = [
                {"input": "wav_5_1", "mode": "general", "bitrate": 256},
                {"input": "wav_5_1", "mode": "music", "bitrate": 256},
                {"input": "damf", "mode": "general", "bitrate": 320},
            ]
            case_path = case_dir / "case.json"
            case_path.write_text(json.dumps(case), encoding="utf-8")

            channel_encoder = tools / "channel_encoder"
            self.write_executable(
                channel_encoder,
                """
                #!/usr/bin/env python3
                import json, os, pathlib, sys, wave
                args = sys.argv[1:]
                source = pathlib.Path(args[args.index("--input") + 1])
                layout = args[args.index("--output-channel-layout") + 1]
                input_format = args[args.index("--input-format") + 1]
                with wave.open(str(source), "rb") as reader:
                    expected = 10 if layout == "5.1.4" else 2
                    assert reader.getnchannels() == expected
                assert input_format == ("cbi_wav" if layout == "5.1.4" else "wav")
                output = pathlib.Path(args[args.index("--output") + 1])
                manifest = pathlib.Path(args[args.index("--output-manifest") + 1])
                output.write_bytes(b"channel raw")
                manifest.write_text(json.dumps({"output_files": [{
                    "path": str(output.resolve()),
                    "duration": int(os.environ["FAKE_DURATION"]),
                    "offset": -2048,
                }]}), encoding="utf-8")
                """,
            )

            ims_encoder = tools / "ims_encoder"
            self.write_executable(
                ims_encoder,
                """
                #!/usr/bin/env python3
                import json, os, pathlib, sys, wave
                args = sys.argv[1:]
                source = pathlib.Path(args[args.index("--input") + 1])
                input_format = args[args.index("--input-format") + 1]
                encoder = args[args.index("--encoder") + 1]
                if input_format == "wav":
                    with wave.open(str(source), "rb") as reader:
                        assert reader.getnchannels() == 6
                else:
                    assert input_format == "atmos_mezz"
                    assert source.name == "master.atmos"
                assert ":iframe_interval=24" in encoder
                target_fps = args[args.index("--target-fps") + 1]
                loudness = args[args.index("--loudness-management") + 1]
                output = pathlib.Path(args[args.index("--output") + 1])
                output.write_bytes(b"ims raw")
                with pathlib.Path(os.environ["DME_NATIVE_ARGS_LOG"]).open(
                    "a", encoding="utf-8"
                ) as log:
                    log.write(json.dumps({
                        "format": input_format,
                        "encoder": encoder,
                        "target_fps": target_fps,
                        "loudness": loudness,
                    }) + "\\n")
                """,
            )

            muxer = tools / "muxer"
            self.write_executable(
                muxer,
                """
                #!/usr/bin/env python3
                import pathlib, shutil, sys
                args = sys.argv[1:]
                source = pathlib.Path(args[args.index("--track") + 1])
                output = pathlib.Path(args[args.index("--output") + 1])
                options = args[args.index("--track-options") + 1]
                assert options in (
                    "offset=-2048:duration=16",
                    "offset=-2000:duration=16",
                    "offset=0:duration=16",
                )
                shutil.copyfile(source, output)
                """,
            )

            ffprobe = tools / "ffprobe"
            self.write_executable(
                ffprobe,
                """
                #!/usr/bin/env python3
                import json
                print(json.dumps({"streams": [{"codec_type": "audio"}]}))
                """,
            )

            env_file = root / "tools.env"
            args_log = root / "native-args.log"
            env_file.write_text(
                "\n".join(
                    (
                        f'DME_AC4_ENCODER="{channel_encoder}"',
                        f'DME_AC4_IMS_ENCODER="{ims_encoder}"',
                        f'DME_MP4MUXER="{muxer}"',
                        f'FFPROBE="{ffprobe}"',
                    )
                )
                + "\n",
                encoding="utf-8",
            )
            environment = os.environ.copy()
            environment.update(
                {
                    "MACINAC4_ENV_FILE": str(env_file),
                    "FAKE_DURATION": "16",
                    "DME_NATIVE_ARGS_LOG": str(args_log),
                }
            )
            command = [
                str(REPO_ROOT / "scripts" / "build_vector.sh"),
                "--profile",
                "dme_native",
                str(case_path),
            ]
            first = subprocess.run(
                command,
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            outputs = sorted((case_dir / "encoded").glob("*.m4a"))
            self.assertEqual(len(outputs), 5)
            self.assertEqual(
                list((case_dir / "encoded").glob(".tmp_macinac4_dme.*")), []
            )
            invocations = [
                json.loads(line)
                for line in args_log.read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len(invocations), 3)
            self.assertEqual(
                [item["format"] for item in invocations],
                ["wav", "wav", "atmos_mezz"],
            )
            self.assertIn("mode=music:drc_profile=music_light", invocations[1]["encoder"])
            self.assertEqual(invocations[0]["target_fps"], "24")
            self.assertEqual(invocations[1]["target_fps"], "native")
            self.assertIn("dialogue_intelligence=0", invocations[1]["loudness"])

            second = subprocess.run(
                command,
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
            self.assertIn("跳过 DME channel", second.stdout)
            self.assertIn("跳过 DME native IMS", second.stdout)
            self.assertEqual(
                list((case_dir / "encoded").glob(".tmp_macinac4_dme.*")), []
            )


if __name__ == "__main__":
    unittest.main()
