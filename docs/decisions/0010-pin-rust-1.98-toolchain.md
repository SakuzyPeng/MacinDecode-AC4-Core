# ADR-0010：固定 Rust 1.98.0 并提升 MSRV

- 状态：Accepted
- 日期：2026-08-26
- 关系：延续 [ADR-0001](0001-language-and-project-boundary.md) 的 Rust 2024 与稳定工具链边界

## 背景

工作区此前声明 MSRV 1.85，但日常开发使用 Rust 1.96，CI 与 release 又通过未固定的
`stable` 通道构建。同一提交可能随 stable 发布而获得不同的 rustfmt、Clippy、标准库和
代码生成结果，MSRV job 也需要长期维护一套已经远离开发基线的兼容路径。

Rust 1.98.0 已通过默认配置、`audio-decode`、`no_std`、Rustdoc、crate 打包、release
构建和本地真实媒体回归。四个公开 crate 尚未发布，仓库也没有版本 tag 或 GitHub Release，
因此可以在首次发布前统一收紧工具链边界。

## 决策

1. workspace 的 MSRV 提升为 Rust 1.98；所有公开 crate 继续继承根清单中的
   `rust-version`。
2. 仓库用 `rust-toolchain.toml` 精确固定 Rust 1.98.0、minimal profile、rustfmt 与
   Clippy。日常命令、CI 和 release 均服从该文件，不再单独跟随 `stable`。
3. CI 保留独立的 `msrv` job，并使用不含版本号的稳定检查名；未来提升 MSRV 时不再迁移
   分支保护上下文。
4. 后续 Rust 升级必须在同一变更中更新 Cargo 元数据、工具链文件、当前支持文档与 CI，
   并重新通过默认配置、`audio-decode`、`no_std`、打包和多平台 release 门禁。
5. 编译器版本属于性能测量环境。已有 Rust 1.96 的 ADR、性能文档和实验 JSON 保持原样；
   新编译器的性能结论只能作为新的独立测量记录加入。

## 影响

正面影响：

- 本地、CI 与 release 使用相同的 rustc、Cargo、rustfmt 和 Clippy，构建结果可复现。
- 公开 crate 的最低编译器要求由 Cargo 元数据直接表达，低版本会在解析依赖时明确失败。
- 后续工具链升级成为显式、可审计的仓库变更，不会由 stable 通道静默触发。

代价：

- Rust 1.85 至 1.97 不再受支持，下游必须先升级工具链。
- 每次采用新 stable 都需要显式提交并重新运行完整门禁。
- release runner 会安装工具链文件声明的 rustfmt 与 Clippy，即使发布构建本身不调用它们。

## 未采用的方向

继续保留 MSRV 1.85 会让兼容路径与实际开发环境继续分叉；仅在本地固定版本而让 CI/release
跟随 stable 仍会保留不可预测的 lint 与代码生成变化；覆盖既有 Rust 1.96 性能记录则会破坏
测量 provenance。三者均不采用。
