// Hide the console window on Windows release builds  Espec §8.1.
// In debug builds we keep the console so `tracing` output is visible.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod translations;

use anyhow::Context;
use chrono::TimeZone;
use factorio::{parse_network_status_line, FactorioServer};
use gsm_core::{
    AppConfig, Backup, BackupId, BackupKind, FactorioDlc, GameServerManager, Language,
    ManagerConfig, PathsConfig, ServerConfig, ServerEvent, ServerStatus,
};
use slint::ComponentHandle;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::EnvFilter;
use translations::Strings;

slint::include_modules!();

/// Mutable runtime state shared by callbacks.
struct UiState {
    config: Option<AppConfig>,
    language: Language,
    /// Last status observed; used to re-translate the status label when the
    /// user toggles language outside of a status change.
    last_status: ServerStatus,
    /// Currently-connected players, keyed by player name. The value is when we
    /// observed the PlayerJoined event.
    players: HashMap<String, chrono::DateTime<chrono::Local>>,
    /// Prevent auto-stop from acting on an incomplete roster after GUI restart.
    player_roster_observed: bool,
    /// Recent Factorio peer/network diagnostics shown in the players tab.
    network_lines: Vec<String>,
    /// Recent join/leave activity, newest last.
    activity: Vec<ActivityEntry>,
    last_world_saved: Option<chrono::DateTime<chrono::Local>>,
    last_log_at: Option<chrono::DateTime<chrono::Local>>,
    /// Last refresh from `list_backups`. Cached so we can rebuild row models
    /// after toggling sort / selection without going back to disk.
    last_backups: Vec<Backup>,
    /// BackupId.0 of currently-selected rows (any list).
    selected_backup_ids: HashSet<String>,
    /// Sort column for the backup list: 0 = when, 1 = size.
    backup_sort_column: u8,
    backup_sort_desc: bool,
}

#[derive(Clone)]
struct ActivityEntry {
    at: chrono::DateTime<chrono::Local>,
    player_name: String,
    kind: ActivityKind,
}

#[derive(Clone, Copy)]
enum ActivityKind {
    Join,
    Leave,
}

struct PlayerRenderContext<'a> {
    status: ServerStatus,
    auto_pause: bool,
    last_world_saved: Option<chrono::DateTime<chrono::Local>>,
    last_log_at: Option<chrono::DateTime<chrono::Local>>,
    last_activity: Option<&'a ActivityEntry>,
    language: Language,
    strings: &'a Strings,
}

#[allow(clippy::too_many_lines)]
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let manager_dir = current_manager_dir()?;
    let config_path = manager_dir.join("config.toml");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    let _guard = rt.enter();

    let ui = MainWindow::new()?;
    let backup_window = BackupWindow::new()?;

    let initial_config = match AppConfig::load(&config_path) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::info!(error = %e, path = %config_path.display(), "no usable config; starting in setup mode");
            None
        }
    };

    let initial_language = initial_config
        .as_ref()
        .map(|c| c.manager.language)
        .unwrap_or(Language::Ja);

    let state = Arc::new(Mutex::new(UiState {
        config: initial_config.clone(),
        language: initial_language,
        last_status: ServerStatus::Stopped,
        players: HashMap::new(),
        player_roster_observed: false,
        network_lines: Vec::new(),
        activity: Vec::new(),
        last_world_saved: None,
        last_log_at: None,
        last_backups: Vec::new(),
        selected_backup_ids: HashSet::new(),
        backup_sort_column: 0,
        backup_sort_desc: true,
    }));

    // Localised strings + language combo selection first, so the first frame
    // is already in the user's language.
    apply_strings(&ui, translations::for_language(initial_language));
    apply_backup_window_strings(&backup_window, translations::for_language(initial_language));
    ui.set_language_index(translations::language_index(initial_language));

    populate_settings_fields(
        &ui,
        initial_config
            .as_ref()
            .unwrap_or(&default_config_for_setup()),
    );
    refresh_save_worlds(&ui);

    let server: Option<Arc<FactorioServer>> = match initial_config.as_ref() {
        Some(cfg) => {
            let t = translations::for_language(initial_language);
            ui.set_status_text(t.status_stopped.into());
            ui.set_paths_summary(translations::render_paths_summary(cfg, t).into());
            ui.set_params_summary(translations::render_params_summary(cfg, t).into());
            update_install_state(&ui, cfg, t);
            ui.set_simulation_state_text(t.simulation_stopped.into());
            ui.set_server_controls_enabled(true);
            ui.set_public_address(cfg.manager.public_address.clone().into());
            // Populate BackupWindow's context strip (world / paths display).
            backup_window.set_world_name(cfg.server.world.clone().into());
            backup_window.set_save_dir_display(cfg.paths.save_dir.display().to_string().into());
            backup_window.set_backup_dir_display(cfg.paths.backup_dir.display().to_string().into());
            backup_window.set_server_controls_enabled(true);
            Some(Arc::new(FactorioServer::new(
                cfg.clone(),
                manager_dir.clone(),
            )))
        }
        None => {
            let t = translations::for_language(initial_language);
            ui.set_status_text(
                format!(
                    "{}{}{}",
                    t.no_config_prefix,
                    config_path.display(),
                    t.no_config_suffix
                )
                .into(),
            );
            ui.set_server_controls_enabled(false);
            ui.set_simulation_state_text(t.simulation_stopped.into());
            None
        }
    };

    if let Some(server) = server.as_ref().cloned() {
        wire_server_callbacks(&ui, server.clone());
        spawn_event_forwarder(&ui, server.clone(), state.clone());
        wire_backup_callbacks(&ui, &backup_window, server.clone(), state.clone());

        // Initial backup list population.
        {
            let server = server.clone();
            let state = state.clone();
            let main_weak = ui.as_weak();
            let backup_weak = backup_window.as_weak();
            tokio::spawn(async move {
                refresh_backups_async(&server, &state, &main_weak, &backup_weak).await;
            });
        }

        // Adopt any Factorio server left running by a previous GUI session.
        // Done in a background task so the window appears immediately; the
        // event forwarder picks up the Running status flip when it resolves.
        let server_for_reattach = server.clone();
        let state_for_reattach = state.clone();
        let weak_for_reattach = ui.as_weak();
        tokio::spawn(async move {
            match server_for_reattach.try_reattach().await {
                Ok(true) => {
                    tracing::info!("re-attached to running server");
                    restore_player_roster_from_log_async(
                        server_for_reattach.config().paths.log_file.clone(),
                        server_for_reattach.factorio_current_log_path(),
                        state_for_reattach,
                        weak_for_reattach,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "re-attach failed"),
            }
        });
    } else {
        ui.on_start_clicked(|| tracing::warn!("start ignored: no config loaded"));
        ui.on_stop_clicked(|| tracing::warn!("stop ignored: no config loaded"));
        ui.on_update_clicked(|| tracing::warn!("update ignored: no config loaded"));
    }

    wire_browse_and_save(&ui, server.clone(), state.clone(), manager_dir.clone());
    wire_save_world_callbacks(&ui);
    wire_mod_callbacks(&ui);
    wire_language_callback(
        &ui,
        &backup_window,
        server.clone(),
        state.clone(),
        manager_dir.clone(),
    );
    wire_connection_callbacks(&ui, state.clone(), manager_dir.clone());

    if std::env::var_os("FACTORIO_MANAGER_README_SCREENSHOT").is_some() {
        apply_readme_screenshot_demo(&ui);
    }

    let ui_result = ui.run();
    // A blocking process-exit watcher may legitimately live for days while
    // the dedicated server keeps running. Do not wait for it when the GUI is
    // closed; dropping the process handle does not terminate Factorio, and a
    // later GUI instance re-attaches through factorio-state.toml.
    drop(_guard);
    rt.shutdown_background();
    ui_result?;
    Ok(())
}

fn current_manager_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    Ok(exe
        .parent()
        .context("current exe has no parent")?
        .to_path_buf())
}

fn default_config_for_setup() -> AppConfig {
    let root = default_factorio_root();
    AppConfig {
        paths: PathsConfig {
            steamcmd: root.join("SteamCMD").join("steamcmd.exe"),
            server_dir: root.join("Server"),
            save_dir: root.join("Saves"),
            backup_dir: default_backup_root().join("factorio"),
            log_file: root.join("Server").join("logs").join("server.log"),
        },
        server: ServerConfig::default(),
        manager: ManagerConfig {
            steam_username: detect_steam_account_name().unwrap_or_default(),
            ..ManagerConfig::default()
        },
    }
}

fn steam_account_name_for_display(cfg: &AppConfig) -> String {
    let configured = cfg.manager.steam_username.trim();
    if configured.is_empty() {
        detect_steam_account_name().unwrap_or_default()
    } else {
        configured.to_string()
    }
}

fn detect_steam_account_name() -> Option<String> {
    steam_loginuser_paths().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        parse_steam_loginusers_account(&text)
    })
}

fn steam_loginuser_paths() -> impl Iterator<Item = PathBuf> {
    ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .map(|root| root.join("Steam").join("config").join("loginusers.vdf"))
        .filter(|path| path.is_file())
}

fn parse_steam_loginusers_account(text: &str) -> Option<String> {
    let mut first_account = None;
    let mut current_account = None;

    for line in text.lines() {
        if let Some(account) = steam_vdf_value(line, "AccountName") {
            first_account.get_or_insert_with(|| account.clone());
            current_account = Some(account);
        } else if steam_vdf_value(line, "MostRecent").as_deref() == Some("1") {
            if let Some(account) = current_account {
                return Some(account);
            }
        } else if line.trim() == "}" {
            current_account = None;
        }
    }

    first_account
}

fn steam_vdf_value(line: &str, key: &str) -> Option<String> {
    let mut quoted = line.split('"');
    let _ = quoted.next()?;
    let found_key = quoted.next()?;
    let _ = quoted.next()?;
    let value = quoted.next()?;
    (found_key == key).then(|| value.to_string())
}

fn default_factorio_root() -> PathBuf {
    home_dir().join(".factorio-server-maintainer")
}

fn default_backup_root() -> PathBuf {
    home_dir().join(".game-server-backups")
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}

