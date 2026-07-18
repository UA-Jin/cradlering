# CradleRing Installer for Windows
# Usage: irm https://raw.githubusercontent.com/UA-Jin/CradleRing/main/install.ps1 | iex
#    or: .\install.ps1
#    or: powershell -ExecutionPolicy Bypass -File install.ps1

param(
    [switch]$NoOnboard,
    [switch]$NoDaemon,
    [switch]$DryRun,
    [string]$Version = "latest"
)

$ErrorActionPreference = "Stop"

# ============================================================================
# 颜色输出
# ============================================================================
function Write-Step    { param($msg) Write-Host "▸ $msg" -ForegroundColor Cyan }
function Write-Info    { param($msg) Write-Host "$msg" -ForegroundColor Gray }
function Write-Success { param($msg) Write-Host "✓ $msg" -ForegroundColor Green }
function Write-Warn    { param($msg) Write-Host "⚠ $msg" -ForegroundColor Yellow }
function Write-Err     { param($msg) Write-Host "✗ $msg" -ForegroundColor Red }
function Write-KV      { param($k, $v) Write-Host ("  {0,-16} {1}" -f "${k}:", $v) -ForegroundColor DarkGray }

# ============================================================================
# 全局路径
# ============================================================================
$InstallDir = "$env:LOCALAPPDATA\Programs\cradle-ring"
$HomeDir = "$env:USERPROFILE\.cradle-ring"
$BinDir = "$InstallDir\bin"
$DataDir = "$HomeDir\data"

# ============================================================================
# Banner
# ============================================================================
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

# 检测架构
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
Write-Info "操作系统: windows $arch"

# ============================================================================
# 检测包管理器（winget / choco）
# ============================================================================
function Get-PackageManager {
    if (Get-Command winget -ErrorAction SilentlyContinue) { return "winget" }
    if (Get-Command choco -ErrorAction SilentlyContinue) { return "choco" }
    return $null
}

# ============================================================================
# 确保 Rust 工具链
# ============================================================================
function Ensure-Rust {
    if (Get-Command rustc -ErrorAction SilentlyContinue) {
        $v = (rustc --version).Split(' ')[1]
        Write-Info "Rust 版本: $v"
        return
    }
    Write-Warn "未检测到 Rust 工具链"
    Write-Step "安装 Rust（rustup-init.exe）..."
    if ($DryRun) { Write-Info "[DRY-RUN] 跳过"; return }
    $rustup = "$env:TEMP\rustup-init.exe"
    try {
        Invoke-WebRequest -Uri "https://win.rustup.rs/$arch" -OutFile $rustup -UseBasicParsing
    } catch {
        # 兜底 x86_64
        Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustup -UseBasicParsing
    }
    & $rustup -y --default-toolchain stable --default-host "x86_64-pc-windows-msvc" | Out-Null
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
        Write-Err "Rust 安装后仍无法检测，请重启 PowerShell 后重试"
        exit 1
    }
    Write-Success "Rust 已安装"
}

