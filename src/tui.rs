use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};

use crate::{
    config::{Config, ConnectionConfig},
    session::{ConnectionState, SessionManager},
    ssm,
};

struct App {
    config_path: PathBuf,
    config: Config,
    manager: SessionManager,
    selected: usize,
    message: String,
    form: Option<ConnectionForm>,
    filter: TextInput,
    filtering: bool,
    delete_confirmation: Option<String>,
    delete_confirmation_selected: bool,
}

impl App {
    fn new(config_path: &Path, config: Config) -> Self {
        Self {
            config_path: config_path.to_owned(),
            config: config.clone(),
            manager: SessionManager::new(config),
            selected: 0,
            message: "ready".to_owned(),
            form: None,
            filter: TextInput::new("type a name or target"),
            filtering: false,
            delete_confirmation: None,
            delete_confirmation_selected: false,
        }
    }

    fn selected_name(&self) -> Option<String> {
        self.visible_statuses()
            .get(self.selected)
            .map(|status| status.name.clone())
    }

    fn visible_statuses(&self) -> Vec<crate::session::ConnectionStatus> {
        let query = self.filter.value.trim().to_lowercase();
        self.manager
            .statuses()
            .into_iter()
            .filter(|status| {
                if query.is_empty() {
                    return true;
                }
                let error = status.last_error.as_deref().unwrap_or_default();
                let document = status.document_name.as_deref().unwrap_or_default();
                [
                    status.name.as_str(),
                    status.target.as_str(),
                    document,
                    format_state(status.state),
                    error,
                ]
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
            })
            .collect()
    }

    fn clamp_selection(&mut self) {
        self.selected = self
            .selected
            .min(self.visible_statuses().len().saturating_sub(1));
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.visible_statuses().len();
        if count == 0 {
            return;
        }
        let last = count - 1;
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            (self.selected + delta as usize).min(last)
        };
    }
}

struct ConnectionForm {
    edit_name: Option<String>,
    active: usize,
    action: Option<FormAction>,
    fields: Vec<FormField>,
    document_options: Vec<String>,
    document_index: usize,
    document_open: bool,
    parameters: Vec<ParameterRow>,
}

struct FormField {
    label: &'static str,
    input: TextInput,
}

struct ParameterRow {
    key: TextInput,
    value: TextInput,
}

struct TextInput {
    placeholder: &'static str,
    value: String,
    cursor: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FormAction {
    Cancel,
    Save,
}

const DOCUMENT_FIELD_INDEX: usize = 4;

impl TextInput {
    fn new(placeholder: &'static str) -> Self {
        Self {
            placeholder,
            value: String::new(),
            cursor: 0,
        }
    }

    fn with_value(placeholder: &'static str, value: String) -> Self {
        let cursor = value.len();
        Self {
            placeholder,
            value,
            cursor,
        }
    }

    fn insert(&mut self, character: char) {
        self.value.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some(start) = previous_char_boundary(&self.value, self.cursor) {
            self.value.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.value.len() {
            let end = next_char_boundary(&self.value, self.cursor);
            self.value.replace_range(self.cursor..end, "");
        }
    }

    fn move_left(&mut self) {
        if let Some(start) = previous_char_boundary(&self.value, self.cursor) {
            self.cursor = start;
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor = next_char_boundary(&self.value, self.cursor);
        }
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.value.len();
    }
}

impl ParameterRow {
    fn new() -> Self {
        Self {
            key: TextInput::new("e.g. host"),
            value: TextInput::new("e.g. db.example.test"),
        }
    }

    fn from_values(key: String, value: String) -> Self {
        Self {
            key: TextInput::with_value("e.g. host", key),
            value: TextInput::with_value("e.g. db.example.test", value),
        }
    }

    fn is_empty(&self) -> bool {
        self.key.value.is_empty() && self.value.value.is_empty()
    }
}

impl ConnectionForm {
    fn new() -> Self {
        Self {
            edit_name: None,
            active: 0,
            action: None,
            fields: vec![
                FormField {
                    label: "Name",
                    input: TextInput::new("e.g. demo-db"),
                },
                FormField {
                    label: "Target",
                    input: TextInput::new("e.g. i-0123456789abcdef0"),
                },
                FormField {
                    label: "Region",
                    input: TextInput::new("e.g. us-west-2"),
                },
                FormField {
                    label: "Profile",
                    input: TextInput::new("e.g. example-profile (optional)"),
                },
                FormField {
                    label: "Document",
                    input: TextInput::new("e.g. AWS-StartPortForwardingSessionToRemoteHost"),
                },
            ],
            document_options: vec![
                String::new(),
                "AWS-StartInteractiveCommand".to_owned(),
                "AWS-StartPortForwardingSession".to_owned(),
                "AWS-StartPortForwardingSessionToRemoteHost".to_owned(),
            ],
            document_index: 0,
            document_open: false,
            parameters: vec![ParameterRow::new()],
        }
    }

