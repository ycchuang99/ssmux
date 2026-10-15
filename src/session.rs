use std::{collections::BTreeMap, process::ExitStatus};

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
    pub state: ConnectionState,
    pub pid: Option<u32>,
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
                    state: ConnectionState::Stopped,
                    pid: None,
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
        self.validate_reload(&config)?;
        let new_configs = config
            .connections
            .into_iter()
            .map(|connection| (connection.name.clone(), connection))
            .collect::<BTreeMap<_, _>>();

        let mut connections = BTreeMap::new();
        for (name, config) in new_configs {
            let mut managed = self.connections.remove(&name).unwrap_or_else(|| {
                let status = ConnectionStatus {
                    name: name.clone(),
                    state: ConnectionState::Stopped,
                    pid: None,
                    last_error: None,
                };
                ManagedConnection {
                    config: config.clone(),
                    child: None,
                    status,
                }
            });
            managed.config = config;
            connections.insert(name, managed);
        }
        self.connections = connections;
        Ok(())
    }

    pub fn validate_reload(&self, config: &Config) -> Result<()> {
        config.validate()?;
        for (name, managed) in &self.connections {
            if matches!(
                managed.status.state,
                ConnectionState::Running | ConnectionState::Stopping
            ) && config.connections.iter().find(|item| item.name == *name)
                != Some(&managed.config)
            {
                anyhow::bail!("stop session {name} before editing or deleting it");
            }
        }
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

        let child = match ssm::spawn_session(&connection.config) {
            Ok(child) => child,
            Err(error) => {
                connection.status.state = ConnectionState::Failed;
                connection.status.last_error = Some(format!("{error:#}"));
                return Err(error).with_context(|| format!("starting AWS session for {name}"));
            }
        };
        connection.status.state = ConnectionState::Running;
        connection.status.pid = child.id();
        connection.status.last_error = None;
        connection.child = Some(child);
        Ok(())
    }

    pub fn stop(&mut self, name: &str) -> Result<()> {
        let connection = self
            .connections
            .get_mut(name)
            .with_context(|| format!("unknown connection: {name}"))?;
        if matches!(
            connection.status.state,
            ConnectionState::Stopped | ConnectionState::Failed
        ) {
            anyhow::bail!("connection {name} is not running");
        }

        if connection.child.is_some() {
            stop_process_group(connection.status.pid).context("stopping AWS session")?;
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
            let _ = stop_process_group(connection.status.pid);
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
            if let Some(child) = connection.child.take() {
                let _ = stop_process_group(connection.status.pid);
                children.push(child);
            }
        }
        for mut child in children {
            let _ = timeout(Duration::from_secs(2), child.wait()).await;
        }
    }
}

fn stop_process_group(pid: Option<u32>) -> Result<()> {
    if let Some(pid) = pid {
        // AWS launches the plugin as a child; stop the whole group to release forwarded ports.
        if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn config() -> Config {
        Config {
            connections: vec![ConnectionConfig {
                name: "test".into(),
                target: "i-test".into(),
                region: None,
                profile: None,
                document_name: Some("AWS-StartPortForwardingSession".into()),
                parameters: BTreeMap::new(),
                extra_args: vec![],
            }],
        }
    }

    fn attach_child(manager: &mut SessionManager, script: &str) -> u32 {
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", script])
            .process_group(0)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let managed = manager.connections.get_mut("test").unwrap();
        managed.status.state = ConnectionState::Running;
        managed.status.pid = Some(pid);
        managed.child = Some(child);
        pid
    }

    async fn wait_for_exit(manager: &mut SessionManager) {
        timeout(Duration::from_secs(3), async {
            while manager.statuses()[0].pid.is_some() {
                manager.poll();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn active_sessions_block_edits_and_delete_until_stopped_and_release_process_group() {
        let original = config();
        let mut manager = SessionManager::new(original.clone());
        let pid = attach_child(&mut manager, "sleep 60 & echo $!; wait");
        use tokio::io::{AsyncBufReadExt, BufReader};
        let stdout = manager
            .connections
            .get_mut("test")
            .unwrap()
            .child
            .as_mut()
            .unwrap()
            .stdout
            .take()
            .unwrap();
        let mut line = String::new();
        timeout(
            Duration::from_secs(3),
            BufReader::new(stdout).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let plugin_pid: i32 = line.trim().parse().unwrap();
        assert_eq!(unsafe { libc::kill(plugin_pid, 0) }, 0);
        assert!(manager.reload_config(original.clone()).is_ok());
        let mut edited = original;
        edited.connections[0].target = "i-other".into();
        assert!(manager.validate_reload(&edited).is_err());
        assert!(manager.reload_config(Config::default()).is_err());
        manager.stop("test").unwrap();
        assert_eq!(manager.statuses()[0].state, ConnectionState::Stopping);
        assert!(manager.validate_reload(&edited).is_err());
        wait_for_exit(&mut manager).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Stopped);
        assert!(manager.statuses()[0].last_error.is_none());
        timeout(Duration::from_secs(3), async {
            while unsafe { libc::kill(-(pid as i32), 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(unsafe { libc::kill(plugin_pid, 0) }, -1);
        manager.reload_config(edited).unwrap();
        assert_eq!(manager.connections["test"].config.target, "i-other");
        manager.reload_config(Config::default()).unwrap();
        assert!(manager.statuses().is_empty());
    }

    #[test]
    fn unsupported_shell_start_records_a_failure_without_spawning() {
        let mut shell = config();
        shell.connections[0].document_name = None;
        let mut manager = SessionManager::new(shell);
        assert!(manager.start("test").is_err());
        let status = &manager.statuses()[0];
        assert_eq!(status.state, ConnectionState::Failed);
        assert!(status.pid.is_none());
        assert!(status.last_error.as_ref().unwrap().contains("terminal"));
    }

    #[tokio::test]
    async fn failed_exit_and_shutdown_are_observed() {
        let mut manager = SessionManager::new(config());
        attach_child(&mut manager, "exit 42");
        wait_for_exit(&mut manager).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Failed);
        assert!(manager.statuses()[0]
            .last_error
            .as_ref()
            .unwrap()
            .contains("42"));
        let pid = attach_child(&mut manager, "sleep 60 & wait");
        manager.shutdown().await;
        assert_eq!(unsafe { libc::kill(-(pid as i32), 0) }, -1);
    }
}
