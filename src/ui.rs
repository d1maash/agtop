use crate::model::{AgentKind, Session};
use crate::watcher::Shared;
use anyhow::Result;
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
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
}

struct App {
    sort: SortBy,
    state: TableState,
    show_inactive: bool,
    shared: Shared,
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
        }
    }

    fn snapshot(&self) -> Vec<Session> {
        let map = self.shared.lock().unwrap();
        let mut v: Vec<Session> = map.values().cloned().collect();
        let now = Utc::now();
        if !self.show_inactive {
            v.retain(|s| {
                s.last_activity
                    .map(|t| (now - t).num_seconds().abs() <= 60 * 60 * 24)
                    .unwrap_or(false)
            });
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

fn sort_sessions(sessions: &mut [Session], by: SortBy) {
    match by {
        SortBy::LastActivity => sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity)),
        SortBy::Tokens => sessions.sort_by(|a, b| b.tokens.total().cmp(&a.tokens.total())),
        SortBy::Project => sessions.sort_by(|a, b| a.project_name().cmp(&b.project_name())),
        SortBy::Cost => sessions.sort_by(|a, b| {
            b.cost_usd()
                .unwrap_or(0.0)
                .partial_cmp(&a.cost_usd().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortBy::Rate => sessions.sort_by(|a, b| b.tokens_per_min().cmp(&a.tokens_per_min())),
    }
}

fn main_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, shared: Shared) -> Result<()> {
    let mut app = App::new(shared);

    loop {
        let sessions = app.snapshot();
        terminal.draw(|f| draw(f, &mut app, &sessions))?;

        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                let len = app.snapshot().len();
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1, len),
                    KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1, len),
                    KeyCode::Char('t') => app.sort = SortBy::Tokens,
                    KeyCode::Char('a') => app.sort = SortBy::LastActivity,
                    KeyCode::Char('p') => app.sort = SortBy::Project,
                    KeyCode::Char('c') => app.sort = SortBy::Cost,
                    KeyCode::Char('m') => app.sort = SortBy::Rate,
                    KeyCode::Char('A') => app.show_inactive = !app.show_inactive,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &mut App, sessions: &[Session]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, chunks[0], sessions);
    draw_table(f, chunks[1], app, sessions);
    draw_footer(f, chunks[2], app);
}

fn draw_header(f: &mut ratatui::Frame, area: ratatui::layout::Rect, sessions: &[Session]) {
    let total_tokens: u64 = sessions.iter().map(|s| s.tokens.total()).sum();
    let total_cost: f64 = sessions.iter().filter_map(|s| s.cost_usd()).sum();
    let active = sessions.iter().filter(|s| is_active(s)).count();
    let claude_n = sessions.iter().filter(|s| s.kind == AgentKind::Claude).count();
    let codex_n = sessions.iter().filter(|s| s.kind == AgentKind::Codex).count();
    let live_rate: u64 = sessions.iter().map(|s| s.tokens_per_min()).sum();

    let line = Line::from(vec![
        Span::styled("agtop", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        Span::raw(format!("sessions: {}  active: {}  ", sessions.len(), active)),
        Span::styled(format!("claude:{}  ", claude_n), Style::default().fg(Color::Magenta)),
        Span::styled(format!("codex:{}", codex_n), Style::default().fg(Color::Green)),
        Span::raw(format!("   tokens: {}", fmt_num(total_tokens))),
        Span::raw("   "),
        Span::styled(
            format!("${:.2}", total_cost),
            Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            format!("{} tok/min", fmt_num(live_rate)),
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
    sessions: &[Session],
) {
    let header_cells = [
        "SRC", "ID", "PROJECT", "MODEL", "IN", "OUT", "CACHE", "TOTAL", "TOK/MIN", "$", "AGO",
        "STATUS",
    ]
    .iter()
    .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
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
            let status_color = if active { Color::Green } else { Color::DarkGray };

            let rate = s.tokens_per_min();
            let rate_cell = if rate > 0 {
                Cell::from(fmt_num(rate)).style(Style::default().fg(Color::Yellow))
            } else {
                Cell::from("·").style(Style::default().fg(Color::DarkGray))
            };

            let cost_cell = match s.cost_usd() {
                Some(c) if c >= 0.01 => Cell::from(format!("${:.2}", c))
                    .style(Style::default().fg(Color::LightGreen)),
                Some(_) => Cell::from("<$0.01").style(Style::default().fg(Color::DarkGray)),
                None => Cell::from("-").style(Style::default().fg(Color::DarkGray)),
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
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Min(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " sessions ({}) — sort: {} ",
            sessions.len(),
            sort_label(app.sort)
        )))
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(40, 40, 60))
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, area, &mut app.state);
}

fn draw_footer(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    let visibility = if app.show_inactive { "all" } else { "24h" };
    let line = Line::from(vec![
        chip("q"), Span::raw(" quit  "),
        chip("↑↓/jk"), Span::raw(" nav  "),
        chip("t"), Span::raw(" tokens  "),
        chip("c"), Span::raw(" cost  "),
        chip("m"), Span::raw(" rate  "),
        chip("a"), Span::raw(" activity  "),
        chip("p"), Span::raw(" project  "),
        chip("A"), Span::raw(format!(" show:{}", visibility)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn chip(label: &str) -> Span<'_> {
    Span::styled(
        format!(" {} ", label),
        Style::default().bg(Color::DarkGray).fg(Color::White),
    )
}

fn is_active(s: &Session) -> bool {
    s.last_activity
        .map(|t| (Utc::now() - t).num_seconds() <= ACTIVE_WINDOW_SECS)
        .unwrap_or(false)
}

fn format_ago(s: &Session) -> String {
    let Some(t) = s.last_activity else {
        return "-".into();
    };
    let secs = (Utc::now() - t).num_seconds();
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
    }
}
