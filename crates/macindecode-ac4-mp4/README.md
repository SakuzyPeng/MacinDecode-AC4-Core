# macindecode-ac4-mp4

`#![no_std]` 的 AC-4 ISO Base Media File Format 适配层。它解析 MP4 box、`ac-4`
sample entry、`dac4`、sample table、movie/media header 与 edit list，并用整数时间线
产出每个 AC-4 access unit 的范围和呈现时间。

```toml
[dependencies]
macindecode-ac4-mp4 = "0.1.0"
```

本 crate 只负责容器定界和时间投影，不解释音频工具语义；AC-4 sync frame、TOC、
OAMD 和重建原语由 `macindecode-ac4-bitstream` 提供。

MSRV 为 Rust 1.85，禁止 unsafe Rust。架构与时间模型见
[项目仓库](https://github.com/SakuzyPeng/MacinDecode-AC4-Core)。

## License

[MIT](LICENSE)
