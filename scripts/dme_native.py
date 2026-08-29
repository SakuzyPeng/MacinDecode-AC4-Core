#!/usr/bin/env python3
"""校验 DME channel-based / native IMS 作业并生成确定性的 speaker WAVE。

``case.json`` 的可选 ``dme_channel`` 与 ``dme_ims`` 数组只保存可复现的编码
参数；外部工具路径继续由未纳入版本控制的 ``.env.local`` 提供。speaker WAVE
从纯 bed 案例的信号配方重新生成，不从 Atmos 对象母版做未经定义的下混。
"""

from __future__ import annotations

import argparse
import json
import sys
import wave
from dataclasses import dataclass
from pathlib import Path

if __package__:
    from . import gen_damf
else:
    import gen_damf


CHANNEL_LAYOUTS = {
    "stereo": ("L", "R"),
    "5.1": ("L", "R", "C", "LFE", "Lss", "Rss"),
    "5.1.4": (
        "L",
        "R",
        "C",
        "LFE",
        "Lss",
        "Rss",
        "Ltf",
        "Rtf",
        "Ltr",
        "Rtr",
    ),
}
CHANNEL_BITRATES = {
    "stereo": frozenset((48, 64, 96, 128, 144, 192, 256, 288, 320, 384, 448, 512, 768)),
    "5.1": frozenset((96, 128, 144, 192, 256, 288, 320, 384, 448, 512, 768)),
    "5.1.4": frozenset((192, 256, 288, 320, 384, 448, 512, 768)),
}
CHANNEL_KEYS = frozenset(("layout", "bitrate"))

IMS_BITRATES = frozenset((64, 96, 128, 144, 256, 320))
IMS_INPUTS = frozenset(("wav_5_1", "damf"))
IMS_MODES = frozenset(("general", "music"))
IMS_KEYS = frozenset(("input", "mode", "bitrate"))

SAMPLE_MAX = (1 << 23) - 1
IMS_ENCODER_DELAY_SAMPLES_24_FPS = 2000


@dataclass(frozen=True)
class DmeChannelJob:
    """一个 DME channel-based AC-4 编码作业。"""

    layout: str
    bitrate: int

    @property
    def output_filename(self) -> str:
        layout = self.layout.replace(".", "_")
        return f"master_ac4_dme_channel_{layout}_{self.bitrate}K.m4a"

    @property
    def input_format(self) -> str:
        return "cbi_wav" if self.layout == "5.1.4" else "wav"

    def provenance(self) -> dict[str, object]:
        return {
            "bitrate_kbps": self.bitrate,
            "channel_layout": self.layout,
            "input_format": self.input_format,
            "input_transform": "case_bed_signal_recipe_smpte_wave",
            "drc_profile": "film_light",
            "iframe_interval": "1sec",
            "loudness_management": "measure_only",
            "output": f"encoded/{self.output_filename}",
        }


@dataclass(frozen=True)
class DmeImsJob:
    """一个 DME 原生 immersive stereo 编码作业。"""

    input: str
    mode: str
    bitrate: int

    @property
    def output_filename(self) -> str:
        input_label = "wav" if self.input == "wav_5_1" else "damf"
        return (
            f"master_ac4_dme_ims_{self.mode}_{input_label}_"
            f"{self.bitrate}K.m4a"
        )

    @property
    def input_format(self) -> str:
        return "wav" if self.input == "wav_5_1" else "atmos_mezz"

    @property
    def drc_profile(self) -> str:
        return "music_light" if self.mode == "music" else "film_light"

    @property
    def target_fps(self) -> str:
        return "native" if self.mode == "music" else "24"

    @property
    def loudness_management(self) -> str:
        if self.mode == "music":
            return "measure_only:preset=manual:dialogue_intelligence=0"
        return "measure_only"

    def provenance(self) -> dict[str, object]:
        transform = (
            "case_bed_signal_recipe_smpte_wave"
            if self.input == "wav_5_1"
            else "canonical_damf_0.5.1_home"
        )
        return {
            "bitrate_kbps": self.bitrate,
            "encoder_mode": self.mode,
            "input": self.input,
            "input_format": self.input_format,
            "input_transform": transform,
            "drc_profile": self.drc_profile,
            "iframe_interval": 24,
            "target_fps": self.target_fps,
            "loudness_management": self.loudness_management,
            "output": f"encoded/{self.output_filename}",
        }


