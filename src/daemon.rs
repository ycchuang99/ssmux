use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, Mutex},
    task::JoinHandle,
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    config::{Config, ConnectionConfig},
    ipc::{ConnectionState, ConnectionStatus, Request, Response},
    ssm,
};

#[derive(Debug)]
struct ProcessEvent {
    name: String,
    stopped_by_request: bool,
    exit_code: Option<i32>,
    error: Option<String>,
}

struct ManagedConnection {
    status: ConnectionStatus,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}

pub struct ConnectionManager {
    config_path: PathBuf,
    configs: BTreeMap<String, ConnectionConfig>,
    connections: BTreeMap<String, ManagedConnection>,
    events: mpsc::UnboundedSender<ProcessEvent>,
}

impl ConnectionManager {
    fn new(
        config: Config,
        config_path: PathBuf,
        events: mpsc::UnboundedSender<ProcessEvent>,
    ) -> Self {
        let configs = config
            .connections
            .into_iter()
            .map(|connection| (connection.name.clone(), connection))
            .collect::<BTreeMap<_, _>>();
        let connections = configs
            .values()
            .map(|config| {
                (
                    config.name.clone(),
                    ManagedConnection {
                        status: ConnectionStatus {
                            name: config.name.clone(),
                            target: config.target.clone(),
                            keep_on_exit: config.keep_on_exit,
                            state: ConnectionState::Stopped,
                            pid: None,
                            started_at: None,
                            last_error: None,
                        },
                        cancel: CancellationToken::new(),
                        task: None,
                    },
                )
            })
            .collect();
        Self {
            config_path,
            configs,
            connections,
            events,
        }
    }

    fn statuses(&self) -> Vec<ConnectionStatus> {
        self.connections
            .values()
            .map(|connection| connection.status.clone())
            .collect()
    }

    fn status_for(&self, name: &str) -> Result<&ManagedConnection> {
        self.connections
            .get(name)
            .with_context(|| format!("unknown connection: {name}"))
    }

    fn start(&mut self, name: &str) -> Result<()> {
        let config = self
            .configs
            .get(name)
            .cloned()
            .with_context(|| format!("unknown connection: {name}"))?;

        if let Some(existing) = self.connections.get(name) {
            match existing.status.state {
                ConnectionState::Starting | ConnectionState::Running => {
                    anyhow::bail!("connection {name} is already active");
                }
                ConnectionState::Stopping => {
                    anyhow::bail!("connection {name} is still stopping");
                }
                ConnectionState::Stopped | ConnectionState::Failed => {}
            }
        }

        let child = ssm::spawn_session(&config)
            .with_context(|| format!("starting AWS session for {name}"))?;
        let pid = child.id();
        let cancel = CancellationToken::new();
        let child_cancel = cancel.clone();
        let child_name = name.to_owned();
        let events = self.events.clone();
        let task = tokio::spawn(async move {
            monitor_process(child_name, child, child_cancel, events).await;
        });

        let managed = self
            .connections
            .get_mut(name)
            .expect("configuration and status maps are initialized together");
        managed.status.state = ConnectionState::Running;
        managed.status.pid = pid;
        managed.status.started_at = Some(unix_timestamp());
        managed.status.last_error = None;
        managed.cancel = cancel;
        managed.task = Some(task);
        info!(connection = name, ?pid, "started SSM session");
        Ok(())
    }

    fn stop(&mut self, name: &str) -> Result<()> {
        let managed = self.status_for(name)?;
        match managed.status.state {
            ConnectionState::Starting | ConnectionState::Running => {}
            ConnectionState::Stopping => anyhow::bail!("connection {name} is already stopping"),
            ConnectionState::Stopped => anyhow::bail!("connection {name} is not running"),
            ConnectionState::Failed => anyhow::bail!("connection {name} is not running"),
        }

        let managed = self
            .connections
            .get_mut(name)
            .expect("status_for verified the connection exists");
        managed.status.state = ConnectionState::Stopping;
        managed.cancel.cancel();
        info!(connection = name, "stopping SSM session");
        Ok(())
    }

    fn stop_all(&mut self, keep_on_exit: bool) -> usize {
        let mut stopped = 0;
        for managed in self.connections.values_mut() {
            if managed.status.state == ConnectionState::Running
                && (!keep_on_exit || !managed.status.keep_on_exit)
            {
                managed.status.state = ConnectionState::Stopping;
                managed.cancel.cancel();
                stopped += 1;
            }
        }
        stopped
    }

    fn reload(&mut self) -> Result<()> {
        let config = Config::load(&self.config_path)?;
        let new_configs = config
            .connections
            .into_iter()
            .map(|connection| (connection.name.clone(), connection))
            .collect::<BTreeMap<_, _>>();

        for (name, managed) in &self.connections {
            let active = matches!(
                managed.status.state,
                ConnectionState::Starting | ConnectionState::Running | ConnectionState::Stopping
            );
            if active && self.configs.get(name) != new_configs.get(name) {
                anyhow::bail!(
                    "cannot reload active connection {name}; stop it before changing or removing it"
                );
            }
        }

        let mut connections = BTreeMap::new();
        for (name, new_config) in &new_configs {
            let managed = if let Some(mut existing) = self.connections.remove(name) {
                existing.status.target = new_config.target.clone();
                existing.status.keep_on_exit = new_config.keep_on_exit;
                existing
            } else {
                ManagedConnection {
                    status: ConnectionStatus {
                        name: name.clone(),
                        target: new_config.target.clone(),
                        keep_on_exit: new_config.keep_on_exit,
                        state: ConnectionState::Stopped,
                        pid: None,
                        started_at: None,
                        last_error: None,
                    },
                    cancel: CancellationToken::new(),
                    task: None,
                }
            };
            connections.insert(name.clone(), managed);
        }

        self.configs = new_configs;
        self.connections = connections;
        Ok(())
    }

