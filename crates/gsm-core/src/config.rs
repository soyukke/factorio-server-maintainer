//! Configuration schema mirroring spec §7.
//!
//! All paths are kept as absolute `PathBuf`s — relative paths are rejected at
//! load time so the GUI is independent of the current working directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    pub paths: PathsConfig,
    pub server: ServerConfig,
    #[serde(default)]
    pub manager: ManagerConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PathsConfig {
    pub steamcmd: PathBuf,
    pub server_dir: PathBuf,
    pub save_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub log_file: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerConfig {
    pub name: String,
    pub world: String,
    pub password: String,
    pub port: u16,
    pub public: u8,
    pub save_interval: u32,
    pub backups: u32,
    /// Spec §6.6: fixed to `false` for the Steam-backend + playit topology.
    #[serde(default)]
    pub crossplay: bool,

    // Valheim world modifiers (game-agnostic to the config schema but
    // game-specific in meaning). Stored as raw cmdline tokens such as
    // "veryeasy" / "muchmore" so we can emit them directly. Empty string
    // means "no preset chosen". Defaults to "default" / "" so previous
    // configs without these fields still load.
    #[serde(default = "default_modifier")]
    pub mod_combat: String,
    #[serde(default = "default_modifier")]
    pub mod_deathpenalty: String,
    #[serde(default = "default_modifier")]
    pub mod_resources: String,
    #[serde(default = "default_modifier")]
    pub mod_raids: String,
    #[serde(default = "default_modifier")]
    pub mod_portals: String,
    #[serde(default)]
    pub preset: String,

    /// Boolean world keys passed via `-setkey <name>`. Each entry is one
    /// key (e.g. `"nobuildcost"`, `"nomap"`). Order doesn't matter.
    #[serde(default)]
    pub world_keys: Vec<String>,
}

fn default_modifier() -> String {
    "default".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManagerConfig {
    pub graceful_stop_timeout_secs: u32,
    pub auto_backup_before_update: bool,
    #[serde(default)]
    pub language: Language,
    /// Free-form connection address shown to other players (e.g. a
    /// `tunnel.playit.gg:NNNNN` URL or a public IP). Purely informational;
    /// not consumed by Valheim itself.
    #[serde(default)]
    pub public_address: String,
    /// Valheim's internal short-cycle world backup interval in seconds.
    /// Passed as `-backupshort`. Default 7200 (2 hours).
    #[serde(default = "default_backup_short_secs")]
    pub backup_short_secs: u32,
    /// Valheim's internal long-cycle world backup interval in seconds.
    /// Passed as `-backuplong`. Default 43200 (12 hours).
    #[serde(default = "default_backup_long_secs")]
    pub backup_long_secs: u32,
}

fn default_backup_short_secs() -> u32 {
    7200
}
fn default_backup_long_secs() -> u32 {
    43200
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum Language {
    #[default]
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "en")]
    En,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            graceful_stop_timeout_secs: 30,
            auto_backup_before_update: true,
            language: Language::default(),
            public_address: String::new(),
            backup_short_secs: 7200,
            backup_long_secs: 43200,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "MyServer".into(),
            world: "Dedicated".into(),
            password: String::new(),
            port: 2456,
            public: 0,
            save_interval: 900,
            backups: 4,
            crossplay: false,
            mod_combat: "default".into(),
            mod_deathpenalty: "default".into(),
            mod_resources: "default".into(),
            mod_raids: "default".into(),
            mod_portals: "default".into(),
            preset: String::new(),
            world_keys: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(PathBuf),
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("validation failed: {0}")]
    Validation(String),
}

impl AppConfig {
    /// Load and validate `config.toml` from `path`.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::NotFound(path.to_path_buf()));
        }
        let text = std::fs::read_to_string(path)?;
        let cfg: AppConfig = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let text = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Apply spec §6.6 + §8.1 invariants.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let v = |c: bool, m: &str| {
            if c {
                Ok(())
            } else {
                Err(ConfigError::Validation(m.into()))
            }
        };

        // §8.1: all paths absolute.
        for (label, p) in [
            ("paths.steamcmd", &self.paths.steamcmd),
            ("paths.server_dir", &self.paths.server_dir),
            ("paths.save_dir", &self.paths.save_dir),
            ("paths.backup_dir", &self.paths.backup_dir),
            ("paths.log_file", &self.paths.log_file),
        ] {
            v(p.is_absolute(), &format!("{label} must be an absolute path"))?;
        }

        // §6.6: parameter rules.
        v(!self.server.name.is_empty(), "server.name must not be empty")?;
        v(
            !self.server.world.is_empty(),
            "server.world must not be empty",
        )?;
        v(
            self.server.password.chars().count() >= 5,
            "server.password must be at least 5 characters",
        )?;
        v(
            self.server.password != self.server.name,
            "server.password must differ from server.name",
        )?;
        v(
            !self.server.name.contains(&self.server.password)
                && !self.server.password.contains(&self.server.name),
            "server.password must not contain or be contained in server.name",
        )?;
        v(
            (1024..=65534).contains(&self.server.port),
            "server.port must be in 1024..=65534 (port+1 must also be free)",
        )?;
        v(
            matches!(self.server.public, 0 | 1),
            "server.public must be 0 or 1",
        )?;
        v(
            self.server.save_interval >= 60,
            "server.save_interval must be >= 60 seconds",
        )?;
        // server.backups is u32, no lower bound beyond type.
        v(
            !self.server.crossplay,
            "server.crossplay must remain false (spec §0, §6.6)",
        )?;

        Ok(())
    }
}
