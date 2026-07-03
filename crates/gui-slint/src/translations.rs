//! UI string table for the supported locales.
//!
//! Log-event prefixes (`[saved]`, `[+]`, `[-]`, ...) and error messages are
//! intentionally left in English: they're technical, easier to grep, and
//! we don't want translation to mask the real diagnostic.
#![allow(dead_code)]

use gsm_core::{AppConfig, Language, ServerStatus};

// Internal cmdline tokens for each world modifier, in display order. The
// ComboBox indices on the UI side map directly into these arrays.
pub const COMBAT_VALUES: &[&str] = &["default", "veryeasy", "easy", "hard", "veryhard"];
pub const DEATHPENALTY_VALUES: &[&str] = &["default", "casual", "veryeasy", "hard", "hardcore"];
pub const RESOURCES_VALUES: &[&str] = &["default", "muchless", "less", "more", "muchmore"];
pub const RAIDS_VALUES: &[&str] = &["default", "none", "muchless", "less", "more", "muchmore"];
pub const PORTALS_VALUES: &[&str] = &["default", "casual", "hard", "veryhard"];
// First slot intentionally empty: "(none)" in display, no -preset flag emitted.
pub const PRESET_VALUES: &[&str] = &[
    "",
    "default",
    "casual",
    "hard",
    "hardcore",
    "immersive",
    "hammer",
];

