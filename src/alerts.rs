use crate::model::SessionView;
use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;
use std::io::Write;

/// Context-fill fraction at which a session row gets a subtle warning tint in
/// the table. Hard-coded so the UI cue is always on — the configurable layer is
/// `--notify-on=...` for actual notifications.
pub const ROW_WARN_CONTEXT: f64 = 0.8;
/// Higher tier: row gets a stronger red tint when this close to auto-compaction.
pub const ROW_DANGER_CONTEXT: f64 = 0.9;

/// One threshold that, when crossed by a session (or by the total cost), fires
/// a notification. Parsed from `--notify-on=NAME>VALUE,...`.
#[derive(Clone, Debug, PartialEq)]
pub enum Trigger {
    /// Session `context_pct` strictly above `threshold` (0.0–1.0).
    Context(f64),
    /// Session `cost_usd` strictly above `threshold` (USD).
    Cost(f64),
}

impl Trigger {
    fn key(&self) -> &'static str {
        match self {
            Trigger::Context(_) => "context",
            Trigger::Cost(_) => "cost",
        }
    }

    fn threshold(&self) -> f64 {
        match self {
            Trigger::Context(t) | Trigger::Cost(t) => *t,
        }
    }

    fn matches(&self, s: &SessionView) -> bool {
        match self {
            Trigger::Context(t) => s.context_pct.is_some_and(|p| p > *t),
            Trigger::Cost(t) => s.cost_usd.is_some_and(|c| c > *t),
        }
    }
}

/// Everything the UI needs to apply alert behavior: which triggers to watch,
/// the optional budget ceiling, and the two output channels (bell, desktop).
#[derive(Clone, Debug, Default)]
pub struct AlertConfig {
    pub triggers: Vec<Trigger>,
    pub budget: Option<f64>,
    pub bell: bool,
    pub desktop: bool,
}

impl AlertConfig {
    /// True when nothing is configured — UI hides the `[alerts]` chip.
    pub fn is_quiet(&self) -> bool {
        self.triggers.is_empty() && self.budget.is_none()
    }
}

/// Parse `--notify-on=context>0.9,cost>50` into [`Trigger`]s. Empty input is OK
/// and yields an empty vec, so omitting the flag and passing `""` are
/// interchangeable.
pub fn parse_notify_on(spec: &str) -> Result<Vec<Trigger>> {
    let mut out = Vec::new();
    for raw in spec.split(',') {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        let (lhs, rhs) = s
            .split_once('>')
            .ok_or_else(|| anyhow!("trigger '{s}' missing '>': expected NAME>VALUE"))?;
        let v: f64 = rhs
            .trim()
            .parse()
            .map_err(|_| anyhow!("trigger '{s}': cannot parse threshold '{}' as a number", rhs.trim()))?;
        out.push(match lhs.trim() {
            "context" | "ctx" => Trigger::Context(v),
            "cost" | "$" => Trigger::Cost(v),
            other => bail!("unknown trigger '{other}': expected 'context' or 'cost'"),
        });
    }
    Ok(out)
}

/// One concrete event the UI may emit. Held briefly between detection and
/// dispatch; never stored.
#[derive(Debug, Clone, PartialEq)]
pub enum Alert {
    Session {
        id: String,
        project: String,
        kind: &'static str,
        value: f64,
        threshold: f64,
    },
    Budget {
        total: f64,
        budget: f64,
    },
}

impl Alert {
    pub fn title(&self) -> String {
        match self {
            Alert::Session { kind, .. } => format!("agtop · {kind} threshold"),
            Alert::Budget { .. } => "agtop · budget exceeded".to_string(),
        }
    }

    pub fn body(&self) -> String {
        match self {
            Alert::Session { id, project, kind, value, threshold } if *kind == "context" => format!(
                "{project} ({id}) context {:.0}% > {:.0}%",
                value * 100.0,
                threshold * 100.0
            ),
            Alert::Session { id, project, kind, value, threshold } if *kind == "cost" => {
                format!("{project} ({id}) cost ${:.2} > ${:.2}", value, threshold)
            }
            Alert::Session { id, project, kind, value, threshold } => {
                format!("{project} ({id}) {kind} {value} > {threshold}")
            }
            Alert::Budget { total, budget } => {
                format!("total cost ${:.2} exceeds budget ${:.2}", total, budget)
            }
        }
    }
}

/// Tracks which (session, trigger) pairs are currently firing so each crossing
/// only emits once. A re-crossing in the other direction re-arms the trigger,
/// so it can fire again later — useful when a long session ramps up, gets
/// compacted, and ramps back into the danger zone.
#[derive(Default)]
pub struct AlertState {
    fired: HashSet<(String, usize)>,
    budget_fired: bool,
}

impl AlertState {
    /// Diff `sessions` against the configured triggers and return alerts that
    /// just crossed their threshold. Updates internal state in place.
    pub fn check(
        &mut self,
        cfg: &AlertConfig,
        sessions: &[SessionView],
        total_cost: f64,
    ) -> Vec<Alert> {
        let mut out = Vec::new();
        for (idx, trig) in cfg.triggers.iter().enumerate() {
            for s in sessions {
                let key = (s.id.clone(), idx);
                if trig.matches(s) {
                    if self.fired.insert(key) {
                        out.push(Alert::Session {
                            id: s.short_id(),
                            project: s.project_name(),
                            kind: trig.key(),
                            value: match trig {
                                Trigger::Context(_) => s.context_pct.unwrap_or(0.0),
                                Trigger::Cost(_) => s.cost_usd.unwrap_or(0.0),
                            },
                            threshold: trig.threshold(),
                        });
                    }
                } else {
                    self.fired.remove(&key);
                }
            }
        }
        if let Some(b) = cfg.budget {
            if total_cost > b {
                if !self.budget_fired {
                    self.budget_fired = true;
                    out.push(Alert::Budget { total: total_cost, budget: b });
                }
            } else {
                self.budget_fired = false;
            }
        }
        out
    }
}

