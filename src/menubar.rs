use std::{
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::{
    aws_settings::{self, AwsSettings},
    config::{Config, ConnectionConfig},
    menubar_form,
    menubar_popover::SessionPopover,
    session::{ConnectionState, ConnectionStatus, FailureKind, SessionManager},
};
use anyhow::{Context, Result};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertSecondButtonReturn, NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::{
        macos::{ActivationPolicy, EventLoopExtMacOS},
        run_return::EventLoopExtRunReturn,
    },
};
use tray_icon::{
    menu::{
        accelerator::{Accelerator, Code, Modifiers},
        Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
    },
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

struct ConnectionMenu {
    name: String,
    action: MenuId,
    recovery: MenuId,
    details: Submenu,
    edit: MenuItem,
    status: MenuItem,
    delete: MenuItem,
    duplicate: MenuItem,
    copy: MenuItem,
    pin: MenuItem,
    diagnostics: MenuItem,
    sign_in: MenuItem,
    row: Option<objc2::rc::Retained<crate::menubar_row::ConnectionRow>>,
}

pub async fn run(config_path: &Path) -> Result<()> {
    let mtm = MainThreadMarker::new().context("menu bar requires the main thread")?;
    let mut config = Config::load_or_default(config_path)?;
    let mut manager = SessionManager::new(config.clone());
    let mut connections = config
        .connections
        .iter()
        .map(build_connection)
        .collect::<Result<Vec<_>>>()?;
    let add = MenuItem::new(
        "New Session…",
        true,
        Some(Accelerator::new(Modifiers::META, Code::KeyN)),
    );
    let find = MenuItem::new(
        "Find Session…",
        true,
        Some(Accelerator::new(Modifiers::META, Code::KeyF)),
    );
    let setup = MenuItem::new("Check AWS Setup…", true, None);
    let quit = MenuItem::new(
        "Quit SSMux & Stop Sessions",
        true,
        Some(Accelerator::new(Modifiers::META, Code::KeyQ)),
    );
    let search_id = MenuId::new("ssmux-search");
    let idle_pixels = include_bytes!("../assets/menu-template.rgba");
    let icons = [
        Icon::from_rgba(idle_pixels.to_vec(), 36, 36)?,
        Icon::from_rgba(active_icon_pixels(idle_pixels)?, 36, 36)?,
    ];
    let mut event_loop = EventLoopBuilder::<MenuEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let app_menu = Menu::new();
    let application = Submenu::new("SSMux", true);
    application.append(&add)?;
    application.append(&find)?;
    application.append(&setup)?;
    application.append(&PredefinedMenuItem::close_window(None))?;
    application.append(&PredefinedMenuItem::separator())?;
    application.append(&quit)?;
    let edit = Submenu::new("Edit", true);
    for item in [
        PredefinedMenuItem::undo(None),
        PredefinedMenuItem::redo(None),
        PredefinedMenuItem::separator(),
        PredefinedMenuItem::cut(None),
        PredefinedMenuItem::copy(None),
        PredefinedMenuItem::paste(None),
        PredefinedMenuItem::select_all(None),
    ] {
        edit.append(&item)?;
    }
    app_menu.append(&application)?;
    app_menu.append(&edit)?;
    app_menu.init_for_nsapp();
    let panel = SessionPopover::new(
        event_loop.create_proxy(),
        add.id().clone(),
        setup.id().clone(),
        search_id.clone(),
        quit.id().clone(),
    )?;
    attach_rows(
        &panel,
        &config,
        &mut connections,
        &event_loop.create_proxy(),
    )?;
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(event);
    }));
    let panel_id = MenuId::new("ssmux-panel");
    let toggle_id = panel_id.clone();
    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            let _ = proxy.send_event(MenuEvent {
                id: toggle_id.clone(),
            });
        }
    }));
    let (setup_tx, setup_rx) = mpsc::channel();
    let mut setup_task = Some(discover_settings(setup_tx.clone()));
    let mut show_setup = false;
    let mut aws = AwsSettings::default();
    let (login_tx, login_rx) = mpsc::channel();
    let mut login_task: Option<tokio::task::JoinHandle<()>> = None;
    let row_proxy = event_loop.create_proxy();
    let mut tray = None;
    let mut launch_error = None;
    let mut last_counts = None;
    let quit_id = quit.id().clone();
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));
        match event {
            Event::NewEvents(StartCause::Init) => {
                match TrayIconBuilder::new().with_icon(icons[0].clone()).with_icon_as_template(true).with_tooltip("SSMux").build() {
                    Ok(item) => tray = Some(item),
                    Err(error) => { launch_error = Some(error); *control_flow = ControlFlow::Exit; }
                }
            }
            Event::UserEvent(event) if event.id == quit_id => *control_flow = ControlFlow::Exit,
            Event::UserEvent(event) if event.id == panel_id || event.id == *find.id() => {
                if let Some(button) = tray.as_ref().and_then(TrayIcon::ns_status_item).and_then(|item| item.button(mtm)) {
                    if event.id == *find.id() { panel.show(&button); panel.focus_search(); }
                    else { panel.toggle(&button); }
                }
            }
            Event::UserEvent(event) if event.id == search_id => display_rows(&panel, &config, &connections),
            Event::UserEvent(event) => {
                manager.poll();
                let result = (|| -> Result<()> {
                    if event.id == *setup.id() {
                        show_setup = true;
                        if setup_task.is_none() { setup_task = Some(discover_settings(setup_tx.clone())); }
                        panel.set_notice("Checking AWS CLI, Session Manager Plugin and local profiles…");
                        return Ok(());
                    }
                    let mut candidate = config.clone();
                    if event.id == *add.id() {
                        panel.close();
                        let Some(connection) = menubar_form::edit_connection(&config, None, &aws)? else { return Ok(()); };
                        candidate.connections.push(connection);
                    } else if let Some(index) = connections.iter().position(|item| item.owns(&event.id)) {
                        let item = &connections[index];
                        let connection = &config.connections[index];
                        let status = manager.statuses().into_iter().find(|status| status.name == item.name).context("session not found")?;
                        if event.id == *item.copy.id() {
                            copy_text(&local_address(connection).context("this session has no local port")?)?;
                            panel.set_notice("Local address copied. Paste it into your database or other client.");
                            return Ok(());
                        } else if event.id == *item.diagnostics.id() {
                            panel.close(); show_diagnostics(&status)?; return Ok(());
                        } else if event.id == *item.pin.id() {
                            candidate.connections[index].pinned = !connection.pinned;
                        } else if event.id == *item.duplicate.id() {
                            panel.close();
                            let Some(duplicate) = menubar_form::duplicate_connection(&config, connection, &aws)? else { return Ok(()); };
                            candidate.connections.push(duplicate);
                        } else if event.id == *item.sign_in.id() || (event.id == item.recovery && status.failure_kind == Some(FailureKind::SsoExpired)) {
                            if login_task.is_some() { panel.set_notice("AWS sign-in is already in progress. Complete it in your browser."); return Ok(()); }
                            let profile = effective_profile(connection);
                            if status.failure_kind != Some(FailureKind::SsoExpired) && !aws.profiles.iter().any(|item| item.name == profile && item.sso) {
                                panel.close();
                                show_information("AWS Sign-In", "This profile is not confirmed as an SSO profile. Use Check Setup to refresh profiles. For other credential providers, renew credentials using your provider, then reconnect.")?;
                                return Ok(());
                            }
                            let tx = login_tx.clone();
                            panel.set_notice("Signing in to AWS… Complete sign-in in your browser, then reconnect.");
                            login_task = Some(tokio::spawn(async move {
                                let result = sign_in(&profile).await;
                                let _ = tx.send(result);
                            }));
                            return Ok(());
                        } else if event.id == *item.edit.id() || (event.id == item.recovery && status.failure_kind == Some(FailureKind::PortInUse)) {
                            if !can_edit(status.state) { anyhow::bail!("Stop this session before editing it."); }
                            panel.close();
                            let Some(edited) = menubar_form::edit_connection(&config, Some(connection), &aws)? else { return Ok(()); };
                            candidate.connections[index] = edited;
                        } else if event.id == *item.delete.id() {
                            if !can_edit(status.state) { anyhow::bail!("Stop this session before deleting it."); }
                            panel.close();
                            if !menubar_form::confirm_delete(&connection.name)? { return Ok(()); }
                            candidate.connections.remove(index);
                        } else if event.id == item.recovery && status.failure_kind == Some(FailureKind::MissingDependency) {
                            show_setup = true;
                            if setup_task.is_none() { setup_task = Some(discover_settings(setup_tx.clone())); }
                            panel.set_notice("Checking local AWS setup…");
                            return Ok(());
                        } else if event.id == item.recovery && !matches!(status.failure_kind, Some(FailureKind::Network | FailureKind::Timeout)) {
                            panel.close(); show_diagnostics(&status)?; return Ok(());
                        } else {
                            match status.state {
                                ConnectionState::Running | ConnectionState::Connecting | ConnectionState::Reconnecting => manager.stop(&item.name)?,
                                ConnectionState::Stopped | ConnectionState::Failed | ConnectionState::Disconnected => manager.start(&item.name)?,
                                ConnectionState::Stopping => {}
                            }
                            return Ok(());
                        }
                    } else { return Ok(()); }
                    save_config(config_path, &mut config, &mut manager, candidate)?;
                    connections = config.connections.iter().map(build_connection).collect::<Result<_>>()?;
                    attach_rows(&panel, &config, &mut connections, &row_proxy)?;
                    last_counts = None;
                    Ok(())
                })();
                if let Err(error) = result {
                    panel.set_notice(&format!("Action failed: {error}"));
                    if !panel.is_shown() { menubar_form::show_error(&error); }
                }
            }
            Event::MainEventsCleared => {
                if let Ok(settings) = setup_rx.try_recv() {
                    aws = settings; setup_task = None;
                    let summary = aws.summary();
                    panel.set_notice(if aws.issues.is_empty() { "Local setup checked. AWS access and tunnel readiness are checked when connecting." } else { "Local setup needs attention. Use Check Setup for details." });
                    if show_setup {
                        show_setup = false; panel.close();
                        if let Err(error) = show_information("Local AWS Setup", &summary) { panel.set_notice(&error.to_string()); }
                    }
                }
                if let Ok(result) = login_rx.try_recv() {
                    login_task = None;
                    panel.set_notice(&result);
                }
                manager.poll();
                if let Some(tray) = &tray {
                    if let Err(error) = update_menu(&manager, &config, &aws, login_task.is_some(), &mut connections, &panel, tray, &icons, &mut last_counts) {
                        panel.set_notice(&format!("Could not update status: {error}"));
                    }
                }
            }
            _ => {}
        }
    });
    if let Some(task) = setup_task {
        task.abort();
    }
    if let Some(task) = login_task {
        task.abort();
    }
    panel.close();
    TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    drop(tray);
    manager.shutdown().await;
    if let Some(error) = launch_error {
        return Err(error).context("creating the SSMux menu bar item");
    }
    Ok(())
}

