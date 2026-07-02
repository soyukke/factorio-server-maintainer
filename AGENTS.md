# AGENTS.md

## プロジェクト

このリポジトリは Windows-first の Factorio サーバーマネージャーです。

主な作業ディレクトリは、このリポジトリのルートです。

```text
factorio-server-maintainer/
```

## 開発環境

Windows での開発を優先します。

- プロジェクトコマンドは `just` を使います。
- ツール導入と互換 wrapper には `mise` を使います。
- `justfile` の主なコマンド:
  - `just setup`
  - `just run`
  - `just build`
  - `just fmt`
  - `just lint`
  - `just test`
  - `just secrets`
  - `just precommit`
  - `just steamcmd-install`
  - `just steam-login <steam_user>`

## 現在の構成

- `crates/gsm-core`: サーバー管理の共通 core
- `crates/ctrlc-helper`: Windows の Ctrl+C helper
- `crates/factorio`: Factorio 実装
- `crates/gui-slint`: Factorio GUI

## Factorio 前提

- Steam app id: `427520`
- 既定ポート: `34197`
- SteamCMD インストール先の Windows 実行ファイル: `bin/x64/factorio.exe`
- セーブファイル: `<save_dir>/<world>.zip`
- 任意の設定ファイル: `<save_dir>/server-settings.json`
- 既定ランタイムルート: `%USERPROFILE%\.factorio-server-maintainer`
- 既定バックアップルート: `%USERPROFILE%\.game-server-backups\factorio`
- DLC mode は `server.dlc` で表します。
  - `base`
  - `space_age`

Space Age が有効な場合、manager は `mods/mod-list.json` に `elevated-rails`、`quality`、`space-age` を有効化して書き込みます。

SteamCMD のパスワードは保存しません。基本導線は `just setup` です。setup と GUI は Steam クライアントの `config/loginusers.vdf` から最近使った Steam ユーザー名を検出し、`manager.steam_username` にユーザー名だけ保存します。ユーザー名が検出できない場合だけ anonymous を試し、`No subscription` などで拒否されたら setup が Steam ユーザー名を聞きます。GUI では Install / Update で SteamCMD の入力コンソールを開きます。

公開リポジトリとして扱うため、秘密情報や個人情報の混入を避けます。`just secrets` は gitleaks を `uvx pre-commit` 経由で実行します。`config.toml`、SteamCMD のログインキャッシュ、ローカルランタイム、バックアップ、実Tailscale IP、個人ユーザー名はコミットしません。

## 作業ルール

- 変更はスコープを絞ります。
- Windows を主な実行ターゲットとして扱います。
- 変更後は基本的に次を実行します。

```text
just precommit
```
