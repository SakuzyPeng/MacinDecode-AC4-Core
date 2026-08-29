#!/usr/bin/env bash
# 由 case.json 跑完整条测试向量生产链。
#
#   ./scripts/build_vector.sh vectors/<case_id>/case.json
#   ./scripts/build_vector.sh --profile all vectors/<case_id>/case.json
#
# 标准作业：gen_damf.py -> ADM 规范化 -> AC-4 编码（按 encodes 逐档）。
# 可选 DME A-JOC 作业：普通模式读取 ADM BWF；3DoF 模式读取隔离的 DAMF 0.6.0，
# 两者都输出 raw AC-4 + timing manifest，再交给官方 MP4 muxer。
# 可选 DME native 作业：从纯 bed 信号配方生成 speaker WAVE 后编码 channel-based
# AC-4 / IMS，或把 canonical DAMF 直接交给 native IMS encoder。
# 可选 DEE IMS 作业：gen_damf.py -> DEE staging -> raw AC-4 -> MP4 封装。
# 各条链最终都校验为恰含一条音频轨，并移除编码器可能附带的辅助视频轨。
# 工具路径优先取自 .env.local；FFPROBE/MP4BOX 未配置时按 check_tools.sh 的
# 约定从 PATH 查找。
#
# 标准编码档位由 case.json 的 encodes 数组给出；DME 与 IMS 作业分别由
# dme_ac4/dme_channel/dme_ims/dee_ims 数组给出。profile 默认为 default，保持
# 既有生产行为；其他 profile 必须显式选择。已存在的产物默认跳过，加 --force
# 重新生成。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${MACINAC4_ENV_FILE:-${REPO_ROOT}/.env.local}"
PYTHON_BIN="${PYTHON_BIN:-python3}"

if [ ! -f "${ENV_FILE}" ]; then
    echo "缺少 ${ENV_FILE}，请先执行 cp .env.local.example .env.local" >&2
    exit 1
fi
set -a
# shellcheck disable=SC1090
source "${ENV_FILE}"
set +a

FORCE=0
BUILD_PROFILE="default"
CASE=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --force) FORCE=1; shift ;;
        --profile)
            if [ "$#" -lt 2 ]; then
                echo "--profile 缺少参数" >&2
                exit 2
            fi
            BUILD_PROFILE="$2"
            shift 2
            ;;
        --profile=*)
            BUILD_PROFILE="${1#--profile=}"
            shift
            ;;
        -*)
            echo "未知参数：$1" >&2
            exit 2
            ;;
        *)
            if [ -n "${CASE}" ]; then
                echo "只能指定一个 case.json" >&2
                exit 2
            fi
            CASE="$1"
            shift
            ;;
    esac
done

if [ "${BUILD_PROFILE}" != "default" ] \
    && [ "${BUILD_PROFILE}" != "dme_ac4" ] \
    && [ "${BUILD_PROFILE}" != "dme_native" ] \
    && [ "${BUILD_PROFILE}" != "dee_ims" ] \
    && [ "${BUILD_PROFILE}" != "all" ]; then
    echo "--profile 必须是 default、dme_ac4、dme_native、dee_ims 或 all" >&2
    exit 2
fi

if [ -z "${CASE}" ] || [ ! -f "${CASE}" ]; then
    echo "用法：$0 [--force] [--profile default|dme_ac4|dme_native|dee_ims|all] vectors/<case_id>/case.json" >&2
    exit 2
fi

FFPROBE_BIN="${FFPROBE:-}"
if [ -z "${FFPROBE_BIN}" ]; then
    FFPROBE_BIN="$(command -v ffprobe 2>/dev/null || true)"
fi
if [ -z "${FFPROBE_BIN}" ] || [ ! -x "${FFPROBE_BIN}" ]; then
    echo "FFPROBE 未配置且 PATH 中找不到 ffprobe，无法校验编码容器" >&2
    exit 1
fi

MP4BOX_BIN="${MP4BOX:-}"
if [ -z "${MP4BOX_BIN}" ]; then
    MP4BOX_BIN="$(command -v MP4Box 2>/dev/null || true)"