    fn from_connection(connection: &ConnectionConfig) -> Self {
        let mut form = Self::new();
        form.edit_name = Some(connection.name.clone());
        form.fields[0].input =
            TextInput::with_value(form.fields[0].input.placeholder, connection.name.clone());
        form.fields[1].input =
            TextInput::with_value(form.fields[1].input.placeholder, connection.target.clone());
        form.fields[2].input = TextInput::with_value(
            form.fields[2].input.placeholder,
            connection.region.clone().unwrap_or_default(),
        );
        form.fields[3].input = TextInput::with_value(
            form.fields[3].input.placeholder,
            connection.profile.clone().unwrap_or_default(),
        );
        if let Some(document_name) = &connection.document_name {
            form.document_index = form
                .document_options
                .iter()
                .position(|option| option == document_name)
                .unwrap_or_else(|| {
                    form.document_options.push(document_name.clone());
                    form.document_options.len() - 1
                });
        }
        form.parameters = connection
            .parameters
            .iter()
            .map(|(key, value)| ParameterRow::from_values(key.clone(), value.clone()))
            .collect();
        if form.parameters.is_empty() {
            form.parameters.push(ParameterRow::new());
        }
        form
    }

    fn to_connection(&self) -> Result<ConnectionConfig> {
        let value = |index: usize| self.fields[index].input.value.trim().to_owned();
        let name = value(0);
        let target = value(1);
        if name.is_empty() || target.is_empty() {
            anyhow::bail!("Name and Target are required");
        }
        let mut parameters = std::collections::BTreeMap::new();
        for (index, row) in self.parameters.iter().enumerate() {
            let key = row.key.value.trim();
            let parameter = row.value.value.trim();
            if key.is_empty() && parameter.is_empty() {
                continue;
            }
            if key.is_empty() || parameter.is_empty() {
                anyhow::bail!("Parameter {} needs both a key and a value", index + 1);
            }
            if parameters
                .insert(key.to_owned(), parameter.to_owned())
                .is_some()
            {
                anyhow::bail!("Duplicate parameter key: {key}");
            }
        }
        Ok(ConnectionConfig {
            name,
            target,
            region: optional_value(value(2)),
            profile: optional_value(value(3)),
            document_name: (!self.document_options[self.document_index].is_empty())
                .then(|| self.document_options[self.document_index].clone()),
            parameters,
            extra_args: Vec::new(),
        })
    }

    fn field_count(&self) -> usize {
        self.fields.len() + self.parameters.len() * 2
    }

    fn active_input_mut(&mut self) -> &mut TextInput {
        if self.active < self.fields.len() {
            return &mut self.fields[self.active].input;
        }
        let parameter_index = (self.active - self.fields.len()) / 2;
        if (self.active - self.fields.len()).is_multiple_of(2) {
            &mut self.parameters[parameter_index].key
        } else {
            &mut self.parameters[parameter_index].value
        }
    }

    fn select_document(&mut self, delta: isize) {
        let count = self.document_options.len();
        if delta.is_negative() {
            self.document_index = if self.document_index == 0 {
                count - 1
            } else {
                self.document_index - 1
            };
        } else {
            self.document_index = (self.document_index + delta as usize) % count;
        }
    }

    fn document_display(&self) -> &str {
        let document = &self.document_options[self.document_index];
        if document.is_empty() {
            "(none)"
        } else {
            document
        }
    }

    fn next_field(&mut self) {
        self.document_open = false;
        if self.active + 1 < self.field_count() {
            self.active += 1;
            return;
        }
        if self
            .parameters
            .last()
            .is_some_and(|parameter| !parameter.is_empty())
        {
            self.parameters.push(ParameterRow::new());
            self.active += 1;
        } else {
            self.action = Some(FormAction::Cancel);
        }
    }

