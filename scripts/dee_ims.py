#!/usr/bin/env python3
"""校验 DEE IMS 作业并渲染单次编码所需的 XML。

``case.json`` 的可选 ``dee_ims`` 数组只描述与 IMS 编码相关的参数；外部
工具路径继续由未纳入版本控制的 ``.env.local`` 提供。
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path


ALLOWED_BITRATES = frozenset((64, 72, 112, 144, 256, 320))
ALLOWED_PROFILES = frozenset(("ims", "ims_music"))
ALLOWED_KEYS = frozenset(("bitrate", "encoding_profile", "legacy_presentation"))


@dataclass(frozen=True)
class DeeImsJob:
    """一个已校验且具有唯一输出名的 DEE IMS 编码作业。"""

    bitrate: int
    encoding_profile: str = "ims"
    legacy_presentation: bool = False

    @property
    def output_filename(self) -> str:
        profile = "ims" if self.encoding_profile == "ims" else "ims_music"
        legacy = "_legacy" if self.legacy_presentation else ""
        return f"master_ac4_{profile}{legacy}_{self.bitrate}K.m4a"

    def provenance(self) -> dict[str, object]:
        return {
            "bitrate_kbps": self.bitrate,
            "encoding_profile": self.encoding_profile,
            "legacy_presentation": self.legacy_presentation,
            "output": f"encoded/{self.output_filename}",
        }


def parse_jobs(case: dict[str, object]) -> list[DeeImsJob]:
    """读取并严格校验 case 的 ``dee_ims`` 数组。"""

    raw = case.get("dee_ims", [])
    if not isinstance(raw, list):
        raise ValueError("dee_ims 必须是数组")

    jobs: list[DeeImsJob] = []
    outputs: set[str] = set()
    for index, value in enumerate(raw):
        label = f"dee_ims[{index}]"
        if not isinstance(value, dict):
            raise ValueError(f"{label} 必须是对象")
        unknown = sorted(set(value).difference(ALLOWED_KEYS))
        if unknown:
            raise ValueError(f"{label} 含未知字段：{', '.join(unknown)}")

        bitrate = value.get("bitrate")
        if type(bitrate) is not int or bitrate not in ALLOWED_BITRATES:
            allowed = ", ".join(str(item) for item in sorted(ALLOWED_BITRATES))
            raise ValueError(f"{label}.bitrate 必须是以下整数之一：{allowed}")

        profile = value.get("encoding_profile", "ims")
        if not isinstance(profile, str) or profile not in ALLOWED_PROFILES:
            raise ValueError(f"{label}.encoding_profile 必须是 ims 或 ims_music")

        legacy = value.get("legacy_presentation", False)
        if type(legacy) is not bool:
            raise ValueError(f"{label}.legacy_presentation 必须是布尔值")

        job = DeeImsJob(bitrate, profile, legacy)
        if job.output_filename in outputs:
            raise ValueError(f"{label} 与前一作业输出重名：{job.output_filename}")
        outputs.add(job.output_filename)
        jobs.append(job)
    return jobs


def load_jobs(case_path: Path) -> list[DeeImsJob]:
    try:
        case = json.loads(case_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"无法读取 {case_path}：{error}") from error
    if not isinstance(case, dict):
        raise ValueError("case.json 顶层必须是对象")
    return parse_jobs(case)


def _set_required(parent: ET.Element, name: str, value: str) -> None:
    nodes = parent.findall(name)
    if len(nodes) != 1:
        raise ValueError(f"DEE IMS 模板必须恰含一个 encode_to_ims_ac4/{name}")
    nodes[0].text = value


def render_template(template: Path, output: Path, job: DeeImsJob) -> None:
    """复制官方 raw AC-4 模板，并只替换受 case 控制的三个字段。"""

    try:
        tree = ET.parse(template)
    except (OSError, ET.ParseError) as error:
        raise ValueError(f"无法读取 DEE IMS 模板 {template}：{error}") from error

    root = tree.getroot()
    filters = root.findall("./filter/audio/encode_to_ims_ac4")
    if len(filters) != 1:
        raise ValueError("DEE IMS 模板必须恰含一个 encode_to_ims_ac4 filter")
    outputs = root.findall("./output")
    if (
        len(outputs) != 1
        or len(outputs[0]) != 1
        or outputs[0][0].tag != "ac4"
    ):
        raise ValueError("DEE IMS 模板必须使用 raw AC-4 output，而不是 MP4 output")

    encode = filters[0]
    _set_required(encode, "data_rate", str(job.bitrate))
    _set_required(
        encode,
        "ims_legacy_presentation",
        "true" if job.legacy_presentation else "false",
    )
    _set_required(encode, "encoding_profile", job.encoding_profile)

    output.parent.mkdir(parents=True, exist_ok=True)
    ET.indent(tree, space="  ")
    tree.write(output, encoding="utf-8", xml_declaration=True)


def workspace_path(root: Path, drive: str, path: Path) -> str:
    """把 host staging 路径转换为包装器映射的 Windows drive 路径。"""

    if re.fullmatch(r"[A-Za-z]:", drive) is None:
        raise ValueError("DEE_WORKSPACE_DRIVE 必须形如 y:")
    resolved_root = root.resolve()
    resolved_path = path.resolve()
    try:
        relative = resolved_path.relative_to(resolved_root)
    except ValueError as error:
        raise ValueError(f"DEE 路径不在工作区内：{resolved_path}") from error
    suffix = relative.as_posix()
    return f"{drive}/{suffix}" if suffix != "." else f"{drive}/"


def _job_from_args(args: argparse.Namespace) -> DeeImsJob:
    return DeeImsJob(
        bitrate=args.bitrate,
        encoding_profile=args.encoding_profile,
        legacy_presentation=args.legacy_presentation == "true",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    list_parser = commands.add_parser("list", help="以 TSV 输出 case 中的 DEE IMS 作业")
    list_parser.add_argument("case", type=Path)

    render_parser = commands.add_parser("render", help="渲染单个 DEE IMS XML")
    render_parser.add_argument("template", type=Path)
    render_parser.add_argument("output", type=Path)
    render_parser.add_argument(
        "--bitrate", type=int, choices=sorted(ALLOWED_BITRATES), required=True
    )
    render_parser.add_argument(
        "--encoding-profile", choices=sorted(ALLOWED_PROFILES), required=True
    )
    render_parser.add_argument(
        "--legacy-presentation", choices=("true", "false"), required=True
    )

    path_parser = commands.add_parser("workspace-path", help="转换 DEE 工作区内的路径")
    path_parser.add_argument("root", type=Path)
    path_parser.add_argument("drive")
    path_parser.add_argument("path", type=Path)

    args = parser.parse_args()
    try:
        if args.command == "list":
            for job in load_jobs(args.case):
                print(
                    job.bitrate,
                    job.encoding_profile,
                    str(job.legacy_presentation).lower(),
                    job.output_filename,
                    sep="\t",
                )
        elif args.command == "render":
            render_template(args.template, args.output, _job_from_args(args))
        else:
            print(workspace_path(args.root, args.drive, args.path))
    except ValueError as error:
        print(f"错误：{error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
