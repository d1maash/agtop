use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// Set of session JSONL files currently held open by any process. None means
/// detection failed (e.g. lsof missing) and the caller should fall back.
pub fn open_session_files() -> Option<HashSet<PathBuf>> {
    let claude = crate::sources::claude_root()?;
    let codex = crate::sources::codex_root()?;

    let mut cmd = Command::new("lsof");
    cmd.args(["-nP", "-Fn", "+D"]);
    let mut any_root = false;
    if claude.exists() {
        cmd.arg(&claude);
        any_root = true;
    }
    if codex.exists() {
        cmd.arg(&codex);
        any_root = true;
    }
    if !any_root {
        return Some(HashSet::new());
    }

    let output = cmd.output().ok()?;
    // lsof exits non-zero when some paths have no open files; that's fine —
    // we still parse stdout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut set = HashSet::new();
    for line in stdout.lines() {
        if let Some(path_str) = line.strip_prefix('n') {
            let p = PathBuf::from(path_str);
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                set.insert(p);
            }
        }
    }
    Some(set)
}

pub struct OpenFilesCache {
    files: Option<HashSet<PathBuf>>,
    last_refresh: Instant,
    ttl: Duration,
}

impl OpenFilesCache {
    pub fn new() -> Self {
        Self {
            files: open_session_files(),
            last_refresh: Instant::now(),
            ttl: Duration::from_secs(2),
        }
    }

    pub fn get(&mut self) -> Option<&HashSet<PathBuf>> {
        if self.last_refresh.elapsed() >= self.ttl {
            self.files = open_session_files();
            self.last_refresh = Instant::now();
        }
        self.files.as_ref()
    }
}