# ============================================================================
# 确保 C 编译器（MSVC 或 MinGW）—— 自动安装
# ============================================================================
function Ensure-C-Toolchain {
    # 已经有 cl（MSVC）或 gcc（MinGW）
    if ((Get-Command cl -ErrorAction SilentlyContinue) -or (Get-Command gcc -ErrorAction SilentlyContinue)) {
        if (Get-Command cl -ErrorAction SilentlyContinue) {
            Write-Info "C 编译器: MSVC (cl.exe)"
        } else {
            Write-Info "C 编译器: MinGW (gcc)"
        }
        return
    }

    Write-Warn "未检测到 C 编译器，开始自动安装..."

    # 策略：直接下载 WinLibs MinGW-w64（最可靠，不依赖 winget 包 ID 是否准确）
    # 失败时再尝试 winget / choco
    Install-MinGW-Manual

    # 刷新 PATH
    Refresh-Path
    Add-MinGW-To-Path

    # 检查是否成功
    if (Get-Command gcc -ErrorAction SilentlyContinue) {
        Write-Success "C 编译器: MinGW (gcc) 已就绪"
        Switch-To-Gnu-Toolchain
        return
    }

    # 直接下载失败，尝试 winget / choco
    $pkgMgr = Get-PackageManager
    if ($pkgMgr -eq "winget") {
        Write-Step "通过 winget 安装 MinGW..."
        if (-not $DryRun) {
            # 尝试多个已知的 MinGW winget 包 ID
            $wingetIds = @(
                "BrechtSanders.WinLibs.POSIX.UCRT",
                "WinLibs.WinLibs.POSIX.UCRT",
                "niXman.mingw-w64"
            )
            $installed = $false
            foreach ($id in $wingetIds) {
                try {
                    Write-Info "尝试 winget 包: $id"
                    winget install --id $id --source winget --accept-package-agreements --accept-source-agreements -e --disable-interactivity 2>&1 | Out-Null
                    if ($LASTEXITCODE -eq 0) { $installed = $true; break }
                } catch { continue }
            }
            if (-not $installed) {
                # 通用搜索
                try {
                    winget install --query "MinGW" --source winget --accept-package-agreements --accept-source-agreements --disable-interactivity 2>&1 | Out-Null
                } catch {}
            }
        }
    } elseif ($pkgMgr -eq "choco") {
        Write-Step "通过 choco 安装 MinGW..."
        if (-not $DryRun) {
            try { choco install mingw -y } catch {}
        }
    }

    Refresh-Path
    Add-MinGW-To-Path

    # 最终校验
    if ((Get-Command cl -ErrorAction SilentlyContinue) -or (Get-Command gcc -ErrorAction SilentlyContinue)) {
        if (Get-Command gcc -ErrorAction SilentlyContinue) {
            Write-Success "C 编译器: MinGW (gcc) 已就绪"
            Switch-To-Gnu-Toolchain
        } else {
            Write-Success "C 编译器: MSVC (cl.exe) 已就绪"
        }
        return
    }

    # 仍然失败：提示用户手动安装
    Write-Err "自动安装 C 编译器失败"
    Write-Host ""
    Write-Host "  请手动安装以下任一选项：" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  [推荐] Visual Studio Build Tools（MSVC）：" -ForegroundColor Yellow
    Write-Host "         https://visualstudio.microsoft.com/visual-cpp-build-tools/" -ForegroundColor Gray
    Write-Host "         安装时勾选「C++ 生成工具」" -ForegroundColor Gray
    Write-Host ""
    Write-Host "  [备选] MinGW-w64 GCC（独立压缩包）：" -ForegroundColor Yellow
    Write-Host "         https://github.com/brechtsanders/winlibs_mingw/releases" -ForegroundColor Gray
    Write-Host "         解压后把 bin 目录加入 PATH" -ForegroundColor Gray
    Write-Host ""
    Write-Host "  [备选] 已安装但未在 PATH？" -ForegroundColor Yellow
    Write-Host "         请重启 PowerShell，或手动把 MinGW 的 bin 目录加入 PATH" -ForegroundColor Gray
    Write-Host ""
    Write-Err "请先安装 C 编译器，然后重新运行本脚本"
    exit 1
}

