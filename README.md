# Factorio Server Maintainer

Windows で Factorio 専用サーバーを管理するための GUI ツールです。

SteamCMD でのサーバー取得、Space Age / Base の切り替え、セーブ選択、バックアップ、Mod Portal からの mod 追加、Tailscale などの共有アドレス管理、プレイヤー不在時の一時停止・自動停止をまとめて扱えます。

![Factorio Server Maintainer dashboard screenshot](docs/screenshots/dashboard.png)

> スクリーンショットは README 用のダミー表示です。実ユーザー名、実パス、実IP、トークンは含めていません。

## できること

- SteamCMD で Factorio server をインストール / 更新
- GUI からサーバー起動・正常停止
- Base / Space Age の DLC プロファイル切り替え
- 既存セーブの選択、新しいワールド名でのセーブ作成
- セーブ zip のスナップショット作成、削除、ロールバック
- Factorio 公式 `auto_pause` による無人時のワールド一時停止
- 最後のプレイヤー退出後に、保存してからサーバーを自動停止
- autosave 更新時のバックアップコピー
- Mod Portal からの mod ダウンロードと有効化
- Tailscale / playit.gg / グローバルIPなどの共有アドレス保存・コピー
- 接続プレイヤー、peer状態、Tailscale ping / timeout などの簡易ネットワーク診断
- gitleaks / fmt / lint / test を pre-commit で実行

## クイックスタート

必要なもの:

- Windows
- Rust toolchain
- `mise`
- `just`
- `uvx` / `pre-commit`
- Factorio を所有している Steam アカウント

初回セットアップ:

```powershell
just setup
```

起動:

```powershell
just run
```

`just setup` は冪等です。すでに必要なファイルがある場合は再利用し、不足しているものだけ準備します。

## 既定の配置

ランタイムはユーザーディレクトリ直下の隠しディレクトリ風の場所に置きます。

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

バックアップはゲームごとに集約します。

```text
%USERPROFILE%\.game-server-backups\
`-- factorio\<world>\<timestamp>\
```

`config.toml` には絶対パスが保存されます。Steam パスワードや Factorio service token は保存しません。

## Steam / SteamCMD

`just setup` はまず Steam クライアントのログイン履歴から Steam ユーザー名を検出します。

SteamCMD がパスワードや Steam Guard code を要求する場合は、SteamCMD のコンソールで入力します。入力内容はこのツールの設定ファイルには保存されません。以後は SteamCMD 側のログインキャッシュを使います。

Steam ユーザー名を明示したい場合:

```powershell
just steam-login your_steam_user
```

SteamCMD だけを bootstrap したい場合:

```powershell
just steamcmd-install
```

## サーバー起動と停止

GUI の「サーバー操作」から起動・停止します。

起動時にセーブ zip が無ければ、Factorio の `--create` で自動作成します。停止時は `ctrlc-helper.exe` で Ctrl+C を送り、Factorio に保存させてから終了します。タイムアウトした場合だけ強制終了します。

## セーブ切り替え

既存セーブで遊ぶ:

1. サーバーを停止する
2. 「セーブ」画面で既存の `*.zip` を選ぶ
3. 「このセーブで保存」を押す
4. アプリ再起動後にサーバーを起動する

新しいワールドで始める:

1. サーバーを停止する
2. 「ワールド名」に新しい名前を入力する
3. 「このセーブで保存」を押す
4. 起動時に新しい `<world>.zip` が作成される

古いセーブ zip は削除しません。あとから一覧で選び直せます。

## バックアップ

このツールのバックアップは、Factorio のセーブ zip をスナップショットとしてコピーします。

- 手動スナップショット
- autosave 更新時の自動コピー
- ロールバック前の自動退避
- スナップショット削除
- スナップショットからのロールバック

サーバー実行中の手動バックアップは避け、停止中または自動保存後のコピーを使います。

## 無人時のワールド進行

Factorio 公式の `auto_pause` を使います。既定では ON です。

```json
"auto_pause": true
```

ON の場合、プレイヤーが 0 人になるとサーバープロセスは残ったまま、ワールド進行が一時停止対象になります。工場、汚染、敵襲、資源消費を無人で進めたくない場合は ON のまま使います。

さらに「プレイヤーがいなくなったらサーバーを停止」を ON にすると、最後のプレイヤーが抜けてから指定秒数後に正常停止します。停止前に誰かが戻ってきた場合は止めません。

## DLC モード

`server.dlc` でプロファイルを選びます。

```toml
[server]
dlc = "base"
```

または:

```toml
[server]
dlc = "space_age"
```

`space_age` の場合、管理対象の `mods\mod-list.json` に以下を有効化して書き込みます。

- `elevated-rails`
- `quality`
- `space-age`

起動時は `--mod-directory <server_dir>\mods` を渡すため、Factorio クライアント側の設定に引きずられにくくなります。

## Mod 管理

GUI の「Mod」画面から、Mod Portal 名を指定して追加できます。

```text
personal-respawn-anchor
```

ダウンロードには Factorio の `player-data.json` にある `service-username` / `service-token` を使います。トークンは `config.toml` には保存しません。

zip を手元に持っている場合は「mod zipを追加」から選べます。コピー後に mod 名を検出し、有効mod一覧へ追加します。

Gameplay mod はサーバーだけでは完結しません。参加者のクライアントにも同じ mod セットが必要です。Factorio は接続時に mod 同期を促します。

## Personal Respawn Anchor

`personal-respawn-anchor` は、プレイヤーごと・惑星ごとに復活アンカーを持てる自作 mod です。管理ツールには同梱せず、Mod Portal から取得します。

- Mod Portal: <https://mods.factorio.com/mod/personal-respawn-anchor>
- Source: <https://github.com/soyukke/personal-respawn-anchor>

古い `respawn-beacon` と同時に有効化すると挙動が混ざるため、どちらか片方だけを使います。

## ネットワーク診断

GUI の「ネットワーク」欄では、Factorio ログと Tailscale から取れる範囲の診断を表示します。

- peer の接続状態
- `DownloadingMap` / `TryingToCatchUp` / `InGame`
- Tailscale の direct / relay
- ping の最大値
- timeout 回数

Factorio のゲーム内プレイヤー別 ping そのものをサーバーから直接取ることはできません。かわりに、Tailscale ping と Factorio peer ログで「ネットワークが揺れているのか」「クライアント側が重いのか」を切り分けます。

## 開発

ビルド:

```powershell
just build
```

release:

```powershell
just release
```

チェック:

```powershell
just precommit
```

実行内容:

- gitleaks
- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings -D clippy::too_many_lines`
- `cargo test --workspace`

hook のインストール:

```powershell
just hook-install
```

## コマンド一覧

```powershell
just --list
```

`mise run build` / `mise run test` / `mise run fmt` も残しています。中身は対応する `just` recipe を呼ぶ薄い wrapper です。

## 注意

このリポジトリは `Valheim_ServerMaintainer` を Factorio 向けに移植したものです。現在は Factorio 単体管理を前提にしています。
