mod model;
mod sources;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "atop", about = "htop-like TUI for local AI agent sessions")]
struct Cli {
    /// Print sessions as a table and exit (no TUI)
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.once {
        let mut sessions = sources::scan_all()?;
        sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        println!(
            "{:<7} {:<10} {:<24} {:<18} {:>10} {:>10} {:>10} {:>10}",
            "SRC", "ID", "PROJECT", "MODEL", "IN", "OUT", "CACHE", "TOTAL"
        );
        for s in &sessions {
            println!(
                "{:<7} {:<10} {:<24} {:<18} {:>10} {:>10} {:>10} {:>10}",
                s.kind.label(),
                s.short_id(),
                truncate(&s.project_name(), 24),
                truncate(s.model.as_deref().unwrap_or("-"), 18),
                s.tokens.input,
                s.tokens.output,
                s.tokens.cache_read + s.tokens.cache_creation,
                s.tokens.total(),
            );
        }
        return Ok(());
    }
    ui::run()
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
