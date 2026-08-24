#!/usr/bin/env bash
# 为已验证的 768K 固定 Core 网格生成 DRP/Core 5.1.4 Float32 PCM CAF 对照。
#
# loudnorm 只负责测量。实际音频处理只有固定 Core 量级换算、声道重排和恒定增益；
# 不经过 APAC、不重渲染、不做动态归一化、限幅或削波。
#
# 实验豁免：本脚本只服务已由人工确认 q0..q8 扬声器语义的本地 A/B 材料，故有意
# 不经过 export-core-caf 的生产 OAMD 网格门禁。下方形状检查不能独立证明对象位置
# 或静态渲染语义；不得把本脚本用于未知输入、公共导出或生产交付。

set -euo pipefail

usage() {
    cat <<'EOF'
用法：
  scripts/export_514_pcm_pair.sh INPUT OUTPUT_DIR [TARGET_LUFS]

示例：
  scripts/export_514_pcm_pair.sh program.m4a ./compare -13.6

环境变量：
  MACINAC4_CLI              release 版 macinac4 路径
  DRP_DECODER_BIN            DRP 对照解码器路径或命令名（必需）
  FFMPEG_BIN                 ffmpeg 路径或命令名（默认 ffmpeg）
  AFCONVERT_BIN              afconvert 路径或命令名（默认 afconvert）
  AFINFO_BIN                 afinfo 路径或命令名（默认 afinfo）
  PYTHON_BIN                 Python 3 路径或命令名（默认 python3）
  MAX_TRUE_PEAK_DBTP         允许的最高真峰值（默认 -1.0）

输出是 48 kHz、10 声道、Float32 CAF，使用 Apple Atmos_5_1_4 标签，顺序为
L R C LFE Ls Rs Vhl Vhr Ltr Rtr。默认目标为 -13.6 LUFS，拒绝覆盖已有文件。

实验豁免：Core 路径只对已人工确认的 768K 固定网格使用；形状检查要求
export-aspx-pcm 给出同一 A-JOC substream 的 q0..q8 和独立 LFE，并映射为
q0 q1 q2 LFE q3..q8。它有意不经过 export-core-caf 的生产 OAMD 网格门禁，
不得据此认为任意同形状 AC-4 Core 都有相同声道语义。
EOF
}

