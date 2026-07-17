#!/bin/bash
set -euo pipefail

# CradleRing Installer for macOS and Linux
# Usage: curl -fsSL https://cradle-ring.dev/install.sh | bash
#   or: curl -fsSL https://cradle-ring.dev/install.sh | bash -s -- --cargo
#   or: curl -fsSL https://cradle-ring.dev/install.sh | bash -s -- --git

BOLD='\033[1m'
ACCENT='\033[38;2;79;158;255m'
INFO='\033[38;2;136;146;176m'
SUCCESS='\033[38;2;0;229;204m'
WARN='\033[38;2;255;176;32m'
ERROR='\033[38;2;230;57;70m'
MUTED='\033[38;2;90;100;128m'
NC='\033[0m'

DEFAULT_TAGLINE="All your chats, one CradleRing."
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

ui_info()    { echo -e "${INFO}$1${NC}"; }
ui_warn()    { echo -e "${WARN}⚠ $1${NC}" >&2; }
ui_error()   { echo -e "${ERROR}✗ $1${NC}" >&2; }
ui_success() { echo -e "${SUCCESS}✓ $1${NC}"; }
ui_step()    { echo -e "${ACCENT}${BOLD}▸ $1${NC}"; }
ui_kv()      { printf "  ${MUTED}%-16s${NC} %s\n" "$1:" "$2"; }

INSTALL_METHOD="${CRADLE_RING_INSTALL_METHOD:-}"
VERSION="${CRADLE_RING_VERSION:-latest}"
HOME_DIR="${CRADLE_RING_HOME:-$HOME/.cradle-ring}"
BIN_DIR="${CRADLE_RING_BIN_DIR:-$HOME/.local/bin}"
NO_ONBOARD="${CRADLE_RING_NO_ONBOARD:-0}"
NO_PROMPT="${CRADLE_RING_NO_PROMPT:-0}"
NO_DAEMON="${CRADLE_RING_NO_DAEMON:-0}"
DRY_RUN=0
VERBOSE=0
VERIFY_INSTALL=0
ACTION="install"
RUST_MIN_MAJOR=1
RUST_MIN_MINOR=75
SYSTEMD_SERVICE="cradle-ring-gateway"
LAUNCHD_LABEL="dev.cradle-ring.gateway"

TMPFILES=()
SRC_DIR=""  # 全局变量：记录源码目录（curl|bash 模式下为克隆的临时目录）
cleanup_tmpfiles() { for f in "${TMPFILES[@]:-}"; do rm -rf "$f" 2>/dev/null || true; done; }
trap cleanup_tmpfiles EXIT
trap 'cleanup_tmpfiles; ui_warn "安装中断"; exit 130' INT
trap 'cleanup_tmpfiles; ui_warn "安装终止"; exit 143' TERM

