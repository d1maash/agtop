use crate::model::{AgentKind, SessionView};
use crate::processes::RunningSnapshot;
use crate::watcher::{current, Shared};
use anyhow::Result;
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState};
use ratatui::Terminal;
use std::io::{self, Stdout};
use std::time::Duration;

const TICK: Duration = Duration::from_millis(250);
const ACTIVE_WINDOW_SECS: i64 = 120;

pub fn run(shared: Shared) -> Result<()> {
    let mut terminal = setup()?;
    let res = main_loop(&mut terminal, shared);
    teardown(&mut terminal)?;
    res
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn teardown(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortBy {
    LastActivity,
    Tokens,
    Project,
    Cost,
    Rate,
    Source,
}

struct App {
    sort: SortBy,
    state: TableState,
    show_inactive: bool,
    shared: Shared,
    open_files: crate::processes::OpenFilesWatcher,
    /// When `Some`, the detail overlay is open for the session with this id.
    /// Tracked by id (not row index) so the view survives re-sorts/filters.
    detail_id: Option<String>,
}

impl App {
    fn new(shared: Shared) -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            sort: SortBy::LastActivity,
            state,
            show_inactive: false,
            shared,
            open_files: crate::processes::OpenFilesWatcher::spawn(),
            detail_id: None,
        }
    }

    /// Returns the published snapshot plus a sorted+filtered list of refs
    /// into it. No session bodies are copied here — the Arc bump is the only
    /// shared-state work, and sorting operates on `&SessionView`.
    fn view<'a>(&self, snap: &'a [SessionView]) -> Vec<&'a SessionView> {
        let mut v: Vec<&SessionView> = snap.iter().collect();
        if !self.show_inactive {
            // The watcher precomputes the live set off the render thread, so
            // this never touches the disk. The mtime fallback only knows file
            // write times, so OR in our parsed last-activity to catch a fresh
            // write the OS reports as stale.
            match self.open_files.snapshot() {
                RunningSnapshot::Tracked(open) => v.retain(|s| open.contains(&s.file)),
                RunningSnapshot::Mtime(open) => {
                    v.retain(|s| open.contains(&s.file) || is_active(s))
                }
            }
        }
        sort_sessions(&mut v, self.sort);
        v
    }

    fn move_cursor(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.state.select(None);
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, len as isize - 1) as usize;
        self.state.select(Some(next));
    }
}

fn sort_sessions(sessions: &mut [&SessionView], by: SortBy) {
    match by {
        SortBy::LastActivity => sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity)),
        SortBy::Tokens => sessions.sort_by_key(|s| std::cmp::Reverse(s.tokens.total())),
        SortBy::Project => sessions.sort_by_key(|a| a.project_name()),
        SortBy::Cost => sessions.sort_by(|a, b| {
            b.cost_usd
                .unwrap_or(0.0)
                .partial_cmp(&a.cost_usd.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortBy::Rate => sessions.sort_by_key(|s| std::cmp::Reverse(s.tokens_last_60s)),
        SortBy::Source => sessions.sort_by(|a, b| {
            (a.kind.label(), std::cmp::Reverse(a.last_activity))
                .cmp(&(b.kind.label(), std::cmp::Reverse(b.last_activity)))
        }),
    }
}

fn main_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, shared: Shared) -> Result<()> {
    let mut app = App::new(shared);

    loop {
        let snap = current(&app.shared);
        let total = snap.len();
        let sessions = app.view(&snap);
        let hidden = total.saturating_sub(sessions.len());
        // Resolve the detail target against the full snapshot so it stays open
        // even if the session drops out of the filtered/sorted list.
        let detail = app
            .detail_id
            .as_deref()
            .and_then(|id| snap.iter().find(|s| s.id == id));
        terminal.draw(|f| draw(f, &mut app, &sessions, hidden, detail))?;

        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                // Detail overlay open: it owns the keyboard. Esc/Enter/q close
                // it; everything else is ignored so the list stays put.
                if app.detail_id.is_some() {
                    if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                        app.detail_id = None;
                    }
                    continue;
                }
                let len = sessions.len();
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Enter => {
                        if let Some(s) = app.state.selected().and_then(|i| sessions.get(i)) {
                            app.detail_id = Some(s.id.clone());
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1, len),
                    KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1, len),
                    KeyCode::Char('t') => app.sort = SortBy::Tokens,
                    KeyCode::Char('a') => app.sort = SortBy::LastActivity,
                    KeyCode::Char('p') => app.sort = SortBy::Project,
                    KeyCode::Char('c') => app.sort = SortBy::Cost,
                    KeyCode::Char('m') => app.sort = SortBy::Rate,
                    KeyCode::Char('s') => app.sort = SortBy::Source,
                    KeyCode::Char('A') => app.show_inactive = !app.show_inactive,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn draw(
    f: &mut ratatui::Frame,
    app: &mut App,
    sessions: &[&SessionView],
    hidden: usize,
    detail: Option<&SessionView>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, chunks[0], sessions);
    draw_table(f, chunks[1], app, sessions, hidden);
    draw_footer(f, chunks[2], app);

    if let Some(s) = detail {
        draw_detail(f, s);
    }
}

