//! DAMF 使用的确定性 YAML 1.2 文本 writer。

/// JSON 双引号字符串也是合法 YAML 1.2 scalar；复用 serde 的完整 UTF-8 与控制
/// 字符转义，避免格式专属代码遗漏边界。
pub(super) fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("String must be serializable as a YAML scalar")
}

/// 统一 LF、两空格缩进和末尾换行。
pub(super) fn finish_lines(lines: Vec<String>) -> String {
    let mut out = String::new();
    for line in lines {
        debug_assert!(
            !line.contains(['\r', '\n']),
            "YAML lines must not contain newlines"
        );
        let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
        debug_assert_eq!(
            indentation % 2,
            0,
            "DAMF YAML must use two-space indentation"
        );
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_has_deterministic_scalar_newlines_and_indentation() {
        assert_eq!(quote("a\n\"\\"), "\"a\\n\\\"\\\\\"");
        let yaml = finish_lines(vec!["root:".to_owned(), "  value: 1".to_owned()]);
        assert_eq!(yaml, "root:\n  value: 1\n");
        assert!(!yaml.contains('\r'));
    }
}
