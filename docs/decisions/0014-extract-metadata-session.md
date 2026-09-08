# ADR-0014：独立的源 AU metadata Session

- 状态：Accepted
- 日期：2026-09-09
- 关系：补充 ADR-0007、ADR-0011 和 ADR-0013；保持 Scene presentation processing 前的边界。

## 决策

新增 `macindecode-ac4-metadata`（`no_std + alloc`），由 Scene 和 Inspect 共用有界 AU 输入、
presentation 选择、控制事务、presentation DRC/group-gain、audio 上下文/DE 与 group OAMD
状态。raw/quantized OAMD 继承原语继续属于 bitstream，播放对齐和映射后的 Scene 语义继续
属于 Scene。这不是单独按 OAMD 规范章节拆包。

decode 新增 `metadata-decode`，供独立音频语法驱动和 Huffman metadata 使用；`audio-decode`
包含它并额外启用音频重建引擎。规范表构建和校验仍只有 decode 一份真相源。音频语法驱动
不判断能否重建 PCM；ASF/A-SPX/Full 支持凭证由音频消费层计算。声道矩阵处理移到单独的
`channel::matrix` 音频模块。层门禁解析子模块及 re-export，防止 facade 隐藏 DSP 依赖。

## 时间、状态与失败

metadata 输出按源 AU 定位，保留配置代次、观察域、原始和可用的整数 timing、逐块目标状态。
offset/ramp 可以跨 AU，观察器不执行插值或表 188 对齐。Scene 的控制到期状态与源顺序观察
使用同一个 bitstream OAMD 继承原语，分别持有实例。

prepare/commit 候选在成功后发布。Scene 必须在所需语法、DSP 和组装全部成功后提交，并可
直接接收 engine 已验证的 audio observation；独立观察器按分区记录失败和失效依赖，保留
独立成功数据。已确认 AU 边界越界是错误，定界前输入不足可以重试。随机访问门禁不发布
虚构的继承状态。借用生命周期限定输出有效期，工作区复用，不持有整文件历史。

## Inspect 与兼容

原 inspect 入口固定为 Basic，完整扫描通过 options / `--metadata-detail full` 显式开启。
路径和 reader 入口继续逐 packet 读取。旧字段保留 canonical baseline 和独立帧采样规则。
新增 `core_layouts` 分别保存声明、实际网格及派生扬声器布局；实际网格按观察域和配置代次
持续汇总，未知拓扑、失效历史和缺失 common 均限制派生结论的覆盖范围。

DSI 声明按唯一 effective ID 关联，无 ID 只允许唯一回退，未知身份阻止猜测性关联。Core
坐标模板与纯 metadata 判据由报告和 CoreCAF 共用；增益、importance、耳机变化不自动成为
几何变化，CAF 仍额外验证可直接写 PCM 的条件。布局冲突同时保留声明和观测证据。

Scene 原有输入、选择和 presentation 侧车公共路径通过 re-export 保留；Full decoder 的
既有语法出口仍保留。CLI envelope 版本不变，inspectResult 新增 typed Core 区块。

## 验证

独立验证无表、metadata-decode、spec-tables 和 audio-decode 构建，包含 no_std 目标、局部
失败和截断事务、共享引用、对象域/时间边界、Core 网格及 DSI 关联。真实媒体比较源 AU
metadata 与 Scene 的 control source AU，并运行既有 Core/A-SPX/objects PCM 和轨迹基线。
