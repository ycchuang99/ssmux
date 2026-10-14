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
    menu::{IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::{
    config::{Config, ConnectionConfig},
    menubar_form,
    session::{ConnectionState, SessionManager},
    ssm,
};

#[derive(Clone, Copy)]
enum SessionAction {
    Start,
    Stop,
}

struct ConnectionMenu {
    name: String,
    item: IconMenuItem,
    state: ConnectionState,
    interactive: bool,
}

pub async fn run(config_path: &Path) -> Result<()> {
    let mut config = Config::load_or_default(config_path)?;
    let mut manager = SessionManager::new(config.clone());
    let menu = Menu::new();
    menu.append(&MenuItem::new("ssmux", false, None))?;
    let summary = MenuItem::new("No active sessions", false, None);
    menu.append(&summary)?;
    let message = MenuItem::new("Click ▶ to start · ■ to stop", false, None);
    menu.append(&message)?;
    menu.append(&PredefinedMenuItem::separator())?;

    let mut connections = Vec::new();
    for connection in &config.connections {
        let item = build_connection(connection)?;
        menu.append(&item.item)?;
        connections.push(item);
    }
    let mut empty_item = if connections.is_empty() {
        let item = MenuItem::new("No sessions configured", false, None);
        menu.append(&item)?;
        Some(item)
    } else {
        None
    };
    menu.append(&PredefinedMenuItem::separator())?;
    let add = MenuItem::new("New Session…", true, None);
    menu.append(&add)?;
    let quit = MenuItem::new("Quit ssmux & Stop Sessions", true, None);
    menu.append(&quit)?;

    let idle_pixels = include_bytes!("../assets/menu-template.rgba");
    let icons = [
        Icon::from_rgba(idle_pixels.to_vec(), 36, 36)?,
        Icon::from_rgba(green_pixels(idle_pixels), 36, 36)?,
    ];
    let mut event_loop = EventLoopBuilder::<MenuEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(event);
    }));

    let mut tray = None;
    let mut launch_error = None;
    let mut last_counts = None;
    let quit_id = quit.id().clone();
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
        match event {
            Event::NewEvents(StartCause::Init) => {
                match TrayIconBuilder::new()
                    .with_menu(Box::new(menu.clone()))
                    .with_icon(icons[0].clone())
                    .with_icon_as_template(true)
                    .with_tooltip("ssmux")
                    .build()
                {
                    Ok(item) => tray = Some(item),
                    Err(error) => {
                        launch_error = Some(error);
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(event) if event.id == quit_id => *control_flow = ControlFlow::Exit,
            Event::UserEvent(event) if event.id == *add.id() => {
                let result = (|| -> Result<Option<String>> {
                    let Some(connection) = menubar_form::new_connection(&config)? else {
                        return Ok(None);
                    };
                    let item = build_connection(&connection)?;
                    let name = connection.name.clone();
                    save_new_connection(config_path, &mut config, &mut manager, connection)?;
                    if let Some(empty) = empty_item.take() {
                        menu.remove(&empty)?;
                    }
                    menu.insert(&item.item, 4 + connections.len())?;
                    connections.push(item);
                    last_counts = None;
                    Ok(Some(name))
                })();
                match result {
                    Ok(Some(name)) => {
                        set_text(&message, format!("Saved {name} · ready to connect"))
                    }
                    Ok(None) => {}
                    Err(error) => set_text(&message, format!("Could not add session: {error}")),
                }
            }
            Event::UserEvent(event) => {
                if let Some(connection) = connections
                    .iter()
                    .find(|connection| *connection.item.id() == event.id)
                {
                    let name = connection.name.clone();
                    let state = manager
                        .statuses()
                        .into_iter()
                        .find(|status| status.name == name)
                        .map(|status| status.state);
                    let action = if connection.interactive {
                        None
                    } else {
                        match state {
                            Some(ConnectionState::Running) => Some(SessionAction::Stop),
                            Some(ConnectionState::Stopped | ConnectionState::Failed) => {
                                Some(SessionAction::Start)
                            }
                            Some(ConnectionState::Stopping) | None => None,
                        }
                    };
                    if let Some(action) = action {
                        let result = match action {
                            SessionAction::Start => manager.start(&name),
                            SessionAction::Stop => manager.stop(&name),
                        };
                        set_text(
                            &message,
                            match result {
                                Ok(()) => format!(
                                    "{} {name}",
                                    match action {
                                        SessionAction::Start => "Started",
                                        SessionAction::Stop => "Stopping",
                                    }
                                ),
                                Err(error) => format!("{name}: {error}"),
                            },
                        );
                    }
                }
            }
            Event::MainEventsCleared => {
                manager.poll();
                if let Some(tray) = &tray {
                    if let Err(error) = update_menu(
                        &manager,
                        &mut connections,
                        &summary,
                        &message,
                        tray,
                        &icons,
                        &mut last_counts,
                    ) {
                        set_text(&message, format!("Could not update status icon: {error}"));
                    }
                }
            }
            _ => {}
        }
    });
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
    summary: &MenuItem,
    message: &MenuItem,
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
            "{active} active · {failed} failed · {} configured",
            statuses.len()
        );
        set_text(summary, text);
        tray.set_icon_with_as_template(Some(icons[usize::from(active > 0)].clone()), active == 0)?;
        *last_counts = Some((active, failed));
    }
    for connection in connections {
        let Some(status) = statuses.iter().find(|s| s.name == connection.name) else {
            continue;
        };
        if connection.state != status.state {
            connection.item.set_text(connection_text(
                &connection.name,
                status.state,
                connection.interactive,
            ));
            connection
                .item
                .set_icon(Some(status_dot(status_color(status.state))?));
            if status.state == ConnectionState::Failed {
                if let Some(error) = &status.last_error {
                    set_text(message, format!("{}: {error}", connection.name));
                }
            }
            connection.state = status.state;
        }
        let can_run = !connection.interactive && status.state != ConnectionState::Stopping;
        if connection.item.is_enabled() != can_run {
            connection.item.set_enabled(can_run);
        }
    }
    Ok(())
}