fi

stream_counts() {
    local target="$1" probe
    if ! probe="$("${FFPROBE_BIN}" -v error \
        -show_entries stream=codec_type -of json "${target}")"; then
        echo "ffprobe 无法读取编码产物：${target}" >&2
        return 1
    fi
    "${PYTHON_BIN}" -c '
import json, sys
types = [stream.get("codec_type") for stream in json.load(sys.stdin)["streams"]]
print(types.count("audio"), types.count("video"),
      sum(kind not in ("audio", "video") for kind in types))
' <<<"${probe}"
}

strip_auxiliary_video_tracks() {
    local target="$1" counts audio_count video_count other_count tmp
    counts="$(stream_counts "${target}")"
    read -r audio_count video_count other_count <<<"${counts}"

    if [ "${audio_count}" -ne 1 ] || [ "${other_count}" -ne 0 ]; then
        echo "编码产物必须恰含一条音频轨且不能含未知轨：${target}" >&2
        return 1
    fi
    if [ "${video_count}" -eq 0 ]; then
        return 0
    fi
    if [ -z "${MP4BOX_BIN}" ] || [ ! -x "${MP4BOX_BIN}" ]; then
        echo "编码器附带了 ${video_count} 条视频轨；请配置 MP4BOX 后重跑，" >&2
        echo "只需重封装现有文件，不需要重新编码。" >&2
        return 1
    fi

    echo "移除 $(basename "${target}") 的 ${video_count} 条辅助视频轨"
    while [ "${video_count}" -gt 0 ]; do
        tmp="${target}.audio-only.$$.m4a"
        rm -f "${tmp}"
        if ! "${MP4BOX_BIN}" -rem video -rb avc1 \
            "${target}" -out "${tmp}" >/dev/null 2>&1; then
            rm -f "${tmp}"
            echo "MP4Box 移除辅助视频轨失败：${target}" >&2
            return 1
        fi
        mv "${tmp}" "${target}"
        counts="$(stream_counts "${target}")"
        read -r audio_count video_count other_count <<<"${counts}"
    done

    if [ "${audio_count}" -ne 1 ] || [ "${other_count}" -ne 0 ]; then
        echo "重封装后的轨道布局不合法：${target}" >&2
        return 1
    fi
}

