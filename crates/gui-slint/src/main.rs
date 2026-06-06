// Hide the console window on Windows release builds — spec §8.1.
// In debug builds we keep the console so `tracing` output is visible.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod translations;

use anyhow::Context;
use gsm_core::{
    AppConfig, Backup, BackupId, BackupKind, GameServerManager, Language, ManagerConfig,
    PathsConfig, ServerConfig, ServerEvent, ServerStatus,
};
use std::collections::HashSet;
use slint::ComponentHandle;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::EnvFilter;
use translations::Strings;
use valheim::ValheimServer;

slint::include_modules!();

/// Mutable runtime state shared by callbacks.
struct UiState {
    config: Option<AppConfig>,
    language: Language,
    /// Last status observed; used to re-translate the status label when the
    /// user toggles language outside of a status change.
    last_status: ServerStatus,
    /// Currently-connected players, keyed by SteamID. The value is when we
    /// observed the PlayerJoined event.
    players: HashMap<u64, chrono::DateTime<chrono::Local>>,
    /// Last refresh from `list_backups`. Cached so we can rebuild row models
    /// after toggling sort / selection without going back to disk.
    last_backups: Vec<Backup>,
    /// BackupId.0 of currently-selected rows (any list).
    selected_backup_ids: HashSet<String>,
    /// Sort column for the backup list: 0 = when, 1 = size.
    backup_sort_column: u8,
    backup_sort_desc: bool,
}

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
    let world_window = WorldSettingsWindow::new()?;

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
        last_backups: Vec::new(),
        selected_backup_ids: HashSet::new(),
        backup_sort_column: 0,
        backup_sort_desc: true,
    }));

    // Localised strings + language combo selection first, so the first frame
    // is already in the user's language.
    apply_strings(&ui, translations::for_language(initial_language));
    apply_backup_window_strings(&backup_window, translations::for_language(initial_language));
    apply_world_window_strings(&world_window, translations::for_language(initial_language));
    apply_world_window_models(&world_window, translations::for_language(initial_language));
    ui.set_language_index(translations::language_index(initial_language));

    populate_settings_fields(
        &ui,
        initial_config.as_ref().unwrap_or(&default_config_for_setup()),
    );

    let server: Option<Arc<ValheimServer>> = match initial_config.as_ref() {
        Some(cfg) => {
            let t = translations::for_language(initial_language);
            ui.set_status_text(t.status_stopped.into());
            ui.set_paths_summary(translations::render_paths_summary(cfg, t).into());
            ui.set_params_summary(translations::render_params_summary(cfg, t).into());
            ui.set_server_controls_enabled(true);
            ui.set_public_address(cfg.manager.public_address.clone().into());
            // Populate BackupWindow's context strip (world / paths display).
            backup_window.set_world_name(cfg.server.world.clone().into());
            backup_window.set_save_dir_display(cfg.paths.save_dir.display().to_string().into());
            backup_window.set_backup_dir_display(cfg.paths.backup_dir.display().to_string().into());
            backup_window.set_server_controls_enabled(true);
            Some(Arc::new(ValheimServer::new(cfg.clone(), manager_dir.clone())))
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
            None
        }
    };

    if let Some(server) = server.as_ref().cloned() {
        wire_server_callbacks(&ui, server.clone());
        spawn_event_forwarder(&ui, server.clone(), state.clone());
        wire_backup_callbacks(
            &ui,
            &backup_window,
            server.clone(),
            state.clone(),
        );

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

        // Adopt any Valheim server left running by a previous GUI session.
        // Done in a background task so the window appears immediately; the
        // event forwarder picks up the Running status flip when it resolves.
        let server_for_reattach = server.clone();
        tokio::spawn(async move {
            match server_for_reattach.try_reattach().await {
                Ok(true) => tracing::info!("re-attached to running server"),
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
    wire_language_callback(
        &ui,
        &backup_window,
        &world_window,
        server.clone(),
        state.clone(),
        manager_dir.clone(),
    );
    wire_connection_callbacks(&ui, state.clone(), manager_dir.clone());
    wire_world_callbacks(&ui, &world_window);

    ui.run()?;
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
    AppConfig {
        paths: PathsConfig {
            steamcmd: PathBuf::from("D:\\Valheim\\SteamCMD\\steamcmd.exe"),
            server_dir: PathBuf::from("D:\\Valheim\\Server"),
            save_dir: PathBuf::from("D:\\Valheim\\Data"),
            backup_dir: PathBuf::from("D:\\Valheim\\Backups"),
            log_file: PathBuf::from("D:\\Valheim\\Server\\logs\\server.log"),
        },
        server: ServerConfig::default(),
        manager: ManagerConfig::default(),
    }
}

fn apply_strings(ui: &MainWindow, t: &Strings) {
    ui.set_t_app_title(t.app_title.into());

    ui.set_t_group_setup(t.group_setup.into());
    ui.set_t_group_paths(t.group_paths.into());
    ui.set_t_group_server(t.group_server.into());
    ui.set_t_group_manager(t.group_manager.into());
    ui.set_t_group_status(t.group_status.into());
    ui.set_t_group_operation(t.group_operation.into());
    ui.set_t_group_log(t.group_log.into());

    ui.set_t_lbl_language(t.lbl_language.into());

    ui.set_t_server_prefix(t.server_prefix.into());
    ui.set_t_btn_start(t.btn_start.into());
    ui.set_t_btn_stop(t.btn_stop.into());
    ui.set_t_btn_update(t.btn_update.into());
    ui.set_t_btn_save(t.btn_save.into());

    ui.set_t_lbl_steamcmd(t.lbl_steamcmd.into());
    ui.set_t_lbl_server_dir(t.lbl_server_dir.into());
    ui.set_t_lbl_save_dir(t.lbl_save_dir.into());
    ui.set_t_lbl_backup_dir(t.lbl_backup_dir.into());
    ui.set_t_lbl_log_file(t.lbl_log_file.into());
    ui.set_t_btn_browse(t.btn_browse.into());

    ui.set_t_lbl_name(t.lbl_name.into());
    ui.set_t_lbl_world(t.lbl_world.into());
    ui.set_t_lbl_password(t.lbl_password.into());
    ui.set_t_lbl_port(t.lbl_port.into());
    ui.set_t_lbl_public(t.lbl_public.into());
    ui.set_t_lbl_save_interval(t.lbl_save_interval.into());
    ui.set_t_lbl_backups(t.lbl_backups.into());

    ui.set_t_btn_open_world(t.btn_open_world.into());

    ui.set_t_lbl_graceful_stop(t.lbl_graceful_stop.into());
    ui.set_t_chk_auto_backup(t.chk_auto_backup.into());
    ui.set_t_lbl_backup_short(t.lbl_backup_short.into());
    ui.set_t_lbl_backup_long(t.lbl_backup_long.into());
    ui.set_t_backup_intervals_hint(t.backup_intervals_hint.into());

    ui.set_t_group_players(t.group_players.into());
    ui.set_t_group_backup(t.group_backup.into());
    ui.set_t_btn_refresh(t.btn_refresh.into());
    ui.set_t_btn_open_backup(t.btn_open_backup.into());
    ui.set_t_no_players(t.no_players.into());
    ui.set_t_no_backups(t.no_backups.into());

    ui.set_t_group_connection(t.group_connection.into());
    ui.set_t_lbl_public_address(t.lbl_public_address.into());
    ui.set_t_public_address_hint(t.public_address_hint.into());
    ui.set_t_btn_copy(t.btn_copy.into());
}

/// Push every translatable label on the BackupWindow.
fn apply_backup_window_strings(bw: &BackupWindow, t: &Strings) {
    bw.set_t_backup_title(t.backup_window_title.into());
    bw.set_t_sidebar_paths(t.backup_sidebar_paths.into());
    bw.set_t_sidebar_list(t.backup_sidebar_list.into());
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

/// Push every translatable label on the WorldSettingsWindow.
fn apply_world_window_strings(ww: &WorldSettingsWindow, t: &Strings) {
    ww.set_t_window_title(t.world_window_title.into());
    ww.set_t_btn_done(t.world_done.into());
    ww.set_t_btn_cancel(t.world_cancel.into());
    ww.set_t_lbl_preset(t.lbl_preset.into());
    ww.set_t_lbl_combat(t.lbl_combat.into());
    ww.set_t_lbl_deathpenalty(t.lbl_deathpenalty.into());
    ww.set_t_lbl_resources(t.lbl_resources.into());
    ww.set_t_lbl_raids(t.lbl_raids.into());
    ww.set_t_lbl_portals(t.lbl_portals.into());
    ww.set_t_preset_description(t.preset_description.into());
    ww.set_t_combat_description(t.combat_description.into());
    ww.set_t_deathpenalty_description(t.deathpenalty_description.into());
    ww.set_t_resources_description(t.resources_description.into());
    ww.set_t_raids_description(t.raids_description.into());
    ww.set_t_portals_description(t.portals_description.into());
    ww.set_t_lbl_keys(t.lbl_keys.into());
    ww.set_t_keys_description(t.keys_description.into());
    ww.set_t_key_nobuildcost(t.key_nobuildcost.into());
    ww.set_t_key_nobuildcost_desc(t.key_nobuildcost_desc.into());
    ww.set_t_key_passivemobs(t.key_passivemobs.into());
    ww.set_t_key_passivemobs_desc(t.key_passivemobs_desc.into());
    ww.set_t_key_nomap(t.key_nomap.into());
    ww.set_t_key_nomap_desc(t.key_nomap_desc.into());
    ww.set_t_key_noportals(t.key_noportals.into());
    ww.set_t_key_noportals_desc(t.key_noportals_desc.into());
    ww.set_t_key_playerevents(t.key_playerevents.into());
    ww.set_t_key_playerevents_desc(t.key_playerevents_desc.into());
    ww.set_t_key_showenemyhud(t.key_showenemyhud.into());
    ww.set_t_key_showenemyhud_desc(t.key_showenemyhud_desc.into());
    ww.set_t_key_devcommands(t.key_devcommands.into());
    ww.set_t_key_devcommands_desc(t.key_devcommands_desc.into());
}

/// Push the localised ComboBox option lists on the WorldSettingsWindow.
/// Slint preserves `current-index` across model swaps when lengths match,
/// so language switching keeps the user's choice intact.
fn apply_world_window_models(ww: &WorldSettingsWindow, t: &Strings) {
    ww.set_preset_options(strings_to_model(t.preset_labels));
    ww.set_combat_options(strings_to_model(t.combat_labels));
    ww.set_deathpenalty_options(strings_to_model(t.deathpenalty_labels));
    ww.set_resources_options(strings_to_model(t.resources_labels));
    ww.set_raids_options(strings_to_model(t.raids_labels));
    ww.set_portals_options(strings_to_model(t.portals_labels));
}

fn strings_to_model(labels: &[&'static str]) -> slint::ModelRc<slint::SharedString> {
    let v: Vec<slint::SharedString> = labels.iter().map(|s| slint::SharedString::from(*s)).collect();
    let model = std::rc::Rc::new(slint::VecModel::from(v));
    slint::ModelRc::from(model)
}

fn populate_settings_fields(ui: &MainWindow, cfg: &AppConfig) {
    ui.set_steamcmd_path(cfg.paths.steamcmd.display().to_string().into());
    ui.set_server_dir(cfg.paths.server_dir.display().to_string().into());
    ui.set_save_dir(cfg.paths.save_dir.display().to_string().into());
    ui.set_backup_dir(cfg.paths.backup_dir.display().to_string().into());
    ui.set_log_file(cfg.paths.log_file.display().to_string().into());

    ui.set_server_name(cfg.server.name.clone().into());
    ui.set_world_name(cfg.server.world.clone().into());
    ui.set_server_password(cfg.server.password.clone().into());
    ui.set_server_port(cfg.server.port.to_string().into());
    ui.set_server_public(cfg.server.public.to_string().into());
    ui.set_save_interval(cfg.server.save_interval.to_string().into());
    ui.set_backup_count(cfg.server.backups.to_string().into());

    ui.set_preset_index(translations::index_of_value(
        &cfg.server.preset,
        translations::PRESET_VALUES,
    ));
    ui.set_mod_combat_index(translations::index_of_value(
        &cfg.server.mod_combat,
        translations::COMBAT_VALUES,
    ));
    ui.set_mod_deathpenalty_index(translations::index_of_value(
        &cfg.server.mod_deathpenalty,
        translations::DEATHPENALTY_VALUES,
    ));
    ui.set_mod_resources_index(translations::index_of_value(
        &cfg.server.mod_resources,
        translations::RESOURCES_VALUES,
    ));
    ui.set_mod_raids_index(translations::index_of_value(
        &cfg.server.mod_raids,
        translations::RAIDS_VALUES,
    ));
    ui.set_mod_portals_index(translations::index_of_value(
        &cfg.server.mod_portals,
        translations::PORTALS_VALUES,
    ));

    let keys = &cfg.server.world_keys;
    let has = |k: &str| keys.iter().any(|s| s == k);
    ui.set_key_nobuildcost(has("nobuildcost"));
    ui.set_key_passivemobs(has("passivemobs"));
    ui.set_key_nomap(has("nomap"));
    ui.set_key_noportals(has("noportals"));
    ui.set_key_playerevents(has("playerevents"));
    ui.set_key_showenemyhud(has("showenemyhud"));
    ui.set_key_devcommands(has("devcommands"));

    ui.set_graceful_stop_timeout_secs(
        cfg.manager.graceful_stop_timeout_secs.to_string().into(),
    );
    ui.set_auto_backup_before_update(cfg.manager.auto_backup_before_update);
    ui.set_backup_short_secs(cfg.manager.backup_short_secs.to_string().into());
    ui.set_backup_long_secs(cfg.manager.backup_long_secs.to_string().into());

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
            crossplay: false,
            mod_combat: translations::value_at_index(
                ui.get_mod_combat_index(),
                translations::COMBAT_VALUES,
            )
            .to_string(),
            mod_deathpenalty: translations::value_at_index(
                ui.get_mod_deathpenalty_index(),
                translations::DEATHPENALTY_VALUES,
            )
            .to_string(),
            mod_resources: translations::value_at_index(
                ui.get_mod_resources_index(),
                translations::RESOURCES_VALUES,
            )
            .to_string(),
            mod_raids: translations::value_at_index(
                ui.get_mod_raids_index(),
                translations::RAIDS_VALUES,
            )
            .to_string(),
            mod_portals: translations::value_at_index(
                ui.get_mod_portals_index(),
                translations::PORTALS_VALUES,
            )
            .to_string(),
            preset: translations::value_at_index(
                ui.get_preset_index(),
                translations::PRESET_VALUES,
            )
            .to_string(),
            world_keys: {
                let mut v: Vec<String> = Vec::new();
                if ui.get_key_nobuildcost() { v.push("nobuildcost".into()); }
                if ui.get_key_passivemobs() { v.push("passivemobs".into()); }
                if ui.get_key_nomap() { v.push("nomap".into()); }
                if ui.get_key_noportals() { v.push("noportals".into()); }
                if ui.get_key_playerevents() { v.push("playerevents".into()); }
                if ui.get_key_showenemyhud() { v.push("showenemyhud".into()); }
                if ui.get_key_devcommands() { v.push("devcommands".into()); }
                v
            },
        },
        manager: ManagerConfig {
            graceful_stop_timeout_secs: parse_u::<u32>(
                &ui.get_graceful_stop_timeout_secs(),
                "graceful_stop_timeout_secs",
            )?,
            auto_backup_before_update: ui.get_auto_backup_before_update(),
            language,
            public_address: ui.get_public_address().to_string(),
            backup_short_secs: parse_u::<u32>(
                &ui.get_backup_short_secs(),
                "backup_short_secs",
            )?,
            backup_long_secs: parse_u::<u32>(
                &ui.get_backup_long_secs(),
                "backup_long_secs",
            )?,
        },
    })
}

fn ensure_directories(cfg: &AppConfig) -> anyhow::Result<()> {
    std::fs::create_dir_all(&cfg.paths.server_dir)
        .with_context(|| format!("create {}", cfg.paths.server_dir.display()))?;
    std::fs::create_dir_all(&cfg.paths.save_dir)
        .with_context(|| format!("create {}", cfg.paths.save_dir.display()))?;
    std::fs::create_dir_all(&cfg.paths.backup_dir)
        .with_context(|| format!("create {}", cfg.paths.backup_dir.display()))?;
    if let Some(parent) = cfg.paths.log_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    Ok(())
}

fn wire_server_callbacks(ui: &MainWindow, server: Arc<ValheimServer>) {
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
                if let Err(e) = server.install_or_update().await {
                    set_status_text(&weak, format!("update failed: {e:#}"));
                }
            });
        });
    }
}

