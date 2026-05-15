use crate::model::Session;
use crate::sources::parse_ts;
use chrono::Utc;
use serde::Deserialize;

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

pub fn update_from_line(session: &mut Session, line: &str, live: bool) {
    let parsed: Line = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };
    // Overwrite the filename-derived id with the first real sessionId we see,
    // then leave it alone — resume/fork lines can carry a different sessionId
    // and we don't want the displayed id to flip every tick.
    let filename_stem = session
        .file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if session.id == filename_stem {
        if let Some(id) = parsed.session_id {
            session.id = id;
        }
    }
    if session.cwd.is_none() {
        session.cwd = parsed.cwd;
    }
    let ts = parsed.timestamp.as_deref().and_then(parse_ts);
    if let Some(t) = ts {
        if session.started_at.map_or(true, |s| t < s) {
            session.started_at = Some(t);
        }
        if session.last_activity.map_or(true, |l| t > l) {
            session.last_activity = Some(t);
        }
    }
    if parsed.r#type == "assistant" {
        if let Some(msg) = parsed.message {
            if let Some(m) = msg.model {
                if session.model.is_none() || session.model.as_deref() == Some("") {
                    session.set_model(m);
                }
            }
            if let Some(u) = msg.usage {
                let added =
                    u.input_tokens + u.output_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens;
                session.tokens.input += u.input_tokens;
                session.tokens.output += u.output_tokens;
                session.tokens.cache_read += u.cache_read_input_tokens;
                session.tokens.cache_creation += u.cache_creation_input_tokens;
                session.turn_count += 1;
                if live {
                    session.push_sample(ts.unwrap_or_else(Utc::now), added);
                }
            }
        }
    }
}

