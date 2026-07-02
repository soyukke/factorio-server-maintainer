//! GUI-agnostic, game-agnostic core for dedicated game server management.
//!
//! Shared process, SteamCMD, backup, config, and log-tail helpers.

pub mod backup;
pub mod config;
pub mod logtail;
pub mod process;
pub mod server;
pub mod steamcmd;

pub use backup::{Backup, BackupId, BackupKind};
pub use config::{AppConfig, FactorioDlc, Language, ManagerConfig, PathsConfig, ServerConfig};
pub use logtail::LogTailConfig;
pub use process::{ServerProcess, SpawnRequest};
pub use server::{GameServerManager, ServerEvent, ServerStatus};
pub use steamcmd::SteamCmdJob;
