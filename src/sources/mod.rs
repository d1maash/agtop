pub mod claude;
pub mod codex;

use crate::model::{AgentKind, Session};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
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
/// update session, advance offset past the last newline consumed.
/// `live` controls whether to push samples into the rate window.
pub fn tail(session: &mut Session, live: bool) -> Result<bool> {
    // Cheap probe first: avoid `File::open` (and its FD/syscalls) when the
    // file hasn't grown since the previous tail. The safety-scan fires this
    // path for every known session every 15s, so most calls are no-ops.
    let len = std::fs::metadata(&session.file)?.len();
    if len < session.file_offset {
        session.file_offset = 0;
    }
    if len == session.file_offset {
        return Ok(false);
    }

    let mut f = File::open(&session.file)?;
    f.seek(SeekFrom::Start(session.file_offset))?;
    let mut reader = BufReader::new(f);

    let mut line = String::new();
    let mut consumed: u64 = 0;
    let mut any = false;
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        // EOF before a newline → partial trailing line. Leave the bytes in
        // the file; the next tail will see them as a complete line.
        if !line.ends_with('\n') {
            break;
        }
        let s = line.trim_end_matches(['\r', '\n']);
        if !s.is_empty() {
            match session.kind {
                AgentKind::Claude => claude::update_from_line(session, s, live),
                AgentKind::Codex => codex::update_from_line(session, s, live),
            }
            any = true;
        }
        consumed += n as u64;
    }
    session.file_offset += consumed;
    Ok(any)
}

/// Initial pass: parse everything, do not contribute to live rate window.
/// Parallelised so startup with hundreds of jsonls stays sub-second.
pub fn initial_scan() -> Result<HashMap<PathBuf, Session>> {
    let map: HashMap<PathBuf, Session> = list_files()
        .into_par_iter()
        .map(|(kind, path)| {
            let mut sess = Session::new(kind, path.clone());
            let _ = tail(&mut sess, false);
            (path, sess)
        })
        .collect();
    Ok(map)
}
