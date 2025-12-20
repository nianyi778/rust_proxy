# rust_proxy

一个 Rust 视频流代理服务（适合 HLS/MP4 Range 场景），用于把播放器请求转发到“动态上游源站”（域名/端口每次请求都可能不同）。

## 功能

- 视频流代理（动态上游）：`/stream?url=...`
- 兼容旧路径：`/api/proxy/stream?url=...`（对齐你之前的 Node/CF 版本）
- 流式透传：不落盘、不缓冲完整响应
- Range 支持：透传 `Range`（源站需支持 Range 才能拖拽/断点续传）
- 内置 CORS：响应会带 `Access-Control-Allow-Origin: *`
- HLS 支持：当上游返回 m3u8（或 URL 包含 `.m3u8`），会重写清单内容，确保分片/KEY 也继续走代理

## 架构设计（Big Picture）

- 入口：客户端访问本服务的 `/stream` 或 `/api/proxy/stream`，并通过 `?url=` 指定上游资源。
- 数据流：客户端 → 本服务 → 上游（CDN/源站）→ 本服务 → 客户端。
- m3u8 重写：
	- 非注释行（分片/子清单 URL）会被改写为 `https://你的域名/api/proxy/stream?url=...`
	- `#EXT-X-KEY:...URI=...` 中的 key URI 也会被改写
- 上游请求头策略（对齐旧版 proxy.ts）：
	- 不转发浏览器的全部请求头
	- 会设置通用 UA，并尝试伪造 Referer/Origin 为目标站点 origin
	- 若上游返回 403，会去掉 Referer/Origin 重试一次

## 使用说明（本地运行）

开发模式：

```bash
export LISTEN_ADDR=0.0.0.0:8080
cargo run
```

Release 运行（推荐）：

```bash
cargo build --release
export LISTEN_ADDR=0.0.0.0:8080
./target/release/rust_proxy
```

提示：直接输入 `rust_proxy` 会出现 `command not found`，因为它不在 PATH；请用 `./target/release/rust_proxy`。

## 运行手册（测试/排障）

1. 健康检查：

```bash
curl -i http://127.0.0.1:8080/health
```

2. 用 m3u8 测试（示例）：

```bash
curl -i 'http://127.0.0.1:8080/api/proxy/stream?url=https%3A%2F%2Fexample.com%2Flive%2Findex.m3u8'
```

检查返回内容里是否出现大量 `/api/proxy/stream?url=...`（说明 m3u8 重写生效，分片会继续走代理）。

3. Range 测试（mp4 常用）：

```bash
curl -i -H 'Range: bytes=0-1023' 'http://127.0.0.1:8080/stream?url=https%3A%2F%2Fexample.com%2Fvideo.mp4'
```

4. 常见问题：

- m3u8 能访问但播放失败：通常是分片 URL 没有被正确重写（或重写出来的域名/协议不对）。
- HTTPS 站点反代后重写出来变成 http：反代需要设置 `X-Forwarded-Proto`/`X-Forwarded-Host`。
- 上游偶发 403：服务会自动“去掉 Referer/Origin 重试一次”，但有些站点还会校验 Token/IP。

## Linux 部署（systemd）

1. 构建二进制：

```bash
cargo build --release
```

2. 复制到服务器（示例路径）：

- `/opt/rust_proxy/rust_proxy`（二进制）
- `/opt/rust_proxy/`（目录）

3. 安装 systemd unit：

- 将 [deploy/rust-proxy.service](deploy/rust-proxy.service) 复制到 `/etc/systemd/system/rust-proxy.service`
- 按需修改 `LISTEN_ADDR`（默认 8080）

4. 启动：

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now rust-proxy
sudo systemctl status rust-proxy
```

查看日志：

```bash
sudo journalctl -u rust-proxy -f
```

## 反向代理（Nginx 示例）

如果你用域名（HTTPS）对外提供服务，建议用 Nginx/Caddy 反代到本服务的 `127.0.0.1:8080`。

关键是要带上 `X-Forwarded-Proto/Host`，这样 m3u8 重写出来的 URL 才会是正确的 `https://你的域名/...`：

```nginx
location /api/proxy/stream {
	proxy_pass http://127.0.0.1:8080;
	proxy_http_version 1.1;
	proxy_set_header Host $host;
	proxy_set_header X-Forwarded-Host $host;
	proxy_set_header X-Forwarded-Proto $scheme;
}
```

## 环境变量

- `LISTEN_ADDR`：监听地址，默认 `0.0.0.0:8080`

说明：这是动态上游代理，上游源站的域名/端口由每次请求的 `?url=...` 决定。

## 安全提示

这是“动态上游代理”，天然具备开放代理（Open Proxy）风险：任何人只要能访问你的接口，就可能拿它去请求任意上游 URL。生产环境务必用网关/鉴权/白名单等做限制（本项目当前按你的要求：不做白名单）。
