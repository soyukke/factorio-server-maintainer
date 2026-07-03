# Factorio Server Maintainer

Windows で Factorio 専用サーバーを 1 台管理するための GUI ツールです。

## 主な機能

- SteamCMD で Factorio をインストール / 更新 (`app_update 427520`)
- `bin\x64\factorio.exe` を `--start-server` 付きで起動
- セーブ zip が無ければ `<save_dir>\<world>.zip` を自動作成
- GUI で既存セーブを選択、または新しいワールド名へ切り替え
- `ctrlc-helper.exe` でまず正常停止し、タイムアウト後だけ強制終了
- セーブ zip をコピーしてスナップショット作成、削除、ロールバック
- Base / Space Age を `mods\mod-list.json` で明示管理
- 公式 `auto_pause` で、誰もいないときのワールド進行を停止
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

## セーブデータの切り替え

GUI の「セーブデータ」欄で、保存フォルダ内の `*.zip` を一覧から選べます。

既存セーブで遊びたい場合:

1. サーバーを停止する
2. 「既存セーブ」から遊びたいセーブを選ぶ
3. 「このセーブで保存」を押す
4. アプリが再起動したら「サーバー起動」を押す

新しいワールドで始めたい場合:

1. サーバーを停止する
2. 「ワールド名」に新しい名前を入力する
3. 「このセーブで保存」を押す
4. アプリが再起動したら「サーバー起動」を押す

新しいワールド名の zip がまだ無い場合、起動時に `<save_dir>\<world>.zip` を自動作成します。古いセーブ zip は削除しないので、後から一覧で選び直せます。

バックアップから戻したい場合は「バックアップ管理を開く…」から対象のスナップショットを選んでロールバックします。ロールバック前の現在状態は自動で退避されます。

## 無人時のワールド進行

Factorio には公式の `auto_pause` 設定があります。このツールでは既定で ON です。

```json
"auto_pause": true
```

ON の場合、プレイヤーが 0 人になるとサーバープロセスは起動したまま、ワールド時間は一時停止対象になります。工場、汚染、敵襲、資源消費を無人で進めたくない場合は ON のまま使ってください。

この設定はサーバー起動時に `server-settings.json` として渡すため、変更の反映にはサーバー再起動が必要です。GUI の「ワールド進行」欄で、現在プレイヤーがいて進行中か、0 人で一時停止対象かを確認できます。

さらに管理ツール側の「プレイヤーがいなくなったらサーバーを停止」を ON にすると、最後のプレイヤーが抜けてから既定 300 秒後に正常停止します。正常停止では Factorio に保存させてから終了し、その後にバックアップスナップショットを作成します。途中で誰かが戻ってきた場合は停止しません。

Factorio の autosave は管理対象サーバーの `Server\UserData\saves\_autosave*.zip` に作られます。管理ツールはサーバー起動中、この autosave が更新されたらバックアップフォルダへ自動コピーします。つまり `save_interval` は Factorio の autosave 間隔であり、その autosave をバックアップとして集約する形です。

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

## Mod 管理

GUI の「Mod」欄で、Mod Portal から mod を追加できます。

```text
%USERPROFILE%\.factorio-server-maintainer\Server\mods
```

「Mod Portal名」に mod 名を入れて「Mod Portalから追加」を押すと、最新リリースをダウンロードしてこのフォルダへコピーします。ダウンロードには Factorio の `player-data.json` にある `service-username` / `service-token` を使います。トークンはこのツールの `config.toml` には保存しません。

zip を手元に持っている場合は「mod zipを追加」から選べます。Factorio の mod zip は通常 `<mod-name>_<version>.zip` なので、コピー後に `<mod-name>` を検出し、「有効にするmod名」に自動追加します。

有効化したい mod は「有効にするmod名」に 1 行ずつ書いて保存します。保存後、次回サーバー起動時に `mod-list.json` へ反映されます。自動追加された名前を消せば、その mod は無効扱いになります。

```text
personal-respawn-anchor
```

Gameplay mod はサーバーだけでは完結しません。参加者のクライアントにも同じ mod セットが必要です。Factorio は接続時に mod 同期を促しますが、最終的には参加者側にも mod が入ります。

### Personal Respawn Anchor

`personal-respawn-anchor` は、マルチ向けの小さな自作 mod です。管理ツールには同梱せず、Mod Portal から取得します。

`respawn-beacon` は Factorio の `force`、つまり通常の協力マルチではチーム全体のスポーン地点を書き換える挙動でした。そのため、誰かがアンカーを置くと全員の復活地点が変わります。

`personal-respawn-anchor` はその代わりに、プレイヤーごと・惑星ごとにアンカー位置を保存します。ゲーム本来のチームスポーン地点は変更せず、死亡後に復活した本人だけを自分のアンカーへ移動します。

- 友達が置いたアンカーは友達用
- 自分が置いたアンカーは自分用
- 惑星ごとに別々のアンカーを持てる
- アンカーを回収すると、そのプレイヤーのその惑星の登録だけ消える
- マップには `<プレイヤー名> spawn` というタグを追加する

GUI の「Mod Portal名」に次を入れて追加します。

```text
personal-respawn-anchor
```

サーバーに反映するには、GUI の「有効にするmod名」に `personal-respawn-anchor` を入れて保存し、サーバーを再起動します。古い `respawn-beacon` と同時に有効にすると挙動が混ざるため、どちらか片方だけを使ってください。

参加者は Factorio の接続時に mod 同期を促されます。

`respawn-beacon` への敬意と独立実装であることは、mod側のクレジットに明記します。コード、画像、thumbnail はコピーしていません。

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
