use crate::alerts::{
    self, AlertConfig, AlertState, ROW_DANGER_CONTEXT, ROW_WARN_CONTEXT,
};
use crate::model::{AgentKind, SessionView};
use crate::processes::RunningSnapshot;
use crate::watcher::{current, Shared, Snapshot};
use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState};
use ratatui::Terminal;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};
use std::time::Duration;

const TICK: Duration = Duration::from_millis(250);
const ACTIVE_WINDOW_SECS: i64 = 120;

pub fn run(shared: Shared, alerts: AlertConfig) -> Result<()> {
    let mut terminal = setup()?;
    let res = main_loop(&mut terminal, shared, alerts);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Filtering,
}

/// One column of the session table. The set actually rendered is chosen at draw
/// time by [`visible_columns`] based on terminal width, and both data rows and
/// group-subtotal rows render against that same list so they stay aligned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Src,
    Id,
    Project,
    Model,
    In,
    Out,
    Cache,
    Total,
    Ctx,
    Rate,
    Cost,
    Ago,
    Status,
}

/// Every column, in the left-to-right order they appear when all are shown.
const CANONICAL: [Col; 13] = [
    Col::Src,
    Col::Id,
    Col::Project,
    Col::Model,
    Col::In,
    Col::Out,
    Col::Cache,
    Col::Total,
    Col::Ctx,
    Col::Rate,
    Col::Cost,
    Col::Ago,
    Col::Status,
];

impl Col {
    fn header(self) -> &'static str {
        match self {
            Col::Src => "SRC",
            Col::Id => "ID",
            Col::Project => "PROJECT",
            Col::Model => "MODEL",
            Col::In => "IN",
            Col::Out => "OUT",
            Col::Cache => "CACHE",
            Col::Total => "TOTAL",
            Col::Ctx => "CTX",
            Col::Rate => "TOK/60S",
            Col::Cost => "$",
            Col::Ago => "AGO",
            Col::Status => "STATUS",
        }
    }

    fn width(self) -> u16 {
        match self {
            Col::Src => 7,
            Col::Id => 10,
            Col::Project => 22,
            Col::Model => 18,
            Col::In => 9,
            Col::Out => 9,
            Col::Cache => 9,
            Col::Total => 10,
            Col::Ctx => 5,
            Col::Rate => 9,
            Col::Cost => 8,
            Col::Ago => 7,
            Col::Status => 8,
        }
    }

    fn constraint(self) -> Constraint {
        match self {
            // Status soaks up any leftover width so the table fills the frame.
            Col::Status => Constraint::Min(self.width()),
            _ => Constraint::Length(self.width()),
        }
    }
}

/// Pick the columns that fit in `avail` cells. A small essential set is always
/// kept; optional columns are added by descending value until space runs out,
/// so on a narrow terminal CACHE/IN/OUT drop first and the core identity + cost
/// columns survive. The returned list keeps canonical left-to-right order.
fn visible_columns(avail: u16) -> Vec<Col> {
    let essential = [
        Col::Src,
        Col::Project,
        Col::Total,
        Col::Cost,
        Col::Ago,
        Col::Status,
    ];
    // Optional columns, most valuable first → CACHE/IN/OUT are the first to go.
    let optional = [
        Col::Ctx,
        Col::Rate,
        Col::Model,
        Col::Id,
        Col::Out,
        Col::In,
        Col::Cache,
    ];

    let spacing = |n: usize| n.saturating_sub(1) as u16; // 1 cell between columns
    let mut included: Vec<Col> = essential.to_vec();
    let mut used: u16 = essential.iter().map(|c| c.width()).sum::<u16>() + spacing(essential.len());

    for c in optional {
        let extra = c.width() + 1; // the column plus one spacing cell
        if used + extra <= avail {
            included.push(c);
            used += extra;
        }
    }
    CANONICAL
        .iter()
        .copied()
        .filter(|c| included.contains(c))
        .collect()
}

/// A subtotal header for a project group in the tree view.
struct GroupHeader {
    project: String,
    count: usize,
    tokens: u64,
    cost: f64,
    collapsed: bool,
}

/// A single rendered table row: either a session or, in grouped mode, a project
/// subtotal header. The selection cursor moves over these, so Enter can mean
/// "open detail" on a session or "collapse/expand" on a header.
enum DisplayRow<'a> {
    Group(GroupHeader),
    Session(&'a SessionView),
}

struct App {
    sort: SortBy,
    /// Invert the active sort order. Toggled by pressing the current sort key.
    sort_reverse: bool,
    state: TableState,
    show_inactive: bool,
    shared: Shared,
    open_files: crate::processes::OpenFilesWatcher,
    /// When `Some`, the detail overlay is open for the session with this id.
    /// Tracked by id (not row index) so the view survives re-sorts/filters.
    detail_id: Option<String>,
    /// Full-screen key reference overlay.
    show_help: bool,
    /// Freeze the displayed snapshot so fast-moving numbers can be read.
    paused: bool,
    /// The snapshot captured at the moment of pausing, shown while paused.
    frozen: Option<Snapshot>,
    /// Group sessions by project with subtotal rows (the tree view).
    group: bool,
    /// Project names whose group is collapsed (children hidden).
    collapsed: HashSet<String>,
    /// Current input mode (normal navigation vs. typing a filter).
    input_mode: InputMode,
    /// Active substring filter (case-insensitive). Empty means no filter.
    filter: String,
    /// Threshold + notification configuration from CLI flags.
    alerts: AlertConfig,
    /// Rising-edge tracker for the alert dispatch loop.
    alert_state: AlertState,
}

