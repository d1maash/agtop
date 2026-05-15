use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};
use walkdir::WalkDir;

/// Returns the set of session JSONL files whose owning CLI process is alive
/// right now. Detection logic:
///   1. Find running `claude` / `codex` CLI processes via `ps -A -o args`.
///   2. For each, read its working directory via `lsof -p PID -d cwd`.
///   3. Map cwd → ~/.claude/projects/<encoded-cwd>/ and pick recently-touched
///      JSONLs in that directory (Claude Code).
///   4. For Codex, scan recently-touched rollout-*.jsonl files and match each
///      file's `session_meta.cwd` against the set of running CLI cwds.
///
/// Returns None if `ps` or `lsof` is unavailable so the caller can fall back.
/// Multiple concurrent terminals in the same cwd produce multiple paths.
pub fn running_session_paths() -> Option<HashSet<PathBuf>> {
    let cwds = running_cli_cwds()?;
    let mut paths = HashSet::new();
    let window = Duration::from_secs(30 * 60);
    let now = SystemTime::now();

    if let Some(claude_root) = crate::sources::claude_root() {
        for cwd in &cwds {
            let encoded = encode_cwd_for_claude(cwd);
            let project_dir = claude_root.join(&encoded);
            let Ok(entries) = std::fs::read_dir(&project_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let Ok(modified) = meta.modified() else { continue };
                if now.duration_since(modified).map_or(false, |d| d <= window) {
                    paths.insert(p);
                }
            }
        }
    }

    if let Some(codex_root) = crate::sources::codex_root() {
        if codex_root.exists() {
            for entry in WalkDir::new(&codex_root)
                .max_depth(5)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                let name = match p.file_name().and_then(|s| s.to_str()) {
                    Some(n) if n.starts_with("rollout-") => n,
                    _ => continue,
                };
                let _ = name;
                let Ok(meta) = entry.metadata() else { continue };
                let Ok(modified) = meta.modified() else { continue };
                if now.duration_since(modified).map_or(true, |d| d > window) {
                    continue;
                }
                if let Some(file_cwd) = codex_session_cwd(p) {
                    if cwds.contains(Path::new(&file_cwd)) {
                        paths.insert(p.to_path_buf());
                    }
                }
            }
        }
    }

    Some(paths)
}

/// Read the first few lines of a Codex rollout and extract `session_meta.cwd`.
fn codex_session_cwd(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).ok()?;
    let text = std::str::from_utf8(&buf[..n]).ok()?;
    for line in text.lines().take(8) {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("session_meta") {
            if let Some(cwd) = v
                .get("payload")
                .and_then(|p| p.get("cwd"))
                .and_then(|c| c.as_str())
            {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

/// CWDs of every running `claude`/`codex` CLI process. Excludes desktop
/// Electron apps and helpers. Dedups.
fn running_cli_cwds() -> Option<HashSet<PathBuf>> {
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
    let mut cwds = HashSet::new();
    for pid in pids {
        if let Some(cwd) = cwd_of(pid) {
            cwds.insert(cwd);
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

/// Claude Code names its project dirs by replacing every '/' AND every '.'
/// in the cwd with '-' (the leading slash becomes a leading dash too). So
/// `/Users/foo/my.app` → `-Users-foo-my-app`.
fn encode_cwd_for_claude(cwd: &Path) -> String {
    cwd.to_string_lossy().replace(['/', '.'], "-")
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
