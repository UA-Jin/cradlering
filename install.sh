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

# 确保 C 编译器（gcc/cc）可用，Rust 编译需要
ensure_c_toolchain() {
    if command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1; then
        return 0
    fi
    ui_warn "未检测到 C 编译器（cc/gcc），Rust 编译需要它"
    ui_step "安装 C 编译器..."
    if [[ "$DRY_RUN" == "1" ]]; then return 0; fi
    # 检测包管理器并安装
    if command -v apt-get >/dev/null 2>&1; then
        # Debian/Ubuntu
        apt-get update -qq >/dev/null 2>&1 || true
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential pkg-config libssl-dev 2>&1 | tail -3 || { ui_error "build-essential 安装失败"; exit 1; }
    elif command -v yum >/dev/null 2>&1; then
        # CentOS/RHEL/Fedora
        yum install -y -q gcc gcc-c++ make pkgconfig openssl-devel 2>&1 | tail -3 || { ui_error "gcc 安装失败"; exit 1; }
    elif command -v dnf >/dev/null 2>&1; then
        # Fedora
        dnf install -y -q gcc gcc-c++ make pkgconfig openssl-devel 2>&1 | tail -3 || { ui_error "gcc 安装失败"; exit 1; }
    elif command -v apk >/dev/null 2>&1; then
        # Alpine
        apk add --no-cache build-base pkgconfig openssl-dev 2>&1 | tail -3 || { ui_error "build-base 安装失败"; exit 1; }
    elif command -v pacman >/dev/null 2>&1; then
        # Arch
        pacman -S --noconfirm base-devel openssl 2>&1 | tail -3 || { ui_error "base-devel 安装失败"; exit 1; }
    else
        ui_error "无法检测包管理器，请手动安装 C 编译器（gcc/build-essential）"
        ui_info "  Debian/Ubuntu: apt-get install build-essential pkg-config libssl-dev"
        ui_info "  CentOS/RHEL:   yum install gcc gcc-c++ make pkgconfig openssl-devel"
        ui_info "  Alpine:        apk add build-base pkgconfig openssl-dev"
        exit 1
    fi
    # 验证安装
    if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
        ui_error "C 编译器安装后仍无法检测"
        exit 1
    fi
    ui_success "C 编译器已安装"
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

# 根据可用内存自动限制 cargo 并行度，避免 OOM (SIGKILL)
# 大依赖（如 serde/reqwest/tokio）release 编译单 crate 可能需要 ~1.5GB
configure_cargo_jobs() {
    # 用户已显式设置则尊重
    if [[ -n "${CARGO_BUILD_JOBS:-}" ]]; then
        ui_info "CARGO_BUILD_JOBS 已由用户设置: $CARGO_BUILD_JOBS"
        return 0
    fi
    local mem_mb=0
    # Linux: MemAvailable（最准确）
    if [[ -r /proc/meminfo ]]; then
        mem_mb=$(awk '/MemAvailable/ {printf "%d", $2/1024}' /proc/meminfo 2>/dev/null)
        [[ -z "$mem_mb" || "$mem_mb" == "0" ]] && mem_mb=$(awk '/MemTotal/ {printf "%d", $2/1024}' /proc/meminfo 2>/dev/null)
    fi
    # macOS: sysctl
    if [[ "$mem_mb" == "0" ]] && command -v sysctl >/dev/null 2>&1; then
        mem_mb=$(sysctl -n hw.memsize 2>/dev/null | awk '{printf "%d", $1/1024/1024}')
    fi
    # fallback
    [[ -z "$mem_mb" || "$mem_mb" == "0" ]] && mem_mb=2048

    # 每 GB 内存最多 1 个并行任务（保守估计）
    # 另外根据 CPU 核数限制（不超过 nproc）
    local nproc
    nproc=$(nproc 2>/dev/null || echo 4)
    local by_mem=$(( mem_mb / 1024 ))
    [[ "$by_mem" -lt 1 ]] && by_mem=1
    local jobs=$(( by_mem < nproc ? by_mem : nproc ))
    # 强制最小 1，最大 8
    [[ "$jobs" -lt 1 ]] && jobs=1
    [[ "$jobs" -gt 8 ]] && jobs=8

    export CARGO_BUILD_JOBS="$jobs"
    if [[ "$jobs" -le 2 ]]; then
        ui_warn "检测到内存 ${mem_mb}MB 较少，限制 cargo 并行度为 $jobs（防 OOM）"
        ui_info "如构建仍失败，可尝试: sudo swapfile 创建 2GB swap 后重试"
    else
        ui_info "内存 ${mem_mb}MB，cargo 并行度 = $jobs"
    fi
}

# 尝试创建 swap 文件（OOM 兜底，仅 root + Linux + 无 swap 时）
ensure_swap_for_build() {
    [[ "$EUID" -ne 0 ]] && return 0
    [[ "$(uname -s)" != "Linux" ]] && return 0
    # 已有 swap 则跳过
    local current_swap
    current_swap=$(awk '/SwapTotal/ {print $2}' /proc/meminfo 2>/dev/null)
    [[ -n "$current_swap" && "$current_swap" -gt 1048576 ]] && return 0  # >1GB 已够

    local swap_file="/swapfile_cradlering"
    if [[ -f "$swap_file" ]]; then
        # 已存在但未启用？
        swapon "$swap_file" 2>/dev/null && ui_info "已启用已有 swap: $swap_file"
        return 0
    fi
    # 仅在内存 <2GB 时主动创建
    local mem_mb
    mem_mb=$(awk '/MemTotal/ {printf "%d", $2/1024}' /proc/meminfo 2>/dev/null)
    [[ -z "$mem_mb" || "$mem_mb" -gt 2048 ]] && return 0

    ui_warn "内存 ${mem_mb}MB 较少，正在创建 2GB swap 以避免 OOM..."
    if fallocate -l 2G "$swap_file" 2>/dev/null || dd if=/dev/zero of="$swap_file" bs=1M count=2048 2>/dev/null; then
        chmod 600 "$swap_file" 2>/dev/null
        mkswap "$swap_file" >/dev/null 2>&1
        if swapon "$swap_file" 2>/dev/null; then
            ui_success "swap 已创建并启用: $swap_file (2GB)"
            # 注册卸载钩子（安装结束时可选保留）
            REGISTERED_SWAP_FILE="$swap_file"
        else
            ui_warn "swap 启用失败，继续构建（可能 OOM）"
            rm -f "$swap_file" 2>/dev/null
        fi
    else
        ui_warn "swap 文件创建失败，继续构建（可能 OOM）"
    fi
}

install_cradle_ring() {
    ui_step "编译 CradleRing（cargo build --release）..."
    # OOM 防护：限制并行度 + 创建 swap
    configure_cargo_jobs
    ensure_swap_for_build
    local src_dir="$SCRIPT_DIR"
    # curl|bash 模式：自动从 GitHub 克隆源码
    if is_curl_bash_mode; then
        echo -e "${INFO}检测到 curl|bash 模式，自动下载源码...${NC}" >&2
        src_dir="$(clone_source)"
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
        ui_warn "未找到 webui/ 目录，跳过前端构建"
        ui_warn "⚠️  这会导致网关页面 404！请确保后续能从其他位置部署 ui-dist"
        ui_info "如需手动构建：cd $src_dir/webui && pnpm install && pnpm build"
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
        # pnpm 11 构建脚本检查会导致失败，用 --ignore-scripts 跳过
        if [[ "$pkg_mgr" == "pnpm" ]]; then
            $pkg_mgr install --no-frozen-lockfile --ignore-scripts 2>&1 | tail -3
        else
            $pkg_mgr install --no-frozen-lockfile 2>&1 | tail -3
        fi
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
    # 关键：与 build_webui 一致地解析源码目录（curl|bash 模式下 SRC_DIR 是 clone 的临时目录）
    local src_for_ui="${SRC_DIR:-$SCRIPT_DIR}"
    local local_ui="$src_for_ui/crates/cradle-ring/ui-dist"
    local webui_dist="$src_for_ui/webui/dist"
    local ui_installed=0
    if [[ -d "$webui_dist/assets" ]]; then
        ui_step "安装 UI（Vue3 + Arco Design Pro）..."
        [[ "$DRY_RUN" != "1" ]] && { rm -rf "$BIN_DIR/ui-dist"; cp -r "$webui_dist" "$BIN_DIR/ui-dist"; }
        ui_installed=1
    elif [[ -d "$local_ui/assets" ]]; then
        ui_step "安装 UI..."
        [[ "$DRY_RUN" != "1" ]] && cp -r "$local_ui"/* "$BIN_DIR/ui-dist/"
        ui_installed=1
    fi
    # 最终校验：确保 index.html 真实存在，否则网关会 404
    if [[ "$DRY_RUN" != "1" ]]; then
        if [[ ! -f "$BIN_DIR/ui-dist/index.html" ]]; then
            ui_error "UI 部署失败：$BIN_DIR/ui-dist/index.html 不存在"
            ui_error "网关启动后页面会 404！请手动构建前端："
            ui_info "  cd $src_for_ui/webui && pnpm install && pnpm build"
            ui_info "  cp -r $src_for_ui/webui/dist/* $BIN_DIR/ui-dist/"
            return 1
        fi
        if [[ "$ui_installed" == "1" ]]; then
            ui_success "UI 文件已安装：$BIN_DIR/ui-dist"
        fi
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

# 交互式配置向导：模型 / 技能 / 绑定地址 / IM 渠道
run_onboarding() {
    [[ "$NO_ONBOARD" == "1" || ! -t 0 ]] && return 0
    [[ "$DRY_RUN" == "1" ]] && return 0
    local cfg="$HOME_DIR/cradle-ring.json"
    # 已有配置则跳过
    [[ -f "$cfg" ]] && { ui_info "配置已存在，跳过向导"; return 0; }

    ui_step "启动配置向导（可随时按 Ctrl+C 跳过）..."
    echo ""

    # ===== 1. 模型配置 =====
    ui_step "【1/4】模型配置（LLM Provider）"
    echo "  国际模型:"
    echo "    1) OpenAI (api.openai.com)"
    echo "    2) Anthropic Claude"
    echo "    3) Groq"
    echo "  国产模型:"
    echo "    4) DeepSeek 深度求索"
    echo "    5) 通义千问 Qwen（阿里云）"
    echo "    6) 智谱 AI（GLM / ChatGLM）"
    echo "    7) 月之暗面 Kimi"
    echo "    8) MiniMax"
    echo "    9) 零一万物 Yi"
    echo "   10) 阶跃星辰 StepFun"
    echo "   11) 百川智能 Baichuan"
    echo "   12) 讯飞星火 Spark"
    echo "   13) 百度文心 ERNIE（千帆）"
    echo "   14) 腾讯混元 Hunyuan"
    echo "   15) 商汤日日新 SenseNova"
    echo "   16) 天工 Skywork"
    echo "   17) 硅基流动 SiliconFlow（聚合平台）"
    echo "   18) 无问芯穹 Infinigence"
    echo "  其他:"
    echo "   19) Ollama（本地，无需 API key）"
    echo "   20) 跳过（稍后手动配置）"
    read -p "选择模型 [1-20，默认 20]: " provider_choice </dev/tty
    provider_choice="${provider_choice:-20}"

    local provider_name="" provider_key="" provider_url="" provider_model=""
    case "$provider_choice" in
        1) provider_name="openai"; provider_url="https://api.openai.com/v1"
           read -p "OpenAI API Key (sk-...): " provider_key </dev/tty
           read -p "模型 [默认 gpt-4o-mini]: " provider_model </dev/tty; provider_model="${provider_model:-gpt-4o-mini}" ;;
        2) provider_name="anthropic"; provider_url="https://api.anthropic.com/v1"
           read -p "Anthropic API Key (sk-ant-...): " provider_key </dev/tty
           read -p "模型 [默认 claude-3-5-sonnet]: " provider_model </dev/tty; provider_model="${provider_model:-claude-3-5-sonnet}" ;;
        3) provider_name="groq"; provider_url="https://api.groq.com/openai/v1"
           read -p "Groq API Key: " provider_key </dev/tty
           read -p "模型 [默认 llama-3.3-70b]: " provider_model </dev/tty; provider_model="${provider_model:-llama-3.3-70b}" ;;
        4) provider_name="deepseek"; provider_url="https://api.deepseek.com/v1"
           read -p "DeepSeek API Key: " provider_key </dev/tty
           read -p "模型 [默认 deepseek-chat]: " provider_model </dev/tty; provider_model="${provider_model:-deepseek-chat}" ;;
        5) provider_name="qwen"; provider_url="https://dashscope.aliyuncs.com/compatible-mode/v1"
           read -p "Qwen API Key: " provider_key </dev/tty
           read -p "模型 [默认 qwen-plus]: " provider_model </dev/tty; provider_model="${provider_model:-qwen-plus}" ;;
        6) provider_name="zhipu"; provider_url="https://open.bigmodel.cn/api/paas/v4"
           read -p "智谱 API Key: " provider_key </dev/tty
           read -p "模型 [默认 glm-4-plus]: " provider_model </dev/tty; provider_model="${provider_model:-glm-4-plus}" ;;
        7) provider_name="kimi"; provider_url="https://api.moonshot.cn/v1"
           read -p "Kimi API Key: " provider_key </dev/tty
           read -p "模型 [默认 moonshot-v1-8k]: " provider_model </dev/tty; provider_model="${provider_model:-moonshot-v1-8k}" ;;
        8) provider_name="minimax"; provider_url="https://api.minimax.chat/v1"
           read -p "MiniMax API Key: " provider_key </dev/tty
           read -p "模型 [默认 abab6.5s-chat]: " provider_model </dev/tty; provider_model="${provider_model:-abab6.5s-chat}" ;;
        9) provider_name="yi"; provider_url="https://api.01.ai/v1"
           read -p "零一万物 API Key: " provider_key </dev/tty
           read -p "模型 [默认 yi-large]: " provider_model </dev/tty; provider_model="${provider_model:-yi-large}" ;;
        10) provider_name="stepfun"; provider_url="https://api.stepfun.com/v1"
            read -p "阶跃星辰 API Key: " provider_key </dev/tty
            read -p "模型 [默认 step-2-16k]: " provider_model </dev/tty; provider_model="${provider_model:-step-2-16k}" ;;
        11) provider_name="baichuan"; provider_url="https://api.baichuan-ai.com/v1"
            read -p "百川 API Key: " provider_key </dev/tty
            read -p "模型 [默认 Baichuan4]: " provider_model </dev/tty; provider_model="${provider_model:-Baichuan4}" ;;
        12) provider_name="spark"; provider_url="https://spark-api-open.xf-yun.com/v1"
            read -p "讯飞星火 API Key: " provider_key </dev/tty
            read -p "模型 [默认 spark-4.0-ultra]: " provider_model </dev/tty; provider_model="${provider_model:-spark-4.0-ultra}" ;;
        13) provider_name="ernie"; provider_url="https://qianfan.baidubce.com/v2"
            read -p "百度千帆 API Key: " provider_key </dev/tty
            read -p "模型 [默认 ernie-4.0-8k]: " provider_model </dev/tty; provider_model="${provider_model:-ernie-4.0-8k}" ;;
        14) provider_name="hunyuan"; provider_url="https://api.hunyuan.cloud.tencent.com/v1"
            read -p "腾讯混元 SecretId: " provider_key </dev/tty
            read -p "模型 [默认 hunyuan-pro]: " provider_model </dev/tty; provider_model="${provider_model:-hunyuan-pro}" ;;
        15) provider_name="sensenova"; provider_url="https://api.sensenova.cn/v1"
            read -p "商汤 API Key: " provider_key </dev/tty
            read -p "模型 [默认 SenseNova-V6]: " provider_model </dev/tty; provider_model="${provider_model:-SenseNova-V6}" ;;
        16) provider_name="skywork"; provider_url="https://api.tiangong.cn/v1"
            read -p "天工 API Key: " provider_key </dev/tty
            read -p "模型 [默认 skywork-mega]: " provider_model </dev/tty; provider_model="${provider_model:-skywork-mega}" ;;
        17) provider_name="siliconflow"; provider_url="https://api.siliconflow.cn/v1"
            read -p "硅基流动 API Key: " provider_key </dev/tty
            read -p "模型 [默认 deepseek-chat]: " provider_model </dev/tty; provider_model="${provider_model:-deepseek-chat}" ;;
        18) provider_name="infinigence"; provider_url="https://api.infinigence.ai/v1"
            read -p "无问芯穹 API Key: " provider_key </dev/tty
            read -p "模型 [默认 qwen-plus]: " provider_model </dev/tty; provider_model="${provider_model:-qwen-plus}" ;;
        19) provider_name="ollama"; provider_url="http://localhost:11434/v1"; provider_key=""
            read -p "Ollama 模型 [默认 llama3.2]: " provider_model </dev/tty; provider_model="${provider_model:-llama3.2}" ;;
        20) ui_info "跳过模型配置" ;;
    esac

    # ===== 1b. Embedding 配置（记忆系统用）=====
    ui_step "【1b/4】Embedding 配置（记忆系统向量检索用）"
    echo "  1) 本地模型 BAAI/bge-small-zh-v1.5（零成本，自动下载 ~100MB，需 ~1GB 内存）"
    echo "  2) 硅基流动 SiliconFlow API（默认 Qwen/Qwen3-VL-Embedding-8B，需 API Key，按量计费，速度快）"
    echo "  3) 跳过（稍后手动配置，记忆系统暂不可用）"
    read -p "选择 Embedding [1-3，默认 1]: " embedding_choice </dev/tty
    embedding_choice="${embedding_choice:-1}"

    local embedding_provider="" embedding_model="" embedding_key="" embedding_url=""
    case "$embedding_choice" in
        1) embedding_provider="local"
           embedding_model="BAAI/bge-small-zh-v1.5"
           ui_info "已选择本地 Embedding（首次使用时自动下载模型，约 100MB）" ;;
        2) embedding_provider="siliconflow"
           embedding_model="Qwen/Qwen3-VL-Embedding-8B"
           embedding_url="https://api.siliconflow.cn/v1"
           read -p "硅基流动 API Key (sk-...): " embedding_key </dev/tty
           if [[ -z "$embedding_key" ]]; then
               ui_warn "未填写 API Key，Embedding 将不可用（可稍后配置）"
               embedding_provider=""
           fi ;;
        3) ui_info "跳过 Embedding 配置（记忆系统暂不可用）" ;;
    esac

    # ===== 2. 技能选择 =====
    ui_step "【2/4】技能选择（内置工具）"
    echo "  1) 全部启用（推荐）"
    echo "  2) 仅核心工具（exec/read_file/write_file/web_search/memory_save）"
    echo "  3) 跳过（稍后手动配置）"
    read -p "选择技能 [1-3，默认 1]: " skills_choice </dev/tty
    skills_choice="${skills_choice:-1}"
    local skills_list="*"
    case "$skills_choice" in
        1) skills_list="*" ;;
        2) skills_list="exec,read_file,write_file,web_search,memory_save" ;;
        3) skills_list="*" ;;
    esac

    # ===== 3. 绑定地址 =====
    ui_step "【3/4】网关绑定地址"
    echo "  1) 仅本机访问 127.0.0.1（推荐，安全）"
    echo "  2) 开放外网访问 0.0.0.0（需防火墙放行 + 强密码）"
    read -p "选择绑定 [1-2，默认 1]: " bind_choice </dev/tty
    bind_choice="${bind_choice:-1}"
    local bind_host="127.0.0.1"
    case "$bind_choice" in
        1) bind_host="127.0.0.1" ;;
        2) bind_host="0.0.0.0"; ui_warn "⚠️  已选择 0.0.0.0，请确保防火墙放行端口 18800 且密码强度足够" ;;
    esac

    # ===== 4. IM 渠道 =====
    ui_step "【4/4】IM 渠道（可多选，留空跳过）"
    echo "  输入要启用的渠道编号，用空格分隔，如：1 3 5"
    echo "  1) 飞书"
    echo "  2) 钉钉"
    echo "  3) Telegram"
    echo "  4) Discord"
    echo "  5) 企业微信"
    echo "  6) 跳过"
    read -p "选择渠道 [默认 6]: " channels_choice </dev/tty
    channels_choice="${channels_choice:-6}"

    local channels_json="{}"
    if [[ "$channels_choice" != "6" ]]; then
        local channel_configs=()
        for ch in $channels_choice; do
            case "$ch" in
                1) # 飞书
                   read -p "飞书 App ID: " feishu_appid </dev/tty
                   read -p "飞书 App Secret: " feishu_secret </dev/tty
                   [[ -n "$feishu_appid" ]] && channel_configs+=("\"feishu\": {\"enabled\": true, \"appId\": \"$feishu_appid\", \"appSecret\": \"$feishu_secret\"}") ;;
                2) # 钉钉
                   read -p "钉钉 App Key: " dingtalk_key </dev/tty
                   read -p "钉钉 App Secret: " dingtalk_secret </dev/tty
                   [[ -n "$dingtalk_key" ]] && channel_configs+=("\"dingtalk\": {\"enabled\": true, \"appKey\": \"$dingtalk_key\", \"appSecret\": \"$dingtalk_secret\"}") ;;
                3) # Telegram
                   read -p "Telegram Bot Token: " tg_token </dev/tty
                   [[ -n "$tg_token" ]] && channel_configs+=("\"telegram\": {\"enabled\": true, \"botToken\": \"$tg_token\"}") ;;
                4) # Discord
                   read -p "Discord Bot Token: " discord_token </dev/tty
                   [[ -n "$discord_token" ]] && channel_configs+=("\"discord\": {\"enabled\": true, \"botToken\": \"$discord_token\"}") ;;
                5) # 企业微信
                   read -p "企业微信 Corp ID: " wecom_corpid </dev/tty
                   read -p "企业微信 Agent ID: " wecom_agentid </dev/tty
                   read -p "企业微信 Secret: " wecom_secret </dev/tty
                   [[ -n "$wecom_corpid" ]] && channel_configs+=("\"wecom\": {\"enabled\": true, \"corpId\": \"$wecom_corpid\", \"agentId\": \"$wecom_agentid\", \"secret\": \"$wecom_secret\"}") ;;
            esac
        done
        if [[ ${#channel_configs[@]} -gt 0 ]]; then
            channels_json="{ $(IFS=,; echo "${channel_configs[*]}") }"
        fi
    fi

    # ===== 生成配置文件 =====
    ui_step "生成配置文件..."
    local token="$(head -c32 /dev/urandom | xxd -p -c32 2>/dev/null || date +%s%N | sha256sum | cut -c1-32)"
    local providers_json="{}"
    if [[ -n "$provider_name" ]]; then
        providers_json="{\"$provider_name\": {\"apiKey\": \"$provider_key\", \"baseUrl\": \"$provider_url\", \"model\": \"$provider_model\", \"enabled\": true}}"
    fi
    local primary_model="${provider_model:-gpt-4o-mini}"
    if [[ "$provider_choice" == "20" ]]; then primary_model="gpt-4o-mini"; fi

    # 生成 embedding JSON 配置
    local embedding_json="{ \"engine\": \"builtin\" }"
    if [[ -n "$embedding_provider" ]]; then
        if [[ "$embedding_provider" == "local" ]]; then
            embedding_json="{ \"engine\": \"builtin\", \"embedding\": { \"provider\": \"local\", \"model\": \"$embedding_model\" } }"
        else
            embedding_json="{ \"engine\": \"builtin\", \"embedding\": { \"provider\": \"$embedding_provider\", \"model\": \"$embedding_model\", \"baseUrl\": \"$embedding_url\", \"apiKey\": \"$embedding_key\" } }"
        fi
    fi

    cat > "$cfg" <<EOCFG
{
  "gateway": {
    "port": 18800,
    "bind": "$bind_host",
    "auth": { "mode": "token", "token": "$token" }
  },
  "providers": $providers_json,
  "models": { "primary": "$primary_model" },
  "skills": { "enabled": "$skills_list" },
  "channels": $channels_json,
  "memory": $embedding_json
}
EOCFG
    ui_success "配置已保存: $cfg"
    echo ""
    ui_info "配置摘要:"
    [[ -n "$provider_name" ]] && ui_info "  模型: $provider_name / $provider_model"
    [[ -n "$embedding_provider" ]] && ui_info "  Embedding: $embedding_provider / $embedding_model"
    ui_info "  绑定: $bind_host"
    ui_info "  渠道: $(echo "$channels_choice" | tr -d '\n')"
    ui_info "  Token: $token"
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
    ensure_c_toolchain
    # 关键顺序：先构建前端（生成 ui-dist），再编译二进制（embed ui-dist），最后部署
    build_webui
    install_cradle_ring
    install_binary_and_ui
    ensure_path
    initialize_config
    generate_admin_credentials
    install_systemd_service
    install_launchd_service

    # Onboarding（交互式配置向导）
    run_onboarding
    [[ "$NO_ONBOARD" == "1" ]] && run_doctor

    [[ "$VERIFY_INSTALL" == "1" ]] && verify_installation
    maybe_open_dashboard
    show_footer
}

if [[ "${CRADLE_RING_INSTALL_SH_NO_RUN:-0}" != "1" ]]; then
    parse_args "$@"
    main
fi
