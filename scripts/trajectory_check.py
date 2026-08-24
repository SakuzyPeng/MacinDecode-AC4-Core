#!/usr/bin/env python3
"""比对母版意图与解出的逐对象轨迹。

    ./scripts/trajectory_check.py vectors/probe_axes_single_object [...]

`case.json` 用 DAMF 约定的有符号归一化坐标声明每个对象的分段位置，码流里
则是 OAMD 的量化码值。两者之间的轴映射由 `probe_axes_single_object` 标定
（见实施路线图 M3）：

    x_code = (x + 1) × 31        x = −1 → 0，  x = +1 → 62
    y_code = (1 − y) × 31        y = +1 → 0，  y = −1 → 62      轴向相反
    z_code = z × 15              z = +1 → 15，z 保留符号

本脚本把该映射当作已确立的事实用于回归：轴映射、槽位对应或解析结果任一
发生变化，相关性都会崩掉。

**判据不是逐帧相等。** 该编码链对位置做了一阶平滑：母版的阶跃在码流里表现
为每帧走完剩余距离约八成的指数逼近（`probe_ramp_control` 的 62 → 0 阶跃解出
`11, 2`，0 → 62 解出 `51, 60`）。段长不足时位置根本追不上母版就已换段，因此
用两条判据：

* **形状**：实测轨迹与母版轨迹在每个轴上的 Pearson 相关系数不低于
  `MIN_CORRELATION`，滞后在 `MAX_LAG_FRAMES` 帧内自动对齐并报告。母版该轴
  恒定时改判实测是否同样恒定且落在期望值上。
* **到位**：段长不短于 `SETTLE_FRAMES` 帧时，期望码值必须在该段的发声窗口内
  精确出现——平滑有足够时间收敛，这是轴映射标定的直接证据。

静音期不参与比对：编码器会把不发声的对象泊到原点角，那不是母版声明的位置。

对象与 A-JOC 输出槽位的对应关系尚无规范依据（测试向量策略 9.6 未决），因此
脚本不假设下标相同，而是为每个母版对象在所有槽位中挑相关性最高的那个，并把
匹配到的槽位打印出来。

需要 audio-decode feature，故需先运行 scripts/fetch_specs.py。
"""

from __future__ import annotations

import argparse
import json
import math
import subprocess
import sys
from pathlib import Path

if __package__:
    from .dee_ims import parse_jobs as parse_dee_ims_jobs
    from .dme_ac4 import parse_jobs as parse_dme_ac4_jobs
else:
    from dee_ims import parse_jobs as parse_dee_ims_jobs
    from dme_ac4 import parse_jobs as parse_dme_ac4_jobs

REPO_ROOT = Path(__file__).resolve().parent.parent

# 量化码值的上界：x/y 为 6 比特但规范只用到 62，z 为 4 比特幅度加符号位。
XY_MAX = 62
Z_MAX = 15

MIN_CORRELATION = 0.9
MAX_LAG_FRAMES = 6
SETTLE_FRAMES = 8
# 恒定轴上允许的量化码偏差，容纳一次平滑残留。
CONSTANT_TOLERANCE = 1


class TrajectoryError(Exception):
    """轨迹数据不足或彼此矛盾，不能安全地继续比对。"""


