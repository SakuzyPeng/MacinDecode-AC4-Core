# macindecode-ac4-bitstream

`#![no_std]` 的 Dolby AC-4 比特流解析与音频重建原语。它负责 raw sync frame、TOC、
presentation/group/substream 拓扑、EMDF、OAMD，以及可选的 ASF、A-SPX 和 Full
A-JOC 音频路径；容器解析由 `macindecode-ac4-mp4` 提供。

本 crate 是独立开源实现，与 Dolby Laboratories 不存在隶属、赞助或认可关系；
相关商标仅用于说明兼容性。

```toml
[dependencies]
macindecode-ac4-bitstream = "0.1.0"
```

默认 feature 提供容器无关的 bitstream、TOC/OAMD/metadata 解析，不需要规范附件。
规范 PDF 或其派生静态表不会随源码仓库或 crate 分发。

## `spec-tables` 与 `audio-decode`

`spec-tables` 启用 ASF 成帧/IMDCT、A-SPX 表与 A-JOC 参数表；其数值由用户从
官方 ETSI PDF 本地生成。`audio-decode` 会自动包含 `spec-tables`，并额外消费
ETSI 随附的 Huffman/QMF C 表。仓库检出中运行：

```bash
python3 -m pip install -r scripts/requirements-spec.txt
./scripts/fetch_specs.py
./scripts/generate_spec_tables.py
cargo build -p macindecode-ac4-bitstream --features spec-tables
cargo build -p macindecode-ac4-bitstream --features audio-decode
```

从注册表作为依赖构建时，请使用与 crate 版本相同的仓库 tag 运行上述脚本，再把
生成好的 `spec/` 目录通过绝对路径传给 `MACINDECODE_AC4_SPEC_DIR`。`spec-tables`
只需要第一项；`audio-decode` 需要全部三项：

- `generated/ts103190_pdf_tables.rs`
- `ts_103190_tables.c`
- `ts_103190_tables_part2.c`

```bash
MACINDECODE_AC4_SPEC_DIR=/absolute/path/to/spec \
  cargo build --features macindecode-ac4-bitstream/audio-decode
```

构建脚本使用 crate 内置摘要校验实际读入的三个文件；缺失、版本不匹配或内容变化
都会直接终止构建。官方 PDF、C 表和生成文件均保持在用户本地。

MSRV 为 Rust 1.85，禁止 unsafe Rust。完整设计、支持矩阵和规范追踪见
[项目仓库](https://github.com/SakuzyPeng/MacinDecode-AC4-Core)。

## License

[MIT](LICENSE)
