use std::{
    cell::RefCell,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use objc2::{rc::Retained, MainThreadOnly};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSColor, NSFont,
    NSPanel, NSPasteboard, NSPasteboardTypeString, NSScrollView, NSTextField, NSTextView,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
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
    copy_address: MenuItem,
    error_details: MenuItem,
    copy_error: MenuItem,
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
    let mut diagnostic_panel: Option<Retained<NSPanel>> = None;
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
                    last_counts = None;
                }
            }
            Event::UserEvent(event) => {
                let is_connection_action = connections.iter().any(|item| item.action == event.id);
                let result = (|| -> Result<()> {
                    if event.id == *add.id() {
                        panel.close();
                        let snapshot = config.clone();
                        let manager_ref = RefCell::new(&mut manager);
                        if !menubar_form::edit_connection(
                            &snapshot,
                            None,
                            |connection| {
                                let mut candidate = config.clone();
                                candidate.connections.push(connection);
                                save_config(
                                    config_path,
                                    &mut config,
                                    &mut manager_ref.borrow_mut(),
                                    candidate,
                                )
                            },
                            || {
                                let mut manager = manager_ref.borrow_mut();
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
                                        panel.set_summary(&format!(
                                            "Could not update status icon: {error}"
                                        ));
                                    }
                                }
                            },
                        )? {
                            return Ok(());
                        }
                    } else if let Some(index) = connections.iter().position(|connection| {
                        *connection.edit.id() == event.id || *connection.delete.id() == event.id
                    }) {
                        panel.close();
                        if event.id == *connections[index].edit.id() {
                            let snapshot = config.clone();
                            let manager_ref = RefCell::new(&mut manager);
                            if !menubar_form::edit_connection(
                                &snapshot,
                                Some(&snapshot.connections[index]),
                                |edited| {
                                    let mut candidate = config.clone();
                                    candidate.connections[index] = edited;
                                    save_config(
                                        config_path,
                                        &mut config,
                                        &mut manager_ref.borrow_mut(),
                                        candidate,
                                    )
                                },
                                || {
                                    let mut manager = manager_ref.borrow_mut();
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
                                            panel.set_summary(&format!(
                                                "Could not update status icon: {error}"
                                            ));
                                        }
                                    }
                                },
                            )? {
                                return Ok(());
                            }
                        } else {
                            let connection = &config.connections[index];
                            if !menubar_form::confirm_delete(&connection.name)? {
                                return Ok(());
                            }
                            let mut candidate = config.clone();
                            candidate.connections.remove(index);
                            save_config(config_path, &mut config, &mut manager, candidate)?;
                        }
                    } else if let Some(index) = connections.iter().position(|connection| {
                        *connection.copy_address.id() == event.id
                            || *connection.error_details.id() == event.id
                            || *connection.copy_error.id() == event.id
                    }) {
                        if event.id == *connections[index].copy_address.id() {
                            if let Some(address) = local_address(&config.connections[index]) {
                                copy_text(&address)?;
                                panel.set_summary(&format!("Copied {address}"));
                            }
                        } else if let Some(error) = manager
                            .statuses()
                            .iter()
                            .find(|status| status.name == connections[index].name)
                            .and_then(|status| status.last_error.as_deref())
                        {
                            if event.id == *connections[index].copy_error.id() {
                                copy_text(error)?;
                                panel.set_summary("Copied Error Details");
                            } else {
                                panel.close();
                                if let Some(previous) = diagnostic_panel.take() {
                                    previous.close();
                                }
                                diagnostic_panel =
                                    Some(show_error_details(&connections[index].name, error)?);
                            }
                        }
                        return Ok(());
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
                            Some(
                                ConnectionState::Starting
                                | ConnectionState::Forwarding
                                | ConnectionState::Running,
                            ) => {
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
    if let Some(diagnostics) = diagnostic_panel {
        diagnostics.close();
    }
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
    let active = statuses.iter().filter(|s| s.state.is_active()).count();
    let failed = statuses
        .iter()
        .filter(|s| s.state == ConnectionState::Failed)
        .count();
    if *last_counts != Some((active, failed)) {
        let text = format!(
            "{active} Active · {failed} Failed · {} Saved",
            statuses.len()
        );
        panel.set_summary(&text);
        tray.set_tooltip(Some(format!("SSMux — {text}")))?;
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
            ConnectionState::Starting => "Starting — waiting for the local port",
            ConnectionState::Forwarding => "Forwarding Ready — local port open",
            ConnectionState::Running => "Process Running — readiness unknown",
            ConnectionState::Stopping => "Stopping…",
            ConnectionState::Stopped => "Ready to Connect",
            ConnectionState::Failed => status.last_error.as_deref().unwrap_or("Connection Failed"),
        };
        set_text(
            &connection.status,
            if status.state == ConnectionState::Failed {
                crate::session::error_summary(state_text)
            } else {
                state_text.to_owned()
            },
        );
        connection
            .error_details
            .set_enabled(status.last_error.is_some());
        connection
            .copy_error
            .set_enabled(status.last_error.is_some());
        let can_edit = matches!(
            status.state,
            ConnectionState::Stopped | ConnectionState::Failed
        );
        connection.edit.set_enabled(can_edit);
        connection.delete.set_enabled(can_edit);
        set_text(
            &connection.edit,
            if can_edit {
                "Edit Session…"
            } else {
                "Edit Session… (Stop First)"
            }
            .to_owned(),
        );
        set_text(
            &connection.delete,
            if can_edit {
                "Delete Session…"
            } else {
                "Delete Session… (Stop First)"
            }
            .to_owned(),
        );
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
    let copy_address = MenuItem::new(
        "Copy Local Address",
        local_address(connection).is_some(),
        None,
    );
    let error_details = MenuItem::new("View Error Details…", false, None);
    let copy_error = MenuItem::new("Copy Error Details", false, None);
    details.append(&copy_address)?;
    details.append(&error_details)?;
    details.append(&copy_error)?;
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
        copy_address,
        error_details,
        copy_error,
        row: None,
    })
}

