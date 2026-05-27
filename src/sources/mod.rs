pub mod claude;
pub mod codex;

use crate::model::{AgentKind, Session};
use anyhow::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

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
        if p.starts_with(&root.to_string_lossy().to_string())
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
        {
            return Some(AgentKind::Codex);
        }
    }
    None
}

pub fn list_files() -> Vec<(AgentKind, PathBuf)> {
    let mut out = Vec::new();
    for root_kind in [
        (claude_root(), AgentKind::Claude),
        (codex_root(), AgentKind::Codex),
    ] {
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
    initial_scan_since(None)
}

/// Filtered initial pass: skip files whose mtime is older than `cutoff`. Old
/// sessions stay out of the map at startup and are picked up lazily by the
/// watcher only when they become active (a notify event fires for them). The
/// safety-scan applies the same cutoff so it doesn't undo this filtering 15s
/// later. Pass `None` for a full scan (behaviour identical to `initial_scan`).
pub fn initial_scan_since(
    cutoff: Option<DateTime<Utc>>,
) -> Result<HashMap<PathBuf, Session>> {
    let map: HashMap<PathBuf, Session> = list_files()
        .into_par_iter()
        .filter(|(_, p)| passes_cutoff(p, cutoff))
        .map(|(kind, path)| {
            let mut sess = Session::new(kind, path.clone());
            let _ = tail(&mut sess, false);
            (path, sess)
        })
        .collect();
    Ok(map)
}

/// `true` when `path`'s mtime is at or after `cutoff`. A missing cutoff means
/// "no filter". A missing/unreadable mtime is treated as old (skipped) to keep
/// startup fast; the watcher will still notice the file if it grows later.
pub fn passes_cutoff(path: &Path, cutoff: Option<DateTime<Utc>>) -> bool {
    let Some(cutoff) = cutoff else {
        return true;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    DateTime::<Utc>::from(modified) >= cutoff
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::io::Write;

    #[test]
    fn passes_cutoff_none_means_no_filter() {
        // Pass a path that doesn't exist; with `None`, we always return true
        // and never touch the filesystem.
        let p = Path::new("/nonexistent/path/that/does/not/exist.jsonl");
        assert!(passes_cutoff(p, None));
    }

    #[test]
    fn passes_cutoff_missing_file_is_excluded() {
        let cutoff = Utc::now() - Duration::days(1);
        let p = Path::new("/nonexistent/path/that/does/not/exist.jsonl");
        assert!(!passes_cutoff(p, Some(cutoff)));
    }

    #[test]
    fn passes_cutoff_fresh_file_passes_and_old_does_not() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("agtop-passes-cutoff-{}.jsonl", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "x").unwrap();
        }
        // Cutoff in the past → the just-created file is newer.
        let recent_cutoff = Utc::now() - Duration::hours(1);
        assert!(passes_cutoff(&path, Some(recent_cutoff)));
        // Cutoff in the future → no real file is newer than that.
        let future_cutoff = Utc::now() + Duration::hours(1);
        assert!(!passes_cutoff(&path, Some(future_cutoff)));
        let _ = std::fs::remove_file(&path);
    }
}
