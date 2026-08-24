#!/usr/bin/env python3
"""由 case.json 生成 DAMF 三件套。

    ./scripts/gen_damf.py vectors/<case_id>/case.json

输出到同目录的 source/：master.atmos、master.atmos.metadata、master.atmos.audio。

案例描述是测试意图的唯一来源；生成结果不允许手工修改后继续沿用同一 case_id。
DAMF 音频容器为 CAF：容器字段大端，样本为交织 24-bit 有符号小端。
"""

import argparse
import json
import math
import struct
import sys
from pathlib import Path

CAF_LPCM_LITTLE_ENDIAN = 2
BYTES_PER_SAMPLE = 3
SAMPLE_MAX = (1 << 23) - 1
HEAD_TRACK_MODES = frozenset(("scene relative", "head relative"))


def db_to_linear(db: float) -> float:
    return 10.0 ** (db / 20.0)


def build_object_signal(spec: dict, total: int) -> list[float]:
    """按 segments 生成对象通道的浮点样本。"""
    signal = [0.0] * total
    kind = spec["signal"]["kind"]
    if kind == "silence":
        return signal
    if kind != "sine":
        raise ValueError(f"暂不支持的信号类型：{kind}")

    amplitude = db_to_linear(spec["signal"]["level_dbfs"])
    frequency = spec["signal"]["frequency_hz"]
    fade = int(spec["signal"].get("fade_samples", 0))
    # burst_samples 省略时对象在整个时长内持续发声。位置在帧内多次更新的案例
    # 需要这种形态：猝发会让大部分帧没有信号，无法观察更新与音频的对齐。
    burst = spec.get("burst_samples")
    if burst is None:
        return continuous_signal(amplitude, frequency, fade, total)
    burst = int(burst)

    for segment in spec["segments"]:
        # 静音段仍然产生位置事件，只是不发声，用于观察槽位的占用与释放
        if segment.get("silent"):
            continue
        start = int(segment["start_samples"])
        for n in range(burst):
            index = start + n
            if index >= total:
                break
            # 每段独立起振，避免跨段相位跳变
            value = amplitude * math.sin(2.0 * math.pi * frequency * n / SAMPLE_RATE)
            if fade:
                if n < fade:
                    value *= n / fade
                elif n >= burst - fade:
                    value *= (burst - 1 - n) / fade
            signal[index] = value
    return signal


def continuous_signal(
    amplitude: float, frequency: float, fade: int, total: int
) -> list[float]:
    """整段持续的正弦，仅在首尾淡入淡出。"""
    signal = [0.0] * total
    for n in range(total):
        value = amplitude * math.sin(2.0 * math.pi * frequency * n / SAMPLE_RATE)
        if fade:
            if n < fade:
                value *= n / fade
            elif n >= total - fade:
                value *= (total - 1 - n) / fade
        signal[n] = value
    return signal


def build_bed_signals(bed: dict, total: int) -> list[list[float]]:
    """按 bed.signal 生成各声道样本。

    `silence` 为全零。`per_channel_sine` 给每个声道一个可区分的频率，用于识别
    声道顺序与串扰；LFE 单独取低频，避免落在其他声道的频段上。
    """
    kind = bed["signal"]["kind"]
    names = bed["channels"]
    if kind == "silence":
        return [[0.0] * total for _ in names]
    if kind != "per_channel_sine":
        raise SystemExit(f"暂不支持的 bed 信号类型：{kind}")

    spec = bed["signal"]
    amplitude = db_to_linear(spec["level_dbfs"])
    base = float(spec.get("base_hz", 200.0))
    step = float(spec.get("step_hz", 150.0))
    lfe = float(spec.get("lfe_hz", 40.0))
    fade = int(spec.get("fade_samples", 0))

    out = []
    for index, name in enumerate(names):
        frequency = lfe if name == "LFE" else base + step * index
        out.append(continuous_signal(amplitude, frequency, fade, total))
    return out


def write_caf(path: Path, channels: list[list[float]], sample_rate: int) -> None:
    channel_count = len(channels)
    frames = len(channels[0])
    bytes_per_packet = channel_count * BYTES_PER_SAMPLE

    with path.open("wb") as f:
        f.write(b"caff")
        f.write(struct.pack(">HH", 1, 0))

        f.write(b"desc")
        f.write(struct.pack(">q", 32))
        f.write(struct.pack(">d", float(sample_rate)))
        f.write(b"lpcm")
        f.write(struct.pack(">IIIII", CAF_LPCM_LITTLE_ENDIAN, bytes_per_packet, 1,
                            channel_count, BYTES_PER_SAMPLE * 8))

        payload = bytearray()
        for frame in range(frames):
            for channel in channels:
                value = channel[frame]
                # 对称截断，避免负半轴多出一个码值
                clamped = max(-1.0, min(1.0, value))
                quantized = int(round(clamped * SAMPLE_MAX))
                quantized = max(-SAMPLE_MAX, min(SAMPLE_MAX, quantized))
                payload += (quantized & 0xFFFFFF).to_bytes(3, "little")

        f.write(b"data")
        f.write(struct.pack(">q", len(payload) + 4))
        f.write(struct.pack(">I", 0))  # mEditCount
        f.write(payload)


