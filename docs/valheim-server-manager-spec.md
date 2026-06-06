# Valheim Dedicated Server Manager — 実装仕様書

> 個人用 Valheim 専用サーバーを管理する Windows デスクトップ GUI。
> 実装は Claude Code に委譲する前提で、設計・要件・実装上の注意点をまとめる。

---

## 0. この文書の前提・スコープ

- 対象 OS: **Windows**（10/11）。Linux/macOS は対象外。
- 稼働形態: 個人サーバー。最大 10 人程度の身内向け。
- ネットワーク: **Steam バックエンド（`-crossplay` なし）** + **playit.gg の UDP トンネル**で外部公開する。
  - crossplay は使わない（playit と排他。crossplay はリレー経由で直 IP 接続不可のため）。
  - 副作用として接続できるのは Steam プレイヤーのみ。代わりに BepInEx 系 Mod が利用可能（将来拡張）。
- ストレージ: バックアップ/ロールバック運用でデータが増えるため、**本体・セーブ・バックアップすべて D ドライブ**に置く。
- 起動性: **GUI をどこからでも起動できる**こと（後述の絶対パス方針）。
- 将来: 同じ Rust + Slint で作っている別の同種 GUI と統合したい。本仕様は**最初から GUI 非依存・ゲーム非依存のコアを切る**方針で設計する。

---

## 1. 技術スタック

| 項目 | 採用 | 備考 |
|---|---|---|
| 言語 | Rust (edition 2021 以降) | |
| GUI | Slint | 宣言的 UI。ロジックは Rust 側に置く |
| 非同期 | tokio | 重い処理（SteamCMD・プロセス監視・ログ tail） |
| 設定 | serde + toml | 絶対パスを保持する単一ファイル |
| Windows API | windows-sys (or windows) | コンソール制御 / Ctrl+C 送出 |
| ログ | tracing | アプリ自身の診断ログ |

---

## 2. ファイルシステム構成（既定値）

```
D:\Valheim\
├─ Server\        ← 専用サーバー本体（SteamCMD が force_install_dir で展開）
│   └─ logs\
│       └─ server.log   ← valheim_server の -logFile 出力先
├─ Data\          ← -savedir 指定先（worlds_local 等が生成される）
│   └─ worlds_local\
│       ├─ <world>.db   / <world>.db.old
│       └─ <world>.fwl  / <world>.fwl.old
├─ Backups\       ← 本ツールが作るスナップショット
│   └─ <world>\<timestamp>\{<world>.db, <world>.fwl}
├─ SteamCMD\      ← steamcmd.exe（同梱 or ユーザー配置）
└─ Manager\       ← 本 GUI の exe + config.toml を固定配置
```

すべてのパスは **設定上は絶対パスで保持**する（相対パスは禁止）。GUI の実体（exe）は `Manager\` に固定し、ショートカットを任意の場所に置く運用とする（→ §8「どこからでも起動」）。

---

## 3. アーキテクチャ（cargo workspace）

```
gsm/                          (cargo workspace。将来は両プロジェクトをここに同居)
├─ Cargo.toml
└─ crates/
   ├─ gsm-core/               ★共有の核。GUI にもゲームにも依存しない
   │   ├─ trait GameServerManager / ServerStatus / ServerEvent
   │   ├─ process    (spawn・監視・終了)
   │   ├─ logtail    (ファイル tail → イベント生成)
   │   ├─ backup     (スナップショット・ロールバック)
   │   ├─ steamcmd   (インストール/更新ラッパ)
   │   └─ config     (serde + toml)
   ├─ ctrlc-helper/           ★単独 exe。指定 PID のコンソールへ Ctrl+C を送るだけ
   ├─ valheim/                gsm-core に対する Valheim 実装（薄い）
   └─ gui-slint/              Slint UI。valheim（将来は別ゲームも）を束ねる
```

**統合の方針**: 汎用ロジック（プロセス制御・Ctrl+C 送出・バックアップ・SteamCMD・設定・ログ tail）はすべて `gsm-core` / `ctrlc-helper` に寄せ、`valheim` クレートはパース規則とパラメータ定義だけを持つ薄い実装にする。統合後の最終形は「`gui-slint` 一枚が `Vec<Box<dyn GameServerManager>>` を保持する」だけになる。

---

## 4. コア抽象（gsm-core）

```rust
#[async_trait::async_trait]
pub trait GameServerManager: Send + Sync {
    /// 安定 ID。例: "valheim"
    fn id(&self) -> &str;

