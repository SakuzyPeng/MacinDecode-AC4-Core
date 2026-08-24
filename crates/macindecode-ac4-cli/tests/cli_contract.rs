#![allow(
    clippy::indexing_slicing,
    reason = "测试以固定诊断 schema 核对必需属性"
)]

#[cfg(feature = "audio-decode")]
mod common;

use std::process::Command;
#[cfg(feature = "audio-decode")]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "audio-decode")]
use common::require_probe_axes_vector;

#[cfg(feature = "audio-decode")]
static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

fn diagnostics(stderr: &[u8]) -> Vec<serde_json::Value> {
    std::str::from_utf8(stderr)
        .expect("stderr 应为 UTF-8")
        .lines()
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("每个 stderr 行都应是 JSON");
            assert_eq!(value["schema"], "macinac4.cli-diagnostic");
            assert_eq!(value["version"], 1);
            value
        })
        .collect()
}

fn assert_english_only(output: &[u8]) {
    let output = std::str::from_utf8(output).expect("CLI output should be UTF-8");
    assert!(
        !output.chars().any(|character| matches!(
            character,
            '\u{3000}'..='\u{312f}'
                | '\u{31a0}'..='\u{31bf}'
                | '\u{3400}'..='\u{4dbf}'
                | '\u{4e00}'..='\u{9fff}'
                | '\u{ac00}'..='\u{d7af}'
                | '\u{f900}'..='\u{faff}'
                | '\u{ff00}'..='\u{ffef}'
        )),
        "CLI output must be English-only: {output}"
    );
}

#[test]
fn clap_errors_are_json_exit_two_and_help_version_stay_text() {
    let invalid = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("not-a-command")
        .output()
        .expect("应能启动 CLI");
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert_english_only(&invalid.stderr);
    let errors = diagnostics(&invalid.stderr);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "cli.invalid_arguments");
    assert_eq!(errors[0]["message"], "Invalid command-line arguments");

    for argument in ["--help", "--version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
            .arg(argument)
            .output()
            .expect("应能启动 CLI");
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err(),
            "help/version 必须保持普通文本"
        );
    }
}

#[test]
fn every_help_page_is_english_only() {
    for command in [
        None,
        Some("trace"),
        Some("export-damf"),
        Some("export-full-damf"),
        Some("export-adm-bwf"),
        Some("export-full-adm-bwf"),
        Some("export-core-caf"),
        Some("export-core-pcm"),
        Some("export-aspx-pcm"),
        Some("export-objects-pcm"),
    ] {
        let mut invocation = Command::new(env!("CARGO_BIN_EXE_macinac4"));
        if let Some(command) = command {
            invocation.arg(command);
        }
        let output = invocation.arg("--help").output().expect("CLI should start");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_english_only(&output.stdout);
    }
}

#[test]
fn runtime_errors_are_json_exit_one_with_empty_stdout() {
    let missing = format!("/definitely-missing-macinac4-input-{}", std::process::id());
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("trace")
        .arg(&missing)
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_english_only(&output.stderr);
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "input.read_failed");
    assert_eq!(errors[0]["context"]["path"], missing);

    let empty =
        std::env::temp_dir().join(format!("macinac4-empty-contract-{}", std::process::id()));
    std::fs::File::create(&empty).expect("应能创建空输入");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("trace")
        .arg(&empty)
        .output()
        .expect("应能启动 CLI");
    std::fs::remove_file(&empty).expect("应能清理空输入");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_english_only(&output.stderr);
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "input.invalid");

    let malformed = std::env::temp_dir().join(format!(
        "macinac4-malformed-contract-{}",
        std::process::id()
    ));
    std::fs::write(&malformed, [0xac, 0x40, 0x00, 0x00]).expect("should write malformed input");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("trace")
        .arg(&malformed)
        .output()
        .expect("CLI should start");
    std::fs::remove_file(&malformed).expect("should remove malformed input");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_english_only(&output.stderr);
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "parse.failed");
    assert_eq!(
        errors[0]["context"]["cause"],
        "frame_size is zero at offset 0"
    );
}

