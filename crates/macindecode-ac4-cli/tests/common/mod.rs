#![allow(
    dead_code,
    reason = "共享模块按 test binary 各编译一份，每个只用其中一部分"
)]

use std::path::PathBuf;

mod result_schema;

const PROBE_AXES_VECTOR_ENV: &str = "MACINAC4_PROBE_AXES_VECTOR";

pub(crate) fn success(command: &str, stdout: &[u8]) -> serde_json::Value {
    result_schema::success(command, stdout)
}

pub(crate) fn require_probe_axes_vector() -> PathBuf {
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("vectors/probe_axes_single_object/encoded/master_ac4_768K.m4a");
    let path = std::env::var_os(PROBE_AXES_VECTOR_ENV)
        .map(PathBuf::from)
        .unwrap_or(default);
    assert!(
        path.is_file(),
        "有条件真实向量测试已显式启用，但测试向量不存在：{}；可运行 ./scripts/build_vector.sh vectors/probe_axes_single_object/case.json 或设置 {PROBE_AXES_VECTOR_ENV}",
        path.display()
    );
    path
}