# 直接下载 MinGW-w64（最可靠的方式）
function Install-MinGW-Manual {
    Write-Step "下载 MinGW-w64（WinLibs UCRT 版本，无需 VS 依赖）..."
    if ($DryRun) { return }
    $mingwDir = "$InstallDir\mingw64"
    $zipFile = "$env:TEMP\winlibs-mingw.zip"

    # 多个镜像 URL（按优先级），任一可用即可
    $urls = @(
        # WinLibs UCRT POSIX SEH（最新稳定版）
        "https://github.com/brechtsanders/winlibs_mingw/releases/download/14.2.0posix-19.1.1-12.0.0-ucrt-r2/winlibs-x86_64-posix-seh-gcc-14.2.0-mingw-w64ucrt-12.0.0-r2.zip",
        # 备用：稍旧版本
        "https://github.com/brechtsanders/winlibs_mingw/releases/download/13.3.0posix-17.0.6-12.0.0-ucrt-r1/winlibs-x86_64-posix-seh-gcc-13.3.0-mingw-w64ucrt-12.0.0-r1.zip",
        # 备用：MSVCRT 版本（更小，兼容性更好）
        "https://github.com/brechtsanders/winlibs_mingw/releases/download/13.3.0posix-17.0.6-11.0.1-msvcrt-r1/winlibs-x86_64-posix-seh-gcc-13.3.0-mingw-w64msvcrt-11.0.1-r1.zip"
    )

    $downloaded = $false
    foreach ($url in $urls) {
        try {
            Write-Info "尝试下载: $($url.Split('/')[-1])"
            # GitHub 大文件需要更长的超时
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest -Uri $url -OutFile $zipFile -UseBasicParsing -TimeoutSec 300
            $ProgressPreference = 'Continue'
            if ((Test-Path $zipFile) -and ((Get-Item $zipFile).Length -gt 1MB)) {
                $downloaded = $true
                Write-Success "下载完成"
                break
            }
        } catch {
            Write-Info "此镜像失败，尝试下一个..."
            $ProgressPreference = 'Continue'
            continue
        }
    }

    if (-not $downloaded) {
        Write-Warn "所有下载镜像均失败"
        return
    }

    Write-Info "解压 MinGW 到 $InstallDir ..."
    try {
        # 清理旧的解压目录
        if (Test-Path "$InstallDir\mingw64") { Remove-Item "$InstallDir\mingw64" -Recurse -Force -ErrorAction SilentlyContinue }
        Expand-Archive -Path $zipFile -DestinationPath $InstallDir -Force

        # WinLibs 解压后通常是 mingw64 子目录，但有时是带版本号的目录
        if (-not (Test-Path "$InstallDir\mingw64\bin\gcc.exe")) {
            # 找到解压出来的含 gcc.exe 的目录
            $extracted = Get-ChildItem -Path $InstallDir -Directory | Where-Object { Test-Path "$($_.FullName)\bin\gcc.exe" } | Select-Object -First 1
            if ($extracted) {
                $tempName = "$InstallDir\__mingw_tmp__"
                Rename-Item -Path $extracted.FullName -NewName "__mingw_tmp__" -Force
                New-Item -ItemType Directory -Force -Path "$InstallDir\mingw64" | Out-Null
                # 移动内容（不是目录本身）
                Get-ChildItem -Path $tempName -Force | ForEach-Object {
                    Move-Item -Path $_.FullName -Destination "$InstallDir\mingw64\" -Force
                }
                Remove-Item $tempName -Force -ErrorAction SilentlyContinue
            }
        }

        Remove-Item $zipFile -Force -ErrorAction SilentlyContinue

        if (Test-Path "$InstallDir\mingw64\bin\gcc.exe") {
            Write-Success "MinGW-w64 已安装到 $InstallDir\mingw64"
        } else {
            Write-Warn "MinGW 解压后未找到 gcc.exe（目录结构异常）"
        }
    } catch {
        Write-Warn "MinGW 解压失败：$_"
        Remove-Item $zipFile -Force -ErrorAction SilentlyContinue
    }
}

# 把 MinGW 加入 PATH
function Add-MinGW-To-Path {
    $candidates = @(
        "$InstallDir\mingw64\bin",
        "$env:PROGRAMFILES\mingw-w64\bin",
        "${env:PROGRAMFILES(X86)}\mingw-w64\bin",
        "C:\msys64\mingw64\bin",
        "C:\mingw64\bin",
        "C:\tools\mingw64\bin"
    )
    foreach ($p in $candidates) {
        if (Test-Path "$p\gcc.exe") {
            if ($env:PATH -notlike "*$p*") {
                $env:PATH = "$p;$env:PATH"
                $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
                if ($userPath -notlike "*$p*") {
                    [Environment]::SetEnvironmentVariable("PATH", "$userPath;$p", "User")
                }
                Write-Info "已加入 PATH: $p"
            }
            return
        }
    }
}

# 刷新当前会话的 PATH（从注册表读取最新的用户+系统 PATH）
function Refresh-Path {
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    $sysPath = [Environment]::GetEnvironmentVariable("PATH", "Machine")
    $env:PATH = "$sysPath;$userPath"
}

