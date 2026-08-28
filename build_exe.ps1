# M3U8下载器 一键打包脚本
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File .\build_exe.ps1
#   或直接双击 build_exe.bat
#
# 产物：dist\M3U8下载器-V{版本号}.exe（便携版单文件）
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root

Write-Host "[1/3] 构建 release 版本..." -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Host "构建失败，请查看上方错误信息" -ForegroundColor Red
    exit 1
}

$version = (Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value

Write-Host "[2/3] 准备发布目录..." -ForegroundColor Cyan
$dist = Join-Path $root "dist"
if (Test-Path $dist) {
    Remove-Item $dist -Recurse -Force
}
New-Item -ItemType Directory -Path $dist | Out-Null

Write-Host "[3/3] 复制产物..." -ForegroundColor Cyan
$source = Join-Path $root "target\release\M3U8下载器.exe"
$target = Join-Path $dist "M3U8下载器-V$version.exe"
Copy-Item $source $target

Write-Host ""
Write-Host "打包完成：" -ForegroundColor Green
Write-Host "  产物: dist\M3U8下载器-V$version.exe"
Write-Host "  大小: $([math]::Round((Get-Item $target).Length / 1MB, 1)) MB"
Write-Host "  ffmpeg 未内置，运行时从 PATH 自动检测" -ForegroundColor DarkGray
