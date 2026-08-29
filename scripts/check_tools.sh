#!/usr/bin/env bash
#
# 校验测试向量生产链所需的外部工具，并输出 provenance fingerprint。
#
#   ./scripts/check_tools.sh          人类可读报告
#   ./scripts/check_tools.sh --json [--profile default|dme_ac4|dee_ims|all]
#                                      provenance.json 的 tools 片段
#
# 退出码 0 表示必需工具齐备；1 表示存在缺失或配置错误。
# 外部工具按不透明黑盒对待：本脚本只校验可达性并记录指纹，不调用它们。

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${MACINAC4_ENV_FILE:-${REPO_ROOT}/.env.local}"
PYTHON_BIN="${PYTHON_BIN:-python3}"

JSON_OUTPUT=0
PROFILE="default"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --json)
            JSON_OUTPUT=1
            shift
            ;;
        --profile)
            if [[ $# -lt 2 ]]; then
                echo "--profile 缺少参数" >&2
                exit 2
            fi
            PROFILE="$2"
            shift 2
            ;;
        *)
            echo "未知参数：$1" >&2
            exit 2
            ;;
    esac
done

need_default=0
need_dme=0
need_dme_native=0
need_dee=0
if [[ "${PROFILE}" == "all" ]]; then
    profile_parts=(default dme_ac4 dme_native dee_ims)
else
    IFS='+' read -r -a profile_parts <<<"${PROFILE}"
fi
if [[ ${#profile_parts[@]} -eq 0 ]]; then
    echo "--profile 不能为空" >&2
    exit 2
fi
for part in "${profile_parts[@]}"; do
    case "${part}" in
        default)
            [[ ${need_default} -eq 0 ]] || {
                echo "--profile 含重复项：default" >&2
                exit 2
            }
            need_default=1
            ;;
        dme_ac4)
            [[ ${need_dme} -eq 0 ]] || {
                echo "--profile 含重复项：dme_ac4" >&2
                exit 2
            }
            need_dme=1
            ;;
        dme_native)
            [[ ${need_dme_native} -eq 0 ]] || {
                echo "--profile 含重复项：dme_native" >&2
                exit 2
            }
            need_dme_native=1
            ;;
        dee_ims)
            [[ ${need_dee} -eq 0 ]] || {
                echo "--profile 含重复项：dee_ims" >&2
                exit 2
            }
            need_dee=1
            ;;
        *)
            echo "--profile 必须由 default、dme_ac4、dme_native、dee_ims 以 + 组合，或使用 all" >&2
            exit 2
            ;;
    esac
done

errors=()
warnings=()

if [[ ! -f "${ENV_FILE}" ]]; then
    echo "缺少 ${ENV_FILE}" >&2
    echo "请先执行：cp .env.local.example .env.local 并填写本机路径" >&2
    exit 1
fi

set -a
# shellcheck disable=SC1090
source "${ENV_FILE}"
set +a

sha256_of() {
    shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1
}

# 校验一个可执行文件；缺失时按 required 决定是错误还是警告。
check_exe() {
    local var_name="$1" required="$2"
    local path="${!var_name:-}"

    if [[ -z "${path}" ]]; then
        if [[ "${required}" == "required" ]]; then
            errors+=("${var_name} 未设置")
        else
            warnings+=("${var_name} 未设置（可选）")
        fi
        return 1
    fi
    if [[ ! -e "${path}" ]]; then
        if [[ "${required}" == "required" ]]; then
            errors+=("${var_name} 指向的路径不存在：${path}")
        else
            warnings+=("${var_name} 指向的路径不存在：${path}")
        fi
        return 1
    fi
    if [[ ! -x "${path}" ]]; then
        errors+=("${var_name} 不可执行：${path}")
        return 1
    fi
    return 0
}

