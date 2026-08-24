# 多平台二进制发布

`.github/workflows/release.yml` 自动构建带 `audio-decode` 的完整 `macinac4` CLI。
构建不依赖仓库内的规范文件：每个 GitHub runner 都会根据
`spec/MANIFEST.json` 从 ETSI 官方地址下载并校验规范，在 runner 本地生成静态表，
随后只上传二进制归档。PDF、C 表和生成的 Rust 表均不会成为 Actions 产物或
GitHub Release 附件。

## 触发方式

- `main` 上影响构建或打包的变更会生成保留 14 天的 Actions 产物；
- `workflow_dispatch` 可从 GitHub Actions 页面手动生成同样的产物；
- 推送与工作区版本一致的 `v<version>` tag 会额外创建 GitHub Release；
- 每周一执行一次只含 `cargo audit --deny warnings` 的依赖审计，不重复构建。

例如，根 `Cargo.toml` 的工作区版本为 `0.1.0` 时：

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

tag 与版本不一致时，工作流会在任何平台开始构建前失败。工作流不会发布
crates.io 包；crate 发布仍按 [crates.io 发布检查](CRATES_IO_RELEASE.md) 人工执行。

## 发布目标

| 系统 | 架构 | Rust target | 归档 |
|---|---|---|---|
| Linux | x86_64 | `x86_64-unknown-linux-musl` | `.tar.gz` |
| Linux | ARM64 | `aarch64-unknown-linux-musl` | `.tar.gz` |
| macOS | Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS | Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows | x86_64 | `x86_64-pc-windows-msvc` | `.zip` |
| Windows | ARM64 | `aarch64-pc-windows-msvc` | `.zip` |

Linux 使用 musl 静态目标，避免把构建 runner 的 glibc 版本变成用户系统的最低要求。
每个归档包含 `macinac4`、中英文 README、MIT 许可证、`Cargo.lock`、规范锁定清单和
`BUILD_INFO.txt`；同目录提供单包 `.sha256`，完整产物另含汇总的 `SHA256SUMS`。

下载后可验证完整产物：

```bash
sha256sum --check SHA256SUMS
```

macOS 也可使用 `shasum -a 256` 对照 `SHA256SUMS` 中记录的摘要。

当前流程不配置 Windows/macOS 商业代码签名，也不执行 Apple notarization；这些步骤
需要发布者证书和仓库 secrets，后续接入时应保持私钥只存在于受保护的发布环境中。
