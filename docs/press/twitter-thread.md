# 推特/微博线程（中文）：用 Rust 自建视频流代理（HLS m3u8 重写 + Range）

> 复制下面每条作为一条推文/微博即可。需要配图的位置我留了占位。

1/ 我做了一个很“窄”的服务：Rust 视频流代理。只干一件事：把播放器请求转发到你指定的上游 URL，并把 HLS m3u8 里的分片/KEY URL 重写回代理入口。

2/ 为什么要做？因为上游地址经常变（域名/端口/token 都可能变），而播放器直连上游会遇到：跨域、HLS 后续分片请求不走代理、MP4 拖拽需要 Range、源站校验 Referer/Origin/UA 等。

3/ 这个项目支持：
- `/stream?url=...`
- 兼容 `/api/proxy/stream?url=...`
- 流式透传不落盘
- `Range` 透传（206）
- m3u8 重写（含 `#EXT-X-KEY URI=`）
- CORS `*`
- 上游 403 去掉 Referer/Origin 重试一次

4/ HLS 的坑：你只代理 `index.m3u8` 没用。播放器拿到清单后会继续请求分片和 key。如果不重写清单里的 URL，后续就会直连上游（跨域/鉴权/域名不一致都可能炸）。

5/ （配图占位）架构图：客户端 → 代理 → 上游 → 代理 → 客户端
把图放到仓库：`docs/press/images/architecture.png`

6/ 本地跑起来：
```bash
export LISTEN_ADDR=0.0.0.0:8080
cargo run
```
健康检查：
```bash
curl -i http://127.0.0.1:8080/health
```

7/ HLS 示例：
```bash
curl -i 'http://127.0.0.1:8080/api/proxy/stream?url=https%3A%2F%2Fexample.com%2Flive%2Findex.m3u8'
```

8/ MP4 Range 示例：
```bash
curl -i -H 'Range: bytes=0-1023' 'http://127.0.0.1:8080/stream?url=https%3A%2F%2Fexample.com%2Fvideo.mp4'
```

9/ 线上部署：systemd 常驻 + Nginx 443 反代到 8080。
重点：Nginx 必须传 `X-Forwarded-Proto/Host`，否则 m3u8 重写出来的 URL 可能变成 http。

10/ CI/CD 也打通了：push/PR 自动跑 fmt/clippy/test/build；合并后自动打版本 + Release；Release 附件提供 Linux 可执行文件（musl 静态，避免 glibc 版本不匹配）。

11/ 安全提醒：动态上游代理 = 可能变成 Open Proxy。生产环境务必加一层鉴权/白名单/限流，或者只对内网开放。
