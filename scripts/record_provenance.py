#!/usr/bin/env python3
"""为一个测试向量案例生成 provenance.json 与 hashes.sha256。

    ./scripts/record_provenance.py vectors/<case_id>

记录内容对应测试向量策略第 4 节：生成器版本、外部工具指纹、编码参数、
全部产物哈希与宿主信息。产物文件本身不进入版本控制，此清单进入，
使任何一个向量都能追溯到产生它的确切工具组合。

哈希覆盖 source/、encoded/；标准作业与 general DME A-JOC 作业还覆盖 normalized/，
纯 3DoF DME 作业直接使用 source/。若当前作业所需的产物目录或声明的编码输出缺失，
脚本报错而不是静默跳过：不完整的 provenance 比没有更危险。
"""

import argparse
import hashlib
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from dee_ims import parse_jobs as parse_dee_jobs
from dme_ac4 import parse_jobs as parse_dme_jobs
from dme_native import parse_channel_jobs as parse_dme_channel_jobs
from dme_native import parse_ims_jobs as parse_dme_ims_jobs

REPO_ROOT = Path(__file__).resolve().parent.parent


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def mp4_payload_sha256(path: Path) -> str | None:
    """提取 MP4 `mdat` 的内容哈希。

    容器头部的 `mvhd`/`tkhd`/`mdhd` 含创建与修改时间，每次封装都会变化，
    因此完整文件哈希无法用作回归基准。媒体负载在编码确定的前提下逐字节
    稳定，实测同一案例两次生成仅相差 12 个时间戳字节，负载完全一致。
    """
    data = path.read_bytes()
    offset = 0
    while offset + 8 <= len(data):
        size = int.from_bytes(data[offset:offset + 4], "big")
        box_type = data[offset + 4:offset + 8]
        header = 8
        if size == 1:  # 64 位扩展尺寸
            size = int.from_bytes(data[offset + 8:offset + 16], "big")
            header = 16
        elif size == 0:  # 延伸至文件末尾
            size = len(data) - offset
        if size < header:
            return None
        if box_type == b"mdat":
            return hashlib.sha256(data[offset + header:offset + size]).hexdigest()
        offset += size
    return None


def git_info() -> dict:
    def run(*args: str) -> str | None:
        result = subprocess.run(["git", "-C", str(REPO_ROOT), *args],
                                capture_output=True, text=True)
        return result.stdout.strip() if result.returncode == 0 else None

    commit = run("rev-parse", "HEAD")
    status = run("status", "--porcelain")
    if status is None:
        return {"commit": commit, "dirty": None}

    # dirty 表示“产生该向量的代码有未提交改动”。溯源文件自身在生成过程中
    # 必然变化，把它们计入会让先后生成的案例得到不同结果，因此排除。
    tracked = [line for line in status.splitlines()
               if not line.endswith(("provenance.json", "hashes.sha256"))]
    return {"commit": commit, "dirty": bool(tracked)}


