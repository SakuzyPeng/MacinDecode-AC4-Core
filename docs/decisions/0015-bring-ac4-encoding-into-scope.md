# ADR-0015：把 AC-4 编码纳入项目边界

- 状态：Accepted
- 日期：2026-09-10
- 关系：改写 [ADR-0001](0001-language-and-project-boundary.md) 决策 6 与其被否决方案「直接
  并入现有编码项目」；重新锚定 [ADR-0011](0011-layer-syntax-decode-and-scene.md) 决策 6 与
  [ADR-0013](0013-extract-decode-crate.md) 决策 6 的 FFI 前提；沿用
  [ADR-0007](0007-preprocessed-scene-rust-api-boundary.md) 决策 1 的边界模式；不触动
  ADR-0002–0010 的数值决策，不改变 [ADR-0012](0012-defer-direct-object.md) 的搁置结论

## 背景

`docs/ROADMAP.md` 的「不属于当前路线图的事项」第 3 条写着「AC-4 编码器」，README 首屏写着
「本项目**不负责**……AC-4 编码」，而 ADR-0001 的被否决方案里有一条正是「直接并入现有编码
项目」。要做编码器，必须正面改写这条边界并处理原论证，否则代码与对外承诺互相矛盾。

**技术前提：AC-4 规范是解码器规范。** TS 103 190-1/-2 规定「给定码流怎么解」，不规定「怎么
产生这些码流」。后者完全开放，也正是编码器里真正困难的部分所在。因此「编码」不是一个功能，
而是四样性质不同的东西——其中三样在现有 ADR 下已有明确归属，只有参数估计层是真的新层。把它
整块塞进一个 crate 会让判据体系互相污染。

现状盘点：语法读侧已由 12 条 A-JOC 与 8 条 channel-based 真实码流验证；反量化表、变换常量、
QMF 原型窗与 Huffman 码本编解码共用（`build_support/spec.rs` 已在构建期生成原始的
`(码长, 码字)` 对）；`aspx::qmf::QmfAnalysisState` 是生产级的正向滤波器组。缺口是：全仓库没有
任何 bit writer，正向 MDCT 只在 `asf/imdct/transform.rs` 存在一个 O(N²) 的测试 oracle。

## 决策

1. **边界改写，但只到实验性码流生成器。** AC-4 编码进入项目范围，目标是「能被参考解码器正确
   解出的码流」。商业认证、Dolby 生态合规声明与「产出物可声称为合规 AC-4」仍在非目标内。
   README 与 ROADMAP 同步改写，**不得写成已支持**——本 ADR 决定的是范围，不是能力。

2. **四处归属分别落在四个地方。**
   - **bit writer 与语法写侧 → `macindecode-ac4-bitstream`。** ADR-0011 的层按职责划分而非
     按数据流方向，写侧与读侧是同一职责的两个方向；crate 名即 `bitstream`，不是 `parser`。
     新增的 `writer` 模块必须在 `scripts/check_layers.py` 的层登记表中登记为 `PRIMITIVE`
     （与 `reader` 对等）——该表对未登记的新顶层模块 fail-closed，漏登记即审计失败。
   - **正向 MDCT → 暂留 `macindecode-ac4-decode`（`asf::mdct`）。** 变换表由 decode 的
     `build.rs` / `build_support/` 生成并由冻结 SHA-256 闭锁，搬表要重做高精度审计；decode
     已经含有正向 DSP（`QmfAnalysisState`），它实质是「恰好叫 decode 的 DSP crate」。按
     ADR-0013 的先例——decode 本身就是先在 CLI/bitstream 里长出来才提取的——先在原地落地，
     将来评估提取 `macindecode-ac4-dsp` 时另立 ADR。ADR-0011 决策 7「拆分不得混入行为变更」
     同样要求提取与新功能分开做。
   - **心理声学、码率控制与 A-SPX/A-JOC 参数估计 → 新 crate `macindecode-ac4-encode`。**
     它依赖 bitstream 与 decode，与 `decode → bitstream` 同向，不制造环。
   - **编码器输入类型 → 独立类型，语义对齐 `Ac4SceneFrame` 但不共享表示。** ADR-0007 决策 1
     已经定下同构模式：处理器返回独立的 `ProcessedPresentationFrame`，不得原地改写场景帧。
     物理原因是 `Ac4SceneFrame<'a>` 只借用 `pub(crate)` 的 `SceneFrameStorage`，外部构造不
     出来（`crates/macindecode-ac4-scene/src/model.rs:1476`）。MacinDecode-AC4-Player 已有
     可循的先例：其 `decoder::worker` 把借用的 Scene view 复制为宿主自有的对象/LFE PCM、
     稳定元素 ID 与 ramp 更新，「Core 类型不会越过该适配边界」，后端也「不得接收
     `Ac4SceneFrame`，不得反向影响解码器的数据模型」。**不得为编码器弯折解码器的零拷贝借用
     设计。**

3. **判据体系隔离。** encode crate 的大部分内容没有条款可引，`SPEC_TRACEABILITY` 的追踪矩阵
   **不得**为它生成没有条款的行。率失真与听测判据另行记录。这条不是洁癖：整个仓库的可信度
   建立在「规范条款 ↔ 实现 ↔ 测试可互相追踪」上，混入一批引不到条款的行会让读者无法分辨
   哪些结论有规范依据。

