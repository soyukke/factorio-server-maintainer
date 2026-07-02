//! Factorio implementation of `gsm-core::GameServerManager`.
//!
//! Factorio-specific Steam app id, executable layout, save format, DLC profile,
//! and command-line arguments.

use anyhow::Context;
use async_trait::async_trait;
use gsm_core::{
    logtail, AppConfig, Backup, BackupId, BackupKind, FactorioDlc, GameServerManager,
    LogTailConfig, ServerEvent, ServerProcess, ServerStatus, SpawnRequest,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const STATE_FILE: &str = "factorio-state.toml";
const STARTUP_READY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize)]
struct RunningState {
    pid: u32,
}

#[derive(Serialize)]
struct ModList {
    mods: Vec<ModEntry>,
}

#[derive(Serialize)]
struct ModEntry {
    name: &'static str,
    enabled: bool,
}

/// Steam app id for Factorio. SteamCMD installs the headless-capable binary.
pub const FACTORIO_APP_ID: u32 = 427_520;
/// Windows executable installed by Steam under the Factorio app directory.
pub const SERVER_EXE: &str = "bin/x64/factorio.exe";
pub const CTRLC_HELPER_EXE: &str = "ctrlc-helper.exe";
pub const SERVER_SETTINGS_JSON: &str = "server-settings.json";
pub const MOD_LIST_JSON: &str = "mod-list.json";
pub const SERVER_CONFIG_INI: &str = "config/config.ini";

pub struct FactorioServer {
    config: AppConfig,
    inner: Arc<Mutex<Option<RunningInner>>>,
    status: Arc<Mutex<ServerStatus>>,
    events: broadcast::Sender<ServerEvent>,
    manager_dir: PathBuf,
}

struct RunningInner {
    process: Arc<ServerProcess>,
    tail: JoinHandle<()>,
    pump: JoinHandle<()>,
}

impl RunningInner {
    fn shutdown(self) {
        self.tail.abort();
        self.pump.abort();
    }
}

impl FactorioServer {
    pub fn new(config: AppConfig, manager_dir: PathBuf) -> Self {
        let (events, _) = broadcast::channel(512);
        Self {
            config,
            inner: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(ServerStatus::Stopped)),
            events,
            manager_dir,
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn save_path(&self) -> PathBuf {
        self.config
            .paths
            .save_dir
            .join(format!("{}.zip", self.config.server.world))
    }

    pub fn server_settings_path(&self) -> PathBuf {
        self.config.paths.save_dir.join(SERVER_SETTINGS_JSON)
    }

    pub fn mod_dir(&self) -> PathBuf {
        self.config.paths.server_dir.join("mods")
    }

    pub fn mod_list_path(&self) -> PathBuf {
        self.mod_dir().join(MOD_LIST_JSON)
    }

    pub fn server_config_path(&self) -> PathBuf {
        self.config.paths.server_dir.join(SERVER_CONFIG_INI)
    }

    pub fn write_data_dir(&self) -> PathBuf {
        self.config.paths.server_dir.join("UserData")
    }

    pub fn build_argv(&self) -> Vec<String> {
        let mut argv = vec![
            "--config".into(),
            self.server_config_path().display().to_string(),
            "--start-server".into(),
            self.save_path().display().to_string(),
            "--port".into(),
            self.config.server.port.to_string(),
            "--console-log".into(),
            self.config.paths.log_file.display().to_string(),
            "--mod-directory".into(),
            self.mod_dir().display().to_string(),
        ];

        let settings = self.server_settings_path();
        if settings.is_file() {
            argv.push("--server-settings".into());
            argv.push(settings.display().to_string());
        }

        argv
    }

    fn write_mod_list(&self) -> anyhow::Result<()> {
        let list = mod_list_for(self.config.server.dlc);
        let mod_dir = self.mod_dir();
        std::fs::create_dir_all(&mod_dir)
            .with_context(|| format!("create {}", mod_dir.display()))?;
        let json = serde_json::to_string_pretty(&list).context("serialize Factorio mod-list")?;
        std::fs::write(self.mod_list_path(), json)
            .with_context(|| format!("write {}", self.mod_list_path().display()))?;
        Ok(())
    }

    fn write_server_config_ini(&self) -> anyhow::Result<()> {
        let config_path = self.server_config_path();
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let write_data = self.write_data_dir();
        std::fs::create_dir_all(&write_data)
            .with_context(|| format!("create {}", write_data.display()))?;
        let write_data = write_data.to_string_lossy().replace('\\', "/");
        let config =
            format!("[path]\nread-data=__PATH__executable__/../../data\nwrite-data={write_data}\n");
        std::fs::write(&config_path, config)
            .with_context(|| format!("write {}", config_path.display()))?;
        Ok(())
    }

    fn set_status(&self, s: ServerStatus) {
        *self.status.lock().expect("status mutex poisoned") = s;
        let _ = self.events.send(ServerEvent::StatusChanged(s));
    }

    fn ctrlc_helper_path(&self) -> PathBuf {
        self.manager_dir.join(CTRLC_HELPER_EXE)
    }

    fn state_path(&self) -> PathBuf {
        self.manager_dir.join(STATE_FILE)
    }

    fn write_state(&self, pid: u32) {
        let state = RunningState { pid };
        match toml::to_string(&state) {
            Ok(s) => {
                if let Err(e) = std::fs::write(self.state_path(), s) {
                    tracing::warn!(error = %e, "failed to write factorio state");
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize factorio state"),
        }
    }

    fn cleanup_after_exit(&self) {
        if let Some(running) = self.inner.lock().expect("inner mutex poisoned").take() {
            running.shutdown();
        }
    }

    fn spawn_log_pump(&self, mut rx: tokio::sync::mpsc::Receiver<String>) -> JoinHandle<()> {
        let events = self.events.clone();
        let status = self.status.clone();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                for ev in parse_log_line(&line) {
                    if let ServerEvent::ServerReady = ev {
                        promote_starting_to_running(&status, &events);
                    }
                    let _ = events.send(ev);
                }
            }
        })
    }

