use crate::model::Session;
use chrono::{DateTime, Utc};
use serde::Deserialize;

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
}

pub fn update_from_line(session: &mut Session, line: &str, live: bool) {
    let parsed: Line = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };
    let ts = parsed.timestamp.as_deref().and_then(parse_ts);
    if let Some(t) = ts {
        if session.started_at.map_or(true, |s| t < s) {
            session.started_at = Some(t);
        }
        if session.last_activity.map_or(true, |l| t > l) {
            session.last_activity = Some(t);
        }
    }
    let Some(payload) = parsed.payload else { return };

    match parsed.r#type.as_str() {
        "session_meta" => {
            if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
                session.id = id.to_string();
            }
            if session.cwd.is_none() {
                session.cwd = payload.get("cwd").and_then(|v| v.as_str()).map(String::from);
            }
        }
        "turn_context" => {
            if session.model.is_none() {
                if let Some(m) = payload.get("model").and_then(|v| v.as_str()) {
                    session.set_model(m.to_string());
                }
            }
            if session.cwd.is_none() {
                session.cwd = payload.get("cwd").and_then(|v| v.as_str()).map(String::from);
            }
        }
        "event_msg" => {
            if payload.get("type").and_then(|v| v.as_str()) == Some("token_count") {
                if let Some(info) = payload.get("info") {
                    if let Some(last_usage) = info.get("last_token_usage") {
                        if let Ok(u) = serde_json::from_value::<TokenUsage>(last_usage.clone()) {
                            // OpenAI's `output_tokens` already includes
                            // `reasoning_output_tokens`, so don't add reasoning
                            // again — that would double-count it both in the
                            // total and in the cost.
                            let net_input = u.input_tokens.saturating_sub(u.cached_input_tokens);
                            let added = u.input_tokens + u.output_tokens;
                            session.tokens.input += net_input;
                            session.tokens.output += u.output_tokens;
                            session.tokens.cache_read += u.cached_input_tokens;
                            session.turn_count += 1;
                            if live {
                                session.push_sample(ts.unwrap_or_else(Utc::now), added);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}
