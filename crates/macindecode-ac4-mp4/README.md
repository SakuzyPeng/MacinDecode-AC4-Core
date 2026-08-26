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

`dac4` DSI v1 额外提供无分配的只读选择信令：program/bitrate、presentation、
substream group、direct-object/A-JOC 分类与 alternative 名称/目标。未知 presentation
版本及规范的 `skip_area` 保持有界不透明；channel group 掩码只表示容器信令，不代表
channel-based PCM 已受支持。解码配置仍必须取自每个 sample 的 TOC。

MSRV 为 Rust 1.98，禁止 unsafe Rust。架构与时间模型见
[项目仓库](https://github.com/SakuzyPeng/MacinDecode-AC4-Core)。

## License

[MIT](LICENSE)