check_file() {
    local var_name="$1" required="$2"
    local path="${!var_name:-}"
    if [[ -z "${path}" ]]; then
        if [[ "${required}" == "required" ]]; then
            errors+=("${var_name} 未设置")
        else
            warnings+=("${var_name} 未设置（可选）")
        fi
        return 1
    fi
    if [[ ! -f "${path}" ]]; then
        if [[ "${required}" == "required" ]]; then
            errors+=("${var_name} 不是普通文件：${path}")
        else
            warnings+=("${var_name} 不是普通文件：${path}")
        fi
        return 1
    fi
    return 0
}

check_workspace() {
    local required="$1" path="${DEE_WORKSPACE_ROOT:-}"
    if [[ -z "${path}" ]]; then
        if [[ "${required}" == "required" ]]; then
            errors+=("DEE_WORKSPACE_ROOT 未设置")
        else
            warnings+=("DEE_WORKSPACE_ROOT 未设置（可选）")
        fi
        return 1
    fi
    if [[ ! -d "${path}" || ! -w "${path}" ]]; then
        if [[ "${required}" == "required" ]]; then
            errors+=("DEE_WORKSPACE_ROOT 必须是可写目录：${path}")
        else
            warnings+=("DEE_WORKSPACE_ROOT 不是可写目录：${path}")
        fi
        return 1
    fi
    return 0
}

# --- 采集 ---

encoder_sha=""; encoder_version=""; encoder_commit=""; encoder_dirty="null"
if [[ ${need_default} -eq 1 ]] && check_exe AC4_ENCODER required; then
    encoder_sha="$(sha256_of "${AC4_ENCODER}")"
    encoder_version="$("${AC4_ENCODER}" --version 2>/dev/null | head -1)"

    if [[ -n "${AC4_ENCODER_REPO:-}" ]]; then
        if [[ -d "${AC4_ENCODER_REPO}/.git" ]]; then
            encoder_commit="$(git -C "${AC4_ENCODER_REPO}" rev-parse HEAD 2>/dev/null)"
            if [[ -n "$(git -C "${AC4_ENCODER_REPO}" status --porcelain 2>/dev/null)" ]]; then
                encoder_dirty="true"
                warnings+=("编码器源仓库有未提交改动，provenance 的 commit 不足以复现该二进制")
            else
                encoder_dirty="false"
            fi

            # 可执行文件早于 HEAD，说明未按当前源码重新构建
            head_epoch="$(git -C "${AC4_ENCODER_REPO}" log -1 --format=%ct 2>/dev/null)"
            bin_epoch="$(stat -f %m "${AC4_ENCODER}" 2>/dev/null)"
            if [[ -n "${head_epoch}" && -n "${bin_epoch}" && "${bin_epoch}" -lt "${head_epoch}" ]]; then
                warnings+=("编码器可执行文件早于其源仓库 HEAD，可能需要重新构建")
            fi
        else
            warnings+=("AC4_ENCODER_REPO 未指向 git 仓库，无法记录 commit")
        fi
    fi
fi

normalizer_sha=""
if [[ ${need_default} -eq 1 || ${need_dme} -eq 1 ]] \
    && check_exe ADM_NORMALIZER required; then
    normalizer_sha="$(sha256_of "${ADM_NORMALIZER}")"
fi

# 三个 DME 编码器与配套 muxer 必须来自同一套本机安装；分别记录哈希，
# 不把安装路径或版本字符串写入可分发的 provenance。
dme_encoder_sha=""; dme_channel_encoder_sha=""; dme_ims_encoder_sha=""
dme_muxer_sha=""; dme_ac4_muxer_sha=""; dme_native_muxer_sha=""
if [[ ${need_dme} -eq 1 ]]; then
    if check_exe DME_AC4_AJOC_ENCODER required; then
        dme_encoder_sha="$(sha256_of "${DME_AC4_AJOC_ENCODER}")"
    fi
fi
if [[ ${need_dme_native} -eq 1 ]]; then
    if check_exe DME_AC4_ENCODER required; then
        dme_channel_encoder_sha="$(sha256_of "${DME_AC4_ENCODER}")"
    fi
    if check_exe DME_AC4_IMS_ENCODER required; then
        dme_ims_encoder_sha="$(sha256_of "${DME_AC4_IMS_ENCODER}")"
    fi
