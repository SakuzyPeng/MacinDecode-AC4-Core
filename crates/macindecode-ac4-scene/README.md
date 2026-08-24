# macindecode-ac4-scene

面向 AC-4 Core 与 Full A-JOC 的容器无关、流式渲染前场景 API。该 crate 提供
`Ac4SceneFrame` 数据契约、presentation 选择、整数采样时间线、结构化错误和
`Ac4DecoderSession`，并以借用视图发布对象/LFE PCM 与 OAMD 状态。

```toml
[dependencies]
macindecode-ac4-scene = "0.1.0"
```

默认构建提供 `#![no_std]` 场景模型与控制面。启用完整音频路径：

```toml
[dependencies]
macindecode-ac4-scene = { version = "0.1.0", features = ["audio-decode"] }
```

`audio-decode` 会转发到 `macindecode-ac4-bitstream/audio-decode`，因此需要用户从
官方 ETSI PDF 本地生成的 Rust 表与外部 C 表。它们不会随 crate 分发；注册表构建
须设置 `MACINDECODE_AC4_SPEC_DIR`。
目录格式与获取方式见
[`macindecode-ac4-bitstream` 文档](https://docs.rs/macindecode-ac4-bitstream)。

MSRV 为 Rust 1.85，禁止 unsafe Rust。容器 sample table、priming 与 edit list
换算不属于本 crate，相关功能由 `macindecode-ac4-mp4` 提供。

## License

[MIT](LICENSE)
