# macindecode-ac4-inspect

面向 Rust 应用的 AC-4 文件级元数据检视库。它单遍扫描 MP4/M4A 或 Annex G raw AC-4，
返回与 `macinac4 inspect` 相同的 owned typed report，并可渲染固定英文纯文本。
本 crate 不需要 `audio-decode`，也不执行响度、DRC、Dialogue Enhancement、downmix 或
PCM 处理。

```toml
[dependencies]
macindecode-ac4-inspect = "0.1.0"
serde_json = "1"
```

检查文件：

```rust
use macindecode_ac4_inspect::inspect_path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report = inspect_path("input.m4a")?;
    println!("frames: {}", report.source.frame_count);
    print!("{}", report.render_text());

    // 这是裸 InspectReport，即 CLI `result.inspectResult` 的形状，不含 CLI envelope。
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    Ok(())
}
```

检查内存数据：

```rust
use macindecode_ac4_inspect::{
    InspectInputFormat, InspectSourceHint, inspect_bytes,
};

fn inspect_network_packet(data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let report = inspect_bytes(
        data,
        InspectSourceHint::new(Some("network-packet.ac4"), InspectInputFormat::AnnexG),
    )?;
    assert_eq!(report.source.input, "network-packet.ac4");
    Ok(())
}
```

`InspectInputFormat::Auto` 只在输入起始为 `AC40`/`AC41` 时选择 Annex G，否则选择 MP4；
它不根据文件扩展名推断格式。`InspectSourceHint::default()` 使用自动检测，报告中的输入名为
`<memory>`。结构损坏、空输入和读取失败通过 `InspectError` 返回；CRC、保留码和已知未支持
语法仍尽可能形成带 `issues` 的可用报告。

报告的五种字段状态为 `present`、`not_present`、`not_applicable`、`unknown` 和
`unsupported`。有码值的语义字段在 `ReportedField::raw_code` 中保留原码。

MSRV 为 Rust 1.98，禁止 unsafe Rust。

## License

[MIT](LICENSE)
