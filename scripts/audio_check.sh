#!/usr/bin/env bash
# A-JOC 音频数据门禁：从 audio_data_ajoc() 的落点一直验证到声道级 PCM 合成。
#
#   ./scripts/audio_check.sh <file.m4a|file.ac4> [...]
#
# `ac4_substream()` 的 audio_size 与 substream_index_table() 的 substream_size
# 是两条彼此独立的长度声明。metadata 侧的门禁（trace 的 audio_substream）已经
# 用后者卡住了「跳过音频数据 + 解析 metadata」的落点；本门禁把跳过的那一段真
# 正解出来，用前者卡住 audio_data_ajoc() 自身。
#
# 判定：
#   reconstruction_invariants.violations 为空
#                           A-JOC 重建链的全部完整性不变量。该清单的唯一声明
#                           在 crates/macindecode-ac4-cli/src/trace/invariants.rs 的
#                           ReconstructionInvariant；本脚本只消费它生成的 JSON，
#                           Scene/PCM 导出另由 Session 的逐 AU 事务负责。
#                           当前 15 条：状态延续、A-JOC 帧完整落地、标度因子
#                           越界、缩放、解组、解组非零谱线数、解组能量漂移、缩放后非有限、
#                           IMDCT 合成、非有限 PCM、PCM 样本数守恒、A-JOC 对象重建、
#                           对象 PCM 有限值、对象输出形状、A-SPX 驱动。
#   reconstruction_invariants.checked > 0
#                           不变量确实被求值过，防止该块整个消失后静默放行
#
# 以下是**依赖具体测试向量**的条件，不适合场景导出，只在本脚本：
#   parsed == frames        每个 A-JOC 帧都完整解析
#   parsed_substreams == substreams
#                           每条唯一物理 A-JOC substream 都解得下来
#   scale_factor_bands > 0  门禁输入确实走过标度因子还原，不接受空检验
#   scale_factor_min/max != null
#                           统计范围与非零频带数一致
#   scaled_lines > 0        缩放确实执行过，同样不接受空检验
#   scaled_peak 有限正数
#   ungrouped_lines > 0     解组确实执行过
#   pcm_frames > 0          门禁输入确实走过声道级合成
#   pcm_peak 有限正数       门禁输入产生了可观察的 PCM
#   ajoc_full_reconstructed_frames > 0
#                           表 188 到期控制确实进入对象矩阵重建
#   ajoc_full_wet_frames > 0
#                           至少一帧实际执行启用的 decorrelator/wet 路径；用于
#                           钉住 256/448 kbps bed 向量，不以矩阵 census 代替 DSP
#   max_fill_bits < 8       落点只差一个 byte_align
#
# 标度因子那条是取值域约束，比落点判据弱得多：合法区间宽 256，实测取值只占
# 77…159，两侧余量足以吸收多种系统性偏移（见规范可追踪性 5.14）。它能兜住的
# 是错得离谱的情形，不能替代单元测试里手工验算的 DPCM 链。
#
# 最后一条比规范要求的强：fill_bits 长度不受规范约束，编码器可以写任意多填充。
# 之所以能当门禁用，是因为实测编码链恒不写填充；一旦某条流的 fill_bits 达到
# 或超过 8，需要先确认那是编码器的填充还是本实现少读了某个字段。
#
# 需要 audio-decode feature，故需先运行 scripts/fetch_specs.py。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"

if [ "$#" -lt 1 ]; then
    echo "用法：$0 <file> [...]" >&2
    exit 2
fi

failed=0

for input in "$@"; do
    if [ ! -f "${input}" ]; then
        echo "找不到输入：${input}" >&2
        failed=1
        continue
    fi

    if ! trace="$(cargo run -q --locked --manifest-path "${REPO_ROOT}/Cargo.toml" \
            --features macindecode-ac4-cli/audio-decode --bin macinac4 -- \
            trace "${input}")"; then
        echo "trace 失败：${input}" >&2
        failed=1
        continue
    fi

    if ! printf '%s' "${trace}" | "${PYTHON_BIN}" -c '
import json, sys

name = sys.argv[1]
section = json.load(sys.stdin)["result"]["validation"]["ajoc"]
if section is None:
    print(f"  {name}：未启用 audio-decode")
    raise SystemExit(1)

audio = {}
for group in ("coverage", "timing", "configuration", "spectrum", "pcm", "observations"):
    audio.update(section[group])
