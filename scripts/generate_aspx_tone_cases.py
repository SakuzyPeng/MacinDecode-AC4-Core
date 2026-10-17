#!/usr/bin/env python3
"""生成不含许可数据的 A-SPX 音调原语对照输入；输出文件必须尚不存在。"""
from pathlib import Path
import argparse

# master, start, stop, noise_sbg, xover, sbx, num_sb_aspx。
# 本地 Rust 重放通过 AspxBandTables::derive 独立验证后两项。
LAYOUTS = [(1, 6, 0, 3, 0, 36, 26), (1, 6, 1, 3, 0, 36, 20),
           (1, 6, 0, 3, 1, 38, 24), (1, 6, 0, 3, 5, 47, 15)]


def generate():
    rows, group = [], 0

    def add(factor=2, left=0, right=16, envelopes=1, layout=LAYOUTS[0], seed=0,
            onset=0, pattern=0, level=1, profile=0, force=0, first=0, new=True):
        nonlocal group
        if new:
            group += 1
        rows.append([f"t{len(rows):04}", group, int(first), factor, left, right, envelopes,
                     *layout, seed, onset, pattern, level, profile, force])

    for factor in (1, 2):
        for left in range(4):
            for seed in range(4):
                for layout in LAYOUTS:
                    for onset in (0, 1):
                        for pattern in (0, 1, 2):
                            add(factor=factor, left=left, seed=seed, layout=layout, onset=onset, pattern=pattern)
    for factor in (1, 2):
        for left in range(4):
            for layout in LAYOUTS:
                add(factor=factor, left=left, layout=layout, first=1)
    for layout in LAYOUTS:
        for level in (0, .125, 1, 2**-23):
            for pattern in (0, 3, 4, 5):
                for force in (0, 1):
                    add(layout=layout, level=level, pattern=pattern, force=force, left=3)
    for factor in (1, 2):
        for envelopes in (2, 4):
            for profile in (0, 1, 2):
                for seed in range(4):
                    add(factor=factor, envelopes=envelopes, profile=profile, seed=seed)
    # 三帧总共 48 ATS 的同一条时间轴；只改变每帧分界。
    for factor in (1, 2):
        for layout in LAYOUTS:
            for seed in range(4):
                for split in range(4):
                    for step, (left, right) in enumerate([(0, 16+split), (split, 16+split), (split, 16)]):
                        add(factor=factor, layout=layout, seed=seed, left=left, right=right, new=step == 0)
    # 静音历史、倍率／频带变化及明确初始化；配置变化本身不重置音调。
    for seed in range(4):
        for step in range(4):
            add(factor=1+step % 2, layout=LAYOUTS[step], seed=seed, left=step % 4,
                level=0 if step == 0 else 1, first=step == 2, new=step == 0)
    return "\n".join(" ".join(map(str, row)) for row in rows)+"\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    with args.output.open("x") as f:
        f.write(generate())


if __name__ == "__main__":
    main()