/// Return the index whose value matches `value`, defaulting to 0 ("default"
/// or "no preset") when unknown.
pub fn index_of_value(value: &str, values: &[&'static str]) -> i32 {
    values
        .iter()
        .position(|v| *v == value)
        .map(|i| i as i32)
        .unwrap_or(0)
}

/// Look up the cmdline token at `idx`, falling back to the first slot when
/// the index is out of range. Caller passes a static slice so we can hand
/// back `&'static str`.
pub fn value_at_index(idx: i32, values: &[&'static str]) -> &'static str {
    let i = if idx < 0 { 0 } else { idx as usize };
    values.get(i).copied().unwrap_or(values[0])
}

#[derive(Clone, Copy)]
pub struct Strings {
    // Toolbar (window title only — no more mode toggle)
    pub app_title: &'static str,

    // Sections (the ARK-style stacked layout)
    pub group_setup: &'static str,
    pub group_paths: &'static str,
    pub group_saves: &'static str,
    pub group_server: &'static str,
    pub group_manager: &'static str,
    pub group_status: &'static str,
    pub group_operation: &'static str,
    pub group_players: &'static str,
    pub group_backup: &'static str,
    pub group_log: &'static str,
    pub progress_steamcmd: &'static str,
    pub progress_factorio: &'static str,
    pub progress_server: &'static str,

    // Players & Backups buttons / placeholders
    pub btn_refresh: &'static str,
    pub btn_rollback: &'static str,
    pub btn_open_backup: &'static str,
    pub no_players: &'static str,
    pub no_backups: &'static str,

    // BackupWindow
    pub backup_window_title: &'static str,
    pub backup_sidebar_paths: &'static str,
    pub backup_sidebar_list: &'static str,
    pub backup_tab_manual: &'static str,
    pub backup_tab_pre_rollback: &'static str,
    pub backup_col_when: &'static str,
    pub backup_col_size: &'static str,
    pub btn_close: &'static str,
    pub btn_take_snapshot: &'static str,
    pub btn_delete_selected: &'static str,
    pub confirm_rollback: &'static str,
    pub confirm_delete: &'static str,
    pub btn_confirm: &'static str,
    pub btn_cancel_short: &'static str,
    /// Format string with a single `{}` placeholder for the connected count.
    /// Filled in Rust via simple replace.
    pub players_count_fmt: &'static str,
    pub backups_count_fmt: &'static str,

    // Public address (connection) section
    pub group_connection: &'static str,
    pub lbl_public_address: &'static str,
    pub public_address_hint: &'static str,
    pub btn_copy: &'static str,
    pub btn_tailscale: &'static str,
    pub copy_success: &'static str,
    pub copy_failed: &'static str,
    pub save_success: &'static str,

    // Setup section
    pub lbl_language: &'static str,

    // Server controls
    pub server_prefix: &'static str,
    pub btn_start: &'static str,
    pub btn_stop: &'static str,
    pub btn_update: &'static str,
    pub btn_save: &'static str,

    // Path labels
    pub lbl_steamcmd: &'static str,
    pub lbl_steam_user: &'static str,
    pub lbl_server_dir: &'static str,
    pub lbl_save_dir: &'static str,
    pub lbl_backup_dir: &'static str,
    pub lbl_log_file: &'static str,
    pub btn_browse: &'static str,
    pub lbl_existing_save: &'static str,
    pub btn_save_world: &'static str,

    // Server params
    pub lbl_name: &'static str,
    pub lbl_world: &'static str,
    pub lbl_password: &'static str,
    pub lbl_port: &'static str,
    pub lbl_public: &'static str,
    pub lbl_save_interval: &'static str,
    pub lbl_backups: &'static str,
    pub chk_auto_pause: &'static str,
    pub lbl_simulation_state: &'static str,
    pub simulation_running: &'static str,
    pub simulation_paused_empty: &'static str,
    pub simulation_empty_unpaused: &'static str,
    pub simulation_stopped: &'static str,
    pub lbl_dlc: &'static str,
    pub group_mods: &'static str,
    pub lbl_mod_dir: &'static str,
    pub lbl_detected_mods: &'static str,
    pub lbl_enabled_mods: &'static str,
    pub lbl_mod_portal_name: &'static str,
    pub btn_add_mod_zip: &'static str,
    pub btn_add_mod_portal: &'static str,
    pub btn_open_mod_dir: &'static str,

    // World modifiers
    pub lbl_preset: &'static str,
    pub lbl_combat: &'static str,
    pub lbl_deathpenalty: &'static str,
    pub lbl_resources: &'static str,
    pub lbl_raids: &'static str,
    pub lbl_portals: &'static str,

    // World settings window — title, entry button, descriptions
    pub world_window_title: &'static str,
    pub btn_open_world: &'static str,
    pub world_done: &'static str,
    pub world_cancel: &'static str,
    pub preset_description: &'static str,
    pub combat_description: &'static str,
    pub deathpenalty_description: &'static str,
    pub resources_description: &'static str,
    pub raids_description: &'static str,
    pub portals_description: &'static str,

    // World keys (sidebar category + per-key label + description)
    pub lbl_keys: &'static str,
    pub keys_description: &'static str,
    pub key_nobuildcost: &'static str,
    pub key_nobuildcost_desc: &'static str,
    pub key_passivemobs: &'static str,
    pub key_passivemobs_desc: &'static str,
    pub key_nomap: &'static str,
    pub key_nomap_desc: &'static str,
    pub key_noportals: &'static str,
    pub key_noportals_desc: &'static str,
    pub key_playerevents: &'static str,
    pub key_playerevents_desc: &'static str,
    pub key_showenemyhud: &'static str,
    pub key_showenemyhud_desc: &'static str,
    pub key_devcommands: &'static str,
    pub key_devcommands_desc: &'static str,

    // Manager intervals retained in config for compatibility. Factorio does
    // not consume these directly.
    pub lbl_backup_short: &'static str,
    pub lbl_backup_long: &'static str,
    pub backup_intervals_hint: &'static str,

    // Localised display labels for each ComboBox. Indices align with the
    // *_VALUES arrays above.
    pub preset_labels: &'static [&'static str],
    pub combat_labels: &'static [&'static str],
    pub deathpenalty_labels: &'static [&'static str],
    pub resources_labels: &'static [&'static str],
    pub raids_labels: &'static [&'static str],
    pub portals_labels: &'static [&'static str],

    // Manager params
    pub lbl_graceful_stop: &'static str,
    pub chk_auto_backup: &'static str,
    pub chk_stop_when_empty: &'static str,
    pub lbl_empty_stop_delay: &'static str,

    // Status names
    pub status_stopped: &'static str,
    pub status_starting: &'static str,
    pub status_running: &'static str,
    pub status_stopping: &'static str,
    pub status_crashed: &'static str,
    pub status_updating: &'static str,
    pub install_ready: &'static str,
    pub install_missing: &'static str,

    // Summary labels
    pub sum_steamcmd: &'static str,
    pub sum_server_dir: &'static str,
    pub sum_save_dir: &'static str,
    pub sum_backup_dir: &'static str,
    pub sum_log_file: &'static str,
    pub sum_name: &'static str,
    pub sum_world: &'static str,
    pub sum_port: &'static str,
    pub sum_port_note: &'static str,
    pub sum_public: &'static str,
    pub sum_save_interval: &'static str,
    pub sum_save_interval_unit: &'static str,
    pub sum_backups: &'static str,
    pub sum_password: &'static str,
    pub sum_password_unset: &'static str,
    pub sum_password_set: &'static str,
    pub sum_steam_login: &'static str,

    pub no_config_prefix: &'static str,
    pub no_config_suffix: &'static str,
}

