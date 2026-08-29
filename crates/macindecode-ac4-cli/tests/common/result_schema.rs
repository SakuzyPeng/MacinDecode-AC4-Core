#![allow(
    clippy::indexing_slicing,
    reason = "测试按发布 schema 的固定 JSON 属性核对成功响应"
)]

use std::collections::BTreeSet;

use serde_json::Value;

const RESULT_SCHEMA: &str = include_str!("../../schema/cli-result-v1.schema.json");

/// 解出一份成功响应，并用发布的 schema 核对它的键集合。
///
/// 八种 wire 投影的无媒体单元测试与可用输入的端到端测试都经过这里。必需键一律
/// 从 schema 文件读出，不在测试里另抄一份，任何一侧单方面改动都会失败。
///
/// 只核对键集合这一层。`frames`、`track` 与八个验证分类的**内部**字段仍随
/// A-SPX 各子节增长，schema 有意把它们留作自由对象——锁死它们会让每加一条统计
/// 都要同时改两处。
pub(crate) fn success(command: &str, stdout: &[u8]) -> Value {
    let value: Value = serde_json::from_slice(stdout).expect("stdout 应为 JSON");
    let schema: Value = serde_json::from_str(RESULT_SCHEMA).expect("发布的 schema 应为 JSON");

    assert_eq!(value["schema"], "macinac4.cli-result");
    assert_eq!(value["version"], 1);
    assert_eq!(value["command"], command);
    assert_closed(&value, &schema, "$");

    let result = &value["result"];
    match command {
        "trace" => assert_trace_shape(result, &schema),
        "inspect" => assert_inspect_shape(result, &schema),
        _ => assert_export_shape(command, result, &schema),
    }
    value
}

fn assert_inspect_shape(result: &Value, schema: &Value) {
    assert_exact(result, definition(schema, "inspectWireResult"), "$.result");
    let report = &result["inspectResult"];
    assert_exact(
        report,
        definition(schema, "inspectResult"),
        "$.result.inspectResult",
    );

    let source = &report["source"];
    assert_exact(
        source,
        definition(schema, "inspectSource"),
        "$.result.inspectResult.source",
    );
    for name in ["track_index", "duration"] {
        assert_reported(
            &source[name],
            schema,
            &format!("$.result.inspectResult.source.{name}"),
        );
    }

    let stream = &report["stream"];
    assert_exact(
        stream,
        definition(schema, "inspectStream"),
        "$.result.inspectResult.stream",
    );
    for name in [
        "bit_rate",
        "estimated_average_bit_rate",
        "bitstream_version",
        "frame_rate",
        "sample_rate",
        "i_frame",
        "i_frame_interval",
        "sync_word",
        "crc_errors",
        "number_of_presentations",
        "number_of_audio_substreams",
    ] {
        assert_reported(
            &stream[name],
            schema,
            &format!("$.result.inspectResult.stream.{name}"),
        );
    }

    for (index, presentation) in report["presentations"]
        .as_array()
        .expect("presentations 应为数组")
        .iter()
        .enumerate()
    {
        let path = format!("$.result.inspectResult.presentations[{index}]");
        assert_exact(
            presentation,
            definition(schema, "inspectPresentation"),
            &path,
        );
        for name in [
            "presentation_id",
            "summary",
            "presentation_type",
            "minimal_compatibility_level",
            "dialogue_normalization",
            "language",
            "multi_pid",
            "bit_rate",
            "audio_substreams",
            "metadata_authentication_id",
        ] {
            assert_reported(&presentation[name], schema, &format!("{path}.{name}"));
        }
        for (name, definition_name) in [
            ("loudness", "inspectLoudness"),
            ("dynamic_range_control", "inspectDrc"),
            ("mixing_metadata", "inspectMixing"),
            ("downmix", "inspectDownmix"),
        ] {
            let nested = &presentation[name];
            assert_exact(
                nested,
                definition(schema, definition_name),
                &format!("{path}.{name}"),
            );
            for field in nested
                .as_object()
                .expect("inspect metadata section 应为对象")
                .keys()
            {
                assert_reported(&nested[field], schema, &format!("{path}.{name}.{field}"));
            }
        }
    }

    for (index, substream) in report["audio_substreams"]
        .as_array()
        .expect("audio_substreams 应为数组")
        .iter()
        .enumerate()
    {
        let path = format!("$.result.inspectResult.audio_substreams[{index}]");
        assert_exact(
            substream,
            definition(schema, "inspectAudioSubstream"),
            &path,
        );
        for name in [
            "summary",
            "channel_configuration",
            "channel_layout",
            "object_coded",
            "bit_rate",
        ] {
            assert_reported(&substream[name], schema, &format!("{path}.{name}"));
        }
        for (name, definition_name) in [
            ("preprocessing", "inspectPreprocessing"),
            ("dialogue_enhancement", "inspectDialogueEnhancement"),
        ] {
            let nested = &substream[name];
            assert_exact(
                nested,
                definition(schema, definition_name),
                &format!("{path}.{name}"),
            );
            for field in nested
                .as_object()
                .expect("inspect substream section 应为对象")
                .keys()
            {
                assert_reported(&nested[field], schema, &format!("{path}.{name}.{field}"));
            }
        }
    }

    for (index, issue) in report["issues"]
        .as_array()
        .expect("issues 应为数组")
        .iter()
        .enumerate()
    {
        assert_exact(
            issue,
            definition(schema, "inspectIssue"),
            &format!("$.result.inspectResult.issues[{index}]"),
        );
    }
}