fn discover_settings(sender: mpsc::Sender<AwsSettings>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _ = sender.send(aws_settings::discover().await);
    })
}

async fn sign_in(profile: &str) -> String {
    let result = tokio::time::timeout(
        Duration::from_secs(180),
        aws_settings::command("aws")
            .args(["sso", "login", "--profile", profile])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await;
    match result {
        Ok(Ok(status)) if status.success() => {
            "AWS sign-in completed. Reconnect the session when ready.".into()
        }
        Err(_) => "AWS sign-in timed out. Try Sign In again and finish in your browser.".into(),
        _ => "AWS sign-in did not complete. Check your SSO profile and browser, then try again."
            .into(),
    }
}

fn effective_profile(connection: &ConnectionConfig) -> String {
    connection
        .profile
        .clone()
        .or_else(|| std::env::var("AWS_PROFILE").ok())
        .or_else(|| std::env::var("AWS_DEFAULT_PROFILE").ok())
        .unwrap_or_else(|| "default".into())
}

fn can_edit(state: ConnectionState) -> bool {
    matches!(
        state,
        ConnectionState::Stopped | ConnectionState::Failed | ConnectionState::Disconnected
    )
}

impl ConnectionMenu {
    fn owns(&self, id: &MenuId) -> bool {
        *id == self.action
            || *id == self.recovery
            || [
                self.edit.id(),
                self.delete.id(),
                self.duplicate.id(),
                self.copy.id(),
                self.pin.id(),
                self.diagnostics.id(),
                self.sign_in.id(),
            ]
            .contains(&id)
    }
}

fn set_text(item: &MenuItem, text: String) {
    if item.text() != text {
        item.set_text(text);
    }
}

#[allow(clippy::too_many_arguments)]
fn update_menu(
    manager: &SessionManager,
    config: &Config,
    aws: &AwsSettings,
    signing_in: bool,
    connections: &mut [ConnectionMenu],
    panel: &SessionPopover,
    tray: &TrayIcon,
    icons: &[Icon; 2],
    last_counts: &mut Option<(usize, usize, usize)>,
) -> Result<()> {
    let statuses = manager.statuses();
    let ready = statuses
        .iter()
        .filter(|s| s.state == ConnectionState::Running)
        .count();
    let pending = statuses
        .iter()
        .filter(|s| {
            matches!(
                s.state,
                ConnectionState::Connecting
                    | ConnectionState::Reconnecting
                    | ConnectionState::Stopping
            )
        })
        .count();
    let problems = statuses
        .iter()
        .filter(|s| {
            matches!(
                s.state,
                ConnectionState::Failed | ConnectionState::Disconnected
            )
        })
        .count();
    if *last_counts != Some((ready, pending, problems)) {
        let summary = format!(
            "{ready} Ready · {pending} In Progress · {problems} Need Attention · {} Saved",
            statuses.len()
        );
        panel.set_summary(&summary);
        tray.set_tooltip(Some(format!("SSMux — {summary}")))?;
        tray.set_icon_with_as_template(Some(icons[usize::from(ready > 0)].clone()), ready == 0)?;
        *last_counts = Some((ready, pending, problems));
    }
    for (connection, saved) in connections.iter_mut().zip(&config.connections) {
        let Some(status) = statuses.iter().find(|s| s.name == connection.name) else {
            continue;
        };
        if let Some(row) = &connection.row {
            row.update(
                status.state,
                status.last_error.as_deref(),
                status.failure_kind,
                status.retry_attempt,
            );
        }
        let text = match status.state {
            ConnectionState::Running => "Tunnel Ready",
            ConnectionState::Connecting => "Connecting…",
            ConnectionState::Reconnecting => "Waiting to Reconnect…",
            ConnectionState::Stopping => "Stopping…",
            ConnectionState::Stopped => "Not Connected",
            ConnectionState::Disconnected => "Connection Interrupted",
            ConnectionState::Failed => "Connection Failed",
        };
        set_text(&connection.status, text.into());
        connection.edit.set_enabled(can_edit(status.state));
        connection.delete.set_enabled(can_edit(status.state));
        connection.sign_in.set_enabled(
            !signing_in
                && can_edit(status.state)
                && (status.failure_kind == Some(FailureKind::SsoExpired)
                    || aws
                        .profiles
                        .iter()
                        .any(|p| p.name == effective_profile(saved) && p.sso)),
        );
    }
    Ok(())
}

fn build_connection(connection: &ConnectionConfig) -> Result<ConnectionMenu> {
    let action = MenuId::new(format!("connection:{}", connection.name));
    let recovery = MenuId::new(format!("recover:{}", connection.name));
    let details = Submenu::new(connection_route(connection), true);
    let status = MenuItem::new("Not Connected", false, None);
    details.append(&MenuItem::new("Manage", false, None))?;
    details.append(&status)?;
    for text in [
        format!(
            "Destination: {}",
            connection
                .parameters
                .get("host")
                .unwrap_or(&connection.target)
        ),
        format!("SSM Target: {}", connection.target),
        connection_identity(connection),
    ] {
        details.append(&MenuItem::new(text, false, None))?;
    }
    details.append(&PredefinedMenuItem::separator())?;
    let copy = MenuItem::new(
        "Copy Local Address",
        local_address(connection).is_some(),
        None,
    );
    let duplicate = MenuItem::new("Duplicate Session…", true, None);
    let pin = MenuItem::new(
        if connection.pinned {
            "Unpin Session"
        } else {
            "Pin Session"
        },
        true,
        None,
    );
    let diagnostics = MenuItem::new("View Diagnostics…", true, None);
    let sign_in = MenuItem::new("Sign In to AWS SSO…", false, None);
    let edit = MenuItem::new("Edit Session…", true, None);
    let delete = MenuItem::new("Delete Session…", true, None);
    for item in [
        &copy,
        &duplicate,
        &pin,
        &diagnostics,
        &sign_in,
        &edit,
        &delete,
    ] {
        details.append(item)?;
    }
    Ok(ConnectionMenu {
        name: connection.name.clone(),
        action,
        recovery,
        details,
        status,
        edit,
        delete,
        duplicate,
        copy,
        pin,
        diagnostics,
        sign_in,
        row: None,
    })
}

pub(crate) fn connection_route(connection: &ConnectionConfig) -> String {
    let destination = connection
        .parameters
        .get("host")
        .unwrap_or(&connection.target);
    match (
        connection.parameters.get("localPortNumber"),
        connection.parameters.get("portNumber"),
    ) {
        (Some(local), Some(remote)) => format!("localhost:{local} → {destination}:{remote}"),
        _ => format!("Target: {destination}"),
    }
}

pub(crate) fn connection_identity(connection: &ConnectionConfig) -> String {
    format!(
        "Profile: {} · Region: {}",
        connection.profile.as_deref().unwrap_or("AWS default"),
        connection.region.as_deref().unwrap_or("AWS default")
    )
}

fn local_address(connection: &ConnectionConfig) -> Option<String> {
    connection
        .parameters
        .get("localPortNumber")
        .map(|port| format!("localhost:{port}"))
}

fn copy_text(text: &str) -> Result<()> {
    let clipboard = NSPasteboard::generalPasteboard();
    clipboard.clearContents();
    if !clipboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString }) {
        anyhow::bail!("Could not copy to the clipboard.");
    }
    Ok(())
}