pub const JA: Strings = Strings {
    app_title: "Factorio サーバーメンテナー",

    group_setup: "セットアップ",
    group_paths: "フォルダ",
    group_saves: "セーブデータ",
    group_server: "サーバー設定",
    group_manager: "マネージャー",
    group_status: "サーバー状態",
    group_operation: "サーバー操作",
    group_players: "接続中プレイヤー",
    group_backup: "バックアップ",
    group_log: "ログ",
    progress_steamcmd: "SteamCMD",
    progress_factorio: "Factorio本体",
    progress_server: "サーバー",

    btn_refresh: "更新",
    btn_rollback: "ロールバック",
    btn_open_backup: "バックアップ管理を開く…",
    no_players: "接続中のプレイヤーはいません",
    no_backups: "バックアップはまだありません",

    backup_window_title: "バックアップ管理 — Factorio サーバーメンテナー",
    backup_sidebar_paths: "パス",
    backup_sidebar_list: "一覧",
    backup_tab_manual: "手動",
    backup_tab_pre_rollback: "ロールバック前",
    backup_col_when: "日時",
    backup_col_size: "サイズ",
    btn_close: "閉じる",
    btn_take_snapshot: "スナップショットを作成",
    btn_delete_selected: "選択を削除",
    confirm_rollback: "このスナップショットにロールバックしますか？(現状は自動でロールバック前バックアップとして退避されます)",
    confirm_delete: "選択したスナップショットを削除しますか？",
    btn_confirm: "実行",
    btn_cancel_short: "キャンセル",
    players_count_fmt: "{} 人が接続中",
    backups_count_fmt: "{} 個のスナップショット",

    group_connection: "接続",
    lbl_public_address: "公開アドレス:",
    public_address_hint: "他のプレイヤーが接続に使用するアドレスです。playit.gg トンネル、Tailscale 名、グローバル IP など自由に入力できます。Enter で保存、「コピー」で共有用にクリップボードへ。",
    btn_copy: "コピー",
    btn_tailscale: "Tailscale名",
    copy_success: "クリップボードにコピーしました",
    copy_failed: "コピーに失敗しました",
    save_success: "保存しました",

    lbl_language: "言語:",

    server_prefix: "状態: ",
    btn_start: "開始",
    btn_stop: "停止",
    btn_update: "更新",
    btn_save: "保存",

    lbl_steamcmd: "SteamCMD 実行ファイル:",
    lbl_steam_user: "Steamユーザー名:",
    lbl_server_dir: "サーバーフォルダ:",
    lbl_save_dir: "セーブフォルダ:",
    lbl_backup_dir: "バックアップフォルダ:",
    lbl_log_file: "ログファイル:",
    btn_browse: "参照…",
    lbl_existing_save: "既存セーブ:",
    btn_save_world: "選択",

    lbl_name: "サーバー名:",
    lbl_world: "ワールド名:",
    lbl_password: "パスワード:",
    lbl_port: "ポート (UDP):",
    lbl_public: "公開 (0 / 1):",
    lbl_save_interval: "セーブ間隔 (秒):",
    lbl_backups: "バックアップ保持数:",
    chk_auto_pause: "誰もいないときワールドを一時停止",
    lbl_simulation_state: "ワールド進行:",
    simulation_running: "プレイヤー接続中: ワールド進行中",
    simulation_paused_empty: "0人: 公式 auto_pause で一時停止対象",
    simulation_empty_unpaused: "0人: ワールドは進行します",
    simulation_stopped: "サーバー停止中",
    lbl_dlc: "DLC:",
    group_mods: "Mod管理",
    lbl_mod_dir: "Modフォルダ:",
    lbl_detected_mods: "検出されたmod:",
    lbl_enabled_mods: "有効にするmod名:",
    lbl_mod_portal_name: "Mod Portal名:",
    btn_add_mod_zip: "zip追加",
    btn_add_mod_portal: "追加",
    btn_open_mod_dir: "フォルダ",

    lbl_preset: "プリセット",
    lbl_combat: "戦闘",
    lbl_deathpenalty: "死亡ペナルティ",
    lbl_resources: "資源量",
    lbl_raids: "襲撃頻度",
    lbl_portals: "ポータル",

    world_window_title: "ワールド設定 — Factorio サーバーメンテナー",
    btn_open_world: "ワールド設定を開く…",
    world_done: "適用",
    world_cancel: "キャンセル",
    preset_description: "プリセットは複数の修飾子を一括で適用します。「(なし)」を選ぶと下の個別設定が使われます。\n\n• デフォルト: 標準難易度\n• カジュアル: 易しめ\n• 難しい: 難易度上昇\n• ハードコア: 厳しい設定\n• イマーシブ: 探索/雰囲気重視\n• ハンマー: クリエイティブ系",
    combat_description: "戦闘の難易度を調整します。プレイヤーが敵に与えるダメージと敵がプレイヤーに与えるダメージを変化させます。\n\n• デフォルト: 標準\n• とても易しい / 易しい: 敵が弱く、プレイヤーが強い\n• 難しい / とても難しい: 敵が強く、プレイヤーが弱い",
    deathpenalty_description: "死亡時のペナルティを調整します。\n\n• デフォルト: 標準（スキル損失あり、装備ドロップ）\n• カジュアル: 装備保持、スキル損失なし\n• とても易しい: スキル損失軽減\n• 難しい: ペナルティ強化\n• ハードコア: 最大ペナルティ",
    resources_description: "ワールド内のリソース量を調整します。\n\n• とても少ない: 約 1/4\n• 少ない: 約 1/2\n• デフォルト: 標準\n• 多い: 約 2 倍\n• とても多い: 約 4 倍",
    raids_description: "拠点襲撃の頻度を調整します。\n\n• なし: 襲撃なし\n• とても少ない / 少ない: 標準より少ない\n• デフォルト: 標準\n• 多い / とても多い: 標準より多い",
    portals_description: "ポータルでの物品輸送制限を調整します。\n\n• カジュアル: 全素材を運べる\n• デフォルト: 標準（鉱石類など一部不可）\n• 難しい: より厳しい制限\n• とても難しい: ポータル無効",

    lbl_keys: "ワールドキー",
    keys_description: "ワールドの挙動を一括で切り替えるトグル群です。複数同時に有効化できます。サーバー起動時に -setkey <name> として渡されます。",
    key_nobuildcost: "クリエイティブ建築（建築コスト無効）",
    key_nobuildcost_desc: "建築に必要な素材を消費しなくなります。",
    key_passivemobs: "パッシブモブ",
    key_passivemobs_desc: "モンスターがプレイヤーを攻撃しなくなります。",
    key_nomap: "ミニマップ非表示",
    key_nomap_desc: "ミニマップを完全に隠します。探索ガチ勢向け。",
    key_noportals: "ポータル禁止",
    key_noportals_desc: "ポータルを建てられなくなります（portals=veryhard とは別に完全無効）。",
    key_playerevents: "プレイヤーイベント共有",
    key_playerevents_desc: "プレイヤーが起こした襲撃などのイベントが他プレイヤーにも発生します。",
    key_showenemyhud: "敵 HP バー表示",
    key_showenemyhud_desc: "敵の HP バーを常時表示します。",
    key_devcommands: "開発者コマンド許可",
    key_devcommands_desc: "コンソールに devcommands を入力可能になります（チート系）。",

    lbl_backup_short: "短期バックアップ間隔 (秒):",
    lbl_backup_long: "長期バックアップ間隔 (秒):",
    backup_intervals_hint: "Factorio では未使用の互換設定です。本ツールのスナップショットはセーブ zip をコピーします。",

    preset_labels: &[
        "(なし)", "デフォルト", "カジュアル", "難しい", "ハードコア", "イマーシブ", "ハンマー",
    ],
    combat_labels: &[
        "デフォルト", "とても易しい", "易しい", "難しい", "とても難しい",
    ],
    deathpenalty_labels: &[
        "デフォルト", "カジュアル", "とても易しい", "難しい", "ハードコア",
    ],
    resources_labels: &[
        "デフォルト", "とても少ない", "少ない", "多い", "とても多い",
    ],
    raids_labels: &[
        "デフォルト", "なし", "とても少ない", "少ない", "多い", "とても多い",
    ],
    portals_labels: &[
        "デフォルト", "カジュアル", "難しい", "とても難しい",
    ],

    lbl_graceful_stop: "正常停止タイムアウト (秒):",
    chk_auto_backup: "更新前に自動バックアップ",
    chk_stop_when_empty: "プレイヤーがいなくなったらサーバーを停止",
    lbl_empty_stop_delay: "無人停止までの待ち時間 (秒):",

    status_stopped: "停止中",
    status_starting: "起動中",
    status_running: "実行中",
    status_stopping: "停止処理中",
    status_crashed: "クラッシュ",
    status_updating: "更新中",
    install_ready: "Factorio本体: インストール済み",
    install_missing: "Factorio本体: 未インストール。まず「サーバーをインストール／更新」を実行してください。",

    sum_steamcmd: "SteamCMD:    ",
    sum_server_dir: "サーバー:    ",
    sum_save_dir: "セーブ:      ",
    sum_backup_dir: "バックアップ:",
    sum_log_file: "ログ:        ",
    sum_name: "名前:    ",
    sum_world: "ワールド: ",
    sum_port: "ポート:  ",
    sum_port_note: " (UDP, クエリは +1)",
    sum_public: "公開:    ",
    sum_save_interval: "セーブ間隔: ",
    sum_save_interval_unit: "秒",
    sum_backups: "保持数:  ",
    sum_password: "パスワード: ",
    sum_password_unset: "(未設定)",
    sum_password_set: "****",
    sum_steam_login: "SteamCMDログイン: ",

    no_config_prefix: "設定ファイルがありません (",
    no_config_suffix: ") — フィールドを埋めて「保存して再起動」を押してください。",
};