def select_trajectory_media(
    case_dir: Path, case: dict
) -> tuple[list[Path], list[Path], list[str]]:
    """按 case 的编码作业选择 A-JOC 媒体。

    默认 ``encodes`` 与 ``dme_ac4`` 都生成 A-JOC，必须进入轨迹门禁；
    ``dee_ims`` 是 channel-based presentation/metadata 向量，存在时明确跳过。
    encoded 目录里无法由这三类声明解释的 M4A 失败关闭，避免新后端静默绕过。
    """
    encoded = case_dir / "encoded"
    actual = {path.name: path for path in sorted(encoded.glob("*.m4a"))}
    errors: list[str] = []

    encodes = case.get("encodes", [])
    if not isinstance(encodes, list):
        return [], [], ["case.json 的 encodes 必须是数组"]

    ajoc_names: list[str] = []
    for index, bitrate in enumerate(encodes):
        if type(bitrate) is not int or bitrate <= 0:
            errors.append(f"encodes[{index}] 必须是正整数码率")
            continue
        ajoc_names.append(f"master_ac4_{bitrate}K.m4a")

    try:
        ajoc_names.extend(job.output_filename for job in parse_dme_ac4_jobs(case))
        ims_names = {job.output_filename for job in parse_dee_ims_jobs(case)}
    except ValueError as error:
        errors.append(str(error))
        ims_names = set()

    if len(ajoc_names) != len(set(ajoc_names)):
        errors.append("A-JOC 编码作业声明了重复输出名")
    ajoc_set = set(ajoc_names)
    overlap = sorted(ajoc_set.intersection(ims_names))
    if overlap:
        errors.append(f"A-JOC 与 IMS 输出重名：{', '.join(overlap)}")

    for name in sorted(ajoc_set.difference(actual)):
        errors.append(f"缺少已声明的 A-JOC 产物：{name}")
    unknown = sorted(set(actual).difference(ajoc_set).difference(ims_names))
    if unknown:
        errors.append(f"存在未分类的编码产物：{', '.join(unknown)}")
    if not ajoc_set:
        errors.append("case.json 没有声明可做轨迹比对的 A-JOC 编码作业")

    media = [actual[name] for name in ajoc_names if name in actual]
    skipped = [actual[name] for name in sorted(ims_names.intersection(actual))]
    return media, skipped, errors


def quantize(position) -> tuple[int, int, int]:
    """DAMF 有符号坐标 → OAMD 量化码值。"""
    x, y, z = position
    return (
        round((x + 1.0) * (XY_MAX / 2)),
        round((1.0 - y) * (XY_MAX / 2)),
        round(z * Z_MAX),
    )


