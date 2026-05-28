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

    /// A jsonl path under the user's claude root that classify() will pick up,
    /// scoped to this process so parallel test runs don't collide. The file
    /// itself isn't created here — callers write it.
    fn temp_claude_jsonl(tag: &str) -> PathBuf {
        let root = claude_root().expect("claude_root resolvable");
        let dir = root.join(format!("-agtop-test-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session.jsonl")
    }

    fn append(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(bytes).unwrap();
    }

    /// First tail seeds the offset and parses the line; a second tail with no
    /// new bytes is a no-op (`Ok(false)`, offset unchanged); appending another
    /// complete line advances the offset and accumulates into the existing
    /// session — i.e. the parser doesn't re-read the bytes it already consumed.
    #[test]
    fn tail_is_incremental_across_writes() {
        let path = temp_claude_jsonl("incremental");
        let _g = scopeguard(&path);

        // First line.
        let l1 = br#"{"type":"assistant","sessionId":"s1","cwd":"/w/proj","message":{"model":"claude-opus-4-7","usage":{"input_tokens":10,"output_tokens":5}}}
"#;
        append(&path, l1);
        let kind = classify(&path).expect("classify");
        let mut sess = crate::model::Session::new(kind, path.clone());

        let changed = tail(&mut sess, false).unwrap();
        assert!(changed, "first tail should report progress");
        assert_eq!(sess.file_offset, l1.len() as u64);
        assert_eq!(sess.tokens.input, 10);
        assert_eq!(sess.tokens.output, 5);
        assert_eq!(sess.turn_count, 1);

        // No new bytes → fast path returns false without parsing again.
        let unchanged = tail(&mut sess, false).unwrap();
        assert!(!unchanged, "second tail without new bytes should be a no-op");
        assert_eq!(sess.tokens.input, 10);
        assert_eq!(sess.turn_count, 1);

        // Append another line. Offset should advance to total file length and
        // tokens should accumulate, proving the parser skipped the old bytes.
        let l2 = br#"{"type":"assistant","message":{"usage":{"input_tokens":3,"output_tokens":7}}}
"#;
        append(&path, l2);
        let changed2 = tail(&mut sess, false).unwrap();
        assert!(changed2);
        assert_eq!(sess.file_offset, (l1.len() + l2.len()) as u64);
        assert_eq!(sess.tokens.input, 13);
        assert_eq!(sess.tokens.output, 12);
        assert_eq!(sess.turn_count, 2);
    }

    /// Bytes without a trailing newline are not yet a complete line: tail
    /// leaves the offset at the last complete-line boundary and the next tail
    /// (after the newline lands) picks them up exactly once.
    #[test]
    fn tail_holds_partial_trailing_line_until_newline() {
        let path = temp_claude_jsonl("partial");
        let _g = scopeguard(&path);

        // Complete line plus a partial fragment (no trailing newline).
        let complete = br#"{"type":"assistant","message":{"usage":{"input_tokens":1,"output_tokens":1}}}
"#;
        let partial = br#"{"type":"assistant","message":{"usage":{"in"#;
        append(&path, complete);
        append(&path, partial);

        let mut sess = crate::model::Session::new(classify(&path).unwrap(), path.clone());
        let changed = tail(&mut sess, false).unwrap();
        assert!(changed);
        // Offset stops at the end of the complete line — the dangling bytes
        // stay on disk to be re-read once their newline arrives.
        assert_eq!(sess.file_offset, complete.len() as u64);
        assert_eq!(sess.turn_count, 1);

        // Now finish the partial line.
        let rest = br#"put_tokens":4,"output_tokens":2}}}
"#;
        append(&path, rest);
        let changed2 = tail(&mut sess, false).unwrap();
        assert!(changed2);
        assert_eq!(
            sess.file_offset,
            (complete.len() + partial.len() + rest.len()) as u64
        );
        assert_eq!(sess.tokens.input, 1 + 4);
        assert_eq!(sess.tokens.output, 1 + 2);
        assert_eq!(sess.turn_count, 2);
    }

    /// Truncation (file shrinks below the cached offset) resets the offset to
    /// zero so the next tail re-reads the file from the start. This is the
    /// recovery path for log rotation that reuses the same filename.
    #[test]
    fn tail_resets_offset_when_file_shrinks() {
        let path = temp_claude_jsonl("shrink");
        let _g = scopeguard(&path);

        let l1 = br#"{"type":"assistant","message":{"usage":{"input_tokens":50,"output_tokens":50}}}
"#;
        append(&path, l1);
        let mut sess = crate::model::Session::new(classify(&path).unwrap(), path.clone());
        tail(&mut sess, false).unwrap();
        assert_eq!(sess.file_offset, l1.len() as u64);

        // Replace the file with a shorter content (simulating rotation/truncate).
        let l2 = br#"{"type":"assistant","message":{"usage":{"input_tokens":1,"output_tokens":2}}}
"#;
        std::fs::write(&path, l2).unwrap();

        let changed = tail(&mut sess, false).unwrap();
        assert!(changed, "tail should re-read after truncate");
        assert_eq!(sess.file_offset, l2.len() as u64);
        // The post-truncate read is added on top of the pre-truncate totals;
        // what matters here is that the offset reset and the new line was
        // parsed (not skipped because `offset > len`).
        assert_eq!(sess.tokens.input, 50 + 1);
        assert_eq!(sess.tokens.output, 50 + 2);
    }

    /// Cleanup helper: removes both the file and its synthetic parent dir
    /// when the test scope ends, even on panic.
    fn scopeguard(path: &Path) -> impl Drop {
        struct Guard(PathBuf);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
                if let Some(parent) = self.0.parent() {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
        Guard(path.to_path_buf())
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