pub const EN: Strings = Strings {
    app_title: "Factorio Server Manager",

    group_setup: "Setup",
    group_paths: "Folders",
    group_saves: "Saves",
    group_server: "Server settings",
    group_manager: "Manager",
    group_status: "Server status",
    group_operation: "Server controls",
    group_players: "Connected players",
    group_backup: "Backups",
    group_log: "Log",
    progress_steamcmd: "SteamCMD",
    progress_factorio: "Factorio",
    progress_server: "Server",

    btn_refresh: "Refresh",
    btn_rollback: "Rollback",
    btn_open_backup: "Open backup management…",
    no_players: "No players connected",
    no_backups: "No backups yet",

    backup_window_title: "Backup management — Factorio Server Manager",
    backup_sidebar_paths: "Paths",
    backup_sidebar_list: "List",
    backup_tab_manual: "Manual",
    backup_tab_pre_rollback: "Pre-rollback",
    backup_col_when: "When",
    backup_col_size: "Size",
    btn_close: "Close",
    btn_take_snapshot: "Take snapshot",
    btn_delete_selected: "Delete selected",
    confirm_rollback: "Roll back to this snapshot? (Current state will be auto-saved as a pre-rollback backup first.)",
    confirm_delete: "Delete the selected snapshots?",
    btn_confirm: "Confirm",
    btn_cancel_short: "Cancel",
    players_count_fmt: "{} player(s) connected",
    backups_count_fmt: "{} snapshot(s)",

    group_connection: "Connection",
    lbl_public_address: "Public address:",
    public_address_hint: "Address other players use to connect — a playit.gg tunnel, Tailscale name, public IP, etc. Press Enter to save, Copy to put it on the clipboard.",
    btn_copy: "Copy",
    btn_tailscale: "Tailscale name",
    copy_success: "Copied to clipboard",
    copy_failed: "Copy failed",
    save_success: "Saved",

    lbl_language: "Language:",

    server_prefix: "State: ",
    btn_start: "Start",
    btn_stop: "Stop",
    btn_update: "Update",
    btn_save: "Save",

    lbl_steamcmd: "SteamCMD exe:",
    lbl_steam_user: "Steam username:",
    lbl_server_dir: "Server dir:",
    lbl_save_dir: "Save dir:",
    lbl_backup_dir: "Backup dir:",
    lbl_log_file: "Log file:",
    btn_browse: "Browse...",
    lbl_existing_save: "Existing save:",
    btn_save_world: "Select",

    lbl_name: "Name:",
    lbl_world: "World:",
    lbl_password: "Password:",
    lbl_port: "Port (UDP):",
    lbl_public: "Public (0 or 1):",
    lbl_save_interval: "Save interval (sec):",
    lbl_backups: "Backups kept:",
    chk_auto_pause: "Pause world when nobody is connected",
    lbl_simulation_state: "World simulation:",
    simulation_running: "Players connected: world is running",
    simulation_paused_empty: "0 players: official auto_pause should pause the world",
    simulation_empty_unpaused: "0 players: world keeps running",
    simulation_stopped: "Server stopped",
    lbl_dlc: "DLC:",
    group_mods: "Mod management",
    lbl_mod_dir: "Mod dir:",
    lbl_detected_mods: "Detected mods:",
    lbl_enabled_mods: "Enabled mod names:",
    lbl_mod_portal_name: "Mod Portal name:",
    btn_add_mod_zip: "Add zip",
    btn_add_mod_portal: "Add",
    btn_open_mod_dir: "Folder",

    lbl_preset: "Preset",
    lbl_combat: "Combat",
    lbl_deathpenalty: "Death penalty",
    lbl_resources: "Resources",
    lbl_raids: "Raids",
    lbl_portals: "Portals",

    world_window_title: "World settings — Factorio Server Manager",
    btn_open_world: "Open world settings…",
    world_done: "Apply",
    world_cancel: "Cancel",
    preset_description: "Presets apply multiple modifiers at once. \"(none)\" means use the individual modifiers below.\n\n• Default: standard difficulty\n• Casual: easier overall\n• Hard: harder overall\n• Hardcore: severe settings\n• Immersive: focused on exploration/atmosphere\n• Hammer: creative-style",
    combat_description: "Adjusts combat difficulty. Affects damage dealt to and from enemies.\n\n• Default: standard\n• Very easy / Easy: weaker enemies, stronger player\n• Hard / Very hard: stronger enemies, weaker player",
    deathpenalty_description: "Adjusts the penalty when you die.\n\n• Default: standard (skill loss, equipment drops)\n• Casual: keep equipment, no skill loss\n• Very easy: reduced skill loss\n• Hard: stronger penalties\n• Hardcore: maximum penalty",
    resources_description: "Adjusts resource amounts in the world.\n\n• Much less: ~1/4\n• Less: ~1/2\n• Default: standard\n• More: ~2x\n• Much more: ~4x",
    raids_description: "Adjusts how often base raids occur.\n\n• None: no raids\n• Much less / Less: less than standard\n• Default: standard\n• More / Much more: more than standard",
    portals_description: "Adjusts portal item-transport restrictions.\n\n• Casual: any material through portals\n• Default: standard (ores etc. blocked)\n• Hard: stricter restrictions\n• Very hard: portals disabled",

    lbl_keys: "World keys",
    keys_description: "Boolean toggles that change world behaviour. Multiple may be enabled at once. Passed to the server as -setkey <name>.",
    key_nobuildcost: "Creative build (no material cost)",
    key_nobuildcost_desc: "Buildings no longer consume materials.",
    key_passivemobs: "Passive mobs",
    key_passivemobs_desc: "Monsters no longer attack players.",
    key_nomap: "Hide minimap",
    key_nomap_desc: "Hides the minimap completely (for exploration-focused play).",
    key_noportals: "No portals",
    key_noportals_desc: "Portals cannot be built (separate from portals=veryhard which also disables them).",
    key_playerevents: "Shared player events",
    key_playerevents_desc: "Raid events triggered by one player affect others too.",
    key_showenemyhud: "Show enemy HP bars",
    key_showenemyhud_desc: "Always display enemy HP bars.",
    key_devcommands: "Allow dev commands",
    key_devcommands_desc: "Lets users type devcommands in the console (cheats).",

    lbl_backup_short: "Backup short interval (sec):",
    lbl_backup_long: "Backup long interval (sec):",
    backup_intervals_hint: "Compatibility setting not used by Factorio. This tool's snapshots copy the save zip.",

    preset_labels: &[
        "(none)", "Default", "Casual", "Hard", "Hardcore", "Immersive", "Hammer",
    ],
    combat_labels: &[
        "Default", "Very easy", "Easy", "Hard", "Very hard",
    ],
    deathpenalty_labels: &[
        "Default", "Casual", "Very easy", "Hard", "Hardcore",
    ],
    resources_labels: &[
        "Default", "Much less", "Less", "More", "Much more",
    ],
    raids_labels: &[
        "Default", "None", "Much less", "Less", "More", "Much more",
    ],
    portals_labels: &[
        "Default", "Casual", "Hard", "Very hard",
    ],

    lbl_graceful_stop: "Graceful stop timeout (sec):",
    chk_auto_backup: "Auto-backup before update",
    chk_stop_when_empty: "Stop server when no players remain",
    lbl_empty_stop_delay: "Empty stop delay (sec):",

    status_stopped: "Stopped",
    status_starting: "Starting",
    status_running: "Running",
    status_stopping: "Stopping",
    status_crashed: "Crashed",
    status_updating: "Updating",
    install_ready: "Factorio: installed",
    install_missing: "Factorio: not installed. Run Install / Update server first.",

    sum_steamcmd: "SteamCMD:    ",
    sum_server_dir: "Server dir:  ",
    sum_save_dir: "Save dir:    ",
    sum_backup_dir: "Backup dir:  ",
    sum_log_file: "Log file:    ",
    sum_name: "Name:    ",
    sum_world: "World:   ",
    sum_port: "Port:    ",
    sum_port_note: " (UDP, +1 for query)",
    sum_public: "Public:  ",
    sum_save_interval: "Save interval: ",
    sum_save_interval_unit: "s",
    sum_backups: "Backups kept: ",
    sum_password: "Password: ",
    sum_password_unset: "(unset)",
    sum_password_set: "****",
    sum_steam_login: "SteamCMD login: ",

    no_config_prefix: "No config at ",
    no_config_suffix: " — fill in the fields and press Save & restart.",
};