# 切换到 GNU 工具链（只有 gcc 没有 MSVC 时）
function Switch-To-Gnu-Toolchain {
    try {
        # 检查当前默认工具链
        $current = rustup show 2>$null | Select-String "stable.*windows"
        if ($current -match "msvc") {
            Write-Step "切换 Rust 到 GNU 工具链（适配 MinGW）..."
            rustup default stable-x86_64-pc-windows-gnu 2>$null | Out-Null
            rustup target add x86_64-pc-windows-gnu 2>$null | Out-Null
            Write-Success "已切换到 GNU 工具链"
        }
    } catch {
        Write-Warn "工具链切换失败（可忽略，继续尝试）"
    }
}

# ============================================================================
# 下载源码
# ============================================================================
function Clone-Source {
    Write-Step "从 GitHub 下载源码..."
    if ($DryRun) { return $env:TEMP }
    $tmpDir = New-Item -ItemType Directory -Path ([System.IO.Path]::GetTempPath() + [System.IO.Path]::GetRandomFileName())
    $tarball = "$tmpDir\cradlering.tar.gz"
    try {
        Invoke-WebRequest -Uri "https://github.com/UA-Jin/CradleRing/archive/refs/heads/main.tar.gz" -OutFile $tarball -UseBasicParsing
    } catch {
        Write-Err "下载源码失败：$_"
        exit 1
    }
    # Windows 10+ 自带 tar
    tar -xzf $tarball -C $tmpDir
    Move-Item -Path "$tmpDir\CradleRing-main\*" -Destination $tmpDir -Force -ErrorAction SilentlyContinue
    Remove-Item "$tmpDir\CradleRing-main" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $tarball -Force -ErrorAction SilentlyContinue
    return $tmpDir.FullName
}

# ============================================================================
# 构建前端（webui）—— 必须在编译二进制之前完成（嵌入到二进制）
# ============================================================================
function Build-Webui {
    param([string]$SrcDir)
    $webuiDir = "$SrcDir\webui"
    if (-not (Test-Path "$webuiDir\package.json")) {
        Write-Warn "未找到 webui/ 目录，跳过前端构建（二进制会使用内嵌的默认 UI）"
        return
    }
    Write-Step "构建前端（Vue3 + Arco Design Pro）..."
    if ($DryRun) { return }

    # 检测 Node.js
    $hasNode = Get-Command node -ErrorAction SilentlyContinue
    $hasPnpm = Get-Command pnpm -ErrorAction SilentlyContinue
    $hasNpm = Get-Command npm -ErrorAction SilentlyContinue

    if (-not $hasNode) {
        Write-Warn "未检测到 Node.js，尝试安装..."
        $pkgMgr = Get-PackageManager
        if ($pkgMgr -eq "winget") {
            winget install OpenJS.NodeJS.LTS --source winget --accept-package-agreements --accept-source-agreements
        } elseif ($pkgMgr -eq "choco") {
            choco install nodejs-lts -y
        } else {
            Write-Warn "无法自动安装 Node.js，跳过前端构建"
            Write-Info "二进制会使用内嵌的默认 UI（功能完整）"
            return
        }
        Refresh-Path
        $hasNode = Get-Command node -ErrorAction SilentlyContinue
        if (-not $hasNode) {
            Write-Warn "Node.js 安装后仍无法检测，跳过前端构建"
            Write-Info "二进制会使用内嵌的默认 UI（功能完整）"
            return
        }
    }

    Write-Info "Node.js: $(node --version)"

    Push-Location $webuiDir
    try {
        if ($hasPnpm) {
            Write-Info "使用 pnpm 安装依赖..."
            pnpm install --no-frozen-lockfile --ignore-scripts 2>&1 | Select-Object -Last 3
            Write-Info "构建生产包..."
            # 直接调 vite 避免 pnpm 11 的 build 脚本检查
            if (Test-Path ".\node_modules\.bin\vite.cmd") {
                & .\node_modules\.bin\vite.cmd build 2>&1 | Select-Object -Last 5
            } else {
                pnpm run build 2>&1 | Select-Object -Last 5
            }
        } elseif ($hasNpm) {
            Write-Info "使用 npm 安装依赖..."
            npm install --no-frozen-lockfile 2>&1 | Select-Object -Last 3
            Write-Info "构建生产包..."
            npm run build 2>&1 | Select-Object -Last 5
        } else {
            Write-Info "安装 pnpm..."
            npm install -g pnpm 2>&1 | Out-Null
            pnpm install --no-frozen-lockfile --ignore-scripts 2>&1 | Select-Object -Last 3
            & .\node_modules\.bin\vite.cmd build 2>&1 | Select-Object -Last 5
        }

        if (Test-Path "$webuiDir\dist\assets") {
            Write-Success "前端构建完成 → $webuiDir\dist"
            # 同步到 ui-dist（编译时嵌入二进制）
            $uiDistDir = "$SrcDir\crates\cradle-ring\ui-dist"
            if (Test-Path $uiDistDir) { Remove-Item $uiDistDir -Recurse -Force }
            Copy-Item "$webuiDir\dist" $uiDistDir -Recurse
            Write-Info "已同步到 crates\cradle-ring\ui-dist（将嵌入二进制）"
        } else {
            Write-Warn "前端构建失败，二进制会使用内嵌的默认 UI"
        }
    } finally {
        Pop-Location
    }
}