def build_manifest(case: dict, stem: str) -> str:
    bed_channels = case["bed"]["channels"]
    lines = [
        "version: 0.5.1",
        "presentations:",
        "  - type: home",
        "    simplified: false",
        f"    metadata: {stem}.atmos.metadata",
        f"    audio: {stem}.atmos.audio",
        f"    offset: {case.get('offset_frames', 0)}",
        f"    fps: {case['frame_rate']}",
        "    scBedConfiguration: [3]",
        "    creationTool: MacinDecode-AC4-Core probe generator",
        "    creationToolVersion: 0.1.0",
        "    downmixType_5to2: LoRo_Stereo",
        "    51-to-20_LsRs90degPhaseShift: false",
        "    warpMode: LoRo",
        "    trimMode:",
        "      SomeSurroundsNoHeights:",
        "        {}",
        "      SomeSurroundsSomeHeights:",
        "        {}",
        "      SomeSurroundsManyHeights:",
        "        {}",
        "      ManySurroundsNoHeights:",
        "        {}",
        "    bedInstances:",
        "      - description: Master",
        "        channels:",
    ]
    for index, channel in enumerate(bed_channels):
        lines.append(f"          - channel: {channel}")
        lines.append(f"            ID: {index}")

    lines.append("    objects:")
    for obj in case["objects"]:
        lines.append(f"      - description: {obj['name']}")
        lines.append(f"        ID: {obj['source_id']}")
    return "\n".join(lines) + "\n"


def object_head_track_mode(fields: dict) -> str:
    """读取对象固定的 DAMF headTrackMode，缺省保持既有 scene-relative 行为。"""

    mode = fields.get("headTrackMode", "scene relative")
    if not isinstance(mode, str) or mode not in HEAD_TRACK_MODES:
        choices = "、".join(sorted(HEAD_TRACK_MODES))
        raise ValueError(f"headTrackMode 必须是 {choices}")
    return mode


def build_metadata(case: dict) -> str:
    lines = ["sampleRate: {}".format(case["sample_rate"]), "events:"]

    for index in range(len(case["bed"]["channels"])):
        lines += [
            f"  - ID: {index}",
            "    samplePos: 0",
            "    active: true",
            "    importance: 1",
            "    gain: 0",
            "    rampLength: 0",
            "    trimBypass: false",
            "    headTrackMode: scene relative",
            "    binauralRenderMode: off" if case["bed"]["channels"][index] == "LFE"
            else "    binauralRenderMode: undefined",
        ]

    for obj in case["objects"]:
        fields = obj["static_fields"]
        head_track_mode = object_head_track_mode(fields)
        for segment in obj["segments"]:
            x, y, z = segment["position"]
            lines += [
                f"  - ID: {obj['source_id']}",
                f"    samplePos: {int(segment['start_samples'])}",
                "    active: true",
                f"    pos: [{x:g}, {y:g}, {z:g}]",
                f"    snap: {str(fields['snap']).lower()}",
                f"    elevation: {str(fields['elevation']).lower()}",
                f"    zones: {fields['zones']}",
                f"    size: {fields['size']:g}",
                f"    decorr: {fields['decorr']:g}",
                f"    importance: {fields['importance']}",
                f"    gain: {fields['gain']}",
                f"    rampLength: {int(segment.get('ramp_samples', 0))}",
                "    trimBypass: false",
                "    dialog: -1",
                "    music: -1",
                f"    screenFactor: {fields['screenFactor']:g}",
                f"    depthFactor: {fields['depthFactor']:g}",
                f"    headTrackMode: {head_track_mode}",
                "    binauralRenderMode: undefined",
            ]
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("case", type=Path, help="case.json 路径")
    ap.add_argument("--stem", default="master", help="输出文件名主干（默认 master）")
    args = ap.parse_args()

    case = json.loads(args.case.read_text(encoding="utf-8"))

    global SAMPLE_RATE
    SAMPLE_RATE = case["sample_rate"]
    total = int(case["duration_samples"])

    bed_channels = case["bed"]["channels"]
    channels = build_bed_signals(case["bed"], total)
    for obj in case["objects"]:
        channels.append(build_object_signal(obj, total))

    out_dir = args.case.parent / "source"
    out_dir.mkdir(parents=True, exist_ok=True)

    manifest_path = out_dir / f"{args.stem}.atmos"
    metadata_path = out_dir / f"{args.stem}.atmos.metadata"
    audio_path = out_dir / f"{args.stem}.atmos.audio"

    manifest_path.write_text(build_manifest(case, args.stem), encoding="utf-8")
    metadata_path.write_text(build_metadata(case), encoding="utf-8")
    write_caf(audio_path, channels, SAMPLE_RATE)

    print(f"case_id      : {case['case_id']}")
    print(f"channels     : {len(bed_channels)} bed + {len(case['objects'])} object")
    print(f"duration     : {total} samples ({total / SAMPLE_RATE:.3f} s)")
    for path in (manifest_path, metadata_path, audio_path):
        print(f"  {path.name:28} {path.stat().st_size:>12,} B")
    return 0


if __name__ == "__main__":
    sys.exit(main())
