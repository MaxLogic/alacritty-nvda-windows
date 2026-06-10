@echo off
setlocal

set "REPO_ROOT=%~dp0..\.."
for %%I in ("%REPO_ROOT%") do set "REPO_ROOT=%%~fI"

set "ALACRITTY_EXE=%REPO_ROOT%\target\x86_64-pc-windows-msvc\current\alacritty.exe"
set "TRACE_DIR=%REPO_ROOT%\target\uia-trace"
set "ALACRITTY_UIA_DEBUG_TRACE=%TRACE_DIR%\alacritty-uia-debug.log"
set "ALACRITTY_UIA_EVENT_TRACE=%TRACE_DIR%\alacritty-uia-events.log"
set "LAUNCHER_TRACE=%TRACE_DIR%\launcher-started.txt"

if not exist "%ALACRITTY_EXE%" (
    echo Alacritty executable not found:
    echo %ALACRITTY_EXE%
    pause
    exit /b 1
)

if not exist "%TRACE_DIR%" mkdir "%TRACE_DIR%"
del "%ALACRITTY_UIA_DEBUG_TRACE%" "%ALACRITTY_UIA_EVENT_TRACE%" 2>nul

> "%LAUNCHER_TRACE%" (
    echo Started at %DATE% %TIME%
    echo Alacritty: %ALACRITTY_EXE%
    echo Debug trace: %ALACRITTY_UIA_DEBUG_TRACE%
    echo Event trace: %ALACRITTY_UIA_EVENT_TRACE%
)

start "Codex UIA Trace Launcher" "%ALACRITTY_EXE%" --title "Codex UIA Trace" --working-directory "%REPO_ROOT%"