def collect_tools(profile: str) -> dict:
    """复用 check_tools.sh 的指纹输出，避免两处各记一套。"""
    script = REPO_ROOT / "scripts" / "check_tools.sh"
    result = subprocess.run(
        [str(script), "--json", "--profile", profile],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"check_tools.sh 失败：\n{result.stdout}{result.stderr}")
    return json.loads(result.stdout)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("case_dir", type=Path, help="vectors/<case_id> 目录")
    args = ap.parse_args()

    case_dir = args.case_dir.resolve()
    case_path = case_dir / "case.json"
    if not case_path.exists():
        raise SystemExit(f"缺少 {case_path}")

    case = json.loads(case_path.read_text(encoding="utf-8"))
    try:
        dee_jobs = parse_dee_jobs(case)
    except ValueError as error:
        raise SystemExit(f"case.json 的 DEE IMS 作业无效：{error}") from error
    try:
        dme_jobs = parse_dme_jobs(case)
    except ValueError as error:
        raise SystemExit(f"case.json 的 DME A-JOC 作业无效：{error}") from error
    try:
        dme_channel_jobs = parse_dme_channel_jobs(case)
        dme_ims_jobs = parse_dme_ims_jobs(case)
    except ValueError as error:
        raise SystemExit(f"case.json 的 DME native 作业无效：{error}") from error
    standard_bitrates = case.get("encodes", [])
    if not isinstance(standard_bitrates, list):
        raise SystemExit("case.json 的 encodes 必须是数组")
    dme_needs_normalized = any(job.mode == "general" for job in dme_jobs)
    encode_jobs: list[dict[str, object]] = [
        {
            "backend": "default",
            "bitrate_kbps": bitrate,
            "output": f"encoded/master_ac4_{bitrate}K.m4a",
        }
        for bitrate in standard_bitrates
    ] + [
        {"backend": "dme_ac4", **job.provenance()}
        for job in dme_jobs
    ] + [
        {"backend": "dme_channel", **job.provenance()}
        for job in dme_channel_jobs
    ] + [
        {
            "backend": "dme_ims",
            **job.provenance(),
        }
        for job in dme_ims_jobs
    ] + [
        {"backend": "dee_ims", **job.provenance()}
        for job in dee_jobs
    ]
    tool_profiles = []
    if standard_bitrates:
        tool_profiles.append("default")
    if dme_jobs:
        tool_profiles.append("dme_ac4")
    if dme_channel_jobs or dme_ims_jobs:
        tool_profiles.append("dme_native")
    if dee_jobs:
        tool_profiles.append("dee_ims")
    tool_profile = "+".join(tool_profiles) if tool_profiles else "default"

    artifacts: dict[str, dict] = {}
    hash_lines: list[str] = []
    missing: list[str] = []

    for name in ("case.json",):
        path = case_dir / name
        artifacts[name] = {"sha256": sha256_file(path), "bytes": path.stat().st_size}
        hash_lines.append(f"{artifacts[name]['sha256']}  {name}")

    artifact_dirs = ["source", "encoded"]
    if standard_bitrates or dme_needs_normalized:
        artifact_dirs.insert(1, "normalized")
    for directory in artifact_dirs:
        target = case_dir / directory
        if not target.is_dir():
            missing.append(directory)
            continue
        for path in sorted(target.rglob("*")):
            if not path.is_file() or path.name == ".DS_Store":
                continue
            relative = path.relative_to(case_dir).as_posix()
            digest = sha256_file(path)
            entry = {"sha256": digest, "bytes": path.stat().st_size}
            if path.suffix.lower() in (".m4a", ".mp4"):
                payload = mp4_payload_sha256(path)
                if payload:
                    entry["payload_sha256"] = payload
            artifacts[relative] = entry
            hash_lines.append(f"{digest}  {relative}")

    if missing:
        raise SystemExit(f"缺少产物目录：{', '.join(missing)}；请先完整生成该案例")
    expected_outputs = [str(job["output"]) for job in encode_jobs]
    missing_outputs = [
        output
        for output in expected_outputs
        if output not in artifacts or artifacts[output]["bytes"] == 0
    ]
    if missing_outputs:
        raise SystemExit(
            "缺少声明的编码产物："
            + ", ".join(missing_outputs)
            + "；请先完整生成该案例"
        )

    tools = collect_tools(tool_profile)

    generator = REPO_ROOT / "scripts" / "gen_damf.py"
    provenance = {
        "case_id": case["case_id"],
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "generator": {
            "script": "scripts/gen_damf.py",
            "sha256": sha256_file(generator),
            "repo": git_info(),
        },
        "encode": {
            "codec": "ac4",
            "bitrates_kbps": standard_bitrates,
            "jobs": encode_jobs,
            "sample_rate": case["sample_rate"],
            "frame_rate": case["frame_rate"],
            "duration_samples": case["duration_samples"],
            # 工具与后端指纹用于追溯，不把它们扩成测试矩阵的一维。
            "encoder_behavior_scope": (
                "backend_and_job_parameters"
                if dme_jobs or dme_channel_jobs or dme_ims_jobs
                else "same_bitrate_behavior_bucket"
            ),
        },
        "tools": tools.get("tools", {}),
        "host": tools.get("host", {}),
        "artifacts": artifacts,
    }

    (case_dir / "provenance.json").write_text(
        json.dumps(provenance, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    (case_dir / "hashes.sha256").write_text("\n".join(hash_lines) + "\n", encoding="utf-8")

    print(f"case_id   : {case['case_id']}")
    print(f"artifacts : {len(artifacts)} 个文件")
    total = sum(item["bytes"] for item in artifacts.values())
    print(f"total     : {total:,} B")
    commit = provenance["generator"]["repo"]["commit"]
    print(f"repo      : {commit[:12] if commit else '<非 git 仓库>'}"
          f"{' (dirty)' if provenance['generator']['repo']['dirty'] else ''}")
    print(f"written   : provenance.json, hashes.sha256")
    return 0


if __name__ == "__main__":
    sys.exit(main())
