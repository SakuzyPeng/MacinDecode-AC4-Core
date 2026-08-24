#!/usr/bin/env python3
"""独立复算 HF patch 表与限幅器表的锚点（`TS103190-1:v1.4.1:5.7.6.3.1.4`/`.5`）。

    ./scripts/check_patch_tables.py
    ./scripts/check_patch_tables.py --sweep   # 另行扫过全部合法配置

`Pseudocode 71` 没有随规范给出示例表，因此没有现成的数值可以核对。Rust 侧的
判据是三层：全配置的结构不变量（源落在低带内、边界严格递增、段数不超过 5）、
源码里的逐字段锚点表，以及覆盖全部合法配置的输出摘要。结构不变量抓不住
「数值错了但仍然合法」的缺陷——
实测把 `goal_sb` 两档对调、`source_band_low` 两档对调、末段并掉的阈值 3 改成 2、
弹回表尾的阈值 3 改成 4，四项注入全部照常通过。锚点表补的正是这一层。

本脚本从 PDF 抄来的模板表出发，独立重写 `Pseudocode 67`–`69`、`71` 与 `72`–`74`；
`Pseudocode 72` 的两处一致性修正在 `limiter.rs` 里单独记录。模板表本身由
`check_aspx_tables.py` 对着 PDF 核对。`--sweep`
把 904 组输入与输出按固定顺序计算 FNV-1a 摘要；Rust 单元测试从生产实现计算同一
摘要，因此未落进锚点的配置也受门禁保护。

只用标准库，不需要规范 PDF。
"""

from __future__ import annotations

import argparse
import ast
import math
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SOURCE = REPO_ROOT / "crates/macindecode-ac4-bitstream/src/aspx/patches.rs"

# 5.7.6.3.1.1 的两张静态模板表。
TEMPLATE_LOWRES = [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 24, 26, 28, 30, 32, 35, 38, 42, 46]
TEMPLATE_HIGHRES = [18, 19, 20, 21, 22, 23, 24, 26, 28, 30, 32, 34, 36, 38, 40, 42, 44, 47, 50, 53, 56, 59, 62]

SPEC_MAX_PATCHES = 5
SPEC_CONFIGURATION_COUNT = 904
FNV64_OFFSET_BASIS = 0xCBF29CE484222325
FNV64_PRIME = 0x00000100000001B3
FNV64_MASK = (1 << 64) - 1


class Unsatisfiable(Exception):
    """伪码在 C 里会读到数组之外。"""


def master_table(master_freq_scale: bool, start_freq: int, stop_freq: int):
    """`Pseudocode 67`。"""
    if master_freq_scale:
        num = 22 - 2 * start_freq - 2 * stop_freq
        template = TEMPLATE_HIGHRES
    else:
        num = 20 - 2 * start_freq - 2 * stop_freq
        template = TEMPLATE_LOWRES
    if num <= 0:
        return None
    return [template[2 * start_freq + sbg] for sbg in range(num + 1)], num


def signal_highres(master: list[int], num_master: int, xover: int):
    """`Pseudocode 68`。"""
    num = num_master - xover
    if num <= 0:
        return None
    table = [master[sbg + xover] for sbg in range(num + 1)]
    return table[0], table[num] - table[0]


def signal_lowres(master, num_master, xover):
    """`Pseudocode 69`：低分辨率表是高分辨率表的二分之一抽取。"""
    num_high = num_master - xover
    high = [master[sbg + xover] for sbg in range(num_high + 1)]
    num_low = num_high - num_high // 2
    low = [high[0]]
    if num_high % 2 == 0:
        low += [high[2 * sbg] for sbg in range(1, num_low + 1)]
    else:
        low += [high[2 * sbg - 1] for sbg in range(1, num_low + 1)]
    return low, num_low


