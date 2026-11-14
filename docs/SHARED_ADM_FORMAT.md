# ADM/DAMF 写侧迁移

本仓库使用公开仓库 MacinFormat-ADM-Core 的 macin-atmos，依赖提交由 Cargo.toml/Cargo.lock 固定。
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

共享库已准备 `0.1.0` 的包元数据、许可证与 README，并通过归档解包构建及发布 dry-run，尚未上传 crates.io。
CLI 的依赖同时声明 `=0.1.0` 与固定 Git 提交，源码构建使用公开 Git 依赖，Cargo 归档会转成注册表版本约束。
CI 通过 `scripts/ci_support.py package` 从 CLI 清单读取固定 Git 提交，为归档验证构建设置临时
`patch.crates-io`。正式上传共享库并待索引可用后，须运行原始 `cargo package` 命令，
核对 workspace 默认与 all-features 配置能独立解析注册表依赖，见
[crates.io 发布检查](CRATES_IO_RELEASE.md)。

DAMF stem 保留尾随句点或空格，因为实际成员名会追加 `.atmos`、`.atmos.metadata` 和 `.atmos.audio`。
回归覆盖显式 `--stem`、从输入文件名提取 stem、probe/full 及 home/3dof 配置。