# ============================================================================
# 编译二进制（嵌入最新 ui-dist）
# ============================================================================
function Build-Binary {
    param([string]$SrcDir)
    Write-Step "编译 CradleRing（cargo build --release）..."
    if ($DryRun) { return "$SrcDir\target\release\cradle-ring.exe" }
    Push-Location $SrcDir
    try {
        cargo build --release --bin cradle-ring
        if ($LASTEXITCODE -ne 0) {
            Write-Err "编译失败"
            exit 1
        }
    } finally {
        Pop-Location
    }
    $binary = "$SrcDir\target\release\cradle-ring.exe"
    if (-not (Test-Path $binary)) {
        Write-Err "二进制未找到: $binary"
        exit 1
    }
    Write-Success "编译完成"
    return $binary
}

# ============================================================================
# 安装二进制 + UI
# ============================================================================
function Install-Binary-And-Ui {
    param([string]$Binary)
    Write-Step "安装到 $InstallDir ..."
    if ($DryRun) { return }
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    Copy-Item $Binary "$BinDir\cradle-ring.exe" -Force
    Write-Success "二进制: $BinDir\cradle-ring.exe"

    # UI 已嵌入二进制（include_dir），无需额外部署 ui-dist

    # 加入 PATH（用户级）
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -notlike "*$BinDir*") {
        [Environment]::SetEnvironmentVariable("PATH", "$userPath;$BinDir", "User")
        Write-Success "已加入用户 PATH（重启 PowerShell 生效）"
    }
}

# ============================================================================
# 初始化配置
# ============================================================================
function Initialize-Config {
    Write-Step "初始化配置..."
    if ($DryRun) { return }
    New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
    New-Item -ItemType Directory -Force -Path "$HomeDir\workspace" | Out-Null
    $cfg = "$HomeDir\cradle-ring.json"
    if (Test-Path $cfg) {
        Write-Info "配置已存在: $cfg"
        return
    }
    # 生成 token
    $bytes = New-Object byte[] 16
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $token = -join ($bytes | ForEach-Object { $_.ToString("x2") })
    # 默认配置（绑定 127.0.0.1，安全）
    $defaultCfg = @{
        gateway = @{
            auth = @{ mode = "token"; token = $token }
            bind = "loopback"
            port = 18800
        }
        models = @{ primary = "gpt-4o-mini" }
        providers = @{
            openai = @{ apiKey = $null; baseUrl = "https://api.openai.com/v1" }
        }
    }
    $defaultCfg | ConvertTo-Json -Depth 10 | Set-Content -Path $cfg -Encoding UTF8
    Write-Success "配置已保存: $cfg"
    Write-Info "Token: $token"
}

# ============================================================================
# 生成管理员凭据
# ============================================================================
function Generate-Admin-Credentials {
    Write-Step "生成管理员凭据..."
    if ($DryRun) { return }
    $credFile = "$DataDir\.admin_credentials"
    if (Test-Path $credFile) {
        Write-Info "凭据已存在: $credFile"
        return
    }
    $randSuffix = -join ((48..57) + (97..122) | Get-Random -Count 6 | ForEach-Object { [char]$_ })
    $randPassword = -join ((33..122) | Get-Random -Count 16 | ForEach-Object { [char]$_ })
    $username = "admin_$randSuffix"
    Set-Content -Path $credFile -Value "${username}:${randPassword}" -NoNewline -Encoding UTF8
    Write-Success "已生成管理员凭据"
    Write-Info "用户名: $username"
    Write-Info "密码: $randPassword"
    Write-Warn "请立即保存此凭据，系统不会再次显示"
    return $credFile
}

