# Factorio Server Maintainer

Windows で Factorio 専用サーバーを 1 台管理するための GUI ツールです。

## 主な機能

- SteamCMD で Factorio をインストール / 更新 (`app_update 427520`)
- `bin\x64\factorio.exe` を `--start-server` 付きで起動
- セーブ zip が無ければ `<save_dir>\<world>.zip` を自動作成
- `ctrlc-helper.exe` でまず正常停止し、タイムアウト後だけ強制終了
- セーブ zip をコピーしてスナップショット作成、削除、ロールバック
- Base / Space Age を `mods\mod-list.json` で明示管理
- GUI から共有用の公開アドレスを保存・コピー

## 既定の配置

`just setup` はユーザーのホーム配下に隠しディレクトリ寄りのランタイム一式を作ります。

```text
%USERPROFILE%\.factorio-server-maintainer\
|-- SteamCMD\steamcmd.exe
|-- Server\
|   |-- bin\x64\factorio.exe
|   |-- logs\server.log
|   `-- mods\mod-list.json
`-- Saves\<world>.zip
```

バックアップは、ゲームごとに集約できる別ディレクトリへ保存します。

```text
%USERPROFILE%\.game-server-backups\
`-- factorio\<world>\<timestamp>\
```

`config.toml` 内のパスはすべて絶対パスです。

## セットアップ

ローカルで動かせる状態まで冪等に準備します。通常は Steam ユーザー名なしで始めます。

```powershell
just setup
```

`just setup` は Steam クライアントのログイン履歴から Steam ユーザー名を自動検出します。見つかった場合はそれを使って Factorio server を取得します。SteamCMD がパスワードと Steam Guard code を聞くことがあります。パスワードはこのアプリの `config.toml` には保存しません。保存するのは Steam ユーザー名だけです。以後は SteamCMD のログインキャッシュを使います。

Steam ユーザー名が見つからない場合は anonymous 取得を試します。SteamCMD が `No subscription` などで拒否した場合は、その場で Steam ユーザー名を聞きます。

このコマンドは次を行います。

- `%USERPROFILE%\.factorio-server-maintainer` を作成
- 旧 `%USERPROFILE%\FactorioServerMaintainer\SteamCMD` があれば SteamCMD を再利用
- SteamCMD が無ければインストールし、あれば更新
- `%USERPROFILE%\.game-server-backups\factorio` を作成
- release バイナリをビルド
- `factorio-server-manager.exe` と `ctrlc-helper.exe` をランタイムディレクトリへコピー
- `config.toml` が無い場合だけ既定値で作成
- Factorio server を SteamCMD で取得
- Steam ユーザー名が必要になった場合は `manager.steam_username` を保存

既定のサーバー名とパスワードは、ローカル初期セットアップ用に単純にしています。

```text
name: Factory
password: factorio
```

セットアップ後は次で起動します。

```powershell
just run
```

## DLC モード

`server.dlc` で Factorio の DLC プロファイルを選びます。

```toml
[server]
dlc = "base"
```

または:

```toml
[server]
dlc = "space_age"
```

`space_age` の場合、管理対象の `mods\mod-list.json` に以下の組み込み DLC mod を有効化して書き込みます。

- `elevated-rails`
- `quality`
- `space-age`

起動時は `--mod-directory <server_dir>\mods` を渡すため、このプロファイルが安定して使われます。

## SteamCMD

SteamCMD は自己更新型なので、固定バージョンのツールではなく `just` タスクとして扱います。

```powershell
just steamcmd-install
```

GUI 側にも、設定された `steamcmd.exe` が無い場合の自動取得処理があります。GUI の Steam ユーザー名欄は Steam クライアントのログイン履歴から自動入力されます。必要なら手で変更して保存できます。ユーザー名が入っている場合、次の Install / Update では SteamCMD のコンソールが表示され、パスワードや Steam Guard code を入力できます。

Steam ログインだけを後から明示的に行う場合:

```powershell
just steam-login your_steam_user
```

GUI の status summary には `SteamCMDログイン: anonymous` または Steam ユーザー名が表示されます。

## ビルド

```powershell
just build
```

release バイナリを作る場合:

```powershell
just release
```

出力:

```text
target\release\factorio-server-manager.exe
target\release\ctrlc-helper.exe
```

## チェック

```powershell
just precommit
```

実行内容:

- `just secrets`
- `just fmt`
- `just lint`
- `just test`

`just secrets` は gitleaks を `uvx pre-commit` 経由で全ファイルに実行します。`rustfmt` の幅は 100 です。Clippy は warning をエラーにし、関数 70 行ルールも有効です。GUI callback の長い配線だけは例外として許容しています。

## pre-commit hook

```powershell
just hook-install
```

hook はローカルと同じ `just` recipe を呼びます。

## コマンド一覧

```powershell
just --list
```

`mise run build` や `mise run test` などの mise タスクも残していますが、中身は対応する `just` recipe を呼ぶ薄い wrapper です。