audio["reconstruction_invariants"] = section["invariants"]["reconstruction"]
audio["min_fill_bits"] = section["timing"]["fill_bits"]["min"]
audio["max_fill_bits"] = section["timing"]["fill_bits"]["max"]
audio["scale_factor_min"] = section["spectrum"]["scale_factor"]["min"]
audio["scale_factor_max"] = section["spectrum"]["scale_factor"]["max"]

frames = audio["frames"]
if frames == 0:
    print(f"  {name}：没有 A-JOC substream，跳过")
    raise SystemExit(0)

parsed = audio["parsed"]
substreams = audio["substreams"]
parsed_substreams = audio["parsed_substreams"]
low = audio["min_fill_bits"]
fill = audio["max_fill_bits"]
dmx = audio["dmx_object_info_blocks"]
umx = audio["umx_object_info_blocks"]
sf_bands = audio["scale_factor_bands"]
scaled_lines = audio["scaled_lines"]
scaled_peak = audio["scaled_peak"]
ungrouped_lines = audio["ungrouped_lines"]
energy_drift = audio["ungroup_energy_drift"]
pcm_frames = audio["pcm_frames"]
pcm_samples = audio["pcm_samples"]
pcm_peak = audio["pcm_peak"]
delayed_zero = audio["pcm_zero_output_with_nonzero_input_frames"]
full_warmup = audio["ajoc_full_warmup_frames"]
full_reconstructed = audio["ajoc_full_reconstructed_frames"]
full_wet = audio["ajoc_full_wet_frames"]
invariants = audio["reconstruction_invariants"]
sf_min = audio["scale_factor_min"]
sf_max = audio["scale_factor_max"]

problems = []
# 重建完整性判据只有一份声明，见
# crates/macindecode-ac4-cli/src/trace/invariants.rs 的 ReconstructionInvariant。
if invariants["checked"] <= 0:
    problems.append("没有求值任何重建不变量，门禁形同虚设")
problems.extend(item["detail"] for item in invariants["violations"])
# 以下条件依赖具体测试向量，不进 Rust 侧的共享清单。
if parsed != frames:
    problems.append("解析 {}/{} 帧".format(parsed, frames))
if parsed_substreams != substreams:
    problems.append("解析 {}/{} 条物理 substream".format(parsed_substreams, substreams))
if sf_bands <= 0:
    problems.append("没有还原出任何标度因子，门禁未覆盖重建路径")
elif sf_min is None or sf_max is None:
    problems.append("已还原 {} 个频带但取值范围不完整：{}…{}".format(
        sf_bands, sf_min, sf_max))
if scaled_lines <= 0:
    problems.append("没有缩放任何谱线，门禁未覆盖反量化路径")
elif not (0.0 < scaled_peak < float("inf")):
    problems.append("缩放峰值不是有限正数：{}".format(scaled_peak))
if ungrouped_lines <= 0:
    problems.append("没有解组任何谱线，门禁未覆盖重排路径")
if pcm_frames <= 0:
    problems.append("没有合成任何声道帧，门禁未覆盖 PCM 路径")
if not (0.0 < pcm_peak < float("inf")):
    problems.append("PCM 峰值不是有限正数：{}".format(pcm_peak))
if full_warmup <= 0:
    problems.append("没有以零对象帧预热 full 终端 QMF 合成")
if full_reconstructed <= 0:
    problems.append("没有任何到期控制进入 A-JOC full 对象重建")
if full_wet <= 0:
    problems.append("没有任何真实帧执行启用的 A-JOC wet/去相关路径")
if fill is None or fill >= 8:
    problems.append("max_fill_bits={}，超出 byte_align 的范围".format(fill))

if problems:
    print("  {}：{}".format(name, "；".join(problems)))
    raise SystemExit(1)

print(
    "  {}：{} 帧全部落在 audio_size 内"
    "（fill_bits {}…{}，dmx {} 块，umx {} 块，标度因子 {} 带 {}…{}，"
    "缩放 {} 线峰值 {:.4g}，解组 {} 线漂移 {:.1e}，"
    "PCM {} 帧/{} 样本峰值 {:.4g}，非零输入零输出 {} 帧，"
    "full 预热/重建/wet {}/{}/{} 帧）".format(
        name, frames, low, fill, dmx, umx, sf_bands, sf_min, sf_max,
        scaled_lines, scaled_peak, ungrouped_lines, energy_drift,
        pcm_frames, pcm_samples, pcm_peak, delayed_zero,
        full_warmup, full_reconstructed, full_wet
    )
)
' "$(basename "${input}")"; then
        failed=1
    fi
done

if [ "${failed}" -ne 0 ]; then
    echo "A-JOC 音频数据门禁未通过" >&2
    exit 1
fi

echo "A-JOC 音频数据门禁通过"