fn apply_strings(ui: &MainWindow, t: &Strings) {
    ui.set_t_app_title(t.app_title.into());

    ui.set_t_group_setup(t.group_setup.into());
    ui.set_t_group_paths(t.group_paths.into());
    ui.set_t_group_saves(t.group_saves.into());
    ui.set_t_group_server(t.group_server.into());
    ui.set_t_group_manager(t.group_manager.into());
    ui.set_t_group_status(t.group_status.into());
    ui.set_t_group_operation(t.group_operation.into());
    ui.set_t_group_log(t.group_log.into());
    ui.set_t_progress_steamcmd(t.progress_steamcmd.into());
    ui.set_t_progress_factorio(t.progress_factorio.into());
    ui.set_t_progress_server(t.progress_server.into());

    ui.set_t_lbl_language(t.lbl_language.into());

    ui.set_t_server_prefix(t.server_prefix.into());
    ui.set_t_btn_start(t.btn_start.into());
    ui.set_t_btn_stop(t.btn_stop.into());
    ui.set_t_btn_update(t.btn_update.into());
    ui.set_t_btn_save(t.btn_save.into());

    ui.set_t_lbl_steamcmd(t.lbl_steamcmd.into());
    ui.set_t_lbl_steam_user(t.lbl_steam_user.into());
    ui.set_t_lbl_server_dir(t.lbl_server_dir.into());
    ui.set_t_lbl_save_dir(t.lbl_save_dir.into());
    ui.set_t_lbl_backup_dir(t.lbl_backup_dir.into());
    ui.set_t_lbl_log_file(t.lbl_log_file.into());
    ui.set_t_btn_browse(t.btn_browse.into());
    ui.set_t_lbl_existing_save(t.lbl_existing_save.into());
    ui.set_t_btn_save_world(t.btn_save_world.into());

    ui.set_t_lbl_name(t.lbl_name.into());
    ui.set_t_lbl_world(t.lbl_world.into());
    ui.set_t_lbl_password(t.lbl_password.into());
    ui.set_t_lbl_port(t.lbl_port.into());
    ui.set_t_lbl_public(t.lbl_public.into());
    ui.set_t_lbl_save_interval(t.lbl_save_interval.into());
    ui.set_t_lbl_backups(t.lbl_backups.into());
    ui.set_t_chk_auto_pause(t.chk_auto_pause.into());
    ui.set_t_lbl_simulation_state(t.lbl_simulation_state.into());
    ui.set_t_lbl_dlc(t.lbl_dlc.into());
    ui.set_t_group_mods(t.group_mods.into());
    ui.set_t_lbl_mod_dir(t.lbl_mod_dir.into());
    ui.set_t_lbl_detected_mods(t.lbl_detected_mods.into());
    ui.set_t_lbl_enabled_mods(t.lbl_enabled_mods.into());
    ui.set_t_lbl_mod_portal_name(t.lbl_mod_portal_name.into());
    ui.set_t_btn_add_mod_zip(t.btn_add_mod_zip.into());
    ui.set_t_btn_add_mod_portal(t.btn_add_mod_portal.into());
    ui.set_t_btn_open_mod_dir(t.btn_open_mod_dir.into());

    ui.set_t_lbl_graceful_stop(t.lbl_graceful_stop.into());
    ui.set_t_chk_auto_backup(t.chk_auto_backup.into());
    ui.set_t_chk_stop_when_empty(t.chk_stop_when_empty.into());
    ui.set_t_lbl_empty_stop_delay(t.lbl_empty_stop_delay.into());

    ui.set_t_group_players(t.group_players.into());
    ui.set_t_group_activity(t.group_activity.into());
    ui.set_t_group_network(t.group_network.into());
    ui.set_t_group_backup(t.group_backup.into());
    ui.set_t_btn_refresh(t.btn_refresh.into());
    ui.set_t_btn_open_backup(t.btn_open_backup.into());
    ui.set_t_no_players(t.no_players.into());
    ui.set_t_no_backups(t.no_backups.into());

    ui.set_t_group_connection(t.group_connection.into());
    ui.set_t_lbl_public_address(t.lbl_public_address.into());
    ui.set_t_public_address_hint(t.public_address_hint.into());
    ui.set_t_btn_copy(t.btn_copy.into());
    ui.set_t_btn_tailscale(t.btn_tailscale.into());
}

/// Push every translatable label on the BackupWindow.
fn apply_backup_window_strings(bw: &BackupWindow, t: &Strings) {
    bw.set_t_backup_title(t.backup_window_title.into());
    bw.set_t_sidebar_paths(t.backup_sidebar_paths.into());
    bw.set_t_sidebar_list(t.backup_sidebar_list.into());
    bw.set_t_tab_auto(t.backup_tab_auto.into());
    bw.set_t_tab_manual(t.backup_tab_manual.into());
    bw.set_t_tab_pre_rollback(t.backup_tab_pre_rollback.into());
    bw.set_t_col_when(t.backup_col_when.into());
    bw.set_t_col_size(t.backup_col_size.into());
    bw.set_t_btn_close(t.btn_close.into());
    bw.set_t_btn_refresh(t.btn_refresh.into());
    bw.set_t_btn_take_snapshot(t.btn_take_snapshot.into());
    bw.set_t_btn_rollback(t.btn_rollback.into());
    bw.set_t_btn_delete_selected(t.btn_delete_selected.into());
    bw.set_t_confirm_rollback(t.confirm_rollback.into());
    bw.set_t_confirm_delete(t.confirm_delete.into());
    bw.set_t_btn_confirm(t.btn_confirm.into());
    bw.set_t_btn_cancel(t.btn_cancel_short.into());
    bw.set_t_no_backups(t.no_backups.into());
    bw.set_t_lbl_world(t.lbl_world.into());
    bw.set_t_lbl_save_dir(t.lbl_save_dir.into());
    bw.set_t_lbl_backup_dir(t.lbl_backup_dir.into());
}

fn populate_settings_fields(ui: &MainWindow, cfg: &AppConfig) {
    ui.set_steamcmd_path(cfg.paths.steamcmd.display().to_string().into());
    ui.set_server_dir(cfg.paths.server_dir.display().to_string().into());
    ui.set_save_dir(cfg.paths.save_dir.display().to_string().into());
    ui.set_backup_dir(cfg.paths.backup_dir.display().to_string().into());
    ui.set_log_file(cfg.paths.log_file.display().to_string().into());
    ui.set_steam_username(steam_account_name_for_display(cfg).into());

    ui.set_server_name(cfg.server.name.clone().into());
    ui.set_world_name(cfg.server.world.clone().into());
    ui.set_server_password(cfg.server.password.clone().into());
    ui.set_server_port(cfg.server.port.to_string().into());
    ui.set_server_public(cfg.server.public.to_string().into());
    ui.set_save_interval(cfg.server.save_interval.to_string().into());
    ui.set_backup_count(cfg.server.backups.to_string().into());
    ui.set_auto_pause(cfg.server.auto_pause);
    ui.set_dlc_index(dlc_index(cfg.server.dlc));
    ui.set_mod_dir(
        cfg.paths
            .server_dir
            .join("mods")
            .display()
            .to_string()
            .into(),
    );
    ui.set_enabled_mods_text(cfg.server.enabled_mods.join("\n").into());
    refresh_detected_mods(ui);

    ui.set_graceful_stop_timeout_secs(cfg.manager.graceful_stop_timeout_secs.to_string().into());
    ui.set_auto_backup_before_update(cfg.manager.auto_backup_before_update);
    ui.set_stop_when_empty(cfg.manager.stop_when_empty);
    ui.set_empty_stop_delay_secs(cfg.manager.empty_stop_delay_secs.to_string().into());

    ui.set_error_text("".into());
}

fn build_config_from_ui(ui: &MainWindow, language: Language) -> Result<AppConfig, String> {
    fn parse_u<T: std::str::FromStr>(s: &str, name: &str) -> Result<T, String>
    where
        T::Err: std::fmt::Display,
    {
        s.trim().parse::<T>().map_err(|e| format!("{name}: {e}"))
    }

    Ok(AppConfig {
        paths: PathsConfig {
            steamcmd: PathBuf::from(ui.get_steamcmd_path().as_str()),
            server_dir: PathBuf::from(ui.get_server_dir().as_str()),
            save_dir: PathBuf::from(ui.get_save_dir().as_str()),
            backup_dir: PathBuf::from(ui.get_backup_dir().as_str()),
            log_file: PathBuf::from(ui.get_log_file().as_str()),
        },
        server: ServerConfig {
            name: ui.get_server_name().to_string(),
            world: ui.get_world_name().to_string(),
            password: ui.get_server_password().to_string(),
            port: parse_u::<u16>(&ui.get_server_port(), "port")?,
            public: parse_u::<u8>(&ui.get_server_public(), "public")?,
            save_interval: parse_u::<u32>(&ui.get_save_interval(), "save_interval")?,
            backups: parse_u::<u32>(&ui.get_backup_count(), "backup_count")?,
            auto_pause: ui.get_auto_pause(),
            enabled_mods: parse_enabled_mods(&ui.get_enabled_mods_text()),
            crossplay: false,
            dlc: dlc_from_index(ui.get_dlc_index()),
        },
        manager: ManagerConfig {
            graceful_stop_timeout_secs: parse_u::<u32>(
                &ui.get_graceful_stop_timeout_secs(),
                "graceful_stop_timeout_secs",
            )?,
            auto_backup_before_update: ui.get_auto_backup_before_update(),
            stop_when_empty: ui.get_stop_when_empty(),
            empty_stop_delay_secs: parse_u::<u32>(
                &ui.get_empty_stop_delay_secs(),
                "empty_stop_delay_secs",
            )?,
            language,
            public_address: ui.get_public_address().to_string(),
            steam_username: ui.get_steam_username().trim().to_string(),
        },
    })
}

fn dlc_index(dlc: FactorioDlc) -> i32 {
    match dlc {
        FactorioDlc::Base => 0,
        FactorioDlc::SpaceAge => 1,
    }
}

fn dlc_from_index(idx: i32) -> FactorioDlc {
    match idx {
        1 => FactorioDlc::SpaceAge,
        _ => FactorioDlc::Base,
    }
}

fn ensure_directories(cfg: &AppConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(&cfg.paths.server_dir)
        .with_context(|| format!("create {}", cfg.paths.server_dir.display()))?;
    std::fs::create_dir_all(&cfg.paths.save_dir)
        .with_context(|| format!("create {}", cfg.paths.save_dir.display()))?;
    std::fs::create_dir_all(&cfg.paths.backup_dir)
        .with_context(|| format!("create {}", cfg.paths.backup_dir.display()))?;
    if let Some(parent) = cfg.paths.log_file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    Ok(())
}

fn update_install_state(ui: &MainWindow, cfg: &AppConfig, t: &Strings) {
    let steamcmd_ready = cfg.paths.steamcmd.is_file();
    let factorio_ready = cfg.paths.server_dir.join(factorio::SERVER_EXE).is_file();
    ui.set_steamcmd_ready(steamcmd_ready);
    ui.set_factorio_ready(factorio_ready);
    ui.set_install_status_text(if factorio_ready {
        t.install_ready.into()
    } else {
        t.install_missing.into()
    });
}

fn wire_server_callbacks(ui: &MainWindow, server: Arc<FactorioServer>) {
    let weak = ui.as_weak();
    {
        let server = server.clone();
        let weak = weak.clone();
        ui.on_start_clicked(move || {
            let server = server.clone();
            let weak = weak.clone();
            tokio::spawn(async move {
                if let Err(e) = server.start().await {
                    set_status_text(&weak, format!("start failed: {e:#}"));
                }
            });
        });
    }
    {
        let server = server.clone();
        let weak = weak.clone();
        ui.on_stop_clicked(move || {
            let server = server.clone();
            let weak = weak.clone();
            tokio::spawn(async move {
                if let Err(e) = server.stop(true).await {
                    set_status_text(&weak, format!("stop failed: {e:#}"));
                    return;
                }
                match server.backup_with_kind(BackupKind::Manual).await {
                    Ok(_) => set_status_text(&weak, "stopped and backed up".to_string()),
                    Err(e) => set_status_text(&weak, format!("stopped; backup failed: {e:#}")),
                }
            });
        });
    }
    {
        let server = server.clone();
        let weak = weak.clone();
        ui.on_update_clicked(move || {
            let server = server.clone();
            let weak = weak.clone();
            tokio::spawn(async move {
                match server.install_or_update().await {
                    Ok(()) => {
                        let cfg = server.config().clone();
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            let t = translations::for_language(translations::language_from_index(
                                ui.get_language_index(),
                            ));
                            update_install_state(&ui, &cfg, t);
                        });
                    }
                    Err(e) => {
                        set_status_text(&weak, format!("update failed: {e:#}"));
                    }
                }
            });
        });
    }
}