    fn handle_process_event(&mut self, event: ProcessEvent) {
        let Some(managed) = self.connections.get_mut(&event.name) else {
            warn!(
                connection = event.name,
                "received event for unknown session"
            );
            return;
        };

        managed.status.pid = None;
        managed.status.state = if event.stopped_by_request || event.exit_code == Some(0) {
            ConnectionState::Stopped
        } else {
            ConnectionState::Failed
        };
        managed.status.last_error = event.error.or_else(|| {
            event
                .exit_code
                .filter(|code| *code != 0)
                .map(|code| format!("AWS session exited with code {code}"))
        });
        if managed.status.state == ConnectionState::Failed {
            warn!(connection = event.name, error = ?managed.status.last_error, "SSM session failed");
        } else {
            info!(connection = event.name, "SSM session stopped");
        }
    }

    async fn shutdown(&mut self) {
        let mut tasks = Vec::new();
        for managed in self.connections.values_mut() {
            if matches!(
                managed.status.state,
                ConnectionState::Starting | ConnectionState::Running
            ) {
                managed.status.state = ConnectionState::Stopping;
                managed.cancel.cancel();
            }
            if let Some(task) = managed.task.take() {
                tasks.push(task);
            }
        }
        for task in tasks {
            let _ = timeout(Duration::from_secs(2), task).await;
        }
    }
}

pub async fn run(config_path: &Path, socket_path: &Path) -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();
    let config = Config::load(config_path)?;
    let listener = bind_socket(socket_path).await?;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(ConnectionManager::new(
        config,
        config_path.to_owned(),
        event_tx,
    )));
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);

    info!(socket = %socket_path.display(), "daemon started");

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                manager.lock().await.handle_process_event(event);
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let manager = Arc::clone(&manager);
                        tokio::spawn(async move {
                            if let Err(error) = handle_client(stream, manager).await {
                                error!(%error, "IPC request failed");
                            }
                        });
                    }
                    Err(error) => warn!(%error, "accept failed"),
                }
            }
            signal = &mut shutdown_signal => {
                signal?;
                break;
            }
        }
    }

    manager.lock().await.shutdown().await;
    remove_socket(socket_path);
    info!("daemon stopped");
    Ok(())
}

async fn wait_for_shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate =
            signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.context("waiting for Ctrl-C"),
            _ = terminate.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.context("waiting for Ctrl-C")
    }
}

async fn bind_socket(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating socket directory {}", parent.display()))?;
    }

    if socket_path.exists() {
        match UnixStream::connect(socket_path).await {
            Ok(_) => anyhow::bail!("another daemon is already using {}", socket_path.display()),
            Err(_) => std::fs::remove_file(socket_path)
                .with_context(|| format!("removing stale socket {}", socket_path.display()))?,
        }
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("binding daemon socket {}", socket_path.display()))?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protecting daemon socket {}", socket_path.display()))?;
    Ok(listener)
}

async fn handle_client(stream: UnixStream, manager: Arc<Mutex<ConnectionManager>>) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: Request = serde_json::from_str(line.trim()).context("decoding IPC request")?;

    let response = {
        let mut manager = manager.lock().await;
        match request {
            Request::Ping => Response::ok("daemon is running", manager.statuses()),
            Request::Status => Response::ok("status retrieved", manager.statuses()),
            Request::Reload => match manager.reload() {
                Ok(()) => Response::ok("configuration reloaded", manager.statuses()),
                Err(error) => Response::error(error.to_string()),
            },
            Request::Start { name } => match manager.start(&name) {
                Ok(()) => Response::ok(format!("started {name}"), manager.statuses()),
                Err(error) => Response::error(error.to_string()),
            },
            Request::Stop { name } => match manager.stop(&name) {
                Ok(()) => Response::ok(format!("stopping {name}"), manager.statuses()),
                Err(error) => Response::error(error.to_string()),
            },
            Request::StopAll => {
                let count = manager.stop_all(false);
                Response::ok(
                    format!("stopping {count} connection(s)"),
                    manager.statuses(),
                )
            }
            Request::TuiExit => {
                let count = manager.stop_all(true);
                Response::ok(
                    format!("stopping {count} temporary connection(s)"),
                    manager.statuses(),
                )
            }
        }
    };

    let mut stream = reader.into_inner();
    let payload = serde_json::to_vec(&response)?;
    stream.write_all(&payload).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;
    Ok(())
}

async fn monitor_process(
    name: String,
    mut child: tokio::process::Child,
    cancel: CancellationToken,
    events: mpsc::UnboundedSender<ProcessEvent>,
) {
    if let Some(mut stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stdout, &mut sink).await;
        });
    }
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stderr, &mut sink).await;
        });
    }

    let (stopped_by_request, exit_code, error) = tokio::select! {
        result = child.wait() => match result {
            Ok(status) => (false, status.code(), None),
            Err(error) => (false, None, Some(error.to_string())),
        },
        _ = cancel.cancelled() => {
            let kill_error = child.kill().await.err().map(|error| error.to_string());
            let wait_result = child.wait().await;
            let wait_error = wait_result.as_ref().err().map(|error| error.to_string());
            (true, wait_result.ok().and_then(|status| status.code()), kill_error.or(wait_error))
        }
    };

    let _ = events.send(ProcessEvent {
        name,
        stopped_by_request,
        exit_code,
        error,
    });
}

fn remove_socket(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!(path = %path.display(), %error, "could not remove daemon socket");
        }
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
