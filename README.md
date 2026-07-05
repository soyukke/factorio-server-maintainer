# Factorio Server Maintainer

Windows-first GUI for running one Factorio dedicated server.

SteamCMD install/update, server start/stop, save switching, Space Age/Base mode,
backups, Mod Portal downloads, and shareable connection addresses are managed
from the app UI.

![Factorio Server Maintainer dashboard](docs/screenshots/dashboard.png)

The screenshots use the real app with anonymized demo data. They do not contain
real usernames, real paths, real IP addresses, or tokens.

## User Guides

- [日本語ユーザーガイド](docs/user-guide.ja.md)
- [English user guide](docs/user-guide.en.md)

## What It Does

- Installs and updates the Factorio dedicated server through SteamCMD
- Starts and stops the server safely from the GUI
- Creates a new save automatically when the selected world save does not exist
- Switches between existing save files from the Saves screen
- Selects Base or Space Age from the Server settings screen
- Manages snapshots and rollback for save zip files
- Uses Factorio's official auto pause so the world can stop progressing when empty
- Optionally stops the server after the last player leaves
- Downloads and enables mods from the Mod Portal
- Stores a copyable Tailscale, playit.gg, or public connection address
- Shows connected players and recent network/peer diagnostics

## Quick Start

Install the local runtime and server tools:

```powershell
just setup
```

Open the GUI:

```powershell
just run
```

`just setup` is idempotent. Running it again reuses existing files and fills in
only missing pieces.

## Default Folders

The app runtime is placed under the current Windows user:

```text
%USERPROFILE%\.factorio-server-maintainer\
|-- factorio-server-manager.exe
|-- ctrlc-helper.exe
|-- config.toml
|-- SteamCMD\steamcmd.exe
|-- Server\
|   |-- bin\x64\factorio.exe
|   |-- logs\server.log
|   `-- mods\mod-list.json
`-- Saves\<world>.zip
```

Backups are grouped separately by game:

```text
%USERPROFILE%\.game-server-backups\
`-- factorio\<world>\<timestamp>\
```

You can change these folders from the Folders screen in the GUI.

## Development

Run the same checks used by pre-commit:

```powershell
just precommit
```

Install the pre-commit hook:

```powershell
just hook-install
```

Useful commands:

```powershell
just --list
```

`mise run build`, `mise run test`, and `mise run fmt` are also available as thin
wrappers around the project commands.
