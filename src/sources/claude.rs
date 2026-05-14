use crate::model::{AgentKind, Session, TokenStats};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

pub fn scan(root: &Path) -> Result<Vec<Session>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for entry in WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(session) = parse_file(p) {
            if let Some(s) = session {
                out.push(s);
            }
        }
    }
    Ok(out)
}

fn parse_file(path: &Path) -> Result<Option<Session>> {
    let meta = std::fs::metadata(path)?;
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut model: Option<String> = None;
    let mut started: Option<DateTime<Utc>> = None;
    let mut last: Option<DateTime<Utc>> = None;
    let mut tokens = TokenStats::default();
    let mut turns: u64 = 0;

    for line in reader.lines().flatten() {
        if line.is_empty() {
            continue;
        }
        let parsed: Line = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if id.is_none() {
            id = parsed.session_id.clone();
        }
        if cwd.is_none() {
            cwd = parsed.cwd.clone();
        }
        if let Some(ts) = parsed.timestamp.as_deref().and_then(parse_ts) {
            if started.map_or(true, |s| ts < s) {
                started = Some(ts);
            }
            if last.map_or(true, |l| ts > l) {
                last = Some(ts);
            }
        }
        if parsed.r#type == "assistant" {
            if let Some(msg) = parsed.message {
                if let Some(m) = msg.model {
                    if model.is_none() || model.as_deref() == Some("") {
                        model = Some(m);
                    }
                }
                if let Some(u) = msg.usage {
                    tokens.input += u.input_tokens;
                    tokens.output += u.output_tokens;
                    tokens.cache_read += u.cache_read_input_tokens;
                    tokens.cache_creation += u.cache_creation_input_tokens;
                    turns += 1;
                }
            }
        }
    }

    let id = match id {
        Some(v) => v,
        None => derive_id_from_path(path),
    };

    Ok(Some(Session {
        kind: AgentKind::Claude,
        id,
        file: PathBuf::from(path),
        cwd,
        model,
        started_at: started,
        last_activity: last,
        tokens,
        turn_count: turns,
        file_size: meta.len(),
    }))
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

fn derive_id_from_path(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}