#[allow(clippy::too_many_lines)]
fn spawn_event_forwarder(ui: &MainWindow, server: Arc<FactorioServer>, state: Arc<Mutex<UiState>>) {
    let weak = ui.as_weak();
    let mut rx = server.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let (
                        status_text,
                        flags,
                        language,
                        players_update,
                        activity_update,
                        network_update,
                        empty_stop_delay,
                    ) = {
                        let mut guard = state.lock().expect("state mutex poisoned");
                        if let ServerEvent::StatusChanged(s) = &ev {
                            guard.last_status = *s;
                            // Clear the player list on transitions away
                            // from Running so we don't leave ghost rows.
                            if !matches!(*s, ServerStatus::Running) {
                                guard.players.clear();
                                guard.player_roster_observed = false;
                                guard.network_lines.clear();
                            }
                        }
                        let mut roster_changed = false;
                        let mut activity_changed = false;
                        let mut network_changed = false;
                        match &ev {
                            ServerEvent::PlayerJoined { name } => {
                                guard.players.insert(name.clone(), chrono::Local::now());
                                guard.player_roster_observed = true;
                                push_activity(&mut guard.activity, name, ActivityKind::Join);
                                roster_changed = true;
                                activity_changed = true;
                            }
                            ServerEvent::PlayerLeft { name } => {
                                guard.players.remove(name);
                                push_activity(&mut guard.activity, name, ActivityKind::Leave);
                                roster_changed = true;
                                activity_changed = true;
                            }
                            ServerEvent::WorldSaved { at } => {
                                guard.last_world_saved = Some(*at);
                                roster_changed = true;
                            }
                            ServerEvent::Log(_) => {
                                guard.last_log_at = Some(chrono::Local::now());
                                roster_changed = true;
                            }
                            ServerEvent::StatusChanged(_) => {
                                roster_changed = true;
                                network_changed = true;
                            }
                            ServerEvent::NetworkStatus { text } => {
                                guard.network_lines.push(text.clone());
                                if guard.network_lines.len() > 8 {
                                    let overflow = guard.network_lines.len() - 8;
                                    guard.network_lines.drain(..overflow);
                                }
                                network_changed = true;
                            }
                            _ => {}
                        }
                        let lang = guard.language;
                        let t = translations::for_language(lang);
                        let (st, flags) = match &ev {
                            ServerEvent::StatusChanged(s) => (
                                Some(translations::status_label(*s, t).to_string()),
                                Some(flags_for_status(*s)),
                            ),
                            _ => (None, None),
                        };
                        let players_update = if roster_changed {
                            Some(build_player_data(
                                &guard.players,
                                player_render_context(&guard, t),
                            ))
                        } else {
                            None
                        };
                        let activity_update = if activity_changed {
                            Some(build_activity_rows(&guard.activity, &guard.players, lang))
                        } else {
                            None
                        };
                        let network_update = if network_changed {
                            Some(guard.network_lines.join("\n"))
                        } else {
                            None
                        };
                        let empty_stop_delay = empty_stop_delay_after_event(&guard, &ev);
                        (
                            st,
                            flags,
                            lang,
                            players_update,
                            activity_update,
                            network_update,
                            empty_stop_delay,
                        )
                    };
                    if let Some(delay) = empty_stop_delay {
                        schedule_empty_stop(server.clone(), state.clone(), weak.clone(), delay);
                    }
                    let log_line = log_line_from(&ev, translations::for_language(language));
                    let weak = weak.clone();
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        if let Some(s) = status_text {
                            ui.set_status_text(s.into());
                        }
                        if let Some((busy, server_running)) = flags {
                            ui.set_busy(busy);
                            ui.set_server_running(server_running);
                        }
                        if let Some((rows, count_text, simulation_text, proof_text)) =
                            players_update
                        {
                            install_player_model(
                                &ui,
                                rows,
                                count_text,
                                simulation_text,
                                proof_text,
                            );
                        }
                        if let Some(rows) = activity_update {
                            install_activity_model(&ui, rows);
                        }
                        if let Some(text) = network_update {
                            ui.set_network_status_text(text.into());
                        }
                        if let Some(line) = log_line {
                            let mut buf = ui.get_log_text().to_string();
                            buf.push_str(&line);
                            buf.push('\n');
                            if buf.len() > 1_048_576 {
                                let cut = buf.len() - 786_432;
                                let cut = buf
                                    .char_indices()
                                    .find(|(i, _)| *i >= cut)
                                    .map(|(i, _)| i)
                                    .unwrap_or(cut);
                                buf = buf.split_off(cut);
                            }
                            ui.set_log_text(buf.into());
                        }
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(lagged = n, "ui event subscriber lagged");
                    let status = server.status();
                    {
                        let mut guard = state.lock().expect("state mutex poisoned");
                        guard.last_status = status;
                    }
                    let weak = weak.clone();
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        let language = translations::language_from_index(ui.get_language_index());
                        let strings = translations::for_language(language);
                        ui.set_status_text(translations::status_label(status, strings).into());
                        let (busy, server_running) = flags_for_status(status);
                        ui.set_busy(busy);
                        ui.set_server_running(server_running);
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// Materialize the player roster into Send-safe data. The caller wraps the
/// Vec in a Slint VecModel/ModelRc inside the UI thread (Rc is !Send).
fn build_player_data(
    players: &HashMap<String, chrono::DateTime<chrono::Local>>,
    ctx: PlayerRenderContext<'_>,
) -> (Vec<PlayerRow>, String, String, String) {
    let mut entries: Vec<(String, chrono::DateTime<chrono::Local>)> =
        players.iter().map(|(k, v)| (k.clone(), *v)).collect();
    entries.sort_by_key(|entry| entry.1);
    let rows: Vec<PlayerRow> = entries
        .into_iter()
        .map(|(name, at)| PlayerRow {
            player_name: name.into(),
            joined_at: at.format("%H:%M:%S").to_string().into(),
        })
        .collect();
    let count_text = translations::fmt_count(ctx.strings.players_count_fmt, players.len());
    let simulation_text =
        simulation_state_text(ctx.status, players.len(), ctx.auto_pause, ctx.strings).to_string();
    let proof_text = world_proof_text(
        ctx.status,
        players.len(),
        ctx.auto_pause,
        ctx.last_world_saved,
        ctx.last_log_at,
        ctx.last_activity,
        ctx.language,
    );
    (rows, count_text, simulation_text, proof_text)
}

fn player_render_context<'a>(state: &'a UiState, strings: &'a Strings) -> PlayerRenderContext<'a> {
    PlayerRenderContext {
        status: state.last_status,
        auto_pause: config_auto_pause(&state.config),
        last_world_saved: state.last_world_saved,
        last_log_at: state.last_log_at,
        last_activity: state.activity.last(),
        language: state.language,
        strings,
    }
}

fn install_player_model(
    ui: &MainWindow,
    rows: Vec<PlayerRow>,
    count_text: String,
    simulation_text: String,
    proof_text: String,
) {
    let model = std::rc::Rc::new(slint::VecModel::from(rows));
    ui.set_player_rows(slint::ModelRc::from(model));
    ui.set_players_count_text(count_text.into());
    ui.set_simulation_state_text(simulation_text.into());
    ui.set_world_proof_text(proof_text.into());
}

fn push_activity(activity: &mut Vec<ActivityEntry>, name: &str, kind: ActivityKind) {
    activity.push(ActivityEntry {
        at: chrono::Local::now(),
        player_name: name.to_string(),
        kind,
    });
    trim_activity(activity);
}

fn trim_activity(activity: &mut Vec<ActivityEntry>) {
    if activity.len() > 24 {
        let overflow = activity.len() - 24;
        activity.drain(..overflow);
    }
}

fn build_activity_rows(
    activity: &[ActivityEntry],
    players: &HashMap<String, chrono::DateTime<chrono::Local>>,
    language: Language,
) -> Vec<ActivityRow> {
    activity_by_player(activity, players, language)
}

fn install_activity_model(ui: &MainWindow, rows: Vec<ActivityRow>) {
    ui.set_activity_summary_text(activity_summary_from_rows(&rows).into());
    let model = std::rc::Rc::new(slint::VecModel::from(rows));
    ui.set_activity_rows(slint::ModelRc::from(model));
}

fn activity_summary_from_rows(rows: &[ActivityRow]) -> String {
    if rows.is_empty() {
        return "No join/leave history yet".to_string();
    }
    rows.iter()
        .take(3)
        .map(|row| format!("{}: {} ({})", row.player_name, row.action, row.at))
        .collect::<Vec<_>>()
        .join(" / ")
}

fn activity_by_player(
    activity: &[ActivityEntry],
    players: &HashMap<String, chrono::DateTime<chrono::Local>>,
    language: Language,
) -> Vec<ActivityRow> {
    let mut by_name: HashMap<String, PlayerActivity> = HashMap::new();
    for entry in activity {
        let row = by_name.entry(entry.player_name.clone()).or_default();
        row.latest = Some(row.latest.map_or(entry.at, |at| at.max(entry.at)));
        match entry.kind {
            ActivityKind::Join => row.last_in = Some(entry.at),
            ActivityKind::Leave => row.last_out = Some(entry.at),
        }
    }

    let mut entries: Vec<(String, PlayerActivity)> = by_name.into_iter().collect();
    entries.sort_by_key(|(_, activity)| std::cmp::Reverse(activity.latest));
    entries
        .into_iter()
        .take(6)
        .map(|(name, activity)| {
            let online = players.contains_key(&name);
            ActivityRow {
                at: player_activity_times(&activity, language).into(),
                player_name: name.into(),
                action: online_label(online, language).into(),
            }
        })
        .collect()
}

#[derive(Default)]
struct PlayerActivity {
    latest: Option<chrono::DateTime<chrono::Local>>,
    last_in: Option<chrono::DateTime<chrono::Local>>,
    last_out: Option<chrono::DateTime<chrono::Local>>,
}

fn player_activity_times(activity: &PlayerActivity, language: Language) -> String {
    let in_label = match language {
        Language::Ja => "IN",
        Language::En => "IN",
    };
    let out_label = match language {
        Language::Ja => "OUT",
        Language::En => "OUT",
    };
    let last_in = format_optional_time(activity.last_in);
    let last_out = format_optional_time(activity.last_out);
    format!("{in_label} {last_in} / {out_label} {last_out}")
}

fn format_optional_time(value: Option<chrono::DateTime<chrono::Local>>) -> String {
    value
        .map(|at| at.format("%m/%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn online_label(online: bool, language: Language) -> &'static str {
    match (online, language) {
        (true, Language::Ja) => "オンライン",
        (false, Language::Ja) => "オフライン",
        (true, Language::En) => "Online",
        (false, Language::En) => "Offline",
    }
}

fn world_proof_text(
    status: ServerStatus,
    player_count: usize,
    auto_pause: bool,
    last_world_saved: Option<chrono::DateTime<chrono::Local>>,
    last_log_at: Option<chrono::DateTime<chrono::Local>>,
    last_activity: Option<&ActivityEntry>,
    language: Language,
) -> String {
    let save = last_world_saved
        .map(|at| match language {
            Language::Ja => format!("最終保存 {}", at.format("%H:%M:%S")),
            Language::En => format!("last save {}", at.format("%H:%M:%S")),
        })
        .unwrap_or_else(|| match language {
            Language::Ja => "最終保存 不明".to_string(),
            Language::En => "last save unknown".to_string(),
        });
    let activity = last_activity
        .map(|entry| format_activity_summary(entry, language))
        .unwrap_or_else(|| match language {
            Language::Ja => "入退室履歴なし".to_string(),
            Language::En => "no join/leave history yet".to_string(),
        });
    let log = last_log_at
        .map(|at| match language {
            Language::Ja => format!("最終ログ {}", at.format("%H:%M:%S")),
            Language::En => format!("last log {}", at.format("%H:%M:%S")),
        })
        .unwrap_or_else(|| match language {
            Language::Ja => "最終ログ 不明".to_string(),
            Language::En => "last log unknown".to_string(),
        });
    match (
        language,
        status == ServerStatus::Running,
        player_count,
        auto_pause,
    ) {
        (Language::Ja, true, 0, true) => {
            format!("0人・auto_pause ON: ワールド時間は停止対象です。{activity}; {save}; {log}.")
        }
        (Language::Ja, true, 0, false) => {
            format!("0人・auto_pause OFF: ワールドは進み続けます。{activity}; {save}; {log}.")
        }
        (Language::Ja, true, count, _) => {
            format!("{count}人接続中: ワールド進行中。{activity}; {save}; {log}.")
        }
        (Language::Ja, false, _, _) => {
            format!("サーバープロセス停止中。{activity}; {save}; {log}.")
        }
        (Language::En, true, 0, true) => {
            format!("No players, auto_pause ON: world should be paused. {activity}; {save}; {log}.")
        }
        (Language::En, true, 0, false) => {
            format!(
                "No players, auto_pause OFF: world can keep running. {activity}; {save}; {log}."
            )
        }
        (Language::En, true, count, _) => {
            format!("{count} player(s) online: world is running. {activity}; {save}; {log}.")
        }
        (Language::En, false, _, _) => {
            format!("Server process is not running. {activity}; {save}; {log}.")
        }
    }
}

fn format_activity_summary(entry: &ActivityEntry, language: Language) -> String {
    let verb = match (entry.kind, language) {
        (ActivityKind::Join, Language::Ja) => "最終IN",
        (ActivityKind::Leave, Language::Ja) => "最終OUT",
        (ActivityKind::Join, Language::En) => "last in",
        (ActivityKind::Leave, Language::En) => "last out",
    };
    format!(
        "{verb} {} {}",
        entry.player_name,
        entry.at.format("%H:%M:%S")
    )
}

fn apply_readme_screenshot_demo(ui: &MainWindow) {
    let language = readme_demo_language();
    apply_strings(ui, translations::for_language(language));
    ui.set_language_index(translations::language_index(language));

    ui.set_main_tab(readme_demo_tab());
    apply_readme_demo_status(ui, language);
    apply_readme_demo_settings(ui);
    apply_readme_demo_players(ui, language);
}

fn readme_demo_language() -> Language {
    match std::env::var("FACTORIO_MANAGER_README_LANGUAGE") {
        Ok(value) if value.eq_ignore_ascii_case("en") => Language::En,
        _ => Language::Ja,
    }
}

fn readme_demo_tab() -> i32 {
    std::env::var("FACTORIO_MANAGER_README_TAB")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|tab| (0..=4).contains(tab))
        .unwrap_or(0)
}

fn apply_readme_demo_status(ui: &MainWindow, language: Language) {
    let (status, ready, simulation, count) = match language {
        Language::Ja => (
            "稼働中",
            "Factorio サーバーを起動できます",
            "プレイヤーがいるためワールドは進行中",
            "接続中: 3人",
        ),
        Language::En => (
            "Running",
            "Factorio server is ready",
            "Players connected: world is running",
            "3 player(s) connected",
        ),
    };
    ui.set_status_text(status.into());
    ui.set_install_status_text(ready.into());
    ui.set_steamcmd_ready(true);
    ui.set_factorio_ready(true);
    ui.set_server_controls_enabled(true);
    ui.set_server_running(true);
    ui.set_busy(false);
    ui.set_simulation_state_text(simulation.into());
    ui.set_backups_count_text("6 snapshot(s)".into());
    ui.set_log_text("[ready] accepting connections\n[net] friend-alpha: InGame, ping 18 ms".into());
    ui.set_error_text("".into());
    ui.set_players_count_text(count.into());
}

fn apply_readme_demo_settings(ui: &MainWindow) {
    ui.set_public_address("factory-demo.tailnet.example:34197".into());
    ui.set_public_address_status("".into());
    ui.set_params_summary("Space Age / factory-main / port 34197".into());
    ui.set_paths_summary("README screenshot demo data".into());
    ui.set_steamcmd_path(r"C:\Demo\FactorioServerMaintainer\SteamCMD\steamcmd.exe".into());
    ui.set_server_dir(r"C:\Demo\FactorioServerMaintainer\Server".into());
    ui.set_save_dir(r"C:\Demo\FactorioServerMaintainer\Saves".into());
    ui.set_backup_dir(r"C:\Demo\GameServerBackups\factorio".into());
    ui.set_log_file(r"C:\Demo\FactorioServerMaintainer\Server\logs\server.log".into());
    ui.set_steam_username("demo-steam-user".into());
    ui.set_server_name("Factory With Friends".into());
    ui.set_world_name("factory-main".into());
    ui.set_server_password("factorio".into());
    ui.set_server_port("34197".into());
    ui.set_server_public("0".into());
    ui.set_save_interval("900".into());
    ui.set_backup_count("6".into());
    ui.set_auto_pause(true);
    ui.set_dlc_index(1);
    ui.set_stop_when_empty(true);
    ui.set_empty_stop_delay_secs("300".into());
    ui.set_graceful_stop_timeout_secs("30".into());
    ui.set_auto_backup_before_update(true);
    ui.set_mod_dir(r"C:\Demo\FactorioServerMaintainer\Server\mods".into());
    ui.set_mod_portal_name("personal-respawn-anchor".into());
    ui.set_detected_mods_text("personal-respawn-anchor\nRateCalculator".into());
    ui.set_enabled_mods_text("personal-respawn-anchor\nRateCalculator".into());

    let worlds = ["factory-main", "space-age-coop", "railworld-test"]
        .into_iter()
        .map(slint::SharedString::from)
        .collect::<Vec<_>>();
    ui.set_save_worlds(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(worlds),
    )));
}

