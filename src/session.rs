use std::{
    collections::BTreeMap,
    process::ExitStatus,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Child,
    task::JoinHandle,
    time::{timeout, Duration},
};

use crate::{
    config::{Config, ConnectionConfig},
    ssm,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Starting,
    Forwarding,
    Running,
    Stopping,
    Stopped,
    Failed,
}

impl ConnectionState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Forwarding | Self::Running | Self::Stopping
        )
    }
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
    output: Option<SessionOutput>,
    exited_at: Option<Instant>,
    status: ConnectionStatus,
}

impl ManagedConnection {
    fn attach(&mut self, mut child: Child) {
        self.output = Some(SessionOutput::capture(&mut child, &self.config));
        self.exited_at = None;
        self.status.state = if ssm::is_forwarding(&self.config) {
            ConnectionState::Starting
        } else {
            ConnectionState::Running
        };
        self.status.pid = child.id();
        self.status.last_error = None;
        self.child = Some(child);
    }
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
                        output: None,
                        exited_at: None,
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
                    output: None,
                    exited_at: None,
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
            if managed.status.state.is_active()
                && config.connections.iter().find(|item| item.name == *name)
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
        if connection.status.state.is_active() {
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
        connection.attach(child);
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
                    let _ = stop_process_group(connection.status.pid);
                    connection.status.pid = None;
                    connection.child = None;
                    connection.output = None;
                    continue;
                }
            };
            let Some(status) = result else {
                if connection.status.state == ConnectionState::Starting
                    && connection
                        .output
                        .as_ref()
                        .is_some_and(|output| output.ready())
                {
                    connection.status.state = ConnectionState::Forwarding;
                }
                continue;
            };
            let _ = stop_process_group(connection.status.pid);
            // A descendant can retain a pipe, so final diagnostics must not block exit forever.
            let exited_at = connection.exited_at.get_or_insert_with(Instant::now);
            if exited_at.elapsed() < Duration::from_secs(1)
                && connection
                    .output
                    .as_ref()
                    .is_some_and(|output| !output.finished())
            {
                continue;
            }
            let was_stopping = connection.status.state == ConnectionState::Stopping;
            let startup_ended = connection.status.state == ConnectionState::Starting
                && !connection.output.as_ref().is_some_and(SessionOutput::ready);
            let error = exit_error(status, was_stopping).or_else(|| {
                startup_ended
                    .then(|| "AWS session ended before the local port was ready.".to_owned())
            });
            connection.status.state = if error.is_some() {
                ConnectionState::Failed
            } else {
                ConnectionState::Stopped
            };
            connection.status.pid = None;
            connection.status.last_error = error.map(|error| {
                let details = connection
                    .output
                    .as_ref()
                    .map(SessionOutput::diagnostics)
                    .unwrap_or_default();
                if details.is_empty() {
                    error
                } else {
                    format!("{error}\n\n{details}")
                }
            });
            connection.child = None;
            connection.output = None;
        }
    }

    pub async fn shutdown(&mut self) {
        let mut children = Vec::new();
        for connection in self.connections.values_mut() {
            if let Some(child) = connection.child.take() {
                let _ = stop_process_group(connection.status.pid);
                children.push(child);
            }
            connection.output = None;
        }
        for mut child in children {
            let _ = timeout(Duration::from_secs(2), child.wait()).await;
        }
    }
}

const OUTPUT_LIMIT: usize = 8192;

#[derive(Default)]
struct OutputBuffer {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    ready: bool,
}

struct SessionOutput {
    buffer: Arc<Mutex<OutputBuffer>>,
    readers: Vec<JoinHandle<()>>,
}

impl SessionOutput {
    fn capture(child: &mut Child, config: &ConnectionConfig) -> Self {
        let mut output = Self {
            buffer: Arc::new(Mutex::new(OutputBuffer::default())),
            readers: Vec::new(),
        };
        let port = config
            .parameters
            .get("localPortNumber")
            .and_then(|port| port.parse().ok());
        if let Some(stdout) = child.stdout.take() {
            output.readers.push(tokio::spawn(read_output(
                stdout,
                output.buffer.clone(),
                true,
                ssm::is_forwarding(config),
                port,
            )));
        }
        if let Some(stderr) = child.stderr.take() {
            output.readers.push(tokio::spawn(read_output(
                stderr,
                output.buffer.clone(),
                false,
                false,
                None,
            )));
        }
        output
    }

    fn ready(&self) -> bool {
        self.buffer.lock().unwrap().ready
    }

    fn finished(&self) -> bool {
        self.readers.iter().all(JoinHandle::is_finished)
    }