impl App {
    fn new(shared: Shared, alerts: AlertConfig) -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            sort: SortBy::LastActivity,
            sort_reverse: false,
            state,
            show_inactive: false,
            shared,
            open_files: crate::processes::OpenFilesWatcher::spawn(),
            detail_id: None,
            show_help: false,
            paused: false,
            frozen: None,
            group: false,
            collapsed: HashSet::new(),
            input_mode: InputMode::Normal,
            filter: String::new(),
            alerts,
            alert_state: AlertState::default(),
        }
    }

    /// Select a sort key, or invert direction if it's already active.
    fn set_sort(&mut self, by: SortBy) {
        if self.sort == by {
            self.sort_reverse = !self.sort_reverse;
        } else {
            self.sort = by;
            self.sort_reverse = false;
        }
    }

    fn toggle_collapse(&mut self, project: &str) {
        if !self.collapsed.remove(project) {
            self.collapsed.insert(project.to_string());
        }
    }

    /// Returns a sorted+filtered list of refs into the snapshot. No session
    /// bodies are copied — sorting operates on `&SessionView`.
    fn view<'a>(&self, snap: &'a [SessionView], now: DateTime<Utc>) -> Vec<&'a SessionView> {
        let mut v: Vec<&SessionView> = snap.iter().collect();
        if !self.show_inactive {
            // The watcher precomputes the live set off the render thread, so
            // this never touches the disk. The mtime fallback only knows file
            // write times, so OR in our parsed last-activity to catch a fresh
            // write the OS reports as stale.
            match self.open_files.snapshot() {
                RunningSnapshot::Tracked(open) => v.retain(|s| open.contains(&s.file)),
                RunningSnapshot::Mtime(open) => {
                    v.retain(|s| open.contains(&s.file) || is_active(s, now))
                }
            }
        }

        // Live substring filter (case-insensitive). Matches project, model,
        // short id, or full cwd. Applied after the running-only filter.
        if !self.filter.is_empty() {
            let f = self.filter.to_lowercase();
            v.retain(|s| {
                s.project_name().to_lowercase().contains(&f)
                    || s.model.as_deref().unwrap_or("").to_lowercase().contains(&f)
                    || s.short_id().to_lowercase().contains(&f)
                    || s.cwd.as_deref().unwrap_or("").to_lowercase().contains(&f)
            });
        }

        sort_sessions(&mut v, self.sort, self.sort_reverse);
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

/// Expand a sorted session list into the rows to render. Flat unless `group`,
/// in which case sessions are bucketed by project (preserving first-seen order)
/// with a subtotal header before each bucket; collapsed buckets hide children.
fn build_display<'a>(
    sessions: &[&'a SessionView],
    group: bool,
    collapsed: &HashSet<String>,
) -> Vec<DisplayRow<'a>> {
    if !group {
        return sessions.iter().map(|&s| DisplayRow::Session(s)).collect();
    }

    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&'a SessionView>> = HashMap::new();
    for s in sessions {
        let project = s.project_name();
        match groups.entry(project.clone()) {
            Entry::Occupied(mut e) => e.get_mut().push(*s),
            Entry::Vacant(e) => {
                order.push(project);
                e.insert(vec![*s]);
            }
        }
    }

    let mut out = Vec::new();
    for project in order {
        let members = &groups[&project];
        let tokens = members.iter().map(|s| s.tokens.total()).sum();
        let cost = members.iter().filter_map(|s| s.cost_usd).sum();
        let is_collapsed = collapsed.contains(&project);
        out.push(DisplayRow::Group(GroupHeader {
            project: project.clone(),
            count: members.len(),
            tokens,
            cost,
            collapsed: is_collapsed,
        }));
        if !is_collapsed {
            out.extend(members.iter().map(|&s| DisplayRow::Session(s)));
        }
    }
    out
}

fn sort_sessions(sessions: &mut [&SessionView], by: SortBy, reverse: bool) {
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
    if reverse {
        sessions.reverse();
    }
}

fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    shared: Shared,
    alerts: AlertConfig,
) -> Result<()> {
    let mut app = App::new(shared, alerts);

    loop {
        // While paused, keep showing the frozen snapshot instead of the latest.
        let snap = if app.paused {
            app.frozen.clone().unwrap_or_else(|| current(&app.shared))
        } else {
            current(&app.shared)
        };
        let total = snap.len();
        // Capture `now` once per tick: filtering, sorting, and rendering all
        // read the same instant so nothing flips mid-frame.
        let now = Utc::now();
        let sessions = app.view(&snap, now);
        let hidden = total.saturating_sub(sessions.len());
        let display = build_display(&sessions, app.group, &app.collapsed);

        // Keep the cursor inside the current row set after re-sorts/filters.
        let len = display.len();
        if len == 0 {
            app.state.select(None);
        } else {
            let i = app.state.selected().unwrap_or(0).min(len - 1);
            app.state.select(Some(i));
        }

        // Resolve the detail target against the full snapshot so it stays open
        // even if the session drops out of the filtered/sorted list.
        let detail = app
            .detail_id
            .as_deref()
            .and_then(|id| snap.iter().find(|s| s.id == id));

        // Alert dispatch runs against the live, unfiltered snapshot so a hidden
        // session can still trigger a notification — the user wants to know
        // even if they're focused elsewhere. Pause silences alerts to match the
        // "freeze the display" intent.
        if !app.paused && !app.alerts.is_quiet() {
            let total_cost: f64 = snap.iter().filter_map(|s| s.cost_usd).sum();
            let fired = app.alert_state.check(&app.alerts, &snap, total_cost);
            for a in &fired {
                if app.alerts.bell {
                    alerts::ring_bell();
                }
                if app.alerts.desktop {
                    alerts::dispatch_desktop(a);
                }
            }
        }

        // Budget vs. *displayed* total drives the header color. The dispatch
        // above uses the unfiltered total; the visual cue tracks what the user
        // is actually looking at, which is intuitive when filters are active.
        let displayed_cost: f64 = sessions.iter().filter_map(|s| s.cost_usd).sum();
        let over_budget = app
            .alerts
            .budget
            .map(|b| displayed_cost > b)
            .unwrap_or(false);

        terminal.draw(|f| {
            draw(f, &mut app, &sessions, &display, hidden, detail, now, over_budget)
        })?;

        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                // Help overlay owns the keyboard: ?/Esc/q/Enter close it.
                if app.show_help {
                    if matches!(
                        k.code,
                        KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter
                    ) {
                        app.show_help = false;
                    }
                    continue;
                }
                // Detail overlay open: Esc/Enter/q close it; rest is ignored.
                if app.detail_id.is_some() {
                    if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                        app.detail_id = None;
                    }
                    continue;
                }

                // Filter input mode: capture typing, backspace, and exit keys.
                if app.input_mode == InputMode::Filtering {
                    match k.code {
                        KeyCode::Esc | KeyCode::Enter => {
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Backspace => {
                            app.filter.pop();
                        }
                        KeyCode::Char(c) => {
                            // Ignore control chars that slip through.
                            if !c.is_control() {
                                app.filter.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Normal mode key handling.
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if !app.filter.is_empty() {
                            // Esc clears active filter instead of quitting.
                            app.filter.clear();
                        } else {
                            break;
                        }
                    }
                    KeyCode::Char('?') => app.show_help = true,
                    KeyCode::Char(' ') => {
                        app.paused = !app.paused;
                        app.frozen = if app.paused { Some(snap.clone()) } else { None };
                    }
                    KeyCode::Enter => {
                        if let Some(row) = app.state.selected().and_then(|i| display.get(i)) {
                            match row {
                                DisplayRow::Session(s) => app.detail_id = Some(s.id.clone()),
                                DisplayRow::Group(g) => {
                                    let p = g.project.clone();
                                    app.toggle_collapse(&p);
                                }
                            }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1, len),
                    KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1, len),
                    KeyCode::Char('t') => app.set_sort(SortBy::Tokens),
                    KeyCode::Char('a') => app.set_sort(SortBy::LastActivity),
                    KeyCode::Char('p') => app.set_sort(SortBy::Project),
                    KeyCode::Char('c') => app.set_sort(SortBy::Cost),
                    KeyCode::Char('m') => app.set_sort(SortBy::Rate),
                    KeyCode::Char('s') => app.set_sort(SortBy::Source),
                    KeyCode::Char('g') => app.group = !app.group,
                    KeyCode::Char('A') => app.show_inactive = !app.show_inactive,
                    KeyCode::Char('/') => {
                        app.input_mode = InputMode::Filtering;
                    }
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
    display: &[DisplayRow],
    hidden: usize,
    detail: Option<&SessionView>,
    now: DateTime<Utc>,
) {
    let show_filter_bar = app.input_mode == InputMode::Filtering || !app.filter.is_empty();

    let chunks = if show_filter_bar {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(1), // filter bar
                Constraint::Length(1), // footer
            ])
            .split(f.area())
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(f.area())
    };

    draw_header(f, chunks[0], sessions, now);
    draw_table(
        f,
        chunks[1],
        &mut app.state,
        app.sort,
        app.sort_reverse,
        app.group,
        display,
        sessions.len(),
        hidden,
        now,
    );

    if show_filter_bar {
        draw_filter_bar(f, chunks[2], app);
        draw_footer(f, chunks[3], app);
    } else {
        draw_footer(f, chunks[2], app);
    }

    if let Some(s) = detail {
        draw_detail(f, s, now);
    }
    if app.show_help {
        draw_help(f);
    }
}

/// Append `span` if it fits within `avail` (or `force`), tracking used width.
fn push_if_fits(
    spans: &mut Vec<Span<'static>>,
    used: &mut u16,
    avail: u16,
    span: Span<'static>,
    force: bool,
) {
    let w = span.content.chars().count() as u16;
    if force || *used + w <= avail {
        *used += w;
        spans.push(span);
    }
}

fn draw_header(f: &mut ratatui::Frame, area: Rect, sessions: &[&SessionView], now: DateTime<Utc>) {
    let avail = area.width.saturating_sub(2); // inside the border
    let total_tokens: u64 = sessions.iter().map(|s| s.tokens.total()).sum();
    let total_cost: f64 = sessions.iter().filter_map(|s| s.cost_usd).sum();
    let active = sessions.iter().filter(|s| is_active(s, now)).count();
    let claude_n = sessions
        .iter()
        .filter(|s| s.kind == AgentKind::Claude)
        .count();
    let codex_n = sessions
        .iter()
        .filter(|s| s.kind == AgentKind::Codex)
        .count();
    let live_rate: u64 = sessions.iter().map(|s| s.tokens_last_60s).sum();

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0u16;
    // `agtop` and the session/active counts always show; the rest is added in
    // priority order while there's room, so a narrow terminal keeps the title.
    push_if_fits(
        &mut spans,
        &mut used,
        avail,
        Span::styled(
            "agtop",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        true,
    );
    push_if_fits(
        &mut spans,
        &mut used,
        avail,
        Span::raw(format!(
            "   sessions: {}  active: {}",
            sessions.len(),
            active
        )),
        true,
    );
    push_if_fits(
        &mut spans,
        &mut used,
        avail,
        Span::styled(
            format!("   ${:.2}", total_cost),
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        ),
        false,
    );
    push_if_fits(
        &mut spans,
        &mut used,
        avail,
        Span::raw(format!("   tokens: {}", fmt_num(total_tokens))),
        false,
    );
    push_if_fits(
        &mut spans,
        &mut used,
        avail,
        Span::styled(
            format!("   {} tok/60s", fmt_num(live_rate)),
            Style::default().fg(Color::Yellow),
        ),
        false,
    );
    // claude/codex breakdown is lowest priority; add the pair together so the
    // counts never appear half-shown.
    let cc1 = Span::styled(
        format!("   claude:{}", claude_n),
        Style::default().fg(Color::Magenta),
    );
    let cc2 = Span::styled(
        format!("  codex:{}", codex_n),
        Style::default().fg(Color::Green),
    );
    let cc_w = (cc1.content.chars().count() + cc2.content.chars().count()) as u16;
    if used + cc_w <= avail {
        spans.push(cc1);
        spans.push(cc2);
    }

    let p = Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

// The arguments here are all UI state the table renders against (sort key,
// direction, group flag, hidden count) — bundling them into a struct would
// just push the same fields through one level of indirection, so the lint is
// silenced rather than refactored around.
#[allow(clippy::too_many_arguments)]
fn draw_table(
    f: &mut ratatui::Frame,
    area: Rect,
    state: &mut TableState,
    sort: SortBy,
    sort_reverse: bool,
    group: bool,
    display: &[DisplayRow],
    session_count: usize,
    hidden: usize,
    now: DateTime<Utc>,
) {
    let cols = visible_columns(area.width.saturating_sub(2));

    let header = Row::new(cols.iter().map(|c| {
        Cell::from(c.header()).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    }))
    .height(1);

    let rows: Vec<Row> = display
        .iter()
        .map(|d| match d {
            DisplayRow::Session(s) => Row::new(session_cells(s, &cols, now)),
            DisplayRow::Group(g) => {
                Row::new(group_cells(g, &cols)).style(Style::default().bg(Color::Rgb(30, 30, 46)))
            }
        })
        .collect();

    let widths: Vec<Constraint> = cols.iter().map(|c| c.constraint()).collect();

    let dir = if sort_reverse { "▲" } else { "▼" };
    let grouped = if group { ", grouped" } else { "" };
    let title = if hidden > 0 {
        format!(
            " sessions ({} of {} — {} hidden, A) — sort: {} {}{} ",
            session_count,
            session_count + hidden,
            hidden,
            sort_label(sort),
            dir,
            grouped
        )
    } else {
        format!(
            " sessions ({}) — sort: {} {}{} ",
            session_count,
            sort_label(sort),
            dir,
            grouped
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

    f.render_stateful_widget(table, area, state);
}

fn session_cells(s: &SessionView, cols: &[Col], now: DateTime<Utc>) -> Vec<Cell<'static>> {
    cols.iter().map(|c| session_cell(s, *c, now)).collect()
}

fn session_cell(s: &SessionView, col: Col, now: DateTime<Utc>) -> Cell<'static> {
    match col {
        Col::Src => {
            let c = match s.kind {
                AgentKind::Claude => Color::Magenta,
                AgentKind::Codex => Color::Green,
            };
            Cell::from(s.kind.label()).style(Style::default().fg(c))
        }
        Col::Id => Cell::from(s.short_id()),
        Col::Project => Cell::from(s.project_name()),
        Col::Model => Cell::from(s.model.clone().unwrap_or_else(|| "-".into())),
        Col::In => Cell::from(fmt_num(s.tokens.input)),
        Col::Out => Cell::from(fmt_num(s.tokens.output)),
        Col::Cache => Cell::from(fmt_num(s.tokens.cache_read + s.tokens.cache_creation)),
        Col::Total => Cell::from(fmt_num(s.tokens.total()))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        Col::Ctx => match s.context_pct {
            Some(p) => Cell::from(format!("{}%", (p * 100.0).round() as u64))
                .style(Style::default().fg(context_color(p))),
            None => Cell::from("·").style(Style::default().fg(Color::DarkGray)),
        },
        Col::Rate => {
            let rate = s.tokens_last_60s;
            if rate > 0 {
                Cell::from(fmt_num(rate)).style(Style::default().fg(Color::Yellow))
            } else {
                Cell::from("·").style(Style::default().fg(Color::DarkGray))
            }
        }
        Col::Cost => match s.cost_usd {
            Some(c) if c >= 0.01 => {
                Cell::from(format!("${:.2}", c)).style(Style::default().fg(Color::LightGreen))
            }
            Some(_) => Cell::from("<$0.01").style(Style::default().fg(Color::DarkGray)),
            None => Cell::from("-").style(Style::default().fg(Color::DarkGray)),
        },
        Col::Ago => Cell::from(format_ago(s, now)),
        Col::Status => {
            let active = is_active(s, now);
            let text = if active { "● active" } else { "  idle" };
            let color = if active {
                Color::Green
            } else {
                Color::DarkGray
            };
            Cell::from(text).style(Style::default().fg(color))
        }
    }
}

fn group_cells(g: &GroupHeader, cols: &[Col]) -> Vec<Cell<'static>> {
    let arrow = if g.collapsed { "▸" } else { "▾" };
    cols.iter()
        .map(|c| match c {
            Col::Src => Cell::from(arrow).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Col::Project => Cell::from(format!("{} ({})", g.project, g.count)).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Col::Total => {
                Cell::from(fmt_num(g.tokens)).style(Style::default().add_modifier(Modifier::BOLD))
            }
            Col::Cost => {
                Cell::from(format!("${:.2}", g.cost)).style(Style::default().fg(Color::LightGreen))
            }
            _ => Cell::from(""),
        })
        .collect()
}

fn draw_footer(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let dir = if app.sort_reverse { "▲" } else { "▼" };
    let visibility = if app.show_inactive { "all" } else { "running" };
    let mut spans = vec![
        chip("q"),
        Span::raw(" quit  "),
        chip("↑↓/jk"),
        Span::raw(" nav  "),
        chip("⏎"),
        Span::raw(" detail  "),
        chip("g"),
        Span::raw(" group  "),
        chip("space"),
        Span::raw(" pause  "),
        chip("?"),
        Span::raw(" help   "),
        Span::styled(
            format!("sort:{} {}  show:{}", sort_label(app.sort), dir, visibility),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if app.group {
        spans.push(Span::styled(
            "  [grouped]",
            Style::default().fg(Color::Cyan),
        ));
    }
    if app.paused {
        spans.push(Span::styled(
            "  [PAUSED]",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn chip(label: &str) -> Span<'_> {
    Span::styled(
        format!(" {} ", label),
        Style::default().bg(Color::DarkGray).fg(Color::White),
    )
}

fn draw_filter_bar(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let is_editing = app.input_mode == InputMode::Filtering;

    let content = if is_editing {
        // Active prompt while the user is typing the filter.
        format!(" / {}", app.filter)
    } else {
        // Subtle indicator when a filter is active but we're back in normal mode.
        format!(" filter: \"{}\"", app.filter)
    };

    let style = if is_editing {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let p = Paragraph::new(content).style(style);
    f.render_widget(p, area);

    if is_editing {
        // Real terminal cursor right after the prompt + typed text.
        // " / " is 3 characters.
        let prompt_len: u16 = 3;
        let cursor_x = area.x + prompt_len + app.filter.chars().count() as u16;
        let cursor_y = area.y;
        f.set_cursor_position(Position { x: cursor_x, y: cursor_y });
    }
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

/// Full-screen key reference, toggled with `?`. Drawn over everything else.
fn draw_help(f: &mut ratatui::Frame) {
    let area = centered_rect(56, 80, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            " help ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let head = |t: &str| {
        Line::from(Span::styled(
            t.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("  {:<10}", k), Style::default().fg(Color::Yellow)),
            Span::raw(d.to_string()),
        ])
    };

    let lines = vec![
        head("Navigation"),
        row("↑/k ↓/j", "move selection"),
        row("Enter", "open detail · collapse/expand group"),
        row("q / Esc", "quit"),
        Line::from(""),
        head("Sort  (press again to reverse)"),
        row("t", "total tokens"),
        row("c", "cost ($)"),
        row("m", "rate (tok/60s)"),
        row("a", "last activity"),
        row("p", "project"),
        row("s", "source"),
        Line::from(""),
        head("View"),
        row("g", "group by project (tree)"),
        row("A", "show all vs. running only"),
        row("Space", "pause / resume the display"),
        row("?", "toggle this help"),
        Line::from(""),
        head("Filter"),
        row("/", "start typing a live filter (project / model / id / path)"),
        row("Esc / Enter", "exit filter mode (filter stays active)"),
        row("Esc (normal)", "clear active filter"),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// Modal detail overlay for one session: identity, token breakdown, cost,
/// context-window gauge, and a sparkline of token activity over the retained
/// window. Drawn on top of the table (which it dims via `Clear`).
fn draw_detail(f: &mut ratatui::Frame, s: &SessionView, now: DateTime<Utc>) {
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
            Span::raw(format_local_with_ago(s.started_at, now, &dash)),
        ]),
        Line::from(vec![
            label("last"),
            Span::raw(format_local_with_ago(s.last_activity, now, &dash)),
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

fn is_active(s: &SessionView, now: DateTime<Utc>) -> bool {
    s.last_activity
        .map(|t| (now - t).num_seconds() <= ACTIVE_WINDOW_SECS)
        .unwrap_or(false)
}

fn format_ago(s: &SessionView, now: DateTime<Utc>) -> String {
    let Some(t) = s.last_activity else {
        return "-".into();
    };
    format_ago_secs((now - t).num_seconds())
}

/// Render an absolute timestamp in the user's local timezone alongside the
/// relative "X ago" delta — the detail overlay needs both: the delta tells you
/// when something happened relative to now, the wall clock tells you when it
/// happened in the day. Falls back to `dash` when the timestamp is unknown.
fn format_local_with_ago(t: Option<DateTime<Utc>>, now: DateTime<Utc>, dash: &str) -> String {
    let Some(t) = t else {
        return dash.to_string();
    };
    let local = t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S");
    let ago = format_ago_secs((now - t).num_seconds());
    format!("{local}  ({ago} ago)")
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
    use crate::model::{AgentKind, Session, SessionView, TokenStats, SPARK_BUCKETS};
    use chrono::TimeZone;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::path::PathBuf;

    fn view_for(project: &str, out: u64) -> SessionView {
        let mut s = Session::new(AgentKind::Claude, PathBuf::from("/tmp/x.jsonl"));
        s.cwd = Some(project.into());
        s.tokens.output = out;
        s.view()
    }

    /// Fixed wall clock used by every snapshot test. Anything time-dependent
    /// (AGO column, STATUS, header `active` count, detail "last") is computed
    /// against this — never `Utc::now()` — so the rendered buffers stay
    /// byte-identical run to run.
    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap()
    }

    /// Build a fully-populated `SessionView` with deterministic fields so the
    /// rendered cells (id, project, model, tokens, cost, ago, status, ctx) are
    /// predictable. `ago_secs` shifts both `last_activity` and `started_at`
    /// from `fixed_now`, so the AGO/STATUS columns can be steered per test.
    fn make_view(
        kind: AgentKind,
        id: &str,
        project: &str,
        ago_secs: i64,
        total_tokens: u64,
        cost: f64,
    ) -> SessionView {
        let now = fixed_now();
        // Split the requested total across input/output/cache so every counter
        // column has something meaningful in it (otherwise CACHE renders 0).
        let input = total_tokens / 4;
        let output = total_tokens / 2;
        let cache_read = total_tokens.saturating_sub(input + output);
        let tokens = TokenStats {
            input,
            output,
            cache_read,
            cache_creation: 0,
        };
        SessionView {
            kind,
            id: id.to_string(),
            file: PathBuf::from(format!("/tmp/{id}.jsonl")),
            cwd: Some(project.to_string()),
            model: Some("claude-opus-4-7".to_string()),
            started_at: Some(now - chrono::Duration::seconds(ago_secs + 3600)),
            last_activity: Some(now - chrono::Duration::seconds(ago_secs)),
            tokens,
            tokens_last_60s: 250,
            cost_usd: Some(cost),
            context_used: 50_000,
            context_max: Some(200_000),
            context_pct: Some(0.25),
            turn_count: 7,
            spark: vec![0; SPARK_BUCKETS],
        }
    }

    /// Render via a [`TestBackend`] and return the buffer as text lines with
    /// trailing whitespace stripped. Style (color/bold) is intentionally
    /// ignored — these are layout/copy snapshots, not pixel diffs.
    fn render_to_lines<F: FnOnce(&mut ratatui::Frame)>(
        width: u16,
        height: u16,
        draw_fn: F,
    ) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(draw_fn).unwrap();
        buffer_to_lines(terminal.backend().buffer())
    }

    fn buffer_to_lines(buf: &Buffer) -> Vec<String> {
        let area = buf.area;
        (0..area.height)
            .map(|y| {
                let mut s = String::new();
                for x in 0..area.width {
                    let sym = buf
                        .cell((x, y))
                        .map(|c| c.symbol())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(" ");
                    s.push_str(sym);
                }
                s.trim_end().to_string()
            })
            .collect()
    }

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
    fn format_local_with_ago_includes_clock_and_delta() {
        let now = Utc::now();
        // None → dash.
        assert_eq!(format_local_with_ago(None, now, "-"), "-");
        // 30s ago: rendered as "YYYY-MM-DD HH:MM:SS  (30s ago)" in local TZ.
        let t = now - chrono::Duration::seconds(30);
        let s = format_local_with_ago(Some(t), now, "-");
        assert!(s.contains("(30s ago)"), "missing relative suffix: {s}");
        // The wall-clock portion ends with seconds, so the line should
        // contain two ':' characters (HH:MM:SS).
        assert!(s.matches(':').count() >= 2, "missing wall clock: {s}");
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

    #[test]
    fn visible_columns_keep_essentials_and_drop_heavy_first() {
        // Wide terminal shows everything in canonical order.
        let wide = visible_columns(200);
        assert_eq!(wide, CANONICAL.to_vec());

        // Narrow terminal keeps the essential identity/cost columns…
        let narrow = visible_columns(70);
        for c in [
            Col::Src,
            Col::Project,
            Col::Total,
            Col::Cost,
            Col::Ago,
            Col::Status,
        ] {
            assert!(narrow.contains(&c), "essential column dropped");
        }
        // …and sheds CACHE/IN/OUT first.
        assert!(!narrow.contains(&Col::Cache));
        assert!(!narrow.contains(&Col::In));
        assert!(!narrow.contains(&Col::Out));

        // Whatever survives stays in canonical left-to-right order.
        let positions: Vec<usize> = narrow
            .iter()
            .map(|c| CANONICAL.iter().position(|x| x == c).unwrap())
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]));
    }

    /// Compare two snapshot listings line-by-line, panicking with a unified
    /// diff that's easy to copy into the expected value when something drifts.
    /// Keeps every snapshot test failure self-explanatory in the test output.
    #[track_caller]
    fn assert_lines_eq(actual: &[String], expected: &[&str]) {
        if actual.len() != expected.len()
            || actual.iter().zip(expected).any(|(a, b)| a != b)
        {
            let mut msg = String::from("snapshot mismatch\n");
            let n = actual.len().max(expected.len());
            for i in 0..n {
                let a = actual.get(i).map(String::as_str).unwrap_or("<missing>");
                let e = expected.get(i).copied().unwrap_or("<missing>");
                let tag = if a == e { " " } else { "*" };
                msg.push_str(&format!("{tag}{i:>3} expected: |{e}|\n"));
                msg.push_str(&format!("{tag}    actual:   |{a}|\n"));
            }
            panic!("{msg}");
        }
    }

    /// Snapshot: full header band at 100×3. Pinned so any change to the
    /// header copy/order is a deliberate, reviewable test update.
    #[test]
    fn snapshot_header_shows_title_counts_cost_and_breakdown() {
        let s = make_view(AgentKind::Claude, "abc12345", "/w/alpha", 10, 4_000, 1.25);
        let views = vec![&s];

        let lines = render_to_lines(100, 3, |f| {
            draw_header(f, f.area(), &views, fixed_now());
        });
        let expected = [
            "┌──────────────────────────────────────────────────────────────────────────────────────────────────┐",
            "│agtop   sessions: 1  active: 1   $1.25   tokens: 4.0k   250 tok/60s   claude:1  codex:0           │",
            "└──────────────────────────────────────────────────────────────────────────────────────────────────┘",
        ];
        assert_lines_eq(&lines, &expected);
    }

    /// Snapshot: full session table at 120×8. Covers column ordering, header
    /// row, both AgentKind variants, the active/idle decoration, and the cost
    /// formatting. CACHE/IN/OUT are intentionally not in scope at this width —
    /// `snapshot_table_narrow_drops_columns` exercises the elision path.
    #[test]
    fn snapshot_table_renders_two_sessions() {
        let s1 = make_view(AgentKind::Claude, "abc12345", "/w/alpha", 10, 4_000, 1.25);
        let s2 = make_view(AgentKind::Codex, "def67890", "/w/beta", 600, 12_000, 0.40);
        let refs = vec![&s1, &s2];
        let display = build_display(&refs, false, &HashSet::new());
        let mut state = TableState::default();
        state.select(Some(0));

        let lines = render_to_lines(120, 8, |f| {
            draw_table(
                f,
                f.area(),
                &mut state,
                SortBy::LastActivity,
                false,
                false,
                &display,
                refs.len(),
                0,
                fixed_now(),
            );
        });
        let expected = [
            "┌ sessions (2) — sort: activity ▼ ─────────────────────────────────────────────────────────────────────────────────────┐",
            "│SRC     ID         PROJECT                MODEL              TOTAL      CTX   TOK/60S   $        AGO     STATUS       │",
            "│claude  abc12345   alpha                  claude-opus-4-7    4.0k       25%   250       $1.25    10s     ● active     │",
            "│codex   def67890   beta                   claude-opus-4-7    12.0k      25%   250       $0.40    10m       idle       │",
            "│                                                                                                                      │",
            "│                                                                                                                      │",
            "│                                                                                                                      │",
            "└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘",
        ];
        assert_lines_eq(&lines, &expected);
    }

    /// Snapshot: narrow terminal (70 cols). Confirms the column-elision
    /// pipeline keeps only the essential identity/cost columns visible and
    /// drops MODEL/ID/CTX/RATE/CACHE/IN/OUT in priority order.
    #[test]
    fn snapshot_table_narrow_drops_columns() {
        let s1 = make_view(AgentKind::Claude, "abc12345", "/w/alpha", 10, 4_000, 1.25);
        let s2 = make_view(AgentKind::Codex, "def67890", "/w/beta", 600, 12_000, 0.40);
        let refs = vec![&s1, &s2];
        let display = build_display(&refs, false, &HashSet::new());
        let mut state = TableState::default();
        state.select(Some(0));

        let lines = render_to_lines(70, 8, |f| {
            draw_table(
                f,
                f.area(),
                &mut state,
                SortBy::LastActivity,
                false,
                false,
                &display,
                refs.len(),
                0,
                fixed_now(),
            );
        });
        let expected = [
            "┌ sessions (2) — sort: activity ▼ ───────────────────────────────────┐",
            "│SRC     PROJECT                TOTAL      $        AGO     STATUS   │",
            "│claude  alpha                  4.0k       $1.25    10s     ● active │",
            "│codex   beta                   12.0k      $0.40    10m       idle   │",
            "│                                                                    │",
            "│                                                                    │",
            "│                                                                    │",
            "└────────────────────────────────────────────────────────────────────┘",
        ];
        assert_lines_eq(&lines, &expected);
    }

    /// Snapshot: grouped (tree) view. Subtotal rows appear above each project
    /// with the count, summed tokens, and summed cost; sessions follow under
    /// their group. The active/idle column survives unchanged.
    #[test]
    fn snapshot_table_grouped_shows_subtotals() {
        let s1 = make_view(AgentKind::Claude, "abc12345", "/w/alpha", 10, 4_000, 1.25);
        let s2 = make_view(AgentKind::Codex, "def67890", "/w/beta", 600, 12_000, 0.40);
        let s3 = make_view(AgentKind::Claude, "extra123", "/w/alpha", 30, 2_000, 0.10);
        let refs = vec![&s1, &s3, &s2];
        let display = build_display(&refs, true, &HashSet::new());
        let mut state = TableState::default();
        state.select(Some(0));

        let lines = render_to_lines(120, 10, |f| {
            draw_table(
                f,
                f.area(),
                &mut state,
                SortBy::LastActivity,
                false,
                true,
                &display,
                refs.len(),
                0,
                fixed_now(),
            );
        });
        let expected = [
            "┌ sessions (3) — sort: activity ▼, grouped ────────────────────────────────────────────────────────────────────────────┐",
            "│SRC     ID         PROJECT                MODEL              TOTAL      CTX   TOK/60S   $        AGO     STATUS       │",
            "│▾                  alpha (2)                                 6.0k                       $1.35                         │",
            "│claude  abc12345   alpha                  claude-opus-4-7    4.0k       25%   250       $1.25    10s     ● active     │",
            "│claude  extra123   alpha                  claude-opus-4-7    2.0k       25%   250       $0.10    30s     ● active     │",
            "│▾                  beta (1)                                  12.0k                      $0.40                         │",
            "│codex   def67890   beta                   claude-opus-4-7    12.0k      25%   250       $0.40    10m       idle       │",
            "│                                                                                                                      │",
            "│                                                                                                                      │",
            "└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘",
        ];
        assert_lines_eq(&lines, &expected);
    }

    /// Snapshot: collapsed group hides its children — the alpha row should
    /// stay (with a `▸` chevron) but its two sessions vanish, while beta and
    /// its one session remain visible.
    #[test]
    fn snapshot_table_collapsed_group_hides_children() {
        let s1 = make_view(AgentKind::Claude, "abc12345", "/w/alpha", 10, 4_000, 1.25);
        let s2 = make_view(AgentKind::Codex, "def67890", "/w/beta", 600, 12_000, 0.40);
        let s3 = make_view(AgentKind::Claude, "extra123", "/w/alpha", 30, 2_000, 0.10);
        let refs = vec![&s1, &s3, &s2];
        let mut collapsed = HashSet::new();
        collapsed.insert("alpha".to_string());
        let display = build_display(&refs, true, &collapsed);
        let mut state = TableState::default();
        state.select(Some(0));

        let lines = render_to_lines(120, 8, |f| {
            draw_table(
                f,
                f.area(),
                &mut state,
                SortBy::LastActivity,
                false,
                true,
                &display,
                refs.len(),
                0,
                fixed_now(),
            );
        });
        // The first content row's chevron switches from ▾ to ▸ and alpha's
        // two child sessions are absent.
        assert!(
            lines[2].contains("▸                  alpha (2)"),
            "expected collapsed chevron, got: {}",
            lines[2]
        );
        assert!(
            lines[3].contains("▾                  beta (1)"),
            "expected next row to be beta header, got: {}",
            lines[3]
        );
        assert!(
            lines.iter().all(|l| !l.contains("abc12345")),
            "alpha child session should be hidden"
        );
    }

    /// Snapshot: detail overlay at 80×28. Pinned so model/project/path/context
    /// rows, the per-bucket token rows, turns/cost/tok-rate, and the
    /// `tokens · last 5m` sparkline-block title don't silently drift.
    #[test]
    fn snapshot_detail_overlay_renders_session_fields() {
        let s = make_view(AgentKind::Claude, "abc12345", "/w/alpha", 10, 4_000, 1.25);
        let lines = render_to_lines(80, 28, |f| draw_detail(f, &s, fixed_now()));
        let expected = [
            "",
            "",
            "",
            "",
            "           ┌ claude · abc12345 ─────────────────────────────────────┐",
            "           │model    claude-opus-4-7                                │",
            "           │project  alpha                                          │",
            "           │path     /tmp/abc12345.jsonl                            │",
            "           │                                                        │",
            "           │context  50.0k / 200.0k  (25%)                          │",
            "           │                                                        │",
            "           │input    1.0k                                           │",
            "           │output   2.0k                                           │",
            "           │cache r  1.0k                                           │",
            "           │cache w  0                                              │",
            "           │total    4.0k                                           │",
            "           │turns    7                                              │",
            "           │cost     $1.2500                                        │",
            "           │tok/60s  250                                            │",
            "           │ tokens · last 5m (peak 0) ─────────────────────────────│",
            "           │                                                        │",
            "           │                                                        │",
            "           │                                                        │",
            "           └────────────────────────────────────────────────────────┘",
            "",
            "",
            "",
            "",
        ];
        assert_lines_eq(&lines, &expected);
    }

    /// Snapshot: help overlay at 60×26. The Navigation/Sort/View sections,
    /// every documented key, and the box-drawing borders are pinned so a
    /// stray edit to the help copy or layout shows up immediately.
    #[test]
    fn snapshot_help_overlay_lists_keybindings() {
        let lines = render_to_lines(60, 26, draw_help);
        let expected = [
            "",
            "",
            "",
            "             ┌ help ──────────────────────────┐",
            "             │Navigation                      │",
            "             │  ↑/k ↓/j   move selection      │",
            "             │  Enter     open detail · collap│",
            "             │  q / Esc   quit                │",
            "             │                                │",
            "             │Sort  (press again to reverse)  │",
            "             │  t         total tokens        │",
            "             │  c         cost ($)            │",
            "             │  m         rate (tok/60s)      │",
            "             │  a         last activity       │",
            "             │  p         project             │",
            "             │  s         source              │",
            "             │                                │",
            "             │View                            │",
            "             │  g         group by project (tr│",
            "             │  A         show all vs. running│",
            "             │  Space     pause / resume the d│",
            "             │  ?         toggle this help    │",
            "             └────────────────────────────────┘",
            "",
            "",
            "",
        ];
        assert_lines_eq(&lines, &expected);
    }

    /// Snapshot: the `hidden` count appears in the table title when the
    /// filter is hiding rows. Confirms the alternate title branch and that
    /// the `A` hint stays attached so users know how to surface them.
    #[test]
    fn snapshot_table_title_shows_hidden_count() {
        let s1 = make_view(AgentKind::Claude, "abc12345", "/w/alpha", 10, 4_000, 1.25);
        let refs = vec![&s1];
        let display = build_display(&refs, false, &HashSet::new());
        let mut state = TableState::default();
        state.select(Some(0));

        let lines = render_to_lines(120, 4, |f| {
            draw_table(
                f,
                f.area(),
                &mut state,
                SortBy::LastActivity,
                false,
                false,
                &display,
                refs.len(),
                3, // 3 sessions hidden by the inactive filter
                fixed_now(),
            );
        });
        assert!(
            lines[0]
                .contains("sessions (1 of 4 — 3 hidden, A) — sort: activity ▼"),
            "title missing hidden hint: {}",
            lines[0]
        );
    }

    #[test]
    fn build_display_flat_when_ungrouped() {
        let a = view_for("/w/alpha", 1);
        let b = view_for("/w/beta", 2);
        let refs = vec![&a, &b];
        let rows = build_display(&refs, false, &HashSet::new());
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0], DisplayRow::Session(_)));
    }

    #[test]
    fn build_display_groups_with_subtotals_and_collapse() {
        let a = view_for("/w/alpha", 100);
        let b = view_for("/w/alpha", 50);
        let c = view_for("/w/beta", 10);
        let refs = vec![&a, &b, &c];

        // Two group headers + three sessions.
        let grouped = build_display(&refs, true, &HashSet::new());
        assert_eq!(grouped.len(), 5);
        match &grouped[0] {
            DisplayRow::Group(g) => {
                assert_eq!(g.project, "alpha");
                assert_eq!(g.count, 2);
                assert_eq!(g.tokens, 150);
            }
            _ => panic!("expected group header first"),
        }

        // Collapsing alpha hides its two sessions: alpha header + beta header +
        // beta's one session = 3 rows.
        let mut collapsed = HashSet::new();
        collapsed.insert("alpha".to_string());
        let partial = build_display(&refs, true, &collapsed);
        assert_eq!(partial.len(), 3);
    }
}