def limiter_table(lowres, num_lowres, patch_borders, num_patches):
    """`Pseudocode 72`–`74`，并保留完整 A-SPX 范围的终止边界。

    原文第二个复制循环行尾有个分号，会让循环体落空且写到越界的下标；这里按
    意图逐个复制。合并循环另会在部分配置上删掉 `sbz`，与子带组表的
    通用定义及 `Pseudocode 96`/`100` 的全子带映射冲突；因此把它视为必留锚点。
    两处判定的理由都见 `limiter.rs` 的模块文档。
    """
    borders = [lowres[sbg] for sbg in range(num_lowres + 1)]
    borders += [patch_borders[sbg] for sbg in range(1, num_patches)]
    borders.sort()
    num_lim = num_lowres + num_patches - 1
    terminal = lowres[num_lowres]

    def is_patch(value):
        return value in patch_borders[: num_patches + 1]

    def is_required(value):
        return value == terminal or is_patch(value)

    sbg = 1
    budget = 4 * (len(borders) + 8)
    while sbg <= num_lim:
        budget -= 1
        if budget < 0:
            raise Unsatisfiable("限幅器合并不终止")
        if math.log2(borders[sbg] / borders[sbg - 1]) >= 0.245:
            sbg += 1
            continue
        if borders[sbg] == borders[sbg - 1]:
            del borders[sbg]
        elif is_required(borders[sbg]):
            if is_required(borders[sbg - 1]):
                sbg += 1
                continue
            del borders[sbg - 1]
        else:
            del borders[sbg]
        num_lim -= 1
    return num_lim, borders[: num_lim + 1]


def patch_table(master, num_master, sba, sbx, num_sb_aspx, master_freq_scale, base_48):
    """`Pseudocode 71`，逐字照抄。

    两处在 C 里会越界的地方抛异常而不是静默夹紧，好确认它们在合法配置上
    确实不可达。
    """
    msb, usb = sba, sbx
    count = 0
    nums: list[int] = []
    starts: list[int] = []
    goal_sb = 43 if base_48 else 46
    source_band_low = 4 if master_freq_scale else 2

    if goal_sb < sbx + num_sb_aspx:
        sbg, i = 0, 0
        while master[i] < goal_sb:
            sbg = i + 1
            i += 1
            if i > num_master:
                raise Unsatisfiable("起点搜索越过主表末端")
    else:
        sbg = num_master

    budget = 4 * (num_master + 8)
    while True:
        j = sbg
        sb = master[j]
        odd = (sb - 2 + sba) % 2
        while sb > (sba - source_band_low + msb - odd):
            j -= 1
            if j < 0:
                raise Unsatisfiable("内层 while 读到 sbg_master[-1]")
            sb = master[j]
            odd = (sb - 2 + sba) % 2

        num = max(sb - usb, 0)
        if num > 0:
            if count >= SPEC_MAX_PATCHES:
                raise Unsatisfiable("段数超过 5")
            nums.append(num)
            starts.append(sba - odd - num)
            usb = msb = sb
            count += 1
        else:
            msb = sbx

        if master[sbg] - sb < 3:
            sbg = num_master
        if sb == sbx + num_sb_aspx:
            break
        budget -= 1
        if budget < 0:
            raise Unsatisfiable("do-while 不终止")

    if count > 1 and nums[count - 1] < 3:
        count -= 1

    borders = [sbx]
    for i in range(1, count + 1):
        borders.append(borders[i - 1] + nums[i - 1])
    return nums[:count], starts[:count], borders


def derive(master_freq_scale, start_freq, stop_freq, xover, base_48):
    built = master_table(master_freq_scale, start_freq, stop_freq)
    if built is None:
        return None
    master, num_master = built
    sig = signal_highres(master, num_master, xover)
    if sig is None:
        return None
    sbx, num_sb_aspx = sig
    patches = patch_table(
        master, num_master, master[0], sbx, num_sb_aspx, master_freq_scale, base_48
    )
    lowres, num_lowres = signal_lowres(master, num_master, xover)
    limits = limiter_table(lowres, num_lowres, patches[2], len(patches[0]))
    return patches, limits