fn apply_readme_demo_players(ui: &MainWindow, language: Language) {
    ui.set_network_status_text(
        [
            "factory-admin: connected, direct, ping 8 ms",
            "friend-alpha: connected, direct, ping 18 ms",
            "friend-beta: connected, relay ok, ping 32 ms",
        ]
        .join("\n")
        .into(),
    );

    let now = chrono::Local::now();
    let rows = [
        ("factory-admin", now - chrono::Duration::minutes(42)),
        ("friend-alpha", now - chrono::Duration::minutes(35)),
        ("friend-beta", now - chrono::Duration::minutes(12)),
    ]
    .into_iter()
    .map(|(name, at)| PlayerRow {
        player_name: name.into(),
        joined_at: at.format("%H:%M:%S").to_string().into(),
    })
    .collect();
    install_player_model(
        ui,
        rows,
        match language {
            Language::Ja => "接続中: 3人",
            Language::En => "3 player(s) connected",
        }
        .to_string(),
        match language {
            Language::Ja => "プレイヤーがいるためワールドは進行中",
            Language::En => "Players connected: world is running",
        }
        .to_string(),
        match language {
            Language::Ja => "3人接続中: ワールド進行中。最終IN friend-beta 12:34:56; 最終保存 12:45:00; 最終ログ 12:45:00.",
            Language::En => "3 player(s) online: world is running. last in friend-beta 12:34:56; last save 12:45:00; last log 12:45:00.",
        }
        .to_string(),
    );
    install_activity_model(
        ui,
        vec![
            ActivityRow {
                at: "IN 07/06 12:34 / OUT -".into(),
                action: "オンライン".into(),
                player_name: "friend-beta".into(),
            },
            ActivityRow {
                at: "IN 07/06 12:12 / OUT -".into(),
                action: "オンライン".into(),
                player_name: "friend-alpha".into(),
            },
            ActivityRow {
                at: "IN 07/06 11:02 / OUT 07/06 11:58".into(),
                action: "オフライン".into(),
                player_name: "factory-admin".into(),
            },
        ],
    );
}

async fn restore_player_roster_from_log_async(
    log_file: PathBuf,
    network_log_file: PathBuf,
    state: Arc<Mutex<UiState>>,
    weak: slint::Weak<MainWindow>,
) {
    let restored = match tokio::task::spawn_blocking(move || {
        Ok::<_, anyhow::Error>((
            restore_player_roster_from_log(&log_file)?,
            restore_activity_from_log(&log_file),
            file_modified_at(&network_log_file),
            restore_network_lines_from_log(&network_log_file),
        ))
    })
    .await
    {
        Ok(Ok(restored)) => restored,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "failed to restore player roster from log");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "player roster restore task failed");
            return;
        }
    };

    let activity = restored.1;
    let log_modified_at = restored.2;
    let network_lines = restored.3;
    let (rows, count_text, simulation_text, proof_text, activity_rows, network_text) = {
        let mut guard = state.lock().expect("state mutex poisoned");
        guard.players = restored.0;
        guard.activity = activity;
        guard.last_log_at = log_modified_at;
        guard.network_lines = network_lines;
        guard.player_roster_observed = true;
        let t = translations::for_language(guard.language);
        let (rows, count_text, simulation_text, proof_text) =
            build_player_data(&guard.players, player_render_context(&guard, t));
        let activity_rows = build_activity_rows(&guard.activity, &guard.players, guard.language);
        (
            rows,
            count_text,
            simulation_text,
            proof_text,
            activity_rows,
            guard.network_lines.join("\n"),
        )
    };

    let _ = weak.upgrade_in_event_loop(move |ui| {
        install_player_model(&ui, rows, count_text, simulation_text, proof_text);
        install_activity_model(&ui, activity_rows);
        ui.set_network_status_text(network_text.into());
    });
}

fn restore_player_roster_from_log(
    log_file: &Path,
) -> anyhow::Result<HashMap<String, chrono::DateTime<chrono::Local>>> {
    let text = std::fs::read_to_string(log_file)
        .with_context(|| format!("read {}", log_file.display()))?;
    Ok(restore_player_roster_from_text(&text))
}

fn restore_player_roster_from_text(text: &str) -> HashMap<String, chrono::DateTime<chrono::Local>> {
    let mut players = HashMap::new();
    let now = chrono::Local::now();
    let recent_session = text
        .rsplit_once("Hosting game")
        .map_or(text, |(_, tail)| tail);

    for line in recent_session.lines() {
        if let Some(name) = parse_factorio_join_name(line) {
            players.insert(name.to_string(), now);
        } else if let Some(name) = parse_factorio_left_name(line) {
            players.remove(name);
        }
    }

    players
}

fn restore_activity_from_log(log_file: &Path) -> Vec<ActivityEntry> {
    let Ok(text) = std::fs::read_to_string(log_file) else {
        return Vec::new();
    };
    restore_activity_from_text(&text)
}

fn file_modified_at(path: &Path) -> Option<chrono::DateTime<chrono::Local>> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Local>::from)
}

fn restore_activity_from_text(text: &str) -> Vec<ActivityEntry> {
    let mut activity = Vec::new();
    for line in text.lines() {
        if let Some(name) = parse_factorio_join_name(line) {
            activity.push(ActivityEntry {
                at: parse_manager_log_time(line).unwrap_or_else(chrono::Local::now),
                player_name: name.to_string(),
                kind: ActivityKind::Join,
            });
        } else if let Some(name) = parse_factorio_left_name(line) {
            activity.push(ActivityEntry {
                at: parse_manager_log_time(line).unwrap_or_else(chrono::Local::now),
                player_name: name.to_string(),
                kind: ActivityKind::Leave,
            });
        }
        trim_activity(&mut activity);
    }
    activity
}

fn parse_manager_log_time(line: &str) -> Option<chrono::DateTime<chrono::Local>> {
    let stamp = line.get(..19)?;
    let naive = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").ok()?;
    chrono::Local.from_local_datetime(&naive).single()
}

fn restore_network_lines_from_log(log_file: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(log_file) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for line in text.lines() {
        if let Some(status) = parse_network_status_line(line) {
            lines.push(status);
            if lines.len() > 8 {
                lines.remove(0);
            }
        }
    }
    lines
}

fn parse_factorio_join_name(line: &str) -> Option<&str> {
    line.split_once(" joined the game")
        .map(|(name, _)| trim_factorio_player_prefix(name))
        .filter(|name| !name.is_empty())
}

fn parse_factorio_left_name(line: &str) -> Option<&str> {
    line.split_once(" left the game")
        .map(|(name, _)| trim_factorio_player_prefix(name))
        .filter(|name| !name.is_empty())
}

fn trim_factorio_player_prefix(value: &str) -> &str {
    let value = value
        .rsplit_once(": ")
        .map_or(value, |(_, name)| name)
        .trim();
    trim_factorio_console_player_marker(value)
}

fn trim_factorio_console_player_marker(value: &str) -> &str {
    ["[JOIN]", "[LEAVE]", "[CHAT]"]
        .into_iter()
        .find_map(|marker| value.rsplit_once(marker).map(|(_, name)| name.trim()))
        .unwrap_or(value)
}

fn config_auto_pause(config: &Option<AppConfig>) -> bool {
    config
        .as_ref()
        .map(|cfg| cfg.server.auto_pause)
        .unwrap_or(true)
}

fn simulation_state_text(
    status: ServerStatus,
    player_count: usize,
    auto_pause: bool,
    t: &Strings,
) -> &'static str {
    if !matches!(status, ServerStatus::Running) {
        return t.simulation_stopped;
    }
    if player_count > 0 {
        return t.simulation_running;
    }
    if auto_pause {
        t.simulation_paused_empty
    } else {
        t.simulation_empty_unpaused
    }
}

fn empty_stop_delay_after_event(state: &UiState, ev: &ServerEvent) -> Option<u32> {
    if !matches!(ev, ServerEvent::PlayerLeft { .. }) {
        return None;
    }
    let cfg = state.config.as_ref()?;
    if !cfg.manager.stop_when_empty || !state.player_roster_observed {
        return None;
    }
    if state.last_status == ServerStatus::Running && state.players.is_empty() {
        Some(cfg.manager.empty_stop_delay_secs)
    } else {
        None
    }
}