fn spawn_event_forwarder(
    ui: &MainWindow,
    server: Arc<ValheimServer>,
    state: Arc<Mutex<UiState>>,
) {
    let weak = ui.as_weak();
    let mut rx = server.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let (status_text, flags, language, players_update) = {
                        let mut guard = state.lock().expect("state mutex poisoned");
                        if let ServerEvent::StatusChanged(s) = &ev {
                            guard.last_status = *s;
                            // Clear the player list on transitions away
                            // from Running so we don't leave ghost rows.
                            if !matches!(*s, ServerStatus::Running) {
                                guard.players.clear();
                            }
                        }
                        let mut roster_changed = false;
                        match &ev {
                            ServerEvent::PlayerJoined { steam_id } => {
                                guard
                                    .players
                                    .insert(*steam_id, chrono::Local::now());
                                roster_changed = true;
                            }
                            ServerEvent::PlayerLeft { steam_id } => {
                                guard.players.remove(steam_id);
                                roster_changed = true;
                            }
                            ServerEvent::StatusChanged(_) => {
                                roster_changed = true;
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
                            Some(build_player_data(&guard.players, t))
                        } else {
                            None
                        };
                        (st, flags, lang, players_update)
                    };
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
                        if let Some((rows, count_text)) = players_update {
                            install_player_model(&ui, rows, count_text);
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
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// Materialize the player roster into Send-safe data. The caller wraps the
/// Vec in a Slint VecModel/ModelRc inside the UI thread (Rc is !Send).
fn build_player_data(
    players: &HashMap<u64, chrono::DateTime<chrono::Local>>,
    t: &Strings,
) -> (Vec<PlayerRow>, String) {
    let mut entries: Vec<(u64, chrono::DateTime<chrono::Local>)> =
        players.iter().map(|(k, v)| (*k, *v)).collect();
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    let rows: Vec<PlayerRow> = entries
        .into_iter()
        .map(|(steam_id, at)| PlayerRow {
            steam_id: format!("steam:{steam_id}").into(),
            joined_at: at.format("%H:%M:%S").to_string().into(),
        })
        .collect();
    let count_text = translations::fmt_count(t.players_count_fmt, players.len());
    (rows, count_text)
}

fn install_player_model(ui: &MainWindow, rows: Vec<PlayerRow>, count_text: String) {
    let model = std::rc::Rc::new(slint::VecModel::from(rows));
    ui.set_player_rows(slint::ModelRc::from(model));
    ui.set_players_count_text(count_text.into());
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
    if subtab == 1 {
        BackupKind::PreRollback
    } else {
        BackupKind::Manual
    }
}

fn wire_backup_callbacks(
    ui: &MainWindow,
    backup_window: &BackupWindow,
    server: Arc<ValheimServer>,
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
            let (selected_set_after, manual_count, pre_count) = {
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
                let manual_count = count_selected_for(&guard, BackupKind::Manual);
                let pre_count = count_selected_for(&guard, BackupKind::PreRollback);
                let selected = guard.selected_backup_ids.clone();
                (selected, manual_count, pre_count)
            };
            // Rebuild both lists (selection state affects checkbox display).
            let (manual_rows, pre_rollback_rows) = {
                let guard = state.lock().expect("state mutex poisoned");
                (
                    backups_to_rows(&guard.last_backups, BackupKind::Manual, &selected_set_after),
                    backups_to_rows(
                        &guard.last_backups,
                        BackupKind::PreRollback,
                        &selected_set_after,
                    ),
                )
            };
            install_backup_models(&bw, manual_rows, pre_rollback_rows);
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
            let (manual_rows, pre_rollback_rows, sort_col, sort_desc) = {
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
                let manual =
                    backups_to_rows(&guard.last_backups, BackupKind::Manual, &selected);
                let pre =
                    backups_to_rows(&guard.last_backups, BackupKind::PreRollback, &selected);
                (manual, pre, col, desc)
            };
            install_backup_models(&bw, manual_rows, pre_rollback_rows);
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
    manual: Vec<BackupRow>,
    pre_rollback: Vec<BackupRow>,
) {
    let manual_model = std::rc::Rc::new(slint::VecModel::from(manual));
    let pre_rollback_model = std::rc::Rc::new(slint::VecModel::from(pre_rollback));
    bw.set_snapshots_manual(slint::ModelRc::from(manual_model));
    bw.set_snapshots_pre_rollback(slint::ModelRc::from(pre_rollback_model));
}

fn wire_world_callbacks(ui: &MainWindow, world_window: &WorldSettingsWindow) {
    let main_weak = ui.as_weak();
    let world_weak = world_window.as_weak();

    // Main: open world settings — copy live indices into the dialog and show.
    {
        let main_weak = main_weak.clone();
        let world_weak = world_weak.clone();
        ui.on_open_world_clicked(move || {
            let Some(main) = main_weak.upgrade() else { return };
            let Some(world) = world_weak.upgrade() else { return };
            world.set_preset_index(main.get_preset_index());
            world.set_mod_combat_index(main.get_mod_combat_index());
            world.set_mod_deathpenalty_index(main.get_mod_deathpenalty_index());
            world.set_mod_resources_index(main.get_mod_resources_index());
            world.set_mod_raids_index(main.get_mod_raids_index());
            world.set_mod_portals_index(main.get_mod_portals_index());
            world.set_key_nobuildcost(main.get_key_nobuildcost());
            world.set_key_passivemobs(main.get_key_passivemobs());
            world.set_key_nomap(main.get_key_nomap());
            world.set_key_noportals(main.get_key_noportals());
            world.set_key_playerevents(main.get_key_playerevents());
            world.set_key_showenemyhud(main.get_key_showenemyhud());
            world.set_key_devcommands(main.get_key_devcommands());
            let _ = world.show();
        });
    }

    // World: Apply — copy dialog indices back to main, hide.
    {
        let main_weak = main_weak.clone();
        let world_weak = world_weak.clone();
        world_window.on_apply_clicked(move || {
            let Some(main) = main_weak.upgrade() else { return };
            let Some(world) = world_weak.upgrade() else { return };
            main.set_preset_index(world.get_preset_index());
            main.set_mod_combat_index(world.get_mod_combat_index());
            main.set_mod_deathpenalty_index(world.get_mod_deathpenalty_index());
            main.set_mod_resources_index(world.get_mod_resources_index());
            main.set_mod_raids_index(world.get_mod_raids_index());
            main.set_mod_portals_index(world.get_mod_portals_index());
            main.set_key_nobuildcost(world.get_key_nobuildcost());
            main.set_key_passivemobs(world.get_key_passivemobs());
            main.set_key_nomap(world.get_key_nomap());
            main.set_key_noportals(world.get_key_noportals());
            main.set_key_playerevents(world.get_key_playerevents());
            main.set_key_showenemyhud(world.get_key_showenemyhud());
            main.set_key_devcommands(world.get_key_devcommands());
            let _ = world.hide();
        });
    }

    // World: Cancel — just hide (discard the in-flight indices).
    {
        let world_weak = world_weak.clone();
        world_window.on_cancel_clicked(move || {
            if let Some(world) = world_weak.upgrade() {
                let _ = world.hide();
            }
        });
    }
}

fn wire_connection_callbacks(
    ui: &MainWindow,
    state: Arc<Mutex<UiState>>,
    manager_dir: PathBuf,
) {
    // Enter in the LineEdit persists the address to config.toml. No restart
    // needed because the public address is informational only — neither
    // Valheim nor the ValheimServer instance read it.
    {
        let state = state.clone();
        let manager_dir = manager_dir.clone();
        let ui_weak = ui.as_weak();
        ui.on_public_address_accepted(move |value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let trimmed = value.trim().to_string();
            let language = {
                let mut guard = state.lock().expect("state mutex poisoned");
                if let Some(cfg) = guard.config.as_mut() {
                    cfg.manager.public_address = trimmed.clone();
                    let path = manager_dir.join("config.toml");
                    if let Err(e) = cfg.save(&path) {
                        tracing::warn!(error = %e, "failed to persist public address");
                    }
                }
                guard.language
            };
            let t = translations::for_language(language);
            ui.set_public_address(trimmed.into());
            ui.set_public_address_status(t.save_success.into());
            // Clear the status banner after a moment so it doesn't linger.
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_public_address_status("".into());
                }
            });
        });
    }

    // Copy current value to the system clipboard via arboard.
    {
        let state = state.clone();
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
            ui.set_public_address_status(msg.into());
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_public_address_status("".into());
                }
            });
        });
    }
}

