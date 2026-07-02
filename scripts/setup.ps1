param(
    [string]$SteamUser = $env:FACTORIO_STEAM_USER
)

$ErrorActionPreference = "Stop"

$Root = Join-Path $HOME ".factorio-server-maintainer"
$LegacyRoot = Join-Path $HOME "FactorioServerMaintainer"
$SteamCmdDir = Join-Path $Root "SteamCMD"
$SteamCmdExe = Join-Path $SteamCmdDir "steamcmd.exe"
$ServerDir = Join-Path $Root "Server"
$SaveDir = Join-Path $Root "Saves"
$BackupRoot = Join-Path $HOME ".game-server-backups"
$BackupDir = Join-Path $BackupRoot "factorio"
$LogDir = Join-Path $ServerDir "logs"
$ConfigPath = Join-Path $Root "config.toml"
$ManagerExe = Join-Path $Root "factorio-server-manager.exe"
$FactorioExe = Join-Path $ServerDir "bin\x64\factorio.exe"

function Save-SteamUsername {
    param([string]$Username)

    if (-not (Test-Path -LiteralPath $ConfigPath)) {
        return
    }

    $Config = Get-Content -LiteralPath $ConfigPath -Raw
    if ($Config -match '(?m)^steam_username\s*=') {
        $Config = $Config -replace '(?m)^steam_username\s*=.*$', "steam_username = `"$Username`""
    } else {
        $Config = $Config -replace '(?m)^public_address\s*=.*$', "`$0`nsteam_username = `"$Username`""
    }
    Set-Content -Path $ConfigPath -Value $Config -Encoding utf8
}

function Get-SteamAccountName {
    $LoginUserFiles = @(
        (Join-Path ${env:ProgramFiles(x86)} "Steam\config\loginusers.vdf"),
        (Join-Path $env:ProgramFiles "Steam\config\loginusers.vdf")
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }

    foreach ($File in $LoginUserFiles) {
        $Content = Get-Content -LiteralPath $File -Raw
        $Blocks = [regex]::Matches(
            $Content,
            '(?s)"\d+"\s*\{.*?"AccountName"\s*"([^"]+)".*?"MostRecent"\s*"1".*?\}'
        )
        if ($Blocks.Count -gt 0) {
            return $Blocks[0].Groups[1].Value
        }

        $Account = [regex]::Match($Content, '"AccountName"\s*"([^"]+)"')
        if ($Account.Success) {
            return $Account.Groups[1].Value
        }
    }

    return ""
}

function Install-FactorioServer {
    param([string]$Username)

    if ([string]::IsNullOrWhiteSpace($Username)) {
        Write-Host "Installing Factorio server with SteamCMD anonymous login."
        & $SteamCmdExe +force_install_dir $ServerDir +login anonymous +app_update 427520 validate +quit
        if ($LASTEXITCODE -eq 0) {
            return
        }

        Write-Host ""
        Write-Host "SteamCMD anonymous install failed. Factorio may require a purchased Steam account."
        $DetectedUser = Get-SteamAccountName
        if (-not [string]::IsNullOrWhiteSpace($DetectedUser)) {
            $InputUser = Read-Host "Steam username [$DetectedUser] (leave empty to use detected, '-' to skip)"
            if ($InputUser -eq "-") {
                Write-Host "Skipped Factorio server install. You can run just setup again later."
                return
            }
            if ([string]::IsNullOrWhiteSpace($InputUser)) {
                $Username = $DetectedUser
            } else {
                $Username = $InputUser
            }
        } else {
            $Username = Read-Host "Steam username (leave empty to skip server install)"
        }
        if ([string]::IsNullOrWhiteSpace($Username)) {
            Write-Host "Skipped Factorio server install. You can run just setup again later."
            return
        }
    }

    Write-Host "Installing Factorio server with SteamCMD user $Username"
    Write-Host "SteamCMD may ask for your password and Steam Guard code."
    & $SteamCmdExe +force_install_dir $ServerDir +login $Username +app_update 427520 validate +quit
    if ($LASTEXITCODE -ne 0) {
        throw "SteamCMD failed to install Factorio server for user $Username."
    }
    Save-SteamUsername -Username $Username
}