def _require_pure_bed(case: dict[str, object], label: str) -> None:
    objects = case.get("objects")
    if not isinstance(objects, list):
        raise ValueError("objects 必须是数组")
    if objects:
        raise ValueError(f"{label} 的 speaker WAVE 输入只允许 objects 为空的纯 bed 案例")


def parse_channel_jobs(case: dict[str, object]) -> list[DmeChannelJob]:
    raw = case.get("dme_channel", [])
    if not isinstance(raw, list):
        raise ValueError("dme_channel 必须是数组")
    if raw and case.get("sample_rate") != 48000:
        raise ValueError("DME channel-based 作业要求 sample_rate 为 48000")
    if raw:
        _require_pure_bed(case, "DME channel-based 作业")

    jobs = []
    outputs: set[str] = set()
    for index, value in enumerate(raw):
        label = f"dme_channel[{index}]"
        if not isinstance(value, dict):
            raise ValueError(f"{label} 必须是对象")
        unknown = sorted(set(value).difference(CHANNEL_KEYS))
        if unknown:
            raise ValueError(f"{label} 含未知字段：{', '.join(unknown)}")
        layout = value.get("layout")
        if not isinstance(layout, str) or layout not in CHANNEL_LAYOUTS:
            raise ValueError(f"{label}.layout 必须是 stereo、5.1 或 5.1.4")
        bitrate = value.get("bitrate")
        allowed = CHANNEL_BITRATES[layout]
        if type(bitrate) is not int or bitrate not in allowed:
            choices = ", ".join(str(item) for item in sorted(allowed))
            raise ValueError(
                f"{label}.bitrate 对 {layout} 必须是以下整数之一：{choices}"
            )
        job = DmeChannelJob(layout, bitrate)
        if job.output_filename in outputs:
            raise ValueError(f"{label} 与前一作业输出重名：{job.output_filename}")
        outputs.add(job.output_filename)
        jobs.append(job)
    return jobs


def parse_ims_jobs(case: dict[str, object]) -> list[DmeImsJob]:
    raw = case.get("dme_ims", [])
    if not isinstance(raw, list):
        raise ValueError("dme_ims 必须是数组")
    if raw and case.get("sample_rate") != 48000:
        raise ValueError("DME native IMS 作业要求 sample_rate 为 48000")
    if raw and case.get("frame_rate") != "24":
        raise ValueError("DME native IMS 作业当前只冻结 frame_rate 24")

    jobs = []
    outputs: set[str] = set()
    for index, value in enumerate(raw):
        label = f"dme_ims[{index}]"
        if not isinstance(value, dict):
            raise ValueError(f"{label} 必须是对象")
        unknown = sorted(set(value).difference(IMS_KEYS))
        if unknown:
            raise ValueError(f"{label} 含未知字段：{', '.join(unknown)}")
        input_kind = value.get("input")
        if not isinstance(input_kind, str) or input_kind not in IMS_INPUTS:
            raise ValueError(f"{label}.input 必须是 wav_5_1 或 damf")
        mode = value.get("mode")
        if not isinstance(mode, str) or mode not in IMS_MODES:
            raise ValueError(f"{label}.mode 必须是 general 或 music")
        bitrate = value.get("bitrate")
        if type(bitrate) is not int or bitrate not in IMS_BITRATES:
            choices = ", ".join(str(item) for item in sorted(IMS_BITRATES))
            raise ValueError(f"{label}.bitrate 必须是以下整数之一：{choices}")
        if input_kind == "wav_5_1":
            _require_pure_bed(case, label)
        job = DmeImsJob(input_kind, mode, bitrate)
        if job.output_filename in outputs:
            raise ValueError(f"{label} 与前一作业输出重名：{job.output_filename}")
        outputs.add(job.output_filename)
        jobs.append(job)
    return jobs


def load_case(case_path: Path) -> dict[str, object]:
    try:
        case = json.loads(case_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"无法读取 {case_path}：{error}") from error
    if not isinstance(case, dict):
        raise ValueError("case.json 顶层必须是对象")
    return case


