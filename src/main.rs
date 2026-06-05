mod alerts;
mod model;
mod pricing;
mod processes;
mod report;
mod sources;
mod ui;
mod watcher;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
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
    /// Only parse session files modified within this window at startup
    /// (e.g. 7d, 24h, 30m). Defaults to 30d. Older files are picked up
    /// lazily when they become active. Use `--all` to disable filtering.
    #[arg(long, value_name = "WINDOW", default_value = "30d")]
    scan_since: String,
    /// Parse every session file at startup regardless of mtime. Slower on
    /// machines with thousands of historical sessions.
    #[arg(long)]
    all: bool,
    /// Comma-separated alert thresholds. Each entry is `NAME>VALUE` with NAME
    /// being `context` (fraction 0..1) or `cost` (USD). Example:
    /// `--notify-on=context>0.9,cost>50`. Fires a desktop notification (macOS
    /// `osascript`, Linux `notify-send`) once per rising-edge crossing.
    #[arg(long, value_name = "SPEC", default_value = "")]
    notify_on: String,
    /// Daily cost ceiling in USD. The header `$` total turns red when it goes
    /// above this, and one notification fires per crossing (pair with
    /// `--scan-since=24h` for a real daily window).
    #[arg(long, value_name = "USD")]
    budget: Option<f64>,
    /// Ring the terminal bell (`\\x07`) when an alert fires.
    #[arg(long)]
    bell: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Offline usage report aggregated by day, project, and model.
    Report {
        /// Only count sessions active within this window (e.g. 7d, 24h, 30m).
        /// Omit for all history.
        #[arg(long)]
        since: Option<String>,
        /// Emit JSON instead of the text report.
        #[arg(long)]
        json: bool,
    },
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
    if let Some(Command::Report { since, json }) = cli.command {
        return report::run(since, json);
    }
    let cutoff: Option<DateTime<Utc>> = resolve_cutoff(cli.all, &cli.scan_since)?;
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
        let map = sources::initial_scan_since(cutoff)?;
        let mut sessions: Vec<_> = map.into_values().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity));
        let out: Vec<SessionJson> = sessions.iter().map(SessionJson::from_session).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if cli.once {
        let map = sources::initial_scan_since(cutoff)?;
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
    let triggers = alerts::parse_notify_on(&cli.notify_on)?;
    let alert_cfg = alerts::AlertConfig {
        // `--notify-on` implies desktop notifications; the user only typed
        // thresholds, no separate opt-in needed. `--bell` is independent so
        // bell-only setups work without `--notify-on`.
        desktop: !triggers.is_empty(),
        triggers,
        budget: cli.budget,
        bell: cli.bell,
    };
    let (shared, map) = watcher::build_initial_state(cutoff);
    watcher::spawn(shared.clone(), map, cutoff)?;
    ui::run(shared, alert_cfg)
}

/// Translate `--all` / `--scan-since` into the optional UTC cutoff that the
/// watcher and initial-scan share. `--all` wins over `--scan-since`. Empty or
/// `"all"` values for `--scan-since` also mean "no filter" so users can opt
/// out without remembering a second flag.
fn resolve_cutoff(all: bool, scan_since: &str) -> Result<Option<DateTime<Utc>>> {
    if all {
        return Ok(None);
    }
    let trimmed = scan_since.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let dur: Duration = report::parse_since(trimmed)?;
    Ok(Some(Utc::now() - dur))
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
