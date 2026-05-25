//! Offline usage report: aggregate parsed sessions by day, project, and model,
//! plus a top-N-by-cost list. Reuses `sources::initial_scan` so the numbers
//! match the TUI exactly (same parsing, same `pricing.rs` cost table). Drives
//! `agtop report [--since=7d] [--json]`.

use crate::model::Session;
use crate::sources;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Local, Utc};
use serde::Serialize;
use std::collections::HashMap;

/// Parse a window like `7d`, `24h`, `30m`, `90s`, `2w` into a `Duration`.
/// A bare number with no unit is rejected so typos don't silently mean seconds.
pub fn parse_since(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty --since value");
    }
    let (val, unit) = s.split_at(s.len() - 1);
    let n: i64 = val
        .parse()
        .with_context(|| format!("invalid duration '{s}' (try 7d, 24h, 30m)"))?;
    if n < 0 {
        bail!("--since must not be negative: {s}");
    }
    let d = match unit {
        "s" => Duration::seconds(n),
        "m" => Duration::minutes(n),
        "h" => Duration::hours(n),
        "d" => Duration::days(n),
        "w" => Duration::weeks(n),
        other => bail!("unknown duration unit '{other}' (use s, m, h, d, or w)"),
    };
    Ok(d)
}

/// One aggregation bucket (a day, a project, or a model).
#[derive(Default, Serialize)]
struct Bucket {
    key: String,
    sessions: u64,
    tokens: u64,
    cost: f64,
}

#[derive(Serialize)]
struct SessionLine {
    source: &'static str,
    id: String,
    project: String,
    model: Option<String>,
    tokens: u64,
    cost: Option<f64>,
}

#[derive(Serialize)]
struct ReportJson {
    since: Option<String>,
    sessions: u64,
    tokens: u64,
    cost: f64,
    by_day: Vec<Bucket>,
    by_project: Vec<Bucket>,
    by_model: Vec<Bucket>,
    top_sessions: Vec<SessionLine>,
}

const TOP_N: usize = 10;

pub fn run(since: Option<String>, json: bool) -> Result<()> {
    let window = since.as_deref().map(parse_since).transpose()?;
    let cutoff: Option<DateTime<Utc>> = window.map(|d| Utc::now() - d);

    let map = sources::initial_scan()?;
    let sessions: Vec<Session> = map
        .into_values()
        .filter(|s| match cutoff {
            Some(c) => s.last_activity.is_some_and(|t| t >= c),
            None => true,
        })
        .collect();

    let report = build(&sessions, since.clone());

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&report);
    }
    Ok(())
}

