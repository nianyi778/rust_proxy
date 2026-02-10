#!/bin/bash
# Rust Proxy 自动部署脚本
# 用法: ./deploy.sh

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# 配置
APP_NAME="rust_proxy"
SERVICE_NAME="rust-proxy"
INSTALL_DIR="/opt/$APP_NAME"
BINARY_NAME="rust_proxy"

echo -e "${GREEN}🚀 Rust Proxy 自动部署脚本${NC}"
echo "========================================"

# 检查是否为 root
check_root() {
    if [ "$EUID" -ne 0 ]; then 
        echo -e "${YELLOW}⚠️  建议使用 sudo 运行此脚本${NC}"
        echo "例如: sudo ./deploy.sh"
        sleep 2
    fi
}

# 检查依赖
check_dependencies() {
    echo -e "${YELLOW}📦 检查依赖...${NC}"
    
    # 检查 Rust
    if ! command -v rustc &> /dev/null; then
        echo -e "${YELLOW}Rust 未安装，正在安装...${NC}"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source "$HOME/.cargo/env"
    fi
    
    # 检查 Git
    if ! command -v git &> /dev/null; then
        echo -e "${RED}❌ Git 未安装，请先安装 Git${NC}"
        exit 1
    fi
    
    echo -e "${GREEN}✅ 依赖检查完成${NC}"
}

# 编译项目
build_project() {
    echo -e "${YELLOW}🔨 编译 Rust 项目...${NC}"
    
    # 获取当前目录
    PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    cd "$PROJECT_DIR"
    
    # 更新代码
    if [ -d ".git" ]; then
        echo "更新代码..."
        git pull origin master
    fi
    
    # 编译发布版本
    echo "开始编译 (这可能需要几分钟)..."
    cargo build --release
    
    # 检查编译结果
    if [ ! -f "target/release/$BINARY_NAME" ]; then
        echo -e "${RED}❌ 编译失败！${NC}"
        exit 1
    fi
    
    echo -e "${GREEN}✅ 编译成功${NC}"
}

# 安装二进制文件
install_binary() {
    echo -e "${YELLOW}📥 安装二进制文件...${NC}"
    
    # 创建安装目录
    sudo mkdir -p "$INSTALL_DIR"
    
    # 复制二进制文件
    sudo cp "target/release/$BINARY_NAME" "$INSTALL_DIR/"
    sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"
    
    # 创建日志目录
    sudo mkdir -p "/var/log/$APP_NAME"
    sudo chmod 755 "/var/log/$APP_NAME"
    
    echo -e "${GREEN}✅ 安装完成: $INSTALL_DIR/$BINARY_NAME${NC}"
}