fn draw_header(f: &mut ratatui::Frame, area: ratatui::layout::Rect, sessions: &[&SessionView]) {
    let total_tokens: u64 = sessions.iter().map(|s| s.tokens.total()).sum();
    let total_cost: f64 = sessions.iter().filter_map(|s| s.cost_usd).sum();
    let active = sessions.iter().filter(|s| is_active(s)).count();
    let claude_n = sessions
        .iter()
        .filter(|s| s.kind == AgentKind::Claude)
        .count();
    let codex_n = sessions
        .iter()
        .filter(|s| s.kind == AgentKind::Codex)
        .count();
    let live_rate: u64 = sessions.iter().map(|s| s.tokens_last_60s).sum();

    let line = Line::from(vec![
        Span::styled(
            "agtop",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::raw(format!(
            "sessions: {}  active: {}  ",
            sessions.len(),
            active
        )),
        Span::styled(
            format!("claude:{}  ", claude_n),
            Style::default().fg(Color::Magenta),
        ),
        Span::styled(
            format!("codex:{}", codex_n),
            Style::default().fg(Color::Green),
        ),
        Span::raw(format!("   tokens: {}", fmt_num(total_tokens))),
        Span::raw("   "),
        Span::styled(
            format!("${:.2}", total_cost),
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            format!("{} tok/60s", fmt_num(live_rate)),
            Style::default().fg(Color::Yellow),
        ),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_table(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    app: &mut App,
    sessions: &[&SessionView],
    hidden: usize,
) {
    let header_cells = [
        "SRC", "ID", "PROJECT", "MODEL", "IN", "OUT", "CACHE", "TOTAL", "CTX", "TOK/60S", "$",
        "AGO", "STATUS",
    ]
    .iter()
    .map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = sessions
        .iter()
        .map(|s| {
            let src_color = match s.kind {
                AgentKind::Claude => Color::Magenta,
                AgentKind::Codex => Color::Green,
            };
            let active = is_active(s);
            let status_text = if active { "● active" } else { "  idle" };
            let status_color = if active {
                Color::Green
            } else {
                Color::DarkGray
            };

            let rate = s.tokens_last_60s;
            let rate_cell = if rate > 0 {
                Cell::from(fmt_num(rate)).style(Style::default().fg(Color::Yellow))
            } else {
                Cell::from("·").style(Style::default().fg(Color::DarkGray))
            };

            let cost_cell = match s.cost_usd {
                Some(c) if c >= 0.01 => {
                    Cell::from(format!("${:.2}", c)).style(Style::default().fg(Color::LightGreen))
                }
                Some(_) => Cell::from("<$0.01").style(Style::default().fg(Color::DarkGray)),
                None => Cell::from("-").style(Style::default().fg(Color::DarkGray)),
            };

            let ctx_cell = match s.context_pct {
                Some(p) => Cell::from(format!("{}%", (p * 100.0).round() as u64))
                    .style(Style::default().fg(context_color(p))),
                None => Cell::from("·").style(Style::default().fg(Color::DarkGray)),
            };

            Row::new(vec![
                Cell::from(s.kind.label()).style(Style::default().fg(src_color)),
                Cell::from(s.short_id()),
                Cell::from(s.project_name()),
                Cell::from(s.model.clone().unwrap_or_else(|| "-".into())),
                Cell::from(fmt_num(s.tokens.input)),
                Cell::from(fmt_num(s.tokens.output)),
                Cell::from(fmt_num(s.tokens.cache_read + s.tokens.cache_creation)),
                Cell::from(fmt_num(s.tokens.total()))
                    .style(Style::default().add_modifier(Modifier::BOLD)),
                ctx_cell,
                rate_cell,
                cost_cell,
                Cell::from(format_ago(s)),
                Cell::from(status_text).style(Style::default().fg(status_color)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(7),
        Constraint::Length(10),
        Constraint::Length(22),
        Constraint::Length(18),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Min(8),
    ];

    let title = if hidden > 0 {
        format!(
            " sessions ({} of {} — {} hidden, press A) — sort: {} ",
            sessions.len(),
            sessions.len() + hidden,
            hidden,
            sort_label(app.sort)
        )
    } else {
        format!(
            " sessions ({}) — sort: {} ",
            sessions.len(),
            sort_label(app.sort)
        )
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 40, 60))
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, area, &mut app.state);
}

fn draw_footer(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    let visibility = if app.show_inactive { "all" } else { "running" };
    let line = Line::from(vec![
        chip("q"),
        Span::raw(" quit  "),
        chip("↑↓/jk"),
        Span::raw(" nav  "),
        chip("⏎"),
        Span::raw(" detail  "),
        chip("t"),
        Span::raw(" tokens  "),
        chip("c"),
        Span::raw(" cost  "),
        chip("m"),
        Span::raw(" rate  "),
        chip("a"),
        Span::raw(" activity  "),
        chip("p"),
        Span::raw(" project  "),
        chip("s"),
        Span::raw(" source  "),
        chip("A"),
        Span::raw(format!(" show:{}", visibility)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn chip(label: &str) -> Span<'_> {
    Span::styled(
        format!(" {} ", label),
        Style::default().bg(Color::DarkGray).fg(Color::White),
    )
}

/// Green well below the limit, yellow approaching it, red near auto-compaction.
fn context_color(pct: f64) -> Color {
    if pct >= 0.9 {
        Color::Red
    } else if pct >= 0.7 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// A centered rect `pct_x`/`pct_y` percent of `area`, for modal overlays.
fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vert[1])[1]
}

/// Modal detail overlay for one session: identity, token breakdown, cost,
/// context-window gauge, and a sparkline of token activity over the retained
/// window. Drawn on top of the table (which it dims via `Clear`).
fn draw_detail(f: &mut ratatui::Frame, s: &SessionView) {
    let area = centered_rect(72, 70, f.area());
    f.render_widget(Clear, area);

    let title = format!(" {} · {} ", s.kind.label(), s.short_id());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Body rows (info) on top, sparkline pinned to the bottom.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(inner);

    let label = |t: &str| Span::styled(format!("{t:<9}"), Style::default().fg(Color::DarkGray));
    let dash = "-".to_string();

    let cost = s
        .cost_usd
        .map(|c| format!("${c:.4}"))
        .unwrap_or_else(|| dash.clone());

    let ctx_line = match (s.context_max, s.context_pct) {
        (Some(max), Some(p)) => Line::from(vec![
            label("context"),
            Span::styled(
                format!(
                    "{} / {}  ({}%)",
                    fmt_num(s.context_used),
                    fmt_num(max),
                    (p * 100.0).round() as u64
                ),
                Style::default()
                    .fg(context_color(p))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        _ => Line::from(vec![
            label("context"),
            Span::styled(
                format!("{} / ?", fmt_num(s.context_used)),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    };

    let info = vec![
        Line::from(vec![
            label("model"),
            Span::raw(s.model.clone().unwrap_or_else(|| dash.clone())),
        ]),
        Line::from(vec![label("project"), Span::raw(s.project_name())]),
        Line::from(vec![
            label("path"),
            Span::styled(
                s.file.display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        ctx_line,
        Line::from(""),
        Line::from(vec![label("input"), Span::raw(fmt_num(s.tokens.input))]),
        Line::from(vec![label("output"), Span::raw(fmt_num(s.tokens.output))]),
        Line::from(vec![
            label("cache r"),
            Span::raw(fmt_num(s.tokens.cache_read)),
        ]),
        Line::from(vec![
            label("cache w"),
            Span::raw(fmt_num(s.tokens.cache_creation)),
        ]),
        Line::from(vec![
            label("total"),
            Span::styled(
                fmt_num(s.tokens.total()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![label("turns"), Span::raw(s.turn_count.to_string())]),
        Line::from(vec![label("cost"), Span::raw(cost)]),
        Line::from(vec![
            label("tok/60s"),
            Span::styled(
                fmt_num(s.tokens_last_60s),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            label("started"),
            Span::raw(
                s.started_at
                    .map(|t| format_ago_secs((Utc::now() - t).num_seconds()))
                    .unwrap_or_else(|| dash.clone()),
            ),
            Span::raw(" ago"),
        ]),
        Line::from(vec![
            label("last"),
            Span::raw(format_ago(s)),
            Span::raw(" ago"),
        ]),
    ];
    f.render_widget(Paragraph::new(info), rows[0]);

    let peak = s.spark.iter().copied().max().unwrap_or(0);
    let spark = Sparkline::default()
        .block(Block::default().borders(Borders::TOP).title(Span::styled(
            format!(" tokens · last 5m (peak {}) ", fmt_num(peak)),
            Style::default().fg(Color::DarkGray),
        )))
        .data(&s.spark)
        .style(Style::default().fg(Color::Cyan));
    f.render_widget(spark, rows[1]);
}

fn is_active(s: &SessionView) -> bool {
    s.last_activity
        .map(|t| (Utc::now() - t).num_seconds() <= ACTIVE_WINDOW_SECS)
        .unwrap_or(false)
}

fn format_ago(s: &SessionView) -> String {
    let Some(t) = s.last_activity else {
        return "-".into();
    };
    format_ago_secs((Utc::now() - t).num_seconds())
}

fn format_ago_secs(secs: i64) -> String {
    if secs < 0 {
        return "now".into();
    }
    if secs < 60 {
        return format!("{}s", secs);
    }
    let m = secs / 60;
    if m < 60 {
        return format!("{}m", m);
    }
    let h = m / 60;
    if h < 48 {
        return format!("{}h", h);
    }
    let d = h / 24;
    format!("{}d", d)
}

fn fmt_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn sort_label(s: SortBy) -> &'static str {
    match s {
        SortBy::LastActivity => "activity",
        SortBy::Tokens => "tokens",
        SortBy::Project => "project",
        SortBy::Cost => "cost",
        SortBy::Rate => "rate",
        SortBy::Source => "source",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_buckets() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(42), "42");
        assert_eq!(fmt_num(999), "999");
        assert_eq!(fmt_num(1_000), "1.0k");
        assert_eq!(fmt_num(1_500), "1.5k");
        assert_eq!(fmt_num(999_999), "1000.0k");
        assert_eq!(fmt_num(1_000_000), "1.0M");
        assert_eq!(fmt_num(2_500_000), "2.5M");
    }

    #[test]
    fn format_ago_buckets() {
        assert_eq!(format_ago_secs(-5), "now");
        assert_eq!(format_ago_secs(0), "0s");
        assert_eq!(format_ago_secs(59), "59s");
        assert_eq!(format_ago_secs(60), "1m");
        assert_eq!(format_ago_secs(3_599), "59m");
        assert_eq!(format_ago_secs(3_600), "1h");
        assert_eq!(format_ago_secs(47 * 3_600), "47h");
        assert_eq!(format_ago_secs(48 * 3_600), "2d");
        assert_eq!(format_ago_secs(10 * 24 * 3_600), "10d");
    }
}