    /// SteamCMD でインストール or 更新。停止中であることを前提とする。
    async fn install_or_update(&self) -> Result<()>;

    /// サーバー起動。すでに起動中なら Err。
    async fn start(&self) -> Result<()>;

    /// 停止。graceful=true なら Ctrl+C 経由でセーブ完了を待つ。
    /// timeout 超過時のみ強制終了（呼び出し側へ警告イベントを流す）。
    async fn stop(&self, graceful: bool) -> Result<()>;

    fn status(&self) -> ServerStatus;

    /// ログ tail から生成されるイベントの購読。GUI はこれを UI へ反映する。
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ServerEvent>;

    async fn list_backups(&self) -> Result<Vec<Backup>>;
    async fn backup(&self) -> Result<Backup>;          // 停止 or アイドル前提
    async fn rollback(&self, id: BackupId) -> Result<()>; // 内部で停止→上書き→起動
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus { Stopped, Starting, Running, Stopping, Crashed }

#[derive(Clone)]
pub enum ServerEvent {
    Log(String),                 // 生ログ1行
    StatusChanged(ServerStatus),
    WorldSaved { at: chrono::DateTime<chrono::Local> },
    PlayerJoined { steam_id: u64 },
    PlayerLeft   { steam_id: u64 },
    ServerReady,                 // 起動完了・接続受付開始
    Warning(String),             // 例: 強制終了でセーブ未確定の可能性
}
```

`ServerEvent` は**サーバーのログを tail して生成**する（§6.5）。`status()` と「直近セーブ時刻」もこのイベント列から導出する。

---

## 5. 機能要件（モジュール）

| # | モジュール | MVP | 概要 |
|---|---|:--:|---|
| A | インストール / 更新 | ✅ | SteamCMD で `app 896660` を取得・更新 |
| B | プロセス制御 | ✅ | 起動 / 安全停止 / 状態表示 |
| C | パラメータ管理 | ✅ | 起動引数を編集・検証して config に保持 |
| D | バックアップ / ロールバック | ✅ | `.db`+`.fwl` をペアでスナップショット・復元 |
| E | ログ / ステータス表示 | ✅ | tail したログとイベントを表示 |
| F | playit 連携 | ▢ | playit エージェントの稼働確認・公開アドレス表示 |
| G | スケジューラ | ▢ | 定時再起動・更新前自動バックアップ・クラッシュ時自動再起動 |

✅ = MVP 必須 / ▢ = 将来

---

## 6. 詳細仕様

### 6.1 インストール / 更新（A）

実行コマンド:

```
<steamcmd> +force_install_dir "<server_dir>" +login anonymous +app_update 896660 validate +quit
```

- 実行前に必ずサーバーを停止する。`auto_backup_before_update = true` なら先に backup() を呼ぶ。
- steamcmd の標準出力を tail し、進捗（`Update state ...`）・成功（`Success! App '896660' ...`）・失敗を判定して UI に出す。
- `.bat`（start_headless_server.bat）は**使わない**。更新で初期化される問題があるため、起動は exe を直接叩く（§6.2）。これにより更新がパラメータを破壊しない。

### 6.2 起動（B-start）

`valheim_server.exe` を直接起動する。**working directory は必ず `<server_dir>`**（依存 DLL 解決のため）。

起動引数（config から組み立て）:

```
valheim_server.exe ^
  -nographics -batchmode ^
  -name "<name>" ^
  -port <port> ^
  -world "<world>" ^
  -password "<password>" ^
  -public <public> ^
  -savedir "<save_dir>" ^
  -saveinterval <save_interval> ^
  -backups <backups> ^
  -logFile "<log_file>"
```

- `-crossplay` は**付けない**（Steam バックエンド固定）。
- ポートはゲーム用 `<port>` とクエリ用 `<port>+1` を消費（既定 2456 / 2457、いずれも UDP）。playit 運用では直接接続を使うためクエリ側（ブラウザ掲載）は不問。
- プロセス生成フラグ（§6.4 と整合）:
  - `CREATE_NEW_CONSOLE`（子に独立コンソールを与える。Ctrl+C 送出に必要）
  - ウィンドウは `STARTUPINFO.wShowWindow = SW_HIDE` で隠す
- 起動後、ログに接続受付開始を示す行が出たら `ServerReady` を発火。

### 6.3 停止（B-stop）— 最重要

Valheim はクリーン終了時にワールドを書き出す。**強制 kill は直近オートセーブ以降の喪失・`.db` 破損のリスク**があるため、原則 Ctrl+C で正常終了させる。

手順:

1. `ctrlc-helper.exe <server_pid>` を起動する。ヘルパー内部（windows-sys）:
   ```
   FreeConsole();
   AttachConsole(server_pid);
   SetConsoleCtrlHandler(None, TRUE);          // 自分は Ctrl+C を無視
   GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0);  // 0 = アタッチ先コンソールの全プロセス = サーバー
   // 少し待ってから FreeConsole();
   ```
   → サーバーに**本物の Ctrl+C** が届く（Ctrl+Break を Valheim が拾うか不明な問題を回避）。
2. ヘルパーを**別 exe に隔離**する意義: 送った信号でヘルパー自身が落ちても、GUI 本体は無傷。
3. GUI 側はサーバープロセスの終了を最大 `graceful_stop_timeout_secs`（既定 30）待つ。ログに最終 `World saved` とプロセス exit を確認できれば正常終了。
4. タイムアウトした場合のみ `TerminateProcess` でフォールバックし、`ServerEvent::Warning` で「直近オートセーブ以降は失われた可能性」を通知する。

### 6.4 プロセス制御とログ取得の両立（設計上の核心）

stdout をパイプ取得する方式と、Ctrl+C をコンソール経由で送る方式は**両立しない**（パイプ前提だと子にコンソールが無く `AttachConsole` できない）。そこで:

- **サーバーは独立した隠しコンソールで起動**（Ctrl+C 送出を可能にする）。
- **ログは `-logFile "<log_file>"` でファイルに書かせ、GUI はそのファイルを tail** する。

これにより「ログ取得」と「Ctrl+C 送出」を分離できる。
※ `-logFile` が必要なイベント（接続・切断・World saved）を取りこぼす場合の代替案: コンソールのスクリーンバッファ読み取り、または `CREATE_NO_WINDOW` + パイプ取得に切り替え、停止は別手段（例: 既知の windows-kill 相当）にする。→ §9 のスパイクで実機確認する。

### 6.5 ログ解析（E）

`<log_file>` を tail（インクリメンタル読み + ファイルローテーション考慮）し、行から `ServerEvent` を生成する。拾う対象（例。実機で正規表現を確定する）:

- 接続: `Got connection SteamID <id>` → `PlayerJoined`
- 切断: `Closing socket <id>` / `Destroying abandoned ZDO` 周辺 → `PlayerLeft`
- セーブ: `World saved ( <ms>ms )` → `WorldSaved`
- 起動完了: ポートバインド / `Game server connected` 相当 → `ServerReady`

`status()` はプロセス生存 + これらのイベントから導出する。

### 6.6 パラメータ管理（C）— 検証ルール

| パラメータ | 既定 | 検証 |
|---|---|---|
| name | "MyServer" | 1 文字以上 |
| world | "Dedicated" | 英数字・空白可。ファイル名として有効 |
| password | （必須入力） | **5 文字以上**、`name` と同一禁止、`name` の部分文字列禁止 |
| port | 2456 | 1024–65534（+1 が空いていること） |
| public | 0 | playit 運用では 0 固定推奨 |
| save_interval | 900 | 秒。60 以上 |
| backups | 4 | 0 以上 |
| crossplay | false | **true 禁止**（本構成では固定） |

### 6.7 バックアップ / ロールバック（D）

- **バックアップ**: サーバー停止（推奨）または直近 `World saved` 後のアイドル時に、`worlds_local\<world>.db` と `<world>.fwl` を**ペアで** `Backups\<world>\<timestamp>\` へコピー。`.old` も任意で同梱。
  - 半端な `.db` を避けるため、稼働中バックアップを許す場合も「コピー中はファイルロックに注意」「直近セーブ完了を待つ」。
- **ロールバック**: 停止 → 対象ワールドの `.db`+`.fwl` をバックアップで**ペアごと上書き**（ファイル名は元のワールド名に一致させる）→ 起動 → 接続して日付・進行を検証。
- `.db` と `.fwl` は**常にペアで扱う**（片方だけの復元は不可）。

---

## 7. 設定スキーマ（config.toml）

`Manager\config.toml`（絶対パス固定）。

```toml
[paths]
steamcmd   = "D:\\Valheim\\SteamCMD\\steamcmd.exe"
server_dir = "D:\\Valheim\\Server"
save_dir   = "D:\\Valheim\\Data"
backup_dir = "D:\\Valheim\\Backups"
log_file   = "D:\\Valheim\\Server\\logs\\server.log"

