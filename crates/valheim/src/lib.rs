//! Valheim implementation of `gsm-core::GameServerManager`.
//!
//! Per spec §3 this crate is intentionally thin: parameter defaults, argv
//! layout (§6.2), and log-line parsing (§6.5). All process/Ctrl+C/tail/backup
//! mechanics live in `gsm-core`.

use anyhow::Context;
use async_trait::async_trait;
use gsm_core::{
    logtail, AppConfig, Backup, BackupId, BackupKind, GameServerManager, LogTailConfig,
    ServerEvent, ServerProcess, ServerStatus, SpawnRequest,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Per-launch state file written by `start()` and read by `try_reattach()`
/// to let a fresh GUI find a still-running Valheim server.
const STATE_FILE: &str = "state.toml";

#[derive(Serialize, Deserialize)]
struct RunningState {
    pid: u32,
}

/// SteamCMD app id for the Valheim dedicated server.
pub const VALHEIM_APP_ID: u32 = 896_660;
/// Name of the dedicated server binary inside `paths.server_dir`.
pub const SERVER_EXE: &str = "valheim_server.exe";
/// Name of the Ctrl+C helper used for graceful shutdown.
pub const CTRLC_HELPER_EXE: &str = "ctrlc-helper.exe";

pub struct ValheimServer {
    config: AppConfig,
    inner: Arc<Mutex<Option<RunningInner>>>,
    status: Arc<Mutex<ServerStatus>>,
    events: broadcast::Sender<ServerEvent>,
    /// Absolute path to the directory holding both the manager exe and
    /// `ctrlc-helper.exe` — spec §2 fixes this to `D:\Valheim\Manager\`.
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

impl ValheimServer {
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

    /// Build argv exactly as spec §6.2 prescribes (without the exe itself).
    pub fn build_argv(&self) -> Vec<String> {
        let s = &self.config.server;
        let p = &self.config.paths;
        let mut argv = vec![
            "-nographics".into(),
            "-batchmode".into(),
            "-name".into(),
            s.name.clone(),
            "-port".into(),
            s.port.to_string(),
            "-world".into(),
            s.world.clone(),
            "-password".into(),
            s.password.clone(),
            "-public".into(),
            s.public.to_string(),
            "-savedir".into(),
            p.save_dir.display().to_string(),
            "-saveinterval".into(),
            s.save_interval.to_string(),
            "-backups".into(),
            s.backups.to_string(),
            "-logFile".into(),
            p.log_file.display().to_string(),
        ];

        // World modifiers: only emit when non-default to keep the argv
        // small and predictable. Empty / "default" values mean "let Valheim
        // pick its baseline".
        for (name, value) in [
            ("combat", &s.mod_combat),
            ("deathpenalty", &s.mod_deathpenalty),
            ("resources", &s.mod_resources),
            ("raids", &s.mod_raids),
            ("portals", &s.mod_portals),
        ] {
            if !value.is_empty() && value != "default" {
                argv.push("-modifier".into());
                argv.push(name.into());
                argv.push(value.clone());
            }
        }
        if !s.preset.is_empty() && s.preset != "default" {
            argv.push("-preset".into());
            argv.push(s.preset.clone());
        }

        // World keys: emit each as -setkey <name>. Only known boolean
        // toggles are typed by the GUI; arbitrary strings in config.toml
        // pass through verbatim.
        for key in &s.world_keys {
            if !key.is_empty() {
                argv.push("-setkey".into());
                argv.push(key.clone());
            }
        }

        // Valheim's internal short / long cycle backups (separate from our
        // own snapshots). Emit only when the user customised the value.
        if self.config.manager.backup_short_secs != 7200 {
            argv.push("-backupshort".into());
            argv.push(self.config.manager.backup_short_secs.to_string());
        }
        if self.config.manager.backup_long_secs != 43200 {
            argv.push("-backuplong".into());
            argv.push(self.config.manager.backup_long_secs.to_string());
        }

        argv
    }

    fn set_status(&self, s: ServerStatus) {
        *self.status.lock().expect("status mutex poisoned") = s;
        let _ = self.events.send(ServerEvent::StatusChanged(s));
    }

    fn ctrlc_helper_path(&self) -> PathBuf {
        self.manager_dir.join(CTRLC_HELPER_EXE)
    }
}

#[async_trait]
impl GameServerManager for ValheimServer {
    fn id(&self) -> &str {
        "valheim"
    }

    async fn install_or_update(&self) -> anyhow::Result<()> {
        if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            anyhow::bail!("server is running or busy; stop it before updating");
        }
        self.set_status(ServerStatus::Updating);

        // Skip auto-backup on first install (no world data yet). Otherwise
        // attempt and continue with a warning on failure — backup is M3 and
        // currently always errors, but we want install_or_update to keep
        // working regardless.
        let world_db = self
            .config
            .paths
            .save_dir
            .join("worlds_local")
            .join(format!("{}.db", self.config.server.world));
        if self.config.manager.auto_backup_before_update && world_db.exists() {
            if let Err(e) = self.backup().await {
                let _ = self.events.send(ServerEvent::Warning(format!(
                    "auto-backup before update failed: {e}"
                )));
            }
        }

        // Make sure server_dir exists so SteamCMD can install into it.
        if let Err(e) = std::fs::create_dir_all(&self.config.paths.server_dir) {
            let msg = format!(
                "failed to create server dir {}: {e}",
                self.config.paths.server_dir.display()
            );
            let _ = self.events.send(ServerEvent::Warning(msg.clone()));
            self.set_status(ServerStatus::Stopped);
            return Err(anyhow::anyhow!(msg));
        }

        let job = gsm_core::SteamCmdJob {
            steamcmd_exe: self.config.paths.steamcmd.clone(),
            install_dir: self.config.paths.server_dir.clone(),
            app_id: VALHEIM_APP_ID,
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
        let events = self.events.clone();
        let pump = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let _ = events.send(ServerEvent::Log(line));
            }
        });

        // Bootstrap steamcmd if the configured exe doesn't exist yet.
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
            "[update] starting steamcmd +app_update {VALHEIM_APP_ID} validate"
        )));

        // SteamCMD's Windows bootstrap performs its own self-update on first
        // invocation and exits without re-running the requested command
        // (commonly exit code 7). Re-running with the same argv after the
        // self-update lets the actual app_update proceed. 3 attempts cover
        // the observed two-stage self-update plus a real install.
        const MAX_ATTEMPTS: u32 = 3;
        let mut result: anyhow::Result<i32> = Ok(-1);
        for attempt in 1..=MAX_ATTEMPTS {
            if attempt > 1 {
                let _ = self.events.send(ServerEvent::Log(format!(
                    "[update] retrying steamcmd (attempt {attempt} of {MAX_ATTEMPTS})"
                )));
            }
            result = gsm_core::steamcmd::run(&job, tx.clone()).await;
            match &result {
                Ok(0) => break,
                Ok(_) => continue,
                Err(_) => break, // spawn/wait error — retrying won't help
            }
        }
        drop(tx);
        let _ = pump.await;

        self.set_status(ServerStatus::Stopped);

        match result {
            Ok(0) => {
                let _ = self.events.send(ServerEvent::Log("[update] complete".into()));
                Ok(())
            }
            Ok(code) => {
                let _ = self
                    .events
                    .send(ServerEvent::Warning(format!("steamcmd exit code {code}")));
                anyhow::bail!("steamcmd exited with code {code}")
            }
            Err(e) => {
                let _ = self
                    .events
                    .send(ServerEvent::Warning(format!("steamcmd failed: {e:#}")));
                Err(e)
            }
        }
    }

    async fn start(&self) -> anyhow::Result<()> {
        if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            anyhow::bail!("server is already running or transitioning");
        }
        // Tear down any leftover tail/pump tasks from a prior run that crashed
        // without anyone observing it. Safe to call when inner is already None.
        if let Some(prev) = self.inner.lock().expect("inner mutex poisoned").take() {
            prev.shutdown();
        }
        self.set_status(ServerStatus::Starting);

        let exe = self.config.paths.server_dir.join(SERVER_EXE);
        let cwd = self.config.paths.server_dir.clone();
        let argv = self.build_argv();
        let req = SpawnRequest::new(exe, argv, cwd);

        // Truncate the log file so we tail only this run. Otherwise we'd
        // replay old `Got connection` lines and falsely fire ServerReady on
        // an unrelated past event.
        if let Some(parent) = self.config.paths.log_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::File::create(&self.config.paths.log_file);

        let process = match ServerProcess::spawn(&req) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                self.set_status(ServerStatus::Crashed);
                return Err(e);
            }
        };

        // Persist PID so a relaunched GUI can re-attach (see try_reattach).
        self.write_state(process.pid());

        let (mut rx, tail_handle) =
            logtail::spawn(LogTailConfig::new(self.config.paths.log_file.clone()));

        // Pump log lines → ServerEvent. Owns the rx so when tail is aborted,
        // the channel closes and this task exits cleanly.
        let pump_handle = {
            let events = self.events.clone();
            let status = self.status.clone();
            tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    for ev in parse_log_line(&line) {
                        if let ServerEvent::ServerReady = ev {
                            let mut s = status.lock().expect("status mutex poisoned");
                            if *s == ServerStatus::Starting {
                                *s = ServerStatus::Running;
                                let _ = events.send(ServerEvent::StatusChanged(
                                    ServerStatus::Running,
                                ));
                            }
                        }
                        let _ = events.send(ev);
                    }
                }
            })
        };

        // Watch process exit. Only mutates status + emits events — never
        // touches `inner` (avoids a race with this very function still
        // populating it on the parent task).
        {
            let process = process.clone();
            let status = self.status.clone();
            let events = self.events.clone();
            let state_path = self.state_path();
            tokio::task::spawn_blocking(move || {
                // Duration::MAX hits the wrapper's u32::MAX clamp, which is
                // the Win32 INFINITE sentinel for WaitForSingleObject.
                let res = process.wait_for_exit_with_timeout(Duration::MAX);
                let mut s = status.lock().expect("status mutex poisoned");
                let next = match res {
                    Ok(Some(code)) if code == 0 || *s == ServerStatus::Stopping => {
                        ServerStatus::Stopped
                    }
                    Ok(Some(_)) => ServerStatus::Crashed,
                    Ok(None) => {
                        // Should not happen: INFINITE wait should never time out.
                        ServerStatus::Crashed
                    }
                    Err(e) => {
                        let _ = events.send(ServerEvent::Warning(format!(
                            "wait_for_exit failed: {e:#}"
                        )));
                        ServerStatus::Crashed
                    }
                };
                *s = next;
                let _ = events.send(ServerEvent::StatusChanged(next));
                // The process is gone; remove the state file so the next
                // GUI launch doesn't try to re-attach to a stale PID.
                let _ = std::fs::remove_file(&state_path);
            });
        }

        *self.inner.lock().expect("inner mutex poisoned") = Some(RunningInner {
            process,
            tail: tail_handle,
            pump: pump_handle,
        });

        Ok(())
    }

    async fn stop(&self, graceful: bool) -> anyhow::Result<()> {
        let process = match self.inner.lock().expect("inner mutex poisoned").as_ref() {
            Some(r) => r.process.clone(),
            None => return Ok(()), // already stopped
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
            match helper_result {
                Ok(0) => {}
                Ok(code) => {
                    let _ = self.events.send(ServerEvent::Warning(format!(
                        "ctrlc-helper exited with code {code}"
                    )));
                }
                Err(e) => {
                    let _ = self.events.send(ServerEvent::Warning(format!(
                        "failed to invoke ctrlc-helper at {}: {e:#}",
                        helper.display()
                    )));
                }
            }

            // Wait up to graceful_stop_timeout_secs for the child to exit.
            let timeout =
                Duration::from_secs(self.config.manager.graceful_stop_timeout_secs as u64);
            let waiter = process.clone();
            let waited = tokio::task::spawn_blocking(move || {
                waiter.wait_for_exit_with_timeout(timeout)
            })
            .await?;
            match waited {
                Ok(Some(_)) => {
                    self.cleanup_after_exit();
                    return Ok(());
                }
                Ok(None) => {
                    let _ = self.events.send(ServerEvent::Warning(
                        "graceful stop timed out; falling back to TerminateProcess. \
                         Data since the last autosave may be lost."
                            .into(),
                    ));
                }
                Err(e) => {
                    let _ = self.events.send(ServerEvent::Warning(format!(
                        "wait after Ctrl+C failed: {e:#}"
                    )));
                }
            }
        }

        // Fallback (or non-graceful) hard kill.
        if let Err(e) = process.terminate() {
            let _ = self
                .events
                .send(ServerEvent::Warning(format!("TerminateProcess failed: {e:#}")));
            return Err(e);
        }

        // Wait briefly for the watcher to observe exit and update status,
        // then tear down tail/pump.
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
            let db = path.join(format!("{}.db", world));
            let fwl = path.join(format!("{}.fwl", world));
            if !db.is_file() || !fwl.is_file() {
                continue; // skip half-written snapshots
            }
            let db_meta = match std::fs::metadata(&db) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let fwl_meta = match std::fs::metadata(&fwl) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size_bytes = db_meta.len() + fwl_meta.len();
            let created_at: chrono::DateTime<chrono::Local> = db_meta
                .modified()
                .ok()
                .map(|st| chrono::DateTime::<chrono::Local>::from(st))
                .unwrap_or_else(chrono::Local::now);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let kind = BackupKind::from_dir_name(&name);

            out.push(Backup {
                id: BackupId(path.to_string_lossy().to_string()),
                world: world.clone(),
                created_at,
                dir: path,
                size_bytes,
                kind,
            });
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    async fn backup(&self) -> anyhow::Result<Backup> {
        self.backup_with_kind(BackupKind::Manual).await
    }

    async fn rollback(&self, id: BackupId) -> anyhow::Result<()> {
        let snapshot_dir = std::path::PathBuf::from(&id.0);
        if !snapshot_dir.is_dir() {
            anyhow::bail!("backup directory not found: {}", snapshot_dir.display());
        }
        let world = &self.config.server.world;
        let db_src = snapshot_dir.join(format!("{}.db", world));
        let fwl_src = snapshot_dir.join(format!("{}.fwl", world));
        if !db_src.is_file() || !fwl_src.is_file() {
            anyhow::bail!(
                "snapshot at {} is missing the {}.db / {}.fwl pair",
                snapshot_dir.display(),
                world,
                world
            );
        }

        // Stop if running; remember so we can re-launch afterward.
        let was_running = matches!(self.status(), ServerStatus::Running);
        if was_running {
            self.stop(true).await?;
        } else if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            anyhow::bail!("server is in a transitional state; wait or stop first");
        }

        // Safety net: snapshot the *current* world state under the
        // pre_rollback kind so the user can revert this rollback if they
        // realise they picked the wrong snapshot. Skip silently if the
        // current world doesn't exist yet (e.g. first install).
        let world_dir = self.config.paths.save_dir.join("worlds_local");
        let live_db = world_dir.join(format!("{}.db", world));
        if live_db.exists() {
            if let Err(e) = self.backup_with_kind(BackupKind::PreRollback).await {
                let _ = self.events.send(ServerEvent::Warning(format!(
                    "pre-rollback snapshot failed: {e:#}; proceeding anyway"
                )));
            }
        }

        let _ = self.events.send(ServerEvent::Log(format!(
            "[rollback] restoring from {}",
            snapshot_dir.display()
        )));

        std::fs::create_dir_all(&world_dir)
            .with_context(|| format!("create {}", world_dir.display()))?;

        let db_target = world_dir.join(format!("{}.db", world));
        let fwl_target = world_dir.join(format!("{}.fwl", world));
        std::fs::copy(&fwl_src, &fwl_target).with_context(|| {
            format!("copy {} -> {}", fwl_src.display(), fwl_target.display())
        })?;
        std::fs::copy(&db_src, &db_target).with_context(|| {
            format!("copy {} -> {}", db_src.display(), db_target.display())
        })?;

        let _ = self
            .events
            .send(ServerEvent::Log("[rollback] restore complete".into()));

        if was_running {
            self.start().await?;
        }
        Ok(())
    }
}

