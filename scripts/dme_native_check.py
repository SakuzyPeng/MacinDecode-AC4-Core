#!/usr/bin/env python3
"""校验 DME channel-based / native IMS 向量的拓扑与 DE metadata。

    ./scripts/dme_native_check.py
    ./scripts/dme_native_check.py vectors/<case_id> [...]

预期由 ``case.json`` 的 ``dme_channel``/``dme_ims`` 作业直接推出，不另建可被
随意重冻的基线：channel layout 对应固定 ``ch_mode``；5.1 WAVE IMS 为 5，DAMF
IMS 为 6；general 与 channel encoder 必须全帧传输已观测的 DE 配置，music
必须全帧缺席。当前向量的 DE body 为 0 bit，本门禁只证明 presence/config/keep，
不把它解释为非零 Huffman 参数覆盖。
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

if __package__:
    from .dme_native import load_case, parse_channel_jobs, parse_ims_jobs
else:
    from dme_native import load_case, parse_channel_jobs, parse_ims_jobs

REPO_ROOT = Path(__file__).resolve().parent.parent
VECTORS = REPO_ROOT / "vectors"
CHANNEL_MODES = {"stereo": 1, "5.1": 4, "5.1.4": 12}


def _integer(value, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ValueError(f"{label} 必须是非负整数")
    return value


def inspect(path: Path) -> dict:
    try:
        result = subprocess.run(
            [
                "cargo",
                "run",
                "-q",
                "--release",
                "--locked",
                "--manifest-path",
                str(REPO_ROOT / "Cargo.toml"),
                "--bin",
                "macinac4",
                "--",
                "trace",
                str(path),
            ],
            capture_output=True,
            text=True,
        )
    except OSError as error:
        raise RuntimeError(f"无法启动 trace：{error}") from error
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "trace 失败")

    try:
        envelope = json.loads(result.stdout)
        if envelope.get("schema") != "macinac4.cli-result":
            raise ValueError("success envelope schema 不匹配")
        validation = envelope["result"]["validation"]
        topology = validation["topology"]
        frames = _integer(topology["coverage"]["frames_parsed"], "frames_parsed")
        parse_failures = _integer(
            topology["coverage"]["parse_failures"], "parse_failures"
        )
        scene_path = topology["configuration"]["scene_path"]
        groups = topology["observations"]["first_frame"]["substream_groups"]
        if not isinstance(groups, list):
            raise ValueError("first_frame.substream_groups 必须是数组")
        channel_modes = []
        for group in groups:
            substreams = group["substreams"]
            if not isinstance(substreams, list):
                raise ValueError("first_frame group.substreams 必须是数组")
            for substream in substreams:
                if substream.get("kind") != "channel":
                    raise ValueError("DME native 向量出现非 channel substream")
                channel_modes.append(
                    _integer(substream["ch_mode"], "first_frame.substream.ch_mode")
                )
        audio = validation["audio_substream"]
        located = _integer(audio["coverage"]["located"], "audio.located")
        parsed = _integer(audio["coverage"]["parsed"], "audio.parsed")
        failures = _integer(audio["coverage"]["failures"], "audio.failures")
        first_error = audio["coverage"]["first_error"]
        de = audio["observations"]["dialogue_enhancement"]
        emdf = topology["observations"]["emdf"]
    except (json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        raise RuntimeError(f"trace 输出无效：{error}") from error

    if not isinstance(scene_path, str):
        raise RuntimeError("trace 输出无效：scene_path 必须是字符串")
    return {
        "frames": frames,
        "parse_failures": parse_failures,
        "scene_path": scene_path,
        "channel_modes": channel_modes,
        "audio": {
            "located": located,
            "parsed": parsed,
            "failures": failures,
            "first_error": first_error,
        },
        "dialogue_enhancement": de,
        "emdf": emdf,
    }


def expected_frames(duration_samples: int, mode: str) -> int:
    if type(duration_samples) is not int or duration_samples <= 0:
        raise ValueError("duration_samples 必须是正整数")
    if mode == "music":
        return (duration_samples + 2047) // 2048
    if mode in ("general", "channel"):
        return (duration_samples + 3999) // 2000
    raise ValueError("mode 必须是 channel、general 或 music")


def validate_observation(
    actual: dict,
    *,
    expected_ch_mode: int,
    expected_frame_count: int,
    mode: str,
) -> list[str]:
    problems = []
    frames = actual.get("frames")
    if frames != expected_frame_count:
        problems.append(f"codec frame={frames}，预期 {expected_frame_count}")
    if actual.get("parse_failures") != 0:
        problems.append(f"topology parse_failures={actual.get('parse_failures')}")
    if actual.get("scene_path") != "channel_based":
        problems.append(f"scene_path={actual.get('scene_path')!r}")
    if actual.get("channel_modes") != [expected_ch_mode]:
        problems.append(
            f"ch_mode={actual.get('channel_modes')!r}，预期 [{expected_ch_mode}]"
        )

    audio = actual.get("audio")
    if not isinstance(audio, dict):
        problems.append("缺少 audio coverage")
    elif (
        audio.get("located") != frames
        or audio.get("parsed") != frames
        or audio.get("failures") != 0
        or audio.get("first_error") is not None
    ):
        problems.append(f"audio coverage 不完整：{audio}")

    new_config_count = None
    de = actual.get("dialogue_enhancement")
    if not isinstance(de, dict):
        problems.append("缺少 dialogue_enhancement census")
    else:
        try:
            absent = _integer(de["absent"], "de.absent")
            present = _integer(de["present"], "de.present")
            new_config = _integer(de["new_config"], "de.new_config")
            new_config_count = new_config
            keep = _integer(de["keep_previous"], "de.keep_previous")
            body_bits = _integer(de["body_bits"], "de.body_bits")
            max_body_bits = _integer(de["max_body_bits"], "de.max_body_bits")
            configurations = de["configurations"]
            if not isinstance(configurations, list):
                raise ValueError("de.configurations 必须是数组")
        except (KeyError, TypeError, ValueError) as error:
            problems.append(str(error))
        else:
            if body_bits != 0 or max_body_bits != 0:
                problems.append(
                    f"DE body 非零：total={body_bits}，max={max_body_bits}"
                )
            if mode == "music":
                if (absent, present, new_config, keep) != (frames, 0, 0, 0):
                    problems.append(
                        "music DE census 应全缺席，实际 "
                        f"absent/present/new/keep={absent}/{present}/{new_config}/{keep}"
                    )
                if configurations:
                    problems.append("music DE 不应报告配置")
            else:
                if absent != 0 or present != frames or new_config == 0:
                    problems.append(
                        "活动 DE census 不完整："
                        f"absent/present/new={absent}/{present}/{new_config}"
                    )
                if new_config + keep != frames:
                    problems.append(
                        f"DE new+keep={new_config + keep}，codec frame={frames}"
                    )
                expected_configurations = [
                    {
                        "method": 0,
                        "max_gain": 2,
                        "channel_config": 0,
                        "count": new_config,
                    }
                ]
                if configurations != expected_configurations:
                    problems.append(f"DE 配置不匹配：{configurations!r}")

    emdf = actual.get("emdf")
    if not isinstance(emdf, dict):
        problems.append("缺少 EMDF census")
    else:
        if mode == "channel" and new_config_count is not None:
            for key in (
                "routed_infos",
                "routed_frames",
                "located_substreams",
                "parsed_substreams",
                "nonempty_substreams",
                "payloads",
                "payload_bytes",
            ):
                if emdf.get(key) != new_config_count:
                    problems.append(
                        f"channel emdf.{key}={emdf.get(key)}，预期 {new_config_count}"
                    )
            for key in ("empty_substreams", "failures"):
                if emdf.get(key) != 0:
                    problems.append(f"channel emdf.{key}={emdf.get(key)}，预期 0")
            expected_routes = [
                {
                    "kind": "primary",
                    "emdf_version": 0,
                    "key_id": 0,
                    "substream_index": 2,
                    "count": new_config_count,
                }
            ]
            if emdf.get("routes") != expected_routes:
                problems.append(f"channel EMDF 路由不匹配：{emdf.get('routes')!r}")
            expected_signatures = [
                {
                    "id": 20,
                    "count": new_config_count,
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
            ]
            if emdf.get("signatures") != expected_signatures:
                problems.append(
                    f"channel EMDF payload 签名不匹配：{emdf.get('signatures')!r}"
                )
        else:
            for key in (
                "routed_infos",
                "routed_frames",
                "located_substreams",
                "parsed_substreams",
                "payloads",
                "payload_bytes",
                "failures",
            ):
                if emdf.get(key) != 0:
                    problems.append(
                        f"native IMS 的 emdf.{key}={emdf.get(key)}，预期 0"
                    )
    return problems


def declared_jobs(case_dir: Path) -> tuple[dict, list[tuple[str, Path, int, str]]]:
    case_path = case_dir / "case.json"
    case = load_case(case_path)
    jobs = []
    for job in parse_channel_jobs(case):
        jobs.append(
            (
                job.output_filename,
                case_dir / "encoded" / job.output_filename,
                CHANNEL_MODES[job.layout],
                "channel",
            )
        )
    for job in parse_ims_jobs(case):
        jobs.append(
            (
                job.output_filename,
                case_dir / "encoded" / job.output_filename,
                5 if job.input == "wav_5_1" else 6,
                job.mode,
            )
        )
    names = [name for name, *_ in jobs]
    if len(names) != len(set(names)):
        raise ValueError("DME native 作业输出重名")
    return case, jobs


def default_case_dirs() -> list[Path]:
    return sorted(path.parent for path in VECTORS.glob("*/case.json"))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cases", nargs="*", type=Path)
    args = parser.parse_args()

    work = []
    failed = False
    for case_dir in args.cases or default_case_dirs():
        try:
            case, jobs = declared_jobs(case_dir)
        except ValueError as error:
            print(f"{case_dir.name}：case 无效：{error}", file=sys.stderr)
            failed = True
            continue
        if jobs:
            work.append((case_dir, case, jobs))
    if not work:
        print("没有声明 DME native 作业的案例", file=sys.stderr)
        return 1

    missing = []
    for case_dir, _, jobs in work:
        for name, path, _, _ in jobs:
            if not path.is_file():
                missing.append((case_dir.name, name))
    for case_id, name in missing:
        print(f"{case_id}/{name}：找不到输入", file=sys.stderr)
    if missing or failed:
        return 1

    checked = 0
    for case_dir, case, jobs in work:
        duration = case.get("duration_samples")
        for name, path, ch_mode, mode in jobs:
            try:
                frame_count = expected_frames(duration, mode)
                actual = inspect(path)
            except (ValueError, RuntimeError) as error:
                print(f"{case_dir.name}/{name}：检查失败：{error}", file=sys.stderr)
                failed = True
                continue
            problems = validate_observation(
                actual,
                expected_ch_mode=ch_mode,
                expected_frame_count=frame_count,
                mode=mode,
            )
            if problems:
                print(
                    f"{case_dir.name}/{name}：{'；'.join(problems)}",
                    file=sys.stderr,
                )
                failed = True
                continue
            checked += 1
            de = actual["dialogue_enhancement"]
            print(
                f"{case_dir.name}/{name}：{frame_count} frame，ch_mode={ch_mode}，"
                f"DE present/absent={de['present']}/{de['absent']}"
            )

    if failed:
        print("DME native 向量门禁未通过", file=sys.stderr)
        return 1
    print(f"DME native 向量门禁通过：{checked} 条媒体")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