fi
if [[ ${need_dme} -eq 1 || ${need_dme_native} -eq 1 ]]; then
    if check_exe DME_MP4MUXER required; then
        dme_muxer_sha="$(sha256_of "${DME_MP4MUXER}")"
    fi
fi
if [[ ${need_dme} -eq 1 ]]; then
    dme_ac4_muxer_sha="${dme_muxer_sha}"
fi
if [[ ${need_dme_native} -eq 1 ]]; then
    dme_native_muxer_sha="${dme_muxer_sha}"
fi

# 后端组件可能是目录也可能是文件；留空表示编码器自包含。
backend_present="null"
if [[ -n "${AC4_ENCODER_BACKEND:-}" ]]; then
    if [[ -e "${AC4_ENCODER_BACKEND}" ]]; then
        backend_present="true"
    else
        backend_present="false"
        if [[ ${need_default} -eq 1 ]]; then
            errors+=("AC4_ENCODER_BACKEND 指向的组件不存在，编码将不可用：${AC4_ENCODER_BACKEND}")
        else
            warnings+=("AC4_ENCODER_BACKEND 指向的组件不存在：${AC4_ENCODER_BACKEND}")
        fi
    fi
fi

# DEE 本身、实际 engine binary 与 XML 模板分别取指纹。只记录哈希，不把
# 外部工具路径或版本字符串写入随仓库分发的 provenance。
dee_encoder_sha=""; dee_engine_sha=""; dee_template_sha=""; dee_workspace_present="null"
if [[ ${need_dee} -eq 1 ]]; then
    dee_template_ok=0
    dee_workspace_ok=0
    dee_drive_ok=0
    if check_exe DEE_ENCODER required; then
        dee_encoder_sha="$(sha256_of "${DEE_ENCODER}")"
    fi
    if check_file DEE_ENGINE_BINARY required; then
        dee_engine_sha="$(sha256_of "${DEE_ENGINE_BINARY}")"
    fi
    if check_file DEE_IMS_TEMPLATE required; then
        dee_template_sha="$(sha256_of "${DEE_IMS_TEMPLATE}")"
        dee_template_ok=1
    fi
    if check_workspace required; then
        dee_workspace_present="true"
        dee_workspace_ok=1
    else
        dee_workspace_present="false"
    fi
    if [[ ! "${DEE_WORKSPACE_DRIVE:-y:}" =~ ^[A-Za-z]:$ ]]; then
        errors+=("DEE_WORKSPACE_DRIVE 必须形如 y:")
    else
        dee_drive_ok=1
    fi
    if [[ ${dee_template_ok} -eq 1 && ${dee_workspace_ok} -eq 1 && ${dee_drive_ok} -eq 1 ]] \
        && ! "${PYTHON_BIN}" "${REPO_ROOT}/scripts/dee_ims.py" workspace-path \
            "${DEE_WORKSPACE_ROOT}" "${DEE_WORKSPACE_DRIVE:-y:}" \
            "${DEE_IMS_TEMPLATE}" >/dev/null 2>&1; then
        errors+=("DEE_IMS_TEMPLATE 必须位于 DEE_WORKSPACE_ROOT 内")
    fi
fi

ffprobe_version=""
if check_exe FFPROBE optional; then
    ffprobe_version="$("${FFPROBE}" -version 2>/dev/null | head -1)"
fi

# DEE IMS 用 MP4Box 把 raw AC-4 封装为 M4A；标准编码器附带辅助视频轨时也用
# 它做无损移除。允许从 PATH 发现，默认 profile 不无条件绑定到 GPAC。
mp4box_path="${MP4BOX:-}"
if [[ -z "${mp4box_path}" ]]; then
    mp4box_path="$(command -v MP4Box 2>/dev/null || true)"