    fn spawn_exit_watcher(&self, process: Arc<ServerProcess>, state_path: PathBuf) {
        let status = self.status.clone();
        let events = self.events.clone();
        tokio::task::spawn_blocking(move || {
            let res = process.wait_for_exit_with_timeout(Duration::MAX);
            let mut s = status.lock().expect("status mutex poisoned");
            let next = status_after_exit(res, *s, &events);
            *s = next;
            let _ = events.send(ServerEvent::StatusChanged(next));
            let _ = std::fs::remove_file(&state_path);
        });
    }

    fn spawn_startup_watchdog(&self, process: Arc<ServerProcess>) {
        let status = self.status.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(STARTUP_READY_TIMEOUT).await;
            if !process.is_alive() {
                return;
            }

            let mut s = status.lock().expect("status mutex poisoned");
            if *s == ServerStatus::Starting {
                *s = ServerStatus::Running;
                let _ = events.send(ServerEvent::Warning(
                    "ready log was not seen, but the Factorio process is still running".into(),
                ));
                let _ = events.send(ServerEvent::StatusChanged(ServerStatus::Running));
            }
        });
    }

    fn install_running_inner(
        &self,
        process: Arc<ServerProcess>,
        tail: JoinHandle<()>,
        pump: JoinHandle<()>,
    ) {
        *self.inner.lock().expect("inner mutex poisoned") = Some(RunningInner {
            process,
            tail,
            pump,
        });
    }

    fn fail_start(&self) {
        self.cleanup_after_exit();
        let _ = std::fs::remove_file(self.state_path());
        self.set_status(ServerStatus::Crashed);
    }

    async fn ensure_save_exists(&self) -> anyhow::Result<()> {
        let save = self.save_path();
        if save.is_file() {
            return Ok(());
        }
        if let Some(parent) = save.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }

        let exe = self.config.paths.server_dir.join(SERVER_EXE);
        if !exe.is_file() {
            anyhow::bail!(
                "factorio.exe not found at {}; run Install / Update server first",
                exe.display()
            );
        }
        let save_for_blocking = save.clone();
        let server_dir = self.config.paths.server_dir.clone();
        let code = tokio::task::spawn_blocking(move || {
            std::process::Command::new(exe)
                .arg("--config")
                .arg(server_dir.join(SERVER_CONFIG_INI))
                .arg("--create")
                .arg(&save_for_blocking)
                .current_dir(&server_dir)
                .status()
                .context("create Factorio save")
                .map(|s| s.code().unwrap_or(-1))
        })
        .await??;

        if code != 0 {
            anyhow::bail!("factorio --create exited with code {code}");
        }
        Ok(())
    }

    async fn start_after_status_changed(&self) -> anyhow::Result<()> {
        self.write_mod_list()?;
        self.write_server_config_ini()?;
        self.ensure_save_exists().await?;

        let exe = self.config.paths.server_dir.join(SERVER_EXE);
        let cwd = self.config.paths.server_dir.clone();
        let argv = self.build_argv();
        let req = SpawnRequest::new(exe, argv, cwd);

        if let Some(parent) = self.config.paths.log_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::File::create(&self.config.paths.log_file);

        let process = Arc::new(ServerProcess::spawn(&req)?);
        self.write_state(process.pid());

        let (rx, tail_handle) =
            logtail::spawn(LogTailConfig::new(self.config.paths.log_file.clone()));
        let pump_handle = self.spawn_log_pump(rx);
        self.spawn_exit_watcher(process.clone(), self.state_path());
        self.spawn_startup_watchdog(process.clone());
        self.install_running_inner(process, tail_handle, pump_handle);
        Ok(())
    }

    pub async fn backup_with_kind(&self, kind: BackupKind) -> anyhow::Result<Backup> {
        if matches!(
            self.status(),
            ServerStatus::Starting | ServerStatus::Stopping | ServerStatus::Updating
        ) {
            anyhow::bail!("server is in a transitional state; wait or stop first");
        }

        let world = &self.config.server.world;
        let save_src = self.save_path();
        if !save_src.is_file() {
            anyhow::bail!("save file not found at {}", save_src.display());
        }

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
        let dir_name = format!("{timestamp}{}", kind.dir_suffix());
        let target_dir = self.config.paths.backup_dir.join(world).join(&dir_name);
        std::fs::create_dir_all(&target_dir)
            .with_context(|| format!("create {}", target_dir.display()))?;

        let save_target = target_dir.join(format!("{world}.zip"));
        std::fs::copy(&save_src, &save_target)
            .with_context(|| format!("copy {} -> {}", save_src.display(), save_target.display()))?;

        let backup = Backup {
            id: BackupId(target_dir.to_string_lossy().to_string()),
            world: world.clone(),
            created_at: chrono::Local::now(),
            dir: target_dir,
            size_bytes: std::fs::metadata(&save_target)?.len(),
            kind,
        };

        let _ = self.events.send(ServerEvent::Log(format!(
            "[backup] saved {} ({} bytes)",
            backup.id.0, backup.size_bytes
        )));
        Ok(backup)
    }

    pub async fn try_reattach(&self) -> anyhow::Result<bool> {
        if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            return Ok(false);
        }

        let state_path = self.state_path();
        if !state_path.exists() {
            return Ok(false);
        }

        let state: RunningState = match std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
        {
            Some(s) => s,
            None => {
                let _ = std::fs::remove_file(&state_path);
                return Ok(false);
            }
        };

        let process = match ServerProcess::open_existing(state.pid) {
            Ok(p) if p.is_alive() => Arc::new(p),
            _ => {
                let _ = std::fs::remove_file(&state_path);
                return Ok(false);
            }
        };

        let log_start_pos = std::fs::metadata(&self.config.paths.log_file)
            .map(|m| m.len())
            .unwrap_or(0);
        let (rx, tail_handle) = logtail::spawn(LogTailConfig {
            path: self.config.paths.log_file.clone(),
            poll_interval: Duration::from_millis(250),
            channel_capacity: 1024,
            start_pos: log_start_pos,
        });

        let pump_handle = self.spawn_log_pump(rx);
        self.spawn_exit_watcher(process.clone(), state_path);
        self.install_running_inner(process, tail_handle, pump_handle);
        *self.status.lock().expect("status mutex poisoned") = ServerStatus::Running;
        let _ = self
            .events
            .send(ServerEvent::StatusChanged(ServerStatus::Running));
        let _ = self.events.send(ServerEvent::Log(format!(
            "[reattach] resumed factorio.exe pid={}",
            state.pid
        )));
        Ok(true)
    }

    /// Remove a snapshot directory entirely. The id is the absolute path
    /// emitted by `list_backups`, so verify it is under backup_dir first.
    pub async fn delete_backup(&self, id: BackupId) -> anyhow::Result<()> {
        let dir = PathBuf::from(&id.0);
        if !dir.is_dir() {
            anyhow::bail!("backup not found: {}", dir.display());
        }
        if !dir.starts_with(&self.config.paths.backup_dir) {
            anyhow::bail!(
                "refusing to delete {} (outside backup_dir {})",
                dir.display(),
                self.config.paths.backup_dir.display()
            );
        }
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove_dir_all {}", dir.display()))?;
        let _ = self.events.send(ServerEvent::Log(format!(
            "[backup] deleted {}",
            dir.display()
        )));
        Ok(())
    }
}

