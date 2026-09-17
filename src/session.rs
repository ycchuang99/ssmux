use std::{
    collections::BTreeMap,
    process::ExitStatus,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use tokio::{
    process::Child,
    time::{timeout, Duration},
};

use crate::{
    config::{Config, ConnectionConfig},
    ssm,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ConnectionStatus {
    pub name: String,
    pub target: String,
    pub document_name: Option<String>,
    pub state: ConnectionState,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub last_error: Option<String>,
}

struct ManagedConnection {
    config: ConnectionConfig,
    child: Option<Child>,
    status: ConnectionStatus,
}

pub struct SessionManager {
    connections: BTreeMap<String, ManagedConnection>,
}

impl SessionManager {
    pub fn new(config: Config) -> Self {
        let connections = config
            .connections
            .into_iter()
            .map(|config| {
                let status = ConnectionStatus {
                    name: config.name.clone(),
                    target: config.target.clone(),
                    document_name: config.document_name.clone(),
                    state: ConnectionState::Stopped,
                    pid: None,
                    started_at: None,
                    last_error: None,
                };
                (
                    config.name.clone(),
                    ManagedConnection {
                        config,
                        child: None,
                        status,
                    },
                )
            })
            .collect();
        Self { connections }
    }

    pub fn statuses(&self) -> Vec<ConnectionStatus> {
        self.connections
            .values()
            .map(|connection| connection.status.clone())
            .collect()
    }

    pub fn reload_config(&mut self, config: Config) -> Result<()> {
        config.validate()?;
        let new_configs = config
            .connections
            .into_iter()
            .map(|connection| (connection.name.clone(), connection))
            .collect::<BTreeMap<_, _>>();

        for (name, managed) in &self.connections {
            let active = matches!(
                managed.status.state,
                ConnectionState::Running | ConnectionState::Stopping
            );
            if active {
                match new_configs.get(name) {
                    None => anyhow::bail!("cannot remove active connection {name}; stop it first"),
                    Some(new_config) if managed.config == *new_config => {}
                    Some(_) => anyhow::bail!("cannot edit active connection {name}; stop it first"),
                }
            }
        }

        let mut connections = BTreeMap::new();
        for (name, config) in new_configs {
            let mut managed = self.connections.remove(&name).unwrap_or_else(|| {
                let status = ConnectionStatus {
                    name: name.clone(),
                    target: config.target.clone(),
                    document_name: config.document_name.clone(),
                    state: ConnectionState::Stopped,
                    pid: None,
                    started_at: None,
                    last_error: None,
                };
                ManagedConnection {
                    config: config.clone(),
                    child: None,
                    status,
                }
            });
            managed.config = config.clone();
            managed.status.target = config.target;
            managed.status.document_name = config.document_name;
            connections.insert(name, managed);
        }
        self.connections = connections;
        Ok(())
    }

    pub fn start(&mut self, name: &str) -> Result<()> {
        let connection = self
            .connections
            .get_mut(name)
            .with_context(|| format!("unknown connection: {name}"))?;
        if matches!(
            connection.status.state,
            ConnectionState::Running | ConnectionState::Stopping
        ) {
            anyhow::bail!("connection {name} is already active");
        }

        let child = ssm::spawn_session(&connection.config)
            .with_context(|| format!("starting AWS session for {name}"))?;
        connection.status.state = ConnectionState::Running;
        connection.status.pid = child.id();
        connection.status.started_at = Some(unix_timestamp());
        connection.status.last_error = None;
        connection.child = Some(child);
        Ok(())
    }

    pub fn stop(&mut self, name: &str) -> Result<()> {
        let connection = self
            .connections
            .get_mut(name)
            .with_context(|| format!("unknown connection: {name}"))?;
        if connection.status.state == ConnectionState::Stopped {
            anyhow::bail!("connection {name} is not running");
        }
        if connection.status.state == ConnectionState::Failed {
            anyhow::bail!("connection {name} is not running");
        }

        if let Some(child) = connection.child.as_mut() {
            child.start_kill().context("stopping AWS session")?;
            connection.status.state = ConnectionState::Stopping;
        } else {
            connection.status.state = ConnectionState::Stopped;
        }
        Ok(())
    }

    pub fn poll(&mut self) {
        for connection in self.connections.values_mut() {
            let Some(child) = connection.child.as_mut() else {
                continue;
            };
            let result = match child.try_wait() {
                Ok(result) => result,
                Err(error) => {
                    connection.status.state = ConnectionState::Failed;
                    connection.status.last_error = Some(error.to_string());
                    connection.status.pid = None;
                    connection.child = None;
                    continue;
                }
            };
            let Some(status) = result else { continue };
            let was_stopping = connection.status.state == ConnectionState::Stopping;
            connection.status.state = if was_stopping || status.success() {
                ConnectionState::Stopped
            } else {
                ConnectionState::Failed
            };
            connection.status.pid = None;
            connection.status.last_error = exit_error(status, was_stopping);
            connection.child = None;
        }
    }

    pub async fn shutdown(&mut self) {
        let mut children = Vec::new();
        for connection in self.connections.values_mut() {
            if let Some(mut child) = connection.child.take() {
                let _ = child.start_kill();
                children.push(child);
            }
        }
        for mut child in children {
            let _ = timeout(Duration::from_secs(2), child.wait()).await;
        }
    }
}

fn exit_error(status: ExitStatus, was_stopping: bool) -> Option<String> {
    if was_stopping || status.success() {
        None
    } else {
        Some(match status.code() {
            Some(code) => format!("AWS session exited with code {code}"),
            None => "AWS session terminated by signal".to_owned(),
        })
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
