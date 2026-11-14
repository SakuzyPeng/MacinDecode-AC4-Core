# ADM/DAMF 写侧迁移

本迁移分支使用私有仓库 MacinFormat-ADM-Core 的 macin-atmos，依赖提交由 Cargo.toml/Cargo.lock 固定。
解码、OAMD 映射、presentation 选择、edit/preroll、对象配对、探针与节目级 PCM 增益仍归本仓库。
AXML、CHNA、DBMD、DAMF 文本及母版容器序列化由共享库实现。

本次使用已知元数据的批量入口，以保留既有 CLI 输出。共享库另提供 create/write_block/finish 的流式入口，
供聚类等调用方使用；本次不把整批 AC-4 解码链改造成流式链。

真实 768K 向量回归核对了 Standard/Logic ADM 和 DAMF：
PCM、AXML、CHNA、DBMD、manifest 与 metadata 均与迁移前逐字节一致。
CLI 命令、JSON v1、严格映射诊断和不覆盖目标的提交行为保持。

默认构建直接使用固定 Git 提交；本地开发可通过被忽略的 Cargo patch 配置指定共享库 checkout。
路径覆盖会改变 Cargo.lock，提交前须恢复固定 Git 依赖并重新运行 locked 检查。
本机继续复用已有 target；不复制媒体、规范表或第三方工具。

私有共享库尚未在 crates.io 分发，因此本分支不能作为公开发布分支。
现有打包、分发和发行检查不放宽；待共享库公开分发后，再将依赖切到正式版本并处理公开集成。
