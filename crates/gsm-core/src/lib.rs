//! GUI-agnostic, game-agnostic core for dedicated game server management.
//!
//! See `docs/valheim-server-manager-spec.md` §3–§4 for the architectural rationale.

pub mod backup;
pub mod config;
pub mod logtail;
pub mod process;
pub mod server;
pub mod steamcmd;

pub use backup::{Backup, BackupId, BackupKind};
pub use config::{AppConfig, Language, ManagerConfig, PathsConfig, ServerConfig};
pub use logtail::LogTailConfig;
pub use process::{ServerProcess, SpawnRequest};
pub use server::{GameServerManager, ServerEvent, ServerStatus};
pub use steamcmd::SteamCmdJob;