fn mod_list_for(dlc: FactorioDlc) -> ModList {
    let dlc_enabled = matches!(dlc, FactorioDlc::SpaceAge);
    ModList {
        mods: vec![
            ModEntry {
                name: "base",
                enabled: true,
            },
            ModEntry {
                name: "elevated-rails",
                enabled: dlc_enabled,
            },
            ModEntry {
                name: "quality",
                enabled: dlc_enabled,
            },
            ModEntry {
                name: "space-age",
                enabled: dlc_enabled,
            },
        ],
    }
}

fn promote_starting_to_running(
    status: &Arc<Mutex<ServerStatus>>,
    events: &broadcast::Sender<ServerEvent>,
) {
    let mut s = status.lock().expect("status mutex poisoned");
    if *s == ServerStatus::Starting {
        *s = ServerStatus::Running;
        let _ = events.send(ServerEvent::StatusChanged(ServerStatus::Running));
    }
}

fn status_after_exit(
    res: anyhow::Result<Option<u32>>,
    current: ServerStatus,
    events: &broadcast::Sender<ServerEvent>,
) -> ServerStatus {
    match res {
        Ok(Some(code)) if code == 0 || current == ServerStatus::Stopping => ServerStatus::Stopped,
        Ok(Some(_)) | Ok(None) => ServerStatus::Crashed,
        Err(e) => {
            let _ = events.send(ServerEvent::Warning(format!("wait_for_exit failed: {e:#}")));
            ServerStatus::Crashed
        }
    }
}