CASE_DIR="$(cd "$(dirname "${CASE}")" && pwd)"
CASE_ID="$("${PYTHON_BIN}" -c 'import json,sys; print(json.load(open(sys.argv[1]))["case_id"])' "${CASE}")"
FPS="$("${PYTHON_BIN}" -c 'import json,sys; print(json.load(open(sys.argv[1]))["frame_rate"])' "${CASE}")"
SAMPLE_RATE="$("${PYTHON_BIN}" -c 'import json,sys; print(json.load(open(sys.argv[1]))["sample_rate"])' "${CASE}")"
DURATION_SAMPLES="$("${PYTHON_BIN}" -c 'import json,sys; print(json.load(open(sys.argv[1]))["duration_samples"])' "${CASE}")"
mapfile -t ALL_BITRATES < <("${PYTHON_BIN}" -c '
import json, sys
for value in json.load(open(sys.argv[1]))["encodes"]:
    print(value)
' "${CASE}")

IMS_LIST="$("${PYTHON_BIN}" "${REPO_ROOT}/scripts/dee_ims.py" list "${CASE}")" || exit $?
ALL_IMS_JOBS=()
if [ -n "${IMS_LIST}" ]; then
    mapfile -t ALL_IMS_JOBS <<<"${IMS_LIST}"
fi

DME_LIST="$("${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_ac4.py" list "${CASE}")" || exit $?
ALL_DME_JOBS=()
if [ -n "${DME_LIST}" ]; then
    mapfile -t ALL_DME_JOBS <<<"${DME_LIST}"
fi

DME_CHANNEL_LIST="$("${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_native.py" list-channel "${CASE}")" || exit $?
ALL_DME_CHANNEL_JOBS=()
if [ -n "${DME_CHANNEL_LIST}" ]; then
    mapfile -t ALL_DME_CHANNEL_JOBS <<<"${DME_CHANNEL_LIST}"
fi

DME_IMS_LIST="$("${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_native.py" list-ims "${CASE}")" || exit $?
ALL_DME_IMS_JOBS=()
if [ -n "${DME_IMS_LIST}" ]; then
    mapfile -t ALL_DME_IMS_JOBS <<<"${DME_IMS_LIST}"
fi

BITRATES=()
DME_JOBS=()
DME_CHANNEL_JOBS=()
DME_IMS_JOBS=()
IMS_JOBS=()
if [ "${BUILD_PROFILE}" = "default" ] || [ "${BUILD_PROFILE}" = "all" ]; then
    BITRATES=("${ALL_BITRATES[@]}")
fi
if [ "${BUILD_PROFILE}" = "dme_ac4" ] || [ "${BUILD_PROFILE}" = "all" ]; then
    DME_JOBS=("${ALL_DME_JOBS[@]}")
fi
if [ "${BUILD_PROFILE}" = "dme_native" ] || [ "${BUILD_PROFILE}" = "all" ]; then
    DME_CHANNEL_JOBS=("${ALL_DME_CHANNEL_JOBS[@]}")
    DME_IMS_JOBS=("${ALL_DME_IMS_JOBS[@]}")
fi
if [ "${BUILD_PROFILE}" = "dee_ims" ] || [ "${BUILD_PROFILE}" = "all" ]; then
    IMS_JOBS=("${ALL_IMS_JOBS[@]}")
fi

DME_NEEDS_NORMALIZED=0
for job in "${DME_JOBS[@]}"; do
    IFS=$'\t' read -r _level _bitrate mode _filename <<<"${job}"
    if [ "${mode}" = "general" ]; then
        DME_NEEDS_NORMALIZED=1
    fi
done

if [ "${#BITRATES[@]}" -eq 0 ] \
    && [ "${#DME_JOBS[@]}" -eq 0 ] \
    && [ "${#DME_CHANNEL_JOBS[@]}" -eq 0 ] \
    && [ "${#DME_IMS_JOBS[@]}" -eq 0 ] \
    && [ "${#IMS_JOBS[@]}" -eq 0 ]; then
    echo "case.json 没有为 ${BUILD_PROFILE} profile 声明作业" >&2
    exit 2
fi

if [ "${#BITRATES[@]}" -gt 0 ] || [ "${DME_NEEDS_NORMALIZED}" -eq 1 ]; then
    if [ -z "${ADM_NORMALIZER:-}" ] || [ ! -x "${ADM_NORMALIZER}" ]; then
        echo "ADM_NORMALIZER 未配置或不可执行，请先运行 ./scripts/check_tools.sh" >&2
        exit 1
    fi
fi

if [ "${#BITRATES[@]}" -gt 0 ]; then
    if [ -z "${AC4_ENCODER:-}" ] || [ ! -x "${AC4_ENCODER}" ]; then
        echo "AC4_ENCODER 未配置或不可执行，请先运行 ./scripts/check_tools.sh" >&2
        exit 1
    fi
fi

if [ "${#DME_JOBS[@]}" -gt 0 ]; then
    for var in DME_AC4_AJOC_ENCODER DME_MP4MUXER; do
        if [ -z "${!var:-}" ] || [ ! -x "${!var}" ]; then
            echo "${var} 未配置或不可执行，DME A-JOC 编码不可用" >&2
            exit 1
        fi
    done
fi

if [ "${#DME_CHANNEL_JOBS[@]}" -gt 0 ] || [ "${#DME_IMS_JOBS[@]}" -gt 0 ]; then
    if [ -z "${DME_MP4MUXER:-}" ] || [ ! -x "${DME_MP4MUXER}" ]; then
        echo "DME_MP4MUXER 未配置或不可执行，DME native 封装不可用" >&2
        exit 1
    fi
fi
if [ "${#DME_CHANNEL_JOBS[@]}" -gt 0 ]; then
    if [ -z "${DME_AC4_ENCODER:-}" ] || [ ! -x "${DME_AC4_ENCODER}" ]; then
        echo "DME_AC4_ENCODER 未配置或不可执行，DME channel-based 编码不可用" >&2
        exit 1
    fi
fi
if [ "${#DME_IMS_JOBS[@]}" -gt 0 ]; then
    if [ -z "${DME_AC4_IMS_ENCODER:-}" ] || [ ! -x "${DME_AC4_IMS_ENCODER}" ]; then
        echo "DME_AC4_IMS_ENCODER 未配置或不可执行，DME native IMS 编码不可用" >&2
        exit 1
    fi
fi

if [ "${#IMS_JOBS[@]}" -gt 0 ]; then
    if [ -z "${DEE_ENCODER:-}" ] || [ ! -x "${DEE_ENCODER}" ]; then
        echo "DEE_ENCODER 未配置或不可执行，DEE IMS 编码不可用" >&2
        exit 1
    fi
    if [ -z "${MP4BOX_BIN}" ] || [ ! -x "${MP4BOX_BIN}" ]; then
        echo "MP4BOX 未配置且 PATH 中找不到 MP4Box，DEE IMS 封装不可用" >&2
        exit 1
    fi
    for var in DEE_ENGINE_BINARY DEE_IMS_TEMPLATE; do
        if [ -z "${!var:-}" ] || [ ! -f "${!var}" ]; then
            echo "${var} 未配置或不是普通文件，无法固定 DEE IMS 指纹" >&2
            exit 1
        fi
    done
    if [ -z "${DEE_WORKSPACE_ROOT:-}" ] || [ ! -d "${DEE_WORKSPACE_ROOT}" ]; then
        echo "DEE_WORKSPACE_ROOT 未配置或不是目录" >&2
        exit 1
    fi
    if [ ! -w "${DEE_WORKSPACE_ROOT}" ]; then
        echo "DEE_WORKSPACE_ROOT 不可写，无法创建隔离 staging" >&2
        exit 1
    fi
    DEE_WORKSPACE_ROOT="$(cd "${DEE_WORKSPACE_ROOT}" && pwd)"
    DEE_WORKSPACE_DRIVE="${DEE_WORKSPACE_DRIVE:-y:}"
    # 提前验证模板确实位于包装器可见的工作区。
    "${PYTHON_BIN}" "${REPO_ROOT}/scripts/dee_ims.py" workspace-path \
        "${DEE_WORKSPACE_ROOT}" "${DEE_WORKSPACE_DRIVE}" "${DEE_IMS_TEMPLATE}" >/dev/null
fi

echo "案例：${CASE_ID}（${FPS} fps，profile=${BUILD_PROFILE}）"
if [ "${#BITRATES[@]}" -gt 0 ]; then
    echo "标准档位：${BITRATES[*]} kbps"
fi
if [ "${#DME_JOBS[@]}" -gt 0 ]; then
    echo "DME A-JOC 作业：${#DME_JOBS[@]} 个"
fi
if [ "${#DME_CHANNEL_JOBS[@]}" -gt 0 ]; then
    echo "DME channel-based 作业：${#DME_CHANNEL_JOBS[@]} 个"
fi
if [ "${#DME_IMS_JOBS[@]}" -gt 0 ]; then
    echo "DME native IMS 作业：${#DME_IMS_JOBS[@]} 个"
fi
if [ "${#IMS_JOBS[@]}" -gt 0 ]; then
    echo "DEE IMS 作业：${#IMS_JOBS[@]} 个"
fi

# --- 1. DAMF ---
"${REPO_ROOT}/scripts/gen_damf.py" "${CASE}"

# --- 2. ADM 规范化 ---
NORMALIZED="${CASE_DIR}/normalized"
if [ "${#BITRATES[@]}" -gt 0 ] || [ "${DME_NEEDS_NORMALIZED}" -eq 1 ]; then
    mkdir -p "${NORMALIZED}"
    if [ "${FORCE}" -eq 1 ] || [ ! -f "${NORMALIZED}/output.wav" ]; then
        echo "规范化 -> ${NORMALIZED}/output.wav"
        "${ADM_NORMALIZER}" -i "${CASE_DIR}/source/master.atmos" \
            -o "${NORMALIZED}" -f wav --target_fps "${FPS}" >/dev/null
    else
        echo "跳过规范化（已存在）"
    fi
fi

# --- 3. 标准编码 ---
ENCODED="${CASE_DIR}/encoded"
mkdir -p "${ENCODED}"
for bitrate in "${BITRATES[@]}"; do
    target="${ENCODED}/master_ac4_${bitrate}K.m4a"
    if [ "${FORCE}" -eq 0 ] && [ -f "${target}" ]; then
        echo "跳过 ${bitrate} kbps（已存在）"
    else
        echo "编码 ${bitrate} kbps -> $(basename "${target}")"
        "${AC4_ENCODER}" encode --codec ac4 --bitrate "${bitrate}" \
            --overwrite -o "${target}" "${NORMALIZED}/output.wav" >/dev/null
    fi
    strip_auxiliary_video_tracks "${target}"
done

# --- 4. 可选后端的隔离 staging ---
DEE_JOB_ROOT=""
DEE_MUX_TARGET=""
DME_JOB_ROOT=""
cleanup_dee_staging() {
    if [ -n "${DEE_MUX_TARGET}" ] && [ -f "${DEE_MUX_TARGET}" ]; then
        case "${DEE_MUX_TARGET}" in
            "${ENCODED}"/master_ac4_ims*.tmp.*.m4a)
                rm -f "${DEE_MUX_TARGET}"
                ;;
            *)
                echo "拒绝清理无法确认的 DEE 临时封装：${DEE_MUX_TARGET}" >&2
                ;;
        esac
    fi
    if [ -z "${DEE_JOB_ROOT}" ] || [ ! -d "${DEE_JOB_ROOT}" ]; then
        return 0
    fi
    case "${DEE_JOB_ROOT}" in
        "${DEE_WORKSPACE_ROOT}"/tmp_macinac4_ims.*)
            find "${DEE_JOB_ROOT}" -depth -delete
            ;;
        *)
            echo "拒绝清理无法确认的 DEE staging：${DEE_JOB_ROOT}" >&2
            ;;
    esac
}

