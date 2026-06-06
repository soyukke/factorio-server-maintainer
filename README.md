# Valheim Server Maintainer

**Valheim** の個人用専用サーバー (Steam バックエンド + playit.gg 想定) を運用するための Rust + Slint 製 GUI ツール。Windows 専用。マルチサーバーは非対応。

設計から実装まで [Claude Code](https://github.com/anthropics/claude-code) とのペアプログラミングで進めた個人開発プロジェクトです。仕様は [`docs/valheim-server-manager-spec.md`](docs/valheim-server-manager-spec.md) にまとめてあります。

---

## 主要機能

- **セットアップ補助** — 初回起動時に設定画面が自動表示、ファイル / フォルダピッカーから各パスを指定 → ディレクトリ自動作成 + `config.toml` 書き出し
- **SteamCMD 自動取得** — `steamcmd.exe` が無ければ Valve 公式 zip を自動ダウンロード・展開、自己更新リトライ込みで「インストール / 更新」ボタン 1 つで Valheim 本体の初回インストールが完了
- **クリーン停止** — `ctrlc-helper.exe` 経由で実 Ctrl+C をサーバーコンソールへ送出、Valheim 側のシャットダウンセーブを必ず走らせて `.db` 破損リスクを回避
- **再 attach** — GUI を閉じてもサーバーは生き残る (`CREATE_BREAKAWAY_FROM_JOB` 付き spawn)。次回 GUI 起動時に `state.toml` の PID を `OpenProcess` で照会、生きていれば自動的にステータス「実行中」で復帰
- **接続中プレイヤー一覧** — ログから `Got connection SteamID` / `Closing socket` を tail してリアルタイム表示
- **バックアップ / ロールバック** — 別ウィンドウで管理。手動スナップショット + ロールバック前自動退避 (`pre_rollback`) + 複数選択 + 一括削除 + 日時 / サイズ ソート
- **ワールド設定** — 別ウィンドウのカテゴリサイドバー切替。プリセット + 5 modifier (combat / deathpenalty / resources / raids / portals) + 7 個の `-setkey` トグル (クリエイティブ建築 / ミニマップ無し 等) を JA / EN 説明付きで編集
- **公開アドレス共有** — playit.gg トンネル名 / グローバル IP などを保存、ワンクリックでクリップボードへコピー
- **JA / EN i18n** — ライブ切替対応、選択は `config.toml` に永続化

## クイックスタート

ビルドには Rust toolchain (1.75 以上) と Visual Studio C++ Build Tools が必要です。

```cmd
build.bat
```

`target\release\` に `valheim-server-manager.exe` と `ctrlc-helper.exe` が出力されます。任意のフォルダ (`Manager` ディレクトリ) に両方をコピーして GUI を起動:

```cmd
mkdir "C:\Path\To\Manager"
copy /Y target\release\valheim-server-manager.exe "C:\Path\To\Manager\"
copy /Y target\release\ctrlc-helper.exe          "C:\Path\To\Manager\"
"C:\Path\To\Manager\valheim-server-manager.exe"
```

初回起動時は `config.toml` が無いので、Settings 画面で各パス (SteamCMD / Server / Save / Backup / Log) を指定して「保存して再起動」。再起動後に「インストール / 更新」を押すと SteamCMD のブートストラップから Valheim 本体 (`app_update 896660`) まで自動で走り、最後に「サーバー起動」で起動します。

## 構成

```
crates/
├─ gsm-core/     GUI / ゲーム非依存の核 (config, process, logtail, backup, steamcmd, GameServerManager trait)
├─ ctrlc-helper/ 単独 exe (Ctrl+C 送出用ヘルパー)
├─ valheim/      gsm-core::GameServerManager の Valheim 実装 (argv 構築 / ログパース / 再 attach)
└─ gui-slint/    Slint UI (MainWindow + BackupWindow + WorldSettingsWindow の 3 ウィンドウ)
```

## ドキュメント

- 📘 **[実装仕様](docs/valheim-server-manager-spec.md)** — 設計の前提、アーキテクチャ、§ ごとの実装方針

## ライセンス

MIT — [`LICENSE`](./LICENSE) を参照。
