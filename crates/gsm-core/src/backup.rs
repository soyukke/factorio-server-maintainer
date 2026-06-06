//! Backup / rollback primitives — see spec §6.7.
//!
//! The implementation here is intentionally a thin stub. The actual filesystem
//! work lives in the game-specific crate (which knows where worlds live) or
//! a future shared helper once the second game implementation lands.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Opaque identifier for a backup. Currently the absolute path to the snapshot
/// directory; kept opaque to leave room for future ID schemes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct BackupId(pub String);

/// Provenance of a snapshot.
///
/// `Auto`: created by a future scheduler (not used yet).
/// `Manual`: user-initiated via the "Backup now" button.
/// `PreRollback`: safety snapshot taken automatically just before a
/// rollback operation overwrites the live world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum BackupKind {
    Auto,
    Manual,
    PreRollback,
}

impl BackupKind {
    /// Directory-name suffix used to record the kind on disk. Legacy dirs
    /// without a suffix are treated as `Manual` on read.
    pub fn dir_suffix(&self) -> &'static str {
        match self {
            BackupKind::Auto => "_auto",
            BackupKind::Manual => "_manual",
            BackupKind::PreRollback => "_pre_rollback",
        }
    }

    pub fn from_dir_name(name: &str) -> Self {
        if name.ends_with("_pre_rollback") {
            BackupKind::PreRollback
        } else if name.ends_with("_auto") {
            BackupKind::Auto
        } else {
            // _manual + legacy unsuffixed dirs.
            BackupKind::Manual
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Backup {
    pub id: BackupId,
    pub world: String,
    pub created_at: chrono::DateTime<chrono::Local>,
    pub dir: PathBuf,
    pub size_bytes: u64,
    #[serde(default = "default_kind")]
    pub kind: BackupKind,
}

fn default_kind() -> BackupKind {
    BackupKind::Manual
}
