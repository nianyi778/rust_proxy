# Rust 视频流代理实战：动态上游、HLS m3u8 重写与 Linux 部署

本文记录一个很“聚焦”的代理服务：把播放器的请求转发到动态上游 URL，并在 HLS 场景重写 m3u8 清单，确保分片与 key 也继续走代理。

- 仓库：`rust_proxy`
- 日期：2025-12-21

---

## 1. 需求与约束

- 上游 URL 是“动态的”（每次请求都可能不同）
- 播放器希望统一访问我自己的域名路径
- HLS 必须能播放（清单、分片、key 都要能走同一代理入口）
- MP4/大文件希望支持拖拽/断点续传（Range）
- 服务端要能在 Linux 上长期稳定运行（systemd）

---

## 2. 服务提供的接口

- 主入口：`/stream?url=...`
- 兼容旧入口：`/api/proxy/stream?url=...`
- 健康检查：`/health`

---

## 3. HLS 为什么必须重写 m3u8

m3u8 只是入口：

- 清单里包含大量分片 URL（或子清单 URL）
- 如果启用加密，会通过 `#EXT-X-KEY` 的 `URI=` 再请求 key

因此代理需要在识别到 m3u8 时改写清单中的 URL，使后续分片与 key 请求仍回到代理入口。

---

## 4. Range 透传：让拖拽/断点续传可用

播放器常会发：`Range: bytes=...`。代理需要透传 `Range` 并正确回传 `206 Partial Content`。

---

## 5. 本地运行与快速验证

```bash
export LISTEN_ADDR=0.0.0.0:8080
cargo run
curl -i http://127.0.0.1:8080/health
```

---

## 6. Linux 部署：systemd 常驻

仓库自带 unit：`deploy/rust-proxy.service`。

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rust-proxy
sudo journalctl -u rust-proxy -f
```

---

## 7. Nginx 反代：443 对外，8080 对内

关键配置：`X-Forwarded-Proto` / `X-Forwarded-Host`，确保 m3u8 重写生成正确的 https URL。

---

## 8. CI/CD：让开源项目可交付

- CI：push/PR 自动跑 fmt/clippy/test/build
- Release：合并后自动创建 GitHub Release，并提供 Linux 可执行文件（musl 静态）

详细说明见：`docs/cicd.md`

---

## 图片占位

- 架构图：`docs/press/images/architecture.png`
- 运行截图：`docs/press/images/demo.png`

![运行截图占位](images/demo.png)