impl ValheimServer {
    fn cleanup_after_exit(&self) {
        if let Some(running) = self.inner.lock().expect("inner mutex poisoned").take() {
            running.shutdown();
        }
    }

    /// Copy the live world's `.db` + `.fwl` to `<backup_dir>/<world>/
    /// <timestamp>_<kind_suffix>/`. Used by `backup()` (Manual) and
    /// `rollback()` (PreRollback safety net).
    pub async fn backup_with_kind(&self, kind: BackupKind) -> anyhow::Result<Backup> {
        if matches!(
            self.status(),
            ServerStatus::Starting | ServerStatus::Stopping | ServerStatus::Updating
        ) {
            anyhow::bail!("server is in a transitional state; wait or stop first");
        }

        let world = &self.config.server.world;
        let world_dir = self.config.paths.save_dir.join("worlds_local");
        let db_src = world_dir.join(format!("{}.db", world));
        let fwl_src = world_dir.join(format!("{}.fwl", world));
        if !db_src.is_file() || !fwl_src.is_file() {
            anyhow::bail!(
                "world files not found at {} / {}",
                db_src.display(),
                fwl_src.display()
            );
        }

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
        let dir_name = format!("{timestamp}{}", kind.dir_suffix());
        let target_dir = self
            .config
            .paths
            .backup_dir
            .join(world)
            .join(&dir_name);
        std::fs::create_dir_all(&target_dir)
            .with_context(|| format!("create {}", target_dir.display()))?;

        let db_target = target_dir.join(format!("{}.db", world));
        let fwl_target = target_dir.join(format!("{}.fwl", world));

        std::fs::copy(&fwl_src, &fwl_target)
            .with_context(|| format!("copy {} -> {}", fwl_src.display(), fwl_target.display()))?;
        std::fs::copy(&db_src, &db_target)
            .with_context(|| format!("copy {} -> {}", db_src.display(), db_target.display()))?;

        let size_bytes = std::fs::metadata(&db_target)?.len()
            + std::fs::metadata(&fwl_target)?.len();
        let backup = Backup {
            id: BackupId(target_dir.to_string_lossy().to_string()),
            world: world.clone(),
            created_at: chrono::Local::now(),
            dir: target_dir,
            size_bytes,
            kind,
        };

        let kind_label = match kind {
            BackupKind::Auto => "auto",
            BackupKind::Manual => "manual",
            BackupKind::PreRollback => "pre_rollback",
        };
        let _ = self.events.send(ServerEvent::Log(format!(
            "[backup] saved {kind_label} {} ({} bytes)",
            backup.id.0, backup.size_bytes
        )));
        Ok(backup)
    }

