# crates.io 发布检查

本文定义 workspace 中 7 个公开 crate 的发布前条件与人工发布顺序。
`macindecode-ac4-perf` 是 `publish = false` 的内部 harness，不进入发布队列。CI 和仓库脚本
只验证归档及其构建，不会上传 crate，也不读取 crates.io token。

## 版本与依赖

根 `Cargo.toml` 的 `[workspace.package].version` 是 7 个公开包的版本来源；
`[workspace.dependencies]` 中六个内部库的 `version` 必须与它同步。路径用于工作区
开发，Cargo 打包时会移除路径并保留 crates.io 版本约束。

CLI 的 `macin-atmos` 依赖同时声明 `=0.1.0` 和公开 Git 仓库的固定提交。
Cargo 打包会移除 Git 来源，归档依赖注册表中的 `macin-atmos =0.1.0`。
共享库当前只完成发布准备，尚未上传 crates.io，正式发布前须先解除这一注册表依赖阻挡。
其发布准备记录见 [MacinFormat-ADM-Core](https://github.com/SakuzyPeng/MacinFormat-ADM-Core/blob/main/docs/CRATES_IO_RELEASE.md)。

CI 使用 `python3 scripts/ci_support.py package`（音频配置追加 `--all-features`）验证
workspace 归档及解包构建。该入口从 CLI 清单读取共享库的固定 Git 提交，通过命令行
`patch.crates-io` 将归档构建指向同一来源，覆盖配置不会写入归档清单。正式发布前仍须运行
下述原始 `cargo package` 命令，确认 crates.io 能独立解析全部依赖。

## 发布前门禁

在干净工作树运行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
for crate_license in crates/*/LICENSE; do cmp LICENSE "$crate_license"; done
cargo package --locked --workspace
python3 scripts/check_spec_distribution.py
```

完整音频 feature 还需验证注册表解包后的构建确实只通过显式目录读取规范表：

```bash
python3 -m pip install -r scripts/requirements-spec.txt
./scripts/fetch_specs.py
./scripts/generate_spec_tables.py
python3 scripts/check_spec_distribution.py --generated
MACINDECODE_AC4_SPEC_DIR="$PWD/spec" \
  cargo package --locked --workspace --all-features
```

用 `cargo package -p <crate> --list` 检查每个归档。归档只应包含清单、锁文件、
许可证、README、源码/构建源码，以及 CLI 的测试和 JSON Schema；不得出现 `.env.local`、
规范 PDF/C 表、本地生成表、真实媒体、向量源文件或 `target/` 制品。crates.io 当前限制之外，项目自身
也要求所有归档保持在 10 MiB 以下。

## 人工发布顺序

内部注册表依赖决定顺序：

先确保共享库 `macin-adm` 与 `macin-bwf` 的 `0.1.0` 已发布，再发布 `macin-atmos 0.1.0`，
并等待注册表能解析后再执行本仓库的打包与发布门禁。

1. `macindecode-ac4-bitstream`
2. `macindecode-ac4-decode` 与 `macindecode-ac4-mp4`
3. `macindecode-ac4-metadata`
4. `macindecode-ac4-scene` 与 `macindecode-ac4-inspect`
5. `macindecode-ac4-cli`

每一层发布后应等待 crates.io 索引能够解析该版本，再处理下一层。实际上传、token
配置、crate 名称占用确认和 owner 设置均是人工步骤，不由 CI 执行。

## 发布后抽查

- docs.rs 默认 feature 文档成功生成；
- `cargo install macindecode-ac4-cli --version <version>` 可安装 `macinac4`；
- 新建临时项目能分别解析 `bitstream`、`decode`、`metadata`、`mp4`、`scene` 与 `inspect` 六个库 crate；
- 用同版本 tag 从官方规范生成外部表并设置 `MACINDECODE_AC4_SPEC_DIR` 后，`audio-decode` 能从注册表依赖构建；
- crates.io 页面显示正确的 README、MIT、仓库、关键词、分类、MSRV 与依赖版本。