# 创建 systemd 服务
create_systemd_service() {
    echo -e "${YELLOW}⚙️  创建 systemd 服务...${NC}"
    
    # 检测服务器 IP
    SERVER_IP=$(curl -s https://api.ipify.org || echo "0.0.0.0")
    
    sudo tee "/etc/systemd/system/$SERVICE_NAME.service" > /dev/null <<EOF
[Unit]
Description=StarFlix Rust Proxy Service
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=$INSTALL_DIR
Environment="LISTEN_ADDR=0.0.0.0:8080"
Environment="RUST_LOG=info"
ExecStart=$INSTALL_DIR/$BINARY_NAME
Restart=always
RestartSec=5
StandardOutput=append:/var/log/$APP_NAME/app.log
StandardError=append:/var/log/$APP_NAME/error.log

[Install]
WantedBy=multi-user.target
EOF

    sudo systemctl daemon-reload
    sudo systemctl enable "$SERVICE_NAME"
    
    echo -e "${GREEN}✅ systemd 服务已创建${NC}"
}

# 启动服务
start_service() {
    echo -e "${YELLOW}▶️  启动服务...${NC}"
    
    sudo systemctl restart "$SERVICE_NAME"
    sleep 2
    
    # 检查服务状态
    if sudo systemctl is-active --quiet "$SERVICE_NAME"; then
        echo -e "${GREEN}✅ 服务启动成功！${NC}"
        
        # 获取服务器 IP
        SERVER_IP=$(curl -s https://api.ipify.org || hostname -I | awk '{print $1}')
        echo ""
        echo -e "${GREEN}🎉 部署完成！${NC}"
        echo "========================================"
        echo "服务地址: http://$SERVER_IP:8080"
        echo "健康检查: http://$SERVER_IP:8080/health"
        echo "日志查看: sudo journalctl -u $SERVICE_NAME -f"
        echo "========================================"
    else
        echo -e "${RED}❌ 服务启动失败！${NC}"
        echo "查看日志: sudo journalctl -u $SERVICE_NAME -n 50"
        exit 1
    fi
}

# 创建打包下载功能
create_package() {
    echo -e "${YELLOW}📦 创建可下载包...${NC}"
    
    PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    cd "$PROJECT_DIR"
    
    # 获取版本和架构信息
    VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
    ARCH=$(uname -m)
    PLATFORM=$(uname -s | tr '[:upper:]' '[:lower:]')
    
    PACKAGE_NAME="${APP_NAME}-${VERSION}-${PLATFORM}-${ARCH}"
    PACKAGE_DIR="target/package/$PACKAGE_NAME"
    
    # 创建包目录
    mkdir -p "$PACKAGE_DIR"
    
    # 复制文件
    cp "target/release/$BINARY_NAME" "$PACKAGE_DIR/"
    cp "deploy.sh" "$PACKAGE_DIR/"
    cp "README.md" "$PACKAGE_DIR/" 2>/dev/null || echo "# Rust Proxy" > "$PACKAGE_DIR/README.md"
    
    # 创建启动脚本
    cat > "$PACKAGE_DIR/start.sh" <<'EOF'
#!/bin/bash
# 简单启动脚本

export LISTEN_ADDR="${LISTEN_ADDR:-0.0.0.0:8080}"
export RUST_LOG="${RUST_LOG:-info}"

./rust_proxy
EOF
    chmod +x "$PACKAGE_DIR/start.sh"
    
    # 创建 systemd 服务文件
    cat > "$PACKAGE_DIR/$SERVICE_NAME.service" <<EOF
[Unit]
Description=StarFlix Rust Proxy Service
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/opt/$APP_NAME
Environment="LISTEN_ADDR=0.0.0.0:8080"
Environment="RUST_LOG=info"
ExecStart=/opt/$APP_NAME/$BINARY_NAME
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    
    # 打包
    cd "target/package"
    tar -czf "${PACKAGE_NAME}.tar.gz" "$PACKAGE_NAME"
    
    PACKAGE_PATH="$(pwd)/${PACKAGE_NAME}.tar.gz"
    PACKAGE_SIZE=$(du -h "$PACKAGE_PATH" | cut -f1)
    
    echo ""
    echo -e "${GREEN}📦 打包完成！${NC}"
    echo "========================================"
    echo "包名: ${PACKAGE_NAME}.tar.gz"
    echo "大小: $PACKAGE_SIZE"
    echo "路径: $PACKAGE_PATH"
    echo ""
    echo "使用方法:"
    echo "1. 下载包到服务器"
    echo "2. 解压: tar -xzf ${PACKAGE_NAME}.tar.gz"
    echo "3. 运行: cd $PACKAGE_NAME && ./start.sh"
    echo "========================================"
    
    # 尝试创建下载链接（如果有 web 服务器）
    if [ -d "/var/www/html" ]; then
        sudo cp "$PACKAGE_PATH" "/var/www/html/"
        echo -e "${GREEN}✅ 已复制到 web 目录: http://$SERVER_IP/${PACKAGE_NAME}.tar.gz${NC}"
    fi
}

# 主函数
main() {
    echo "========================================"
    echo "Rust Proxy 自动部署脚本"
    echo "========================================"
    echo ""
    
    check_root
    check_dependencies
    build_project
    install_binary
    create_systemd_service
    start_service
    create_package
    
    echo ""
    echo -e "${GREEN}🎉 所有步骤完成！${NC}"
}

# 如果直接运行此脚本
if [ "${BASH_SOURCE[0]}" == "${0}" ]; then
    main "$@"
fi
