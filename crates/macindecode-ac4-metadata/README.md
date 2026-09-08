# macindecode-ac4-metadata

逐个已定界 `raw_ac4_frame` 观察源 AU 元数据。crate 使用 `no_std + alloc`，输入已经剥离
sync wrapper；MP4、文件读取及容器 edit 属于外层适配器。

```rust
use macindecode_ac4_metadata::{
    Ac4MetadataConfig, Ac4MetadataSession, AccessUnit, AccessUnitContext, MetadataError,
};

fn receive(session: &mut Ac4MetadataSession, raw: &[u8], index: u64) -> Result<(), MetadataError> {
    let metadata = session.observe_access_unit(AccessUnit::new(raw, AccessUnitContext::new(index)))?;
    for presentation in metadata.presentations() {
        if let Some(view) = metadata.presentation_metadata(presentation.presentation_index) {
            let configuration = view.effective_drc_configuration();
            let gains = view.effective_group_gain_codes();
            let _ = (configuration, gains);
        }
    }
    // 分区诊断与成功数据同时可见；调用方决定所需分区是否足够完整。
    for diagnostic in metadata.diagnostics() { let _ = diagnostic; }
    Ok(())
}

let mut session = Ac4MetadataSession::new(Ac4MetadataConfig::new())?;
# Ok::<(), MetadataError>(())
```

默认 `MetadataDetail::Basic` 不依赖 decode 或规范表，保留 TOC、presentation、外层 audio
metadata、group OAMD，以及能够无表还原的 standalone OAMD。完整 typed payload 与 opaque
字段可从借用视图和原 payload 取得。unknown、reserved 或无法派生的上下文不会被伪造成缺席。

`MetadataDetail::Full` 需要显式启用 `metadata-decode`，并按 decode crate README 准备本地
规范表。后端使用独立 `FullAjocSyntaxDecoder` 解析已支持的动态 A-JOC 语法，保留两侧 OAMD、
alternative 边界、DRC gains 与有效 DE 参数；不创建 PCM、IMDCT overlap、QMF 或 A-JOC 重建
状态。该 feature 不包含 `audio-decode`。缺少后端时 `new` 返回 `FeatureUnavailable`。

默认观察全部 presentation；`PresentationScope::Selected` 支持 `AutoUnique`、Index 和 Id。
选择与 PCM 解码支持门禁分离，ID 重复或缺失时保持结构化选择失败。

输出只借用 Session 的当前存储，有效期截至下一次可变调用。跨调用保留历史需显式复制。
对象由 `(observation_epoch, configuration_generation, substream_index, domain, object_index)`
定位，Core、Full、Standalone 不共用索引空间。状态是按源码流顺序还原的目标值；offset/ramp
仍按规范的整数采样单位保留，可以跨过当前 AU。这里不执行 ramp 插值，不把它声明为播放
起点状态，也不应用表 188 的 PCM/control 对齐。

分区失败会清除该分区及依赖历史，其他成功分区仍可发布；AU 边界越界返回错误。定界前的
`NeedMoreData` 不推进状态或时间。seek/splice 使用 `mark_discontinuity`；`reset` 开启新的
观察域。等待随机访问点时仍可观察 TOC/拓扑，继承数据不可用。稳定配置复用状态、payload
及更新缓冲，但不承诺硬实时。

`layout` 提供共享的 Core 坐标模板和空间属性判据。它们识别 metadata 几何；可否直接导出
PCM 仍取决于导出器额外的增益、活动状态、LFE 信号与覆盖范围验证。