fi
mp4box_sha=""; mp4box_version=""
if [[ -n "${mp4box_path}" && -x "${mp4box_path}" ]]; then
    mp4box_sha="$(sha256_of "${mp4box_path}")"
    mp4box_version="$("${mp4box_path}" -version 2>&1 | head -1)"
else
    if [[ ${need_dee} -eq 1 ]]; then
        errors+=("MP4BOX 未配置且 PATH 中不存在，DEE IMS 封装不可用")
    else
        warnings+=("MP4BOX 未配置且 PATH 中不存在（编码器附带视频轨时才需要）")
    fi
fi
muxer_sha=""
muxer_backend="null"
if [[ ${need_dee} -eq 1 ]]; then
    muxer_sha="${mp4box_sha}"
    muxer_backend='"gpac_mp4box"'
fi

vectors_dir="${VECTORS_DIR:-}"
[[ -z "${vectors_dir}" ]] && vectors_dir="${REPO_ROOT}/vectors"

# --- 输出 ---

# 非 ASCII 字符按两个终端列计算，printf 的宽度是按字节的
disp_width() {
    local s="$1" cjk
    cjk=$(printf '%s' "${s}" | grep -o '[^ -~]' | wc -l | tr -d ' ')
    echo $(( ${#s} + cjk ))
}

row() {
    local label="$1" value="$2" w
    w=$(disp_width "${label}")
    printf '  %s%*s%s\n' "${label}" $(( 18 - w )) "" "${value}"
}

# --json 供 record_provenance.py 写入随仓库分发的 provenance.json，因此只输出
# 指纹，不输出可执行文件的路径与版本字符串——后者会暴露外部工具的名称与本机
# 位置。sha256 与 commit 已足以判定「是否同一个工具」，这正是溯源需要的。
# 人类可读输出保留路径，它只在本机显示，不进入版本控制。
if [[ ${JSON_OUTPUT} -eq 1 ]]; then
    cat <<EOF
{
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "host": {
    "os": "$(sw_vers -productName 2>/dev/null) $(sw_vers -productVersion 2>/dev/null)",
    "arch": "$(uname -m)"
  },
  "tools": {
    "ac4_encoder": {
      "sha256": "${encoder_sha}",
      "commit": "${encoder_commit}",
      "worktree_dirty": ${encoder_dirty}
    },
    "adm_normalizer": {
      "sha256": "${normalizer_sha}"
    },
    "dme_ac4": {
      "encoder_sha256": "${dme_encoder_sha}",
      "muxer_sha256": "${dme_ac4_muxer_sha}"
    },
    "dme_native": {
      "channel_encoder_sha256": "${dme_channel_encoder_sha}",
      "ims_encoder_sha256": "${dme_ims_encoder_sha}",
      "muxer_sha256": "${dme_native_muxer_sha}"
    },
    "ac4_muxer": {
      "backend": ${muxer_backend},
      "sha256": "${muxer_sha}"
    },
    "encoder_backend": {
      "present": ${backend_present}
    },
    "dee_ims_encoder": {
      "wrapper_sha256": "${dee_encoder_sha}",
      "engine_sha256": "${dee_engine_sha}",
      "template_sha256": "${dee_template_sha}",
      "workspace_present": ${dee_workspace_present}
    },
    "ffprobe": {
      "version": "${ffprobe_version}"
    },
    "mp4box": {
      "sha256": "${mp4box_sha}",
      "version": "${mp4box_version}"
    }
  },
  "profile": "${PROFILE}",
  "vectors_dir_configured": $( [[ -n "${VECTORS_DIR:-}" ]] && echo true || echo false )
}
EOF
else
    echo "MacinDecode-AC4-Core 测试向量工具链"
    echo "配置来源：${ENV_FILE}"
    echo "校验配置：${PROFILE}"
    echo
    if [[ ${need_default} -eq 1 ]]; then
        row "AC-4 编码器" "${encoder_version:-<缺失>}"
        row "" "${AC4_ENCODER:-}"
        [[ -n "${encoder_sha}" ]]    && row "" "sha256 ${encoder_sha:0:16}…"
        [[ -n "${encoder_commit}" ]] && row "" "commit ${encoder_commit:0:12}$(
            [[ "${encoder_dirty}" == "true" ]] && echo ' (dirty)')"
        row "ADM 规范化" "${ADM_NORMALIZER:-<缺失>}"
        [[ -n "${normalizer_sha}" ]] && row "" "sha256 ${normalizer_sha:0:16}…"
        case "${backend_present}" in
            true)  row "编码后端" "已找到" ;;
            false) row "编码后端" "<缺失>" ;;
            *)     row "编码后端" "<未配置>" ;;
        esac
        echo
    fi
    if [[ ${need_dme} -eq 1 ]]; then
        row "DME A-JOC" "${DME_AC4_AJOC_ENCODER:-<缺失>}"
        [[ -n "${dme_encoder_sha}" ]] && row "" "encoder sha256 ${dme_encoder_sha:0:16}…"
        row "DME MP4 muxer" "${DME_MP4MUXER:-<缺失>}"
        [[ -n "${dme_muxer_sha}" ]] && row "" "muxer sha256 ${dme_muxer_sha:0:16}…"
        if [[ ${need_default} -eq 0 ]]; then
            row "ADM 规范化" "${ADM_NORMALIZER:-<缺失>}"
            [[ -n "${normalizer_sha}" ]] && row "" "sha256 ${normalizer_sha:0:16}…"
        fi
        echo
    fi
    if [[ ${need_dme_native} -eq 1 ]]; then
        row "DME channel AC-4" "${DME_AC4_ENCODER:-<缺失>}"
        [[ -n "${dme_channel_encoder_sha}" ]] && row "" "encoder sha256 ${dme_channel_encoder_sha:0:16}…"
        row "DME native IMS" "${DME_AC4_IMS_ENCODER:-<缺失>}"
        [[ -n "${dme_ims_encoder_sha}" ]] && row "" "encoder sha256 ${dme_ims_encoder_sha:0:16}…"
        row "DME MP4 muxer" "${DME_MP4MUXER:-<缺失>}"
        [[ -n "${dme_native_muxer_sha}" ]] && row "" "muxer sha256 ${dme_native_muxer_sha:0:16}…"
        echo
    fi
    if [[ ${need_dee} -eq 1 ]]; then
        row "DEE IMS" "${DEE_ENCODER:-<缺失>}"
        [[ -n "${dee_encoder_sha}" ]] && row "" "wrapper sha256 ${dee_encoder_sha:0:16}…"
        [[ -n "${dee_engine_sha}" ]]  && row "" "engine sha256 ${dee_engine_sha:0:16}…"
        [[ -n "${dee_template_sha}" ]] && row "" "template sha256 ${dee_template_sha:0:16}…"
        case "${dee_workspace_present}" in
            true)  row "DEE 工作区" "可写" ;;
            false) row "DEE 工作区" "<不可用>" ;;
            *)     row "DEE 工作区" "<未配置>" ;;
        esac
        echo
    fi
    row "ffprobe" "${ffprobe_version:-<未配置>}"
    row "GPAC MP4Box" "${mp4box_path:-<未找到>}"
    [[ -n "${mp4box_sha}" ]] && row "" "sha256 ${mp4box_sha:0:16}…"
    echo
    row "向量输出目录" "${vectors_dir}"
    echo

    if [[ ${#warnings[@]} -gt 0 ]]; then
        echo "警告："
        printf '  - %s\n' "${warnings[@]}"
        echo
    fi
    if [[ ${#errors[@]} -gt 0 ]]; then
        echo "错误："
        printf '  - %s\n' "${errors[@]}"
        echo
        echo "结果：工具链不完整"
        exit 1
    fi
    echo "结果：必需工具齐备"
fi

[[ ${#errors[@]} -eq 0 ]] || exit 1
exit 0