    fn diagnostics(&self) -> String {
        let buffer = self.buffer.lock().unwrap();
        [(&buffer.stderr, "stderr"), (&buffer.stdout, "stdout")]
            .into_iter()
            .filter_map(|(bytes, name)| {
                let text = String::from_utf8_lossy(bytes)
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                    .collect::<String>();
                (!text.trim().is_empty()).then(|| format!("{name}:\n{}", text.trim()))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

impl Drop for SessionOutput {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

async fn read_output(
    mut reader: impl AsyncRead + Unpin,
    buffer: Arc<Mutex<OutputBuffer>>,
    stdout: bool,
    forwarding: bool,
    port: Option<u16>,
) {
    let mut chunk = [0; 1024];
    loop {
        let count = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                let mut buffer = buffer.lock().unwrap();
                append_tail(
                    &mut buffer.stderr,
                    format!("\nCould not read AWS output: {error}").as_bytes(),
                );
                break;
            }
        };
        let mut buffer = buffer.lock().unwrap();
        let tail = if stdout {
            &mut buffer.stdout
        } else {
            &mut buffer.stderr
        };
        append_tail(tail, &chunk[..count]);
        if stdout && forwarding && forwarding_ready(tail, port) {
            buffer.ready = true;
        }
    }
}

fn append_tail(tail: &mut Vec<u8>, bytes: &[u8]) {
    tail.extend_from_slice(bytes);
    if tail.len() > OUTPUT_LIMIT {
        tail.drain(..tail.len() - OUTPUT_LIMIT);
    }
}

fn forwarding_ready(bytes: &[u8], expected_port: Option<u16>) -> bool {
    // shortcut: AWS plugin's post-listen message confirms the local port only; add remote probes only if requested.
    String::from_utf8_lossy(bytes).lines().any(|line| {
        let Some((port, session)) = line
            .trim()
            .strip_prefix("Port ")
            .and_then(|line| line.split_once(" opened for sessionId "))
        else {
            return false;
        };
        let Ok(port) = port.parse::<u16>() else {
            return false;
        };
        port != 0
            && expected_port
                .filter(|port| *port != 0)
                .is_none_or(|expected| expected == port)
            && session.ends_with('.')
            && session.len() > 1
    })
}

pub fn error_summary(error: &str) -> String {
    let lower = error.to_lowercase();
    let hint = if lower.contains("expiredtoken")
        || lower.contains("token has expired")
        || (lower.contains("sso session") && lower.contains("expired"))
    {
        Some("AWS login expired. Sign in again, then retry.")
    } else if lower.contains("accessdenied") || lower.contains("not authorized") {
        Some("AWS denied access. Check this profile's SSM permissions.")
    } else if lower.contains("address already in use") {
        Some("Local port is in use. Stop the other connection or choose another port.")
    } else if lower.contains("unable to locate credentials")
        || lower.contains("could not be found") && lower.contains("profile")
    {
        Some("AWS credentials are unavailable. Check the selected profile.")
    } else if lower.contains("targetnotconnected") {
        Some("SSM target is offline. Check the target and region, then retry.")
    } else if lower.contains("sessionmanagerplugin is not found")
        || lower.contains("session-manager-plugin: not found")
    {
        Some("Install the AWS Session Manager Plugin, then retry.")
    } else {
        None
    };
    hint.map(str::to_owned).unwrap_or_else(|| {
        let details = error
            .split_once("stderr:\n")
            .or_else(|| error.split_once("stdout:\n"))
            .map(|(_, details)| details)
            .unwrap_or(error);
        details
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Connection failed.")
            .chars()
            .take(180)
            .collect()
    })
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

    fn attach_captured_child(manager: &mut SessionManager, script: &str) {
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", script])
            .process_group(0)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        manager.connections.get_mut("test").unwrap().attach(child);
    }

    #[tokio::test]
    async fn forwarding_waits_for_its_local_port_and_can_be_stopped_while_starting() {
        let mut config = config();
        config.connections[0]
            .parameters
            .insert("localPortNumber".into(), "13306".into());
        let mut manager = SessionManager::new(config);
        attach_captured_child(&mut manager, "printf 'Port 13307 opened for sessionId wrong.\n'; printf 'Port 13306 opened for sessionId stderr.\n' >&2; sleep 60");
        tokio::time::sleep(Duration::from_millis(50)).await;
        manager.poll();
        assert_eq!(manager.statuses()[0].state, ConnectionState::Starting);
        assert!(manager.start("test").is_err());
        assert!(manager.reload_config(Config::default()).is_err());
        manager.stop("test").unwrap();
        wait_for_exit(&mut manager).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Stopped);
        assert!(manager.statuses()[0].last_error.is_none());

        attach_captured_child(
            &mut manager,
            "printf 'Port 133'; sleep 0.02; printf '06 opened for sessionId ready.\n'; sleep 60",
        );
        timeout(Duration::from_secs(3), async {
            while manager.statuses()[0].state != ConnectionState::Forwarding {
                manager.poll();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(manager.reload_config(Config::default()).is_err());
        manager.stop("test").unwrap();
        wait_for_exit(&mut manager).await;
    }

    #[tokio::test]
    async fn custom_document_does_not_claim_forwarding_readiness() {
        let mut config = config();
        config.connections[0].document_name = Some("Custom-Document".into());
        let mut manager = SessionManager::new(config);
        attach_captured_child(
            &mut manager,
            "printf 'Port 13306 opened for sessionId custom.\n'; sleep 60",
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        manager.poll();
        assert_eq!(manager.statuses()[0].state, ConnectionState::Running);
        manager.stop("test").unwrap();
        wait_for_exit(&mut manager).await;
    }

    #[tokio::test]
    async fn forwarding_exit_without_readiness_is_a_startup_failure_even_with_zero_exit() {
        let mut manager = SessionManager::new(config());
        attach_captured_child(
            &mut manager,
            "printf 'Session ended before opening the port\n'; exit 0",
        );
        wait_for_exit(&mut manager).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Failed);
        assert!(manager.statuses()[0]
            .last_error
            .as_ref()
            .unwrap()
            .contains("before the local port was ready"));
    }

    #[tokio::test]
    async fn lingering_output_reader_cannot_hold_an_exited_session_active() {
        let mut manager = SessionManager::new(config());
        attach_captured_child(&mut manager, "exit 42");
        let output = manager
            .connections
            .get_mut("test")
            .unwrap()
            .output
            .as_mut()
            .unwrap();
        output.readers.push(tokio::spawn(std::future::pending()));
        wait_for_exit(&mut manager).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Failed);
        assert!(manager.connections["test"].output.is_none());
    }

    #[tokio::test]
    async fn noisy_pipes_are_drained_and_final_error_is_retained() {
        let mut manager = SessionManager::new(config());
        attach_captured_child(&mut manager, "i=0; while [ $i -lt 2000 ]; do printf 'long diagnostic output with no newline'; printf 'more diagnostic output with no newline' >&2; i=$((i + 1)); done; printf '\nUnknown failure after noisy output\n' >&2; exit 42");
        wait_for_exit(&mut manager).await;
        let status = &manager.statuses()[0];
        assert_eq!(status.state, ConnectionState::Failed);
        let error = status.last_error.as_ref().unwrap();
        assert!(error.contains("Unknown failure after noisy output"));
        assert!(error.len() <= OUTPUT_LIMIT * 2 + 100);
        assert!(manager.connections["test"].output.is_none());
        attach_captured_child(&mut manager, "sleep 60");
        assert!(manager.statuses()[0].last_error.is_none());
        manager.shutdown().await;
        assert!(manager.connections["test"].output.is_none());
    }

    #[test]
    fn readiness_and_error_summaries_are_specific_and_keep_unknown_errors() {
        assert!(forwarding_ready(
            b"Port 13306 opened for sessionId example.\n",
            Some(13306)
        ));
        assert!(!forwarding_ready(
            b"Port 13306 opened for sessionId example",
            Some(13306)
        ));
        assert!(!forwarding_ready(
            b"Port 13307 opened for sessionId example.\n",
            Some(13306)
        ));
        assert!(!forwarding_ready(
            b"Port 0 opened for sessionId example.\n",
            None
        ));
        assert!(error_summary("ExpiredTokenException").contains("Sign in"));
        assert!(error_summary("AccessDeniedException").contains("permissions"));
        assert!(error_summary("bind: address already in use").contains("another port"));
        assert_eq!(
            error_summary("AWS session exited with code 42\n\nstderr:\nUnexpected example error"),
            "Unexpected example error"
        );
        let mut tail = Vec::new();
        append_tail(&mut tail, &vec![b'x'; OUTPUT_LIMIT * 3]);
        assert_eq!(tail.len(), OUTPUT_LIMIT);
        assert!(!forwarding_ready(&[0xff, 0xfe], None));
        let output = SessionOutput {
            buffer: Arc::new(Mutex::new(OutputBuffer {
                stdout: vec![0xff; OUTPUT_LIMIT],
                ..OutputBuffer::default()
            })),
            readers: Vec::new(),
        };
        assert!(output.diagnostics().len() <= OUTPUT_LIMIT * 3 + 20);
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