    fn previous_field(&mut self) {
        self.document_open = false;
        self.active = if self.active == 0 {
            self.field_count() - 1
        } else {
            self.active - 1
        };
    }

    fn remove_active_parameter(&mut self) {
        if self.active < self.fields.len() {
            return;
        }
        let parameter_index = (self.active - self.fields.len()) / 2;
        if self.parameters.len() == 1 {
            self.parameters[0] = ParameterRow::new();
            self.active = self.fields.len();
            return;
        }
        self.parameters.remove(parameter_index);
        self.active = self.active.min(self.field_count() - 1);
    }
}

fn previous_char_boundary(value: &str, cursor: usize) -> Option<usize> {
    value[..cursor]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
}

fn next_char_boundary(value: &str, cursor: usize) -> usize {
    value[cursor..]
        .chars()
        .next()
        .map_or(value.len(), |character| cursor + character.len_utf8())
}

fn optional_value(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

pub async fn run(config_path: &Path) -> Result<()> {
    let config = Config::load_or_default(config_path)?;
    let mut app = App::new(config_path, config);

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, &mut app).await;
    app.manager.shutdown().await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        app.manager.poll();
        terminal.draw(|frame| draw(frame, app))?;

        if event::poll(Duration::from_millis(100))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if app.form.is_some() {
                handle_form_key(app, key)?;
                continue;
            }
            if app.delete_confirmation.is_some() {
                handle_delete_confirmation_key(app, key);
                continue;
            }
            if app.filtering {
                handle_filter_key(app, key);
                continue;
            }
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
                    code: KeyCode::Char('s'),
                    ..
                } => start_selected(terminal, app).await,
                KeyEvent {
                    code: KeyCode::Char('x'),
                    ..
                } => stop_selected(app),
                KeyEvent {
                    code: KeyCode::Char('n'),
                    ..
                } => app.form = Some(ConnectionForm::new()),
                KeyEvent {
                    code: KeyCode::Char('e'),
                    ..
                } => begin_edit(app),
                KeyEvent {
                    code: KeyCode::Char('d'),
                    ..
                } => delete_selected(app),
                KeyEvent {
                    code: KeyCode::Char('/'),
                    ..
                } => {
                    app.filtering = true;
                    app.filter.move_end();
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn begin_edit(app: &mut App) {
    let Some(name) = app.selected_name() else {
        app.message = "no configured connections".to_owned();
        return;
    };
    let Some(connection) = app.config.connections.iter().find(|item| item.name == name) else {
        app.message = format!("configuration not found: {name}");
        return;
    };
    if app.manager.statuses().iter().any(|status| {
        status.name == name
            && matches!(
                status.state,
                ConnectionState::Running | ConnectionState::Stopping
            )
    }) {
        app.message = "stop the connection before editing it".to_owned();
        return;
    }
    app.form = Some(ConnectionForm::from_connection(connection));
}

fn delete_selected(app: &mut App) {
    let Some(name) = app.selected_name() else {
        app.message = "no configured connections".to_owned();
        return;
    };
    if app.manager.statuses().iter().any(|status| {
        status.name == name
            && matches!(
                status.state,
                ConnectionState::Running | ConnectionState::Stopping
            )
    }) {
        app.message = "stop the connection before deleting it".to_owned();
        return;
    }
    app.delete_confirmation = Some(name);
    app.delete_confirmation_selected = false;
}

fn handle_delete_confirmation_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => confirm_delete(app),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => cancel_delete(app),
        KeyCode::Enter => {
            if app.delete_confirmation_selected {
                confirm_delete(app);
            } else {
                cancel_delete(app);
            }
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Tab => {
            app.delete_confirmation_selected = !app.delete_confirmation_selected
        }
        _ => {}
    }
}

fn cancel_delete(app: &mut App) {
    app.delete_confirmation = None;
    app.delete_confirmation_selected = false;
    app.message = "delete cancelled".to_owned();
}

fn confirm_delete(app: &mut App) {
    let Some(name) = app.delete_confirmation.take() else {
        return;
    };
    app.delete_confirmation_selected = false;
    let mut config = app.config.clone();
    config.connections.retain(|item| item.name != name);
    if let Err(error) = save_config(app, config) {
        app.message = error.to_string();
    } else {
        app.message = format!("deleted {name}");
    }
}

fn handle_filter_key(app: &mut App, key: KeyEvent) {
    match key {
        KeyEvent {
            code: KeyCode::Esc | KeyCode::Enter,
            ..
        } => {
            app.filtering = false;
        }
        KeyEvent {
            code: KeyCode::Backspace,
            ..
        } => {
            app.filter.backspace();
            app.clamp_selection();
        }
        KeyEvent {
            code: KeyCode::Delete,
            ..
        } => {
            app.filter.delete();
            app.clamp_selection();
        }
        KeyEvent {
            code: KeyCode::Left,
            ..
        } => app.filter.move_left(),
        KeyEvent {
            code: KeyCode::Right,
            ..
        } => app.filter.move_right(),
        KeyEvent {
            code: KeyCode::Home,
            ..
        } => app.filter.move_home(),
        KeyEvent {
            code: KeyCode::End, ..
        } => app.filter.move_end(),
        KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            ..
        } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            app.filter.insert(character);
            app.selected = 0;
        }
        _ => {}
    }
}