fn build_connection(connection: &ConnectionConfig) -> Result<ConnectionMenu> {
    let interactive = ssm::is_interactive(connection);
    let state = ConnectionState::Stopped;
    let item = IconMenuItem::new(
        connection_text(&connection.name, state, interactive),
        !interactive,
        Some(status_dot(status_color(state))?),
        None,
    );
    Ok(ConnectionMenu {
        name: connection.name.clone(),
        item,
        state,
        interactive,
    })
}

fn connection_text(name: &str, state: ConnectionState, interactive: bool) -> String {
    if interactive {
        format!("{name}  TUI only")
    } else {
        let action = match state {
            ConnectionState::Running | ConnectionState::Stopping => "■",
            ConnectionState::Stopped | ConnectionState::Failed => "▶",
        };
        format!("{name}  {action}")
    }
}

fn status_color(state: ConnectionState) -> [u8; 3] {
    match state {
        ConnectionState::Running => [46, 204, 113],
        ConnectionState::Stopping => [235, 170, 45],
        ConnectionState::Stopped => [145, 145, 150],
        ConnectionState::Failed => [230, 75, 75],
    }
}

fn green_pixels(pixels: &[u8]) -> Vec<u8> {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [46, 204, 113, pixel[3]])
        .collect()
}

fn status_dot(color: [u8; 3]) -> Result<tray_icon::menu::Icon> {
    let mut pixels = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16 {
        for x in 0..16 {
            let distance = ((x as f64 - 7.5).powi(2) + (y as f64 - 7.5).powi(2)).sqrt();
            let alpha = ((5.5 - distance).clamp(0., 1.) * 255.) as u8;
            pixels.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    tray_icon::menu::Icon::from_rgba(pixels, 16, 16).context("creating the status dot")
}

fn save_new_connection(
    path: &Path,
    config: &mut Config,
    manager: &mut SessionManager,
    connection: ConnectionConfig,
) -> Result<()> {
    if Config::load_or_default(path)?.connections != config.connections {
        anyhow::bail!("configuration changed outside SSMux; restart it before adding a session");
    }
    let mut candidate = config.clone();
    candidate.connections.push(connection);
    candidate.validate()?;
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
    fn adding_a_session_persists_and_updates_manager_without_overwriting_external_edits() {
        let directory = std::env::temp_dir().join(format!(
            "ssmux-add-session-{}-{}",
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
        save_new_connection(&path, &mut config, &mut manager, connection.clone()).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().connections,
            vec![connection.clone()]
        );
        assert_eq!(manager.statuses()[0].name, "new");
        assert_eq!(manager.statuses()[0].state, ConnectionState::Stopped);
        let mut external = config.clone();
        let mut outside = connection.clone();
        outside.name = "external".into();
        external.connections.push(outside);
        external.save_atomic(&path).unwrap();
        let mut another = connection;
        another.name = "another".into();
        assert!(save_new_connection(&path, &mut config, &mut manager, another).is_err());
        assert_eq!(
            Config::load(&path).unwrap().connections,
            external.connections
        );
        assert_eq!(manager.statuses().len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