cleanup_dme_staging() {
    if [ -z "${DME_JOB_ROOT}" ] || [ ! -d "${DME_JOB_ROOT}" ]; then
        return 0
    fi
    case "${DME_JOB_ROOT}" in
        "${ENCODED}"/.tmp_macinac4_dme.*)
            find "${DME_JOB_ROOT}" -depth -delete
            ;;
        *)
            echo "拒绝清理无法确认的 DME staging：${DME_JOB_ROOT}" >&2
            ;;
    esac
}

cleanup_production_staging() {
    local exit_status=$? dee_status dme_status
    # EXIT trap 继承触发它的失败状态；先关闭 errexit，确保两条后端都得到清理机会。
    set +e
    cleanup_dee_staging
    dee_status=$?
    cleanup_dme_staging
    dme_status=$?
    if [ "${exit_status}" -ne 0 ]; then
        exit "${exit_status}"
    fi
    if [ "${dee_status}" -ne 0 ] || [ "${dme_status}" -ne 0 ]; then
        exit 1
    fi
    exit 0
}
trap cleanup_production_staging EXIT

file_size() {
    local path="$1"
    if [ "$(uname -s)" = "Darwin" ]; then
        stat -f%z "${path}"
    else
        stat -c%s "${path}"
    fi
}

dee_workspace_path() {
    "${PYTHON_BIN}" "${REPO_ROOT}/scripts/dee_ims.py" workspace-path \
        "${DEE_WORKSPACE_ROOT}" "${DEE_WORKSPACE_DRIVE}" "$1"
}

