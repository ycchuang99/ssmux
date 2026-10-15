use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
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

use crate::{
    config::{Config, ConnectionConfig},
    menubar_form,
    menubar_popover::SessionPopover,
    session::{ConnectionState, SessionManager},
};

struct ConnectionMenu {
    name: String,
    action: MenuId,
    details: Submenu,
    edit: MenuItem,
    status: MenuItem,
    delete: MenuItem,
    row: Option<objc2::rc::Retained<crate::menubar_row::ConnectionRow>>,
}

pub async fn run(config_path: &Path) -> Result<()> {
    let mtm = objc2::MainThreadMarker::new().context("menu bar requires the main thread")?;
    let mut config = Config::load_or_default(config_path)?;
    let mut manager = SessionManager::new(config.clone());
    let mut connections = Vec::new();
    for connection in &config.connections {
        let item = build_connection(connection)?;
        connections.push(item);
    }
    let add = MenuItem::new(
        "New Session…",
        true,
        Some(Accelerator::new(Modifiers::META, Code::KeyN)),
    );
    let quit = MenuItem::new(
        "Quit SSMux & Stop Sessions",
        true,
        Some(Accelerator::new(Modifiers::META, Code::KeyQ)),
    );

    let idle_pixels = include_bytes!("../assets/menu-template.rgba");
    let icons = [
        Icon::from_rgba(idle_pixels.to_vec(), 36, 36)?,
        Icon::from_rgba(active_icon_pixels(idle_pixels)?, 36, 36)?,
    ];
    let mut event_loop = EventLoopBuilder::<MenuEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    // Native edit commands give the form standard Command-C/V/A/Z shortcuts.
    let app_menu = Menu::new();
    let application = Submenu::new("SSMux", true);
    application.append(&add)?;
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

    let row_proxy = event_loop.create_proxy();
    let mut tray = None;
    let mut launch_error = None;
    let mut last_counts = None;
    let quit_id = quit.id().clone();
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
        match event {
            Event::NewEvents(StartCause::Init) => {
                match TrayIconBuilder::new()
                    .with_icon(icons[0].clone())
                    .with_icon_as_template(true)
                    .with_tooltip("SSMux")
                    .build()
                {
                    Ok(item) => {
                        tray = Some(item);
                    }
                    Err(error) => {
                        launch_error = Some(error);
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(event) if event.id == quit_id => *control_flow = ControlFlow::Exit,
            Event::UserEvent(event) if event.id == panel_id => {
                if let Some(button) = tray
                    .as_ref()
                    .and_then(TrayIcon::ns_status_item)
                    .and_then(|item| item.button(mtm))
                {
                    panel.toggle(&button);
                }
            }
            Event::UserEvent(event) => {
                let is_connection_action = connections.iter().any(|item| item.action == event.id);
                let result = (|| -> Result<()> {
                    let mut candidate = config.clone();
                    if event.id == *add.id() {
                        panel.close();
                        let Some(connection) = menubar_form::edit_connection(&config, None)? else {
                            return Ok(());
                        };
                        candidate.connections.push(connection);
                    } else if let Some(index) = connections.iter().position(|connection| {
                        *connection.edit.id() == event.id || *connection.delete.id() == event.id
                    }) {
                        panel.close();
                        let connection = &config.connections[index];
                        if event.id == *connections[index].edit.id() {
                            let Some(edited) =
                                menubar_form::edit_connection(&config, Some(connection))?
                            else {
                                return Ok(());
                            };
                            candidate.connections[index] = edited;
                        } else {
                            if !menubar_form::confirm_delete(&connection.name)? {
                                return Ok(());
                            }
                            candidate.connections.remove(index);
                        }
                    } else if let Some(connection) =
                        connections.iter().find(|item| item.action == event.id)
                    {
                        let name = &connection.name;
                        let state = manager
                            .statuses()
                            .into_iter()
                            .find(|status| status.name == *name)
                            .map(|status| status.state);
                        match state {
                            Some(ConnectionState::Running) => {
                                manager.stop(name)?;
                            }
                            Some(ConnectionState::Stopped | ConnectionState::Failed) => {
                                manager.start(name)?;
                            }
                            _ => {}
                        }
                        return Ok(());
                    } else {
                        return Ok(());
                    }
                    save_config(config_path, &mut config, &mut manager, candidate)?;
                    connections = config
                        .connections
                        .iter()
                        .map(build_connection)
                        .collect::<Result<_>>()?;
                    attach_rows(&panel, &config, &mut connections, &row_proxy)?;
                    last_counts = None;
                    Ok(())
                })();
                if let Err(error) = result {
                    panel.set_summary(&format!("Action Failed: {error}"));
                    if !is_connection_action {
                        menubar_form::show_error(&error);
                    }
                }
            }
            Event::MainEventsCleared => {
                manager.poll();
                if let Some(tray) = &tray {
                    if let Err(error) = update_menu(
                        &manager,
                        &mut connections,
                        &panel,
                        tray,
                        &icons,
                        &mut last_counts,
                    ) {
                        panel.set_summary(&format!("Could not update status icon: {error}"));
                    }
                }
            }
            _ => {}
        }
    });
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

fn set_text(item: &MenuItem, text: String) {
    if item.text() != text {
        item.set_text(text);
    }
}

fn update_menu(
    manager: &SessionManager,
    connections: &mut [ConnectionMenu],
    panel: &SessionPopover,
    tray: &TrayIcon,
    icons: &[Icon; 2],
    last_counts: &mut Option<(usize, usize)>,
) -> Result<()> {
    let statuses = manager.statuses();
    let active = statuses
        .iter()
        .filter(|s| {
            matches!(
                s.state,
                ConnectionState::Running | ConnectionState::Stopping
            )
        })
        .count();
    let failed = statuses
        .iter()
        .filter(|s| s.state == ConnectionState::Failed)
        .count();
    if *last_counts != Some((active, failed)) {
        let text = format!(
            "{active} Running · {failed} Failed · {} Saved",
            statuses.len()
        );
        panel.set_summary(&text);
        tray.set_icon_with_as_template(Some(icons[usize::from(active > 0)].clone()), active == 0)?;
        *last_counts = Some((active, failed));
    }
    for connection in connections {
        let Some(status) = statuses.iter().find(|s| s.name == connection.name) else {
            continue;
        };
        if let Some(row) = &connection.row {
            row.update(status.state, status.last_error.as_deref());
        }
        let state_text = match status.state {
            ConnectionState::Running => "Running",
            ConnectionState::Stopping => "Stopping…",
            ConnectionState::Stopped => "Ready to Connect",
            ConnectionState::Failed => status.last_error.as_deref().unwrap_or("Connection Failed"),
        };
        set_text(&connection.status, state_text.to_owned());
        let can_edit = matches!(
            status.state,
            ConnectionState::Stopped | ConnectionState::Failed
        );
        connection.edit.set_enabled(can_edit);
        connection.delete.set_enabled(can_edit);
    }
    Ok(())
}

fn build_connection(connection: &ConnectionConfig) -> Result<ConnectionMenu> {
    let action = MenuId::new(format!("connection:{}", connection.name));
    let details = Submenu::new(connection_route(connection), true);
    let status = MenuItem::new("Ready to Connect", false, None);
    details.append(&MenuItem::new("Manage", false, None))?;
    details.append(&status)?;
    let destination = connection
        .parameters
        .get("host")
        .map(String::as_str)
        .unwrap_or(&connection.target);
    for text in [
        format!("Destination: {destination}"),
        format!("SSM Target: {}", connection.target),
        format!(
            "{} · {}",
            connection.region.as_deref().unwrap_or("Default Region"),
            connection.profile.as_deref().unwrap_or("Default Profile")
        ),
    ] {
        details.append(&MenuItem::new(text, false, None))?;
    }
    details.append(&PredefinedMenuItem::separator())?;
    let edit = MenuItem::new("Edit Session…", true, None);
    let delete = MenuItem::new("Delete Session…", true, None);
    details.append(&edit)?;
    details.append(&delete)?;
    Ok(ConnectionMenu {
        name: connection.name.clone(),
        action,
        details,
        status,
        edit,
        delete,
        row: None,
    })
}

pub(crate) fn connection_route(connection: &ConnectionConfig) -> String {
    match (
        connection.parameters.get("localPortNumber"),
        connection.parameters.get("portNumber"),
    ) {
        (Some(local), Some(remote)) => format!("localhost:{local} → remote:{remote}"),
        _ => "Connection Details".to_owned(),
    }
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
            proxy.clone(),
        )?);
    }
    panel.set_rows(
        &connections
            .iter()
            .filter_map(|item| item.row.as_ref().map(|row| row.view()))
            .collect::<Vec<_>>(),
    );
    Ok(())
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
    fn route_shows_local_and_remote_ports_without_exposing_a_long_hostname() {
        let connection = ConnectionConfig {
            name: "test".into(),
            target: "i-test".into(),
            region: None,
            profile: None,
            document_name: None,
            extra_args: vec![],
            parameters: std::collections::BTreeMap::from([
                ("host".into(), "db.example.test".into()),
                ("localPortNumber".into(), "13308".into()),
                ("portNumber".into(), "3306".into()),
            ]),
        };
        assert_eq!(
            connection_route(&connection),
            "localhost:13308 → remote:3306"
        );
        let mut custom = connection;
        custom.parameters.clear();
        assert_eq!(connection_route(&custom), "Connection Details");
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