fn show_information(title: &str, text: &str) -> Result<()> {
    let mtm = MainThreadMarker::new().context("details require the main thread")?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(text));
    alert.addButtonWithTitle(&NSString::from_str("Close"));
    alert.runModal();
    Ok(())
}

fn show_diagnostics(status: &ConnectionStatus) -> Result<()> {
    let mtm = MainThreadMarker::new().context("diagnostics require the main thread")?;
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!(
        "Diagnostics — {}",
        status.name
    )));
    let summary = status
        .failure_kind
        .map(|kind| format!("{}\n{}", kind.summary(), kind.suggestion()))
        .unwrap_or_else(|| format!("State: {:?}", status.state));
    let details = if status.diagnostics.is_empty() {
        status
            .last_error
            .as_deref()
            .unwrap_or("No diagnostic output for this session.")
    } else {
        &status.diagnostics
    };
    let text = format!("{summary}\n\n{details}");
    // Keep the alert readable; Copy Diagnostics includes the full bounded output.
    let preview: String = text.chars().take(2000).collect();
    alert.setInformativeText(&NSString::from_str(&preview));
    alert.addButtonWithTitle(&NSString::from_str("Close"));
    alert.addButtonWithTitle(&NSString::from_str("Copy Diagnostics"));
    if alert.runModal() == NSAlertSecondButtonReturn {
        copy_text(&text)?;
    }
    Ok(())
}

