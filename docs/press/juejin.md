# 用 Rust 自建一个“动态上游”的视频流代理：HLS m3u8 重写 + Range 透传 + Linux 一键部署

> 适用场景：你有一个播放器（Web / App / IPTV / 自己的前端），但上游视频源（CDN/源站）地址经常变化，或者你需要把播放请求统一收口到自己的域名下。

- 项目：`rust_proxy`
- 当前版本：以仓库 README 为准
- 更新时间：2025-12-21

---

## 背景：为什么要做“动态上游代理”

很多时候我们拿到的不是“固定源站”，而是一个随时变化的 URL（域名、端口、路径、token 都可能变）。如果把播放器直接指向上游：

- 前端跨域 / CORS 很麻烦
- HLS 播放链路里，m3u8 只是入口，后续分片与 KEY 会继续请求别的 URL
- MP4/TS 想要拖拽、断点续传，必须正确支持 `Range`
- 有的源站还会对 `Referer/Origin/UA` 做策略校验

所以我做了一个很“窄”的服务：只做一件事——把你的播放请求转发到你指定的上游 URL，并且在 HLS 场景把清单里的 URL 重写回代理入口。

---

## 这个项目做了什么

`rust_proxy` 是一个 Rust 视频流代理服务（适合 HLS/MP4 Range 场景），核心能力：

- 动态上游代理：`/stream?url=...`
- 兼容旧路径：`/api/proxy/stream?url=...`
- 流式透传：不落盘、不缓冲完整响应
- `Range` 透传：支持 `206 Partial Content`（前提是源站支持）
- HLS 支持：识别 m3u8 并重写清单内容
  - 重写分片 / 子清单 URL
  - 重写 `#EXT-X-KEY` 里的 `URI=`
- CORS：默认允许 `*`
- 403 重试：若上游返回 403，会去掉 `Referer/Origin` 重试一次（对齐旧版本行为）

---

## 架构图（占位）

你可以后续把图放到 `docs/press/images/architecture.png`。

![架构图占位](images/architecture.png)

数据流：客户端 → `rust_proxy` → 上游（CDN/源站）→ `rust_proxy` → 客户端。

---

## 快速开始（本地）

```bash
export LISTEN_ADDR=0.0.0.0:8080
cargo run
```

健康检查：

```bash
curl -i http://127.0.0.1:8080/health
```

HLS 测试（示例）：

```bash
curl -i 'http://127.0.0.1:8080/api/proxy/stream?url=https%3A%2F%2Fexample.com%2Flive%2Findex.m3u8'
```

MP4 Range 测试（示例）：

```bash
curl -i -H 'Range: bytes=0-1023' 'http://127.0.0.1:8080/stream?url=https%3A%2F%2Fexample.com%2Fvideo.mp4'
```

---

## HLS 的关键点：m3u8 必须重写

HLS 播放通常是：

1. 播放器请求 `index.m3u8`
2. m3u8 返回一堆分片 URL（`.ts` / `.m4s`）或子清单 URL
3. 如果有加密，还会在 `#EXT-X-KEY` 里提供一个 `URI=` 让播放器去拿 key

如果你只代理第 1 步，后续第 2/3 步仍会直连上游（跨域、鉴权、域名不一致都可能炸）。

这个项目在检测到 m3u8 时，会把清单里的 URL 改写成：

- `https://你的域名/api/proxy/stream?url=...`

从而保证后续请求仍回到代理。

---

## 线上部署（Linux + systemd）

仓库里提供了 systemd unit：

- `deploy/rust-proxy.service`

推荐路径（示例）：

- `/opt/rust_proxy/rust_proxy`（二进制）
- `/opt/rust_proxy/`（工作目录）

启动：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rust-proxy
sudo systemctl status rust-proxy
sudo journalctl -u rust-proxy -f
```

---

## 反向代理（Nginx：HTTPS 443 → 8080）

如果你希望外部统一走 `https://your-domain/...`，用 Nginx 反代到本服务的 8080。

关键：必须把 `X-Forwarded-Proto/Host` 传进去，否则 m3u8 重写出来的 URL 可能变成 `http://...`。

```nginx
location /api/proxy/stream {
  proxy_pass http://127.0.0.1:8080;
  proxy_http_version 1.1;
  proxy_set_header Host $host;
  proxy_set_header X-Forwarded-Host $host;
  proxy_set_header X-Forwarded-Proto $scheme;
}

location /stream {
  proxy_pass http://127.0.0.1:8080;
  proxy_http_version 1.1;
  proxy_set_header Host $host;
  proxy_set_header X-Forwarded-Host $host;
  proxy_set_header X-Forwarded-Proto $scheme;
}
```

---

## CI/CD 与 Release（开源交付）

- CI：push/PR 自动跑 `fmt/clippy/test/build`
- CD：自动版本 PR、合并后自动创建 Release
- Release 附件：提供 Linux 可执行文件（musl 静态，避免 glibc 版本不匹配）

详细见：`docs/cicd.md`

---

## 安全提示（很重要）

这是“动态上游代理”，天然存在 Open Proxy 风险：任何能访问你接口的人，可能用它去请求任意 URL。

生产环境建议至少做一层限制：

- Nginx / 网关鉴权（token、basic auth）
- 只允许内网访问
- 白名单（域名/前缀）
- 限流

（本项目当前保持“按需最小实现”，默认不内置白名单。）

---

## 最后

如果你也在做播放器收口、HLS/Range、或者上游地址不稳定的播放链路，这类“动态上游代理”会非常省心。