fn steam_username(config: &AppConfig) -> Option<String> {
    let username = config.manager.steam_username.trim();
    (!username.is_empty()).then(|| username.to_string())
}

fn steam_login_label(config: &AppConfig) -> String {
    steam_username(config).unwrap_or_else(|| "anonymous".to_string())
}

#[async_trait]
impl GameServerManager for FactorioServer {
    fn id(&self) -> &str {
        "factorio"
    }

    async fn install_or_update(&self) -> anyhow::Result<()> {
        if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            anyhow::bail!("server is running or busy; stop it before updating");
        }
        self.set_status(ServerStatus::Updating);

        std::fs::create_dir_all(&self.config.paths.server_dir)
            .with_context(|| format!("create {}", self.config.paths.server_dir.display()))?;

        let job = gsm_core::SteamCmdJob {
            steamcmd_exe: self.config.paths.steamcmd.clone(),
            install_dir: self.config.paths.server_dir.clone(),
            app_id: FACTORIO_APP_ID,
            username: steam_username(&self.config),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
        let events = self.events.clone();
        let pump = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let _ = events.send(ServerEvent::Log(line));
            }
        });

        if let Err(e) =
            gsm_core::steamcmd::ensure_installed(&self.config.paths.steamcmd, tx.clone()).await
        {
            let _ = self
                .events
                .send(ServerEvent::Warning(format!("ensure steamcmd: {e:#}")));
            drop(tx);
            let _ = pump.await;
            self.set_status(ServerStatus::Stopped);
            return Err(e);
        }

        let _ = self.events.send(ServerEvent::Log(format!(
            "[update] starting steamcmd as {} +app_update {FACTORIO_APP_ID} validate",
            steam_login_label(&self.config)
        )));

        let result = gsm_core::steamcmd::run(&job, tx.clone()).await;
        drop(tx);
        let _ = pump.await;
        self.set_status(ServerStatus::Stopped);

        match result {
            Ok(0) => Ok(()),
            Ok(code) if steam_username(&self.config).is_none() => anyhow::bail!(
                "steamcmd exited with code {code}; anonymous install failed. \
                 If SteamCMD reports a login or subscription error, enter your Steam username \
                 in the setup section, save, and try Install / Update again."
            ),
            Ok(code) => anyhow::bail!("steamcmd exited with code {code}"),
            Err(e) => Err(e),
        }
    }

    async fn start(&self) -> anyhow::Result<()> {
        if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            anyhow::bail!("server is already running or transitioning");
        }
        if let Some(prev) = self.inner.lock().expect("inner mutex poisoned").take() {
            prev.shutdown();
        }
        self.set_status(ServerStatus::Starting);
        let result = self.start_after_status_changed().await;
        if result.is_err() {
            self.fail_start();
        }
        result
    }

    async fn stop(&self, graceful: bool) -> anyhow::Result<()> {
        let process = match self.inner.lock().expect("inner mutex poisoned").as_ref() {
            Some(r) => r.process.clone(),
            None => return Ok(()),
        };
        self.set_status(ServerStatus::Stopping);

        if graceful {
            let helper = self.ctrlc_helper_path();
            let pid = process.pid();
            let helper_for_blocking = helper.clone();
            let helper_result: anyhow::Result<i32> = tokio::task::spawn_blocking(move || {
                gsm_core::process::run_helper_blocking(&helper_for_blocking, &[pid.to_string()])
            })
            .await?;
            if let Err(e) = helper_result {
                let _ = self.events.send(ServerEvent::Warning(format!(
                    "failed to invoke ctrlc-helper at {}: {e:#}",
                    helper.display()
                )));
            }

            let timeout =
                Duration::from_secs(self.config.manager.graceful_stop_timeout_secs as u64);
            let waiter = process.clone();
            let waited =
                tokio::task::spawn_blocking(move || waiter.wait_for_exit_with_timeout(timeout))
                    .await?;
            if matches!(waited, Ok(Some(_))) {
                self.cleanup_after_exit();
                return Ok(());
            }
            let _ = self.events.send(ServerEvent::Warning(
                "graceful stop timed out; falling back to TerminateProcess.".into(),
            ));
        }

        process.terminate()?;
        let waiter = process.clone();
        let _ = tokio::task::spawn_blocking(move || {
            waiter.wait_for_exit_with_timeout(Duration::from_secs(5))
        })
        .await?;
        self.cleanup_after_exit();
        Ok(())
    }

    fn status(&self) -> ServerStatus {
        *self.status.lock().expect("status mutex poisoned")
    }

    fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    async fn list_backups(&self) -> anyhow::Result<Vec<Backup>> {
        let world = &self.config.server.world;
        let dir = self.config.paths.backup_dir.join(world);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let save = path.join(format!("{world}.zip"));
            if !save.is_file() {
                continue;
            }
            let meta = std::fs::metadata(&save)?;
            let created_at: chrono::DateTime<chrono::Local> = meta
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Local>::from)
                .unwrap_or_else(chrono::Local::now);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(Backup {
                id: BackupId(path.to_string_lossy().to_string()),
                world: world.clone(),
                created_at,
                dir: path,
                size_bytes: meta.len(),
                kind: BackupKind::from_dir_name(&name),
            });
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(out)
    }

    async fn backup(&self) -> anyhow::Result<Backup> {
        self.backup_with_kind(BackupKind::Manual).await
    }

    async fn rollback(&self, id: BackupId) -> anyhow::Result<()> {
        let snapshot_dir = PathBuf::from(&id.0);
        if !snapshot_dir.is_dir() {
            anyhow::bail!("backup directory not found: {}", snapshot_dir.display());
        }

        let world = &self.config.server.world;
        let save_src = snapshot_dir.join(format!("{world}.zip"));
        if !save_src.is_file() {
            anyhow::bail!("snapshot is missing {}", save_src.display());
        }

        let was_running = matches!(self.status(), ServerStatus::Running);
        if was_running {
            self.stop(true).await?;
        } else if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            anyhow::bail!("server is in a transitional state; wait or stop first");
        }

        if self.save_path().is_file() {
            if let Err(e) = self.backup_with_kind(BackupKind::PreRollback).await {
                let _ = self.events.send(ServerEvent::Warning(format!(
                    "pre-rollback snapshot failed: {e:#}; proceeding anyway"
                )));
            }
        }

        if let Some(parent) = self.save_path().parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::copy(&save_src, self.save_path()).with_context(|| {
            format!(
                "copy {} -> {}",
                save_src.display(),
                self.save_path().display()
            )
        })?;

        if was_running {
            self.start().await?;
        }
        Ok(())
    }
}

