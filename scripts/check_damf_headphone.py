#!/usr/bin/env python3
"""显式验证官方工具接受 DAMF 局部耳机事件，且原生往返不改写连续字段命令。

需要本地 canonical DAMF（默认检查对象 ID 10）与自行安装的 Dolby 工具。
所有测试包位于临时目录，不修改输入。ADM 转换会重写 ramp，不作为保真判据。
"""

from __future__ import annotations

import argparse
from itertools import groupby
import json
import os
from pathlib import Path
import subprocess
import tempfile


HEADPHONE_FIELDS = frozenset(("headTrackMode", "binauralRenderMode"))


def read_events(text: str) -> list[dict[str, str]]:
    """读取本检查器使用的平面 DAMF 字段事件，支持官方 writer 省略重复 ID。"""
    records: list[dict[str, str]] = []
    for line in text.splitlines():
        if line.startswith("  - "):
            records.append({})
            line = line[4:]
        elif not line.startswith("    ") or not records:
            continue
        key, separator, value = line.strip().partition(":")
        if separator:
            records[-1][key] = value.strip()
    previous_id: str | None = None
    for record in records:
        current_id = record.get("ID", previous_id)
        if current_id is None or "samplePos" not in record:
            raise ValueError("DAMF event 缺少 ID 或 samplePos")
        record["ID"] = current_id
        previous_id = current_id
        if int(record["samplePos"]) < 0:
            raise ValueError("DAMF event samplePos 必须非负")
    return records


def continuous_commands(text: str, object_id: int) -> list[tuple]:
    out = []
    for record in read_events(text):
        if int(record["ID"]) != object_id:
            continue
        fields = tuple(sorted((key, value) for key, value in record.items()
                              if key not in HEADPHONE_FIELDS | {"ID", "samplePos"}))
        if fields:
            out.append((int(record["samplePos"]), fields))
    return out


def headphone_timeline(text: str, object_id: int) -> list[tuple[int, str, str]]:
    records = [record for record in read_events(text) if int(record["ID"]) == object_id]
    records.sort(key=lambda record: int(record["samplePos"]))
    tracking, render = "scene relative", "undefined"
    result: list[tuple[int, str, str]] = []
    for sample, simultaneous in groupby(records, key=lambda record: int(record["samplePos"])):
        for record in simultaneous:
            tracking = record.get("headTrackMode", tracking)
            render = record.get("binauralRenderMode", render)
        value = (sample, tracking, render)
        if not result or result[-1][1:] != value[1:]:
            result.append(value)
    return result


def verify_roundtrip(before: str, after: str, expected: str, object_id: int) -> None:
    if continuous_commands(before, object_id) != continuous_commands(after, object_id):
        raise ValueError("局部耳机更新改变了位置、增益、ramp 或其他连续字段命令")
    if headphone_timeline(after, object_id) != headphone_timeline(expected, object_id):
        raise ValueError("原生往返改变了耳机策略或其采样时刻")


def serialize(records: list[dict[str, str]]) -> str:
    lines = ["sampleRate: 48000", "events:"]
    for record in records:
        lines.append(f"  - ID: {record['ID']}")
        lines.extend(f"    {key}: {value}" for key, value in record.items() if key != "ID")
    return "\n".join(lines) + "\n"


def fixtures(source: str, object_id: int) -> tuple[str, str]:
    records = read_events(source)
    selected = [record for record in records if int(record["ID"]) == object_id]
    if not selected:
        raise ValueError(f"输入不含对象 {object_id}")
    first = dict(selected[0])
    first.update(samplePos="0", active="true", pos="[-1, 0, 0]", gain="-28",
                 rampLength="0", headTrackMode="scene relative", binauralRenderMode="near")
    ramp = dict(first)
    ramp.update(samplePos="1000", pos="[1, 0, 0]", gain="-16", rampLength="4000")
    other = [record for record in records if int(record["ID"]) != object_id]
    baseline = serialize(other + [first, ramp])
    merged = dict(ramp, headTrackMode="head relative")
    changed = serialize(other + [first, merged,
        {"ID": str(object_id), "samplePos": "3000", "headTrackMode": "scene relative", "binauralRenderMode": "far"},
        {"ID": str(object_id), "samplePos": "5000", "headTrackMode": "head relative"},
    ])
    return baseline, changed


def run(command: list[str], cwd: Path) -> None:
    result = subprocess.run(command, cwd=cwd, capture_output=True, text=True, timeout=60)
    if result.returncode:
        raise RuntimeError(f"工具失败 ({result.returncode}): {command[0]}\n{(result.stdout + result.stderr)[-2500:]}")


def check(input_path: Path, atmos_info: Path, conversion_tool: Path, object_id: int) -> dict:
    input_path = input_path.resolve(strict=True)
    atmos_info = atmos_info.resolve(strict=True)
    conversion_tool = conversion_tool.resolve(strict=True)
    metadata_path = Path(str(input_path) + ".metadata")
    audio_entry = Path(str(input_path) + ".audio")
    audio_path = audio_entry.resolve(strict=True)
    source = metadata_path.read_text()
    if not source.startswith("sampleRate: 48000\n"):
        raise ValueError("本检查器需要 48 kHz canonical DAMF")
    baseline, changed = fixtures(source, object_id)
    manifest = input_path.read_text()
    if "version: 0.5.1" not in manifest or "type: home" not in manifest:
        raise ValueError("输入应为 DAMF 0.5.1/home canonical 包")
    manifest = manifest.replace(metadata_path.name, "master.atmos.metadata").replace(audio_entry.name, "master.atmos.audio")
    normalized: dict[str, str] = {}
    with tempfile.TemporaryDirectory(prefix="macinac4-damf-headphone-") as temporary:
        root = Path(temporary)
        for name, metadata, three_dof in (("baseline", baseline, False), ("changed", changed, False), ("3dof", changed, True)):
            case = root / name
            case.mkdir()
            package_manifest = manifest
            if three_dof:
                package_manifest = manifest.replace("version: 0.5.1", "version: 0.6.0").replace("type: home", "type: 3dof")
            (case / "master.atmos").write_text(package_manifest)
            (case / "master.atmos.metadata").write_text(metadata)
            os.symlink(audio_path, case / "master.atmos.audio")
            run([str(atmos_info), "--input", str(case / "master.atmos"), "--validate", "1"], case)
            if three_dof:
                continue
            output = case / "roundtrip"
            output.mkdir()
            run([str(conversion_tool), "--pm_in", str(case / "master.atmos"), "--output_path", str(output / "master.atmos"), "--output_format", "atmos"], case)
            files = list(output.rglob("*.metadata"))
            if len(files) != 1:
                raise ValueError("官方原生往返没有产生唯一 metadata 文件")
            normalized[name] = files[0].read_text()
        verify_roundtrip(normalized["baseline"], normalized["changed"], changed, object_id)
    return {"home_validated": True, "three_dof_validated": True,
            "native_continuous_commands_unchanged": True,
            "headphone_timeline": headphone_timeline(normalized["changed"], object_id)}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--atmos-info", required=True, type=Path)
    parser.add_argument("--conversion-tool", required=True, type=Path)
    parser.add_argument("--object-id", type=int, default=10)
    args = parser.parse_args()
    try:
        result = check(args.input, args.atmos_info, args.conversion_tool, args.object_id)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"DAMF 耳机局部事件检查失败：{error}\n")
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