ensure_dee_staging() {
    if [ -n "${DEE_JOB_ROOT}" ]; then
        return 0
    fi
    DEE_JOB_ROOT="$(mktemp -d "${DEE_WORKSPACE_ROOT}/tmp_macinac4_ims.XXXXXX")"
    mkdir -p "${DEE_JOB_ROOT}/input" "${DEE_JOB_ROOT}/jobs" \
        "${DEE_JOB_ROOT}/output" "${DEE_JOB_ROOT}/temp"
    cp "${CASE_DIR}/source/master.atmos"* "${DEE_JOB_ROOT}/input/"
}

# --- 5. DEE IMS 编码 ---
for job in "${IMS_JOBS[@]}"; do
    IFS=$'\t' read -r bitrate profile legacy filename <<<"${job}"
    target="${ENCODED}/${filename}"
    if [ "${FORCE}" -eq 0 ] && [ -f "${target}" ]; then
        echo "跳过 DEE IMS ${bitrate} kbps（已存在：${filename}）"
        strip_auxiliary_video_tracks "${target}"
    else
        ensure_dee_staging
        stem="${filename%.m4a}"
        job_xml="${DEE_JOB_ROOT}/jobs/${stem}.xml"
        raw_output="${DEE_JOB_ROOT}/output/${stem}.ac4"
        job_temp="${DEE_JOB_ROOT}/temp/${stem}"
        job_log="${DEE_JOB_ROOT}/output/${stem}.log"
        mkdir -p "${job_temp}"

        "${PYTHON_BIN}" "${REPO_ROOT}/scripts/dee_ims.py" render \
            "${DEE_IMS_TEMPLATE}" "${job_xml}" \
            --bitrate "${bitrate}" --encoding-profile "${profile}" \
            --legacy-presentation "${legacy}"

        echo "DEE IMS ${bitrate} kbps (${profile}, legacy=${legacy}) -> ${filename}"
        "${DEE_ENCODER}" \
            --xml "$(dee_workspace_path "${job_xml}")" \
            --input-audio "$(dee_workspace_path "${DEE_JOB_ROOT}/input/master.atmos")" \
            --output "$(dee_workspace_path "${raw_output}")" \
            --temp "$(dee_workspace_path "${job_temp}")" \
            --log-file "$(dee_workspace_path "${job_log}")" \
            --stdout --verbose info >/dev/null

        if [ ! -s "${raw_output}" ]; then
            echo "DEE 未生成有效 raw AC-4：${raw_output}" >&2
            exit 1
        fi

        DEE_MUX_TARGET="${target%.m4a}.tmp.$$.m4a"
        rm -f "${DEE_MUX_TARGET}"
        if ! "${MP4BOX_BIN}" -quiet -add "${raw_output}" \
            -new "${DEE_MUX_TARGET}"; then
            rm -f "${DEE_MUX_TARGET}"
            DEE_MUX_TARGET=""
            echo "GPAC 无法封装 DEE raw AC-4：${filename}" >&2
            exit 1
        fi
        if ! strip_auxiliary_video_tracks "${DEE_MUX_TARGET}"; then
            rm -f "${DEE_MUX_TARGET}"
            DEE_MUX_TARGET=""
            echo "DEE M4A 校验失败，保留既有产物：${filename}" >&2
            exit 1
        fi
        mv "${DEE_MUX_TARGET}" "${target}"
        DEE_MUX_TARGET=""
    fi