pub(crate) fn connection_route(connection: &ConnectionConfig) -> String {
    let Some(local) = local_address(connection) else {
        return if crate::ssm::is_forwarding(connection) {
            "Forwarding · Check Local Port in Details".to_owned()
        } else {
            "Custom Document · View Details".to_owned()
        };
    };
    let Some(remote) = connection.parameters.get("portNumber") else {
        return format!("{local} · View Details");
    };
    let destination = if connection.document_name.as_deref()
        == Some("AWS-StartPortForwardingSessionToRemoteHost")
    {
        connection
            .parameters
            .get("host")
            .map(String::as_str)
            .unwrap_or("destination")
    } else {
        &connection.target
    };
    let destination = if destination.contains(':') && !destination.starts_with('[') {
        format!("[{destination}]")
    } else {
        destination.to_owned()
    };
    format!("{local} → {destination}:{remote}")
}

fn local_address(connection: &ConnectionConfig) -> Option<String> {
    if !crate::ssm::is_forwarding(connection) {
        return None;
    }
    let port = connection
        .parameters
        .get("localPortNumber")?
        .parse::<u16>()
        .ok()?;
    (port != 0).then(|| format!("localhost:{port}"))
}

fn copy_text(text: &str) -> Result<()> {
    let clipboard = NSPasteboard::generalPasteboard();
    clipboard.clearContents();
    anyhow::ensure!(
        unsafe { clipboard.setString_forType(&NSString::from_str(text), NSPasteboardTypeString) },
        "could not copy to the clipboard"
    );
    Ok(())
}

fn show_error_details(name: &str, error: &str) -> Result<Retained<NSPanel>> {
    let mtm = objc2::MainThreadMarker::new().context("error details require the main thread")?;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(NSPoint::new(0., 0.), NSSize::new(560., 360.)),
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe {
        panel.setReleasedWhenClosed(false);
    }
    panel.setHidesOnDeactivate(false);
    panel.setContentMinSize(NSSize::new(400., 260.));
    panel.setTitle(&NSString::from_str(&format!("{name} — Connection Failed")));
    let content = panel.contentView().context("creating error details")?;
    let summary = NSTextField::wrappingLabelWithString(
        &NSString::from_str(&crate::session::error_summary(error)),
        mtm,
    );
    summary.setFrame(NSRect::new(NSPoint::new(16., 300.), NSSize::new(528., 44.)));
    summary.setFont(Some(&NSFont::systemFontOfSize(13.)));
    summary.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );
    content.addSubview(&summary);
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::new(16., 16.), NSSize::new(528., 272.)),
    );
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    let details = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(NSPoint::new(0., 0.), NSSize::new(508., 272.)),
    );
    details.setString(&NSString::from_str(error));
    details.setEditable(false);
    details.setSelectable(true);
    details.setFont(NSFont::userFixedPitchFontOfSize(12.).as_deref());
    details.setTextColor(Some(&NSColor::textColor()));
    details.setBackgroundColor(&NSColor::textBackgroundColor());
    details.setAccessibilityLabel(Some(&NSString::from_str("AWS Session Error Details")));
    details.setVerticallyResizable(true);
    details.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    scroll.setDocumentView(Some(&details));
    content.addSubview(&scroll);
    panel.setInitialFirstResponder(Some(&details));
    panel.center();
    panel.makeKeyAndOrderFront(None);
    NSApplication::sharedApplication(mtm).activate();
    Ok(panel)
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
    fn route_identifies_destination_and_only_offers_known_forwarding_addresses() {
        let connection = ConnectionConfig {
            name: "test".into(),
            target: "i-test".into(),
            region: None,
            profile: None,
            document_name: Some("AWS-StartPortForwardingSessionToRemoteHost".into()),
            extra_args: vec![],
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
        let mut instance = connection.clone();
        instance.document_name = Some("AWS-StartPortForwardingSession".into());
        assert_eq!(connection_route(&instance), "localhost:13308 → i-test:3306");
        let mut ipv6 = connection.clone();
        ipv6.parameters.insert("host".into(), "::1".into());
        assert_eq!(connection_route(&ipv6), "localhost:13308 → [::1]:3306");
        for port in ["0", "65536", "invalid"] {
            let mut invalid = connection.clone();
            invalid
                .parameters
                .insert("localPortNumber".into(), port.into());
            assert!(local_address(&invalid).is_none());
        }
        let mut custom = connection;
        custom.document_name = Some("Custom-Document".into());
        assert_eq!(connection_route(&custom), "Custom Document · View Details");
        assert!(local_address(&custom).is_none());
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
