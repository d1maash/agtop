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
    timestamp: Option<String>,
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

pub fn scan(root: &Path) -> Result<Vec<Session>> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
    for entry in WalkDir::new(root)
        .max_depth(5)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            if !name.starts_with("rollout-") {
                continue;
            }
        }
        if let Ok(Some(s)) = parse_file(p) {
            out.push(s);
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

        if let Some(ts) = parsed.timestamp.as_deref().and_then(parse_ts) {
            if started.map_or(true, |s| ts < s) {
                started = Some(ts);
            }
            if last.map_or(true, |l| ts > l) {
                last = Some(ts);
            }
        }

        let payload = match &parsed.payload {
            Some(v) => v,
            None => continue,
        };

        match parsed.r#type.as_str() {
            "session_meta" => {
                if id.is_none() {
                    id = payload.get("id").and_then(|v| v.as_str()).map(String::from);
                }
                if cwd.is_none() {
                    cwd = payload.get("cwd").and_then(|v| v.as_str()).map(String::from);
                }
            }
            "turn_context" => {
                if model.is_none() {
                    model = payload.get("model").and_then(|v| v.as_str()).map(String::from);
                }
                if cwd.is_none() {
                    cwd = payload.get("cwd").and_then(|v| v.as_str()).map(String::from);
                }
            }
            "event_msg" => {
                if payload.get("type").and_then(|v| v.as_str()) == Some("token_count") {
                    if let Some(info) = payload.get("info") {
                        if let Some(last_usage) = info.get("last_token_usage") {
                            if let Ok(u) = serde_json::from_value::<TokenUsage>(last_usage.clone())
                            {
                                tokens.input += u.input_tokens.saturating_sub(u.cached_input_tokens);
                                tokens.output += u.output_tokens;
                                tokens.cache_read += u.cached_input_tokens;
                                tokens.reasoning += u.reasoning_output_tokens;
                                turns += 1;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let id = id.unwrap_or_else(|| derive_id_from_path(path));

    Ok(Some(Session {
        kind: AgentKind::Codex,
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
