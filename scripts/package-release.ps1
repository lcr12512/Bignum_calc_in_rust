#Requires -Version 5.1
<#
.SYNOPSIS
    在本地构建 BIGNUM 的 Windows 便携包（与 .github/workflows/release.yml 行为一致）。

.DESCRIPTION
    1. 校验工作区与工具链；2. 跑格式检查、测试与 Release 构建；
    3. 组装便携包（exe + 文档 + 可选 WebView2 官方引导程序）；
    4. 生成 zip 与 SHA256 校验文件。
    产物写入仓库根目录下的 dist-release\（已加入 .gitignore，不会污染 git 历史）。

.PARAMETER Tag
    版本标签，例如 v0.1.0。用作 zip 文件名。

.PARAMETER SkipWebView2
    不在线下载 MicrosoftEdgeWebview2Setup.exe（离线构建或不需要首次安装引导时使用）。

.PARAMETER SkipTest
    跳过 cargo test（仅在你已单独验证过时使用）。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\scripts\package-release.ps1 -Tag v0.1.0

.NOTES
    若系统禁止运行脚本，可用：
        powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\package-release.ps1 -Tag v0.1.0
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^v\d+\.\d+\.\d+([-+][0-9A-Za-z.-]+)?$')]
    [string]$Tag,

    [switch]$SkipWebView2,
    [switch]$SkipTest
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}

# ---------------------------------------------------------------------------
# 0. 前置检查
# ---------------------------------------------------------------------------
Write-Step "检查工具链与工作区"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "未找到 cargo，请先安装 Rust（https://rustup.rs）。"
}
Write-Host ("rustc: " + (rustc --version))
Write-Host ("cargo: " + (cargo --version))

foreach ($required in @('Cargo.toml', 'LICENSE', 'README.md', 'THIRD_PARTY_NOTICES.md', 'vendor\cookie')) {
    if (-not (Test-Path $required)) {
        throw "缺少必需文件/目录：$required。请先按 README.md 的「从源码构建」与 CONTRIBUTING.md 完成仓库准备。"
    }
}

if (Test-Path '.git') {
    $dirty = (git status --porcelain) 2>$null
    if ($dirty) {
        Write-Warning "工作区存在未提交改动，发布包会基于当前工作区内容构建。"
    }
}

# ---------------------------------------------------------------------------
# 1. 质量门禁
# ---------------------------------------------------------------------------
Write-Step "格式检查（cargo fmt --check）"
cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) { throw "代码格式不符合 rustfmt 要求，请先运行 cargo fmt --all。" }

Write-Step "Clippy（警告即失败）"
cargo clippy --all-targets --locked -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "Clippy 未通过，请先修复警告。" }

if (-not $SkipTest) {
    Write-Step "单元测试（cargo test --locked）"
    cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw "单元测试未通过，已中止打包。" }
}

Write-Step "Release 构建（cargo build --release --locked）"
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw "Release 构建失败。" }

$exe = Join-Path $repoRoot 'target\release\bignumcalc.exe'
if (-not (Test-Path $exe)) { throw "未找到构建产物：$exe" }

# ---------------------------------------------------------------------------
# 2. 组装便携包
# ---------------------------------------------------------------------------
$name  = "BIGNUM-calculator-$Tag-win64"
$outDir = Join-Path $repoRoot 'dist-release'
$stage  = Join-Path $outDir $name

Write-Step "组装便携包：$name"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item $exe $stage
Copy-Item 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md' $stage

if (-not $SkipWebView2) {
    Write-Step "下载 WebView2 官方引导程序（Microsoft Evergreen Bootstrapper）"
    $bootstrapper = Join-Path $stage 'MicrosoftEdgeWebview2Setup.exe'
    try {
        Invoke-WebRequest -Uri 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' `
            -OutFile $bootstrapper -UseBasicParsing -TimeoutSec 120
        Write-Host ("已下载：" + $bootstrapper + "（" + (Get-Item $bootstrapper).Length + " 字节）")
    } catch {
        Write-Warning "下载 WebView2 引导程序失败：$($_.Exception.Message)。已跳过，便携包仍可分发（程序会提示官方下载地址）。"
    }
} else {
    Write-Host "按参数跳过 WebView2 引导程序。"
}

$zip = Join-Path $outDir "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
$shaFile = "$zip.sha256"
"$hash  $name.zip" | Set-Content -Encoding ascii $shaFile

# ---------------------------------------------------------------------------
# 3. 汇总
# ---------------------------------------------------------------------------
Write-Step "完成"
Write-Host ("便携包 : " + $zip)
Write-Host ("大小   : " + [math]::Round((Get-Item $zip).Length / 1MB, 2) + " MB")
Write-Host ("SHA256 : " + $hash)
Write-Host ("校验文件: " + $shaFile)
Write-Host ""
Write-Host "下一步：在 GitHub 创建 Release（建议打 v* 标签让 release.yml 自动完成），或手动上传上面两个文件。" -ForegroundColor Green
