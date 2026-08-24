#!/usr/bin/env python3
"""把两个已编码的 AC-4 轨道拼接成一条 Annex G 裸流。

用途是构造 `sequence_counter` 来源变化与 reset 状态机的真实数据向量。
`TS103190-1:v1.4.1:4.3.3.2.2` 规定该字段用于检测流来源的非受控变化，拼接
正是它被设计来检测的操作，因此这不是伪造数据，而是制造该字段的目标场景。

拼接点默认落在**非随机访问帧**上，使解码器在切换后必须等待下一个完整随机
访问点才能重置——这条路径在单一来源的向量里永远不会出现。

    ./scripts/make_splice.py A.m4a B.m4a -o out.ac4

输出为 `Annex G` 裸流：sync_word 0xAC40 + frame_size(16) + raw_ac4_frame。
本脚本不改写任何一帧的内容，只重新定界与排序。
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from pathlib import Path

SYNC_WORD_PLAIN = 0xAC40
FRAME_SIZE_ESCAPE = 0xFFFF


def iter_boxes(buf: bytes, start: int, end: int):
    pos = start
    while pos + 8 <= end:
        size, kind = struct.unpack_from(">I4s", buf, pos)
        if size == 1:
            size = struct.unpack_from(">Q", buf, pos + 8)[0]
            header = 16
        elif size == 0:
            size = end - pos
            header = 8
        else:
            header = 8
        if size < header:
            raise SystemExit(f"偏移 {pos} 处 box 尺寸 {size} 小于头部 {header}")
        yield kind.decode("latin1"), pos + header, pos + size
        pos += size


def find_box(buf: bytes, name: str, start: int, end: int):
    for kind, body_start, body_end in iter_boxes(buf, start, end):
        if kind == name:
            return body_start, body_end
    return None


def find_path(buf: bytes, names, start: int, end: int):
    cursor = (start, end)
    for name in names:
        found = find_box(buf, name, *cursor)
        if found is None:
            return None
        cursor = found
    return cursor


def read_sample_table(data: bytes, stbl: tuple[int, int]) -> list[tuple[int, int]]:
    """返回 [(文件偏移, 字节数)]，顺序即解码顺序。"""
    stsz = find_box(data, "stsz", *stbl)
    if stsz is None:
        raise SystemExit("缺少 stsz")
    uniform, count = struct.unpack_from(">II", data, stsz[0] + 4)
    if uniform:
        sizes = [uniform] * count
    else:
        sizes = list(struct.unpack_from(f">{count}I", data, stsz[0] + 12))

    stco = find_box(data, "stco", *stbl)
    co64 = find_box(data, "co64", *stbl)
    if stco is not None:
        base, wide = stco, False
    elif co64 is not None:
        base, wide = co64, True
    else:
        raise SystemExit("缺少 stco/co64")
    chunk_count = struct.unpack_from(">I", data, base[0] + 4)[0]
    fmt = f">{chunk_count}{'Q' if wide else 'I'}"
    chunk_offsets = list(struct.unpack_from(fmt, data, base[0] + 8))

    stsc = find_box(data, "stsc", *stbl)
    if stsc is None:
        raise SystemExit("缺少 stsc")
    entry_count = struct.unpack_from(">I", data, stsc[0] + 4)[0]
    entries = [
        struct.unpack_from(">III", data, stsc[0] + 8 + 12 * index)
        for index in range(entry_count)
    ]

    samples: list[tuple[int, int]] = []
    sample_index = 0
    for position, (first_chunk, per_chunk, _) in enumerate(entries):
        last_chunk = (
            entries[position + 1][0] - 1
            if position + 1 < len(entries)
            else len(chunk_offsets)
        )
        for chunk in range(first_chunk, last_chunk + 1):
            offset = chunk_offsets[chunk - 1]
            for _ in range(per_chunk):
                if sample_index >= count:
                    break
                samples.append((offset, sizes[sample_index]))
                offset += sizes[sample_index]
                sample_index += 1
    if sample_index != count:
        raise SystemExit(f"stsc 覆盖 {sample_index} 个 sample，stsz 声明 {count} 个")
    return samples


def read_sync_samples(
    data: bytes, stbl: tuple[int, int], sample_count: int
) -> set[int]:
    """`stss` 声明的同步样本，返回 0 基下标。缺失时视为全部同步。"""
    stss = find_box(data, "stss", *stbl)
    if stss is None:
        return set(range(sample_count))
    count = struct.unpack_from(">I", data, stss[0] + 4)[0]
    entries = struct.unpack_from(f">{count}I", data, stss[0] + 8)
    return {value - 1 for value in entries}


def load_track(path: Path) -> tuple[list[bytes], set[int]]:
    data = path.read_bytes()
    moov = find_box(data, "moov", 0, len(data))
    if moov is None:
        raise SystemExit(f"{path}：缺少 moov")

    for kind, start, end in iter_boxes(data, *moov):
        if kind != "trak":
            continue
        stbl = find_path(data, ["mdia", "minf", "stbl"], start, end)
        if stbl is None:
            continue
        stsd = find_box(data, "stsd", *stbl)
        if stsd is None or b"ac-4" not in data[stsd[0] : stsd[1]]:
            continue
        samples = read_sample_table(data, stbl)
        frames = [data[offset : offset + size] for offset, size in samples]
        if any(len(frame) != size for frame, (_, size) in zip(frames, samples)):
            raise SystemExit(f"{path}：sample 越过文件末尾")
        return frames, read_sync_samples(data, stbl, len(frames))
    raise SystemExit(f"{path}：未找到 ac-4 轨道")


def sequence_counter(raw: bytes) -> int:
    """读出 `ac4_toc()` 的 `sequence_counter`，见 `TS103190-1:v1.4.1:4.2.3.1`。

    该字段位于 `bitstream_version` 之后，是 TOC 中第一个变长字段之前的固定
    位置，因此可以不实现完整 TOC 解析就取到。`bitstream_version` 取 3 时会由
    `variable_bits()` 扩展，此处不猜测，直接报错。

    本函数只用于**预测**拼接后的期望值；判定仍由 Rust 侧的完整 TOC 解析给出，
    两者互不依赖。
    """
    if len(raw) < 2:
        raise SystemExit("帧过短，无法读出 sequence_counter")
    head = int.from_bytes(raw[:2], "big")
    bitstream_version = head >> 14
    if bitstream_version == 3:
        raise SystemExit("bitstream_version 走了 variable_bits 扩展，本脚本不解析")
    return (head >> 4) & 0x3FF


def is_continuous(previous: int, current: int) -> bool:
    """`4.3.3.2.2` 的正常转移：递增、1 020 -> 1 回绕，或 splice 标记后恢复。"""
    if previous > 1020 or current > 1020:
        return False
    return (
        (previous < 1020 and current == previous + 1)
        or (previous == 1020 and current == 1)
        or (previous == 0 and current != 0)
    )


def wrap_sync_frame(raw: bytes) -> bytes:
    """按 `Annex G` 封装一帧，不带 CRC。"""
    if not raw:
        raise SystemExit("raw_ac4_frame 为空")
    out = struct.pack(">H", SYNC_WORD_PLAIN)
    if len(raw) >= FRAME_SIZE_ESCAPE:
        # G.3.2：转义值是替换而非累加。
        out += struct.pack(">H", FRAME_SIZE_ESCAPE) + len(raw).to_bytes(3, "big")
    else:
        out += struct.pack(">H", len(raw))
    return out + raw


def pick_splice_point(sync_samples: set[int], count: int, requested: int | None) -> int:
    if requested is not None:
        return requested
    # 取中段的一个非同步帧：切换后必须等到下一个完整随机访问点。
    for index in range(count // 2, count):
        if index not in sync_samples:
            return index
    raise SystemExit("找不到非同步帧作为拼接点")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("first", type=Path, help="拼接点之前的来源")
    parser.add_argument("second", type=Path, help="拼接点之后的来源")
    parser.add_argument("-o", "--output", type=Path, required=True)
    parser.add_argument(
        "--splice-at",
        type=int,
        default=None,
        help="第一条流保留的帧数；默认取中段第一个非同步帧",
    )
    parser.add_argument(
        "--report",
        type=Path,
        default=None,
        help="把拼接参数与期望值写入 JSON，供门禁比对",
    )
    parser.add_argument(
        "--second-start",
        type=int,
        default=None,
        help="第二条流的起始帧；默认与拼接点同下标，保证不落在随机访问点",
    )
    args = parser.parse_args()

    head_frames, head_sync = load_track(args.first)
    tail_frames, tail_sync = load_track(args.second)

    splice_at = pick_splice_point(head_sync, len(head_frames), args.splice_at)
    second_start = (
        args.second_start
        if args.second_start is not None
        else pick_splice_point(tail_sync, len(tail_frames), None)
    )

    if not 0 < splice_at <= len(head_frames):
        raise SystemExit(f"拼接点 {splice_at} 超出第一条流的 {len(head_frames)} 帧")
    if not 0 <= second_start < len(tail_frames):
        raise SystemExit(f"起始帧 {second_start} 超出第二条流的 {len(tail_frames)} 帧")

    selected = head_frames[:splice_at] + tail_frames[second_start:]
    payload = b"".join(wrap_sync_frame(frame) for frame in selected)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(payload)

    tail_random_access = sorted(
        index - second_start + splice_at
        for index in tail_sync
        if index >= second_start
    )
    head_random_access = sorted(index for index in head_sync if index < splice_at)

    # 期望值全部由容器 sample table 与计数器算术导出，与 Rust 侧的比特级
    # TOC 解析没有共同来源。
    boundary_continuous = is_continuous(
        sequence_counter(head_frames[splice_at - 1]),
        sequence_counter(tail_frames[second_start]),
    )
    expected_source_changes = 0 if boundary_continuous else 1
    if tail_random_access:
        # 来源变化后挂起重置，直到下一个完整随机访问点才执行。
        waiting = tail_random_access[0] - splice_at
    else:
        waiting = len(selected) - splice_at
    expected_waiting = 0 if boundary_continuous else waiting
    report = {
        "first": args.first.name,
        "second": args.second.name,
        "splice_at": splice_at,
        "second_start": second_start,
        "frames": len(selected),
        "boundary_sequence_continuous": boundary_continuous,
        "expected_source_changes": expected_source_changes,
        "expected_waiting_for_random_access_frames": expected_waiting,
        "expected_reset_events": 1 + expected_source_changes,
        "expected_full_random_access_frames": len(head_random_access)
        + len(tail_random_access),
    }
    if args.report is not None:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
    print(f"来源 A         : {args.first.name}")
    print(f"来源 B         : {args.second.name}")
    print(f"拼接点         : 第 {splice_at} 帧（A 的 0..{splice_at - 1}）")
    print(f"B 起始帧       : {second_start}"
          f"{'（同步样本）' if second_start in tail_sync else '（非同步样本）'}")
    print(f"输出帧数       : {len(selected)}")
    print(f"拼接后首个起解点: 第 {tail_random_access[0]} 帧"
          if tail_random_access else "拼接后无随机访问点")
    print(f"边界计数连续    : {'是' if boundary_continuous else '否'}")
    print(f"输出           : {args.output} （{len(payload):,} B）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
