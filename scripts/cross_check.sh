#!/usr/bin/env bash
#
# 用独立工具交叉核对本项目的 trace 输出。
#
#   ./scripts/cross_check.sh <file.m4a>
#
# M1/M2/M3/M4 退出门禁要求 ffprobe、Bento4 mp4info 与 MediaInfo 全部可用
# 且关键字段一致。GPAC MP4Box 属于额外核对：安装后同样必须通过。
#
# 注意帧数存在两种口径：容器中的 sample 总数，与应用 edit list 后落在
# 呈现区间相交的帧数。后者用于核对 MediaInfo FrameCount。

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INPUT="${1:-}"

if [[ -z "${INPUT}" || ! -f "${INPUT}" ]]; then
    echo "用法：./scripts/cross_check.sh <file.m4a>" >&2
    exit 2
fi

# shellcheck disable=SC1091
[[ -f "${REPO_ROOT}/.env.local" ]] && { set -a; source "${REPO_ROOT}/.env.local"; set +a; }

tool() {
    local var="$1" name="$2"
    local configured="${!var:-}"
    if [[ -n "${configured}" ]]; then
        if [[ ! -x "${configured}" ]]; then
            echo "${var} 配置不可执行" >&2
            return 2
        fi
        printf '%s\n' "${configured}"
        return
    fi
    command -v "${name}" 2>/dev/null
}

missing=()
FFPROBE_BIN=""
MP4INFO_BIN=""
MEDIAINFO_BIN=""
PYTHON_BIN=""

if ! FFPROBE_BIN="$(tool FFPROBE ffprobe)"; then
    missing+=("ffprobe")
fi
if ! MP4INFO_BIN="$(tool MP4INFO mp4info)"; then
    missing+=("Bento4 mp4info")
fi
if ! MEDIAINFO_BIN="$(tool MEDIAINFO mediainfo)"; then
    missing+=("MediaInfo")
fi
if ! PYTHON_BIN="$(command -v python3 2>/dev/null)"; then
    missing+=("python3")
fi