#[cfg(not(feature = "audio-decode"))]
#[test]
fn feature_errors_use_the_stable_code() {
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .args([
            "export-damf",
            "unused.ac4",
            "--output",
            "unused-output",
            "--object",
            "0",
        ])
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors[0]["code"], "feature.required");

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .args([
            "export-aspx-pcm",
            "unused.ac4",
            "--output",
            "unused-aspx.wav",
        ])
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors[0]["command"], "export-aspx-pcm");
    assert_eq!(errors[0]["code"], "feature.required");

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .args([
            "export-objects-pcm",
            "unused.ac4",
            "--output",
            "unused-objects.wav",
        ])
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors[0]["command"], "export-objects-pcm");
    assert_eq!(errors[0]["code"], "feature.required");

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .args([
            "export-full-adm-bwf",
            "unused.ac4",
            "--output",
            "unused-full-adm.wav",
        ])
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors[0]["command"], "export-full-adm-bwf");
    assert_eq!(errors[0]["code"], "feature.required");

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .args([
            "export-full-damf",
            "unused.ac4",
            "--output",
            "unused-full-damf",
        ])
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors[0]["command"], "export-full-damf");
    assert_eq!(errors[0]["code"], "feature.required");

    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .args([
            "export-core-caf",
            "unused.ac4",
            "--output",
            "unused-core.caf",
        ])
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let errors = diagnostics(&output.stderr);
    assert_eq!(errors[0]["command"], "export-core-caf");
    assert_eq!(errors[0]["code"], "feature.required");
}

#[cfg(feature = "audio-decode")]
#[test]
fn export_parse_and_option_errors_use_the_stable_codes() {
    let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    let truncated = std::env::temp_dir().join(format!(
        "macinac4-truncated-contract-{}-{serial}.ac4",
        std::process::id()
    ));
    let truncated_output = truncated.with_extension("damf");
    let truncated_full_output = truncated.with_extension("full-adm.wav");
    let truncated_full_damf = truncated.with_extension("full-damf");
    std::fs::write(&truncated, [0xac, 0x40, 0x00, 0x10]).expect("应能写入截断输入");
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-damf")
        .arg(&truncated)
        .arg("--output")
        .arg(&truncated_output)
        .arg("--object")
        .arg("0")
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(diagnostics(&output.stderr)[0]["code"], "parse.failed");
    assert!(!truncated_output.exists());

    let full = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-adm-bwf")
        .arg(&truncated)
        .arg("--output")
        .arg(&truncated_full_output)
        .output()
        .expect("应能启动 full ADM CLI");
    assert_eq!(full.status.code(), Some(1));
    assert_eq!(diagnostics(&full.stderr)[0]["code"], "parse.failed");
    assert!(!truncated_full_output.exists());

    let full_damf = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-full-damf")
        .arg(&truncated)
        .arg("--output")
        .arg(&truncated_full_damf)
        .output()
        .expect("应能启动 full DAMF CLI");
    assert_eq!(full_damf.status.code(), Some(1));
    assert_eq!(diagnostics(&full_damf.stderr)[0]["code"], "parse.failed");
    assert!(!truncated_full_damf.exists());

    std::fs::remove_file(&truncated).expect("应能清理截断输入");

    let invalid_output = std::env::temp_dir().join(format!(
        "macinac4-invalid-option-contract-{}-{serial}",
        std::process::id()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-damf")
        .arg("unused.ac4")
        .arg("--output")
        .arg(&invalid_output)
        .arg("--object")
        .arg("0")
        .arg("--probe-level-dbfs")
        .arg("1")
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(diagnostics(&output.stderr)[0]["code"], "selection.invalid");
    assert!(!invalid_output.exists());
}

#[cfg(feature = "audio-decode")]
#[test]
#[ignore = "需要本地 probe_axes_single_object AC-4 向量"]
fn invalid_object_selector_uses_the_stable_selection_code() {
    let vector = require_probe_axes_vector();
    let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    let output_path = std::env::temp_dir().join(format!(
        "macinac4-invalid-selection-contract-{}-{serial}",
        std::process::id()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_macinac4"))
        .arg("export-damf")
        .arg(vector)
        .arg("--output")
        .arg(&output_path)
        .arg("--object")
        .arg("bad")
        .output()
        .expect("应能启动 CLI");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(diagnostics(&output.stderr)[0]["code"], "selection.invalid");
    assert!(!output_path.exists());
}
