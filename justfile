set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"]

runtime := env_var("USERPROFILE") / ".factorio-server-maintainer"
manager_exe := runtime / "factorio-server-manager.exe"

default:
    just --list

# Check formatting without modifying files.
fmt:
    cargo fmt --all --check

# Apply Rust formatting.
fmt-fix:
    cargo fmt --all

# Run Clippy with project lint policy.
lint:
    cargo clippy --workspace --all-targets -- -D warnings -D clippy::too_many_lines

# Run all workspace tests. Extra args are passed to cargo test.
test *args:
    cargo test --workspace {{args}}

# Build the full workspace in debug mode.
build:
    cargo build --workspace

# Build the GUI binary in debug mode.
build-gui:
    cargo build -p gui-slint

# Build release binaries for distribution.
release:
    cargo build --release -p gui-slint -p ctrlc-helper

# Idempotently prepare a runnable local install under ~/.factorio-server-maintainer.
setup steam_user="":
    if ("{{steam_user}}" -eq "") { powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/setup.ps1 } else { powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/setup.ps1 -SteamUser "{{steam_user}}" }

# Log in to SteamCMD and save the username to config.toml.
steam-login steam_user:
    powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File scripts/setup.ps1 -SteamUser "{{steam_user}}"

# Run the installed GUI. Runs setup first if the executable is missing.
run:
    if (-not (Test-Path -LiteralPath "{{manager_exe}}")) { just setup }; Start-Process -FilePath "{{manager_exe}}" -WorkingDirectory "{{runtime}}"

# Open the runtime directory in Explorer.
open-runtime:
    if (-not (Test-Path -LiteralPath "{{runtime}}")) { just setup }; explorer "{{runtime}}"

# Run the same checks used by pre-commit.
precommit: secrets fmt lint test

# Scan the working tree for committed secrets.
secrets:
    uvx pre-commit run gitleaks --all-files

# Install the local pre-commit hook.
hook-install:
    uvx pre-commit install

# Bootstrap SteamCMD into the default Factorio tools directory.
steamcmd-install:
    $ErrorActionPreference='Stop'; $dest=Join-Path $HOME '.factorio-server-maintainer\SteamCMD'; New-Item -ItemType Directory -Force -Path $dest | Out-Null; $exe=Join-Path $dest 'steamcmd.exe'; if (-not (Test-Path -LiteralPath $exe)) { $zip=Join-Path $env:TEMP 'steamcmd.zip'; Invoke-WebRequest -Uri 'https://steamcdn-a.akamaihd.net/client/installer/steamcmd.zip' -OutFile $zip; Expand-Archive -Force -Path $zip -DestinationPath $dest }; & $exe +quit