    /// Remove a snapshot directory entirely. The id is the absolute path
    /// emitted by `list_backups`, so this is a `remove_dir_all` on that
    /// path after verifying it's under the configured backup_dir.
    pub async fn delete_backup(&self, id: BackupId) -> anyhow::Result<()> {
        let dir = std::path::PathBuf::from(&id.0);
        if !dir.is_dir() {
            anyhow::bail!("backup not found: {}", dir.display());
        }
        // Safety: refuse to delete anything outside the configured
        // backup_dir so a stale BackupId can't reach into worlds_local.
        if !dir.starts_with(&self.config.paths.backup_dir) {
            anyhow::bail!(
                "refusing to delete {} (outside backup_dir {})",
                dir.display(),
                self.config.paths.backup_dir.display()
            );
        }
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove_dir_all {}", dir.display()))?;
        let _ = self
            .events
            .send(ServerEvent::Log(format!("[backup] deleted {}", dir.display())));
        Ok(())
    }

    fn state_path(&self) -> PathBuf {
        self.manager_dir.join(STATE_FILE)
    }

    fn write_state(&self, pid: u32) {
        let state = RunningState { pid };
        match toml::to_string(&state) {
            Ok(s) => {
                if let Err(e) = std::fs::write(self.state_path(), s) {
                    tracing::warn!(error = %e, "failed to write state.toml");
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize running state"),
        }
    }

    /// Attempt to re-attach to a Valheim server left running by a previous
    /// GUI session. Returns `Ok(true)` when a live process was adopted.
    ///
    /// Behaviour:
    /// - No `state.toml` → returns Ok(false) silently.
    /// - `state.toml` present but PID is dead → removes the file, returns Ok(false).
    /// - PID alive → builds a `ServerProcess` from `OpenProcess`, tails the
    ///   log file from its current end (so we don't replay history), spawns
    ///   the same pump + watcher tasks as `start()`, and flips status to
    ///   Running. A `[reattach]` event is emitted so the GUI log makes the
    ///   transition observable.
    pub async fn try_reattach(&self) -> anyhow::Result<bool> {
        if !matches!(self.status(), ServerStatus::Stopped | ServerStatus::Crashed) {
            // Someone already called start() / try_reattach() — don't stomp.
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
            Ok(p) if p.is_alive() => p,
            _ => {
                let _ = std::fs::remove_file(&state_path);
                return Ok(false);
            }
        };

        // Start the tail from the current end of the log file so we don't
        // dump megabytes of historical lines into the GUI on attach.
        let log_start_pos = std::fs::metadata(&self.config.paths.log_file)
            .map(|m| m.len())
            .unwrap_or(0);

        let process = Arc::new(process);

        let (mut rx, tail_handle) = logtail::spawn(LogTailConfig {
            path: self.config.paths.log_file.clone(),
            poll_interval: Duration::from_millis(250),
            channel_capacity: 1024,
            start_pos: log_start_pos,
        });

        let pump_handle = {
            let events = self.events.clone();
            let status = self.status.clone();
            tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    for ev in parse_log_line(&line) {
                        if let ServerEvent::ServerReady = ev {
                            let mut s = status.lock().expect("status mutex poisoned");
                            if *s == ServerStatus::Starting {
                                *s = ServerStatus::Running;
                                let _ = events.send(ServerEvent::StatusChanged(
                                    ServerStatus::Running,
                                ));
                            }
                        }
                        let _ = events.send(ev);
                    }
                }
            })
        };

