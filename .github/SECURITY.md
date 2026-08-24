# 安全策略 / Security Policy

## 支持范围

安全修复优先应用于 `main` 和最新 GitHub Release。早期 `0.x` 版本仅在可行时回补；
请先确认问题在最新提交或最新发布版中仍可复现。

本策略适用于解析、解码、时间线、ADM/DAMF/PCM 导出及 CLI 输入处理中的安全问题，
包括恶意输入导致的越界、资源耗尽、拒绝服务、错误文件写入或安全边界绕过。
一般兼容性问题、功能请求和不涉及安全影响的崩溃可以使用普通 issue。

## 私密报告

请勿在公开 issue、讨论或 pull request 中披露尚未修复的漏洞细节。

仓库公开并启用 GitHub Private Vulnerability Reporting 后，请从仓库的
**Security → Advisories → Report a vulnerability** 提交私密报告。如果该入口不可见，
请只在普通 issue 中说明“需要私密安全报告渠道”，不要附加漏洞细节或复现文件。

报告最好包含：

- 受影响版本或 commit、操作系统与架构；
- 安全影响和受影响的信任边界；
- 最小复现步骤、命令和必要日志；
- 尽量使用合成或最小化输入，不要提交客户媒体、受版权保护的音频、规范 PDF、
  专有二进制、凭据或其他不可再分发材料；
- 已知缓解措施或修复建议（如有）。

维护者会先确认报告并协调修复与披露时间。项目目前不设漏洞赏金计划。

---

## Supported Versions

Security fixes target `main` and the latest GitHub Release. Earlier `0.x` releases receive
backports only when practical. Please first confirm that the issue is reproducible on the latest
commit or release.

This policy covers security defects in parsing, decoding, timelines, ADM/DAMF/PCM export, and
CLI input handling, including malicious-input crashes, resource exhaustion, denial of service,
unsafe file writes, or security-boundary bypasses. Compatibility questions, feature requests,
and crashes without a security impact may use a regular issue.

## Private Reporting

Do not disclose an unpatched vulnerability in a public issue, discussion, or pull request.

After the repository is public and GitHub Private Vulnerability Reporting is enabled, use
**Security → Advisories → Report a vulnerability**. If that option is unavailable, open a regular
issue containing only a request for a private reporting channel; do not include vulnerability
details or reproduction files.

Please include, when possible:

- the affected version or commit, operating system, and architecture;
- the security impact and affected trust boundary;
- minimal reproduction steps, commands, and relevant logs;
- a synthetic or minimized input instead of customer media, copyrighted audio, specification
  PDFs, proprietary binaries, credentials, or other non-redistributable material;
- any known mitigation or suggested fix.

Maintainers will acknowledge the report and coordinate remediation and disclosure. This project
does not currently operate a bug bounty program.