fn schedule_empty_stop(
    server: Arc<FactorioServer>,
    state: Arc<Mutex<UiState>>,
    weak: slint::Weak<MainWindow>,
    delay_secs: u32,
) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(delay_secs as u64)).await;
        if !should_stop_for_empty_server(&state) {
            return;
        }
        set_status_text(
            &weak,
            "no players online; stopping server safely...".to_string(),
        );
        if let Err(e) = server.stop(true).await {
            set_status_text(&weak, format!("empty stop failed: {e:#}"));
            return;
        }
        match server.backup_with_kind(BackupKind::Manual).await {
            Ok(_) => set_status_text(
                &weak,
                "stopped and backed up after players left".to_string(),
            ),
            Err(e) => set_status_text(&weak, format!("stopped; backup failed: {e:#}")),
        }
    });
}

fn should_stop_for_empty_server(state: &Arc<Mutex<UiState>>) -> bool {
    let guard = state.lock().expect("state mutex poisoned");
    let Some(cfg) = guard.config.as_ref() else {
        return false;
    };
    cfg.manager.stop_when_empty
        && guard.player_roster_observed
        && guard.players.is_empty()
        && guard.last_status == ServerStatus::Running
}

fn list_save_worlds(save_dir: &Path) -> Vec<slint::SharedString> {
    let mut worlds = Vec::new();
    let Ok(entries) = std::fs::read_dir(save_dir) else {
        return worlds;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_zip = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
        if !is_zip {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            worlds.push(slint::SharedString::from(stem));
        }
    }

    worlds.sort_by_key(|world| world.to_string().to_ascii_lowercase());
    worlds
}

fn refresh_save_worlds(ui: &MainWindow) {
    let save_dir = PathBuf::from(ui.get_save_dir().as_str());
    let worlds = list_save_worlds(&save_dir);
    let model = std::rc::Rc::new(slint::VecModel::from(worlds));
    ui.set_save_worlds(slint::ModelRc::from(model));
}

fn wire_save_world_callbacks(ui: &MainWindow) {
    let ui_weak = ui.as_weak();
    ui.on_refresh_saves_clicked(move || {
        if let Some(ui) = ui_weak.upgrade() {
            refresh_save_worlds(&ui);
        }
    });

    let ui_weak = ui.as_weak();
    ui.on_use_save_world(move |world| {
        if let Some(ui) = ui_weak.upgrade() {
            let world = world.trim();
            if !world.is_empty() {
                ui.set_world_name(world.into());
            }
        }
    });
}

fn parse_enabled_mods(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModArchive {
    name: String,
    version: String,
    path: PathBuf,
}

fn mod_archive(path: &Path) -> Option<ModArchive> {
    let is_zip = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    if !is_zip {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy();
    let (name, version) = stem.rsplit_once('_')?;
    Some(ModArchive {
        name: name.to_string(),
        version: version.to_string(),
        path: path.to_path_buf(),
    })
}

fn detected_mod_archives(mod_dir: &Path) -> Vec<ModArchive> {
    let Ok(entries) = std::fs::read_dir(mod_dir) else {
        return Vec::new();
    };
    let mut archives: Vec<ModArchive> = entries
        .flatten()
        .filter_map(|entry| mod_archive(&entry.path()))
        .collect();
    archives.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| right.version.cmp(&left.version))
    });
    archives
}

fn mod_name_from_zip(path: &Path) -> Option<String> {
    mod_archive(path).map(|archive| archive.name)
}

fn refresh_detected_mods(ui: &MainWindow) {
    let enabled: HashSet<String> = parse_enabled_mods(&ui.get_enabled_mods_text())
        .into_iter()
        .collect();
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
    for archive in detected_mod_archives(Path::new(ui.get_mod_dir().as_str())) {
        grouped
            .entry(archive.name)
            .or_default()
            .push(archive.version);
    }
    let mut names: Vec<String> = grouped.keys().cloned().collect();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    let rows = names
        .into_iter()
        .map(|name| {
            let versions = grouped.remove(&name).unwrap_or_default().join(", ");
            ModRow {
                enabled: enabled.contains(&name),
                name: name.into(),
                versions: versions.into(),
            }
        })
        .collect::<Vec<_>>();
    let text = if rows.is_empty() {
        "(なし)".to_string()
    } else {
        rows.iter()
            .map(|row| {
                let status = if row.enabled { "enabled" } else { "disabled" };
                format!("{}  {}  [{status}]", row.name, row.versions)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    ui.set_installed_mod_rows(slint::ModelRc::from(std::rc::Rc::new(
        slint::VecModel::from(rows),
    )));
    ui.set_detected_mods_text(text.into());
}

fn remove_old_mod_archives(mod_dir: &Path, mod_name: &str, keep: &Path) -> anyhow::Result<usize> {
    let mut removed = 0;
    let Ok(entries) = std::fs::read_dir(mod_dir) else {
        return Ok(removed);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(archive) = mod_archive(&path) else {
            continue;
        };
        if archive.name == mod_name && path != keep {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove old mod archive {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn remove_mod_archives(mod_dir: &Path, mod_name: &str) -> anyhow::Result<usize> {
    let mut removed = 0;
    let Ok(entries) = std::fs::read_dir(mod_dir) else {
        return Ok(removed);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(archive) = mod_archive(&path) else {
            continue;
        };
        if archive.name == mod_name {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove mod archive {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn add_enabled_mod_name(ui: &MainWindow, name: &str) {
    let mut names = parse_enabled_mods(&ui.get_enabled_mods_text());
    if names.iter().all(|existing| existing != name) {
        names.push(name.to_string());
        ui.set_enabled_mods_text(names.join("\n").into());
    }
}

fn remove_enabled_mod_name(ui: &MainWindow, name: &str) {
    let names = parse_enabled_mods(&ui.get_enabled_mods_text())
        .into_iter()
        .filter(|existing| existing != name)
        .collect::<Vec<_>>();
    ui.set_enabled_mods_text(names.join("\n").into());
}

fn pick_mod_zip() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Factorio mod zip", &["zip"])
        .pick_file()
}

fn install_mod_zip(ui: &MainWindow, source: &Path) -> anyhow::Result<Option<String>> {
    let mod_dir = PathBuf::from(ui.get_mod_dir().as_str());
    std::fs::create_dir_all(&mod_dir).with_context(|| format!("create {}", mod_dir.display()))?;
    let file_name = source
        .file_name()
        .context("selected mod zip has no file name")?;
    let target = mod_dir.join(file_name);
    std::fs::copy(source, &target)
        .with_context(|| format!("copy {} -> {}", source.display(), target.display()))?;
    let name = mod_name_from_zip(&target);
    if let Some(name) = &name {
        remove_old_mod_archives(&mod_dir, name, &target)?;
    }
    Ok(name)
}

#[derive(Clone, Debug)]
struct PortalModRelease {
    version: String,
    file_name: String,
    download_url: String,
}

fn portal_mod_releases(mod_name: &str) -> anyhow::Result<Vec<PortalModRelease>> {
    let mod_name = mod_name.trim();
    anyhow::ensure!(!mod_name.is_empty(), "mod name is empty");
    let cache_bust = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let url = format!("https://mods.factorio.com/api/mods/{mod_name}/full?cache_bust={cache_bust}");
    let details_text = ureq::get(&url)
        .set("Cache-Control", "no-cache")
        .call()
        .context("fetch mod portal release information")?
        .into_string()
        .context("read mod portal release information")?;
    let details: serde_json::Value = serde_json::from_str(&details_text)?;
    let releases = details["releases"]
        .as_array()
        .context("mod has no releases")?;
    let mut out = Vec::new();
    for release in releases.iter().rev() {
        let Some(file_name) = release["file_name"].as_str() else {
            continue;
        };
        let Some(download_url) = release["download_url"].as_str() else {
            continue;
        };
        let version = release["version"]
            .as_str()
            .map(str::to_string)
            .or_else(|| mod_archive(Path::new(file_name)).map(|archive| archive.version));
        let Some(version) = version else { continue };
        out.push(PortalModRelease {
            version,
            file_name: file_name.to_string(),
            download_url: download_url.to_string(),
        });
    }
    anyhow::ensure!(!out.is_empty(), "mod has no downloadable releases");
    Ok(out)
}

fn install_mod_from_portal(
    mod_name: &str,
    version: &str,
    mod_dir: &Path,
) -> anyhow::Result<(String, String, usize)> {
    let mod_name = mod_name.trim();
    let releases = portal_mod_releases(mod_name)?;
    let release = if version.trim().is_empty() {
        releases.first()
    } else {
        releases
            .iter()
            .find(|release| release.version == version.trim())
    }
    .with_context(|| format!("version {} was not found", version.trim()))?;
    let (username, token) = factorio_service_credentials(mod_dir)?;

    std::fs::create_dir_all(mod_dir).with_context(|| format!("create {}", mod_dir.display()))?;
    let target = mod_dir.join(&release.file_name);
    let partial = target.with_extension("zip.part");
    let download = format!(
        "https://mods.factorio.com{}?username={username}&token={token}",
        release.download_url
    );
    let mut response = ureq::get(&download)
        .call()
        .map_err(|err| match err {
            ureq::Error::Status(code, _) => {
                anyhow::anyhow!("mod portal download returned status code {code}")
            }
            ureq::Error::Transport(_) => {
                anyhow::anyhow!("mod portal download failed; check network or Factorio login")
            }
        })?
        .into_reader();
    let mut file =
        std::fs::File::create(&partial).with_context(|| format!("create {}", partial.display()))?;
    if let Err(err) = std::io::copy(&mut response, &mut file)
        .with_context(|| format!("download mod to {}", partial.display()))
    {
        let _ = std::fs::remove_file(&partial);
        return Err(err);
    }
    drop(file);
    if target.exists() {
        std::fs::remove_file(&target).with_context(|| format!("replace {}", target.display()))?;
    }
    std::fs::rename(&partial, &target).with_context(|| format!("install {}", target.display()))?;

    let installed_name =
        mod_name_from_zip(&target).context("downloaded file name did not look like a mod zip")?;
    let removed = remove_old_mod_archives(mod_dir, &installed_name, &target)?;
    Ok((installed_name, release.version.clone(), removed))
}

fn factorio_service_credentials(mod_dir: &Path) -> anyhow::Result<(String, String)> {
    for path in factorio_player_data_candidates(mod_dir) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let username = json["service-username"].as_str().unwrap_or("").trim();
        let token = json["service-token"].as_str().unwrap_or("").trim();
        if !username.is_empty() && !token.is_empty() {
            return Ok((username.to_string(), token.to_string()));
        }
    }
    anyhow::bail!("Factorio service token not found; open Factorio and log in to Mod Portal first");
}

fn factorio_player_data_candidates(mod_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(server_dir) = mod_dir.parent() {
        out.push(server_dir.join("UserData").join("player-data.json"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        out.push(
            PathBuf::from(appdata)
                .join("Factorio")
                .join("player-data.json"),
        );
    }
    out
}

fn wire_mod_callbacks(ui: &MainWindow) {
    let ui_weak = ui.as_weak();
    ui.on_refresh_mods_clicked(move || {
        if let Some(ui) = ui_weak.upgrade() {
            refresh_detected_mods(&ui);
        }
    });

    let ui_weak = ui.as_weak();
    ui.on_open_mod_dir_clicked(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        let dir = ui.get_mod_dir().to_string();
        let _ = std::fs::create_dir_all(&dir);
        if let Err(e) = std::process::Command::new("explorer").arg(&dir).spawn() {
            ui.set_error_text(format!("failed to open mod dir: {e}").into());
        }
    });

    let ui_weak = ui.as_weak();
    ui.on_add_mod_zip_clicked(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        let Some(source) = pick_mod_zip() else { return };
        match install_mod_zip(&ui, &source) {
            Ok(Some(name)) => {
                add_enabled_mod_name(&ui, &name);
                refresh_detected_mods(&ui);
                ui.set_error_text(format!("mod zip added: {name}").into());
            }
            Ok(None) => {
                refresh_detected_mods(&ui);
                ui.set_error_text("mod zip added".into());
            }
            Err(e) => ui.set_error_text(format!("failed to add mod zip: {e:#}").into()),
        }
    });

    let ui_weak = ui.as_weak();
    ui.on_installed_mod_selected(move |name| {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_mod_portal_name(name);
            ui.set_mod_portal_version("".into());
            ui.set_mod_portal_versions(slint::ModelRc::default());
            ui.invoke_fetch_mod_versions_clicked();
        }
    });

    wire_mod_version_lookup_callback(ui);
    wire_mod_install_callback(ui);
    wire_mod_remove_callback(ui);
}

fn wire_mod_version_lookup_callback(ui: &MainWindow) {
    let ui_weak = ui.as_weak();
    ui.on_fetch_mod_versions_clicked(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        let mod_name = ui.get_mod_portal_name().to_string();
        let weak = ui.as_weak();
        ui.set_error_text(format!("fetching versions: {}", mod_name.trim()).into());
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || portal_mod_releases(&mod_name))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("version lookup task failed: {e}")));
            let _ = weak.upgrade_in_event_loop(move |ui| match result {
                Ok(releases) => {
                    let versions = releases
                        .into_iter()
                        .map(|release| slint::SharedString::from(release.version))
                        .collect::<Vec<_>>();
                    let selected = versions.first().cloned().unwrap_or_default();
                    ui.set_mod_portal_versions(slint::ModelRc::from(std::rc::Rc::new(
                        slint::VecModel::from(versions),
                    )));
                    ui.set_mod_portal_version(selected.clone());
                    ui.set_error_text(format!("latest version: {selected}").into());
                }
                Err(e) => ui.set_error_text(format!("failed to fetch mod versions: {e:#}").into()),
            });
        });
    });
}

fn wire_mod_install_callback(ui: &MainWindow) {
    let ui_weak = ui.as_weak();
    ui.on_add_mod_portal_clicked(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        if ui.get_server_running() {
            ui.set_error_text("stop the server before installing or updating mods".into());
            return;
        }
        let mod_name = ui.get_mod_portal_name().to_string();
        let version = ui.get_mod_portal_version().to_string();
        let mod_dir = PathBuf::from(ui.get_mod_dir().as_str());
        let weak = ui.as_weak();
        ui.set_error_text(
            format!("downloading mod: {} {}", mod_name.trim(), version.trim()).into(),
        );
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                install_mod_from_portal(&mod_name, &version, &mod_dir)
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("download task failed: {e}")));
            let _ = weak.upgrade_in_event_loop(move |ui| match result {
                Ok((name, version, removed)) => {
                    add_enabled_mod_name(&ui, &name);
                    refresh_detected_mods(&ui);
                    ui.set_error_text(
                        format!(
                            "mod installed: {name} {version}; removed {removed} old archive(s)"
                        )
                        .into(),
                    );
                }
                Err(e) => ui.set_error_text(format!("failed to download mod: {e:#}").into()),
            });
        });
    });
}

