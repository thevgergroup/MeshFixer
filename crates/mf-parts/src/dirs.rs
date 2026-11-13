//! PLATFORM ADAPTER (MF-PLAT-001): where MeshFixer keeps its settings.
//! The only module that knows each OS's convention; no dependency (D20).
//!
//! - `MESHFIXER_CONFIG_DIR`, when set, replaces the platform directory
//!   (tests, portable installs);
//! - macOS: `$HOME/Library/Application Support`;
//! - Windows: `%APPDATA%`;
//! - elsewhere: `$XDG_CONFIG_HOME`, else `$HOME/.config`.
//!
//! MeshFixer's own files go in [`meshfixer_dir`], `<config dir>/meshfixer`.

use std::path::PathBuf;

/// The environment variable that overrides the platform config directory.
pub const CONFIG_DIR_ENV: &str = "MESHFIXER_CONFIG_DIR";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirsError {
    #[error(
        "can't find the settings folder: {variable} isn't set (set MESHFIXER_CONFIG_DIR to \
         choose one)"
    )]
    NotSet { variable: &'static str },
}

fn var(name: &'static str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The platform's per-user configuration directory (see the module docs).
pub fn config_dir() -> Result<PathBuf, DirsError> {
    if let Some(dir) = var(CONFIG_DIR_ENV) {
        return Ok(dir);
    }
    if cfg!(target_os = "macos") {
        var("HOME")
            .map(|h| h.join("Library").join("Application Support"))
            .ok_or(DirsError::NotSet { variable: "HOME" })
    } else if cfg!(windows) {
        var("APPDATA").ok_or(DirsError::NotSet {
            variable: "APPDATA",
        })
    } else {
        var("XDG_CONFIG_HOME")
            .or_else(|| var("HOME").map(|h| h.join(".config")))
            .ok_or(DirsError::NotSet { variable: "HOME" })
    }
}

/// `<config dir>/meshfixer`, where MeshFixer's settings files live.
pub fn meshfixer_dir() -> Result<PathBuf, DirsError> {
    Ok(config_dir()?.join("meshfixer"))
}