fn handle_form_key(app: &mut App, key: KeyEvent) -> Result<()> {
    if app.form.as_ref().is_some_and(|form| form.action.is_some()) {
        return handle_form_action_key(app, key);
    }

    match key {
        KeyEvent {
            code: KeyCode::Esc, ..
        } => {
            let close_document = app.form.as_ref().is_some_and(|form| form.document_open);
            if close_document {
                if let Some(form) = app.form.as_mut() {
                    form.document_open = false;
                }
            } else {
                app.form = None;
                app.message = "cancelled".to_owned();
            }
        }
        KeyEvent {
            code: KeyCode::Tab, ..
        } => {
            if let Some(form) = app.form.as_mut() {
                form.next_field();
            }
        }
        KeyEvent {
            code: KeyCode::BackTab,
            ..
        } => {
            if let Some(form) = app.form.as_mut() {
                form.previous_field();
            }
        }
        KeyEvent {
            code: KeyCode::Down,
            ..
        } => {
            if let Some(form) = app.form.as_mut() {
                if form.active == DOCUMENT_FIELD_INDEX && form.document_open {
                    form.select_document(1);
                } else {
                    form.next_field();
                }
            }
        }
        KeyEvent {
            code: KeyCode::Up, ..
        } => {
            if let Some(form) = app.form.as_mut() {
                if form.active == DOCUMENT_FIELD_INDEX && form.document_open {
                    form.select_document(-1);
                } else {
                    form.previous_field();
                }
            }
        }
        KeyEvent {
            code: KeyCode::Enter,
            ..
        } => {
            let document_active = app
                .form
                .as_ref()
                .is_some_and(|form| form.active == DOCUMENT_FIELD_INDEX);
            if document_active {
                let document_open = app.form.as_ref().is_some_and(|form| form.document_open);
                if document_open {
                    if let Some(form) = app.form.as_mut() {
                        form.document_open = false;
                        form.next_field();
                    }
                } else if let Some(form) = app.form.as_mut() {
                    form.document_open = true;
                }
                return Ok(());
            }
            let save = app
                .form
                .as_ref()
                .is_some_and(|form| form.active + 1 == form.field_count());
            if save {
                if let Err(error) = save_form(app) {
                    app.message = error.to_string();
                }
            } else if let Some(form) = app.form.as_mut() {
                form.next_field();
            }
        }
        KeyEvent {
            code: KeyCode::Backspace,
            ..
        } => {
            if let Some(form) = app.form.as_mut() {
                form.active_input_mut().backspace();
            }
        }
        KeyEvent {
            code: KeyCode::Delete,
            ..
        } => {
            if let Some(form) = app.form.as_mut() {
                form.active_input_mut().delete();
            }
        }
        KeyEvent {
            code: KeyCode::Left,
            ..
        } => {
            if let Some(form) = app.form.as_mut() {
                if form.active == DOCUMENT_FIELD_INDEX && form.document_open {
                    form.select_document(-1);
                } else if form.active != DOCUMENT_FIELD_INDEX {
                    form.active_input_mut().move_left();
                }
            }
        }
        KeyEvent {
            code: KeyCode::Right,
            ..
        } => {
            if let Some(form) = app.form.as_mut() {
                if form.active == DOCUMENT_FIELD_INDEX && form.document_open {
                    form.select_document(1);
                } else if form.active != DOCUMENT_FIELD_INDEX {
                    form.active_input_mut().move_right();
                }
            }
        }
        KeyEvent {
            code: KeyCode::Home,
            ..
        } => {
            if let Some(form) = app.form.as_mut() {
                form.active_input_mut().move_home();
            }
        }
        KeyEvent {
            code: KeyCode::End, ..
        } => {
            if let Some(form) = app.form.as_mut() {
                form.active_input_mut().move_end();
            }
        }
        KeyEvent {
            code: KeyCode::Char('d'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(form) = app.form.as_mut() {
                form.remove_active_parameter();
            }
        }
        KeyEvent {
            code: KeyCode::Char(' '),
            modifiers,
            ..
        } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            if let Some(form) = app.form.as_mut() {
                if form.active == DOCUMENT_FIELD_INDEX {
                    form.document_open = !form.document_open;
                } else {
                    form.active_input_mut().insert(' ');
                }
            }
        }
        KeyEvent {
            code: KeyCode::Char(character),
            modifiers,
            ..
        } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            if let Some(form) = app.form.as_mut() {
                if form.active != DOCUMENT_FIELD_INDEX {
                    form.active_input_mut().insert(character);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_form_action_key(app: &mut App, key: KeyEvent) -> Result<()> {
    let action = app
        .form
        .as_ref()
        .and_then(|form| form.action)
        .expect("form action exists while handling form action");

    match key.code {
        KeyCode::Esc => {
            app.form = None;
            app.message = "cancelled".to_owned();
        }
        KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
            if let Some(form) = app.form.as_mut() {
                form.action = Some(match action {
                    FormAction::Cancel => FormAction::Save,
                    FormAction::Save => FormAction::Cancel,
                });
            }
        }
        KeyCode::BackTab | KeyCode::Up | KeyCode::Left => {
            if let Some(form) = app.form.as_mut() {
                form.action = match action {
                    FormAction::Cancel => None,
                    FormAction::Save => Some(FormAction::Cancel),
                };
            }
        }
        KeyCode::Enter => match action {
            FormAction::Cancel => {
                app.form = None;
                app.message = "cancelled".to_owned();
            }
            FormAction::Save => {
                if let Err(error) = save_form(app) {
                    app.message = error.to_string();
                }
            }
        },
        _ => {}
    }

    Ok(())
}

fn save_form(app: &mut App) -> Result<()> {
    let (edit_name, connection) = {
        let form = app.form.as_ref().expect("form exists while saving");
        (form.edit_name.clone(), form.to_connection()?)
    };
    let mut config = app.config.clone();
    if let Some(old_name) = edit_name {
        if connection.name != old_name
            && config
                .connections
                .iter()
                .any(|item| item.name == connection.name)
        {
            anyhow::bail!("connection already exists: {}", connection.name);
        }
        let item = config
            .connections
            .iter_mut()
            .find(|item| item.name == old_name)
            .with_context(|| format!("configuration not found: {old_name}"))?;
        *item = connection.clone();
    } else {
        if config
            .connections
            .iter()
            .any(|item| item.name == connection.name)
        {
            anyhow::bail!("connection already exists: {}", connection.name);
        }
        config.connections.push(connection.clone());
    }
    config.validate()?;
    save_config(app, config)?;
    app.form = None;
    app.message = format!("saved {}", connection.name);
    Ok(())
}

fn save_config(app: &mut App, config: Config) -> Result<()> {
    app.manager.reload_config(config.clone())?;
    config.save_atomic(&app.config_path)?;
    app.config = config;
    app.clamp_selection();
    Ok(())
}

fn selected_connection(app: &App) -> Option<ConnectionConfig> {
    let name = app.selected_name()?;
    app.config
        .connections
        .iter()
        .find(|connection| connection.name == name)
        .cloned()
}

async fn start_selected(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) {
    let Some(name) = app.selected_name() else {
        app.message = "no configured connections".to_owned();
        return;
    };
    let Some(connection) = selected_connection(app) else {
        app.message = format!("configuration not found: {name}");
        return;
    };
    if ssm::is_interactive(&connection) {
        app.message = match run_interactive_session(terminal, &connection).await {
            Ok(status) if status.success() => format!("session ended: {name}"),
            Ok(status) => format!("session failed with {status}: {name}"),
            Err(error) => error.to_string(),
        };
    } else {
        app.message = match app.manager.start(&name) {
            Ok(()) => format!("started {name}"),
            Err(error) => error.to_string(),
        };
    }
}

async fn run_interactive_session(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    connection: &ConnectionConfig,
) -> Result<std::process::ExitStatus> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    let session_result = async {
        let mut child = ssm::spawn_interactive_session(connection)?;
        child.wait().await.context("waiting for AWS session")
    }
    .await;

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    session_result
}

fn stop_selected(app: &mut App) {
    let Some(name) = app.selected_name() else {
        app.message = "no configured connections".to_owned();
        return;
    };
    app.message = match app.manager.stop(&name) {
        Ok(()) => format!("stopping {name}"),
        Err(error) => error.to_string(),
    };
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    let show_filter = app.filtering || !app.filter.value.is_empty();
    let mut constraints = vec![Constraint::Length(5)];
    if show_filter {
        constraints.push(Constraint::Length(3));
    }
    constraints.push(Constraint::Min(4));
    let show_details = !app.visible_statuses().is_empty();
    if show_details {
        constraints.push(Constraint::Length(9));
    }
    constraints.push(Constraint::Length(2));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let header_block = Block::default().borders(Borders::BOTTOM);
    let header_inner = header_block.inner(chunks[0]);
    frame.render_widget(header_block, chunks[0]);
    let header_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(header_inner);
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " ssmux ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  AWS SSM session manager"),
    ]));
    frame.render_widget(title, header_rows[0]);
    let shortcut_rows = [
        [
            shortcut_line("↑/↓ j/k", "select", Color::Cyan),
            shortcut_line("n", "new", Color::Green),
            shortcut_line("e", "edit", Color::Yellow),
        ],
        [
            shortcut_line("d", "delete", Color::Red),
            shortcut_line("s", "start", Color::Green),
            shortcut_line("x", "stop", Color::Red),
        ],
        [
            shortcut_line("/", "filter", Color::Cyan),
            shortcut_line("q", "quit", Color::Magenta),
            Line::from(""),
        ],
    ];
    for (row_index, row) in shortcut_rows.into_iter().enumerate() {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(33),
                Constraint::Percentage(33),
                Constraint::Percentage(34),
            ])
            .split(header_rows[row_index + 1]);
        for (column_index, line) in row.into_iter().enumerate() {
            frame.render_widget(Paragraph::new(line), columns[column_index]);
        }
    }

    let content_index = if show_filter {
        let filter_style = if app.filtering {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Cyan)
        };
        let filter = Paragraph::new(input_line("/", &app.filter, app.filtering))
            .block(Block::default().title(" Filter ").borders(Borders::ALL))
            .style(filter_style);
        frame.render_widget(filter, chunks[1]);
        2
    } else {
        1
    };

    let statuses = app.visible_statuses();
    let widths = table_widths(chunks[content_index].width, &statuses);
    let header = Row::new(["NAME", "STATE", "TARGET", "DOCUMENT", "PID", "ERROR"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);
    let rows = statuses.iter().map(|status| {
        let state_style = match status.state {
            ConnectionState::Running => Style::default().fg(Color::Green),
            ConnectionState::Stopping => Style::default().fg(Color::Yellow),
            ConnectionState::Failed => Style::default().fg(Color::Red),
            ConnectionState::Stopped => Style::default().fg(Color::DarkGray),
        };
        Row::new([
            Cell::from(status.name.clone()),
            Cell::from(format_state(status.state)).style(state_style),
            Cell::from(status.target.clone()),
            Cell::from(status.document_name.as_deref().unwrap_or("-")),
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
            Constraint::Length(widths[0]),
            Constraint::Length(widths[1]),
            Constraint::Length(widths[2]),
            Constraint::Length(widths[3]),
            Constraint::Length(widths[4]),
            Constraint::Length(widths[5]),
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
    if !statuses.is_empty() {
        table_state.select(Some(app.selected.min(statuses.len() - 1)));
    }
    frame.render_stateful_widget(table, chunks[content_index], &mut table_state);

    let footer_index = if show_details {
        draw_details(frame, chunks[content_index + 1], app);
        content_index + 2
    } else {
        content_index + 1
    };
    let footer = Paragraph::new(Line::from(Span::styled(
        app.message.as_str(),
        Style::default().fg(Color::Gray),
    )))
    .block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, chunks[footer_index]);

    if let Some(form) = &app.form {
        draw_form(frame, form);
    }
    if let Some(name) = &app.delete_confirmation {
        draw_delete_confirmation(frame, name, app.delete_confirmation_selected);
    }
}

fn shortcut_line(key: &'static str, label: &'static str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(key, Style::default().fg(color)),
        Span::raw(format!(" {label}")),
    ])
}

fn draw_details(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let statuses = app.visible_statuses();
    let Some(status) = statuses.get(app.selected.min(statuses.len().saturating_sub(1))) else {
        return;
    };
    let Some(connection) = app
        .config
        .connections
        .iter()
        .find(|connection| connection.name == status.name)
    else {
        return;
    };
    let parameters = if connection.parameters.is_empty() {
        "-".to_owned()
    } else {
        connection
            .parameters
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let label_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let detail = Paragraph::new(vec![
        detail_line("NAME", connection.name.as_str(), label_style),
        detail_line("STATE", format_state(status.state), label_style),
        detail_line("TARGET", connection.target.as_str(), label_style),
        detail_line(
            "REGION",
            connection.region.as_deref().unwrap_or("-"),
            label_style,
        ),
        detail_line(
            "PROFILE",
            connection.profile.as_deref().unwrap_or("-"),
            label_style,
        ),
        detail_line(
            "DOCUMENT",
            connection.document_name.as_deref().unwrap_or("-"),
            label_style,
        ),
        detail_line("PARAMETERS", parameters.as_str(), label_style),
    ])
    .block(Block::default().title(" Details ").borders(Borders::ALL));
    frame.render_widget(detail, area);
}

fn detail_line(label: &str, value: &str, label_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), label_style),
        Span::raw(value.to_owned()),
    ])
}