def run_trace(media: Path) -> dict:
    result = subprocess.run(
        [
            "cargo", "run", "-q",
            "--manifest-path", str(REPO_ROOT / "Cargo.toml"),
            "--features", "macindecode-ac4-cli/audio-decode",
            "--bin", "macinac4", "--",
            "trace", str(media),
        ],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise SystemExit(f"trace 失败：{media}\n{result.stderr.strip()}")
    return json.loads(result.stdout)


def audio_integrity_errors(audio: dict, total_frames: int) -> list[str]:
    """返回会让轨迹门禁失去完整性的 trace 统计错误。"""
    frames = audio["frames"]
    parsed = audio["parsed"]
    substreams = audio["substreams"]
    parsed_substreams = audio["parsed_substreams"]
    failures = audio["failures"]

    errors = []
    if total_frames <= 0:
        errors.append("容器没有音频帧")
    if frames <= 0:
        errors.append("没有 A-JOC substream 帧")
    if frames > total_frames:
        errors.append(f"A-JOC 帧数 {frames} 超过容器样本数 {total_frames}")
    if failures:
        errors.append(f"A-JOC 解析报告 {failures} 个失败帧")
    if parsed != frames:
        errors.append(f"A-JOC 帧未全部解析：{parsed}/{frames}")
    if substreams <= 0:
        errors.append("没有可解析的 A-JOC substream")
    if parsed_substreams != substreams:
        errors.append(
            f"A-JOC substream 未全部解析：{parsed_substreams}/{substreams}"
        )
    return errors


def rebuild_tracks(
    audio: dict, total_frames: int
) -> dict[tuple[int, int], list[tuple[int, int, int] | None]]:
    """把首帧快照与变化点展开成 (substream, object) → 逐帧位置。

    `position_timeline` 只记录变化点，因此重建的前提是它没有被截断；截断时
    缺失的变化点无从补回，只能拒绝比对而不是给出一份不完整的轨迹。
    """
    if audio.get("position_timeline_truncated"):
        raise TrajectoryError("position_timeline 已截断，无法重建完整轨迹")
    if total_frames <= 0:
        raise TrajectoryError("容器没有可用于重建轨迹的帧")

    tracks: dict[tuple[int, int], list[tuple[int, int, int] | None]] = {}
    starts: dict[tuple[int, int], int] = {}
    for snapshot in audio["first_positions"]:
        if snapshot is None:
            continue
        start_frame = snapshot["frame"]
        if not 0 <= start_frame < total_frames:
            raise TrajectoryError(
                f"首位置帧 {start_frame} 超出容器范围 0…{total_frames - 1}"
            )
        substream = snapshot["substream"]
        for item in snapshot["objects"]:
            key = (substream, item["object"])
            if key in tracks:
                raise TrajectoryError(f"{key} 出现重复的首位置快照")
            start = (item["x"], item["y"], item["z"])
            track: list[tuple[int, int, int] | None] = [None] * total_frames
            track[start_frame:] = [start] * (total_frames - start_frame)
            tracks[key] = track
            starts[key] = start_frame

    for change in audio["position_timeline"]:
        key = (change["substream"], change["object"])
        track = tracks.get(key)
        if track is None:
            raise TrajectoryError(f"{key} 有变化点但没有首位置快照")
        frame = change["frame"]
        if frame < starts[key]:
            raise TrajectoryError(f"{key} 的变化帧 {frame} 早于首位置帧 {starts[key]}")
        if frame >= total_frames:
            raise TrajectoryError(
                f"{key} 的变化帧 {frame} 超出容器范围 0…{total_frames - 1}"
            )
        value = (change["x"], change["y"], change["z"])
        for target in range(frame, total_frames):
            track[target] = value
    return tracks


def master_track(case: dict, obj: dict, frame_len: int, frames: int):
    """母版的逐帧期望轨迹与发声掩码。

    帧 `f` 取第 `f × frame_len` 个采样时刻生效的那一段。`burst_samples` 存在
    时段的后半是静音，`silent` 段整段不发声，两者都置为不发声。
    """
    segments = obj.get("segments", [])
    burst = obj.get("burst_samples")
    duration = case["duration_samples"]
    expected: list[tuple[int, int, int] | None] = []
    audible: list[bool] = []
    for frame in range(frames):
        sample = frame * frame_len
        current = None
        for index, segment in enumerate(segments):
            if segment["start_samples"] > sample:
                break
            current = (index, segment)
        if current is None:
            expected.append(None)
            audible.append(False)
            continue
        index, segment = current
        start = segment["start_samples"]
        end = (
            segments[index + 1]["start_samples"]
            if index + 1 < len(segments)
            else duration
        )
        if burst is not None:
            end = min(end, start + burst)
        expected.append(quantize(segment["position"]))
        audible.append(not segment.get("silent") and sample < end)
    return expected, audible


def pearson(left: list[float], right: list[float]) -> float | None:
    count = len(left)
    if count < 2:
        return None
    mean_left = sum(left) / count
    mean_right = sum(right) / count
    cov = sum((a - mean_left) * (b - mean_right) for a, b in zip(left, right))
    var_left = sum((a - mean_left) ** 2 for a in left)
    var_right = sum((b - mean_right) ** 2 for b in right)
    if var_left == 0 or var_right == 0:
        return None
    return cov / math.sqrt(var_left * var_right)


def score_axes(expected, audible, track, lag: int):
    """在给定滞后下逐轴评分。

    返回 `(轴 → 相关系数或 None, 恒定轴上的最大偏差)`；相关系数为 `None`
    表示母版该轴恒定，改由偏差判定。
    """
    pairs = [[], [], []]
    missing = 0
    for frame, value in enumerate(track):
        source = frame - lag
        if source < 0 or source >= len(expected):
            continue
        if not audible[source] or expected[source] is None:
            continue
        if value is None:
            missing += 1
            continue
        for axis in range(3):
            pairs[axis].append((expected[source][axis], value[axis]))

    scores: list[float | None] = []
    worst_constant = 0
    for axis in range(3):
        if not pairs[axis]:
            scores.append(None)
            continue
        want = [float(item[0]) for item in pairs[axis]]
        got = [float(item[1]) for item in pairs[axis]]
        if len(set(want)) == 1:
            scores.append(None)
            worst_constant = max(
                worst_constant, max(abs(a - b) for a, b in pairs[axis])
            )
            continue
        # 走到这里母版该轴一定在变化，`pearson` 只会因实测恒定而无定义——
        # 那是「轨迹丢了这个轴」，必须判为零相关，不能当作无从评价而跳过。
        value = pearson(want, got)
        scores.append(0.0 if value is None else value)
    return scores, worst_constant, sum(len(item) for item in pairs) // 3, missing


def settled_misses(case: dict, obj: dict, track, frame_len: int, frames: int):
    """段长足够时要求期望码值精确出现，返回未出现的段。"""
    segments = obj.get("segments", [])
    burst = obj.get("burst_samples")
    duration = case["duration_samples"]
    misses = []
    checked = 0
    for index, segment in enumerate(segments):
        if segment.get("silent"):
            continue
        start = segment["start_samples"]
        end = (
            segments[index + 1]["start_samples"]
            if index + 1 < len(segments)
            else duration
        )
        if burst is not None:
            end = min(end, start + burst)
        first = math.ceil(start / frame_len)
        last = min(math.ceil(end / frame_len), frames)
        if last - first < SETTLE_FRAMES:
            continue
        checked += 1
        expected = quantize(segment["position"])
        window = track[first:last]
        if expected not in window:
            misses.append(
                {
                    "label": segment.get("label"),
                    "start": start,
                    "expected": expected,
                    "first": first,
                    "last": last,
                    "observed": window[0] if window else None,
                }
            )
    return misses, checked


def evaluate(case: dict, obj: dict, track, frame_len: int, frames: int):
    expected, audible = master_track(case, obj, frame_len, frames)
    best = None
    for lag in range(MAX_LAG_FRAMES + 1):
        scores, worst_constant, samples, missing = score_axes(
            expected, audible, track, lag
        )
        rated = [value for value in scores if value is not None]
        if samples == 0 and missing == 0:
            continue
        # 恒定轴的偏差也计入排序，否则一个把 y 甩飞、只有 x 对得上的槽位
        # 会压过真正的那一个。
        rank = (sum(rated) / len(rated) if rated else 0.0) - worst_constant / (
            XY_MAX + 1
        )
        rank -= missing / (samples + missing)
        if best is None or rank > best["rank"]:
            best = {
                "lag": lag,
                "scores": scores,
                "constant": worst_constant,
                "samples": samples,
                "missing": missing,
                "rank": rank,
            }
    if best is None:
        return None
    best["misses"], best["settled"] = settled_misses(
        case, obj, track, frame_len, frames
    )
    rated = [value for value in best["scores"] if value is not None]
    best["ok"] = (
        not best["misses"]
        and best["missing"] == 0
        and best["constant"] <= CONSTANT_TOLERANCE
        and all(value >= MIN_CORRELATION for value in rated)
    )
    return best


AXIS_NAMES = ("x", "y", "z")


def describe(best: dict) -> str:
    parts = []
    for axis, value in enumerate(best["scores"]):
        if value is None:
            continue
        parts.append(f"{AXIS_NAMES[axis]} r={value:.3f}")
    shape = "、".join(parts) if parts else f"三轴恒定，最大偏差 {best['constant']}"
    settled = f"，{best['settled']} 段到位" if best["settled"] else ""
    missing = f"，缺 {best['missing']} 帧" if best["missing"] else ""
    return f"滞后 {best['lag']} 帧，{shape}{settled}{missing}"


def flatten_validation(section: dict) -> dict:
    """把 v1 分组投影回轨迹算法使用的统计视图。"""
    flat = {}
    for group in (
        "coverage", "references", "timing", "configuration",
        "spectrum", "pcm", "observations",
    ):
        flat.update(section.get(group, {}))
    invariants = section.get("invariants", {}).get("reconstruction")
    if invariants is not None:
        flat["reconstruction_invariants"] = invariants
    fill = section.get("timing", {}).get("fill_bits", {})
    flat["min_fill_bits"] = fill.get("min")
    flat["max_fill_bits"] = fill.get("max")
    scale = section.get("spectrum", {}).get("scale_factor", {})
    flat["scale_factor_min"] = scale.get("min")
    flat["scale_factor_max"] = scale.get("max")
    return flat


def check_case(case_dir: Path) -> bool:
    case = json.loads((case_dir / "case.json").read_text())
    objects = case.get("objects", [])
    name = case["case_id"]
    if not objects:
        print(f"  {name}：母版不含对象，跳过")
        return True

    media, skipped, selection_errors = select_trajectory_media(case_dir, case)
    for item in skipped:
        print(f"  {name}/{item.name}：channel-based IMS，不参与 A-JOC 轨迹门禁")
    if selection_errors:
        for error in selection_errors:
            print(f"  {name}：{error}")
        if not media:
            return False

    ok = not selection_errors
    for item in media:
        trace = run_trace(item)["result"]
        section = trace["validation"].get("ajoc")
        if section is None:
            print(f"  {name}/{item.name}：未启用 audio-decode")
            ok = False
            continue
        audio = flatten_validation(section)

        container = trace["source"]["track"]
        frames = container["sample_count"]
        errors = audio_integrity_errors(audio, frames)
        if errors:
            ok = False
            print(f"  {name}/{item.name}：轨迹输入不完整")
            for error in errors:
                print(f"      {error}")
            continue
        frame_len = container["media_duration"] // frames
        if frame_len <= 0:
            print(f"  {name}/{item.name}：容器帧长无效")
            ok = False
            continue
        try:
            tracks = rebuild_tracks(audio, frames)
        except TrajectoryError as error:
            print(f"  {name}/{item.name}：{error}")
            ok = False
            continue
        if not tracks:
            print(f"  {name}/{item.name}：没有可重建的位置轨迹")
            ok = False
            continue

        for index, obj in enumerate(objects):
            label = obj.get("name") or f"对象 {index}"
            best = None
            for key, track in sorted(tracks.items()):
                candidate = evaluate(case, obj, track, frame_len, frames)
                if candidate is None:
                    continue
                if best is None or candidate["rank"] > best[1]["rank"]:
                    best = (key, candidate)
            if best is None:
                print(f"  {name}/{item.name}：{label} 没有可比对的帧")
                ok = False
                continue
            (substream, slot), result = best
            where = f"substream {substream} / 对象 {slot}"
            if result["ok"]:
                print(f"  {name}/{item.name}：{label} 对上 {where}（{describe(result)}）")
                continue
            ok = False
            print(f"  {name}/{item.name}：{label} 与 {where} 不符（{describe(result)}）")
            for miss in result["misses"][:3]:
                tag = f"「{miss['label']}」" if miss.get("label") else ""
                print(
                    f"      段 {tag}起于 {miss['start']} 采样，帧 "
                    f"{miss['first']}…{miss['last']} 内未出现 {miss['expected']}"
                )
    return ok


def main() -> int:
    parser = argparse.ArgumentParser(description="比对母版意图与解出的逐对象轨迹")
    parser.add_argument("cases", nargs="+", type=Path, help="vectors/<case_id> 目录")
    args = parser.parse_args()

    failed = False
    for case_dir in args.cases:
        if not (case_dir / "case.json").is_file():
            print(f"找不到 case.json：{case_dir}", file=sys.stderr)
            failed = True
            continue
        if not check_case(case_dir):
            failed = True

    if failed:
        print("轨迹比对未通过", file=sys.stderr)
        return 1
    print("轨迹比对通过")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
