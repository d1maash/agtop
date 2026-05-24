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
        if session.started_at.is_none_or(|s| t < s) {
            session.started_at = Some(t);
        }
        if session.last_activity.is_none_or(|l| t > l) {
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
                let added = u.input_tokens
                    + u.output_tokens
                    + u.cache_read_input_tokens
                    + u.cache_creation_input_tokens;
                session.tokens.input += u.input_tokens;
                session.tokens.output += u.output_tokens;
                session.tokens.cache_read += u.cache_read_input_tokens;
                session.tokens.cache_creation += u.cache_creation_input_tokens;
                // Current context occupancy = this turn's full prompt (fresh
                // input + cached + cache-creation). Overwrite, don't accumulate.
                session.last_context_tokens =
                    u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens;
                session.turn_count += 1;
                if live {
                    session.push_sample(ts.unwrap_or_else(Utc::now), added);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, Session};
    use std::path::PathBuf;

    fn sess() -> Session {
        Session::new(AgentKind::Claude, PathBuf::from("/tmp/abc.jsonl"))
    }

    #[test]
    fn assistant_line_accumulates_tokens_and_sets_model() {
        let mut s = sess();
        let line = r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","sessionId":"sess-1","cwd":"/work/proj","message":{"model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}"#;
        update_from_line(&mut s, line, false);
        assert_eq!(s.id, "sess-1");
        assert_eq!(s.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(s.tokens.input, 100);
        assert_eq!(s.tokens.output, 50);
        assert_eq!(s.tokens.cache_read, 10);
        assert_eq!(s.tokens.cache_creation, 5);
        assert_eq!(s.turn_count, 1);
        assert!(s.price.is_some()); // resolved via set_model
                                    // live=false → no sample push.
        assert!(s.samples.is_empty());
    }

    #[test]
    fn multiple_assistant_lines_accumulate() {
        let mut s = sess();
        for _ in 0..3 {
            let line =
                r#"{"type":"assistant","message":{"usage":{"input_tokens":1,"output_tokens":2}}}"#;
            update_from_line(&mut s, line, false);
        }
        assert_eq!(s.tokens.input, 3);
        assert_eq!(s.tokens.output, 6);
        assert_eq!(s.turn_count, 3);
    }

    #[test]
    fn session_id_locks_after_first_real_id() {
        let mut s = sess();
        let l1 = r#"{"type":"system","sessionId":"first"}"#;
        let l2 = r#"{"type":"system","sessionId":"forked-resume"}"#;
        update_from_line(&mut s, l1, false);
        assert_eq!(s.id, "first");
        update_from_line(&mut s, l2, false);
        assert_eq!(s.id, "first"); // do not flip on resume/fork
    }

    #[test]
    fn live_pushes_sample() {
        let mut s = sess();
        // Use a current timestamp so the rate-window retain doesn't evict.
        let now = Utc::now().to_rfc3339();
        let line = format!(
            r#"{{"type":"assistant","timestamp":"{now}","message":{{"usage":{{"input_tokens":7,"output_tokens":3}}}}}}"#
        );
        update_from_line(&mut s, &line, true);
        assert_eq!(s.samples.len(), 1);
        assert_eq!(s.samples.front().unwrap().1, 10);
    }

    #[test]
    fn malformed_json_is_ignored() {
        let mut s = sess();
        update_from_line(&mut s, "{not json", false);
        update_from_line(&mut s, "", false);
        assert_eq!(s.tokens.total(), 0);
        assert_eq!(s.turn_count, 0);
    }

    #[test]
    fn timestamps_track_earliest_and_latest() {
        let mut s = sess();
        update_from_line(
            &mut s,
            r#"{"type":"system","timestamp":"2026-01-02T00:00:00Z"}"#,
            false,
        );
        update_from_line(
            &mut s,
            r#"{"type":"system","timestamp":"2026-01-01T00:00:00Z"}"#,
            false,
        );
        update_from_line(
            &mut s,
            r#"{"type":"system","timestamp":"2026-01-03T00:00:00Z"}"#,
            false,
        );
        assert_eq!(
            s.started_at.unwrap().to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
        assert_eq!(
            s.last_activity.unwrap().to_rfc3339(),
            "2026-01-03T00:00:00+00:00"
        );
    }
}
