# macindecode-ac4-scene

面向 AC-4 Core 与 Full A-JOC 的容器无关、流式渲染前场景 API。该 crate 提供
`Ac4SceneFrame` 数据契约、presentation 选择、整数采样时间线、结构化错误和
`Ac4DecoderSession`，并以借用视图发布对象/LFE PCM 与 OAMD 状态。

成功解码的 AU 还可从 `DecodedAccessUnit::presentation_metadata` 借用所选 presentation 的
完整 processing-metadata payload、当前帧解析视图、有效 DRC 配置和 substream-group gain
码值。该 AU 级侧车只解析、验证和保留原值，不进入 `Ac4SceneFrame`，也不执行 DRC、gain、
pan、downmix、loudness correction 或 renderer 处理。

容器或系统层已经解析的 presentation metadata 可由调用方放入泛型
`PresentationSelectionMetadata<T>`；已选择的
`ScenePresentation::match_selection_metadata` 只按双方唯一的 effective presentation ID
关联，并返回原 entry 的只读视图。没有 ID 时只允许双方各自唯一的无 ID 项回退，重复 ID 或
多路无 ID 保持歧义。metadata 不参与 TOC 驱动的解码配置，因此 `T` 可以直接是
`macindecode-ac4-mp4` 的借用 DSI envelope，未知版本仍保留其原始定界 body，而本 crate
不需要依赖 MP4。opaque body 的身份必须标为 `Unavailable`，不能把尚未解析 ID 猜成明确
无 ID 后走唯一回退；只要集合仍含这类身份不可用项，关联结果就是 `Indeterminate`，不会
把其余已知候选提前宣称为唯一。

```toml
[dependencies]
macindecode-ac4-scene = "0.1.0"
```

默认构建提供 `#![no_std]` 场景模型与控制面。启用完整音频路径：

```toml
[dependencies]
macindecode-ac4-scene = { version = "0.1.0", features = ["audio-decode"] }
```

`audio-decode` 会转发到 `macindecode-ac4-decode/audio-decode`，因此需要用户从
官方 ETSI PDF 本地生成的 Rust 表与外部 C 表。它们不会随 crate 分发；注册表构建
须设置 `MACINDECODE_AC4_SPEC_DIR`。
目录格式与获取方式见
[`macindecode-ac4-decode` 文档](https://docs.rs/macindecode-ac4-decode)。

MSRV 为 Rust 1.98，禁止 unsafe Rust。容器 sample table、priming 与 edit list
换算不属于本 crate，相关功能由 `macindecode-ac4-mp4` 提供。

## 有效耳机策略

renderer 从 `SceneObjectState::headphone_policy()` 读取内容策略：

- `Resolved(policy)`：`render_mode()` 为 Bypass/Near/Mid/Far，`head_tracking()` 为
  SceneRelative/HeadRelative。设备能力、用户设置和实时头姿仍由 renderer 处理。
- `Unspecified`：内容没有可解析的声明，不等于禁用头追；采用何种默认播放策略由调用方决定。
- `Unsupported(issue)`：保留模式或 group 歧义等使语义不能确定。原始数据和 PCM 仍保留，
  `semantic_complete(SemanticScope::All)` 及帧的语义完整性标记为假；它不等于 `DecodeError::Unsupported`。

Scene 按 `TS103190-2:v1.3.1:6.3.9.10a–11` 的操作模式选择控制源：Stereo 为
Bypass/HeadRelative；默认 Near/Far 使用全局位；Manual 使用逐对象位。未指定的 common
不借用逐对象字段猜测操作模式。多 group 引用同一物理子流时，所有适用组的有效策略必须一致。

每帧起点的 `initial_state()` 已包含 offset 0 控制。调用方先建立完整起点状态，再按采样偏移
调度 `metadata_updates()`；`HEADPHONE_POLICY` 表示有效策略变化，策略在偏移处离散切换，
不使用事件的位置/增益 ramp。全局变化也能产生对象事件，即使没有新的逐对象 OAMD 块。
同一时刻全部控制合并后才确定耳机策略；raw 块顺序保留，只有最后相关事件标记策略变化。
warm-up 尚无状态时仍为 `None`；reset/discontinuity 后应清除 renderer 的历史。

可编译的调度示例位于 `examples/headphone_policy.rs`。它保留策略的未指定/不支持状态，
并把整数 offset 交给调用方的音频调度器，不在解码期间提前执行未来的渲染更新。

### 事件接口迁移

| 原调用 | 当前契约 |
|---|---|
| `state.headphone()` | 保持逐对象含义；最终内容策略改读 `headphone_policy()`。 |
| `MetadataFields::HEADPHONE` | 保持逐对象字段变化含义；renderer 监听 `HEADPHONE_POLICY`。 |
| `update.raw().block()` | `raw()` 返回 `Option<RawOamdUpdate>`；common 派生事件为 `None`。 |
| 单一 control-source AU | `control_source_access_unit_index()` 返回 `Option<u64>`；共同来源不同时为 `None`。 |

完整来源分别由 `raw()` 和 `common_origin()` 提供。后者保留 group mask 和自己的
control source AU；共同来源可以同时存在，不能把 `None` 理解成“没有来源”。逐对象 raw、
`RawOamdState` 及帧级 `oamd_common_states()` 继续无损保留，诊断/格式导出可独立读取。

## License

[MIT](LICENSE)

### 语义完整性检查范围

`SceneObjectState::semantic_complete(scope)` 现在要求显式选择 `SemanticScope`。
原无参数调用迁移为 `SemanticScope::All`，保留包括耳机策略在内的完整性检查；
只消费通用空间语义、并自行处理耳机降级的 renderer 使用 `SemanticScope::Spatial`。
后者仍拒绝未知的空间语义，不修改 `headphone_policy()` 的 `Unsupported` 原因。
`Unspecified` 在两种范围下都不是错误；状态尚未到齐的 `None` 与语义完整性仍分开判断。
帧级 `semantic_metadata_complete()` 继续按 `All` 汇总，PCM 和导出严格判定不变。
