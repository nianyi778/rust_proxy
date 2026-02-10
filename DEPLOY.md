# Rust Proxy 部署指南

## 快速安装（推荐）

上传到服务器后，直接运行：

```bash
cd rust_proxy
chmod +x install.sh
./install.sh
```

这个脚本会自动：

1. 安装 Rust（如果没有）
2. 编译项目
3. 安装到 `/opt/rust_proxy`
4. 创建 systemd 服务
5. 启动服务

## 完整部署（带打包功能）

```bash
chmod +x deploy.sh
./deploy.sh
```

## 手动安装

```bash
# 1. 编译
cargo build --release

# 2. 复制二进制文件
sudo mkdir -p /opt/rust_proxy
sudo cp target/release/rust_proxy /opt/rust_proxy/

# 3. 运行
LISTEN_ADDR=0.0.0.0:8080 ./rust_proxy
```

## 服务管理

```bash
# 查看状态
sudo systemctl status rust-proxy

# 重启服务
sudo systemctl restart rust-proxy

# 查看日志
sudo journalctl -u rust-proxy -f

# 停止服务
sudo systemctl stop rust-proxy
```

## 环境变量

- `LISTEN_ADDR`: 监听地址，默认 `0.0.0.0:8080`
- `RUST_LOG`: 日志级别，默认 `info`

## 下载编译好的版本

运行 `./deploy.sh` 后，会在 `target/package/` 目录生成：

- `rust_proxy-{version}-{platform}-{arch}.tar.gz`

如果服务器有 web 服务器，会自动复制到 `/var/www/html/`，可以通过浏览器下载。

## 测试

```bash
curl http://localhost:8080/health
```
