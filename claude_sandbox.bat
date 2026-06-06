@echo off
chcp 65001 >nul
set WORKSPACE="C:\\Users\\owner\\Desktop\\Apps\\DockerSandboxWs\\Valheim_ServerMaintainer"
set SANDBOX_NAME=claude-valheim_server_manager

:: 既存のサンドボックスが存在するか確認
docker sandbox ls 2>nul | findstr /C:"%SANDBOX_NAME%" >nul 2>&1
if %errorlevel%==0 (
    echo 既存のサンドボックス "%SANDBOX_NAME%" を起動します...
    if "%1"=="resume" (
        docker sandbox run %SANDBOX_NAME% -- --resume
    ) else if "%1"=="continue" (
        docker sandbox run %SANDBOX_NAME% -- --continue
    ) else (
        docker sandbox run %SANDBOX_NAME%
    )
) else (
    echo 新規サンドボックス "%SANDBOX_NAME%" を作成します...
    if "%1"=="resume" (
        docker sandbox run --name %SANDBOX_NAME% claude %WORKSPACE% -- --resume
    ) else if "%1"=="continue" (
        docker sandbox run --name %SANDBOX_NAME% claude %WORKSPACE% -- --continue
    ) else (
        docker sandbox run --name %SANDBOX_NAME% claude %WORKSPACE%
    )
)