fn parse_log_line(line: &str) -> Vec<ServerEvent> {
    let mut out = vec![ServerEvent::Log(line.to_string())];

    if line.contains("Hosting game at IP ADDR")
        || line.contains("Hosting game at")
        || line.contains("changing state from(CreatingGame) to(InGame)")
    {
        out.push(ServerEvent::ServerReady);
    }
    if line.contains("Saving finished") || line.contains("Autosaving finished") {
        out.push(ServerEvent::WorldSaved {
            at: chrono::Local::now(),
        });
    }
    if let Some(name) = parse_join_name(line) {
        out.push(ServerEvent::Log(format!("[+] {name}")));
    }
    if let Some(name) = parse_left_name(line) {
        out.push(ServerEvent::Log(format!("[-] {name}")));
    }

    out
}

fn parse_join_name(line: &str) -> Option<&str> {
    line.split(" joined the game")
        .next()
        .filter(|s| s.len() != line.len())
}

fn parse_left_name(line: &str) -> Option<&str> {
    line.split(" left the game")
        .next()
        .filter(|s| s.len() != line.len())
}

#[allow(dead_code)]
fn is_under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ready_line() {
        let events = parse_log_line(
            "Info ServerMultiplayerManager.cpp:776: Hosting game at IP ADDR:({0.0.0.0:34197})",
        );
        assert!(events.iter().any(|e| matches!(e, ServerEvent::ServerReady)));
    }

    #[test]
    fn parse_ingame_ready_line() {
        let events = parse_log_line(
            "Info ServerMultiplayerManager.cpp:808: updateTick(0) changing state \
             from(CreatingGame) to(InGame)",
        );
        assert!(events.iter().any(|e| matches!(e, ServerEvent::ServerReady)));
    }

    #[test]
    fn parse_save_line() {
        let events = parse_log_line("Info AppManager.cpp:306: Saving finished");
        assert!(events
            .iter()
            .any(|e| matches!(e, ServerEvent::WorldSaved { .. })));
    }

    #[test]
    fn build_player_log_events() {
        let events = parse_log_line("alice joined the game");
        assert!(events
            .iter()
            .any(|e| matches!(e, ServerEvent::Log(s) if s == "[+] alice")));
    }

    #[test]
    fn argv_uses_managed_mod_directory() {
        let server = FactorioServer::new(test_config(), PathBuf::from("C:\\Manager"));
        let argv = server.build_argv();
        assert!(argv
            .windows(2)
            .any(|w| w == ["--mod-directory", "C:\\Factorio\\Server\\mods"]));
    }

    #[test]
    fn argv_uses_dedicated_server_config() {
        let server = FactorioServer::new(test_config(), PathBuf::from("C:\\Manager"));
        let config_path = server.server_config_path().display().to_string();
        let argv = server.build_argv();
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--config" && w[1] == config_path));
    }

    #[test]
    fn mod_list_can_enable_space_age_bundle() {
        let list = mod_list_for(FactorioDlc::SpaceAge);
        let json = serde_json::to_string(&list).expect("serialize mod list");
        assert!(json.contains(r#""name":"base","enabled":true"#));
        assert!(json.contains(r#""name":"elevated-rails","enabled":true"#));
        assert!(json.contains(r#""name":"quality","enabled":true"#));
        assert!(json.contains(r#""name":"space-age","enabled":true"#));
    }

    fn test_config() -> AppConfig {
        AppConfig {
            paths: gsm_core::PathsConfig {
                steamcmd: PathBuf::from("C:\\Factorio\\SteamCMD\\steamcmd.exe"),
                server_dir: PathBuf::from("C:\\Factorio\\Server"),
                save_dir: PathBuf::from("C:\\Factorio\\Saves"),
                backup_dir: PathBuf::from("C:\\Factorio\\Backups"),
                log_file: PathBuf::from("C:\\Factorio\\Server\\logs\\server.log"),
            },
            server: gsm_core::ServerConfig {
                password: "secret1".into(),
                ..gsm_core::ServerConfig::default()
            },
            manager: gsm_core::ManagerConfig::default(),
        }
    }
}
