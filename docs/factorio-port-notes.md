# Factorio Server Manager メモ

## スコープ

このリポジトリは Factorio 専用の Windows GUI マネージャーです。主な実行環境は Windows 上の `factorio.exe` と、そのプロセス制御です。

WSL2 / Nix は編集や将来の Linux 対応には便利ですが、このアプリの主な実行経路ではありません。

## Factorio ランタイム前提

- Steam app id: `427520`
- 既定ポート: `34197`
- SteamCMD インストール後の Windows 実行ファイル: `bin/x64/factorio.exe`
- セーブファイル: `<save_dir>/<world>.zip`
- 任意のサーバー設定: `<save_dir>/server-settings.json`
- 管理対象 mod プロファイル: `<server_dir>/mods/mod-list.json`
- 既定ランタイムルート: `%USERPROFILE%/.factorio-server-maintainer`
- 既定バックアップルート: `%USERPROFILE%/.game-server-backups/factorio`

起動コマンドの形:

```text
factorio.exe ^
  --start-server <save.zip> ^
  --port <port> ^
  --console-log <log_file> ^
  --mod-directory <server_dir>\mods
```

`<save.zip>` が無い場合は、起動前に作成します。

```text
factorio.exe --create <save.zip>
```

`server-settings.json` が存在する場合は、起動時に次も追加します。

```text
--server-settings <save_dir>\server-settings.json
```

## DLC 管理

DLC は `config.toml` の `server.dlc` で表します。

```toml
[server]
dlc = "base"
```

または:

```toml
[server]
dlc = "space_age"
```

サーバー起動時に、管理対象の `mods/mod-list.json` を書き出します。`base` では `base` のみ有効、`space_age` では以下の組み込み DLC mod を有効にします。

- `elevated-rails`
- `quality`
- `space-age`

これにより、Factorio が別のユーザーデータディレクトリを見に行って DLC 状態がぶれる問題を避けます。

## Personal Respawn Anchor

`personal-respawn-anchor` は、マルチ向けの小さな自作 mod です。管理ツールには同梱せず、Mod Portal から取得します。

公開mod `respawn-beacon` は `force.set_spawn_position` を使うため、通常の協力マルチでは全員共通のスポーン地点を変更します。`personal-respawn-anchor` はチームのスポーン地点を触らず、プレイヤーごと・惑星ごとにアンカー位置を保存し、復活直後に本人だけをその位置へ移動します。アンカーを置くと、マップに `<プレイヤー名> spawn` のタグも追加します。

有効化する場合は `config.toml` または GUI の mod 管理で `enabled_mods` に `personal-respawn-anchor` を入れます。`respawn-beacon` と同時有効にすると挙動が混ざるため、どちらか片方だけを使います。

## SteamCMD

SteamCMD は自己更新型です。固定ツールではなく `just` タスクで bootstrap します。

```text
just steamcmd-install
```

GUI 側にも、設定された `steamcmd.exe` が存在しない場合の自動取得処理があります。

Steam 版 Factorio の取得で Steam アカウントが必要な場合、まずは通常の setup から入ります。setup は Steam クライアントの `config/loginusers.vdf` から最近使った Steam ユーザー名を検出します。

```text
just setup
```

Steam ユーザー名が検出できない場合は anonymous を試します。anonymous が `No subscription` などで拒否されたら、setup が Steam ユーザー名を聞きます。GUI では Steam ユーザー名欄を自動入力し、Install / Update で SteamCMD の入力コンソールを開きます。

明示的にログインだけ行う場合:

```text
just steam-login <steam_user>
```

パスワードは保存しません。SteamCMD のログインキャッシュに任せ、`config.toml` には `manager.steam_username` だけを書きます。

## 冪等セットアップ

```text
just setup
```

このコマンドは再実行しても安全です。ホーム配下の `%USERPROFILE%/.factorio-server-maintainer` と `%USERPROFILE%/.game-server-backups/factorio` を作成し、SteamCMD をインストールまたは更新し、release バイナリをビルドしてコピーします。Factorio server が未取得なら SteamCMD で取得を試します。既存の `config.toml` は原則上書きしませんが、旧既定値のランタイム内バックアップパスだけは集約バックアップパスへ移行します。

## 検証

```text
just precommit
```

format、Clippy、test を実行します。`rustfmt.toml` で幅 100、`clippy.toml` で関数長 70 行を設定しています。GUI callback の長い配線は現時点では例外として許容しています。