fn table_widths(available_width: u16, statuses: &[crate::session::ConnectionStatus]) -> [u16; 6] {
    let name_width = statuses
        .iter()
        .map(|status| display_width(&status.name))
        .max()
        .unwrap_or_default();
    let document_width = statuses
        .iter()
        .map(|status| display_width(status.document_name.as_deref().unwrap_or("-")))
        .max()
        .unwrap_or_default();
    let mut widths = [
        name_width.saturating_add(2).clamp(20, 36),
        10,
        22,
        document_width.saturating_add(2).clamp(24, 50),
        7,
        14,
    ];
    let minimums = [18, 10, 18, 20, 7, 8];
    let available_width = available_width.saturating_sub(2);
    while widths.iter().map(|width| u32::from(*width)).sum::<u32>() > u32::from(available_width) {
        let mut changed = false;
        for index in [5, 3, 2, 0] {
            if widths[index] > minimums[index] {
                widths[index] -= 1;
                changed = true;
                if widths.iter().map(|width| u32::from(*width)).sum::<u32>()
                    <= u32::from(available_width)
                {
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }
    widths
}

fn display_width(value: &str) -> u16 {
    value.chars().count().min(u16::MAX as usize) as u16
}

fn draw_form(frame: &mut Frame<'_>, form: &ConnectionForm) {
    let parameter_header_index = form.fields.len();
    let parameter_start_index = parameter_header_index + 1;
    let footer_index = parameter_start_index + form.parameters.len();
    let content_height = (form.fields.len() as u16)
        .saturating_mul(2)
        .saturating_add(1)
        .saturating_add((form.parameters.len() as u16).saturating_mul(2))
        .saturating_add(2);
    let dropdown_height = if form.active == DOCUMENT_FIELD_INDEX && form.document_open {
        form.document_options.len() as u16 + 2
    } else {
        0
    };
    let desired_height = content_height
        .saturating_add(1)
        .saturating_add(dropdown_height);
    let area = centered_fixed_rect(90, desired_height, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(if form.edit_name.is_some() {
            " Edit connection "
        } else {
            " New connection "
        })
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut constraints = vec![Constraint::Length(2); form.fields.len()];
    constraints.push(Constraint::Length(1));
    constraints.extend(std::iter::repeat_n(
        Constraint::Length(2),
        form.parameters.len(),
    ));
    constraints.push(Constraint::Length(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    for (index, field) in form.fields.iter().enumerate() {
        if index == DOCUMENT_FIELD_INDEX {
            render_document(frame, rows[index], form);
        } else {
            render_input(
                frame,
                rows[index],
                field.label,
                &field.input,
                form.active == index,
            );
        }
    }
    let parameter_header = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(rows[parameter_header_index]);
    let header_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new("KEY").style(header_style),
        parameter_header[0],
    );
    frame.render_widget(
        Paragraph::new("VALUE").style(header_style),
        parameter_header[1],
    );
    for (index, parameter) in form.parameters.iter().enumerate() {
        let row_index = parameter_start_index + index;
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(rows[row_index]);
        render_input(
            frame,
            columns[0],
            &format!("Param {} Key", index + 1),
            &parameter.key,
            form.active == form.fields.len() + index * 2,
        );
        render_input(
            frame,
            columns[1],
            "Value",
            &parameter.value,
            form.active == form.fields.len() + index * 2 + 1,
        );
    }
    let cancel_style = if form.action == Some(FormAction::Cancel) {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    };
    let save_style = if form.action == Some(FormAction::Save) {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };
    let footer_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(22)])
        .split(rows[footer_index]);
    frame.render_widget(
        Paragraph::new("Ctrl+D removes row").style(Style::default().fg(Color::Gray)),
        footer_columns[0],
    );
    let actions = Paragraph::new(Line::from(vec![
        Span::styled("[ Cancel ]", cancel_style),
        Span::raw(" "),
        Span::styled("[ Save ]", save_style),
    ]))
    .alignment(Alignment::Right);
    frame.render_widget(actions, footer_columns[1]);
    if form.active == DOCUMENT_FIELD_INDEX && form.document_open {
        draw_document_dropdown(frame, form, rows[DOCUMENT_FIELD_INDEX]);
    }
}

fn render_document(frame: &mut Frame<'_>, area: Rect, form: &ConnectionForm) {
    let style = if form.active == DOCUMENT_FIELD_INDEX {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };
    let indicator = if form.document_open { "▲" } else { "▼" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Document: ", style),
            Span::styled(
                format!("[ {} ] {indicator}", form.document_display()),
                style.add_modifier(Modifier::BOLD),
            ),
        ]))
        .block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn draw_document_dropdown(frame: &mut Frame<'_>, form: &ConnectionForm, anchor: Rect) {
    let screen = frame.area();
    let height = form.document_options.len() as u16 + 2;
    let width = anchor.width.min(78);
    let max_y = screen.y + screen.height.saturating_sub(height);
    let area = Rect {
        x: anchor.x,
        y: (anchor.y + anchor.height).min(max_y),
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let lines = form
        .document_options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let label = if option.is_empty() { "(none)" } else { option };
            let marker = if index == form.document_index {
                "▶ "
            } else {
                "  "
            };
            let style = if index == form.document_index {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(Span::styled(format!("{marker}{label}"), style))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Document ").borders(Borders::ALL))
            .style(Style::default().bg(Color::Black)),
        area,
    );
}

fn render_input(frame: &mut Frame<'_>, area: Rect, label: &str, input: &TextInput, active: bool) {
    frame.render_widget(
        Paragraph::new(input_line(label, input, active))
            .block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn input_line(label: &str, input: &TextInput, active: bool) -> Line<'static> {
    let style = if active {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut spans = Vec::new();
    if !label.is_empty() {
        spans.push(Span::styled(format!("{label}: "), style));
    }
    if input.value.is_empty() {
        if active {
            spans.push(Span::styled("│", style));
        }
        spans.push(Span::styled(
            input.placeholder,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
    } else if active {
        spans.push(Span::raw(input.value[..input.cursor].to_owned()));
        spans.push(Span::styled("│", style));
        spans.push(Span::raw(input.value[input.cursor..].to_owned()));
    } else {
        spans.push(Span::raw(input.value.clone()));
    }
    Line::from(spans)
}

fn draw_delete_confirmation(frame: &mut Frame<'_>, name: &str, confirm_selected: bool) {
    let message = format!("Delete connection '{name}'?");
    let width = display_width(&message).saturating_add(8).clamp(32, 60);
    let area = centered_width_rect(width, 6, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Confirm Delete ")
        .borders(Borders::ALL)
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Red));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let cancel_style = if confirm_selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    let confirm_style = if confirm_selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(message),
            Line::from(""),
            Line::from(vec![
                Span::styled(" Cancel ", cancel_style),
                Span::raw("   "),
                Span::styled(" Confirm ", confirm_style),
            ]),
        ])
        .alignment(Alignment::Center),
        inner,
    );
}

fn centered_fixed_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area
        .width
        .saturating_mul(percent_x)
        .saturating_div(100)
        .min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn centered_width_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn format_state(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Running => "running",
        ConnectionState::Stopping => "stopping",
        ConnectionState::Stopped => "stopped",
        ConnectionState::Failed => "failed",
    }
}
