import json
import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

from scripts.dme_ac4 import (
    DmeAc4Job,
    manifest_track_options,
    parse_jobs,
    prepare_3dof_manifest,
)
from scripts.gen_damf import build_metadata


REPO_ROOT = Path(__file__).resolve().parent.parent


class DmeAc4JobsTests(unittest.TestCase):
    def test_levels_rates_and_output_names_are_stable(self) -> None:
        jobs = parse_jobs(
            {
                "sample_rate": 48000,
                "dme_ac4": [
                    {"level": 3, "bitrate": 768},
                    {"level": 4, "bitrate": 1500},
                    {"level": 4, "bitrate": 768, "mode": "3dof"},
                ],
            }
        )
        self.assertEqual(jobs[0], DmeAc4Job(3, 768))
        self.assertEqual(jobs[0].output_filename, "master_ac4_dme_l3_768K.m4a")
        self.assertEqual(jobs[1].output_filename, "master_ac4_dme_l4_1500K.m4a")
        self.assertEqual(jobs[2], DmeAc4Job(4, 768, "3dof"))
        self.assertEqual(
            jobs[2].output_filename, "master_ac4_dme_l4_768K_3dof.m4a"
        )
        self.assertEqual(
            jobs[2].provenance()["input_transform"],
            "damf_0.6.0_type_3dof",
        )

    def test_rejects_unsupported_or_ambiguous_jobs(self) -> None:
        invalid = (
            {"sample_rate": 48000, "dme_ac4": {"level": 3, "bitrate": 768}},
            {"sample_rate": 44100, "dme_ac4": [{"level": 3, "bitrate": 768}]},
            {"sample_rate": 48000, "dme_ac4": [{"level": True, "bitrate": 768}]},
            {"sample_rate": 48000, "dme_ac4": [{"level": 2, "bitrate": 768}]},
            {"sample_rate": 48000, "dme_ac4": [{"level": 3, "bitrate": 1500}]},
            {
                "sample_rate": 48000,
                "dme_ac4": [{"level": 3, "bitrate": 768, "mode": "3dof"}],
            },
            {
                "sample_rate": 48000,
                "dme_ac4": [{"level": 4, "bitrate": 1500, "mode": "music"}],
            },
            {
                "sample_rate": 48000,
                "dme_ac4": [
                    {"level": 4, "bitrate": 768},
                    {"level": 4, "bitrate": 768},
                ],
            },
        )
        for case in invalid:
            with self.subTest(case=json.dumps(case, sort_keys=True)):
                with self.assertRaises(ValueError):
                    parse_jobs(case)