# rustfmt 会把每个元组拆成多行，元素间的 `\s*` 因此要吃下换行（`\s` 本就包含
# 换行，不需要 re.DOTALL——正则里没有 `.`）。跨行匹配容易少读却不报错，故解析后
# 另用括号计数交叉核对条数。
def hash_limiter(
    digest: int,
    scale: bool,
    start_freq: int,
    stop_freq: int,
    xover: int,
    base_48: bool,
    limits,
) -> int:
    """限幅器表单独一个摘要：混进 patch 摘要会让失败无法定位到哪张表。"""
    num_lim, borders = limits
    if len(borders) != num_lim + 1:
        raise ValueError("限幅器边界数应为组数加一")
    fields = [int(scale), start_freq, stop_freq, xover, int(base_48), num_lim]
    fields.extend(borders)
    fields.append(0xFF)
    for value in fields:
        digest = hash_byte(digest, value)
    return digest


ANCHOR_ROW = re.compile(
    r"\(\s*(true|false)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(true|false)\s*,"
    r"\s*&(\[[^\]]*\])\s*,\s*&(\[[^\]]*\])\s*,\s*&(\[[^\]]*\])\s*,?\s*\)"
)
SWEEP_DIGEST = re.compile(
    r"PATCH_SWEEP_FNV64\s*:\s*u64\s*=\s*(0x[0-9a-fA-F_]+|\d+)\s*;"
)
LIMITER_DIGEST = re.compile(
    r"LIMITER_SWEEP_FNV64\s*:\s*u64\s*=\s*(0x[0-9a-fA-F_]+|\d+)\s*;"
)
LIMITER_SOURCE = REPO_ROOT / "crates/macindecode-ac4-bitstream/src/aspx/limiter.rs"


def read_anchors(text: str):
    start = text.find("const PATCH_ANCHORS")
    if start < 0:
        raise SystemExit("patches.rs 里找不到 PATCH_ANCHORS")
    end = text.find("\n];", start)
    if end < 0:
        raise SystemExit("PATCH_ANCHORS 没有结尾")
    body = text[start:end]
    rows = []
    for match in ANCHOR_ROW.finditer(body):
        scale, start_freq, stop_freq, xover, base_48, nums, starts, borders = match.groups()
        rows.append(
            (
                scale == "true",
                int(start_freq),
                int(stop_freq),
                int(xover),
                base_48 == "true",
                ast.literal_eval(nums),
                ast.literal_eval(starts),
                ast.literal_eval(borders),
            )
        )
    if not rows:
        raise SystemExit("PATCH_ANCHORS 解析为空")
    # 表里每一项恰好是一个顶层元组，`&[` 只出现在元组内部的三个数组上。
    declared = body.count("&[") // 3
    if declared != len(rows):
        raise SystemExit(
            f"PATCH_ANCHORS 有 {declared} 项，正则只读出 {len(rows)} 项——"
            "格式变化会让核对静默地少查"
        )
    return rows


def read_sweep_digest(text: str) -> int:
    matches = SWEEP_DIGEST.findall(text)
    if len(matches) != 1:
        raise SystemExit(
            f"patches.rs 应恰有一个 PATCH_SWEEP_FNV64，实际找到 {len(matches)} 个"
        )
    return int(matches[0].replace("_", ""), 0)


def read_limiter_digest(text: str) -> int:
    matches = LIMITER_DIGEST.findall(text)
    if len(matches) != 1:
        raise SystemExit(
            f"limiter.rs 应恰有一个 LIMITER_SWEEP_FNV64，实际找到 {len(matches)} 个"
        )
    return int(matches[0].replace("_", ""), 0)


def hash_byte(digest: int, value: int) -> int:
    if not 0 <= value <= 0xFF:
        raise ValueError(f"摘要字段 {value} 超出一个字节")
    return ((digest ^ value) * FNV64_PRIME) & FNV64_MASK