New-Item -ItemType Directory -Force -Path $Root, $SteamCmdDir, $ServerDir, $SaveDir, $BackupRoot, $BackupDir, $LogDir | Out-Null

if ([string]::IsNullOrWhiteSpace($SteamUser)) {
    $SteamUser = Get-SteamAccountName
}

$RunningManager = Get-Process -Name "factorio-server-manager" -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq $ManagerExe }
if ($RunningManager) {
    throw "Factorio Server Manager is running. Close the GUI and run `just setup` again."
}

if (-not (Test-Path -LiteralPath $SteamCmdExe)) {
    $LegacySteamCmdDir = Join-Path $LegacyRoot "SteamCMD"
    if (Test-Path -LiteralPath (Join-Path $LegacySteamCmdDir "steamcmd.exe")) {
        Copy-Item -Recurse -Force -Path (Join-Path $LegacySteamCmdDir "*") -Destination $SteamCmdDir
    } else {
        $Zip = Join-Path $env:TEMP "steamcmd.zip"
        Invoke-WebRequest -Uri "https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip" -OutFile $Zip
        Expand-Archive -Force -Path $Zip -DestinationPath $SteamCmdDir
    }
}

& $SteamCmdExe +quit

if (-not [string]::IsNullOrWhiteSpace($SteamUser)) {
    Write-Host "Logging in to SteamCMD as $SteamUser"
    Write-Host "SteamCMD may ask for your password and Steam Guard code."
    & $SteamCmdExe +login $SteamUser +quit
}

cargo build --release -p gui-slint -p ctrlc-helper

Copy-Item -Force -Path "target\release\factorio-server-manager.exe" -Destination $ManagerExe
Copy-Item -Force -Path "target\release\ctrlc-helper.exe" -Destination $Root

if (-not (Test-Path -LiteralPath $ConfigPath)) {
    $Config = @"
[paths]
steamcmd = "$($SteamCmdExe.Replace('\', '\\'))"
server_dir = "$($ServerDir.Replace('\', '\\'))"
save_dir = "$($SaveDir.Replace('\', '\\'))"
backup_dir = "$($BackupDir.Replace('\', '\\'))"
log_file = "$((Join-Path $LogDir 'server.log').Replace('\', '\\'))"

[server]
name = "Factory"
world = "Dedicated"
password = "factorio"
port = 34197
public = 0
save_interval = 900
backups = 4
crossplay = false
dlc = "base"

[manager]
graceful_stop_timeout_secs = 30
auto_backup_before_update = true
language = "ja"
public_address = ""
steam_username = "$SteamUser"
"@
    Set-Content -Path $ConfigPath -Value $Config -Encoding utf8
} else {
    $OldBackupDir = Join-Path $Root "Backups"
    $OldEscaped = $OldBackupDir.Replace('\', '\\')
    $NewEscaped = $BackupDir.Replace('\', '\\')
    $Config = Get-Content -LiteralPath $ConfigPath -Raw
    if ($Config.Contains("backup_dir = `"$OldEscaped`"")) {
        $Config = $Config.Replace("backup_dir = `"$OldEscaped`"", "backup_dir = `"$NewEscaped`"")
        Set-Content -Path $ConfigPath -Value $Config -Encoding utf8
    }
    $Config = Get-Content -LiteralPath $ConfigPath -Raw
    if ($Config -notmatch '(?m)^steam_username\s*=') {
        $Config = $Config -replace '(?m)^public_address\s*=.*$', "`$0`nsteam_username = `"$SteamUser`""
        Set-Content -Path $ConfigPath -Value $Config -Encoding utf8
    } elseif (-not [string]::IsNullOrWhiteSpace($SteamUser)) {
        $Config = $Config -replace '(?m)^steam_username\s*=.*$', "steam_username = `"$SteamUser`""
        Set-Content -Path $ConfigPath -Value $Config -Encoding utf8
    }
}

if (-not (Test-Path -LiteralPath $FactorioExe)) {
    Install-FactorioServer -Username $SteamUser
} else {
    Write-Host "Factorio server already exists:"
    Write-Host "  $FactorioExe"
}

Write-Host "Setup complete:"
Write-Host "  $Root"
Write-Host "Run:"
Write-Host "  $Root\factorio-server-manager.exe"
