# CI/CD 说明（适用范围与使用方式）

本文档说明本仓库的 GitHub Actions CI/CD 的作用、触发方式、产物形态与适用范围，便于对外开源后维护与协作。

## 目标与非目标

**目标**

- 每次提交自动做基础质量检查：格式、静态检查、测试、可构建。
- 自动版本管理（自动开 Release PR、合并后自动打 tag / 生成 GitHub Release）。
- 自动产出可直接部署到 Linux 服务器的二进制包，并附带校验文件。

**非目标**

- 不负责自动部署到你的服务器（SSH/scp/systemd 重启等未内置）。
- 不发布到 crates.io（本项目是服务端二进制，当前配置为不发布 crate）。

## Workflows 总览

| Workflow | 文件 | 触发条件 | 作用 | 产物 |
|---|---|---|---|---|
| CI | `.github/workflows/ci.yml` | `push` / `pull_request` | fmt/clippy/test/build | Actions Artifact（用于临时下载） |
| 自动版本 + Release | `.github/workflows/release-plz.yml` | `push` 到 `master/main`，或手动触发 | 自动开 Release PR；合并后自动创建 tag/Release，并上传二进制附件 | GitHub Release Assets（长期可下载） |
| tag 发版构建 | `.github/workflows/release.yml` | `push` tag `v*` | 针对手工打 tag 的场景构建并上传附件 | GitHub Release Assets |
| 手动补传附件 | `.github/workflows/upload-assets.yml` | 手动触发（输入 tag） | 给已存在的 Release 补传/重传二进制附件 | GitHub Release Assets |

## CI（`.github/workflows/ci.yml`）

做的事情：

- `cargo fmt -- --check`
- `cargo clippy -- -D warnings`
- `cargo test`
- `cargo build --release`
- 上传 `target/release/rust_proxy` 作为 Actions Artifact

适用范围：

- 适合所有 PR 和主分支提交的“质量门禁”。
- Artifact 适合临时分享/内部测试，**不适合作为长期对外下载渠道**（有保留期、可能需要登录）。

## 自动版本 + Release（`.github/workflows/release-plz.yml`）

这个 workflow 负责“自动打版本号 + 自动创建 Release”，并在创建 Release 后**直接构建并上传 Linux 可执行文件**。

### 版本号如何决定

使用 [release-plz](https://github.com/release-plz/release-plz) 根据提交历史（推荐 Conventional Commits 风格）自动计算下一版本号。

常见约定（建议）：

- `fix:` → patch：`0.1.0 -> 0.1.1`
- `feat:` → minor：`0.1.0 -> 0.2.0`
- `feat!:` 或正文包含 `BREAKING CHANGE:` → major：`1.0.0 -> 2.0.0`

> 如果提交信息不遵循约定，release-plz 可能不会提升版本号（例如 next version 仍然是 `0.1.0`）。

### 发布到哪里

- 只发布到 GitHub Releases（创建 tag + Release）。
- 不发布到 crates.io：见 [release-plz.toml](../release-plz.toml) 的 `publish = false`。

### 权限要求（非常重要）

release-plz 需要创建/更新 PR。

在 GitHub 仓库设置里开启：

- Settings → Actions → General → Workflow permissions
  - 选择 **Read and write permissions**
  - 勾选 **Allow GitHub Actions to create and approve pull requests**

否则会出现 403：`GitHub Actions is not permitted to create or approve pull requests.`

### 二进制兼容性（musl 静态）

Release 附件使用 `x86_64-unknown-linux-musl` 构建，为 **musl 静态链接**：

- 优点：不依赖目标机 glibc 版本，兼容较老的 Ubuntu（避免 `GLIBC_2.xx not found`）。
- 限制：仅覆盖 `x86_64`；如果你需要 ARM（例如 `aarch64`），需要扩展 workflow 增加对应 target。

## tag 发版构建（`.github/workflows/release.yml`）

当你手工创建并 push tag（形如 `v1.2.3`）时触发，用于构建并向对应 Release 上传附件。

适用范围：

- 你不使用 release-plz 时（完全手工管理 tag/Release），仍能有“一键构建并上传附件”的能力。

## 手动补传附件（`.github/workflows/upload-assets.yml`）

当某个 Release 已存在但缺少附件、或你升级了构建方式（比如从 glibc 动态改成 musl 静态）需要回填旧版本附件时使用。

使用方式：

- GitHub → Actions → `upload-assets` → Run workflow
- 输入 `tag`（例如 `v0.1.0`）

该 workflow 会：

- 校验该 tag 对应的 GitHub Release 存在
- 构建并上传附件到该 Release

## 对外开源时的建议

- 把对外可下载渠道定位为 **GitHub Releases**；Actions Artifacts 只用于临时测试。
- 对外说明“这是动态上游代理”，在 README 保留安全提示（Open Proxy 风险）。
- 若后续要做“自动部署到服务器”，建议通过 GitHub Environments + Secrets（SSH key）实现，并加入审批/白名单控制。