[server]
name          = "MyServer"
world         = "Dedicated"
password      = ""        # 5 文字以上・name と異なること
port          = 2456
public        = 0
save_interval = 900
backups       = 4
crossplay     = false     # 本構成では固定で false

[manager]
graceful_stop_timeout_secs = 30
auto_backup_before_update  = true
```

---

## 8. 非機能要件

### 8.1 どこからでも起動
- 設定は**全項目を絶対パス**で保持し、カレントディレクトリに依存しない。
- exe は `Manager\` に固定配置。ショートカットを任意の場所（デスクトップ・タスクバー・スタートメニュー）に置く運用。
- Slint アプリは `#![windows_subsystem = "windows"]`（コンソール無し）。サーバー側コンソールは §6.4 のとおり子プロセスに与える。

### 8.2 更新安全性
- 起動は exe 直叩き（`.bat` 非依存）。更新でパラメータが消えない。
- `auto_backup_before_update` で更新前スナップショット。

### 8.3 クラッシュ / データ安全性
- 停止は原則 Ctrl+C（§6.3）。強制終了は最終手段かつ警告。
- バックアップ/ロールバックは常に `.db`+`.fwl` ペア。

### 8.4 並行性（Slint との橋渡し）
- 重い処理は tokio 上で実行。
- UI 更新は `slint::Weak` + `slint::invoke_from_event_loop` でメインスレッドへマーシャル。
- ログ一覧・バックアップ一覧は Slint の `VecModel` にバインドし、`subscribe()` 受信で push。

