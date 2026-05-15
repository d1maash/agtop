use crate::pricing::Price;
use chrono::{DateTime, Duration, Utc};
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TokenStats {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub reasoning: u64,
}

impl TokenStats {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation + self.reasoning
    }
}

const RATE_WINDOW_SECS: i64 = 60;
const RATE_RETAIN_SECS: i64 = 300; // bound memory

#[derive(Debug, Clone)]
pub struct Session {
    pub kind: AgentKind,
    pub id: String,
    pub file: PathBuf,
    pub cwd: Option<String>,
    pub model: Option<String>,
    /// Cached pricing for `model`. Resolved once when `model` is set so the
    /// per-render cost path skips the substring matching in `pricing::lookup`.
    pub price: Option<Price>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub tokens: TokenStats,
    pub turn_count: u64,
    pub file_offset: u64,
    /// (event_time, tokens_added_in_event). Capped to RATE_RETAIN_SECS.
    pub samples: VecDeque<(DateTime<Utc>, u64)>,
}

impl Session {
    pub fn new(kind: AgentKind, file: PathBuf) -> Self {
        let id = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        Self {
            kind,
            id,
            file,
            cwd: None,
            model: None,
            price: None,
            started_at: None,
            last_activity: None,
            tokens: TokenStats::default(),
            turn_count: 0,
            file_offset: 0,
            samples: VecDeque::new(),
        }
    }

    /// Set the model name and resolve its price once. Source parsers call
    /// this instead of writing `model` directly so the cached `price` field
    /// stays in sync.
    pub fn set_model(&mut self, model: String) {
        self.price = crate::pricing::lookup(&model);
        self.model = Some(model);
    }

    pub fn short_id(&self) -> String {
        self.id.chars().take(8).collect()
    }

    pub fn project_name(&self) -> String {
        self.cwd
            .as_deref()
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or("-")
            .to_string()
    }

    pub fn push_sample(&mut self, ts: DateTime<Utc>, added: u64) {
        if added == 0 {
            return;
        }
        self.samples.push_back((ts, added));
        let cutoff = Utc::now() - Duration::seconds(RATE_RETAIN_SECS);
        while let Some(&(t, _)) = self.samples.front() {
            if t < cutoff {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Tokens per minute over the last 60 wall-clock seconds.
    pub fn tokens_per_min(&self) -> u64 {
        let cutoff = Utc::now() - Duration::seconds(RATE_WINDOW_SECS);
        self.samples
            .iter()
            .filter(|(t, _)| *t >= cutoff)
            .map(|(_, n)| *n)
            .sum::<u64>()
    }

    pub fn cost_usd(&self) -> Option<f64> {
        let p = self.price?;
        let t = &self.tokens;
        let per = 1_000_000.0;
        // `t.reasoning` is informational only — for both Claude and Codex, the
        // vendor's `output_tokens` already includes reasoning/thinking tokens,
        // so billing it again here would double-charge.
        Some(
            (t.input as f64) * p.input / per
                + (t.output as f64) * p.output / per
                + (t.cache_read as f64) * p.cache_read / per
                + (t.cache_creation as f64) * p.cache_write / per,
        )
    }
}
