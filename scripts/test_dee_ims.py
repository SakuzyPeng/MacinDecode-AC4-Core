import json
import os
import subprocess
import tempfile
import textwrap
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

from scripts.dee_ims import DeeImsJob, parse_jobs, render_template, workspace_path


REPO_ROOT = Path(__file__).resolve().parent.parent


TEMPLATE = """<?xml version="1.0"?>
<job_config>
  <filter><audio><encode_to_ims_ac4>
    <data_rate>256</data_rate>
    <ims_legacy_presentation>false</ims_legacy_presentation>
    <encoding_profile>ims</encoding_profile>
  </encode_to_ims_ac4></audio></filter>
  <output><ac4><file_name>FILE_NAME</file_name></ac4></output>
</job_config>
"""


class DeeImsJobsTests(unittest.TestCase):
    def test_defaults_and_output_names_are_stable(self) -> None:
        jobs = parse_jobs(
            {
                "dee_ims": [
                    {"bitrate": 256},
                    {
                        "bitrate": 320,
                        "encoding_profile": "ims_music",
                        "legacy_presentation": True,
                    },
                ]
            }
        )
        self.assertEqual(jobs[0], DeeImsJob(256, "ims", False))
        self.assertEqual(jobs[0].output_filename, "master_ac4_ims_256K.m4a")
        self.assertEqual(
            jobs[1].output_filename, "master_ac4_ims_music_legacy_320K.m4a"
        )

    def test_rejects_unsupported_or_ambiguous_jobs(self) -> None:
        invalid = (
            {"dee_ims": {"bitrate": 256}},
            {"dee_ims": [{"bitrate": 768}]},
            {"dee_ims": [{"bitrate": 256, "legacy_presentation": 1}]},
            {"dee_ims": [{"bitrate": 256, "encoding_profile": "broadcast"}]},
            {"dee_ims": [{"bitrate": 256, "legacy": True}]},
            {"dee_ims": [{"bitrate": 256}, {"bitrate": 256}]},
        )
        for case in invalid:
            with self.subTest(case=json.dumps(case, sort_keys=True)):
                with self.assertRaises(ValueError):
                    parse_jobs(case)

    def test_render_changes_only_case_controlled_fields(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            template = root / "template.xml"
            output = root / "job.xml"
            template.write_text(TEMPLATE, encoding="utf-8")

            render_template(
                output=output,
                template=template,
                job=DeeImsJob(144, "ims_music", True),
            )

            encode = ET.parse(output).getroot().find("./filter/audio/encode_to_ims_ac4")
            self.assertIsNotNone(encode)
            assert encode is not None
            self.assertEqual(encode.findtext("data_rate"), "144")
            self.assertEqual(encode.findtext("ims_legacy_presentation"), "true")
            self.assertEqual(encode.findtext("encoding_profile"), "ims_music")

    def test_rejects_mp4_template(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            template = root / "template.xml"
            output = root / "job.xml"
            template.write_text(
                TEMPLATE.replace("<ac4>", "<mp4>").replace("</ac4>", "</mp4>"),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "raw AC-4"):
                render_template(template, output, DeeImsJob(256))

    def test_rejects_template_with_both_raw_and_mp4_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            template = root / "template.xml"
            output = root / "job.xml"
            template.write_text(
                TEMPLATE.replace(
                    "</ac4></output>",
                    "</ac4><mp4><file_name>preview.mp4</file_name></mp4></output>",
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "raw AC-4"):
                render_template(template, output, DeeImsJob(256))

    def test_workspace_path_rejects_prefix_sibling(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            root = parent / "workspace"
            inside = root / "jobs" / "input.atmos"
            sibling = parent / "workspace-other" / "input.atmos"
            inside.parent.mkdir(parents=True)
            sibling.parent.mkdir(parents=True)
            inside.touch()
            sibling.touch()

            self.assertEqual(
                workspace_path(root, "y:", inside), "y:/jobs/input.atmos"
            )
            with self.assertRaisesRegex(ValueError, "不在工作区内"):
                workspace_path(root, "y:", sibling)


class DeeImsToolBoundaryTests(unittest.TestCase):
    def test_default_profile_does_not_claim_the_dee_muxer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            encoder = root / "encoder"
            normalizer = root / "normalizer"
            mp4box = root / "MP4Box"
            for executable in (encoder, normalizer, mp4box):
                executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                executable.chmod(0o755)
            env_file = root / "tools.env"
            env_file.write_text(
                "\n".join(
                    (
                        f'AC4_ENCODER="{encoder}"',
                        f'ADM_NORMALIZER="{normalizer}"',
                        f'MP4BOX="{mp4box}"',
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
                    "default",
                ],
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
            tools = json.loads(checked.stdout)["tools"]
            self.assertIsNone(tools["ac4_muxer"]["backend"])
            self.assertEqual(tools["ac4_muxer"]["sha256"], "")
            self.assertNotEqual(tools["mp4box"]["sha256"], "")

    def test_check_tools_rejects_template_outside_the_workspace(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workspace = root / "workspace"
            workspace.mkdir()
            wrapper = root / "dee"
            mp4box = root / "MP4Box"
            for executable in (wrapper, mp4box):
                executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                executable.chmod(0o755)
            engine = root / "engine"
            engine.write_bytes(b"engine")
            template = root / "template.xml"
            template.write_text(TEMPLATE, encoding="utf-8")
            env_file = root / "tools.env"
            env_file.write_text(
                "\n".join(
                    (
                        f'DEE_ENCODER="{wrapper}"',
                        f'DEE_ENGINE_BINARY="{engine}"',
                        f'DEE_IMS_TEMPLATE="{template}"',
                        f'DEE_WORKSPACE_ROOT="{workspace}"',
                        'DEE_WORKSPACE_DRIVE="y:"',
                        f'MP4BOX="{mp4box}"',
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
                    "--profile",
                    "dee_ims",
                ],
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(checked.returncode, 1)
            self.assertIn(
                "DEE_IMS_TEMPLATE 必须位于 DEE_WORKSPACE_ROOT 内",
                checked.stdout + checked.stderr,
            )


class RecordProvenanceTests(unittest.TestCase):
    def test_declared_output_must_exist_before_provenance_is_written(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            case_dir = Path(directory) / "case"
            (case_dir / "source").mkdir(parents=True)
            (case_dir / "encoded").mkdir()
            (case_dir / "source" / "master.atmos").write_bytes(b"source")
            (case_dir / "case.json").write_text(
                json.dumps(
                    {
                        "case_id": "missing_output",
                        "sample_rate": 48000,
                        "duration_samples": 1,
                        "frame_rate": "24",
                        "encodes": [],
                        "dee_ims": [{"bitrate": 256}],
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
            self.assertIn("缺少声明的编码产物", recorded.stdout + recorded.stderr)
            self.assertFalse((case_dir / "provenance.json").exists())
            self.assertFalse((case_dir / "hashes.sha256").exists())


class BuildVectorDeeImsTests(unittest.TestCase):
    def write_executable(self, path: Path, source: str) -> None:
        path.write_text(textwrap.dedent(source).lstrip(), encoding="utf-8")
        path.chmod(0o755)

    def test_dee_only_case_stages_muxes_and_cleans(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case_dir = root / "case"
            tools = root / "tools"
            workspace = root / "dee-workspace"
            case_dir.mkdir()
            tools.mkdir()
            workspace.mkdir()

            case = {
                "case_id": "dee_ims_smoke",
                "sample_rate": 48000,
                "duration_samples": 1,
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
                    "signal": {"kind": "silence"},
                },
                "objects": [],
                "encodes": [],
                "dee_ims": [
                    {"bitrate": 256},
                    {"bitrate": 256, "legacy_presentation": True},
                ],
            }
            case_path = case_dir / "case.json"
            case_path.write_text(
                json.dumps(case, ensure_ascii=False), encoding="utf-8"
            )

            template = workspace / "damf_encode_to_ims_ac4.xml"
            template.write_text(TEMPLATE, encoding="utf-8")
            engine = workspace / "dee.exe"
            engine.write_bytes(b"engine-fingerprint")

            fake_dee = tools / "dee"
            self.write_executable(
                fake_dee,
                r"""
                #!/usr/bin/env python3
                import os
                import pathlib
                import sys

                args = sys.argv[1:]
                root = pathlib.Path(os.environ["DEE_WORKSPACE_ROOT"])
                def host_path(option):
                    value = args[args.index(option) + 1]
                    relative = value.split(":", 1)[1].lstrip("/\\")
                    return root / relative

                output = host_path("--output")
                output.parent.mkdir(parents=True, exist_ok=True)
                output.write_bytes(b"fake raw ac4")
                log = host_path("--log-file")
                log.parent.mkdir(parents=True, exist_ok=True)
                log.write_text("DEE smoke log", encoding="utf-8")
                """,
            )

            fake_mp4box = tools / "MP4Box"
            self.write_executable(
                fake_mp4box,
                """
                #!/usr/bin/env python3
                import pathlib
                import shutil
                import sys

                args = sys.argv[1:]
                if "-version" in args:
                    print("MP4Box smoke 1.0")
                else:
                    source = pathlib.Path(args[args.index("-add") + 1])
                    output = pathlib.Path(args[args.index("-new") + 1])
                    shutil.copyfile(source, output)
                """,
            )

            fake_ffprobe = tools / "ffprobe"
            self.write_executable(
                fake_ffprobe,
                """
                #!/usr/bin/env python3
                import json
                import pathlib
                import sys
                if "-version" in sys.argv:
                    print("ffprobe smoke 1.0")
                else:
                    target = pathlib.Path(sys.argv[-1])
                    streams = [] if target.read_bytes() == b"invalid m4a" else [
                        {"codec_type": "audio"}
                    ]
                    print(json.dumps({"streams": streams}))
                """,
            )

            env_file = root / "tools.env"
            env_file.write_text(
                "\n".join(
                    (
                        f'DEE_ENCODER="{fake_dee}"',
                        f'DEE_ENGINE_BINARY="{engine}"',
                        f'DEE_IMS_TEMPLATE="{template}"',
                        f'DEE_WORKSPACE_ROOT="{workspace}"',
                        'DEE_WORKSPACE_DRIVE="y:"',
                        f'MP4BOX="{fake_mp4box}"',
                        f'FFPROBE="{fake_ffprobe}"',
                    )
                )
                + "\n",
                encoding="utf-8",
            )

            environment = os.environ.copy()
            environment["MACINAC4_ENV_FILE"] = str(env_file)
            command = [
                str(REPO_ROOT / "scripts" / "build_vector.sh"),
                "--profile",
                "dee_ims",
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
            self.assertTrue(
                (case_dir / "encoded" / "master_ac4_ims_256K.m4a").is_file()
            )
            self.assertTrue(
                (
                    case_dir / "encoded" / "master_ac4_ims_legacy_256K.m4a"
                ).is_file()
            )
            self.assertEqual(list(workspace.glob("tmp_macinac4_ims.*")), [])

            second = subprocess.run(
                command,
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
            self.assertIn("跳过 DEE IMS", second.stdout)
            self.assertEqual(list(workspace.glob("tmp_macinac4_ims.*")), [])

            checked = subprocess.run(
                [
                    str(REPO_ROOT / "scripts" / "check_tools.sh"),
                    "--json",
                    "--profile",
                    "dee_ims",
                ],
                cwd=REPO_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
            fingerprints = json.loads(checked.stdout)["tools"]["dee_ims_encoder"]
            self.assertNotEqual(fingerprints["wrapper_sha256"], "")
            self.assertNotEqual(fingerprints["engine_sha256"], "")
            self.assertNotEqual(fingerprints["template_sha256"], "")
            tools_fingerprints = json.loads(checked.stdout)["tools"]
            self.assertEqual(
                tools_fingerprints["ac4_muxer"]["backend"], "gpac_mp4box"
            )
            self.assertEqual(
                tools_fingerprints["ac4_muxer"]["sha256"],
                tools_fingerprints["mp4box"]["sha256"],
            )

            targets = (
                case_dir / "encoded" / "master_ac4_ims_256K.m4a",
                case_dir / "encoded" / "master_ac4_ims_legacy_256K.m4a",
            )
            original = {target: target.read_bytes() for target in targets}
            self.write_executable(
                fake_mp4box,
                """
                #!/usr/bin/env python3
                import pathlib
                import sys

                args = sys.argv[1:]
                if "-version" in args:
                    print("MP4Box smoke 1.0")
                else:
                    output = pathlib.Path(args[args.index("-new") + 1])
                    output.write_bytes(b"invalid m4a")
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
            for target in targets:
                self.assertEqual(target.read_bytes(), original[target])
            self.assertEqual(
                list((case_dir / "encoded").glob("*.tmp.*.m4a")), []
            )
            self.assertEqual(list(workspace.glob("tmp_macinac4_ims.*")), [])


if __name__ == "__main__":
    unittest.main()