fn wire_mod_remove_callback(ui: &MainWindow) {
    let ui_weak = ui.as_weak();
    ui.on_remove_mod_clicked(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        if ui.get_server_running() {
            ui.set_error_text("stop the server before removing mods".into());
            return;
        }
        let mod_name = ui.get_mod_portal_name().trim().to_string();
        if mod_name.is_empty() {
            ui.set_error_text("enter a mod name to remove".into());
            return;
        }
        let confirmed = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Remove Factorio mod")
            .set_description(format!(
                "Remove every installed version of '{mod_name}'?\n\nBack up the save first if this mod created entities in the world."
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed != rfd::MessageDialogResult::Yes {
            return;
        }
        let mod_dir = PathBuf::from(ui.get_mod_dir().as_str());
        match remove_mod_archives(&mod_dir, &mod_name) {
            Ok(removed) => {
                remove_enabled_mod_name(&ui, &mod_name);
                refresh_detected_mods(&ui);
                ui.set_error_text(format!("removed {mod_name}: {removed} archive(s)").into());
            }
            Err(e) => ui.set_error_text(format!("failed to remove mod: {e:#}").into()),
        }
    });
}

/// Sort `last_backups` in place using the column/direction held in state.
fn sort_backups_inplace(backups: &mut [Backup], column: u8, desc: bool) {
    backups.sort_by(|a, b| {
        let ord = match column {
            1 => a.size_bytes.cmp(&b.size_bytes),
            _ => a.created_at.cmp(&b.created_at),
        };
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

fn backups_to_rows(
    backups: &[Backup],
    kind_filter: BackupKind,
    selected: &HashSet<String>,
) -> Vec<BackupRow> {
    backups
        .iter()
        .filter(|b| b.kind == kind_filter)
        .map(|b| BackupRow {
            id: b.id.0.clone().into(),
            timestamp: b.created_at.format("%Y-%m-%d %H:%M:%S").to_string().into(),
            size: format!("{:.2} MiB", b.size_bytes as f64 / (1024.0 * 1024.0)).into(),
            source: b
                .dir
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
                .into(),
            kind: backup_kind_to_int(b.kind),
            selected: selected.contains(&b.id.0),
        })
        .collect()
}

fn backup_kind_to_int(k: BackupKind) -> i32 {
    match k {
        BackupKind::Auto => 0,
        BackupKind::Manual => 1,
        BackupKind::PreRollback => 2,
    }
}

fn subtab_to_kind(subtab: i32) -> BackupKind {
    match subtab {
        1 => BackupKind::Manual,
        2 => BackupKind::PreRollback,
        _ => BackupKind::Auto,
    }
}

#[allow(clippy::too_many_lines)]
fn wire_backup_callbacks(
    ui: &MainWindow,
    backup_window: &BackupWindow,
    server: Arc<FactorioServer>,
    state: Arc<Mutex<UiState>>,
) {
    let main_weak = ui.as_weak();
    let bw_weak = backup_window.as_weak();

    // MainWindow: open backup window.
    {
        let bw_weak = bw_weak.clone();
        ui.on_open_backup_clicked(move || {
            if let Some(bw) = bw_weak.upgrade() {
                let _ = bw.show();
            }
        });
    }
    // MainWindow: refresh from inline section.
    {
        let server = server.clone();
        let state = state.clone();
        let main_weak = main_weak.clone();
        let bw_weak = bw_weak.clone();
        ui.on_refresh_backups_clicked(move || {
            let server = server.clone();
            let state = state.clone();
            let main_weak = main_weak.clone();
            let bw_weak = bw_weak.clone();
            tokio::spawn(async move {
                refresh_backups_async(&server, &state, &main_weak, &bw_weak).await;
            });
        });
    }

    // BackupWindow: close.
    {
        let bw_weak = bw_weak.clone();
        backup_window.on_close_clicked(move || {
            if let Some(bw) = bw_weak.upgrade() {
                let _ = bw.hide();
            }
        });
    }
    // BackupWindow: refresh.
    {
        let server = server.clone();
        let state = state.clone();
        let main_weak = main_weak.clone();
        let bw_weak = bw_weak.clone();
        backup_window.on_refresh_clicked(move || {
            let server = server.clone();
            let state = state.clone();
            let main_weak = main_weak.clone();
            let bw_weak = bw_weak.clone();
            tokio::spawn(async move {
                refresh_backups_async(&server, &state, &main_weak, &bw_weak).await;
            });
        });
    }
    // BackupWindow: take snapshot now (manual).
    {
        let server = server.clone();
        let state = state.clone();
        let main_weak = main_weak.clone();
        let bw_weak = bw_weak.clone();
        backup_window.on_take_snapshot_clicked(move || {
            let server = server.clone();
            let state = state.clone();
            let main_weak = main_weak.clone();
            let bw_weak = bw_weak.clone();
            tokio::spawn(async move {
                if let Err(e) = server.backup_with_kind(BackupKind::Manual).await {
                    set_status_text(&main_weak, format!("backup failed: {e:#}"));
                }
                refresh_backups_async(&server, &state, &main_weak, &bw_weak).await;
            });
        });
    }
    // BackupWindow: toggle row checkbox.
    {
        let state = state.clone();
        let bw_weak = bw_weak.clone();
        backup_window.on_toggle_row_selected(move |kind_int, idx| {
            let bw = match bw_weak.upgrade() {
                Some(b) => b,
                None => return,
            };
            let kind_filter = subtab_to_kind(kind_int);
            // Read the visible row's id from state (filtered + sorted view).
            let (selected_set_after, auto_count, manual_count, pre_count) = {
                let mut guard = state.lock().expect("state mutex poisoned");
                let view: Vec<&Backup> = guard
                    .last_backups
                    .iter()
                    .filter(|b| b.kind == kind_filter)
                    .collect();
                let target_id = match view.get(idx as usize) {
                    Some(b) => b.id.0.clone(),
                    None => return,
                };
                if guard.selected_backup_ids.contains(&target_id) {
                    guard.selected_backup_ids.remove(&target_id);
                } else {
                    guard.selected_backup_ids.insert(target_id);
                }
                let auto_count = count_selected_for(&guard, BackupKind::Auto);
                let manual_count = count_selected_for(&guard, BackupKind::Manual);
                let pre_count = count_selected_for(&guard, BackupKind::PreRollback);
                let selected = guard.selected_backup_ids.clone();
                (selected, auto_count, manual_count, pre_count)
            };
            // Rebuild both lists (selection state affects checkbox display).
            let (auto_rows, manual_rows, pre_rollback_rows) = {
                let guard = state.lock().expect("state mutex poisoned");
                (
                    backups_to_rows(&guard.last_backups, BackupKind::Auto, &selected_set_after),
                    backups_to_rows(&guard.last_backups, BackupKind::Manual, &selected_set_after),
                    backups_to_rows(
                        &guard.last_backups,
                        BackupKind::PreRollback,
                        &selected_set_after,
                    ),
                )
            };
            install_backup_models(&bw, auto_rows, manual_rows, pre_rollback_rows);
            bw.set_auto_selected_count(auto_count as i32);
            bw.set_manual_selected_count(manual_count as i32);
            bw.set_pre_rollback_selected_count(pre_count as i32);
        });
    }
    // BackupWindow: sort header.
    {
        let state = state.clone();
        let bw_weak = bw_weak.clone();
        backup_window.on_sort_by_clicked(move |column| {
            let bw = match bw_weak.upgrade() {
                Some(b) => b,
                None => return,
            };
            let column_u = if column == 1 { 1u8 } else { 0u8 };
            let (auto_rows, manual_rows, pre_rollback_rows, sort_col, sort_desc) = {
                let mut guard = state.lock().expect("state mutex poisoned");
                if guard.backup_sort_column == column_u {
                    guard.backup_sort_desc = !guard.backup_sort_desc;
                } else {
                    guard.backup_sort_column = column_u;
                    guard.backup_sort_desc = true;
                }
                let col = guard.backup_sort_column;
                let desc = guard.backup_sort_desc;
                sort_backups_inplace(&mut guard.last_backups, col, desc);
                let selected = guard.selected_backup_ids.clone();
                let auto = backups_to_rows(&guard.last_backups, BackupKind::Auto, &selected);
                let manual = backups_to_rows(&guard.last_backups, BackupKind::Manual, &selected);
                let pre = backups_to_rows(&guard.last_backups, BackupKind::PreRollback, &selected);
                (auto, manual, pre, col, desc)
            };
            install_backup_models(&bw, auto_rows, manual_rows, pre_rollback_rows);
            bw.set_sort_column(sort_col as i32);
            bw.set_sort_desc(sort_desc);
        });
    }
    // BackupWindow: rollback confirmed.
    {
        let server = server.clone();
        let state = state.clone();
        let main_weak = main_weak.clone();
        let bw_weak = bw_weak.clone();
        backup_window.on_rollback_confirmed(move |id_shared| {
            let id = BackupId(id_shared.to_string());
            let server = server.clone();
            let state = state.clone();
            let main_weak = main_weak.clone();
            let bw_weak = bw_weak.clone();
            tokio::spawn(async move {
                if let Err(e) = server.rollback(id).await {
                    set_status_text(&main_weak, format!("rollback failed: {e:#}"));
                }
                refresh_backups_async(&server, &state, &main_weak, &bw_weak).await;
            });
        });
    }
    // BackupWindow: request delete-selected (count selected, ask for confirm).
    {
        let state = state.clone();
        let bw_weak = bw_weak.clone();
        backup_window.on_request_delete_selected(move |kind_int| {
            let Some(bw) = bw_weak.upgrade() else { return };
            let kind = subtab_to_kind(kind_int);
            let count = {
                let guard = state.lock().expect("state mutex poisoned");
                count_selected_for(&guard, kind)
            };
            if count > 0 {
                bw.set_pending_delete_list(kind_int);
                bw.set_pending_delete_count(count as i32);
            }
        });
    }
    // BackupWindow: delete selected confirmed.
    {
        let server = server.clone();
        let state = state.clone();
        let main_weak = main_weak.clone();
        let bw_weak = bw_weak.clone();
        backup_window.on_delete_selected_confirmed(move |kind_int| {
            let kind = subtab_to_kind(kind_int);
            let ids: Vec<BackupId> = {
                let guard = state.lock().expect("state mutex poisoned");
                guard
                    .last_backups
                    .iter()
                    .filter(|b| b.kind == kind && guard.selected_backup_ids.contains(&b.id.0))
                    .map(|b| b.id.clone())
                    .collect()
            };
            let server = server.clone();
            let state = state.clone();
            let main_weak = main_weak.clone();
            let bw_weak = bw_weak.clone();
            tokio::spawn(async move {
                for id in ids {
                    if let Err(e) = server.delete_backup(id).await {
                        set_status_text(&main_weak, format!("delete failed: {e:#}"));
                    }
                }
                refresh_backups_async(&server, &state, &main_weak, &bw_weak).await;
            });
        });
    }
}

fn count_selected_for(state: &UiState, kind: BackupKind) -> usize {
    state
        .last_backups
        .iter()
        .filter(|b| b.kind == kind && state.selected_backup_ids.contains(&b.id.0))
        .count()
}

fn install_backup_models(
    bw: &BackupWindow,
    auto: Vec<BackupRow>,
    manual: Vec<BackupRow>,
    pre_rollback: Vec<BackupRow>,
) {
    let auto_model = std::rc::Rc::new(slint::VecModel::from(auto));
    let manual_model = std::rc::Rc::new(slint::VecModel::from(manual));
    let pre_rollback_model = std::rc::Rc::new(slint::VecModel::from(pre_rollback));
    bw.set_snapshots_auto(slint::ModelRc::from(auto_model));
    bw.set_snapshots_manual(slint::ModelRc::from(manual_model));
    bw.set_snapshots_pre_rollback(slint::ModelRc::from(pre_rollback_model));
}

fn wire_connection_callbacks(ui: &MainWindow, state: Arc<Mutex<UiState>>, manager_dir: PathBuf) {
    wire_public_address_save(ui, state.clone(), manager_dir.clone());
    wire_tailscale_address(ui, state.clone(), manager_dir);
    wire_public_address_copy(ui, state);
}

fn wire_public_address_save(ui: &MainWindow, state: Arc<Mutex<UiState>>, manager_dir: PathBuf) {
    let ui_weak = ui.as_weak();
    ui.on_public_address_accepted(move |value| {
        let Some(ui) = ui_weak.upgrade() else { return };
        let trimmed = value.trim().to_string();
        let language = current_language_after_address_save(&state, &manager_dir, &trimmed);
        let t = translations::for_language(language);
        ui.set_public_address(trimmed.into());
        set_public_address_status_briefly(&ui, t.save_success);
    });
}

fn wire_tailscale_address(ui: &MainWindow, state: Arc<Mutex<UiState>>, manager_dir: PathBuf) {
    let ui_weak = ui.as_weak();
    ui.on_use_tailscale_address(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        let (language, port) = current_language_and_port(&state);
        let t = translations::for_language(language);
        let Some(host) = detect_tailscale_host() else {
            set_public_address_status_briefly(&ui, "Tailscale address not found");
            return;
        };
        let address = format!("{host}:{port}");
        persist_public_address(&state, &manager_dir, &address);
        ui.set_public_address(address.into());
        set_public_address_status_briefly(&ui, t.save_success);
    });
}

fn wire_public_address_copy(ui: &MainWindow, state: Arc<Mutex<UiState>>) {
    let ui_weak = ui.as_weak();
    ui.on_copy_public_address(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        let value = ui.get_public_address().to_string();
        let language = state.lock().expect("state mutex poisoned").language;
        let t = translations::for_language(language);
        let msg = match arboard::Clipboard::new().and_then(|mut c| c.set_text(value)) {
            Ok(()) => t.copy_success.to_string(),
            Err(e) => {
                tracing::warn!(error = %e, "clipboard copy failed");
                t.copy_failed.to_string()
            }
        };
        set_public_address_status_briefly(&ui, &msg);
    });
}

fn current_language_after_address_save(
    state: &Arc<Mutex<UiState>>,
    manager_dir: &Path,
    address: &str,
) -> Language {
    persist_public_address(state, manager_dir, address);
    state.lock().expect("state mutex poisoned").language
}

fn current_language_and_port(state: &Arc<Mutex<UiState>>) -> (Language, u16) {
    let guard = state.lock().expect("state mutex poisoned");
    let port = guard
        .config
        .as_ref()
        .map(|cfg| cfg.server.port)
        .unwrap_or(34197);
    (guard.language, port)
}

fn set_public_address_status_briefly(ui: &MainWindow, msg: &str) {
    ui.set_public_address_status(msg.into());
    let weak = ui.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_public_address_status("".into());
        }
    });
}

