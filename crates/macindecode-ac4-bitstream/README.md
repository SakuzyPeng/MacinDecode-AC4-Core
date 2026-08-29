# macindecode-ac4-bitstream

`#![no_std]` 的 Dolby AC-4 比特流语法、拓扑与元数据解析。它负责 raw sync frame、TOC、
presentation/group/substream 拓扑、alternative presentation 选择前缀、EMDF 与 OAMD；
音频数值重建由 `macindecode-ac4-decode` 提供，容器解析由 `macindecode-ac4-mp4` 提供。

本 crate 是独立开源实现，与 Dolby Laboratories 不存在隶属、赞助或认可关系；
相关商标仅用于说明兼容性。

```toml
[dependencies]
macindecode-ac4-bitstream = "0.1.0"
```

本 crate 没有 feature、没有构建脚本，也不含任何规范表：容器无关的 bitstream、
TOC/OAMD/metadata 解析不需要规范附件。需要 ETSI 表的处理一律在
`macindecode-ac4-decode`，见 ADR-0013。

MSRV 为 Rust 1.98，禁止 unsafe Rust。完整设计、支持矩阵和规范追踪见
[项目仓库](https://github.com/SakuzyPeng/MacinDecode-AC4-Core)。

## License

[MIT](LICENSE)