class DmeThreeDofInputTests(unittest.TestCase):
    def test_prepares_isolated_damf_0_6_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.atmos"
            destination = root / "staged" / "master.atmos"
            source.write_text(
                textwrap.dedent(
                    """
                    version: 0.5.1
                    presentations:
                      - type: home
                        metadata: master.atmos.metadata
                        audio: master.atmos.audio
                    """
                ).lstrip(),
                encoding="utf-8",
            )

            prepare_3dof_manifest(source, destination)

            staged = destination.read_text(encoding="utf-8")
            self.assertIn("version: 0.6.0\n", staged)
            self.assertIn("  - type: 3dof\n", staged)
            self.assertIn("metadata: master.atmos.metadata", staged)
            self.assertIn("audio: master.atmos.audio", staged)
            self.assertIn("  - type: home\n", source.read_text(encoding="utf-8"))
            with self.assertRaisesRegex(ValueError, "原地改写"):
                prepare_3dof_manifest(source, source)

    def test_rejects_unsupported_or_multiple_presentations(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.atmos"
            destination = root / "master.atmos"
            for text, message in (
                ("version: 1.0.0\npresentations:\n  - type: home\n", "1.0.0"),
                (
                    "version: 0.5.1\npresentations:\n"
                    "  - type: home\n  - type: home\n",
                    "单 presentation",
                ),
                ("version: 0.5.1\npresentations:\n  - type: cinema\n", "cinema"),
            ):
                with self.subTest(message=message):
                    source.write_text(text, encoding="utf-8")
                    with self.assertRaisesRegex(ValueError, message):
                        prepare_3dof_manifest(source, destination)

    def test_head_track_mode_is_configurable_per_object(self) -> None:
        base_fields = {
            "size": 0.0,
            "decorr": 0,
            "snap": False,
            "elevation": True,
            "zones": "all",
            "gain": 0,
            "importance": 1,
            "screenFactor": 0,
            "depthFactor": 0,
        }
        case = {
            "sample_rate": 48000,
            "bed": {"channels": ["C"]},
            "objects": [
                {
                    "source_id": 1,
                    "segments": [{"start_samples": 0, "position": [0, 0, 0]}],
                    "static_fields": {
                        **base_fields,
                        "headTrackMode": "scene relative",
                    },
                },
                {
                    "source_id": 2,
                    "segments": [{"start_samples": 0, "position": [0, 0, 0]}],
                    "static_fields": {
                        **base_fields,
                        "headTrackMode": "head relative",
                    },
                },
            ],
        }

        metadata = build_metadata(case)

        self.assertRegex(
            metadata,
            r"(?s)- ID: 1.*?headTrackMode: scene relative",
        )
        self.assertRegex(
            metadata,
            r"(?s)- ID: 2.*?headTrackMode: head relative",
        )

        case["objects"][1]["static_fields"]["headTrackMode"] = "world relative"
        with self.assertRaisesRegex(ValueError, "headTrackMode"):
            build_metadata(case)


class DmeTimingManifestTests(unittest.TestCase):
    def test_returns_exact_muxer_options(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = root / "output.ac4"
            manifest = root / "manifest.json"
            raw.write_bytes(b"raw ac4")
            manifest.write_text(
                json.dumps(
                    {
                        "output_files": [
                            {"path": str(raw), "duration": 48000, "offset": -2048}
                        ]
                    }
                ),
                encoding="utf-8",
            )
            self.assertEqual(
                manifest_track_options(manifest, raw, 48000),
                "offset=-2048:duration=48000",
            )

    def test_rejects_wrong_output_or_duration(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = root / "output.ac4"
            other = root / "other.ac4"
            manifest = root / "manifest.json"
            raw.write_bytes(b"raw ac4")
            other.write_bytes(b"other")

            for entry, message in (
                ({"path": str(other), "duration": 48000, "offset": -2048}, "path"),
                ({"path": str(raw), "duration": 47999, "offset": -2048}, "时长"),
                ({"path": str(raw), "duration": 48000, "offset": False}, "offset"),
            ):
                with self.subTest(entry=entry):
                    manifest.write_text(
                        json.dumps({"output_files": [entry]}), encoding="utf-8"
                    )
                    with self.assertRaisesRegex(ValueError, message):
                        manifest_track_options(manifest, raw, 48000)


class DmeToolBoundaryTests(unittest.TestCase):
    def test_dme_profile_requires_only_normalizer_and_dme_pair(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            normalizer = root / "normalizer"
            encoder = root / "encoder"
            muxer = root / "muxer"
            for executable in (normalizer, encoder, muxer):
                executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                executable.chmod(0o755)
            env_file = root / "tools.env"
            env_file.write_text(
                "\n".join(
                    (
                        f'ADM_NORMALIZER="{normalizer}"',
                        f'DME_AC4_AJOC_ENCODER="{encoder}"',
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
                    "dme_ac4",
                ],
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
            tools = json.loads(checked.stdout)["tools"]
            self.assertNotEqual(tools["adm_normalizer"]["sha256"], "")
            self.assertNotEqual(tools["dme_ac4"]["encoder_sha256"], "")
            self.assertNotEqual(tools["dme_ac4"]["muxer_sha256"], "")
            self.assertEqual(tools["ac4_encoder"]["sha256"], "")
            self.assertEqual(tools["dee_ims_encoder"]["wrapper_sha256"], "")


class RecordProvenanceDmeTests(unittest.TestCase):
    def test_declared_dme_output_must_exist(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            case_dir = Path(directory) / "case"
            (case_dir / "source").mkdir(parents=True)
            (case_dir / "normalized").mkdir()
            (case_dir / "encoded").mkdir()
            (case_dir / "source" / "master.atmos").write_bytes(b"source")
            (case_dir / "normalized" / "output.wav").write_bytes(b"adm")
            (case_dir / "case.json").write_text(
                json.dumps(
                    {
                        "case_id": "missing_dme_output",
                        "sample_rate": 48000,
                        "duration_samples": 1,
                        "frame_rate": "24",
                        "encodes": [],
                        "dme_ac4": [{"level": 3, "bitrate": 768}],
                    }
                ),
                encoding="utf-8",
            )
            recorded = subprocess.run(
                [
                    str(REPO_ROOT / "scripts" / "record_provenance.py"),
                    str(case_dir),
                ],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(recorded.returncode, 0)
            self.assertIn(
                "encoded/master_ac4_dme_l3_768K.m4a",
                recorded.stdout + recorded.stderr,
            )
            self.assertFalse((case_dir / "provenance.json").exists())
            self.assertFalse((case_dir / "hashes.sha256").exists())


class BuildVectorDmeTests(unittest.TestCase):
    @staticmethod
    def write_executable(path: Path, source: str) -> None:
        path.write_text(textwrap.dedent(source).lstrip(), encoding="utf-8")
        path.chmod(0o755)

    def test_dme_only_case_encodes_muxes_skips_and_cleans(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case_dir = root / "case"
            tools = root / "tools"
            case_dir.mkdir()
            tools.mkdir()

            case = {
                "case_id": "dme_smoke",
                "sample_rate": 48000,
                "duration_samples": 16,
                "frame_rate": "24",
                "offset_frames": 0,
                "bed": {
                    "layout": "mono",
                    "channels": ["C"],
                    "signal": {"kind": "silence"},
                },
                "objects": [],
                "encodes": [],
                "dme_ac4": [
                    {"level": 4, "bitrate": 1500},
                    {"level": 4, "bitrate": 768, "mode": "3dof"},
                ],
                "dee_ims": [],
            }
            case_path = case_dir / "case.json"
            case_path.write_text(json.dumps(case), encoding="utf-8")

            normalizer = tools / "normalizer"
            self.write_executable(
                normalizer,
                """
                #!/usr/bin/env python3
                import pathlib
                import sys
                args = sys.argv[1:]
                output = pathlib.Path(args[args.index("-o") + 1])
                output.mkdir(parents=True, exist_ok=True)
                (output / "output.wav").write_bytes(b"fake adm bwf")
                """,
            )

            encoder = tools / "dme_encoder"
            self.write_executable(
                encoder,
                """
                #!/usr/bin/env python3
                import json
                import os
                import pathlib
                import sys
                args = sys.argv[1:]
                output = pathlib.Path(args[args.index("--output") + 1])
                manifest = pathlib.Path(args[args.index("--output-manifest") + 1])
                source = pathlib.Path(args[args.index("--input") + 1])
                encoder = args[args.index("--encoder") + 1]
                if encoder.startswith("mode=3dof:"):
                    staged = source.read_text(encoding="utf-8")
                    assert "version: 0.6.0" in staged
                    assert "  - type: 3dof" in staged
                    assert source.with_suffix(".atmos.audio").is_file()
                    assert source.with_suffix(".atmos.metadata").is_file()
                else:
                    assert encoder.startswith("mode=general:")
                    assert source.name == "output.wav"
                output.write_bytes(b"fake raw ac4")
                manifest.write_text(json.dumps({"output_files": [{
                    "path": str(output.resolve()),
                    "duration": int(os.environ["FAKE_DURATION"]),
                    "offset": -2048,
                }]}), encoding="utf-8")
                with pathlib.Path(os.environ["DME_ARGS_LOG"]).open(
                    "a", encoding="utf-8"
                ) as log:
                    log.write(json.dumps({"encoder": encoder, "input": str(source)}) + "\\n")
                """,
            )

            muxer = tools / "dme_muxer"
            self.write_executable(
                muxer,
                """
                #!/usr/bin/env python3
                import pathlib
                import shutil
                import sys
                args = sys.argv[1:]
                source = pathlib.Path(args[args.index("--track") + 1])
                output = pathlib.Path(args[args.index("--output") + 1])
                assert args[args.index("--track-options") + 1] == "offset=-2048:duration=16"
                shutil.copyfile(source, output)
                """,
            )

            ffprobe = tools / "ffprobe"
            self.write_executable(
                ffprobe,
                """
                #!/usr/bin/env python3
                import json
                import pathlib
                import sys
                target = pathlib.Path(sys.argv[-1])
                streams = [] if target.read_bytes().startswith(b"invalid") else [
                    {"codec_type": "audio"}
                ]
                print(json.dumps({"streams": streams}))
                """,
            )

            env_file = root / "tools.env"
            args_log = root / "dme-args.log"
            env_file.write_text(
                "\n".join(
                    (
                        f'ADM_NORMALIZER="{normalizer}"',
                        f'DME_AC4_AJOC_ENCODER="{encoder}"',
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
                    "DME_ARGS_LOG": str(args_log),
                }
            )
            command = [
                str(REPO_ROOT / "scripts" / "build_vector.sh"),
                "--profile",
                "dme_ac4",
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
            target = case_dir / "encoded" / "master_ac4_dme_l4_1500K.m4a"
            three_dof_target = (
                case_dir / "encoded" / "master_ac4_dme_l4_768K_3dof.m4a"
            )
            self.assertEqual(target.read_bytes(), b"fake raw ac4")
            self.assertEqual(three_dof_target.read_bytes(), b"fake raw ac4")
            invocations = [
                json.loads(line)
                for line in args_log.read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len(invocations), 2)
            self.assertTrue(invocations[0]["encoder"].startswith("mode=general:"))
            self.assertTrue(invocations[1]["encoder"].startswith("mode=3dof:"))
            self.assertTrue(invocations[1]["input"].endswith("/input/master.atmos"))
            self.assertEqual(
                list((case_dir / "encoded").glob(".tmp_macinac4_dme.*")), []
            )

            second = subprocess.run(
                command,
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
            self.assertIn("跳过 DME", second.stdout)

            original = target.read_bytes()
            self.write_executable(
                muxer,
                """
                #!/usr/bin/env python3
                import pathlib
                import sys
                args = sys.argv[1:]
                pathlib.Path(args[args.index("--output") + 1]).write_bytes(b"invalid")
                """,
            )
            forced = subprocess.run(
                [command[0], "--force", *command[1:]],
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(forced.returncode, 0)
            self.assertIn("保留既有产物", forced.stdout + forced.stderr)
            self.assertEqual(target.read_bytes(), original)
            self.assertEqual(
                list((case_dir / "encoded").glob(".tmp_macinac4_dme.*")),
                [],
                forced.stdout + forced.stderr,
            )


if __name__ == "__main__":
    unittest.main()
