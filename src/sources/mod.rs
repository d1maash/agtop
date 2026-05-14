pub mod claude;
pub mod codex;

use crate::model::{AgentKind, Session};
use anyhow::Result;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn claude_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

pub fn codex_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("sessions"))
}

pub fn classify(path: &Path) -> Option<AgentKind> {
    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
        return None;
    }
    let p = path.to_string_lossy();
    if let Some(root) = claude_root() {
        if p.starts_with(&root.to_string_lossy().to_string()) {
            return Some(AgentKind::Claude);
        }
    }
    if let Some(root) = codex_root() {
        if p.starts_with(&root.to_string_lossy().to_string()) {
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("rollout-"))
            {
                return Some(AgentKind::Codex);
            }
        }
    }
    None
}

pub fn list_files() -> Vec<(AgentKind, PathBuf)> {
    let mut out = Vec::new();
    for root_kind in [(claude_root(), AgentKind::Claude), (codex_root(), AgentKind::Codex)] {
        let Some(root) = root_kind.0 else { continue };
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(&root)
            .max_depth(5)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if let Some(kind) = classify(p) {
                if kind == root_kind.1 {
                    out.push((kind, p.to_path_buf()));
                }
            }
        }
    }
    out
}

/// Read new bytes since `session.file_offset`, parse complete lines,
/// update session, advance offset to position of last newline read.
/// `live` controls whether to push samples into the rate window.
pub fn tail(session: &mut Session, live: bool) -> Result<bool> {
    let mut f = File::open(&session.file)?;
    let len = f.metadata()?.len();
    if len < session.file_offset {
        session.file_offset = 0;
    }
    if len == session.file_offset {
        return Ok(false);
    }
    f.seek(SeekFrom::Start(session.file_offset))?;
    let mut buf = Vec::with_capacity((len - session.file_offset) as usize);
    f.read_to_end(&mut buf)?;
    let last_nl = buf.iter().rposition(|&b| b == b'\n');
    let Some(last_nl) = last_nl else {
        return Ok(false);
    };
    let mut any = false;
    for line in buf[..=last_nl].split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(s) = std::str::from_utf8(line) {
            match session.kind {
                AgentKind::Claude => claude::update_from_line(session, s, live),
                AgentKind::Codex => codex::update_from_line(session, s, live),
            }
            any = true;
        }
    }
    session.file_offset += (last_nl + 1) as u64;
    Ok(any)
}

/// Initial pass: parse everything, do not contribute to live rate window.
pub fn initial_scan() -> Result<HashMap<PathBuf, Session>> {
    let mut map: HashMap<PathBuf, Session> = HashMap::new();
    for (kind, path) in list_files() {
        let mut sess = Session::new(kind, path.clone());
        let _ = tail(&mut sess, false);
        map.insert(path, sess);
    }
    Ok(map)
}
