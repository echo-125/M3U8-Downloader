@echo off
rem M3U8 downloader - one-click release build.
rem This batch file is just a launcher; the actual logic lives in build_exe.ps1.
where pwsh >nul 2>nul
if %errorlevel%==0 (
    pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0build_exe.ps1"
) else (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build_exe.ps1"
)
if errorlevel 1 pause
