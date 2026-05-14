use chrono::{DateTime, Utc};
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

#[derive(Debug, Clone)]
pub struct Session {
    pub kind: AgentKind,
    pub id: String,
    pub file: PathBuf,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
    pub tokens: TokenStats,
    pub turn_count: u64,
    pub file_size: u64,
}

impl Session {
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