fn build(sessions: &[Session], since: Option<String>) -> ReportJson {
    let mut by_day: HashMap<String, Bucket> = HashMap::new();
    let mut by_project: HashMap<String, Bucket> = HashMap::new();
    let mut by_model: HashMap<String, Bucket> = HashMap::new();
    let mut total_tokens = 0u64;
    let mut total_cost = 0.0f64;

    let add = |map: &mut HashMap<String, Bucket>, key: String, tokens: u64, cost: f64| {
        let b = map.entry(key.clone()).or_default();
        b.key = key;
        b.sessions += 1;
        b.tokens += tokens;
        b.cost += cost;
    };

    for s in sessions {
        let tokens = s.tokens.total();
        let cost = s.cost_usd().unwrap_or(0.0);
        total_tokens += tokens;
        total_cost += cost;

        // Whole-session bucketing: a session counts toward the day of its last
        // activity. Sessions rarely span days, so this keeps the report simple
        // without re-reading every line for per-event timestamps.
        let day = s
            .last_activity
            .map(|t| t.with_timezone(&Local).format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unknown".into());
        add(&mut by_day, day, tokens, cost);
        add(&mut by_project, s.project_name(), tokens, cost);
        add(
            &mut by_model,
            s.model.clone().unwrap_or_else(|| "-".into()),
            tokens,
            cost,
        );
    }

    // Days ascending (chronological); project/model by cost descending.
    let mut by_day: Vec<Bucket> = by_day.into_values().collect();
    by_day.sort_by(|a, b| a.key.cmp(&b.key));
    let by_project = sort_by_cost(by_project);
    let by_model = sort_by_cost(by_model);

    let mut ranked: Vec<&Session> = sessions.iter().collect();
    ranked.sort_by(|a, b| {
        b.cost_usd()
            .unwrap_or(0.0)
            .partial_cmp(&a.cost_usd().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top_sessions = ranked
        .into_iter()
        .take(TOP_N)
        .map(|s| SessionLine {
            source: s.kind.label(),
            id: s.short_id(),
            project: s.project_name(),
            model: s.model.clone(),
            tokens: s.tokens.total(),
            cost: s.cost_usd(),
        })
        .collect();

    ReportJson {
        since,
        sessions: sessions.len() as u64,
        tokens: total_tokens,
        cost: total_cost,
        by_day,
        by_project,
        by_model,
        top_sessions,
    }
}

fn sort_by_cost(map: HashMap<String, Bucket>) -> Vec<Bucket> {
    let mut v: Vec<Bucket> = map.into_values().collect();
    v.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    v
}

fn print_text(r: &ReportJson) {
    let window = match &r.since {
        Some(s) => format!("last {s}"),
        None => "all history".to_string(),
    };
    println!("agtop report — {window}");
    println!();
    println!("Totals");
    println!("  sessions   {}", r.sessions);
    println!("  tokens     {}", group_thousands(r.tokens));
    println!("  cost       ${:.2}", r.cost);

    print_buckets("By day", &r.by_day);
    print_buckets("By project", &r.by_project);
    print_buckets("By model", &r.by_model);

    if !r.top_sessions.is_empty() {
        println!();
        println!("Top sessions by cost");
        for s in &r.top_sessions {
            let cost = s
                .cost
                .map(|c| format!("${c:.2}"))
                .unwrap_or_else(|| "-".into());
            println!(
                "  {:>9}  {:<6}  {:<8}  {:<20}  {:<18}  {:>14} tok",
                cost,
                s.source,
                s.id,
                truncate(&s.project, 20),
                truncate(s.model.as_deref().unwrap_or("-"), 18),
                group_thousands(s.tokens),
            );
        }
    }
}

fn print_buckets(title: &str, buckets: &[Bucket]) {
    if buckets.is_empty() {
        return;
    }
    println!();
    println!("{title}");
    for b in buckets {
        println!(
            "  {:<20}  {:>14} tok  {:>10}  {} session{}",
            truncate(&b.key, 20),
            group_thousands(b.tokens),
            format!("${:.2}", b.cost),
            b.sessions,
            if b.sessions == 1 { "" } else { "s" },
        );
    }
}

/// 1234567 -> "1,234,567". Reports favour exact, grouped numbers over the
/// `1.2M` abbreviation the live TUI uses.
fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        // A separator precedes every digit whose distance from the end is a
        // positive multiple of 3.
        if i != 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, Session};
    use chrono::Utc;
    use std::path::PathBuf;

    #[test]
    fn parse_since_units() {
        assert_eq!(parse_since("90s").unwrap(), Duration::seconds(90));
        assert_eq!(parse_since("30m").unwrap(), Duration::minutes(30));
        assert_eq!(parse_since("24h").unwrap(), Duration::hours(24));
        assert_eq!(parse_since("7d").unwrap(), Duration::days(7));
        assert_eq!(parse_since("2w").unwrap(), Duration::weeks(2));
    }

    #[test]
    fn parse_since_rejects_bad_input() {
        assert!(parse_since("").is_err());
        assert!(parse_since("7").is_err()); // no unit
        assert!(parse_since("7y").is_err()); // unknown unit
        assert!(parse_since("xd").is_err()); // non-numeric
        assert!(parse_since("-3d").is_err()); // negative
    }

    #[test]
    fn group_thousands_inserts_separators() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(42), "42");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
        assert_eq!(group_thousands(12_345), "12,345");
    }

    #[test]
    fn build_aggregates_by_project_and_model() {
        let mut a = Session::new(AgentKind::Claude, PathBuf::from("/tmp/a.jsonl"));
        a.cwd = Some("/work/alpha".into());
        a.set_model("claude-opus-4-7".into());
        a.tokens.output = 1_000;
        a.last_activity = Some(Utc::now());

        let mut b = Session::new(AgentKind::Claude, PathBuf::from("/tmp/b.jsonl"));
        b.cwd = Some("/work/alpha".into());
        b.set_model("claude-sonnet-4-6".into());
        b.tokens.output = 500;
        b.last_activity = Some(Utc::now());

        let r = build(&[a, b], None);
        assert_eq!(r.sessions, 2);
        assert_eq!(r.tokens, 1_500);
        // Both sessions share one project bucket; two distinct model buckets.
        assert_eq!(r.by_project.len(), 1);
        assert_eq!(r.by_project[0].key, "alpha");
        assert_eq!(r.by_project[0].sessions, 2);
        assert_eq!(r.by_model.len(), 2);
        // Top sessions ranked by cost, most expensive first (opus > sonnet).
        assert_eq!(r.top_sessions.len(), 2);
        assert_eq!(r.top_sessions[0].model.as_deref(), Some("claude-opus-4-7"));
    }
}