# ============================================================================
# 注册开机自启（Task Scheduler）
# ============================================================================
function Register-Autostart {
    if ($NoDaemon) { return }
    Write-Step "注册开机自启任务（Task Scheduler）..."
    if ($DryRun) { return }
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

# ============================================================================
# 交互式配置向导（Onboarding）
# 与 install.sh 对齐：模型 / Embedding / 绑定地址
# ============================================================================
function Run-Onboarding {
    if ($NoOnboard) { return }
    if ($DryRun) { return }

    Write-Host ""
    Write-Step "【交互式配置向导】"
    Write-Host "  可选步骤，按 Ctrl+C 跳过。回车使用默认值。" -ForegroundColor DarkGray
    Write-Host ""

    $cfg = Get-Content "$HomeDir\cradle-ring.json" -Raw | ConvertFrom-Json
    $modified = $false

    # ===== 1. 模型 Provider =====
    Write-Host "  【1/3】选择大模型 Provider：" -ForegroundColor Cyan
    Write-Host "   1) OpenAI（默认 gpt-4o-mini）"
    Write-Host "   2) DeepSeek（国产，便宜）"
    Write-Host "   3) 智谱 GLM"
    Write-Host "   4) 通义千问 Qwen"
    Write-Host "   5) 月之暗面 Kimi"
    Write-Host "   6) Ollama（本地，无需 API Key）"
    Write-Host "   7) 跳过（稍后手动配置）"
    $providerChoice = Read-Host "选择 [1-7，默认 7]"
    if (-not $providerChoice) { $providerChoice = "7" }

    $providerName = ""
    $providerUrl = ""
    $providerKey = $null
    $providerModel = ""
    switch ($providerChoice) {
        "1" {
            $providerName = "openai"; $providerUrl = "https://api.openai.com/v1"
            $providerKey = Read-Host "OpenAI API Key (sk-...)"
            $providerModel = Read-Host "模型 [默认 gpt-4o-mini]"
            if (-not $providerModel) { $providerModel = "gpt-4o-mini" }
        }
        "2" {
            $providerName = "deepseek"; $providerUrl = "https://api.deepseek.com/v1"
            $providerKey = Read-Host "DeepSeek API Key (sk-...)"
            $providerModel = "deepseek-chat"
        }
        "3" {
            $providerName = "zhipu"; $providerUrl = "https://open.bigmodel.cn/api/paas/v4"
            $providerKey = Read-Host "智谱 API Key"
            $providerModel = "glm-4-flash"
        }
        "4" {
            $providerName = "qwen"; $providerUrl = "https://dashscope.aliyuncs.com/compatible-mode/v1"
            $providerKey = Read-Host "通义千问 API Key (sk-...)"
            $providerModel = "qwen-turbo"
        }
        "5" {
            $providerName = "moonshot"; $providerUrl = "https://api.moonshot.cn/v1"
            $providerKey = Read-Host "Kimi API Key (sk-...)"
            $providerModel = "moonshot-v1-8k"
        }
        "6" {
            $providerName = "ollama"; $providerUrl = "http://127.0.0.1:11434/v1"
            $providerModel = Read-Host "Ollama 模型 [默认 llama3.2]"
            if (-not $providerModel) { $providerModel = "llama3.2" }
        }
        default { Write-Info "跳过模型配置" }
    }

    if ($providerName) {
        # 重写 providers 节
        $cfg | Add-Member -NotePropertyName providers -NotePropertyValue ([PSCustomObject]@{
            $providerName = [PSCustomObject]@{
                apiKey = $providerKey
                baseUrl = $providerUrl
            }
        }) -Force
        $cfg.models.primary = $providerModel
        $modified = $true
        Write-Success "已配置: $providerName / $providerModel"
    }

    # ===== 2. Embedding 配置 =====
    Write-Host ""
    Write-Host "  【2/3】Embedding 配置（记忆系统向量检索用）：" -ForegroundColor Cyan
    Write-Host "   1) 本地模型（零成本，自动下载 ~100MB）"
    Write-Host "   2) 硅基流动 SiliconFlow（默认 Qwen3-VL-Embedding-8B，需 API Key）"
    Write-Host "   3) 跳过（使用内置默认，记忆系统仍可用）"
    $embChoice = Read-Host "选择 [1-3，默认 3]"
    if (-not $embChoice) { $embChoice = "3" }

    $embProvider = ""
    $embModel = ""
    $embKey = $null
    $embUrl = ""
    switch ($embChoice) {
        "1" {
            $embProvider = "local"
            $embModel = "BAAI/bge-small-zh-v1.5"
            Write-Info "已选择本地 Embedding（首次使用时自动下载）"
        }
        "2" {
            $embProvider = "siliconflow"
            $embModel = "Qwen/Qwen3-VL-Embedding-8B"
            $embUrl = "https://api.siliconflow.cn/v1"
            $embKey = Read-Host "硅基流动 API Key (sk-...)"
            if (-not $embKey) {
                Write-Warn "未填写 API Key，使用内置 Embedding"
                $embProvider = ""
            }
        }
        default { Write-Info "跳过 Embedding 配置（使用内置）" }
    }

    if ($embProvider) {
        $embCfg = [PSCustomObject]@{
            engine = "builtin"
            embedding = [PSCustomObject]@{
                provider = $embProvider
                model = $embModel
            }
        }
        if ($embUrl) {
            $embCfg.embedding | Add-Member -NotePropertyName baseUrl -NotePropertyValue $embUrl
        }
        if ($embKey) {
            $embCfg.embedding | Add-Member -NotePropertyName apiKey -NotePropertyValue $embKey
        }
        $cfg | Add-Member -NotePropertyName memory -NotePropertyValue $embCfg -Force
        $modified = $true
        Write-Success "已配置 Embedding: $embProvider / $embModel"
    }

    # ===== 3. 绑定地址 =====
    Write-Host ""
    Write-Host "  【3/3】网关绑定地址：" -ForegroundColor Cyan
    Write-Host "   1) 仅本机访问 127.0.0.1（推荐，安全）"
    Write-Host "   2) 开放外网访问 0.0.0.0（需防火墙放行 + 强密码）"
    $bindChoice = Read-Host "选择 [1-2，默认 1]"
    if (-not $bindChoice) { $bindChoice = "1" }
    switch ($bindChoice) {
        "1" { $cfg.gateway.bind = "loopback" }
        "2" {
            $cfg.gateway.bind = "all"
            Write-Warn "已选择 0.0.0.0，请确保防火墙放行端口 18800 且密码强度足够"
        }
    }
    $modified = $true

    if ($modified) {
        $cfg | ConvertTo-Json -Depth 10 | Set-Content -Path "$HomeDir\cradle-ring.json" -Encoding UTF8
        Write-Success "配置已保存"
    }
}

# ============================================================================
# 主流程
# ============================================================================

# 1. 确保工具链
Ensure-Rust
Ensure-C-Toolchain

# 2. 下载源码（先于构建）
$srcDir = Clone-Source

# 3. 构建前端（嵌入二进制，必须在 cargo build 之前）
Build-Webui -SrcDir $srcDir

# 4. 编译二进制（嵌入最新 ui-dist）
$binary = Build-Binary -SrcDir $srcDir

# 5. 安装
Install-Binary-And-Ui -Binary $binary

# 6. 初始化配置 + 凭据
Initialize-Config
$credFile = Generate-Admin-Credentials

# 7. 注册开机自启
Register-Autostart

# 8. 交互式配置向导
Run-Onboarding

# ============================================================================
# 完成
# ============================================================================
Write-Host ""
Write-Host "══════════════════════════════════════════════════" -ForegroundColor Cyan
Write-Success "CradleRing 安装完成！"
Write-Host ""
if ($credFile -and (Test-Path $credFile)) {
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
if (-not $NoOnboard -and -not $DryRun) {
    $answer = Read-Host "是否立即启动网关？[Y/n]"
    if ($answer -ne "n" -and $answer -ne "N") {
        Start-Process -FilePath "$BinDir\cradle-ring.exe" -ArgumentList "gateway start"
        Start-Sleep -Seconds 3
        Start-Process "http://127.0.0.1:18800"
    }
}