show_help() {
    cat <<EOF
CradleRing Installer

用法: install.sh [选项]

安装方式:
  --cargo              使用 cargo 编译安装（默认）
  --git, --github      从 git 克隆编译安装
  --source             在当前目录编译安装

选项:
  --version <ver>      指定版本（默认: latest）
  --prefix <path>      二进制安装目录（默认: ~/.local/bin）
  --home <path>        数据目录（默认: ~/.cradle-ring）
  --no-onboard         跳过 onboarding
  --no-prompt          非交互模式
  --no-daemon          不安装系统服务
  --verify             安装后验证
  --dry-run            只显示步骤不执行
  --verbose            详细输出
  --uninstall          卸载
  --help, -h           帮助

一行安装:
  curl -fsSL https://cradle-ring.dev/install.sh | bash
EOF
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --cargo)         INSTALL_METHOD="cargo"; shift ;;
            --git|--github)  INSTALL_METHOD="git"; shift ;;
            --source)        INSTALL_METHOD="source"; shift ;;
            --version)       [[ $# -lt 2 ]] && { ui_error "缺少参数"; exit 2; }; VERSION="$2"; shift 2 ;;
            --prefix)        [[ $# -lt 2 ]] && { ui_error "缺少参数"; exit 2; }; BIN_DIR="$2"; shift 2 ;;
            --home)          [[ $# -lt 2 ]] && { ui_error "缺少参数"; exit 2; }; HOME_DIR="$2"; shift 2 ;;
            --no-onboard)    NO_ONBOARD=1; shift ;;
            --onboard)       NO_ONBOARD=0; shift ;;
            --no-prompt)     NO_PROMPT=1; shift ;;
            --no-daemon)     NO_DAEMON=1; shift ;;
            --verify)        VERIFY_INSTALL=1; shift ;;
            --dry-run)       DRY_RUN=1; shift ;;
            --verbose)       VERBOSE=1; shift ;;
            --uninstall)     ACTION="uninstall"; shift ;;
            --help|-h)       show_help; exit 0 ;;
            *)               ui_error "未知参数: $1"; exit 1 ;;
        esac
    done
    [[ -z "$INSTALL_METHOD" ]] && INSTALL_METHOD="cargo"
}

detect_os_or_die() {
    case "$(uname -s)" in
        Linux)  PLATFORM="linux" ;;
        Darwin) PLATFORM="macos" ;;
        *)      ui_error "不支持的操作系统: $(uname -s)"; exit 1 ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64)  ARCH="x86_64" ;;
        aarch64|arm64) ARCH="aarch64" ;;
        *)             ui_error "不支持的架构: $(uname -m)"; exit 1 ;;
    esac
    ui_info "操作系统: $PLATFORM $ARCH"
}

check_rustc() {
    command -v rustc &>/dev/null || return 1
    local v major minor
    v="$(rustc --version 2>/dev/null | awk '{print $2}')"
    major="${v%%.*}"; minor="${v#*.}"; minor="${minor%%.*}"
    [[ "$major" -lt "$RUST_MIN_MAJOR" ]] && return 1
    [[ "$major" -eq "$RUST_MIN_MAJOR" && "$minor" -lt "$RUST_MIN_MINOR" ]] && return 1
    RUSTC_VERSION="$v"
    return 0
}

ensure_rust_toolchain() {
    if check_rustc; then
        ui_info "Rust 版本: $RUSTC_VERSION"
        return 0
    fi
    ui_warn "未检测到 Rust 工具链"
    ui_step "安装 Rust（rustup）..."
    if [[ "$NO_PROMPT" == "1" ]] || [[ ! -t 0 ]]; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y || { ui_error "Rust 安装失败"; exit 1; }
    else
        read -p "安装 Rust 工具链？(Y/n) " -n1 -r; echo
        [[ $REPLY =~ ^[Nn]$ ]] && { ui_error "Rust 是必需的"; exit 1; }
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y || { ui_error "Rust 安装失败"; exit 1; }
    fi
    source "$HOME/.cargo/env" 2>/dev/null || true
    export PATH="$HOME/.cargo/bin:$PATH"
    check_rustc || { ui_error "Rust 安装后仍无法检测"; exit 1; }
    ui_success "Rust: $RUSTC_VERSION"
}

# 检测是否在 curl|bash 模式下运行（无本地源码）
is_curl_bash_mode() {
    # 如果 SCRIPT_DIR 下没有 Cargo.toml，说明是通过 curl 下载的脚本
    [[ ! -f "$SCRIPT_DIR/Cargo.toml" ]]
}

