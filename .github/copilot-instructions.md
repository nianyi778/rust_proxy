# Copilot Instructions for rust_proxy

## 项目概述

这是一个 Rust 视频流代理服务，专用于 HLS/MP4 Range 场景。核心功能是将播放器请求转发到动态上游源站（每次请求的域名/端口可能不同）。

**单文件架构**: 所有核心逻辑在 [src/main.rs](../src/main.rs)，约 400 行。

## 技术栈

- **Rust 2021** + `tokio` 异步运行时
- **axum 0.6** 作为 HTTP 框架（注意：非最新版本）
- **hyper 0.14** + `hyper-rustls` 用于上游 HTTPS 请求
- **tracing** 做日志

## 核心数据流

```
客户端 → /stream?url=<encoded_upstream_url> → 本服务 → 上游CDN/源站 → 本服务 → 客户端
```

关键行为：
- m3u8 内容会被重写：分片/子清单 URL 改为 `proxy_origin/stream?url=...`
- `#EXT-X-KEY` 的 URI 也会被重写
- Range 头透传（支持视频拖拽/断点续传）
- 若上游返回 403，会自动去掉 Referer/Origin 重试一次

## 开发命令

```bash
# 开发运行
LISTEN_ADDR=0.0.0.0:8080 cargo run

# CI 检查（必须全部通过）
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all

# Release 构建
cargo build --release
```

## 测试现状

- 当前仓库没有专门的单元测试文件，但 CI 仍会执行 `cargo test --all`（见 [README.md](../README.md) 和 [.github/workflows/ci.yml](../.github/workflows/ci.yml)）。
- 若新增测试，请与现有单文件结构对齐，优先就近放在 [src/main.rs](../src/main.rs) 的 `#[cfg(test)]` 模块中。

## 代码约定

- **错误处理**: 使用 `anyhow::Result` 作为 main 返回类型；handler 返回 `Result<Response<Body>, (StatusCode, String)>`
- **日志级别**: 使用 `RUST_LOG` 环境变量控制，默认 `info`
- **CORS**: 所有响应自动添加 `Access-Control-Allow-Origin: *`

## CI/CD 约定

- **Commit 格式**: 遵循 Conventional Commits（`fix:`, `feat:`, `feat!:`）
  - release-plz 根据提交历史自动计算版本号
- **发版流程**: 推送到 main → release-plz 自动开 PR → 合并后自动创建 tag 和 GitHub Release
- **构建目标**: `x86_64-unknown-linux-musl`（静态链接，避免 glibc 版本问题）

## 安全提示（Open Proxy 风险）

- 这是动态上游代理，任何可访问者都可请求任意上游 URL，存在 Open Proxy 风险。
- 现有实现不做白名单/鉴权（见 [README.md](../README.md)），生产环境应由网关层做鉴权/限流/白名单。

## 修改代码时注意

- 路由 `/stream` 和 `/api/proxy/stream` 功能相同，修改时需同步处理
- `rewrite_m3u8()` 和 `rewrite_ext_x_key_line()` 是 m3u8 重写的核心函数
- 反向代理场景需要 `X-Forwarded-Proto`/`X-Forwarded-Host` 头才能正确重写 URL

## 部署

- systemd 配置模板: [deploy/rust-proxy.service](../deploy/rust-proxy.service)
- 唯一环境变量: `LISTEN_ADDR`（默认 `0.0.0.0:8080`）

## Docker 部署（可选）

- 本仓库当前未内置 Dockerfile；如需 Docker 部署，可新增根目录 Dockerfile，使用多阶段构建生成静态二进制。
- 参考流程（构建 + 运行）：
  - `docker build -t rust_proxy:local .`
  - `docker run --rm -p 8080:8080 -e LISTEN_ADDR=0.0.0.0:8080 rust_proxy:local`
- 多阶段构建建议使用 `x86_64-unknown-linux-musl` 目标，确保运行时镜像可用（与 CI 产物一致）。
