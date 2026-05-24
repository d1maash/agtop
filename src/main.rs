mod model;
mod pricing;
mod processes;
mod sources;
mod ui;
mod watcher;

use anyhow::Result;
use clap::Parser;
use model::Session;
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "agtop",
    version,
    about = "htop-like TUI for local AI agent sessions"
)]
struct Cli {
    /// Print sessions as a table and exit (no TUI)
    #[arg(long)]
    once: bool,
    /// Print sessions as JSON and exit (good for scripts, cron, Grafana)
    #[arg(long)]
    json: bool,
    /// Print only the JSONL files of currently-running CLI sessions, then exit.
    #[arg(long)]
    running: bool,
}

/// Flat, serde-friendly projection of a `Session` for `--json`. Kept separate
/// from `Session` so the export schema is explicit and stable, and so the
/// internal-only fields (`price`, `samples`, `file_offset`) stay out of it.
#[derive(Serialize)]
struct SessionJson {
    source: &'static str,
    id: String,
    project: String,
    cwd: Option<String>,
    file: String,
    model: Option<String>,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
    total: u64,
    tokens_last_60s: u64,
    cost_usd: Option<f64>,
    context_used: u64,
    context_max: Option<u64>,
    context_pct: Option<f64>,
    turn_count: u64,
    started_at: Option<String>,
    last_activity: Option<String>,
}

impl SessionJson {
    fn from_session(s: &Session) -> Self {
        Self {
            source: s.kind.label(),
            id: s.id.clone(),
            project: s.project_name(),
            cwd: s.cwd.clone(),
            file: s.file.display().to_string(),
            model: s.model.clone(),
            input: s.tokens.input,
            output: s.tokens.output,
            cache_read: s.tokens.cache_read,
            cache_creation: s.tokens.cache_creation,
            total: s.tokens.total(),
            tokens_last_60s: s.tokens_last_60s(),
            cost_usd: s.cost_usd(),
            context_used: s.last_context_tokens,
            context_max: s.context_window,
            context_pct: s.context_pct(),
            turn_count: s.turn_count,
            started_at: s.started_at.map(|t| t.to_rfc3339()),
            last_activity: s.last_activity.map(|t| t.to_rfc3339()),
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.running {
        match processes::running_session_paths() {
            Some(set) if set.is_empty() => {
                eprintln!("(no running CLI sessions detected)");
            }
            Some(set) => {
                let mut paths: Vec<_> = set.into_iter().collect();
                paths.sort();
                for p in paths {
                    println!("{}", p.display());
                }
            }
            None => {
                eprintln!("ps/lsof unavailable — falling back to heuristic in TUI");
            }
        }
        return Ok(());
    }
    if cli.json {
        let map = sources::initial_scan()?;
        let mut sessions: Vec<_> = map.into_values().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
        let out: Vec<SessionJson> = sessions.iter().map(SessionJson::from_session).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if cli.once {
        let map = sources::initial_scan()?;
        let mut sessions: Vec<_> = map.into_values().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
        println!(
            "{:<7} {:<10} {:<24} {:<18} {:>10} {:>10} {:>10} {:>10} {:>9}",
            "SRC", "ID", "PROJECT", "MODEL", "IN", "OUT", "CACHE", "TOTAL", "$"
        );
        for s in &sessions {
            println!(
                "{:<7} {:<10} {:<24} {:<18} {:>10} {:>10} {:>10} {:>10} {:>9}",
                s.kind.label(),
                s.short_id(),
                truncate(&s.project_name(), 24),
                truncate(s.model.as_deref().unwrap_or("-"), 18),
                s.tokens.input,
                s.tokens.output,
                s.tokens.cache_read + s.tokens.cache_creation,
                s.tokens.total(),
                s.cost_usd()
                    .map(|c| format!("${:.2}", c))
                    .unwrap_or_else(|| "-".into()),
            );
        }
        return Ok(());
    }
    let (shared, map) = watcher::build_initial_state();
    watcher::spawn(shared.clone(), map)?;
    ui::run(shared)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