done

# --- 6. DME A-JOC 编码 ---
ensure_dme_staging() {
    if [ -n "${DME_JOB_ROOT}" ]; then
        return 0
    fi
    DME_JOB_ROOT="$(mktemp -d "${ENCODED}/.tmp_macinac4_dme.XXXXXX")"
}

DME_3DOF_INPUT=""
prepare_dme_3dof_input() {
    local suffix source_path
    if [ -n "${DME_3DOF_INPUT}" ]; then
        return 0
    fi
    ensure_dme_staging
    mkdir -p "${DME_JOB_ROOT}/input"
    for suffix in audio metadata; do
        source_path="${CASE_DIR}/source/master.atmos.${suffix}"
        if [ ! -f "${source_path}" ]; then
            echo "3DoF DAMF 缺少 ${source_path}" >&2
            exit 1
        fi
        ln -s "${source_path}" "${DME_JOB_ROOT}/input/master.atmos.${suffix}"
    done
    DME_3DOF_INPUT="${DME_JOB_ROOT}/input/master.atmos"
    "${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_ac4.py" prepare-3dof \
        "${CASE_DIR}/source/master.atmos" "${DME_3DOF_INPUT}"
}

for job in "${DME_JOBS[@]}"; do
    IFS=$'\t' read -r level bitrate mode filename <<<"${job}"
    target="${ENCODED}/${filename}"
    if [ "${FORCE}" -eq 0 ] && [ -f "${target}" ]; then
        echo "跳过 DME L${level} ${bitrate} kbps (${mode})（已存在：${filename}）"
        strip_auxiliary_video_tracks "${target}"
        continue
    fi

    ensure_dme_staging
    stem="${filename%.m4a}"
    raw_output="${DME_JOB_ROOT}/${stem}.ac4"
    timing_manifest="${DME_JOB_ROOT}/${stem}.json"
    mux_output="${DME_JOB_ROOT}/${stem}.m4a"

    if [ "${mode}" = "3dof" ]; then
        prepare_dme_3dof_input
        encoder_input="${DME_3DOF_INPUT}"
    else
        encoder_input="${NORMALIZED}/output.wav"
    fi

    echo "DME A-JOC L${level} ${bitrate} kbps (${mode}) -> ${filename}"
    "${DME_AC4_AJOC_ENCODER}" \
        --overwrite 1 --progress 0 --cc 0 --loglevel warning \
        --start 0 --time-base file_position \
        --level "${level}" --data-rate "${bitrate}" \
        --encoder mode="${mode}":drc_profile=film_light:iframe_interval=1sec \
        --loudness-management measure_only \
        --input "${encoder_input}" \
        --output "${raw_output}" --output-manifest "${timing_manifest}" >/dev/null

    if [ ! -s "${raw_output}" ]; then
        echo "DME 未生成有效 raw AC-4：${raw_output}" >&2
        exit 1
    fi
    track_options="$("${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_ac4.py" \
        track-options "${timing_manifest}" "${raw_output}" \
        --expected-duration "${DURATION_SAMPLES}")"

    "${DME_MP4MUXER}" --overwrite 1 --loglevel warning \
        --time-scale "${SAMPLE_RATE}" --track "${raw_output}" \
        --track-options "${track_options}" --output "${mux_output}" >/dev/null
    if ! strip_auxiliary_video_tracks "${mux_output}"; then
        echo "DME M4A 校验失败，保留既有产物：${filename}" >&2
        exit 1
    fi
    mv "${mux_output}" "${target}"
done

# --- 7. DME channel-based / native IMS 编码 ---
DME_NATIVE_WAVE=""
prepare_dme_native_wave() {
    local layout="$1" token
    ensure_dme_staging
    token="${layout//./_}"
    DME_NATIVE_WAVE="${DME_JOB_ROOT}/input_${token}.wav"
    if [ ! -s "${DME_NATIVE_WAVE}" ]; then
        "${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_native.py" prepare-wave \
            "${CASE}" "${DME_NATIVE_WAVE}" --layout "${layout}"
    fi
}

for job in "${DME_CHANNEL_JOBS[@]}"; do
    IFS=$'\t' read -r layout bitrate input_format filename <<<"${job}"
    target="${ENCODED}/${filename}"
    if [ "${FORCE}" -eq 0 ] && [ -f "${target}" ]; then
        echo "跳过 DME channel ${layout} ${bitrate} kbps（已存在：${filename}）"
        strip_auxiliary_video_tracks "${target}"
        continue
    fi

    prepare_dme_native_wave "${layout}"
    stem="${filename%.m4a}"
    raw_output="${DME_JOB_ROOT}/${stem}.ac4"
    timing_manifest="${DME_JOB_ROOT}/${stem}.json"
    mux_output="${DME_JOB_ROOT}/${stem}.m4a"

    echo "DME channel ${layout} ${bitrate} kbps -> ${filename}"
    "${DME_AC4_ENCODER}" \
        --overwrite 1 --progress 0 --cc 0 --loglevel warning \
        --start 0 --time-base file_position \
        --input-format "${input_format}" --input "${DME_NATIVE_WAVE}" \
        --output-channel-layout "${layout}" --data-rate "${bitrate}" \
        --encoder drc_profile=film_light:iframe_interval=1sec \
        --loudness-management measure_only \
        --output "${raw_output}" --output-manifest "${timing_manifest}" >/dev/null

    if [ ! -s "${raw_output}" ]; then
        echo "DME channel encoder 未生成有效 raw AC-4：${raw_output}" >&2
        exit 1
    fi
    track_options="$("${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_ac4.py" \
        track-options "${timing_manifest}" "${raw_output}" \
        --expected-duration "${DURATION_SAMPLES}")"
    "${DME_MP4MUXER}" --overwrite 1 --loglevel warning \
        --time-scale "${SAMPLE_RATE}" --track "${raw_output}" \
        --track-options "${track_options}" --output "${mux_output}" >/dev/null
    if ! strip_auxiliary_video_tracks "${mux_output}"; then
        echo "DME channel M4A 校验失败，保留既有产物：${filename}" >&2
        exit 1
    fi
    mv "${mux_output}" "${target}"
done

for job in "${DME_IMS_JOBS[@]}"; do
    IFS=$'\t' read -r input_kind mode bitrate input_format drc_profile target_fps loudness filename <<<"${job}"
    target="${ENCODED}/${filename}"
    if [ "${FORCE}" -eq 0 ] && [ -f "${target}" ]; then
        echo "跳过 DME native IMS ${mode}/${input_kind} ${bitrate} kbps（已存在：${filename}）"
        strip_auxiliary_video_tracks "${target}"
        continue
    fi

    ensure_dme_staging
    if [ "${input_kind}" = "wav_5_1" ]; then
        prepare_dme_native_wave "5.1"
        encoder_input="${DME_NATIVE_WAVE}"
    else
        encoder_input="${CASE_DIR}/source/master.atmos"
    fi
    stem="${filename%.m4a}"
    raw_output="${DME_JOB_ROOT}/${stem}.ac4"
    mux_output="${DME_JOB_ROOT}/${stem}.m4a"

    echo "DME native IMS ${mode}/${input_kind} ${bitrate} kbps -> ${filename}"
    "${DME_AC4_IMS_ENCODER}" \
        --overwrite 1 --progress 0 --cc 0 --loglevel warning \
        --start 0 --time-base file_position \
        --input-format "${input_format}" --input "${encoder_input}" \
        --data-rate "${bitrate}" --target-fps "${target_fps}" \
        --encoder "mode=${mode}:drc_profile=${drc_profile}:iframe_interval=24" \
        --loudness-management "${loudness}" --output "${raw_output}" >/dev/null

    if [ ! -s "${raw_output}" ]; then
        echo "DME native IMS encoder 未生成有效 raw AC-4：${raw_output}" >&2
        exit 1
    fi
    track_options="$("${PYTHON_BIN}" "${REPO_ROOT}/scripts/dme_native.py" \
        ims-track-options --expected-duration "${DURATION_SAMPLES}" --mode "${mode}")"
    "${DME_MP4MUXER}" --overwrite 1 --loglevel warning \
        --time-scale "${SAMPLE_RATE}" --track "${raw_output}" \
        --track-options "${track_options}" --output "${mux_output}" >/dev/null
    if ! strip_auxiliary_video_tracks "${mux_output}"; then
        echo "DME native IMS M4A 校验失败，保留既有产物：${filename}" >&2
        exit 1
    fi
    mv "${mux_output}" "${target}"
done

cleanup_dme_staging
DME_JOB_ROOT=""

# 编码器可能在输出目录留下自己的中间目录，不属于向量产物。这里按「隐藏目录」
# 通用清理，不写死任何外部工具的命名。
find "${ENCODED}" -maxdepth 1 -type d -name '.*' ! -path "${ENCODED}" -exec rm -rf {} + 2>/dev/null || true

echo
echo "产物："
for path in "${CASE_DIR}"/source/* "${NORMALIZED}"/*.wav "${ENCODED}"/*.m4a; do
    [ -f "${path}" ] || continue
    printf '  %-34s %12d B\n' "${path#"${CASE_DIR}"/}" "$(file_size "${path}")"
done