async fn refresh_backups_async(
    server: &Arc<ValheimServer>,
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

    let (manual_rows, pre_rollback_rows, manual_count, pre_count, count_text, total) = {
        let mut guard = state.lock().expect("state mutex poisoned");
        // Apply current sort order.
        sort_backups_inplace(&mut backups, guard.backup_sort_column, guard.backup_sort_desc);

        // Drop selections that no longer correspond to live snapshots.
        let live: HashSet<String> = backups.iter().map(|b| b.id.0.clone()).collect();
        guard.selected_backup_ids.retain(|id| live.contains(id));

        guard.last_backups = backups.clone();

        let manual = backups_to_rows(&backups, BackupKind::Manual, &guard.selected_backup_ids);
        let pre = backups_to_rows(
            &backups,
            BackupKind::PreRollback,
            &guard.selected_backup_ids,
        );
        let manual_count = count_selected_for(&guard, BackupKind::Manual);
        let pre_count = count_selected_for(&guard, BackupKind::PreRollback);
        let t = translations::for_language(guard.language);
        let count_text = if backups.is_empty() {
            t.no_backups.to_string()
        } else {
            translations::fmt_count(t.backups_count_fmt, backups.len())
        };
        (manual, pre, manual_count, pre_count, count_text, backups.len())
    };
    let _ = total;

    // Push to BackupWindow.
    {
        let bw = backup_weak.clone();
        let manual = manual_rows;
        let pre = pre_rollback_rows;
        let _ = bw.upgrade_in_event_loop(move |bw| {
            install_backup_models(&bw, manual, pre);
            bw.set_manual_selected_count(manual_count as i32);
            bw.set_pre_rollback_selected_count(pre_count as i32);
        });
    }

    // Push count text to MainWindow.
    {
        let main = main_weak.clone();
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

fn wire_browse_and_save(
    ui: &MainWindow,
    server: Option<Arc<ValheimServer>>,
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
                    "server_dir" => ui.set_server_dir(s.into()),
                    "save_dir" => ui.set_save_dir(s.into()),
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
                    ui.set_error_text(
                        format!("Cannot save while server is {st:?}. Stop it first.").into(),
                    );
                    return;
                }
            }

            let language = state.lock().expect("state mutex poisoned").language;
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
    world_window: &WorldSettingsWindow,
    server: Option<Arc<ValheimServer>>,
    state: Arc<Mutex<UiState>>,
    manager_dir: PathBuf,
) {
    let ui_weak = ui.as_weak();
    let bw_weak = backup_window.as_weak();
    let ww_weak = world_window.as_weak();
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
        if let Some(ww) = ww_weak.upgrade() {
            apply_world_window_strings(&ww, t);
            apply_world_window_models(&ww, t);
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

        // Re-render the player roster so its count label uses the new
        // locale (the row data itself stays the same).
        {
            let guard = state.lock().expect("state mutex poisoned");
            let (rows, count_text) = build_player_data(&guard.players, t);
            install_player_model(&ui, rows, count_text);
        }
        // Same for backups, but the data needs a re-list to be safe.
        if let Some(s) = server.as_ref() {
            let server = s.clone();
            let state_for_refresh = state.clone();
            let main_weak = ui.as_weak();
            let backup_weak = bw_weak.clone();
            tokio::spawn(async move {
                refresh_backups_async(&server, &state_for_refresh, &main_weak, &backup_weak)
                    .await;
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
        ServerEvent::PlayerJoined { steam_id } => Some(format!("[+] steam:{steam_id}")),
        ServerEvent::PlayerLeft { steam_id } => Some(format!("[-] steam:{steam_id}")),
        ServerEvent::ServerReady => Some("[ready] accepting connections".into()),
        ServerEvent::Warning(s) => Some(format!("[warning] {s}")),
        ServerEvent::StatusChanged(s) => {
            Some(format!("[status] {}", translations::status_label(*s, t)))
        }
    }
}