        {
            let process = process.clone();
            let status = self.status.clone();
            let events = self.events.clone();
            let state_path = state_path.clone();
            tokio::task::spawn_blocking(move || {
                let res = process.wait_for_exit_with_timeout(Duration::MAX);
                let mut s = status.lock().expect("status mutex poisoned");
                let next = match res {
                    Ok(Some(code)) if code == 0 || *s == ServerStatus::Stopping => {
                        ServerStatus::Stopped
                    }
                    Ok(Some(_)) => ServerStatus::Crashed,
                    Ok(None) => ServerStatus::Crashed,
                    Err(e) => {
                        let _ = events.send(ServerEvent::Warning(format!(
                            "wait_for_exit failed: {e:#}"
                        )));
                        ServerStatus::Crashed
                    }
                };
                *s = next;
                let _ = events.send(ServerEvent::StatusChanged(next));
                let _ = std::fs::remove_file(&state_path);
            });
        }

        *self.inner.lock().expect("inner mutex poisoned") = Some(RunningInner {
            process,
            tail: tail_handle,
            pump: pump_handle,
        });

        // We can't reliably tell whether the server is still in the
        // 60-second world-generation phase or already accepting connections.
        // OpenProcess succeeding only guarantees the process is alive. We
        // optimistically declare Running — incoming `Opened Steam server`
        // log lines later are no-ops thanks to the `*s == Starting` guard.
        *self.status.lock().expect("status mutex poisoned") = ServerStatus::Running;
        let _ = self
            .events
            .send(ServerEvent::StatusChanged(ServerStatus::Running));
        let _ = self.events.send(ServerEvent::Log(format!(
            "[reattach] resumed existing valheim_server.exe pid={}",
            state.pid
        )));