fn assert_reported(value: &Value, schema: &Value, path: &str) {
    assert_closed(value, definition(schema, "reportedField"), path);
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("{path} 应为 reported field 对象"));
    match value["status"].as_str() {
        Some("present") => {
            assert!(
                object.contains_key("value"),
                "{path} present 必须携带 value"
            );
            assert!(
                !object.contains_key("reason"),
                "{path} present 不得携带 reason"
            );
        }
        Some("not_present" | "not_applicable") => {
            assert_eq!(
                object.len(),
                1,
                "{path} not_present/not_applicable 只能携带 status"
            );
        }
        Some("unknown" | "unsupported") => {
            assert!(
                object.get("reason").and_then(Value::as_str).is_some(),
                "{path} unknown/unsupported 必须携带字符串 reason"
            );
            assert!(
                !object.contains_key("value"),
                "{path} unavailable 不得携带 value"
            );
            assert!(
                !object.contains_key("unit"),
                "{path} unavailable 不得携带 unit"
            );
        }
        other => panic!("{path}.status 应为五种稳定状态之一，实际为 {other:?}"),
    }
}

fn assert_trace_shape(result: &Value, schema: &Value) {
    assert_exact(result, definition(schema, "traceResult"), "$.result");

    let source = &result["source"];
    let source_definition = match source["kind"].as_str() {
        Some("mp4") => "mp4Source",
        Some("annex_g") => "annexGSource",
        other => panic!("source.kind 不是 schema 声明的判别值：{other:?}"),
    };
    let source_schema = definition(schema, source_definition);
    assert_exact(source, source_schema, "$.result.source");
    if source_definition == "annexGSource" {
        let crc_schema = source_schema
            .pointer("/properties/crc")
            .expect("annexGSource 应声明 crc");
        assert_exact(&source["crc"], crc_schema, "$.result.source.crc");
    }

    let validation = &result["validation"];
    let sections = definition(schema, "traceValidation");
    assert_exact(validation, sections, "$.result.validation");

    let section = definition(schema, "validationSection");
    for name in names(sections.get("required")) {
        let node = &validation[&name];
        if node.is_null() {
            // schema 的 oneOf 只给 ajoc 开了 null 这一支：未启用 audio-decode 时
            // 该 section 不存在，其余三个任何时候都必须是对象。
            assert_eq!(name, "ajoc", "只有 ajoc 允许为 null");
            continue;
        }
        assert_exact(node, section, &format!("$.result.validation.{name}"));
    }
}