fn active_icon_pixels(pixels: &[u8]) -> Result<Vec<u8>> {
    use objc2_app_kit::{NSColor, NSColorSpace};
    let color = NSColor::systemGreenColor()
        .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
        .context("resolving status color")?;
    let color = [
        color.redComponent(),
        color.greenComponent(),
        color.blueComponent(),
    ]
    .map(|channel| (channel.clamp(0., 1.) * 255.).round() as u8);
    Ok(pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [color[0], color[1], color[2], pixel[3]])
        .collect())
}

fn attach_rows(
    panel: &SessionPopover,
    config: &Config,
    connections: &mut [ConnectionMenu],
    proxy: &tao::event_loop::EventLoopProxy<MenuEvent>,
) -> Result<()> {
    for (connection, item) in config.connections.iter().zip(connections.iter_mut()) {
        item.row = Some(crate::menubar_row::ConnectionRow::new(
            connection,
            &item.details,
            item.action.clone(),
            item.recovery.clone(),
            proxy.clone(),
        )?);
    }
    display_rows(panel, config, connections);
    Ok(())
}

fn visible_connections(config: &Config, query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let mut visible = config
        .connections
        .iter()
        .enumerate()
        .filter(|(_, connection)| {
            query.is_empty()
                || [
                    connection.name.as_str(),
                    connection.target.as_str(),
                    connection.parameters.get("host").map_or("", String::as_str),
                    connection.profile.as_deref().unwrap_or(""),
                    connection.region.as_deref().unwrap_or(""),
                ]
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    visible.sort_by_key(|index| !config.connections[*index].pinned);
    visible
}

fn display_rows(panel: &SessionPopover, config: &Config, connections: &[ConnectionMenu]) {
    panel.set_rows(
        &visible_connections(config, &panel.query())
            .into_iter()
            .filter_map(|index| connections[index].row.as_ref().map(|row| row.view()))
            .collect::<Vec<_>>(),
    );
}

fn save_config(
    path: &Path,
    config: &mut Config,
    manager: &mut SessionManager,
    candidate: Config,
) -> Result<()> {
    if Config::load_or_default(path)?.connections != config.connections {
        anyhow::bail!("configuration changed outside SSMux; restart it before saving changes");
    }
    manager.validate_reload(&candidate)?;
    candidate.save_atomic(path)?;
    manager.reload_config(candidate.clone())?;
    *config = candidate;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn route_and_identity_distinguish_destinations_and_copy_the_local_address() {
        let connection = ConnectionConfig {
            name: "test".into(),
            target: "i-test".into(),
            region: None,
            profile: None,
            document_name: None,
            extra_args: vec![],
            auto_reconnect: false,
            pinned: false,
            parameters: std::collections::BTreeMap::from([
                ("host".into(), "db.example.test".into()),
                ("localPortNumber".into(), "13308".into()),
                ("portNumber".into(), "3306".into()),
            ]),
        };
        assert_eq!(
            connection_route(&connection),
            "localhost:13308 → db.example.test:3306"
        );
        assert_eq!(
            local_address(&connection).as_deref(),
            Some("localhost:13308")
        );
        assert_eq!(
            connection_identity(&connection),
            "Profile: AWS default · Region: AWS default"
        );
        let mut custom = connection;
        custom.parameters.clear();
        assert_eq!(connection_route(&custom), "Target: i-test");
        assert!(local_address(&custom).is_none());
    }

    #[test]
    fn search_matches_environment_and_pins_without_changing_saved_order() {
        let config: Config = toml::from_str(
            r#"
[[connections]]
name = "Stage Database"
target = "i-stage"
profile = "staging"
[connections.parameters]
host = "db.stage.example.test"
[[connections]]
name = "Production Database"
target = "i-prod"
profile = "production"
region = "ap-northeast-1"
pinned = true
[[connections]]
name = "Stage Cache"
target = "i-cache"
profile = "staging"
pinned = true
"#,
        )
        .unwrap();
        assert_eq!(visible_connections(&config, ""), vec![1, 2, 0]);
        assert_eq!(visible_connections(&config, " STAGING "), vec![2, 0]);
        assert_eq!(visible_connections(&config, "db.stage"), vec![0]);
        assert_eq!(visible_connections(&config, "NORTHEAST"), vec![1]);
        assert!(visible_connections(&config, "missing").is_empty());
        assert_eq!(config.connections[0].name, "Stage Database");
    }

    #[test]
    fn crud_persists_updates_manager_and_rejects_external_edits_and_invalid_changes() {
        let directory = std::env::temp_dir().join(format!(
            "ssmux-crud-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("config.toml");
        let mut config = Config::default();
        let mut manager = SessionManager::new(config.clone());
        let connection = ConnectionConfig {
            name: "new".into(),
            target: "i-123".into(),
            region: None,
            profile: None,
            document_name: Some("AWS-StartPortForwardingSession".into()),
            parameters: Default::default(),
            extra_args: vec![],
            auto_reconnect: false,
            pinned: false,
        };
        let mut candidate = config.clone();
        candidate.connections.push(connection);
        save_config(&path, &mut config, &mut manager, candidate).unwrap();
        assert_eq!(Config::load(&path).unwrap().connections, config.connections);
        assert_eq!(manager.statuses()[0].name, "new");
        let mut candidate = config.clone();
        candidate.connections[0].name = "renamed".into();
        candidate.connections[0].target = "i-456".into();
        save_config(&path, &mut config, &mut manager, candidate).unwrap();
        assert_eq!(manager.statuses()[0].name, "renamed");
        assert_eq!(Config::load(&path).unwrap().connections[0].target, "i-456");
        let mut invalid = config.clone();
        invalid.connections.push(invalid.connections[0].clone());
        assert!(save_config(&path, &mut config, &mut manager, invalid).is_err());
        assert_eq!(Config::load(&path).unwrap().connections, config.connections);
        let blocked_temporary = path.with_extension("toml.tmp");
        std::fs::create_dir(&blocked_temporary).unwrap();
        assert!(save_config(&path, &mut config, &mut manager, Config::default()).is_err());
        assert_eq!(Config::load(&path).unwrap().connections, config.connections);
        assert_eq!(manager.statuses().len(), 1);
        std::fs::remove_dir(blocked_temporary).unwrap();
        let mut external = config.clone();
        external.connections[0].region = Some("us-east-1".into());
        external.save_atomic(&path).unwrap();
        assert!(save_config(&path, &mut config, &mut manager, Config::default()).is_err());
        assert_eq!(
            Config::load(&path).unwrap().connections,
            external.connections
        );
        assert_eq!(manager.statuses().len(), 1);
        config.save_atomic(&path).unwrap();
        save_config(&path, &mut config, &mut manager, Config::default()).unwrap();
        assert!(Config::load(&path).unwrap().connections.is_empty());
        assert!(manager.statuses().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
