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
}

impl TokenStats {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

const RATE_WINDOW_SECS: i64 = 60;
const RATE_RETAIN_SECS: i64 = 300; // bound memory

/// Number of time-buckets in the detail-view sparkline. Each bucket covers
/// `RATE_RETAIN_SECS / SPARK_BUCKETS` seconds of the retained sample window.
pub const SPARK_BUCKETS: usize = 60;

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
    /// Cached context-window size for `model`, resolved alongside `price`.
    pub context_window: Option<u64>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub tokens: TokenStats,
    /// Prompt tokens (input + cache) sent on the most recent turn — i.e. the
    /// current context-window occupancy, as opposed to the lifetime sums in
    /// `tokens`. Parsers overwrite this each turn rather than accumulating.
    pub last_context_tokens: u64,
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
            context_window: None,
            started_at: None,
            last_activity: None,
            tokens: TokenStats::default(),
            last_context_tokens: 0,
            turn_count: 0,
            file_offset: 0,
            samples: VecDeque::new(),
        }
    }

    /// Set the model name and resolve its price and context window once. Source
    /// parsers call this instead of writing `model` directly so the cached
    /// fields stay in sync.
    pub fn set_model(&mut self, model: String) {
        self.price = crate::pricing::lookup(&model);
        self.context_window = crate::pricing::context_window(&model);
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

    /// Sum of token deltas observed in the last 60 wall-clock seconds. Note:
    /// this is a windowed *count*, not a true rate — a single 30k-token burst
    /// 5s ago reads as 30000 here even though the instantaneous rate is much
    /// higher. The UI label "tok/60s" matches this definition.
    pub fn tokens_last_60s(&self) -> u64 {
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
        // Reasoning/thinking tokens aren't tracked separately: for both Claude
        // and Codex the vendor's `output_tokens` already includes them, so
        // they're billed via `t.output` here and counted once in `total()`.
        Some(
            (t.input as f64) * p.input / per
                + (t.output as f64) * p.output / per
                + (t.cache_read as f64) * p.cache_read / per
                + (t.cache_creation as f64) * p.cache_write / per,
        )
    }

    /// Fraction of the model's context window occupied by the last turn's
    /// prompt, in `0.0..` (can exceed 1.0 right before auto-compaction).
    /// `None` when the model's window is unknown.
    pub fn context_pct(&self) -> Option<f64> {
        let max = self.context_window?;
        if max == 0 {
            return None;
        }
        Some(self.last_context_tokens as f64 / max as f64)
    }

    /// Bucket the retained samples into `SPARK_BUCKETS` equal time slices over
    /// the last `RATE_RETAIN_SECS`, oldest → newest, summing tokens per slice.
    /// Feeds `ratatui::widgets::Sparkline` in the detail view.
    pub fn spark(&self) -> Vec<u64> {
        let mut buckets = vec![0u64; SPARK_BUCKETS];
        let now = Utc::now();
        let span = RATE_RETAIN_SECS as f64;
        for (t, n) in &self.samples {
            let age = (now - *t).num_seconds() as f64;
            if !(0.0..=span).contains(&age) {
                continue;
            }
            // age 0 (newest) → last bucket; age == span (oldest) → first.
            let frac = 1.0 - age / span;
            let idx = ((frac * SPARK_BUCKETS as f64) as usize).min(SPARK_BUCKETS - 1);
            buckets[idx] += n;
        }
        buckets
    }

    /// Render-only projection: drops `samples` (the unbounded VecDeque that
    /// makes `Session::clone` expensive) and pre-resolves the time-derived
    /// quantities the UI needs. The watcher hands a `Vec<SessionView>` to the
    /// UI every publish tick (~250 ms), so cloning each session's sample
    /// buffer would do real allocation work for no visible benefit. The
    /// bucketed `spark` (a fixed `SPARK_BUCKETS`-long Vec) is cheap and bounded.
    pub fn view(&self) -> SessionView {
        SessionView {
            kind: self.kind,
            id: self.id.clone(),
            file: self.file.clone(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            started_at: self.started_at,
            last_activity: self.last_activity,
            tokens: self.tokens,
            tokens_last_60s: self.tokens_last_60s(),
            cost_usd: self.cost_usd(),
            context_used: self.last_context_tokens,
            context_max: self.context_window,
            context_pct: self.context_pct(),
            turn_count: self.turn_count,
            spark: self.spark(),
        }
    }
}

/// Snapshot of a `Session` for UI rendering. Cheap to clone — no `VecDeque`,
/// no pricing pointer, no per-event sample history. Times that depend on
/// "now" (rate window, cost) are frozen at the moment of construction.
#[derive(Debug, Clone)]
pub struct SessionView {
    pub kind: AgentKind,
    pub id: String,
    pub file: PathBuf,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub tokens: TokenStats,
    pub tokens_last_60s: u64,
    pub cost_usd: Option<f64>,
    /// Last turn's prompt tokens (current context occupancy).
    pub context_used: u64,
    /// Model context-window size, if known.
    pub context_max: Option<u64>,
    /// `context_used / context_max`, if known.
    pub context_pct: Option<f64>,
    pub turn_count: u64,
    /// Bucketed token activity over the retained window for the sparkline.
    pub spark: Vec<u64>,
}

impl SessionView {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sess() -> Session {
        Session::new(AgentKind::Claude, PathBuf::from("/tmp/x.jsonl"))
    }

    #[test]
    fn push_sample_ignores_zero() {
        let mut s = sess();
        s.push_sample(Utc::now(), 0);
        assert!(s.samples.is_empty());
    }

    #[test]
    fn tokens_last_60s_sums_recent_only() {
        let mut s = sess();
        let now = Utc::now();
        s.push_sample(now - Duration::seconds(120), 1_000); // outside window
        s.push_sample(now - Duration::seconds(30), 500);
        s.push_sample(now - Duration::seconds(10), 200);
        assert_eq!(s.tokens_last_60s(), 700);
    }

    #[test]
    fn push_sample_drops_beyond_retain() {
        let mut s = sess();
        let now = Utc::now();
        // Older than RATE_RETAIN_SECS (300s).
        s.push_sample(now - Duration::seconds(600), 42);
        s.push_sample(now, 1);
        // The stale entry should have been evicted on the second push.
        assert_eq!(s.samples.len(), 1);
        assert_eq!(s.samples.front().unwrap().1, 1);
    }

    #[test]
    fn token_stats_total_sums_all_fields() {
        let t = TokenStats {
            input: 1,
            output: 2,
            cache_read: 4,
            cache_creation: 8,
        };
        assert_eq!(t.total(), 15);
    }

    #[test]
    fn project_name_uses_last_path_segment() {
        let mut s = sess();
        s.cwd = Some("/Users/foo/my-app".into());
        assert_eq!(s.project_name(), "my-app");
        s.cwd = None;
        assert_eq!(s.project_name(), "-");
    }
}