---

## 9. 実装マイルストーン

- **M0（スパイク・最優先）**: `ctrlc-helper` を作り、実機 Valheim サーバーを起動→ Ctrl+C で**クリーン終了（最終セーブ確認）**できること、`-logFile` で接続/セーブイベントが取れることを検証。最大のリスクをここで潰す。
- **M1**: `gsm-core` 骨格 + `GameServerManager` trait + `valheim` 実装（start/stop/status）+ 最小 Slint 画面（状態表示・起動/停止・ログビュー）。
- **M2**: SteamCMD インストール/更新（A）+ パラメータエディタ（C）+ config 永続化。
- **M3**: バックアップ/ロールバック UI（D）。
- **将来**: playit 連携（F）、スケジューラ（G）、2 つ目のゲーム実装 + 統合シェル、crossplay/Mod 切替。

---

## 10. スコープ外 / 将来拡張

- crossplay モードと BepInEx Mod 管理（現状は Steam バックエンド固定）。
- playit エージェントの深い統合（起動制御・公開アドレス取得）。M1〜M3 では「playit が別途常駐している前提」で十分。
- マルチサーバー / 複数ゲームの統合シェル（`gsm-core` の抽象により後付け可能な設計）。

---

## 11. 確認したい未決事項

1. アーキテクチャは「最初から `gsm-core` + trait を切る」前提で記述。既存のもう一つの GUI に既存構造があるなら、その crate 構成・trait 形に合わせて調整する余地あり。
2. steamcmd.exe は同梱配布するか、ユーザー配置（パスを設定）か。
3. ワールド名の既定値（`Dedicated` のままで良いか）。
4. ログ解析の正規表現は実機ログで確定する（M0 で採取）。
