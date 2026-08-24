#!/usr/bin/env python3
"""校验 DME A-JOC 作业、准备 3DoF DAMF 并读取 timing manifest。

``case.json`` 的可选 ``dme_ac4`` 数组保存可复现的 Level、码率与模式；DME
可执行文件路径继续由未纳入版本控制的 ``.env.local`` 提供。3DoF 作业使用
隔离生成的 DAMF 0.6.0 manifest，不改写案例的 canonical DAMF。
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path


ALLOWED_BITRATES = {
    3: frozenset((320, 448, 768)),
    4: frozenset((128, 256, 448, 768, 1500)),
}
ALLOWED_MODES = frozenset(("general", "3dof"))
ALLOWED_KEYS = frozenset(("level", "bitrate", "mode"))
DAMF_3DOF_VERSION = "0.6.0"


@dataclass(frozen=True)
class DmeAc4Job:
    """一个已校验且具有唯一输出名的 DME A-JOC 编码作业。"""

    level: int
    bitrate: int
    mode: str = "general"

    @property
    def output_filename(self) -> str:
        mode_suffix = "_3dof" if self.mode == "3dof" else ""
        return f"master_ac4_dme_l{self.level}_{self.bitrate}K{mode_suffix}.m4a"

    def provenance(self) -> dict[str, object]:
        out: dict[str, object] = {
            "level": self.level,
            "bitrate_kbps": self.bitrate,
            "start_samples": 0,
            "encoder_mode": self.mode,
            "drc_profile": "film_light",
            "iframe_interval": "1sec",
            "loudness_management": "measure_only",
            "output": f"encoded/{self.output_filename}",
        }
        if self.mode == "3dof":
            out["input_transform"] = "damf_0.6.0_type_3dof"
        return out


def prepare_3dof_manifest(source: Path, destination: Path) -> None:
    """把单 presentation 的 canonical DAMF manifest 提升为 3DoF 0.6.0。

    只改写版本与 presentation type；音频和逐对象 metadata 文件仍由原
    manifest 的相对路径引用。输入不符合生成器的单 presentation 形态时失败关闭。
    """

    if source.resolve() == destination.resolve():
        raise ValueError("3DoF staging 不能原地改写 canonical DAMF manifest")

    try:
        text = source.read_text(encoding="utf-8")
    except OSError as error:
        raise ValueError(f"无法读取 DAMF manifest {source}：{error}") from error

    lines = text.splitlines()
    version_indices = [
        index for index, line in enumerate(lines) if line.startswith("version: ")
    ]
    type_indices = [
        index for index, line in enumerate(lines) if line.startswith("  - type: ")
    ]
    if len(version_indices) != 1:
        raise ValueError("DAMF manifest 必须恰含一个顶层 version")
    if len(type_indices) != 1:
        raise ValueError("3DoF staging 只支持单 presentation DAMF")

    version_index = version_indices[0]
    version = lines[version_index].removeprefix("version: ")
    if not (version.startswith("0.5.") or version.startswith("0.6.")):
        raise ValueError(f"无法把 DAMF {version} 提升为 3DoF 0.6.0")

    type_index = type_indices[0]
    presentation_type = lines[type_index].removeprefix("  - type: ")
    if presentation_type not in ("home", "3dof"):
        raise ValueError(
            f"3DoF staging 不接受 presentation type {presentation_type}"
        )

    lines[version_index] = f"version: {DAMF_3DOF_VERSION}"
    lines[type_index] = "  - type: 3dof"
    try:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text("\n".join(lines) + "\n", encoding="utf-8")
    except OSError as error:
        raise ValueError(f"无法写入 3DoF DAMF manifest {destination}：{error}") from error


def parse_jobs(case: dict[str, object]) -> list[DmeAc4Job]:
    """读取并严格校验 case 的 ``dme_ac4`` 数组。"""

    raw = case.get("dme_ac4", [])
    if not isinstance(raw, list):
        raise ValueError("dme_ac4 必须是数组")
    if raw and case.get("sample_rate") != 48000:
        raise ValueError("DME A-JOC 作业要求 sample_rate 为 48000")

    jobs: list[DmeAc4Job] = []
    outputs: set[str] = set()
    for index, value in enumerate(raw):
        label = f"dme_ac4[{index}]"
        if not isinstance(value, dict):
            raise ValueError(f"{label} 必须是对象")
        unknown = sorted(set(value).difference(ALLOWED_KEYS))
        if unknown:
            raise ValueError(f"{label} 含未知字段：{', '.join(unknown)}")

        level = value.get("level")
        if type(level) is not int or level not in ALLOWED_BITRATES:
            raise ValueError(f"{label}.level 必须是整数 3 或 4")
        bitrate = value.get("bitrate")
        allowed = ALLOWED_BITRATES[level]
        if type(bitrate) is not int or bitrate not in allowed:
            choices = ", ".join(str(item) for item in sorted(allowed))
            raise ValueError(
                f"{label}.bitrate 对 Level {level} 必须是以下整数之一：{choices}"
            )
        mode = value.get("mode", "general")
        if not isinstance(mode, str) or mode not in ALLOWED_MODES:
            raise ValueError(f"{label}.mode 必须是 general 或 3dof")
        if mode == "3dof" and level != 4:
            raise ValueError(f"{label} 的 3dof 模式只支持 Level 4")

        job = DmeAc4Job(level, bitrate, mode)
        if job.output_filename in outputs:
            raise ValueError(f"{label} 与前一作业输出重名：{job.output_filename}")
        outputs.add(job.output_filename)
        jobs.append(job)
    return jobs


def load_jobs(case_path: Path) -> list[DmeAc4Job]:
    try:
        case = json.loads(case_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"无法读取 {case_path}：{error}") from error
    if not isinstance(case, dict):
        raise ValueError("case.json 顶层必须是对象")
    return parse_jobs(case)


def manifest_track_options(
    manifest_path: Path, raw_output: Path, expected_duration: int
) -> str:
    """返回官方 muxer 所需的 ``offset``/``duration``，并失败关闭。"""

    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"无法读取 DME timing manifest {manifest_path}：{error}") from error
    if not isinstance(manifest, dict):
        raise ValueError("DME timing manifest 顶层必须是对象")
    outputs = manifest.get("output_files")
    if not isinstance(outputs, list) or len(outputs) != 1:
        raise ValueError("DME timing manifest 必须恰含一个 output_files 条目")
    output = outputs[0]
    if not isinstance(output, dict):
        raise ValueError("DME timing manifest 的 output_files[0] 必须是对象")

    encoded_path = output.get("path")
    if not isinstance(encoded_path, str):
        raise ValueError("DME timing manifest 缺少字符串 path")
    if Path(encoded_path).resolve() != raw_output.resolve():
        raise ValueError("DME timing manifest 的 path 与本次 raw AC-4 输出不一致")

    duration = output.get("duration")
    offset = output.get("offset")
    if type(duration) is not int or duration <= 0:
        raise ValueError("DME timing manifest 的 duration 必须是正整数")
    if type(offset) is not int:
        raise ValueError("DME timing manifest 的 offset 必须是整数")
    if duration != expected_duration:
        raise ValueError(
            f"DME 输出时长 {duration} 与 case.json 的 {expected_duration} samples 不一致"
        )
    return f"offset={offset}:duration={duration}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    list_parser = commands.add_parser("list", help="以 TSV 输出 case 中的 DME A-JOC 作业")
    list_parser.add_argument("case", type=Path)

    timing_parser = commands.add_parser(
        "track-options", help="校验 timing manifest 并输出 muxer track-options"
    )
    timing_parser.add_argument("manifest", type=Path)
    timing_parser.add_argument("raw_output", type=Path)
    timing_parser.add_argument("--expected-duration", type=int, required=True)

    prepare_parser = commands.add_parser(
        "prepare-3dof", help="生成隔离的 DAMF 0.6.0/type: 3dof manifest"
    )
    prepare_parser.add_argument("source", type=Path)
    prepare_parser.add_argument("destination", type=Path)

    args = parser.parse_args()
    try:
        if args.command == "list":
            for job in load_jobs(args.case):
                print(
                    job.level,
                    job.bitrate,
                    job.mode,
                    job.output_filename,
                    sep="\t",
                )
        elif args.command == "track-options":
            print(
                manifest_track_options(
                    args.manifest, args.raw_output, args.expected_duration
                )
            )
        else:
            prepare_3dof_manifest(args.source, args.destination)
    except ValueError as error:
        print(f"错误：{error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
