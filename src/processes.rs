use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

/// Returns the set of session JSONL files whose owning CLI process is alive
/// right now. Detection logic:
///   1. Find running `claude` / `codex` CLI processes via `ps -A -o args`.
///   2. For each, read its working directory via `lsof -p PID -d cwd`.
///   3. Map cwd → ~/.claude/projects/<encoded-cwd>/ and pick the newest
///      JSONL in that directory (that's the session this CLI is writing to).
///   4. For Codex, also match parsed `session.cwd` to running CLI cwds.
///
/// Returns None if `ps` or `lsof` is unavailable so the caller can fall back.
pub fn running_session_paths() -> Option<HashSet<PathBuf>> {
    let cwds = running_cli_cwds()?;
    let claude_root = crate::sources::claude_root()?;
    let mut paths = HashSet::new();
    for cwd in &cwds {
        let encoded = encode_cwd_for_claude(cwd);
        let project_dir = claude_root.join(&encoded);
        if let Some(latest) = latest_jsonl(&project_dir) {
            paths.insert(latest);
        }
    }
    Some(paths)
}

/// CWDs of every running `claude`/`codex` CLI process. Excludes desktop
/// Electron apps and helpers.
fn running_cli_cwds() -> Option<Vec<PathBuf>> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,args="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pids = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let (pid_str, rest) = match trimmed.split_once(char::is_whitespace) {
            Some(x) => x,
            None => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if is_cli_command(rest.trim()) {
            pids.push(pid);
        }
    }
    let mut cwds = Vec::new();
    for pid in pids {
        if let Some(cwd) = cwd_of(pid) {
            cwds.push(cwd);
        }
    }
    Some(cwds)
}

fn is_cli_command(cmdline: &str) -> bool {
    let first = cmdline.split_whitespace().next().unwrap_or("");
    let is_claude = (first.ends_with("/claude")
        || first.ends_with("/claude.exe")
        || first == "claude"
        || first == "claude.exe")
        && !first.contains("/Claude.app/");
    let is_codex = (first.ends_with("/codex") || first == "codex")
        && !first.contains("/Codex.app/")
        && !cmdline.contains("Codex Helper")
        && !cmdline.contains("Codex.app/Contents/Resources/codex app-server");
    is_claude || is_codex
}

fn cwd_of(pid: u32) -> Option<PathBuf> {
    let out = Command::new("lsof")
        .args(["-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(path) = line.strip_prefix('n') {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Claude Code names its project dirs by replacing every '/' in the cwd
/// with '-' (the leading slash becomes a leading dash too).
fn encode_cwd_for_claude(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

fn latest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(m) = meta.modified() else { continue };
        if best.as_ref().map_or(true, |(bm, _)| m > *bm) {
            best = Some((m, p));
        }
    }
    best.map(|(_, p)| p)
}

pub struct OpenFilesCache {
    files: Option<HashSet<PathBuf>>,
    last_refresh: Instant,
    ttl: Duration,
}

impl OpenFilesCache {
    pub fn new() -> Self {
        Self {
            files: running_session_paths(),
            last_refresh: Instant::now(),
            ttl: Duration::from_secs(2),
        }
    }

    pub fn get(&mut self) -> Option<&HashSet<PathBuf>> {
        if self.last_refresh.elapsed() >= self.ttl {
            self.files = running_session_paths();
            self.last_refresh = Instant::now();
        }
        self.files.as_ref()
    }
}