die() {
    echo "错误：$*" >&2
    exit 1
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    usage >&2
    exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INPUT="$1"
OUTPUT_DIR="$2"
TARGET_LUFS="${3:--13.6}"
MAX_TRUE_PEAK_DBTP="${MAX_TRUE_PEAK_DBTP:--1.0}"
CORE_CLI="${MACINAC4_CLI:-${REPO_ROOT}/target/release/macinac4}"
DRP_CLI="${DRP_DECODER_BIN:-}"
FFMPEG_BIN="${FFMPEG_BIN:-ffmpeg}"
AFCONVERT_BIN="${AFCONVERT_BIN:-afconvert}"
AFINFO_BIN="${AFINFO_BIN:-afinfo}"
PYTHON_BIN="${PYTHON_BIN:-python3}"

[ -f "$INPUT" ] || die "找不到输入：$INPUT"
[ -x "$CORE_CLI" ] || die "找不到 release Core CLI：$CORE_CLI"
[ -n "$DRP_CLI" ] || die "实验脚本要求通过 DRP_DECODER_BIN 指定 DRP 对照解码器"
command -v "$DRP_CLI" >/dev/null 2>&1 || die "找不到 DRP 对照解码器：$DRP_CLI"
for tool in "$FFMPEG_BIN" "$AFCONVERT_BIN" "$AFINFO_BIN" "$PYTHON_BIN"; do
    command -v "$tool" >/dev/null 2>&1 || die "找不到工具：$tool"
done
awk -v value="$TARGET_LUFS" 'BEGIN { exit !(value ~ /^-?[0-9]+([.][0-9]+)?$/) }' \
    || die "TARGET_LUFS 不是数字：$TARGET_LUFS"
awk -v value="$MAX_TRUE_PEAK_DBTP" 'BEGIN { exit !(value ~ /^-?[0-9]+([.][0-9]+)?$/) }' \
    || die "MAX_TRUE_PEAK_DBTP 不是数字：$MAX_TRUE_PEAK_DBTP"

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
INPUT_DIR="$(cd "$(dirname "$INPUT")" && pwd)"
INPUT="${INPUT_DIR}/$(basename "$INPUT")"
INPUT_NAME="$(basename "$INPUT")"
INPUT_STEM="${INPUT_NAME%.*}"

DRP_OUTPUT="${OUTPUT_DIR}/${INPUT_STEM}_DRP_direct_5.1.4_PCM_F32_LUFS${TARGET_LUFS}.caf"
CORE_OUTPUT="${OUTPUT_DIR}/${INPUT_STEM}_core_direct_5.1.4_PCM_F32_LUFS${TARGET_LUFS}.caf"
[ ! -e "$DRP_OUTPUT" ] || die "输出已存在：$DRP_OUTPUT"
[ ! -e "$CORE_OUTPUT" ] || die "输出已存在：$CORE_OUTPUT"

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/macin-514-pcm.XXXXXX")"
DRP_PARTIAL=""
CORE_PARTIAL=""
cleanup() {
    if [ -n "$DRP_PARTIAL" ] && [ -e "$DRP_PARTIAL" ]; then
        unlink "$DRP_PARTIAL" || true
    fi
    if [ -n "$CORE_PARTIAL" ] && [ -e "$CORE_PARTIAL" ]; then
        unlink "$CORE_PARTIAL" || true
    fi
    case "$WORK_DIR" in
        "${TMPDIR:-/tmp}"/macin-514-pcm.*)
            if [ -d "$WORK_DIR" ]; then
                find "$WORK_DIR" -depth -delete || true
            fi
            ;;
    esac
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

CORE_SCALE="0.000030517578125"
CORE_FILTER="pan=5.1.4|FL=c0|FR=c1|FC=c2|LFE=c9|SL=c3|SR=c4|TFL=c5|TFR=c6|TBL=c7|TBR=c8,volume=${CORE_SCALE}:precision=float"

validate_core_shape() {
    "$PYTHON_BIN" - "$1" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
try:
    payload = json.loads(path.read_text(encoding="utf-8"))
except (OSError, UnicodeError, json.JSONDecodeError) as error:
    raise SystemExit(f"无法读取 Core CLI JSON：{error}")

if payload.get("schema") != "macinac4.cli-result" or payload.get("version") != 1:
    raise SystemExit("Core CLI 未返回 cli-result v1")
if payload.get("command") != "export-aspx-pcm":
    raise SystemExit("Core CLI 返回了错误的 command")
result = payload.get("result")
if not isinstance(result, dict):
    raise SystemExit("Core CLI 缺少 result")
audio = result.get("audio")
if not isinstance(audio, dict) or audio.get("sample_rate_hz") != 48000 or audio.get("channels") != 10:
    raise SystemExit("Core 输出必须是 48 kHz、10 声道")
if result.get("bandwidth") != "aspx":
    raise SystemExit("Core 输出不是 A-SPX PCM")
if result.get("channel_order") != "ajoc_input_then_lfe":
    raise SystemExit("Core 输出缺少 ajoc_input_then_lfe 顺序保证")
if result.get("scale") != "±32768":
    raise SystemExit("Core 输出的内部量级不是 ±32768")

tracks = result.get("tracks")
if not isinstance(tracks, list) or len(tracks) != 10:
    raise SystemExit("Core 输出必须正好包含 9 个 q 和 1 个 LFE")
for index, track in enumerate(tracks[:9]):
    if not isinstance(track, dict):
        raise SystemExit(f"Core q{index} 轨描述无效")
    if track.get("role") != "ajoc_input" or track.get("ajoc_input") != index:
        raise SystemExit(f"Core 第 {index} 轨不是 q{index}")
lfe = tracks[9]
if not isinstance(lfe, dict) or lfe.get("role") != "lfe" or "ajoc_input" in lfe:
    raise SystemExit("Core 第 9 轨不是独立 LFE")
substreams = {track.get("substream") for track in tracks if isinstance(track, dict)}
if len(substreams) != 1 or None in substreams:
    raise SystemExit("Core q/LFE 必须来自同一 A-JOC substream")
PY
}

measure_loudness() {
    local input="$1"
    local label="$2"
    local prefix_filter="$3"
    local log="${WORK_DIR}/${label}.loudnorm.log"
    "$FFMPEG_BIN" -hide_banner -nostats -i "$input" -map 0:a:0 \
        -af "${prefix_filter},loudnorm=I=-24:TP=-1:LRA=20:print_format=json" \
        -f null - >/dev/null 2>"$log"
    local integrated true_peak
    integrated="$(sed -nE 's/.*"input_i"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' "$log" | tail -n 1)"
    true_peak="$(sed -nE 's/.*"input_tp"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' "$log" | tail -n 1)"
    [ -n "$integrated" ] && [ -n "$true_peak" ] \
        || die "无法从 ffmpeg 读取 ${label} 的响度结果；见 $log"
    printf '%s %s\n' "$integrated" "$true_peak"
}

echo "并行导出 DRP 5.1.4 与 Core A-SPX q0..q8 + LFE"
"$DRP_CLI" --input "$INPUT" --output "${WORK_DIR}/drp" --channels 5.1.4 \
    >"${WORK_DIR}/drp.stdout.log" 2>"${WORK_DIR}/drp.stderr.log" &
DRP_EXPORT_PID=$!
"$CORE_CLI" export-aspx-pcm "$INPUT" --output "${WORK_DIR}/core_aspx.wav" \
    >"${WORK_DIR}/core.json" 2>"${WORK_DIR}/core.stderr.log" &
CORE_EXPORT_PID=$!
DRP_EXPORT_STATUS=0
CORE_EXPORT_STATUS=0
wait "$DRP_EXPORT_PID" || DRP_EXPORT_STATUS=$?
wait "$CORE_EXPORT_PID" || CORE_EXPORT_STATUS=$?
if [ "$DRP_EXPORT_STATUS" -ne 0 ]; then
    sed -n '1,120p' "${WORK_DIR}/drp.stderr.log" >&2
    die "DRP CLI 导出失败（状态 ${DRP_EXPORT_STATUS}）"
fi
if [ "$CORE_EXPORT_STATUS" -ne 0 ]; then
    sed -n '1,120p' "${WORK_DIR}/core.stderr.log" >&2
    die "Core CLI 导出失败（状态 ${CORE_EXPORT_STATUS}）"
fi
[ -f "${WORK_DIR}/drp.wav" ] || die "DRP CLI 未生成 drp.wav"
[ -f "${WORK_DIR}/core_aspx.wav" ] || die "Core CLI 未生成 core_aspx.wav"
validate_core_shape "${WORK_DIR}/core.json" || die "Core 固定网格验证失败"

echo "并行测量源 PCM 响度与真峰值"
measure_loudness "${WORK_DIR}/drp.wav" drp_source anull >"${WORK_DIR}/drp_source.stats" &
DRP_MEASURE_PID=$!
measure_loudness "${WORK_DIR}/core_aspx.wav" core_source "$CORE_FILTER" >"${WORK_DIR}/core_source.stats" &
CORE_MEASURE_PID=$!
DRP_MEASURE_STATUS=0
CORE_MEASURE_STATUS=0
wait "$DRP_MEASURE_PID" || DRP_MEASURE_STATUS=$?
wait "$CORE_MEASURE_PID" || CORE_MEASURE_STATUS=$?
[ "$DRP_MEASURE_STATUS" -eq 0 ] || die "DRP 响度测量失败"
[ "$CORE_MEASURE_STATUS" -eq 0 ] || die "Core 响度测量失败"
read -r DRP_I DRP_TP <"${WORK_DIR}/drp_source.stats"
read -r CORE_I CORE_TP <"${WORK_DIR}/core_source.stats"

DRP_GAIN="$(awk -v target="$TARGET_LUFS" -v measured="$DRP_I" \
    'BEGIN { printf "%.6f", target - measured }')"
CORE_GAIN="$(awk -v target="$TARGET_LUFS" -v measured="$CORE_I" \
    'BEGIN { printf "%.6f", target - measured }')"

check_predicted_peak() {
    local label="$1"
    local true_peak="$2"
    local gain="$3"
    if ! awk -v peak="$true_peak" -v gain="$gain" -v ceiling="$MAX_TRUE_PEAK_DBTP" \
        'BEGIN { exit !((peak + gain) <= ceiling) }'; then
        local predicted
        predicted="$(awk -v peak="$true_peak" -v gain="$gain" \
            'BEGIN { printf "%.2f", peak + gain }')"
        die "${label} 达到 ${TARGET_LUFS} LUFS 时预计为 ${predicted} dBTP，超过 ${MAX_TRUE_PEAK_DBTP} dBTP；请降低目标响度"
    fi
}
check_predicted_peak DRP "$DRP_TP" "$DRP_GAIN"
check_predicted_peak Core "$CORE_TP" "$CORE_GAIN"

echo "并行写 CAF：DRP ${DRP_GAIN} dB；Core 固定缩放后 ${CORE_GAIN} dB"
"$FFMPEG_BIN" -hide_banner -nostats -loglevel warning -i "${WORK_DIR}/drp.wav" \
    -map 0:a:0 -af "volume=${DRP_GAIN}dB:precision=float" -c:a pcm_f32le \
    -ar 48000 -channel_layout 5.1.4 -f caf "${WORK_DIR}/drp_untagged.caf" &
DRP_WRITE_PID=$!
"$FFMPEG_BIN" -hide_banner -nostats -loglevel warning -i "${WORK_DIR}/core_aspx.wav" \
    -map 0:a:0 -af "${CORE_FILTER},volume=${CORE_GAIN}dB:precision=float" \
    -c:a pcm_f32le -ar 48000 -channel_layout 5.1.4 -f caf "${WORK_DIR}/core_untagged.caf" &
CORE_WRITE_PID=$!
DRP_WRITE_STATUS=0
CORE_WRITE_STATUS=0
wait "$DRP_WRITE_PID" || DRP_WRITE_STATUS=$?
wait "$CORE_WRITE_PID" || CORE_WRITE_STATUS=$?
[ "$DRP_WRITE_STATUS" -eq 0 ] || die "DRP CAF 写入失败"
[ "$CORE_WRITE_STATUS" -eq 0 ] || die "Core CAF 写入失败"

echo "并行写入 Apple Atmos_5_1_4 channel layout tag"
"$AFCONVERT_BIN" "${WORK_DIR}/drp_untagged.caf" -o "${WORK_DIR}/drp_aligned.caf" \
    -f caff -d 0 -l Atmos_5_1_4 &
DRP_TAG_PID=$!
"$AFCONVERT_BIN" "${WORK_DIR}/core_untagged.caf" -o "${WORK_DIR}/core_aligned.caf" \
    -f caff -d 0 -l Atmos_5_1_4 &
CORE_TAG_PID=$!
DRP_TAG_STATUS=0
CORE_TAG_STATUS=0
wait "$DRP_TAG_PID" || DRP_TAG_STATUS=$?
wait "$CORE_TAG_PID" || CORE_TAG_STATUS=$?
[ "$DRP_TAG_STATUS" -eq 0 ] || die "DRP CAF 标签写入失败"
[ "$CORE_TAG_STATUS" -eq 0 ] || die "Core CAF 标签写入失败"

validate_caf() {
    local input="$1"
    local label="$2"
    local report="${WORK_DIR}/${label}.afinfo.txt"
    "$AFINFO_BIN" "$input" >"$report" 2>&1 || die "afinfo 无法读取 ${label}"
    grep -Eq 'Data format:[[:space:]]+10 ch,[[:space:]]+48000 Hz, Float32, interleaved' "$report" \
        || die "${label} 不是 48 kHz、10 声道、交错 Float32 CAF"
    grep -Fq 'Channel layout: 5.1.4 (L R C LFE Ls Rs Vhl Vhr Ltr Rtr)' "$report" \
        || die "${label} 缺少 Apple Atmos_5_1_4 标签"
}
validate_caf "${WORK_DIR}/drp_aligned.caf" DRP
validate_caf "${WORK_DIR}/core_aligned.caf" Core

echo "并行复测最终 CAF"
measure_loudness "${WORK_DIR}/drp_aligned.caf" drp_final anull >"${WORK_DIR}/drp_final.stats" &
DRP_FINAL_PID=$!
measure_loudness "${WORK_DIR}/core_aligned.caf" core_final anull >"${WORK_DIR}/core_final.stats" &
CORE_FINAL_PID=$!
DRP_FINAL_STATUS=0
CORE_FINAL_STATUS=0
wait "$DRP_FINAL_PID" || DRP_FINAL_STATUS=$?
wait "$CORE_FINAL_PID" || CORE_FINAL_STATUS=$?
[ "$DRP_FINAL_STATUS" -eq 0 ] || die "DRP 最终响度测量失败"
[ "$CORE_FINAL_STATUS" -eq 0 ] || die "Core 最终响度测量失败"
read -r DRP_FINAL_I DRP_FINAL_TP <"${WORK_DIR}/drp_final.stats"
read -r CORE_FINAL_I CORE_FINAL_TP <"${WORK_DIR}/core_final.stats"

validate_final() {
    local label="$1"
    local integrated="$2"
    local true_peak="$3"
    awk -v actual="$integrated" -v target="$TARGET_LUFS" \
        'BEGIN { delta = actual - target; if (delta < 0) delta = -delta; exit !(delta <= 0.05) }' \
        || die "${label} 最终响度 ${integrated} LUFS 与目标 ${TARGET_LUFS} LUFS 相差超过 0.05 LU"
    awk -v peak="$true_peak" -v ceiling="$MAX_TRUE_PEAK_DBTP" \
        'BEGIN { exit !(peak <= ceiling) }' \
        || die "${label} 最终真峰值 ${true_peak} dBTP 超过 ${MAX_TRUE_PEAK_DBTP} dBTP"
}
validate_final DRP "$DRP_FINAL_I" "$DRP_FINAL_TP"
validate_final Core "$CORE_FINAL_I" "$CORE_FINAL_TP"

DRP_PARTIAL="${DRP_OUTPUT}.partial.$$"
CORE_PARTIAL="${CORE_OUTPUT}.partial.$$"
cp "${WORK_DIR}/drp_aligned.caf" "$DRP_PARTIAL"
cp "${WORK_DIR}/core_aligned.caf" "$CORE_PARTIAL"
if ! ln "$DRP_PARTIAL" "$DRP_OUTPUT"; then
    die "发布时输出已存在：$DRP_OUTPUT"
fi
if ! ln "$CORE_PARTIAL" "$CORE_OUTPUT"; then
    if [ "$DRP_OUTPUT" -ef "$DRP_PARTIAL" ]; then
        unlink "$DRP_OUTPUT"
    fi
    die "发布时输出已存在：$CORE_OUTPUT"
fi
unlink "$DRP_PARTIAL"
unlink "$CORE_PARTIAL"
DRP_PARTIAL=""
CORE_PARTIAL=""

printf '完成：\n  DRP  %s LUFS / %s dBTP  %s\n' "$DRP_FINAL_I" "$DRP_FINAL_TP" "$DRP_OUTPUT"
printf '  Core %s LUFS / %s dBTP  %s\n' "$CORE_FINAL_I" "$CORE_FINAL_TP" "$CORE_OUTPUT"