/// Write a BEL byte to stdout. Cheap, non-blocking, fine to invoke inside the
/// render loop. Crossterm's raw mode doesn't gate ASCII control bytes, so the
/// terminal bell rings as expected; failures (closed stdout) are ignored.
pub fn ring_bell() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}

/// Fire a system notification for `alert`. Spawns and detaches — no waiting,
/// no error propagation. macOS uses `osascript`, Linux uses `notify-send`,
/// other platforms (Windows, BSDs) are silent for now.
pub fn dispatch_desktop(alert: &Alert) {
    let title = alert.title();
    let body = alert.body();
    #[cfg(target_os = "macos")]
    {
        // `osascript` parses the script as AppleScript; the only meta-character
        // we have to defend against is `"` since the strings come from session
        // metadata (project names, ids) and could contain anything.
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('\\', "\\\\").replace('"', "\\\""),
            title.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .arg(&title)
            .arg(&body)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (&title, &body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, TokenStats};
    use chrono::Utc;
    use std::path::PathBuf;

    fn view(id: &str, ctx: Option<f64>, cost: Option<f64>) -> SessionView {
        SessionView {
            kind: AgentKind::Claude,
            id: id.to_string(),
            file: PathBuf::from("/tmp/x.jsonl"),
            cwd: Some("/w/x".into()),
            model: Some("claude-opus-4-7".into()),
            started_at: Some(Utc::now()),
            last_activity: Some(Utc::now()),
            tokens: TokenStats::default(),
            tokens_last_60s: 0,
            cost_usd: cost,
            context_used: 0,
            context_max: Some(200_000),
            context_pct: ctx,
            turn_count: 0,
            spark: vec![],
        }
    }

    #[test]
    fn parse_notify_on_handles_both_keys_and_ignores_blanks() {
        let trigs = parse_notify_on("context>0.9, cost>50, ").unwrap();
        assert_eq!(
            trigs,
            vec![Trigger::Context(0.9), Trigger::Cost(50.0)]
        );
        assert_eq!(parse_notify_on("").unwrap(), Vec::<Trigger>::new());
        // `ctx` and `$` are recognized aliases.
        let trigs = parse_notify_on("ctx>0.5,$>1.5").unwrap();
        assert_eq!(trigs, vec![Trigger::Context(0.5), Trigger::Cost(1.5)]);
    }

    #[test]
    fn parse_notify_on_rejects_garbage() {
        assert!(parse_notify_on("context").is_err());
        assert!(parse_notify_on("context>oops").is_err());
        assert!(parse_notify_on("speed>10").is_err());
    }

    #[test]
    fn alert_state_fires_on_rising_edge_only() {
        let cfg = AlertConfig {
            triggers: vec![Trigger::Context(0.8)],
            ..Default::default()
        };
        let mut state = AlertState::default();
        let s = view("a", Some(0.85), None);
        let first = state.check(&cfg, std::slice::from_ref(&s), 0.0);
        assert_eq!(first.len(), 1);
        // Still above threshold next tick → no second fire.
        let second = state.check(&cfg, std::slice::from_ref(&s), 0.0);
        assert!(second.is_empty());
        // Drop below, then back above → re-arms.
        let cooler = view("a", Some(0.5), None);
        state.check(&cfg, std::slice::from_ref(&cooler), 0.0);
        let hot = view("a", Some(0.95), None);
        let third = state.check(&cfg, std::slice::from_ref(&hot), 0.0);
        assert_eq!(third.len(), 1);
    }

    #[test]
    fn alert_state_budget_fires_once_per_crossing() {
        let cfg = AlertConfig {
            budget: Some(10.0),
            ..Default::default()
        };
        let mut state = AlertState::default();
        let s = view("a", None, None);
        assert_eq!(state.check(&cfg, std::slice::from_ref(&s), 5.0).len(), 0);
        assert_eq!(state.check(&cfg, std::slice::from_ref(&s), 12.0).len(), 1);
        assert_eq!(state.check(&cfg, std::slice::from_ref(&s), 15.0).len(), 0);
        assert_eq!(state.check(&cfg, std::slice::from_ref(&s), 8.0).len(), 0);
        assert_eq!(state.check(&cfg, std::slice::from_ref(&s), 99.0).len(), 1);
    }

    #[test]
    fn alert_body_renders_known_kinds_pretty() {
        let a = Alert::Session {
            id: "abc12345".into(),
            project: "atop".into(),
            kind: "context",
            value: 0.93,
            threshold: 0.9,
        };
        assert_eq!(a.body(), "atop (abc12345) context 93% > 90%");
        let b = Alert::Budget { total: 25.5, budget: 20.0 };
        assert_eq!(b.body(), "total cost $25.50 exceeds budget $20.00");
    }
}
