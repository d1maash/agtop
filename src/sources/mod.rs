pub mod claude;
pub mod codex;

use crate::model::Session;
use anyhow::Result;
use std::path::PathBuf;

pub fn scan_all() -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    if let Some(dir) = claude_root() {
        sessions.extend(claude::scan(&dir)?);
    }
    if let Some(dir) = codex_root() {
        sessions.extend(codex::scan(&dir)?);
    }
    Ok(sessions)
}

fn claude_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

fn codex_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("sessions"))
}
