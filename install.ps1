# CradleRing Installer for Windows
# Usage: irm https://raw.githubusercontent.com/UA-Jin/CradleRing/main/install.ps1 | iex
#    or: .\install.ps1
#    or: powershell -ExecutionPolicy Bypass -File install.ps1

param(
    [switch]$NoOnboard,
    [switch]$NoDaemon,
    [string]$Version = "latest"
)

$ErrorActionPreference = "Stop"

# ---------- 颜色输出 ----------
function Write-Step    { param($msg) Write-Host "▸ $msg" -ForegroundColor Cyan }
function Write-Info    { param($msg) Write-Host "$msg" -ForegroundColor Gray }
function Write-Success { param($msg) Write-Host "✓ $msg" -ForegroundColor Green }
function Write-Warn    { param($msg) Write-Host "⚠ $msg" -ForegroundColor Yellow }
function Write-Err     { param($msg) Write-Host "✗ $msg" -ForegroundColor Red }
function Write-KV      { param($k, $v) Write-Host ("  {0,-16} {1}" -f "${k}:", $v) -ForegroundColor DarkGray }

$InstallDir = "$env:LOCALAPPDATA\Programs\cradle-ring"
$HomeDir = "$env:USERPROFILE\.cradle-ring"
$BinDir = "$InstallDir\bin"

Write-Host ""
Write-Host "╔══════════════════════════════════════════════════╗" -ForegroundColor Cyan
Write-Host "║          CradleRing Installer (Windows)          ║" -ForegroundColor Cyan
Write-Host "║          企业级 AI Agent 协作平台                ║" -ForegroundColor Cyan
Write-Host "╚══════════════════════════════════════════════════╝" -ForegroundColor Cyan
Write-Host ""
Write-KV "安装方式" "cargo"
Write-KV "版本" "$Version"
Write-KV "安装目录" "$InstallDir"
Write-KV "数据目录" "$HomeDir"
Write-Host ""

# ---------- 检测架构 ----------
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
Write-Info "操作系统: windows $arch"

# ---------- 确保 Rust 工具链 ----------
function Ensure-Rust {
    if (Get-Command rustc -ErrorAction SilentlyContinue) {
        $v = (rustc --version).Split(' ')[1]
        Write-Info "Rust 版本: $v"
        return
    }
    Write-Warn "未检测到 Rust 工具链"
    Write-Step "安装 Rust（rustup-init.exe）..."
    $rustup = "$env:TEMP\rustup-init.exe"
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustup
    & $rustup -y --default-toolchain stable | Out-Null
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
        Write-Err "Rust 安装后仍无法检测，请重启 PowerShell 后重试"
        exit 1
    }
    Write-Success "Rust 已安装"
}

# ---------- 确保 C 编译器（MSVC 或 MinGW）----------
function Ensure-C-Toolchain {
    # 检测 cl (MSVC) 或 gcc (MinGW)
    if ((Get-Command cl -ErrorAction SilentlyContinue) -or (Get-Command gcc -ErrorAction SilentlyContinue)) {
        return
    }
    Write-Warn "未检测到 C 编译器（MSVC 或 MinGW），Rust 编译需要它"
    Write-Host ""
    Write-Host "  请手动安装以下任一选项：" -ForegroundColor Yellow
    Write-Host "  [推荐] Visual Studio Build Tools（MSVC）：" -ForegroundColor Yellow
    Write-Host "         https://visualstudio.microsoft.com/visual-cpp-build-tools/" -ForegroundColor Gray
    Write-Host "         安装时勾选「C++ 生成工具」" -ForegroundColor Gray
    Write-Host ""
    Write-Host "  [备选] MinGW-w64 GCC：" -ForegroundColor Yellow
    Write-Host "         https://www.mingw-w64.org/downloads/" -ForegroundColor Gray
    Write-Host "         安装后把 bin 目录加入 PATH" -ForegroundColor Gray
    Write-Host ""
    Write-Err "请先安装 C 编译器，然后重新运行本脚本"
    exit 1
}

# ---------- 下载源码 ----------
function Clone-Source {
    Write-Step "从 GitHub 下载源码..."
    $tmpDir = New-Item -ItemType Directory -Path ([System.IO.Path]::GetTempPath() + [System.IO.Path]::GetRandomFileName())
    $tarball = "$tmpDir\cradlering.tar.gz"
    Invoke-WebRequest -Uri "https://github.com/UA-Jin/CradleRing/archive/refs/heads/main.tar.gz" -OutFile $tarball
    # Windows 10+ 自带 tar
    tar -xzf $tarball -C $tmpDir
    Move-Item -Path "$tmpDir\CradleRing-main\*" -Destination $tmpDir -Force -ErrorAction SilentlyContinue
    Remove-Item "$tmpDir\CradleRing-main" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $tarball -Force -ErrorAction SilentlyContinue
    return $tmpDir.FullName
}

