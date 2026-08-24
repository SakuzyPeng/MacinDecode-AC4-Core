#!/usr/bin/env bash
# 拼接向量门禁：验证 reset 状态机在真实来源变化下的行为。
#
#   ./scripts/splice_check.sh <A.m4a> <B.m4a>
#
# 单一来源的向量永远走不到 SourceChange、WaitForRandomAccess 与 OAMD 清史
# 这几条路径——它们在两个探针案例上的计数恒为 0。本门禁用两条已编码流拼出
# 真实的来源变化来覆盖它们。
#
# 期望值由 make_splice.py 从容器 sample table 与计数器算术导出；判定值由
# Rust 侧的比特级 TOC 解析给出。两者没有共同来源。
#
# 构造两个变体：
#   同下标拼接：两条流的 sequence_counter 恰好衔接，拼接不可检测。
#   错位拼接：  计数不连续，触发来源变化并等待下一个完整随机访问点。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON_BIN="${PYTHON_BIN:-python3}"

if [ "$#" -ne 2 ]; then
    echo "用法：$0 <A.m4a> <B.m4a>" >&2
    exit 2
fi

FIRST="$1"
SECOND="$2"
for path in "${FIRST}" "${SECOND}"; do
    if [ ! -f "${path}" ]; then
        echo "找不到输入：${path}" >&2
        exit 1
    fi
done

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

mismatch=0

disp_width() {
    "${PYTHON_BIN}" -c '
import sys, unicodedata
text = sys.argv[1]
print(sum(2 if unicodedata.east_asian_width(ch) in "WF" else 1 for ch in text))
' "$1"
}

check() {
    local label="$1" expected="$2" actual="$3" width
    width=$(disp_width "${label}")
    if [ "${expected}" = "${actual}" ]; then
        printf '  %s%*s%-12s 一致\n' "${label}" $(( 34 - width )) "" "${actual}"
    else
        printf '  %s%*s%-12s 不一致：期望 %s\n' \
            "${label}" $(( 34 - width )) "" "${actual}" "${expected}"
        mismatch=1
    fi
}

field() {
    "${PYTHON_BIN}" -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' \
        "$1" "$2"
}

topology_field() {
    "${PYTHON_BIN}" -c '
import json, sys
topology = json.load(open(sys.argv[1]))["result"]["validation"]["topology"]
key = sys.argv[2]
for group in ("coverage", "references", "timing", "configuration", "observations"):
    if key in topology[group]:
        print(topology[group][key])
        break
else:
    raise KeyError(key)
' "$1" "$2"
}

run_variant() {
    local name="$1"
    shift
    local stream="${WORK}/${name}.ac4"
    local report="${WORK}/${name}.json"
    local trace="${WORK}/${name}.trace.json"

    "${REPO_ROOT}/scripts/make_splice.py" "${FIRST}" "${SECOND}" \
        -o "${stream}" --report "${report}" "$@" >/dev/null

    if ! cargo run -q --locked --manifest-path "${REPO_ROOT}/Cargo.toml" --bin macinac4 -- \
            trace "${stream}" >"${trace}"; then
        echo "trace 失败：${stream}" >&2
        exit 1
    fi

    local continuous
    continuous="$(field "${report}" boundary_sequence_continuous)"
    printf '\n%s（拼接点 %s，B 起始 %s，边界计数%s）：\n' \
        "${name}" \
        "$(field "${report}" splice_at)" \
        "$(field "${report}" second_start)" \
        "$([ "${continuous}" = "True" ] && echo "连续" || echo "不连续")"

    check "帧数" "$(field "${report}" frames)" "$(topology_field "${trace}" frames_parsed)"
    check "拓扑解析失败" "0" "$(topology_field "${trace}" parse_failures)"
    check "substream 引用不完整" "0" \
        "$(topology_field "${trace}" substream_reference_failures)"
    check "完整随机访问点" "$(field "${report}" expected_full_random_access_frames)" \
        "$(topology_field "${trace}" full_random_access_frames)"
    check "来源变化" "$(field "${report}" expected_source_changes)" \
        "$(topology_field "${trace}" source_changes)"
    check "reset 事件" "$(field "${report}" expected_reset_events)" \
        "$(topology_field "${trace}" reset_events)"
    check "等待随机访问帧" \
        "$(field "${report}" expected_waiting_for_random_access_frames)" \
        "$(topology_field "${trace}" waiting_for_random_access_frames)"
    check "结束时仍等待随机访问" "False" \
        "$(topology_field "${trace}" awaiting_random_access)"

    local oamd
    oamd="$("${PYTHON_BIN}" -c '
import json, sys
o = json.load(open(sys.argv[1]))["result"]["validation"]["oamd"]
print(o["coverage"]["parsed"], o["coverage"]["failures"], o["timing"]["max_align_bits"])
' "${trace}")"
    read -r parsed failures align <<<"${oamd}"
    check "OAMD 解析帧数" "$(field "${report}" frames)" "${parsed}"
    check "OAMD 解析失败" "0" "${failures}"
    if [ "${align}" -lt 8 ]; then
        printf '  %s%*s%-12s < 8，一致\n' "byte_align 残余上界" \
            $(( 34 - $(disp_width "byte_align 残余上界") )) "" "${align}"
    else
        printf '  %s%*s%-12s 不一致：应 < 8\n' "byte_align 残余上界" \
            $(( 34 - $(disp_width "byte_align 残余上界") )) "" "${align}"
        mismatch=1
    fi
}

printf '来源 A：%s\n来源 B：%s\n' "$(basename "${FIRST}")" "$(basename "${SECOND}")"

# 同下标拼接：两条流的计数恰好衔接。这一变体的意义在于说明
# sequence_counter 不是充分的拼接指示器。
run_variant "同下标拼接" --splice-at 60 --second-start 60

# 错位拼接：制造真实的计数不连续。
run_variant "错位拼接" --splice-at 60 --second-start 100

echo
if [ "${mismatch}" -ne 0 ]; then
    echo "结果：拼接向量存在不一致"
    exit 1
fi
echo "结果：reset 状态机在真实来源变化下行为符合预期"
