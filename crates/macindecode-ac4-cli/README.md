# macindecode-ac4-cli

AC-4 检视、验证和渲染前场景导出命令行工具。crate 安装后提供 `macinac4`，支持
MP4/raw AC-4 人读 inspect 与 JSON trace，以及 PCM WAVE、ADM BWF、DAMF 和 Apple CAF 导出。

本工具是独立开源实现，与 Dolby Laboratories 不存在隶属、赞助或认可关系；
相关商标仅用于说明兼容性。

```bash
cargo install macindecode-ac4-cli
macinac4 inspect path/to/input.m4a
macinac4 trace path/to/input.m4a
```

默认构建可执行 `inspect` 与容器、TOC/OAMD/metadata trace；需要音频解码的导出命令返回稳定的
`feature.required` 诊断。完整音频构建使用：

```bash
MACINDECODE_AC4_SPEC_DIR=/absolute/path/to/spec \
  cargo install macindecode-ac4-cli --features audio-decode
```

`audio-decode` 所需 ETSI PDF 派生表与随附 C 表不随 crate 分发。请使用与 crate
版本相同的仓库 tag 运行 `fetch_specs.py` 和 `generate_spec_tables.py`；指定目录须包含
`generated/ts103190_pdf_tables.rs`、`ts_103190_tables.c` 和
`ts_103190_tables_part2.c`。获取和校验流程见
[项目仓库](https://github.com/SakuzyPeng/MacinDecode-AC4-Core)。

成功时 stdout 通常使用带 `schema`/`version` 的 JSON v1 envelope；`inspect` 默认输出
英文纯文本，使用 `--format json` 可取得同一 envelope。失败时 stdout 为空，诊断写入
stderr。完整命令和输出契约见
[CLI 使用指南](https://github.com/SakuzyPeng/MacinDecode-AC4-Core/blob/main/docs/CLI_USAGE.md)
与随包分发的 [`schema/`](schema/)。

MSRV 为 Rust 1.98。

## License

[MIT](LICENSE)