# ---------- 编译 ----------
Ensure-Rust
Ensure-C-Toolchain

Write-Step "编译 CradleRing（cargo build --release）..."
$srcDir = Clone-Source
Push-Location $srcDir
cargo build --release --bin cradle-ring
if ($LASTEXITCODE -ne 0) { Write-Err "编译失败"; Pop-Location; exit 1 }
Pop-Location
$binary = "$srcDir\target\release\cradle-ring.exe"
if (-not (Test-Path $binary)) { Write-Err "二进制未找到: $binary"; exit 1 }
Write-Success "编译完成"

# ---------- 安装 ----------
Write-Step "安装到 $InstallDir ..."
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
Copy-Item $binary "$BinDir\cradle-ring.exe" -Force
Write-Success "二进制: $BinDir\cradle-ring.exe"

# 加入 PATH（用户级）
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$BinDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$userPath;$BinDir", "User")
    Write-Success "已加入用户 PATH（重启 PowerShell 生效）"
}

# ---------- 生成管理员凭据 ----------
Write-Step "生成管理员凭据..."
New-Item -ItemType Directory -Force -Path "$HomeDir\data" | Out-Null
$credFile = "$HomeDir\data\.admin_credentials"
if (-not (Test-Path $credFile)) {
    $randSuffix = -join ((48..57) + (97..122) | Get-Random -Count 6 | ForEach-Object { [char]$_ })
    $randPassword = -join ((33..122) | Get-Random -Count 16 | ForEach-Object { [char]$_ })
    $username = "admin_$randSuffix"
    Set-Content -Path $credFile -Value "${username}:${randPassword}" -NoNewline
    Write-Success "已生成管理员凭据"
    Write-Info "用户名: $username"
    Write-Info "密码: $randPassword"
    Write-Warn "请立即保存此凭据，系统不会再次显示"
}

# ---------- 注册为 Windows 服务（可选，用 Task Scheduler）----------
if (-not $NoDaemon) {
    Write-Step "注册开机自启任务（Task Scheduler）..."
    $action = New-ScheduledTaskAction -Execute "$BinDir\cradle-ring.exe" -Argument "gateway start"
    $trigger = New-ScheduledTaskTrigger -AtLogOn
    $principal = New-ScheduledTaskPrincipal -UserId "$env:USERNAME" -RunLevel Highest
    try {
        Register-ScheduledTask -TaskName "CradleRing Gateway" -Action $action -Trigger $trigger -Principal $principal -Force | Out-Null
        Write-Success "已注册开机自启任务"
    } catch {
        Write-Warn "注册失败（可忽略）: $_"
    }
}

# ---------- 完成 ----------
Write-Host ""
Write-Host "══════════════════════════════════════════════════" -ForegroundColor Cyan
Write-Success "CradleRing 安装完成！"
Write-Host ""
if (Test-Path $credFile) {
    $creds = (Get-Content $credFile).Split(':')
    Write-Host "  ┌─────────────────────────────────────────┐" -ForegroundColor Yellow
    Write-Host "  │          登录凭据（请妥善保管）           │" -ForegroundColor Yellow
    Write-Host "  └─────────────────────────────────────────┘" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  用户名: $($creds[0])" -ForegroundColor White
    Write-Host "  密码:   $($creds[1])" -ForegroundColor White
    Write-Host ""
    Write-Host "  ⚠️  此密码仅显示一次，请立即保存或修改" -ForegroundColor DarkGray
    Write-Host ""
}
Write-Host "  cradle-ring gateway start    启动网关" -ForegroundColor Gray
Write-Host "  cradle-ring gateway status   查看状态" -ForegroundColor Gray
Write-Host "  cradle-ring doctor           运行诊断" -ForegroundColor Gray
Write-Host ""
Write-Host "  浏览器: http://127.0.0.1:18800" -ForegroundColor Gray
Write-Host ""

# 询问是否立即启动
if (-not $NoOnboard) {
    $answer = Read-Host "是否立即启动网关？[Y/n]"
    if ($answer -ne "n" -and $answer -ne "N") {
        Start-Process -FilePath "$BinDir\cradle-ring.exe" -ArgumentList "gateway start"
        Start-Sleep -Seconds 3
        Start-Process "http://127.0.0.1:18800"
    }
}
