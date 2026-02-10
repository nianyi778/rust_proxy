#!/bin/bash
# Rust Proxy 快速安装脚本
# 一键安装并启动服务

set -e

APP_NAME="rust_proxy"
SERVICE_NAME="rust-proxy"
INSTALL_DIR="/opt/$APP_NAME"

echo "🚀 Rust Proxy 快速安装"
echo "======================"

# 1. 安装 Rust（如果没有）
if ! command -v cargo &> /dev/null; then
    echo "📦 安装 Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

# 2. 编译
echo "🔨 编译项目..."
cargo build --release

# 3. 安装
sudo mkdir -p "$INSTALL_DIR"
sudo cp "target/release/rust_proxy" "$INSTALL_DIR/"
sudo chmod +x "$INSTALL_DIR/rust_proxy"

# 4. 创建服务
sudo tee "/etc/systemd/system/$SERVICE_NAME.service" > /dev/null <<EOF
[Unit]
Description=Rust Proxy
After=network.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/rust_proxy
Restart=always
Environment="LISTEN_ADDR=0.0.0.0:8080"
Environment="RUST_LOG=info"

[Install]
WantedBy=multi-user.target
EOF

# 5. 启动
sudo systemctl daemon-reload
sudo systemctl enable "$SERVICE_NAME"
sudo systemctl start "$SERVICE_NAME"

# 6. 检查状态
sleep 2
if sudo systemctl is-active --quiet "$SERVICE_NAME"; then
    IP=$(curl -s https://api.ipify.org || echo "localhost")
    echo ""
    echo "✅ 安装成功！"
    echo "======================"
    echo "服务地址: http://$IP:8080"
    echo "健康检查: curl http://$IP:8080/health"
    echo "查看日志: sudo journalctl -u $SERVICE_NAME -f"
    echo "======================"
else
    echo "❌ 启动失败"
    sudo journalctl -u "$SERVICE_NAME" -n 20
    exit 1
fi