def hash_configuration(
    digest: int,
    scale: bool,
    start_freq: int,
    stop_freq: int,
    xover: int,
    base_48: bool,
    patches,
) -> int:
    nums, starts, borders = patches
    if len(nums) != len(starts) or len(borders) != len(nums) + 1:
        raise ValueError("patch 输出的三个数组长度不一致")
    fields = [int(scale), start_freq, stop_freq, xover, int(base_48), len(nums)]
    for num, start in zip(nums, starts):
        fields.extend((num, start))
    fields.extend(borders)
    fields.append(0xFF)
    for value in fields:
        digest = hash_byte(digest, value)
    return digest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sweep", action="store_true", help="额外扫过全部合法配置")
    args = parser.parse_args()

    source_text = SOURCE.read_text(encoding="utf-8")
    anchors = read_anchors(source_text)
    failures = 0
    for scale, start_freq, stop_freq, xover, base_48, nums, starts, borders in anchors:
        pair = derive(scale, start_freq, stop_freq, xover, base_48)
        label = f"scale={int(scale)} start={start_freq} stop={stop_freq} xover={xover} f48={int(base_48)}"
        if pair is None:
            print(f"锚点 {label} 在参考实现里推不出频带表", file=sys.stderr)
            failures += 1
            continue
        got = pair[0]
        if list(got[0]) != list(nums) or list(got[1]) != list(starts) or list(got[2]) != list(borders):
            print(f"锚点 {label} 不符：", file=sys.stderr)
            print(f"  源码 num_sb={nums} start_sb={starts} borders={borders}", file=sys.stderr)
            print(f"  复算 num_sb={got[0]} start_sb={got[1]} borders={got[2]}", file=sys.stderr)
            failures += 1

    if failures:
        print(f"patch 表锚点核对失败：{failures} / {len(anchors)}", file=sys.stderr)
        return 1
    print(f"patch 表锚点核对通过：{len(anchors)} 组配置逐字段一致")

    if args.sweep:
        expected_digest = read_sweep_digest(source_text)
        expected_limiter = read_limiter_digest(
            LIMITER_SOURCE.read_text(encoding="utf-8")
        )
        total = unsatisfiable = 0
        digest = FNV64_OFFSET_BASIS
        limiter_digest = FNV64_OFFSET_BASIS
        for scale in (False, True):
            for start_freq in range(8):
                for stop_freq in range(4):
                    for xover in range(8):
                        for base_48 in (False, True):
                            try:
                                got = derive(scale, start_freq, stop_freq, xover, base_48)
                            except Unsatisfiable as error:
                                unsatisfiable += 1
                                print(
                                    f"  伪码越界 scale={int(scale)} start={start_freq} "
                                    f"stop={stop_freq} xover={xover} f48={int(base_48)}：{error}",
                                    file=sys.stderr,
                                )
                                continue
                            if got is None:
                                continue
                            digest = hash_configuration(
                                digest,
                                scale,
                                start_freq,
                                stop_freq,
                                xover,
                                base_48,
                                got[0],
                            )
                            limiter_digest = hash_limiter(
                                limiter_digest,
                                scale,
                                start_freq,
                                stop_freq,
                                xover,
                                base_48,
                                got[1],
                            )
                            total += 1

        sweep_failed = False
        if total != SPEC_CONFIGURATION_COUNT:
            print(
                f"全配置数应为 {SPEC_CONFIGURATION_COUNT}，实际 {total}",
                file=sys.stderr,
            )
            sweep_failed = True
        if unsatisfiable:
            print(f"伪码越界共 {unsatisfiable} 组", file=sys.stderr)
            sweep_failed = True
        if limiter_digest != expected_limiter:
            print(
                f"限幅器表摘要不符：Rust 期望 0x{expected_limiter:016x}，"
                f"参考实现得到 0x{limiter_digest:016x}",
                file=sys.stderr,
            )
            sweep_failed = True
        if digest != expected_digest:
            print(
                f"patch 表摘要不符：Rust 期望 0x{expected_digest:016x}，"
                f"参考实现得到 0x{digest:016x}",
                file=sys.stderr,
            )
            sweep_failed = True
        if sweep_failed:
            return 1
        print(
            f"全配置核对通过：{total} 组，"
            f"patch FNV-1a 0x{digest:016x}，"
            f"limiter FNV-1a 0x{limiter_digest:016x}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