fn assert_export_shape(command: &str, result: &Value, schema: &Value) {
    let export = definition(schema, "exportResult");
    assert_closed(result, export, "$.result");

    // 实际键必须恰好是公共必需键与命令专属必需键的并集。只查子集会让某命令
    // 意外带上另一命令的字段，也会让 allOf 分支漏写 required 而不报错。
    let branch = command_branch(schema, command);
    let command_required = names(branch.pointer("/then/properties/result/required"));
    let expected = names(export.get("required"))
        .union(&command_required)
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        keys(result, "$.result"),
        expected,
        "$.result 的键必须恰好匹配 {command} 的公共与专属 required"
    );

    let artifacts = result["artifacts"]
        .as_array()
        .expect("$.result.artifacts 应为数组");
    let bound = |name: &str| {
        let limit = branch
            .pointer(&format!(
                "/then/properties/result/properties/artifacts/{name}"
            ))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{command} 分支应声明 artifacts.{name}"));
        usize::try_from(limit).expect("schema 的条数界应可表示")
    };
    let count = artifacts.len();
    let min = bound("minItems");
    let max = bound("maxItems");
    assert!(
        count >= min,
        "$.result.artifacts 有 {count} 项，少于 minItems={min}"
    );
    assert!(
        count <= max,
        "$.result.artifacts 有 {count} 项，多于 maxItems={max}"
    );

    let artifact = definition(schema, "artifact");
    for (index, item) in artifacts.iter().enumerate() {
        assert_exact(item, artifact, &format!("$.result.artifacts[{index}]"));
    }
    assert_exact(
        &result["audio"],
        definition(schema, "audio"),
        "$.result.audio",
    );
}

/// 在 [`assert_closed`] 之上，要求 schema 自身的 `required` 覆盖全部
/// `properties`。
///
/// 契约 §2 里那些「固定包含」的形状没有可选字段。少写一项 `required` 会把它悄
/// 悄放松成可选，而实际输出仍带着该字段——两边都不报错，契约却已经变了。只有
/// `exportResult` 是例外，它的可选字段按命令分派，由 [`assert_export_shape`] 从
/// `allOf` 分支单独核对。
fn assert_exact(value: &Value, definition: &Value, path: &str) {
    assert_closed(value, definition, path);
    assert_eq!(
        names(definition.get("required")),
        names(definition.get("properties")),
        "{path} 的 schema 定义不应有可选字段"
    );
}

/// 核对一个封闭对象：实际键落在 `properties` 内，且覆盖 `required`。
///
/// 两个方向都要查。只查 `required` 会放过多出来的字段（外部消费者按 schema
/// 写的解析会漏掉它们）；只查 `properties` 会放过整块消失的字段。
fn assert_closed(value: &Value, definition: &Value, path: &str) {
    assert_eq!(
        definition["additionalProperties"],
        Value::Bool(false),
        "{path} 对应的 schema 定义不是封闭对象，核对键集合没有意义"
    );
    let actual = keys(value, path);
    let allowed = names(definition.get("properties"));
    let required = names(definition.get("required"));
    assert!(
        actual.is_subset(&allowed),
        "{path} 出现 schema 未声明的键：{:?}",
        actual.difference(&allowed).collect::<Vec<_>>()
    );
    assert!(
        required.is_subset(&actual),
        "{path} 缺少 schema 声明的必需键：{:?}",
        required.difference(&actual).collect::<Vec<_>>()
    );
}

fn definition<'a>(schema: &'a Value, name: &str) -> &'a Value {
    schema
        .pointer(&format!("/$defs/{name}"))
        .unwrap_or_else(|| panic!("schema 应声明 $defs/{name}"))
}

fn command_branch<'a>(schema: &'a Value, command: &str) -> &'a Value {
    schema["allOf"]
        .as_array()
        .expect("schema 应有 allOf 分支")
        .iter()
        .find(|branch| {
            branch
                .pointer("/if/properties/command/const")
                .and_then(Value::as_str)
                == Some(command)
        })
        .unwrap_or_else(|| panic!("schema 应为 {command} 声明一条 allOf 分支"))
}

fn keys(value: &Value, path: &str) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{path} 应为对象"))
        .keys()
        .cloned()
        .collect()
}

/// 取一个 schema 节点的名称集合：`properties` 取键名，`required` 取元素。
fn names(node: Option<&Value>) -> BTreeSet<String> {
    match node {
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().expect("required 的元素应为字符串").to_owned())
            .collect(),
        _ => BTreeSet::new(),
    }
}