fn persist_public_address(state: &Arc<Mutex<UiState>>, manager_dir: &Path, address: &str) {
    let mut guard = state.lock().expect("state mutex poisoned");
    if let Some(cfg) = guard.config.as_mut() {
        cfg.manager.public_address = address.to_string();
        let path = manager_dir.join("config.toml");
        if let Err(e) = cfg.save(&path) {
            tracing::warn!(error = %e, "failed to persist public address");
        }
    }
}

fn detect_tailscale_ipv4() -> Option<String> {
    detect_tailscale_ipv4_from_cli().or_else(detect_tailscale_ipv4_from_adapter)
}

fn detect_tailscale_host() -> Option<String> {
    detect_tailscale_dns_name()
        .or_else(detect_tailscale_ipv4)
        .map(|host| host.trim_end_matches('.').to_string())
}

fn detect_tailscale_dns_name() -> Option<String> {
    let output = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    find_json_string_value(&stdout, "DNSName").filter(|name| !name.is_empty())
}

fn find_json_string_value(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let after_key = json.split_once(&needle)?.1;
    let after_colon = after_key.split_once(':')?.1.trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }

    let mut value = String::new();
    let mut escaped = false;
    for ch in after_colon[1..].chars() {
        if escaped {
            value.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(value);
        } else {
            value.push(ch);
        }
    }
    None
}

fn detect_tailscale_ipv4_from_cli() -> Option<String> {
    let output = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| is_tailscale_ipv4(line))
        .map(str::to_string)
}

fn detect_tailscale_ipv4_from_adapter() -> Option<String> {
    let script = "Get-NetIPAddress -AddressFamily IPv4 | \
                  Where-Object { $_.InterfaceAlias -like '*Tailscale*' } | \
                  Select-Object -First 1 -ExpandProperty IPAddress";
    let output = std::process::Command::new("powershell")
        .args(["-NoLogo", "-NoProfile", "-Command", script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| is_tailscale_ipv4(line))
        .map(str::to_string)
}

fn is_tailscale_ipv4(value: &str) -> bool {
    value.starts_with("100.") && value.parse::<std::net::Ipv4Addr>().is_ok()
}

struct BackupViewData {
    auto_rows: Vec<BackupRow>,
    manual_rows: Vec<BackupRow>,
    pre_rollback_rows: Vec<BackupRow>,
    auto_count: usize,
    manual_count: usize,
    pre_count: usize,
    count_text: String,
}

fn prepare_backup_view(backups: &mut [Backup], state: &Arc<Mutex<UiState>>) -> BackupViewData {
    let mut guard = state.lock().expect("state mutex poisoned");
    sort_backups_inplace(backups, guard.backup_sort_column, guard.backup_sort_desc);

    let live: HashSet<String> = backups.iter().map(|b| b.id.0.clone()).collect();
    guard.selected_backup_ids.retain(|id| live.contains(id));
    guard.last_backups = backups.to_vec();

    let selected = &guard.selected_backup_ids;
    let auto_rows = backups_to_rows(backups, BackupKind::Auto, selected);
    let manual_rows = backups_to_rows(backups, BackupKind::Manual, selected);
    let pre_rollback_rows = backups_to_rows(backups, BackupKind::PreRollback, selected);
    let auto_count = count_selected_for(&guard, BackupKind::Auto);
    let manual_count = count_selected_for(&guard, BackupKind::Manual);
    let pre_count = count_selected_for(&guard, BackupKind::PreRollback);
    let t = translations::for_language(guard.language);
    let count_text = if backups.is_empty() {
        t.no_backups.to_string()
    } else {
        translations::fmt_count(t.backups_count_fmt, backups.len())
    };

    BackupViewData {
        auto_rows,
        manual_rows,
        pre_rollback_rows,
        auto_count,
        manual_count,
        pre_count,
        count_text,
    }
}

async fn refresh_backups_async(
    server: &Arc<FactorioServer>,
    state: &Arc<Mutex<UiState>>,
    main_weak: &slint::Weak<MainWindow>,
    backup_weak: &slint::Weak<BackupWindow>,
) {
    let mut backups = match server.list_backups().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "list_backups failed");
            return;
        }
    };

    let view = prepare_backup_view(&mut backups, state);

    // Push to BackupWindow.
    {
        let bw = backup_weak.clone();
        let auto = view.auto_rows;
        let manual = view.manual_rows;
        let pre = view.pre_rollback_rows;
        let auto_count = view.auto_count;
        let manual_count = view.manual_count;
        let pre_count = view.pre_count;
        let _ = bw.upgrade_in_event_loop(move |bw| {
            install_backup_models(&bw, auto, manual, pre);
            bw.set_auto_selected_count(auto_count as i32);
            bw.set_manual_selected_count(manual_count as i32);
            bw.set_pre_rollback_selected_count(pre_count as i32);
        });
    }

    // Push count text to MainWindow.
    {
        let main = main_weak.clone();
        let count_text = view.count_text;
        let _ = main.upgrade_in_event_loop(move |ui| {
            ui.set_backups_count_text(count_text.into());
        });
    }
}

/// Map a ServerStatus to `(busy, server_running)` flags driving the action
/// button enablement and the progress bar visibility.
fn flags_for_status(s: ServerStatus) -> (bool, bool) {
    match s {
        ServerStatus::Stopped | ServerStatus::Crashed => (false, false),
        ServerStatus::Starting | ServerStatus::Stopping | ServerStatus::Updating => (true, false),
        ServerStatus::Running => (false, true),
    }
}

