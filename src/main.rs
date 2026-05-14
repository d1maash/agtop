mod model;
mod pricing;
mod processes;
mod sources;
mod ui;
mod watcher;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "agtop", version, about = "htop-like TUI for local AI agent sessions")]
struct Cli {
    /// Print sessions as a table and exit (no TUI)
    #[arg(long)]
    once: bool,
    /// Print only the JSONL files of currently-running CLI sessions, then exit.
    #[arg(long)]
    running: bool,
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
    if cli.once {
        let map = sources::initial_scan()?;
        let mut sessions: Vec<_> = map.into_values().collect();
        sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
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
    let shared = watcher::build_initial_state();
    watcher::spawn(shared.clone())?;
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
