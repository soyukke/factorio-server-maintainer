//! Core trait and event types — see spec §4.

use crate::backup::{Backup, BackupId};
use async_trait::async_trait;
use tokio::sync::broadcast;

#[async_trait]
pub trait GameServerManager: Send + Sync {
    /// Stable identifier. For example: `"factorio"`.
    fn id(&self) -> &str;

    /// Install or update via SteamCMD. Caller guarantees the server is stopped.
    async fn install_or_update(&self) -> anyhow::Result<()>;

    /// Start the server. Errors if it is already running.
    async fn start(&self) -> anyhow::Result<()>;

    /// Stop the server. When `graceful` is true, send Ctrl+C and wait for the
    /// final save. Force-kill only after `graceful_stop_timeout_secs` and emit
    /// a warning event in that case.
    async fn stop(&self, graceful: bool) -> anyhow::Result<()>;

    fn status(&self) -> ServerStatus;

    /// Subscribe to log-derived events. The GUI reflects these in the UI.
    fn subscribe(&self) -> broadcast::Receiver<ServerEvent>;

    async fn list_backups(&self) -> anyhow::Result<Vec<Backup>>;
    /// Snapshot the current world. Stopped or idle precondition.
    async fn backup(&self) -> anyhow::Result<Backup>;
    /// Restore a backup. Internally stops, overwrites, then starts.
    async fn rollback(&self, id: BackupId) -> anyhow::Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
    /// SteamCMD is running an install/update. The server process itself is
    /// not running during this state.
    Updating,
}

#[derive(Clone, Debug)]
pub enum ServerEvent {
    /// One raw log line.
    Log(String),
    StatusChanged(ServerStatus),
    WorldSaved {
        at: chrono::DateTime<chrono::Local>,
    },
    PlayerJoined {
        name: String,
    },
    PlayerLeft {
        name: String,
    },
    /// Network diagnostics derived from Factorio peer state logs.
    NetworkStatus {
        text: String,
    },
    /// Startup completed; the server is accepting connections.
    ServerReady,
    /// For example: forced termination may have lost data since the last autosave.
    Warning(String),
}