def _pcm24(value: float) -> bytes:
    clamped = max(-1.0, min(1.0, value))
    quantized = int(round(clamped * SAMPLE_MAX))
    quantized = max(-SAMPLE_MAX, min(SAMPLE_MAX, quantized))
    return (quantized & 0xFF_FFFF).to_bytes(3, "little")


def prepare_wave(case: dict[str, object], output: Path, layout: str) -> None:
    """从纯 bed 的信号配方生成 DME 所需的 SMPTE 顺序 24-bit PCM WAVE。"""
    if layout not in CHANNEL_LAYOUTS:
        raise ValueError(f"不支持的 speaker WAVE layout：{layout}")
    _require_pure_bed(case, "speaker WAVE")
    sample_rate = case.get("sample_rate")
    duration = case.get("duration_samples")
    if type(sample_rate) is not int or sample_rate != 48000:
        raise ValueError("speaker WAVE 要求 sample_rate 为 48000")
    if type(duration) is not int or duration <= 0:
        raise ValueError("duration_samples 必须是正整数")
    bed = case.get("bed")
    if not isinstance(bed, dict) or not isinstance(bed.get("signal"), dict):
        raise ValueError("bed.signal 必须是对象")

    generated_bed = {
        "channels": list(CHANNEL_LAYOUTS[layout]),
        "signal": bed["signal"],
    }
    gen_damf.SAMPLE_RATE = sample_rate
    channels = gen_damf.build_bed_signals(generated_bed, duration)

    output.parent.mkdir(parents=True, exist_ok=True)
    try:
        with wave.open(str(output), "wb") as writer:
            writer.setnchannels(len(channels))
            writer.setsampwidth(3)
            writer.setframerate(sample_rate)
            payload = bytearray()
            for frame in range(duration):
                for channel in channels:
                    payload.extend(_pcm24(channel[frame]))
            writer.writeframes(payload)
    except (OSError, wave.Error) as error:
        raise ValueError(f"无法写入 speaker WAVE {output}：{error}") from error


def ims_track_options(expected_duration: int, mode: str) -> str:
    """冻结 DME native IMS 的 AC-4 encoder delay 与可见节目时长。"""
    if type(expected_duration) is not int or expected_duration <= 0:
        raise ValueError("expected_duration 必须是正整数")
    if mode not in IMS_MODES:
        raise ValueError("mode 必须是 general 或 music")
    offset = -IMS_ENCODER_DELAY_SAMPLES_24_FPS if mode == "general" else 0
    return f"offset={offset}:duration={expected_duration}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    for name, help_text in (
        ("list-channel", "以 TSV 输出 case 中的 DME channel-based 作业"),
        ("list-ims", "以 TSV 输出 case 中的 DME native IMS 作业"),
    ):
        subparser = commands.add_parser(name, help=help_text)
        subparser.add_argument("case", type=Path)

    wave_parser = commands.add_parser("prepare-wave", help="生成隔离的 speaker WAVE")
    wave_parser.add_argument("case", type=Path)
    wave_parser.add_argument("output", type=Path)
    wave_parser.add_argument("--layout", choices=sorted(CHANNEL_LAYOUTS), required=True)

    timing_parser = commands.add_parser(
        "ims-track-options", help="输出 DME muxer 的 native IMS track-options"
    )
    timing_parser.add_argument("--expected-duration", type=int, required=True)
    timing_parser.add_argument("--mode", choices=sorted(IMS_MODES), required=True)

    args = parser.parse_args()
    try:
        if args.command == "list-channel":
            for job in parse_channel_jobs(load_case(args.case)):
                print(
                    job.layout,
                    job.bitrate,
                    job.input_format,
                    job.output_filename,
                    sep="\t",
                )
        elif args.command == "list-ims":
            for job in parse_ims_jobs(load_case(args.case)):
                print(
                    job.input,
                    job.mode,
                    job.bitrate,
                    job.input_format,
                    job.drc_profile,
                    job.target_fps,
                    job.loudness_management,
                    job.output_filename,
                    sep="\t",
                )
        elif args.command == "prepare-wave":
            prepare_wave(load_case(args.case), args.output, args.layout)
        else:
            print(ims_track_options(args.expected_duration, args.mode))
    except ValueError as error:
        print(f"错误：{error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
