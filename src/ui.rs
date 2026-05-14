use crate::model::{AgentKind, Session};
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
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_millis(2000);
const TICK: Duration = Duration::from_millis(100);
const ACTIVE_WINDOW_SECS: i64 = 120;

pub fn run() -> Result<()> {
    let mut terminal = setup()?;
    let res = main_loop(&mut terminal);
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
}

struct App {
    sessions: Vec<Session>,
    state: TableState,
    sort: SortBy,
    last_refresh: Instant,
    show_inactive: bool,
}

impl App {
    fn new() -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            sessions: Vec::new(),
            state,
            sort: SortBy::LastActivity,
            last_refresh: Instant::now() - REFRESH,
            show_inactive: false,
        }
    }

    fn refresh(&mut self) {
        if let Ok(mut s) = crate::sources::scan_all() {
            sort_sessions(&mut s, self.sort);
            self.sessions = s;
            let max = self.visible().len().saturating_sub(1);
            if let Some(cur) = self.state.selected() {
                if cur > max {
                    self.state.select(Some(max));
                }
            }
        }
        self.last_refresh = Instant::now();
    }

    fn visible(&self) -> Vec<&Session> {
        let now = Utc::now();
        self.sessions
            .iter()
            .filter(|s| {
                if self.show_inactive {
                    return true;
                }
                s.last_activity
                    .map(|t| (now - t).num_seconds().abs() <= 60 * 60 * 24)
                    .unwrap_or(false)
            })
            .collect()
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.visible().len();
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
    }
}

fn main_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let mut app = App::new();
    app.refresh();

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1),
                    KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1),
                    KeyCode::Char('r') => app.refresh(),
                    KeyCode::Char('t') => {
                        app.sort = SortBy::Tokens;
                        sort_sessions(&mut app.sessions, app.sort);
                    }
                    KeyCode::Char('a') => {
                        app.sort = SortBy::LastActivity;
                        sort_sessions(&mut app.sessions, app.sort);
                    }
                    KeyCode::Char('p') => {
                        app.sort = SortBy::Project;
                        sort_sessions(&mut app.sessions, app.sort);
                    }
                    KeyCode::Char('A') => {
                        app.show_inactive = !app.show_inactive;
                    }
                    _ => {}
                }
            }
        }

        if app.last_refresh.elapsed() >= REFRESH {
            app.refresh();
        }
    }
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_table(f, chunks[1], app);
    draw_footer(f, chunks[2], app);
}

fn draw_header(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    let visible = app.visible();
    let total_tokens: u64 = visible.iter().map(|s| s.tokens.total()).sum();
    let active = visible
        .iter()
        .filter(|s| is_active(s))
        .count();

    let claude_n = visible.iter().filter(|s| s.kind == AgentKind::Claude).count();
    let codex_n = visible.iter().filter(|s| s.kind == AgentKind::Codex).count();

    let line = Line::from(vec![
        Span::styled("agent-top", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        Span::raw(format!("sessions: {}  active: {}  ", visible.len(), active)),
        Span::styled(format!("claude:{}  ", claude_n), Style::default().fg(Color::Magenta)),
        Span::styled(format!("codex:{}", codex_n), Style::default().fg(Color::Green)),
        Span::raw(format!("   tokens: {}", fmt_num(total_tokens))),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_table(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &mut App) {
    let header_cells = [
        "SRC", "ID", "PROJECT", "MODEL", "IN", "OUT", "CACHE", "TOTAL", "TURNS", "AGO", "STATUS",
    ]
    .iter()
    .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    let visible = app.visible();
    let rows: Vec<Row> = visible
        .iter()
        .map(|s| {
            let src_color = match s.kind {
                AgentKind::Claude => Color::Magenta,
                AgentKind::Codex => Color::Green,
            };
            let active = is_active(s);
            let status_text = if active { "● active" } else { "  idle" };
            let status_color = if active { Color::Green } else { Color::DarkGray };
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
                Cell::from(s.turn_count.to_string()),
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
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Min(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " sessions ({}) — sort: {} ",
            visible.len(),
            sort_label(app.sort)
        )))
        .row_highlight_style(Style::default().bg(Color::Rgb(40, 40, 60)).add_modifier(Modifier::BOLD));

    f.render_stateful_widget(table, area, &mut app.state);
}

fn draw_footer(f: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) {
    let visibility = if app.show_inactive { "all" } else { "24h" };
    let line = Line::from(vec![
        Span::styled(" q ", Style::default().bg(Color::DarkGray)),
        Span::raw(" quit  "),
        Span::styled(" ↑↓/jk ", Style::default().bg(Color::DarkGray)),
        Span::raw(" nav  "),
        Span::styled(" t ", Style::default().bg(Color::DarkGray)),
        Span::raw(" sort:tokens  "),
        Span::styled(" a ", Style::default().bg(Color::DarkGray)),
        Span::raw(" sort:activity  "),
        Span::styled(" p ", Style::default().bg(Color::DarkGray)),
        Span::raw(" sort:project  "),
        Span::styled(" A ", Style::default().bg(Color::DarkGray)),
        Span::raw(format!(" show:{}  ", visibility)),
        Span::styled(" r ", Style::default().bg(Color::DarkGray)),
        Span::raw(" refresh"),
    ]);
    f.render_widget(Paragraph::new(line), area);
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
    }
}