        Ok(true)
    }
}

/// Map one raw log line into zero or more `ServerEvent`s.
///
/// The patterns here are spec §6.5 placeholders — M0 will confirm them
/// against a real server log and we may tighten them then.
fn parse_log_line(line: &str) -> Vec<ServerEvent> {
    let mut out = Vec::with_capacity(2);
    out.push(ServerEvent::Log(line.to_string()));

    if line.contains("World saved") {
        out.push(ServerEvent::WorldSaved {
            at: chrono::Local::now(),
        });
    }
    if let Some(rest) = line.split("Got connection SteamID ").nth(1) {
        if let Some(token) = rest.split_whitespace().next() {
            if let Ok(id) = token.parse::<u64>() {
                out.push(ServerEvent::PlayerJoined { steam_id: id });
            }
        }
    }
    if let Some(rest) = line.split("Closing socket ").nth(1) {
        if let Some(token) = rest.split_whitespace().next() {
            if let Ok(id) = token.parse::<u64>() {
                out.push(ServerEvent::PlayerLeft { steam_id: id });
            }
        }
    }
    // Ready = the server is registered with Steam matchmaking and external
    // clients can join. Confirmed against real Valheim 0.221.12 output: this
    // is emitted right after `Done generating locations`, ~60s after start
    // on a fresh world. `Game server connected` and `DungeonDB Start` fire
    // earlier (before location gen) and are misleading as "ready".
    if line.contains("Opened Steam server") {
        out.push(ServerEvent::ServerReady);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_world_saved() {
        let events = parse_log_line("11/23/2024 14:55:01: World saved ( 123ms )");
        assert!(events.iter().any(|e| matches!(e, ServerEvent::WorldSaved { .. })));
    }

    #[test]
    fn parse_player_joined() {
        let events = parse_log_line("Got connection SteamID 76561198000000000");
        assert!(events
            .iter()
            .any(|e| matches!(e, ServerEvent::PlayerJoined { steam_id: 76561198000000000 })));
    }

    #[test]
    fn parse_player_left() {
        let events = parse_log_line("Closing socket 76561198000000000");
        assert!(events
            .iter()
            .any(|e| matches!(e, ServerEvent::PlayerLeft { steam_id: 76561198000000000 })));
    }
}
