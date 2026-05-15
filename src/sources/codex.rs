use crate::model::Session;
use crate::sources::parse_ts;
use chrono::Utc;
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
        if session.started_at.is_none_or(|s| t < s) {
            session.started_at = Some(t);
        }
        if session.last_activity.is_none_or(|l| t > l) {
            session.last_activity = Some(t);
        }
    }
    let Some(payload) = parsed.payload else {
        return;
    };

    match parsed.r#type.as_str() {
        "session_meta" => {
            if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
                session.id = id.to_string();
            }
            if session.cwd.is_none() {
                session.cwd = payload
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
        }
        "turn_context" => {
            if session.model.is_none() {
                if let Some(m) = payload.get("model").and_then(|v| v.as_str()) {
                    session.set_model(m.to_string());
                }
            }
            if session.cwd.is_none() {
                session.cwd = payload
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(String::from);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, Session};
    use std::path::PathBuf;

    fn sess() -> Session {
        Session::new(AgentKind::Codex, PathBuf::from("/tmp/rollout-x.jsonl"))
    }

    #[test]
    fn session_meta_sets_id_and_cwd() {
        let mut s = sess();
        let line = r#"{"type":"session_meta","timestamp":"2026-01-01T00:00:00Z","payload":{"id":"abc-123","cwd":"/work/proj"}}"#;
        update_from_line(&mut s, line, false);
        assert_eq!(s.id, "abc-123");
        assert_eq!(s.cwd.as_deref(), Some("/work/proj"));
    }

    #[test]
    fn turn_context_sets_model_only_once() {
        let mut s = sess();
        update_from_line(
            &mut s,
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            false,
        );
        update_from_line(
            &mut s,
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
            false,
        );
        assert_eq!(s.model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn token_count_subtracts_cached_from_input_but_not_output() {
        let mut s = sess();
        let now = Utc::now().to_rfc3339();
        let line = format!(
            r#"{{"type":"event_msg","timestamp":"{now}","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":1000,"output_tokens":200,"cached_input_tokens":300}}}}}}}}"#
        );
        update_from_line(&mut s, &line, true);
        // input is net of cached; cached goes to cache_read; output unchanged.
        assert_eq!(s.tokens.input, 700);
        assert_eq!(s.tokens.cache_read, 300);
        assert_eq!(s.tokens.output, 200);
        assert_eq!(s.turn_count, 1);
        // Sample uses the full gross input + output (1000 + 200), matching
        // what the model actually consumed/produced in that turn.
        assert_eq!(s.samples.len(), 1);
        assert_eq!(s.samples.front().unwrap().1, 1200);
    }

    #[test]
    fn non_token_event_msg_is_ignored() {
        let mut s = sess();
        let line = r#"{"type":"event_msg","payload":{"type":"something_else"}}"#;
        update_from_line(&mut s, line, false);
        assert_eq!(s.tokens.total(), 0);
        assert_eq!(s.turn_count, 0);
    }

    #[test]
    fn malformed_json_is_ignored() {
        let mut s = sess();
        update_from_line(&mut s, "not json at all", false);
        assert_eq!(s.tokens.total(), 0);
    }
}