4. **自建编码器不得进入测试向量生产链。** 12 条基线向量继续由外部 DME/DEE 链产出，ADR-0001
   决策 3 不变。用自己的编码器造向量来测自己的解码器是同源错误，会一次性毁掉基线的独立性。

5. **阶段门：语法写侧立即可开，音频段等差分裁决。** round-trip（parse → write → 与原始码流
   逐字节相同）比的是字节、不经过任何数值路径，对 `SPEC_TRACEABILITY` 第 7 节的 17 条未裁决
   判读免疫，因此不设前置。音频段必须等第 7 节裁决完成：编码器的自验环是 encode → decode →
   compare，未裁决判读会被编解码两侧同时继承，环对它们完全失明——表 D.2 的实虚序、
   `Pseudocode 90` 的 `num_ts_in_ats` 倍率、LFE 的 `δ_ASPX` 都属于这一类。

## 重新锚定 C ABI 的前提

ADR-0011 决策 6 与 ADR-0013 决策 6 把版本化 `macindecode-ac4-ffi` 挂在「Rust Scene API 稳定
与真实宿主所有权需求出现」之后（ADR-0012 已把 direct-object 从该清单移除）。

**「真实宿主所有权需求出现」这个条件其实已经满足了，只是被绕过去而没有记账。**
MacinDecode-AC4-Player 是真实宿主：它有实时线程、2 秒有界 Scene FIFO、seek/replay epoch、两套
平台输出后端和显式 16 MiB 栈。它没有使用 C ABI——因为它选择了 Rust 技术栈，而这个选择正是在
缺少 ABI 的前提下做出的。**这是受约束的选择，不是「不需要 ABI」的证据。** 其后果是整个生态
目前只能被 Rust 宿主消费。

本 ADR 只把这笔代价记上账并改述前提，**不裁决 ABI 本身**：ABI 是否建立、何时建立仍待另一份
ADR。需要说明的是它与编码器的关系——CLI 形态的编码器（ADM/DAMF 进、AC-4 出）不需要 ABI，
现有向量链就是这么使用外部编码器的；只有把编码器嵌入 DAW/NLE 这类宿主时才需要，而那类宿主
以 C++ 为主。因此 ABI 不在编码器的关键路径上。

## 理由

- ADR-0001 否决「直接并入现有编码项目」的论证是它会「混合编码测试工具、专有本地 runtime
  发现、容器处理与产品解码核心，增加平台耦合并削弱测试独立性」。**自建编码器不引入其中任何
  一项**：它不含厂商工具路径，不做本地 runtime 发现，而测试独立性由决策 4 明确保护。原论证
  针对的是「并入那个特定的外部项目」，不是「不得有编码能力」。
- 分离 `macindecode-ac4-encode` 使解码用户不为编码器付编译时间与体积，也守住 CI 那两条
  thumbv7em 门禁——心理声学模型大概率需要 alloc 与重浮点，扛不住 decode/scene 的 `no_std`
  约束。
- 阶段门让第一段交付物能独立收口：写侧语法 + `bitstream` 中 54 处 `skip_bits` 的逐处待决
  清单 + 逐字节判据。即使编码器后续不再推进，这段对解码侧的语法完整性验证依然成立——它回答
  的是「我们到底完整理解了多少语法」，而这个问题现在没有答案。

## 影响

正面影响：

- 对外承诺与实际范围重新一致，读者不会再读到「不负责 AC-4 编码」却看见编码代码。
- 四处归属各自锚定在既有 ADR 上，不需要为编码器新造一套架构原则。

代价：

- **feature 轴从三条变四条**（默认 / `spec-tables` / `audio-decode` / encode），CI 三个 job
  需要相应扩展。CLAUDE.md 已把「只测一个配置」列为本仓库最常见的失误，而「`fmt` 只在第一个
  job 检查、MSRV 只在开启 feature 时验证」这两处会更容易漏。
- 支持矩阵与规范可追踪性的读者需要分辨哪些行属于解码、哪些属于编码；决策 3 是防止两者混淆的
  唯一保障。

## 被否决方案

### 单一 `macindecode-ac4-encode` 承载全部四部分

会把 bit writer 与语法写侧从它们所属的层里挪走，`check_layers.py` 的方向审计随之失去意义，
round-trip 判据也从 crate 内变成跨 crate。而这四部分里恰恰是最不需要新家的那两部分。

### 在 bitstream / decode 内直接加 `encode` feature

feature 轴爆炸，且 `no_std` 约束扛不住心理声学模型；CI 的两条 thumbv7em 门禁守的正是这两个
crate。CLAUDE.md 已经记过 `#[cfg]` 加在内容混合的模块上会在另一配置下断裂。

### 等 C ABI 或 channel-based 完成之后再开始

C ABI 是果不是因：真实宿主已经出现并用 Rust 绕过了它，等它不会带来新信息。channel-based 则
已按 A-JOC 优先的取舍移出编码器计划——它是播放器侧可能出现的需求，与编码器路线无关。

### 先做音频段，语法写侧随后补

顺序反了。语法写侧是唯一有唯一正确答案的部分，也是唯一不受第 7 节未裁决判读影响的部分；
先做它能在裁决进行的同时并行推进，反过来则会把 17 条判读原样复制进编码器且无从察觉。