#[allow(clippy::too_many_lines)]
fn wire_browse_and_save(
    ui: &MainWindow,
    server: Option<Arc<FactorioServer>>,
    state: Arc<Mutex<UiState>>,
    manager_dir: PathBuf,
) {
    // Browse: native file/folder picker per field key.
    {
        let ui_weak = ui.as_weak();
        ui.on_browse_path(move |key_shared| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let key = key_shared.to_string();
            let current = match key.as_str() {
                "steamcmd" => ui.get_steamcmd_path().to_string(),
                "server_dir" => ui.get_server_dir().to_string(),
                "save_dir" => ui.get_save_dir().to_string(),
                "backup_dir" => ui.get_backup_dir().to_string(),
                "log_file" => ui.get_log_file().to_string(),
                _ => return,
            };
            if let Some(picked) = pick_for_key(&key, &current) {
                let s = picked.to_string_lossy().to_string();
                match key.as_str() {
                    "steamcmd" => ui.set_steamcmd_path(s.into()),
                    "server_dir" => {
                        let mod_dir = PathBuf::from(&s).join("mods").display().to_string();
                        ui.set_server_dir(s.into());
                        ui.set_mod_dir(mod_dir.into());
                        refresh_detected_mods(&ui);
                    }
                    "save_dir" => {
                        ui.set_save_dir(s.into());
                        refresh_save_worlds(&ui);
                    }
                    "backup_dir" => ui.set_backup_dir(s.into()),
                    "log_file" => ui.set_log_file(s.into()),
                    _ => {}
                }
            }
        });
    }

    // Save: validate, mkdir, write config.toml, then respawn this exe.
    {
        let ui_weak = ui.as_weak();
        let server = server.clone();
        let manager_dir = manager_dir.clone();
        let state = state.clone();
        ui.on_save_settings(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };

            if let Some(s) = server.as_ref() {
                let st = s.status();
                if !matches!(st, ServerStatus::Stopped | ServerStatus::Crashed) {
                    let language = state.lock().expect("state mutex poisoned").language;
                    let msg = match language {
                        Language::Ja => {
                            "サーバー実行中は保存できません。先にサーバーを停止してください。"
                        }
                        Language::En => "Cannot save while the server is running. Stop it first.",
                    };
                    tracing::info!(status = ?st, "save blocked while server is active");
                    ui.set_error_text(msg.into());
                    return;
                }
            }

            let language = {
                let guard = state.lock().expect("state mutex poisoned");
                guard.language
            };
            let cfg = match build_config_from_ui(&ui, language) {
                Ok(c) => c,
                Err(e) => {
                    ui.set_error_text(e.into());
                    return;
                }
            };
            if let Err(e) = cfg.validate() {
                ui.set_error_text(format!("{e}").into());
                return;
            }
            if let Err(e) = ensure_directories(&cfg) {
                ui.set_error_text(format!("Failed to create directories: {e:#}").into());
                return;
            }
            let config_path = manager_dir.join("config.toml");
            if let Err(e) = cfg.save(&config_path) {
                ui.set_error_text(
                    format!("Failed to write {}: {e:#}", config_path.display()).into(),
                );
                return;
            }

            ui.set_error_text("Saved. Restarting...".into());
            let _ = ui.hide();
            if let Err(e) = restart_self() {
                tracing::error!(error = ?e, "respawn failed");
            }
            let _ = slint::quit_event_loop();
        });
    }
}

fn wire_language_callback(
    ui: &MainWindow,
    backup_window: &BackupWindow,
    server: Option<Arc<FactorioServer>>,
    state: Arc<Mutex<UiState>>,
    manager_dir: PathBuf,
) {
    let ui_weak = ui.as_weak();
    let bw_weak = backup_window.as_weak();
    ui.on_language_changed(move |idx| {
        let Some(ui) = ui_weak.upgrade() else {
            return;
        };
        let lang = translations::language_from_index(idx);
        let t = translations::for_language(lang);

        apply_strings(&ui, t);
        if let Some(bw) = bw_weak.upgrade() {
            apply_backup_window_strings(&bw, t);
        }
        ui.set_language_index(translations::language_index(lang));

        let (cfg_snapshot, last_status) = {
            let mut guard = state.lock().expect("state mutex poisoned");
            guard.language = lang;
            (guard.config.clone(), guard.last_status)
        };

        match &cfg_snapshot {
            Some(cfg) => {
                ui.set_paths_summary(translations::render_paths_summary(cfg, t).into());
                ui.set_params_summary(translations::render_params_summary(cfg, t).into());
                update_install_state(&ui, cfg, t);
                let status_label = if let Some(s) = server.as_ref() {
                    translations::status_label(s.status(), t)
                } else {
                    translations::status_label(last_status, t)
                };
                ui.set_status_text(status_label.into());
            }
            None => {
                let config_path = manager_dir.join("config.toml");
                ui.set_status_text(
                    format!(
                        "{}{}{}",
                        t.no_config_prefix,
                        config_path.display(),
                        t.no_config_suffix
                    )
                    .into(),
                );
            }
        }

        rerender_players_for_language(&ui, &state, t);
        // Same for backups, but the data needs a re-list to be safe.
        if let Some(s) = server.as_ref() {
            let server = s.clone();
            let state_for_refresh = state.clone();
            let main_weak = ui.as_weak();
            let backup_weak = bw_weak.clone();
            tokio::spawn(async move {
                refresh_backups_async(&server, &state_for_refresh, &main_weak, &backup_weak).await;
            });
        }

        if let Some(mut cfg) = cfg_snapshot {
            cfg.manager.language = lang;
            let config_path = manager_dir.join("config.toml");
            if let Err(e) = cfg.save(&config_path) {
                tracing::warn!(error = %e, "failed to persist language change");
            }
            state.lock().expect("state mutex poisoned").config = Some(cfg);
        }
    });
}

fn rerender_players_for_language(ui: &MainWindow, state: &Arc<Mutex<UiState>>, t: &Strings) {
    let guard = state.lock().expect("state mutex poisoned");
    let (rows, count_text, simulation_text, proof_text) =
        build_player_data(&guard.players, player_render_context(&guard, t));
    let activity_rows = build_activity_rows(&guard.activity, &guard.players, guard.language);
    install_player_model(ui, rows, count_text, simulation_text, proof_text);
    install_activity_model(ui, activity_rows);
}

fn pick_for_key(key: &str, current: &str) -> Option<PathBuf> {
    let current_path = PathBuf::from(current);
    let mut dlg = rfd::FileDialog::new();
    if let Some(dir) = starting_dir_for(&current_path) {
        dlg = dlg.set_directory(dir);
    }

    match key {
        "steamcmd" => dlg.add_filter("steamcmd.exe", &["exe"]).pick_file(),
        "server_dir" | "save_dir" | "backup_dir" => dlg.pick_folder(),
        "log_file" => {
            let name = current_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "server.log".to_string());
            dlg.set_file_name(name).save_file()
        }
        _ => None,
    }
}

fn starting_dir_for(p: &Path) -> Option<PathBuf> {
    if p.is_dir() {
        Some(p.to_path_buf())
    } else if let Some(parent) = p.parent() {
        if parent.as_os_str().is_empty() {
            None
        } else if parent.exists() {
            Some(parent.to_path_buf())
        } else {
            None
        }
    } else {
        None
    }
}

fn restart_self() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    std::process::Command::new(exe)
        .spawn()
        .context("respawn current exe")?;
    Ok(())
}

fn set_status_text(weak: &slint::Weak<MainWindow>, msg: String) {
    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_status_text(msg.into()));
}

fn log_line_from(ev: &ServerEvent, t: &Strings) -> Option<String> {
    match ev {
        ServerEvent::Log(s) => Some(s.clone()),
        ServerEvent::WorldSaved { at } => {
            Some(format!("[saved] {}", at.format("%Y-%m-%d %H:%M:%S")))
        }
        ServerEvent::PlayerJoined { name } => Some(format!("[+] {name}")),
        ServerEvent::PlayerLeft { name } => Some(format!("[-] {name}")),
        ServerEvent::NetworkStatus { text } => Some(format!("[net] {text}")),
        ServerEvent::ServerReady => Some("[ready] accepting connections".into()),
        ServerEvent::Warning(s) => Some(format!("[warning] {s}")),
        ServerEvent::StatusChanged(s) => {
            Some(format!("[status] {}", translations::status_label(*s, t)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mod_archive_name_and_version() {
        let archive = mod_archive(Path::new("personal-respawn-anchor_0.1.3.zip")).unwrap();
        assert_eq!(archive.name, "personal-respawn-anchor");
        assert_eq!(archive.version, "0.1.3");
        assert!(mod_archive(Path::new("personal-respawn-anchor.zip")).is_none());
        assert!(mod_archive(Path::new("personal-respawn-anchor_0.1.3.txt")).is_none());
    }

    #[test]
    fn removes_only_old_versions_of_the_selected_mod() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("factorio-mod-cleanup-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("personal-respawn-anchor_0.1.2.zip");
        let keep = dir.join("personal-respawn-anchor_0.1.3.zip");
        let other = dir.join("respawn-beacon_1.0.0.zip");
        std::fs::write(&old, []).unwrap();
        std::fs::write(&keep, []).unwrap();
        std::fs::write(&other, []).unwrap();

        assert_eq!(
            remove_old_mod_archives(&dir, "personal-respawn-anchor", &keep).unwrap(),
            1
        );
        assert!(!old.exists());
        assert!(keep.exists());
        assert!(other.exists());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_most_recent_steam_account() {
        let text = r#"
"users"
{
    "111"
    {
        "AccountName" "old_user"
        "MostRecent" "0"
    }
    "222"
    {
        "AccountName" "recent_user"
        "MostRecent" "1"
    }
}
"#;

        assert_eq!(
            parse_steam_loginusers_account(text).as_deref(),
            Some("recent_user")
        );
    }

    #[test]
    fn parses_first_steam_account_without_most_recent() {
        let text = r#"
"users"
{
    "111"
    {
        "AccountName" "first_user"
    }
}
"#;

        assert_eq!(
            parse_steam_loginusers_account(text).as_deref(),
            Some("first_user")
        );
    }

    #[test]
    fn recognizes_tailscale_ipv4() {
        assert!(is_tailscale_ipv4("100.64.0.1"));
        assert!(!is_tailscale_ipv4("192.168.1.10"));
        assert!(!is_tailscale_ipv4("100.not.an.ip"));
    }

    #[test]
    fn parses_tailscale_dns_name_from_status_json() {
        let json = r#"{"Self":{"DNSName":"server.example.ts.net."}}"#;
        assert_eq!(
            find_json_string_value(json, "DNSName").as_deref(),
            Some("server.example.ts.net.")
        );
    }

    #[test]
    fn restores_player_roster_from_latest_log_session() {
        let text = "\
old joined the game
Hosting game at IP ADDR
123.456 Info ServerMultiplayerManager.cpp:123: alice joined the game
bob joined the game
alice left the game
";
        let roster = restore_player_roster_from_text(text);
        assert!(!roster.contains_key("old"));
        assert!(!roster.contains_key("alice"));
        assert!(roster.contains_key("bob"));
    }

    #[test]
    fn restores_player_roster_from_console_markers() {
        let text = "\
Hosting game at IP ADDR
2026-07-04 07:18:41 [JOIN] alice joined the game
2026-07-04 07:19:28 [JOIN] bob joined the game
2026-07-04 07:21:10 [LEAVE] alice left the game
";
        let roster = restore_player_roster_from_text(text);
        assert!(!roster.contains_key("alice"));
        assert!(roster.contains_key("bob"));
    }

    #[test]
    fn restores_recent_activity_from_manager_log() {
        let text = "\
2026-07-04 07:18:41 [JOIN] alice joined the game
2026-07-04 07:19:28 [JOIN] bob joined the game
2026-07-04 07:21:10 [LEAVE] alice left the game
";
        let rows = restore_activity_from_text(text);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].player_name, "alice");
        assert!(matches!(rows[0].kind, ActivityKind::Join));
        assert_eq!(rows[2].player_name, "alice");
        assert!(matches!(rows[2].kind, ActivityKind::Leave));
        assert_eq!(
            rows[2].at.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-07-04 07:21:10"
        );
    }

    #[test]
    fn groups_activity_by_player() {
        let text = "\
2026-07-04 07:18:41 [JOIN] alice joined the game
2026-07-04 07:19:28 [JOIN] bob joined the game
2026-07-04 07:21:10 [LEAVE] alice left the game
";
        let activity = restore_activity_from_text(text);
        let mut online = HashMap::new();
        online.insert("bob".to_string(), chrono::Local::now());
        let rows = build_activity_rows(&activity, &online, Language::En);
        let alice = rows.iter().find(|row| row.player_name == "alice").unwrap();
        let bob = rows.iter().find(|row| row.player_name == "bob").unwrap();
        assert_eq!(alice.action, "Offline");
        assert!(alice.at.contains("IN 07/04 07:18"));
        assert!(alice.at.contains("OUT 07/04 07:21"));
        assert_eq!(bob.action, "Online");
        assert!(bob.at.contains("IN 07/04 07:19"));
    }
}