pub fn for_language(lang: Language) -> &'static Strings {
    match lang {
        Language::Ja => &JA,
        Language::En => &EN,
    }
}

pub fn status_label(s: ServerStatus, t: &Strings) -> &'static str {
    match s {
        ServerStatus::Stopped => t.status_stopped,
        ServerStatus::Starting => t.status_starting,
        ServerStatus::Running => t.status_running,
        ServerStatus::Stopping => t.status_stopping,
        ServerStatus::Crashed => t.status_crashed,
        ServerStatus::Updating => t.status_updating,
    }
}

pub fn render_paths_summary(cfg: &AppConfig, t: &Strings) -> String {
    format!(
        "{}{}\n{}{}\n{}{}\n{}{}\n{}{}",
        t.sum_steamcmd,
        cfg.paths.steamcmd.display(),
        t.sum_server_dir,
        cfg.paths.server_dir.display(),
        t.sum_save_dir,
        cfg.paths.save_dir.display(),
        t.sum_backup_dir,
        cfg.paths.backup_dir.display(),
        t.sum_log_file,
        cfg.paths.log_file.display(),
    )
}

pub fn render_params_summary(cfg: &AppConfig, t: &Strings) -> String {
    let pw = if cfg.server.password.is_empty() {
        t.sum_password_unset
    } else {
        t.sum_password_set
    };
    let steam_login = if cfg.manager.steam_username.trim().is_empty() {
        "anonymous"
    } else {
        cfg.manager.steam_username.trim()
    };
    format!(
        "{}{}\n{}{}\n{}{}{}\n{}{}\n{}{}{}\n{}{}\n{}{}\n{}{}",
        t.sum_name,
        cfg.server.name,
        t.sum_world,
        cfg.server.world,
        t.sum_port,
        cfg.server.port,
        t.sum_port_note,
        t.sum_public,
        cfg.server.public,
        t.sum_save_interval,
        cfg.server.save_interval,
        t.sum_save_interval_unit,
        t.sum_backups,
        cfg.server.backups,
        t.sum_password,
        pw,
        t.sum_steam_login,
        steam_login,
    )
}

pub fn language_index(lang: Language) -> i32 {
    match lang {
        Language::Ja => 0,
        Language::En => 1,
    }
}

pub fn language_from_index(idx: i32) -> Language {
    match idx {
        1 => Language::En,
        _ => Language::Ja,
    }
}

/// Substitute a single `{}` placeholder in the template. Used for the
/// localized "N players connected" / "M snapshots" summary lines.
pub fn fmt_count(template: &str, count: usize) -> String {
    template.replacen("{}", &count.to_string(), 1)
}
