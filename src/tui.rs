use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};

use crate::ipc::{self, ConnectionState, ConnectionStatus, Request};

struct App {
    statuses: Vec<ConnectionStatus>,
    selected: usize,
    message: String,
    last_refresh: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            statuses: Vec::new(),
            selected: 0,
            message: "connecting to daemon…".to_owned(),
            last_refresh: Instant::now() - Duration::from_secs(2),
        }
    }

    async fn refresh(&mut self, socket_path: &Path) {
        match ipc::send_request(socket_path, Request::Status).await {
            Ok(response) if response.ok => {
                self.statuses = response.statuses;
                if self.statuses.is_empty() {
                    self.selected = 0;
                } else {
                    self.selected = self.selected.min(self.statuses.len() - 1);
                }
                self.message = response.message;
            }
            Ok(response) => self.message = response.message,
            Err(error) => self.message = format!("daemon unavailable: {error:#}"),
        }
        self.last_refresh = Instant::now();
    }

    fn selected_name(&self) -> Option<String> {
        self.statuses
            .get(self.selected)
            .map(|status| status.name.clone())
    }

    fn move_selection(&mut self, delta: isize) {
        if self.statuses.is_empty() {
            return;
        }
        let last = self.statuses.len() - 1;
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected + delta as usize).min(last)
        };
    }
}

pub async fn run(socket_path: &Path) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, socket_path).await;
    let _ = ipc::send_request(socket_path, Request::TuiExit).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    socket_path: &Path,
) -> Result<()> {
    let mut app = App::new();
    loop {
        if app.last_refresh.elapsed() >= Duration::from_secs(1) {
            app.refresh(socket_path).await;
        }
        terminal.draw(|frame| draw(frame, &app))?;

        if event::poll(Duration::from_millis(100))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            match key {
                KeyEvent {
                    code: KeyCode::Char('q'),
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Esc, ..
                } => break,
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyEvent {
                    code: KeyCode::Down,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Char('j'),
                    ..
                } => app.move_selection(1),
                KeyEvent {
                    code: KeyCode::Up, ..
                }
                | KeyEvent {
                    code: KeyCode::Char('k'),
                    ..
                } => app.move_selection(-1),
                KeyEvent {
                    code: KeyCode::Char('r'),
                    ..
                } => app.refresh(socket_path).await,
                KeyEvent {
                    code: KeyCode::Char('s'),
                    ..
                } => {
                    if let Some(name) = app.selected_name() {
                        send_selected(&mut app, socket_path, Request::Start { name }).await;
                    } else {
                        app.message = "no configured connections".to_owned();
                    }
                }
                KeyEvent {
                    code: KeyCode::Char('x'),
                    ..
                } => {
                    if let Some(name) = app.selected_name() {
                        send_selected(&mut app, socket_path, Request::Stop { name }).await;
                    } else {
                        app.message = "no configured connections".to_owned();
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

async fn send_selected(app: &mut App, socket_path: &Path, request: Request) {
    if app.statuses.is_empty() {
        app.message = "no configured connections".to_owned();
        return;
    }
    match ipc::send_request(socket_path, request).await {
        Ok(response) => app.message = response.message,
        Err(error) => app.message = format!("request failed: {error:#}"),
    }
    app.refresh(socket_path).await;
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " ssmux ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  AWS SSM session manager"),
    ]))
    .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(title, chunks[0]);

    let header = Row::new(["NAME", "MODE", "STATE", "TARGET", "PID", "ERROR"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);
    let rows = app.statuses.iter().map(|status| {
        let state_style = match status.state {
            ConnectionState::Running => Style::default().fg(Color::Green),
            ConnectionState::Starting | ConnectionState::Stopping => {
                Style::default().fg(Color::Yellow)
            }
            ConnectionState::Failed => Style::default().fg(Color::Red),
            ConnectionState::Stopped => Style::default().fg(Color::DarkGray),
        };
        Row::new([
            Cell::from(status.name.clone()),
            Cell::from(if status.keep_on_exit {
                "background"
            } else {
                "temporary"
            }),
            Cell::from(format_state(status.state)).style(state_style),
            Cell::from(status.target.clone()),
            Cell::from(
                status
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".to_owned()),
            ),
            Cell::from(status.last_error.clone().unwrap_or_default()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Length(12),
            Constraint::Length(11),
            Constraint::Length(24),
            Constraint::Length(8),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Connections ")
            .borders(Borders::ALL),
    )
    .row_highlight_style(Style::default().bg(Color::Rgb(45, 55, 72)));
    let mut table_state = TableState::default();
    table_state.select((!app.statuses.is_empty()).then_some(app.selected));
    frame.render_stateful_widget(table, chunks[1], &mut table_state);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled("↑/↓ or j/k", Style::default().fg(Color::Cyan)),
        Span::raw(" select  "),
        Span::styled("s", Style::default().fg(Color::Green)),
        Span::raw(" start  "),
        Span::styled("x", Style::default().fg(Color::Red)),
        Span::raw(" stop  "),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(" refresh  "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" quit\n"),
        Span::styled(app.message.as_str(), Style::default().fg(Color::Gray)),
    ]))
    .block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, chunks[2]);
}

fn format_state(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Starting => "starting",
        ConnectionState::Running => "running",
        ConnectionState::Stopping => "stopping",
        ConnectionState::Stopped => "stopped",
        ConnectionState::Failed => "failed",
    }
}
