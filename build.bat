@echo off
setlocal

rem Build the release binaries from the project root regardless of where this
rem .bat is invoked from. %~dp0 is the directory containing this script.
pushd "%~dp0"

set LOGFILE=build.log

rem cargo writes progress to stderr. We merge stderr into stdout and redirect
rem both to %LOGFILE% in one shot. This:
rem   - avoids PowerShell's NativeCommandError noise (no Tee-Object pipeline).
rem   - produces a plain ANSI / UTF-8 log that's easy to grep.
rem Live progress is replaced with a single `type` dump at the end — the
rem trade-off is acceptable because incremental builds are short.
cargo build --release -p gui-slint -p ctrlc-helper > "%LOGFILE%" 2>&1
set EXITCODE=%ERRORLEVEL%

type "%LOGFILE%"

popd

if %EXITCODE% NEQ 0 (
    echo.
    echo Build failed with exit code %EXITCODE%.
    echo Full log: %~dp0%LOGFILE%
    exit /b %EXITCODE%
)

echo.
echo Built:
echo   %~dp0target\release\valheim-server-manager.exe
echo   %~dp0target\release\ctrlc-helper.exe
echo Log:   %~dp0%LOGFILE%
endlocal