if (( ${#missing[@]} > 0 )); then
    echo "缺少必需工具：" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    exit 1
fi

MP4BOX_BIN=""
if MP4BOX_BIN="$(tool MP4BOX MP4Box)"; then
    have_mp4box=1
else
    have_mp4box=0
fi

# 非 ASCII 字符占两个终端列，printf 的宽度按字节计，需自行折算
disp_width() {
    local text="$1" wide
    wide=$(printf '%s' "${text}" | grep -o '[^ -~]' | wc -l | tr -d ' ')
    echo $(( ${#text} + wide ))
}

row() {
    local label="$1" value="$2" rest="${3:-}" width
    width=$(disp_width "${label}")
    printf '  %s%*s%-12s %s\n' "${label}" $(( 30 - width )) "" "${value}" "${rest}"
}

printf '输入：%s\n\n' "$(basename "${INPUT}")"

# --- 本项目 ---
if ! ours="$(cargo run -q --locked --manifest-path "${REPO_ROOT}/Cargo.toml" --bin macinac4 -- \
        trace "${INPUT}")"; then
    echo "本项目 trace 失败" >&2
    exit 1
fi

if ! ours_fields="$(printf '%s' "${ours}" | "${PYTHON_BIN}" -c '
import json, sys
d = json.load(sys.stdin)
result = d["result"]
source, f = result["source"], result["frames"]
c, p, s, derived = source["track"], source["presentation"], source["dac4"], source["derived"]
validation = result["validation"]
def flat(section):
    out = {}
    for group in ("coverage", "references", "timing", "configuration", "spectrum", "pcm", "observations"):
        out.update(section[group])
    return out
t = flat(validation["topology"])
o = flat(validation["oamd"])
o["min_obj_info_blocks"] = validation["oamd"]["configuration"]["object_info_blocks"]["min"]
o["max_obj_info_blocks"] = validation["oamd"]["configuration"]["object_info_blocks"]["max"]
au = flat(validation["audio_substream"])
au["min_audio_size"] = validation["audio_substream"]["configuration"]["audio_size_bytes"]["min"]
au["max_audio_size"] = validation["audio_substream"]["configuration"]["audio_size_bytes"]["max"]
au["min_metadata_bytes"] = validation["audio_substream"]["configuration"]["metadata_bytes"]["min"]
au["max_metadata_bytes"] = validation["audio_substream"]["configuration"]["metadata_bytes"]["max"]
first = t.get("first_frame") or {}
presentations = first.get("presentations") or [{}]
pres0 = presentations[0]
dsi_comparison = s.get("first_toc_comparison") or {}
dsi_matches = dsi_comparison.get("presentations") or []
dsi_for_toc0 = next(
    (item.get("dsi") or {} for item in dsi_matches if item.get("toc_index") == 0),
    {},
)
delta = derived.get("media_timeline", {}).get("sample_delta", {})
if "constant" in delta:
    max_delta = delta["constant"]
elif "alternating" in delta:
    max_delta = max(delta["alternating"])
else:
    max_delta = ""
print(
    c["sample_count"], c["media_duration"], p["presented_duration"],
    c["media_timescale"], s["bitstream_version"], s["frame_rate_index"],
    f["sync_frames"], f["count"], f["presented_count"],
    f["toc_parse_failures"], f["dac4_toc_mismatches"],
    f["stss_iframe_mismatches"], f["sequence_discontinuities"], max_delta,
    t["frames_parsed"], t["parse_failures"], t["substream_size_overruns"],
    t["dangling_group_references"], t["substream_reference_failures"],
    t["stss_random_access_mismatches"], t["frames_differing_from_first"],
    t["scene_path"], t["total_objects"],
    t["full_random_access_frames"], t["audio_only_random_access_frames"],
    t["config_generations"], t["source_changes"], t["reset_events"],
    t["waiting_for_random_access_frames"], int(t["awaiting_random_access"]),
    o["located"], o["parsed"], o["failures"], o["max_align_bits"],
    o["common_data_frames"], o["common_data_sync_mismatches"],
    o["timing_frames"], o["timing_carryover_frames"],
    o["dyndata_blocks"], o["history_dependent_blocks"],
    o["min_obj_info_blocks"], o["max_obj_info_blocks"], o["max_ramp_duration"],
    au["located"], au["parsed"], au["failures"],
    au["min_audio_size"], au["max_audio_size"],
    au["min_metadata_bytes"], au["max_metadata_bytes"],
    au["max_tools_metadata_bits"], au["dialnorm_frames"],
    au["substream_loudness_frames"],
    pres0.get("version", ""), pres0.get("md_compat", ""),
    int(dsi_comparison.get("consistent") is True),
    dsi_comparison.get("field_mismatches", ""),
    len(dsi_comparison.get("unmatched_dsi") or []),
    len(dsi_comparison.get("unmatched_toc") or []),
    dsi_for_toc0.get("version", ""), dsi_for_toc0.get("md_compat", ""),
)
')"; then
    echo "无法解析本项目 JSON trace" >&2
    exit 1
fi

read -r OURS_SAMPLES OURS_MEDIA OURS_PRESENTED OURS_TS OURS_BSV OURS_FRI \
    OURS_SYNC OURS_FRAMES OURS_PRESENTED_FRAMES OURS_TOC_FAILURES \
    OURS_DAC4_MISMATCHES OURS_STSS_MISMATCHES OURS_SEQUENCE_CHANGES OURS_MAX_DELTA \
    OURS_TOPO_PARSED OURS_TOPO_FAILURES OURS_TOPO_OVERRUNS \
    OURS_TOPO_DANGLING OURS_SUBSTREAM_REFS OURS_STSS_RA_MISMATCHES \
    OURS_TOPO_DRIFT OURS_SCENE_PATH OURS_OBJECTS \
    OURS_RA_FULL OURS_RA_AUDIO OURS_CONFIG_GENS \
    OURS_SOURCE_CHANGES OURS_RESET_EVENTS OURS_WAITING_RA OURS_AWAITING_RA \
    OURS_OAMD_LOCATED OURS_OAMD_PARSED OURS_OAMD_FAILURES OURS_OAMD_ALIGN \
    OURS_OAMD_COMMON OURS_OAMD_COMMON_SYNC_MISMATCHES \
    OURS_OAMD_TIMING OURS_OAMD_CARRYOVER \
    OURS_OAMD_DYNDATA OURS_OAMD_HISTORY \
    OURS_OAMD_MIN_BLOCKS OURS_OAMD_MAX_BLOCKS OURS_OAMD_MAX_RAMP \
    OURS_AUD_LOCATED OURS_AUD_PARSED OURS_AUD_FAILURES \
    OURS_AUD_MIN_SIZE OURS_AUD_MAX_SIZE \
    OURS_AUD_MIN_MD OURS_AUD_MAX_MD \
    OURS_AUD_TOOLS OURS_AUD_DIALNORM OURS_AUD_LOUDNESS \
    OURS_PRES_VER OURS_MD_COMPAT \
    OURS_DSI_COMPARISON OURS_DSI_FIELD_MISMATCHES \
    OURS_DSI_UNMATCHED OURS_TOC_UNMATCHED \
    OURS_DSI_PRES_VER OURS_DSI_MD_COMPAT <<<"${ours_fields}"

mismatch=0
check() {
    local label="$1" expected="$2" actual="$3"
    if [[ -z "${actual}" ]]; then
        row "${label}" "-" "**字段缺失**"
        mismatch=$((mismatch + 1))
    elif [[ "${expected}" == "${actual}" ]]; then
        row "${label}" "${actual}" "一致"
    else
        row "${label}" "${actual}" "**与本项目 ${expected} 不一致**"
        mismatch=$((mismatch + 1))
    fi
}

check_tolerance() {
    local label="$1" expected="$2" actual="$3" tolerance="$4" delta
    if [[ -z "${actual}" ]]; then
        row "${label}" "-" "**字段缺失**"
        mismatch=$((mismatch + 1))
    elif [[ ! "${expected}" =~ ^[0-9]+$ || ! "${actual}" =~ ^[0-9]+$ ]]; then
        row "${label}" "${actual}" "**无法按整数比较**"
        mismatch=$((mismatch + 1))
    else
        delta=$((expected - actual))
        (( delta < 0 )) && delta=$((-delta))
        if (( delta <= tolerance )); then
            row "${label}" "${actual}" "差 ${delta}，刻度舍入范围内"
        else
            row "${label}" "${actual}" "**与本项目 ${expected} 相差 ${delta}**"
            mismatch=$((mismatch + 1))
        fi
    fi
}

tool_failed() {
    row "$1" "-" "**工具执行或输出解析失败**"
    mismatch=$((mismatch + 1))
}

echo "本项目自检："
row "sample 数" "${OURS_SAMPLES}"
row "呈现区间帧数" "${OURS_PRESENTED_FRAMES}"
row "媒体时长" "${OURS_MEDIA}"
row "呈现时长" "${OURS_PRESENTED}"
row "时间刻度" "${OURS_TS}"
row "bitstream_version" "${OURS_BSV}"
row "frame_rate_index" "${OURS_FRI}"
row "同步样本" "${OURS_SYNC}"
check "frame/sample 数" "${OURS_SAMPLES}" "${OURS_FRAMES}"
check "TOC 解析失败" "0" "${OURS_TOC_FAILURES}"
check "dac4/TOC 不一致" "0" "${OURS_DAC4_MISMATCHES}"
if [[ "${OURS_SCENE_PATH}" == "channel_based" ]]; then
    row "dac4 presentation/首帧 TOC" "${OURS_DSI_COMPARISON}" "Channel-based 延后，仅记录"
    row "dac4 presentation 字段失配" "${OURS_DSI_FIELD_MISMATCHES}" "Channel-based 延后，仅记录"
    row "dac4 未匹配 presentation" "${OURS_DSI_UNMATCHED}" "Channel-based 延后，仅记录"
    row "TOC 未匹配 presentation" "${OURS_TOC_UNMATCHED}" "Channel-based 延后，仅记录"
else
    check "dac4 presentation/首帧 TOC" "1" "${OURS_DSI_COMPARISON}"
    check "dac4 presentation 字段失配" "0" "${OURS_DSI_FIELD_MISMATCHES}"
    check "dac4 未匹配 presentation" "0" "${OURS_DSI_UNMATCHED}"
    check "TOC 未匹配 presentation" "0" "${OURS_TOC_UNMATCHED}"
    check "presentation_version" "${OURS_PRES_VER}" "${OURS_DSI_PRES_VER}"
    check "md_compat" "${OURS_MD_COMPAT}" "${OURS_DSI_MD_COMPAT}"
fi
check "stss/I-frame 不一致" "0" "${OURS_STSS_MISMATCHES}"
check "sequence 来源变化" "0" "${OURS_SEQUENCE_CHANGES}"
echo

echo "拓扑自检（M2）："
row "编码路径" "${OURS_SCENE_PATH}"
row "对象总数" "${OURS_OBJECTS}"
check "拓扑解析帧数" "${OURS_FRAMES}" "${OURS_TOPO_PARSED}"
check "拓扑解析失败" "0" "${OURS_TOPO_FAILURES}"
# payload_base 与 substream_index_table 必须与帧长自洽；错一位即越界
check "substream 尺寸越界" "0" "${OURS_TOPO_OVERRUNS}"
check "悬空 group 引用" "0" "${OURS_TOPO_DANGLING}"
check "substream 引用不完整" "0" "${OURS_SUBSTREAM_REFS}"
check "帧间配置漂移" "0" "${OURS_TOPO_DRIFT}"
# 容器 stss 与码流侧「全部 ndot 为真」的判定应给出同一批帧。二者依据
# 完全不同：前者是容器的同步样本表，后者是 TOC 内各 substream 的标志。
check "stss/完整随机访问逐帧失配" "0" "${OURS_STSS_RA_MISMATCHES}"
row "完整随机访问点" "${OURS_RA_FULL}" "stss=${OURS_SYNC}"
row "仅音频起解帧" "${OURS_RA_AUDIO}"
row "配置代次" "${OURS_CONFIG_GENS}"
check "状态机来源变化" "0" "${OURS_SOURCE_CHANGES}"
row "reset 事件" "${OURS_RESET_EVENTS}"
row "等待随机访问帧" "${OURS_WAITING_RA}"
check "结束时仍等待随机访问" "0" "${OURS_AWAITING_RA}"
echo

echo "OAMD 载荷（M3）："
# channel-coded group 按 P2 6.2.1.6 不携带 OAMD substream；对象路径才要求
# 每帧都能按索引表定位并解析。把 channel-based IMS 强制对齐到总帧数会制造误报。
if [[ "${OURS_SCENE_PATH}" == "channel_based" ]]; then
    EXPECTED_OAMD_FRAMES=0
    EXPECTED_OAMD_COMMON=0
else
    EXPECTED_OAMD_FRAMES="${OURS_FRAMES}"
    EXPECTED_OAMD_COMMON="${OURS_SYNC}"
fi
check "OAMD 定位帧数" "${EXPECTED_OAMD_FRAMES}" "${OURS_OAMD_LOCATED}"
check "OAMD 解析帧数" "${EXPECTED_OAMD_FRAMES}" "${OURS_OAMD_PARSED}"
check "OAMD 解析失败" "0" "${OURS_OAMD_FAILURES}"
# oamd_substream() 以 byte_align 结尾：残余必须落在 0…7。该门禁可发现多数
# 可变长字段错位；字段语义另由构造码流单元测试覆盖。
if [ "${OURS_OAMD_ALIGN}" -lt 8 ] 2>/dev/null; then
    row "byte_align 残余上界" "${OURS_OAMD_ALIGN}" "< 8，一致"
else
    row "byte_align 残余上界" "${OURS_OAMD_ALIGN}" "不一致：应 < 8"
    mismatch=1
fi
# 对象路径的公共数据只在完整随机访问点传输，与 stss 同批。
check "OAMD 公共数据帧/stss" "${EXPECTED_OAMD_COMMON}" "${OURS_OAMD_COMMON}"
check "OAMD 公共数据/stss 逐帧失配" "0" "${OURS_OAMD_COMMON_SYNC_MISMATCHES}"
row "OAMD 时间数据帧" "${OURS_OAMD_TIMING}" "沿用前序 ${OURS_OAMD_CARRYOVER}"
row "每帧 object_info_block" "${OURS_OAMD_MIN_BLOCKS}..${OURS_OAMD_MAX_BLOCKS}"
row "ramp_duration 上界" "${OURS_OAMD_MAX_RAMP}"
# A-JOC 路径下逐对象动态数据在 audio_data_ajoc，不在 oamd_substream（表 7）。
row "dyndata_multi 块数" "${OURS_OAMD_DYNDATA}" "依赖前序 ${OURS_OAMD_HISTORY}"
echo

echo "音频 substream 框架（M4）："
# 按 4.3.4.1 用 audio_size 跳过音频数据直达 metadata，不解码音频。
# 判定条件是解析后恰好落在 substream 末尾——错一个可变长字段即失败。
check "音频 substream 定位帧数" "${OURS_FRAMES}" "${OURS_AUD_LOCATED}"
check "框架/metadata 解析帧数" "${OURS_FRAMES}" "${OURS_AUD_PARSED}"
check "落点未对齐 substream 末尾" "0" "${OURS_AUD_FAILURES}"
row "audio_size 范围" "${OURS_AUD_MIN_SIZE}..${OURS_AUD_MAX_SIZE}" "字节"
row "metadata 区段" "${OURS_AUD_MIN_MD}..${OURS_AUD_MAX_MD}" "字节"
row "tools_metadata 上界" "${OURS_AUD_TOOLS}" "比特"
# sus_ver 在 bitstream_version = 2 下恒为 1，dialnorm_bits 不传输。
check "dialnorm 帧数" "0" "${OURS_AUD_DIALNORM}"
row "substream 响度帧数" "${OURS_AUD_LOUDNESS}"
echo

# --- ffprobe ---
echo "ffprobe:"
if ffprobe_json="$("${FFPROBE_BIN}" -v error -select_streams a:0 \
        -show_entries stream=nb_frames,sample_rate -of json "${INPUT}")" \
    && ffprobe_fields="$(printf '%s' "${ffprobe_json}" | "${PYTHON_BIN}" -c '
import json, sys
streams = json.load(sys.stdin).get("streams", [])
if not streams:
    raise SystemExit(1)
s = streams[0]
print(s.get("nb_frames", ""), s.get("sample_rate", ""))
')"; then
    read -r ffprobe_frames ffprobe_rate <<<"${ffprobe_fields}"
    check "sample 数" "${OURS_SAMPLES}" "${ffprobe_frames}"
    check "采样率" "${OURS_TS}" "${ffprobe_rate}"
else
    tool_failed "ffprobe"
fi
echo

# --- Bento4 ---
echo "Bento4 mp4info:"
if info="$("${MP4INFO_BIN}" "${INPUT}" 2>/dev/null)"; then
    check "sample 数" "${OURS_SAMPLES}" \
        "$(printf '%s' "${info}" | awk '/sample count:/ {print $3; exit}')"
    check "媒体时长" "${OURS_MEDIA}" \
        "$(printf '%s' "${info}" | awk '/duration:.*media timescale/ {print $2; exit}')"
    codec="$(printf '%s' "${info}" | awk '/Codec String:/ {print $3; exit}')"
    # RFC 6381 形如 ac-4.<bitstream_version>.<presentation_version>.<mdcompat>
    check "codec string 中的 bsv" "${OURS_BSV}" \
        "$(printf '%s' "${codec}" | awk -F. 'NF > 1 {printf "%d", $2}')"
    # Channel-based 的 DSI presentation v2 当前保持不透明，继续以 TOC 值
    # 核对 Bento4；已覆盖路径则先核对本项目的 DSI，再由首帧比较闭合 TOC。
    codec_pres_ver="${OURS_DSI_PRES_VER}"
    codec_md_compat="${OURS_DSI_MD_COMPAT}"
    if [[ "${OURS_SCENE_PATH}" == "channel_based" ]]; then
        codec_pres_ver="${OURS_PRES_VER}"
        codec_md_compat="${OURS_MD_COMPAT}"
    fi
    check "codec string 中的 pres_ver" "${codec_pres_ver}" \
        "$(printf '%s' "${codec}" | awk -F. 'NF > 2 {printf "%d", $3}')"
    check "codec string 中的 mdcompat" "${codec_md_compat}" \
        "$(printf '%s' "${codec}" | awk -F. 'NF > 3 {printf "%d", $4}')"
else
    tool_failed "Bento4 mp4info"
fi
echo

# --- GPAC ---
if (( have_mp4box == 1 )); then
    echo "GPAC MP4Box（额外核对）:"
    if info="$("${MP4BOX_BIN}" -info "${INPUT}" 2>&1)"; then
        check "sample 数" "${OURS_SAMPLES}" \
            "$(printf '%s' "${info}" | awk '/Media Samples:/ {print $3; exit}')"
        check "最大 sample 时长" "${OURS_MAX_DELTA}" \
            "$(printf '%s' "${info}" | awk '/Max sample duration:/ {print $4; exit}')"
    else
        tool_failed "GPAC MP4Box"
    fi
    echo
else
    echo "GPAC MP4Box：未安装，跳过额外核对"
    echo
fi

# --- MediaInfo ---
echo "MediaInfo:"
if mediainfo_json="$("${MEDIAINFO_BIN}" --Output=JSON "${INPUT}" 2>/dev/null)" \
    && mediainfo_fields="$(printf '%s' "${mediainfo_json}" | \
        "${PYTHON_BIN}" -c '
import json, sys
timescale = int(sys.argv[1])
presented = int(sys.argv[2])
tracks = json.load(sys.stdin).get("media", {}).get("track", [])
audio = next((t for t in tracks if t.get("@type") == "Audio"), None)
if audio is None:
    raise SystemExit(1)
# MediaInfo 的 Duration 只有毫秒精度。拿它与采样精确值直接比较，会在时长
# 不是整毫秒时误报——例如 81 920 采样 = 1 706.67 ms，回算得 81 936。
# 因此两侧统一化到毫秒再比。
duration_ms = round(float(audio.get("Duration", "nan")) * 1000)
ours_ms = round(presented / timescale * 1000)
print(
    audio.get("Format", ""), audio.get("SamplingRate", ""),
    duration_ms, ours_ms, audio.get("FrameCount", "")
)
' "${OURS_TS}" "${OURS_PRESENTED}")"; then
    read -r mediainfo_format mediainfo_rate mediainfo_duration ours_duration_ms \
        mediainfo_frames <<<"${mediainfo_fields}"
    check "格式" "AC-4" "${mediainfo_format}"
    check "采样率" "${OURS_TS}" "${mediainfo_rate}"
    # movie timescale 到毫秒的二次量化最多相差 1 ms；GPAC 的 6.016 s
    # 媒体时长在 600 Hz movie timescale 中即表现为 6.015 s。
    check_tolerance "呈现时长（ms）" "${ours_duration_ms}" "${mediainfo_duration}" 1
    check "呈现区间帧数" "${OURS_PRESENTED_FRAMES}" "${mediainfo_frames}"
else
    tool_failed "MediaInfo"
fi
echo

if (( mismatch > 0 )); then
    echo "结果：${mismatch} 项失败"
    exit 1
fi
echo "结果：M1/M2/M3/M4 必需字段全部一致"