# 从 GitHub 下载源码到临时目录（优先 git，fallback 到 curl tarball）
clone_source() {
    local tmp_dir
    tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'cradlering')"
    # 日志输出到 stderr，不干扰返回值
    echo -e "${ACCENT}${BOLD}▸ 从 GitHub 下载源码...${NC}" >&2
    # 优先用 git（如果可用）
    if command -v git >/dev/null 2>&1; then
        if [[ -d "$tmp_dir/.git" ]]; then
            cd "$tmp_dir" && git fetch --all 2>/dev/null && git checkout main 2>/dev/null || true
        else
            git clone --depth 1 https://github.com/UA-Jin/CradleRing.git "$tmp_dir" >&2 || { ui_error "克隆失败" >&2; exit 1; }
        fi
    else
        # fallback：用 curl 下载 tarball（无需 git）
        echo -e "${INFO}git 不可用，改用 curl 下载源码包...${NC}" >&2
        local tarball="$tmp_dir/cradlering.tar.gz"
        curl -fsSL -o "$tarball" https://github.com/UA-Jin/CradleRing/archive/refs/heads/main.tar.gz >&2 || { ui_error "下载失败" >&2; exit 1; }
        tar -xzf "$tarball" -C "$tmp_dir" >&2 || { ui_error "解压失败" >&2; exit 1; }
        # 把解压后的子目录内容移到顶层
        mv "$tmp_dir"/CradleRing-main/* "$tmp_dir/" 2>/dev/null || true
        rm -rf "$tmp_dir"/CradleRing-main "$tarball" 2>/dev/null || true
    fi
    # 只输出路径到 stdout（供 $( ) 捕获）
    echo "$tmp_dir"
}

install_cradle_ring() {
    ui_step "编译 CradleRing（cargo build --release）..."
    local src_dir="$SCRIPT_DIR"
    # curl|bash 模式：自动从 GitHub 克隆源码
    if is_curl_bash_mode; then
        echo -e "${INFO}检测到 curl|bash 模式，自动下载源码...${NC}" >&2
        src_dir="$(clone_source)"; echo "DEBUG: src_dir=[$src_dir]" >&2
        TMPFILES+=("$src_dir")
        SRC_DIR="$src_dir"  # 保存到全局变量，供后续步骤使用
    fi
    case "$INSTALL_METHOD" in
        cargo|source|"")
            if [[ -f "$src_dir/Cargo.toml" ]]; then
                ui_info "源码目录: $src_dir"
                [[ "$DRY_RUN" == "1" ]] && return 0
                cd "$src_dir"
                cargo build --release --bin cradle-ring || { ui_error "编译失败"; exit 1; }
                BINARY="$src_dir/target/release/cradle-ring"
            else
                [[ "$DRY_RUN" == "1" ]] && return 0
                cargo install cradle-ring --locked --force 2>/dev/null || {
                    ui_warn "crates.io 未发布，从源码编译"
                    [[ -d "$src_dir/crates" ]] && cd "$src_dir" && cargo build --release --bin cradle-ring || { ui_error "编译失败"; exit 1; }
                }
                BINARY="$src_dir/target/release/cradle-ring"
            fi ;;
        git)
            local repo_dir="$HOME_DIR/repo"
            ui_info "从 git 克隆..."
            [[ "$DRY_RUN" == "1" ]] && return 0
            if [[ -d "$repo_dir/.git" ]]; then
                cd "$repo_dir"; git fetch --all 2>/dev/null; git checkout "$VERSION" 2>/dev/null || true
            else
                git clone "https://github.com/UA-Jin/CradleRing.git" "$repo_dir" || { ui_error "克隆失败"; exit 1; }
                cd "$repo_dir"; git checkout "$VERSION" 2>/dev/null || true
            fi
            cargo build --release --bin cradle-ring || { ui_error "编译失败"; exit 1; }
            BINARY="$repo_dir/target/release/cradle-ring" ;;
    esac
    [[ -f "$BINARY" ]] || { ui_error "二进制未找到: $BINARY"; exit 1; }
    ui_success "编译完成"
}

# 构建前端（Vue3 + Arco Design Pro）
build_webui() {
    local src_dir="$SRC_DIR"
    if [[ -z "$src_dir" ]]; then
        src_dir="$SCRIPT_DIR"
    fi
    local webui_dir="$src_dir/webui"
    if [[ ! -d "$webui_dir" ]] || [[ ! -f "$webui_dir/package.json" ]]; then
        ui_warn "未找到 webui/ 目录，跳过前端构建（将使用已有 ui-dist）"
        return 0
    fi
    ui_step "构建前端（Vue3 + Arco Design Pro）..."
    [[ "$DRY_RUN" == "1" ]] && return 0
    # 检测 pnpm/npm
    local pkg_mgr=""
    command -v pnpm >/dev/null 2>&1 && pkg_mgr="pnpm"
    [[ -z "$pkg_mgr" ]] && command -v npm >/dev/null 2>&1 && pkg_mgr="npm"
    if [[ -z "$pkg_mgr" ]]; then
        ui_warn "未找到 pnpm/npm，跳过前端构建。请手动运行：cd webui && pnpm install && pnpm build"
        return 0
    fi
    (
        cd "$webui_dir"
        ui_info "使用 $pkg_mgr 安装依赖..."
        # pnpm 11 需要 approve-builds 才能运行构建脚本（vue-demi 等）
        if [[ "$pkg_mgr" == "pnpm" ]]; then
            pnpm approve-builds vue-demi 2>&1 | tail -1 || true
        fi
        $pkg_mgr install --no-frozen-lockfile 2>&1 | tail -3
        ui_info "构建生产包..."
        if [[ "$pkg_mgr" == "pnpm" ]]; then
            # pnpm 11 严格依赖检查可能阻止 build，直接调 vite
            ./node_modules/.bin/vite build 2>&1 | tail -5 || $pkg_mgr build 2>&1 | tail -5
        else
            $pkg_mgr run build 2>&1 | tail -5
        fi
    )
    if [[ -d "$webui_dir/dist/assets" ]]; then
        ui_success "前端构建完成 → $webui_dir/dist"
        # 同步到 ui-dist
        rm -rf "$src_dir/crates/cradle-ring/ui-dist"
        cp -r "$webui_dir/dist" "$src_dir/crates/cradle-ring/ui-dist"
        ui_info "已同步到 crates/cradle-ring/ui-dist"
    else
        ui_error "前端构建失败"
        return 1
    fi
}

install_binary_and_ui() {
    mkdir -p "$BIN_DIR" "$BIN_DIR/ui-dist"
    [[ -f "$BINARY" ]] && {
        ui_step "安装二进制..."
        [[ "$DRY_RUN" != "1" ]] && { cp "$BINARY" "$BIN_DIR/cradle-ring"; chmod +x "$BIN_DIR/cradle-ring"; }
        ui_success "二进制: $BIN_DIR/cradle-ring"
    }
    local local_ui="$SCRIPT_DIR/crates/cradle-ring/ui-dist"
    local webui_dist="$SCRIPT_DIR/webui/dist"
    if [[ -d "$webui_dist/assets" ]]; then
        ui_step "安装 UI（Vue3 + Arco Design Pro）..."
        [[ "$DRY_RUN" != "1" ]] && { rm -rf "$BIN_DIR/ui-dist"; cp -r "$webui_dist" "$BIN_DIR/ui-dist"; }
        ui_success "UI 文件已安装（Arco Design Pro）"
    elif [[ -d "$local_ui/assets" ]]; then
        ui_step "安装 UI..."
        [[ "$DRY_RUN" != "1" ]] && cp -r "$local_ui"/* "$BIN_DIR/ui-dist/" 2>/dev/null || true
        ui_success "UI 文件已安装"
    fi
}

initialize_config() {
    mkdir -p "$HOME_DIR/data" "$HOME_DIR/workspace"
    local cfg="$HOME_DIR/cradle-ring.json"
    [[ -f "$cfg" ]] && { ui_info "配置已存在: $cfg"; return 0; }
    ui_step "初始化配置..."
    local token; token="$(head -c32 /dev/urandom | xxd -p -c32 2>/dev/null || echo "$(date +%s%N|sha256sum|cut -c1-32)")"
    [[ "$DRY_RUN" != "1" ]] && cat > "$cfg" <<EOF
{
  "gateway": { "port": 18800, "bind": "loopback", "auth": { "mode": "token", "token": "$token" } },
  "providers": {},
  "models": { "primary": "" },
  "memory": { "engine": "builtin" }
}
EOF
    ui_success "配置: $cfg"
    ui_info "Token: $token"
}

install_systemd_service() {
    [[ "$PLATFORM" != "linux" || "$NO_DAEMON" == "1" ]] && return 0
    local dir="$HOME/.config/systemd/user"; local unit="$dir/$SYSTEMD_SERVICE.service"
    mkdir -p "$dir"
    ui_step "创建 systemd 服务（开机自启）..."
    [[ "$DRY_RUN" != "1" ]] && cat > "$unit" <<EOF
[Unit]
Description=CradleRing Gateway
After=network-online.target
Wants=network-online.target
StartLimitBurst=5
StartLimitIntervalSec=60

[Service]
ExecStart=%h/.local/bin/cradle-ring gateway start
Environment=CRADLE_RING_UI_DIR=%h/.local/bin/ui-dist
Environment=CRADLE_RING_HOME=%h/.cradle-ring
Restart=always
RestartSec=5
TimeoutStopSec=30
TimeoutStartSec=60

[Install]
WantedBy=default.target
EOF
    [[ "$DRY_RUN" != "1" ]] && {
        loginctl enable-linger "$USER" 2>/dev/null || true
        systemctl --user daemon-reload
        systemctl --user enable "$SYSTEMD_SERVICE" 2>/dev/null || true
        systemctl --user stop "$SYSTEMD_SERVICE" 2>/dev/null || true
        systemctl --user start "$SYSTEMD_SERVICE" 2>/dev/null && ui_success "服务已启动（开机自启）" || ui_warn "服务启动失败（手动: cradle-ring gateway start）"
    } || ui_info "[dry-run] systemctl --user enable --now $SYSTEMD_SERVICE"
}

install_launchd_service() {
    [[ "$PLATFORM" != "macos" || "$NO_DAEMON" == "1" ]] && return 0
    local dir="$HOME/Library/LaunchAgents"; local plist="$dir/$LAUNCHD_LABEL.plist"
    mkdir -p "$dir"; mkdir -p "$HOME_DIR/logs"
    ui_step "创建 launchd 服务（开机自启）..."
    [[ "$DRY_RUN" != "1" ]] && cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$LAUNCHD_LABEL</string>
    <key>ProgramArguments</key><array>
        <string>$BIN_DIR/cradle-ring</string><string>gateway</string><string>start</string>
    </array>
    <key>EnvironmentVariables</key><dict>
        <key>CRADLE_RING_UI_DIR</key><string>$BIN_DIR/ui-dist</string>
        <key>CRADLE_RING_HOME</key><string>$HOME_DIR</string>
    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>$HOME_DIR/logs/gateway.log</string>
    <key>StandardErrorPath</key><string>$HOME_DIR/logs/gateway-error.log</string>
</dict>
</plist>
EOF
    [[ "$DRY_RUN" != "1" ]] && {
        launchctl unload "$plist" 2>/dev/null || true
        launchctl load "$plist" 2>/dev/null && ui_success "服务已启动（开机自启）" || ui_warn "服务启动失败"
    }
}

ensure_path() {
    [[ ":$PATH:" == *":$BIN_DIR:"* ]] && return 0
    ui_warn "$BIN_DIR 不在 PATH 中"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    if [[ "$NO_PROMPT" != "1" ]] && [[ -t 0 ]]; then
        read -p "自动添加到 PATH？(Y/n) " -n1 -r; echo
        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            local rc="$HOME/.zshrc"; [[ -n "${BASH_VERSION:-}" ]] && rc="$HOME/.bashrc"
            echo "export PATH=\"\$HOME/.local/bin:\$PATH\"" >> "$rc"
            ui_success "已添加到 $rc"
        fi
    fi
}

run_doctor() {
    local claw="$BIN_DIR/cradle-ring"; [[ ! -x "$claw" ]] && return 1
    ui_step "运行诊断..."
    [[ "$DRY_RUN" == "1" ]] && return 0
    "$claw" doctor < /dev/null 2>&1 || true
}

maybe_open_dashboard() {
    local url="http://127.0.0.1:18800"
    [[ "$NO_PROMPT" == "1" || ! -t 0 ]] && { ui_info "浏览器: $url"; return 0; }
    [[ "$DRY_RUN" == "1" ]] && return 0
    { [[ "$PLATFORM" == "macos" ]] && open "$url"; } 2>/dev/null || xdg-open "$url" 2>/dev/null || true
    ui_success "浏览器: $url"
}

verify_installation() {
    ui_step "验证..."
    [[ -x "$BIN_DIR/cradle-ring" ]] && ui_success "二进制 ✓" || { ui_error "二进制未找到"; return 1; }
    "$BIN_DIR/cradle-ring" --version 2>/dev/null | head -1 | xargs -I{} ui_success "版本: {}"
    [[ -f "$HOME_DIR/cradle-ring.json" ]] && ui_success "配置 ✓"
    if [[ "$PLATFORM" == "linux" ]]; then
        systemctl --user is-active "$SYSTEMD_SERVICE" &>/dev/null && ui_success "服务: 运行中 ✓" || ui_warn "服务未运行"
    fi
    ui_success "验证通过"
}

do_uninstall() {
    ui_step "卸载 CradleRing..."
    [[ "$PLATFORM" == "linux" ]] && {
        systemctl --user stop "$SYSTEMD_SERVICE" 2>/dev/null || true
        systemctl --user disable "$SYSTEMD_SERVICE" 2>/dev/null || true
        rm -f "$HOME/.config/systemd/user/$SYSTEMD_SERVICE.service"
        systemctl --user daemon-reload 2>/dev/null || true
    }
    [[ "$PLATFORM" == "macos" ]] && {
        launchctl unload "$HOME/Library/LaunchAgents/$LAUNCHD_LABEL.plist" 2>/dev/null || true
        rm -f "$HOME/Library/LaunchAgents/$LAUNCHD_LABEL.plist"
    }
    rm -f "$BIN_DIR/cradle-ring"; rm -rf "$BIN_DIR/ui-dist"
    if [[ -d "$HOME_DIR" ]]; then
        if [[ "$NO_PROMPT" == "1" ]]; then ui_info "保留: $HOME_DIR"
        else read -p "删除 $HOME_DIR？(y/N) " -n1 -r; echo; [[ $REPLY =~ ^[Yy]$ ]] && rm -rf "$HOME_DIR" && ui_success "已删除: $HOME_DIR"
        fi
    fi
    ui_success "CradleRing 已卸载"
}

print_banner() {
    echo ""
    echo -e "${ACCENT}${BOLD}╔══════════════════════════════════════════════════╗${NC}"
    echo -e "${ACCENT}${BOLD}║          CradleRing Installer                     ║${NC}"
    echo -e "${ACCENT}${BOLD}║          企业级 AI Agent 协作平台                 ║${NC}"
    echo -e "${ACCENT}${BOLD}╚══════════════════════════════════════════════════╝${NC}"
    echo ""
    ui_kv "安装方式" "$INSTALL_METHOD"
    ui_kv "版本" "$VERSION"
    ui_kv "安装目录" "$BIN_DIR"
    ui_kv "数据目录" "$HOME_DIR"
    echo ""
}

show_footer() {
    echo ""
    echo -e "${ACCENT}${BOLD}══════════════════════════════════════════════════${NC}"
    echo ""
    ui_success "CradleRing 安装完成！"
    echo ""
    # 显示登录凭据（如果存在）
    if [[ -f "$HOME_DIR/data/.admin_credentials" ]]; then
        local creds
        creds="$(cat "$HOME_DIR/data/.admin_credentials")"
        local username password
        username="$(echo "$creds" | cut -d: -f1)"
        password="$(echo "$creds" | cut -d: -f2)"
        echo -e "  ${WARN}${BOLD}┌─────────────────────────────────────────┐${NC}"
        echo -e "  ${WARN}${BOLD}│          登录凭据（请妥善保管）           │${NC}"
        echo -e "  ${WARN}${BOLD}└─────────────────────────────────────────┘${NC}"
        echo ""
        echo -e "  ${MUTED}用户名:${NC} ${INFO}${BOLD}$username${NC}"
        echo -e "  ${MUTED}密码:${NC}   ${INFO}${BOLD}$password${NC}"
        echo ""
        echo -e "  ${MUTED}⚠️  此密码仅显示一次，请立即保存或修改${NC}"
        echo ""
    fi
    echo -e "  ${INFO}cradle-ring gateway start${NC}    启动网关"
    echo -e "  ${INFO}cradle-ring gateway status${NC}   查看状态"
    echo -e "  ${INFO}cradle-ring doctor${NC}          运行诊断"
    echo ""
    echo -e "  ${MUTED}浏览器:${NC} ${INFO}http://127.0.0.1:18800${NC}"
    echo -e "  ${MUTED}卸载:${NC} ./install.sh --uninstall"
    echo ""
}

# 生成随机管理员凭据
generate_admin_credentials() {
    local cred_file="$HOME_DIR/data/.admin_credentials"
    # 如果已存在，不重复生成
    [[ -f "$cred_file" ]] && return 0
    # 生成随机用户名（admin_ + 6位随机字符）和随机密码（16位）
    local rand_suffix rand_password
    rand_suffix="$(head -c4 /dev/urandom | xxd -p 2>/dev/null || date +%s%N | sha256sum | cut -c1-6)"
    rand_password="$(head -c12 /dev/urandom | base64 | tr -dc 'a-zA-Z0-9!@#$%^&*' | head -c16 2>/dev/null || date +%s%N | sha256sum | cut -c1-16)"
    local username="admin_${rand_suffix}"
    # 保存到凭据文件（仅 root 可读）
    mkdir -p "$HOME_DIR/data"
    echo "${username}:${rand_password}" > "$cred_file"
    chmod 600 "$cred_file"
    ui_success "已生成管理员凭据"
    ui_info "用户名: $username"
    ui_info "密码: $rand_password"
    ui_warn "请立即保存此凭据，系统不会再次显示"
}

main() {
    [[ "$ACTION" == "uninstall" ]] && { do_uninstall; exit 0; }
    print_banner
    detect_os_or_die
    ensure_rust_toolchain
    install_cradle_ring
    build_webui
    install_binary_and_ui
    ensure_path
    initialize_config
    generate_admin_credentials
    install_systemd_service
    install_launchd_service

    # Onboarding（交互式配置向导）
    if [[ "$NO_ONBOARD" != "1" ]] && [[ -t 0 ]]; then
        local claw="$BIN_DIR/cradle-ring"
        if [[ -x "$claw" ]]; then
            ui_step "启动配置向导..."
            if [[ "$DRY_RUN" != "1" ]]; then
                "$claw" onboard </dev/tty || true
            fi
        fi
    else
        run_doctor
    fi

    [[ "$VERIFY_INSTALL" == "1" ]] && verify_installation
    maybe_open_dashboard
    show_footer
}

if [[ "${CRADLE_RING_INSTALL_SH_NO_RUN:-0}" != "1" ]]; then
    parse_args "$@"
    main
fi
