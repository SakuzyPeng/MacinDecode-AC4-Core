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

公共 `Ac4Mp4::parse()` 一次收口首个 AC-4 轨、`mdhd`、`dac4` 与 sample table；
`sample_infos()` 统一拒绝混合 sample description，`access_units()` 再把每个 64 位文件
偏移和大小严格定界到完整输入。需要 edit/priming 的调用方按需取得固定容量的
`presentation_timeline()`，并共用同一套整数时间换算、presentation shift 与 media span；
只读 inspect 不需要 `mvhd`，不会被迫接受导出器的额外容器前提。Inspect、CLI 与性能工具
均消费这一入口，不再分别切片或实现时间数学。

底层的 `find_ac4_track`、`SampleTable` 与时间函数继续公开，供需要自行组合 ISO BMFF
结构的调用方使用；高层入口不会解释 AC-4 音频工具语义。

`dac4` DSI v1 额外提供无分配的只读选择信令：program/bitrate、presentation、
substream group、direct-object/A-JOC 分类与 alternative 名称/目标。未知 presentation
版本及规范的 `skip_area` 保持有界不透明；channel group 掩码只表示容器信令，不代表
channel-based PCM 已受支持。解码配置仍必须取自每个 sample 的 TOC。

MSRV 为 Rust 1.98，禁止 unsafe Rust。架构与时间模型见
[项目仓库](https://github.com/SakuzyPeng/MacinDecode-AC4-Core)。

## License

[MIT](LICENSE)
