use std::{collections::BTreeMap, process::ExitStatus, time::Instant};

use anyhow::{Context, Result};
use tokio::time::{timeout, Duration};

use crate::{
    config::{Config, ConnectionConfig},
    ssm::{self, SessionProcess},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_RETRIES: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Connecting,
    Running,
    Reconnecting,
    Stopping,
    Stopped,
    Disconnected,
    Failed,
}

impl ConnectionState {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Running | Self::Reconnecting | Self::Stopping
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureKind {
    SsoExpired,
    Credentials,
    Permission,
    TargetUnavailable,
    PortInUse,
    MissingDependency,
    Network,
    Timeout,
    Unknown,
}

impl FailureKind {
    pub fn summary(self) -> &'static str {
        match self {
            Self::SsoExpired => "AWS SSO sign-in expired",
            Self::Credentials => "AWS credentials are unavailable or expired",
            Self::Permission => "AWS denied access to this session",
            Self::TargetUnavailable => "The destination is unavailable",
            Self::PortInUse => "The local port is already in use",
            Self::MissingDependency => "An AWS command dependency is unavailable",
            Self::Network => "The session connection was interrupted",
            Self::Timeout => "The tunnel did not become ready within 30 seconds",
            Self::Unknown => "The session could not be completed",
        }
    }

    pub fn suggestion(self) -> &'static str {
        match self {
            Self::SsoExpired => "Sign in again with this AWS SSO profile, then reconnect.",
            Self::Credentials => "Refresh the credentials for this profile, then reconnect.",
            Self::Permission => "Check the profile and ask your AWS administrator to verify SSM permissions.",
            Self::TargetUnavailable => "Check the target, region, SSM Agent, and destination reachability.",
            Self::PortInUse => "Stop the other session or edit this session's local port.",
            Self::MissingDependency => "Check Setup for AWS CLI and Session Manager Plugin installation.",
            Self::Network => "Check your network, then reconnect.",
            Self::Timeout => "Check the target and document; only a confirmed port-forwarding listener is marked ready.",
            Self::Unknown => "Review the diagnostic details before retrying.",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionStatus {
    pub name: String,
    pub state: ConnectionState,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub diagnostics: String,
    pub failure_kind: Option<FailureKind>,
    pub retry_attempt: u8,
}

struct ManagedConnection {
    config: ConnectionConfig,
    process: Option<SessionProcess>,
    status: ConnectionStatus,
    started_at: Option<Instant>,
    exited: Option<(ExitStatus, Instant)>,
    forced_failure: Option<FailureKind>,
    stop_requested: bool,
    ever_ready: bool,
    retry_at: Option<Instant>,
}

impl ManagedConnection {
    fn new(config: ConnectionConfig) -> Self {
        Self {
            status: ConnectionStatus {
                name: config.name.clone(),
                state: ConnectionState::Stopped,
                pid: None,
                last_error: None,
                diagnostics: String::new(),
                failure_kind: None,
                retry_attempt: 0,
            },
            config,
            process: None,
            started_at: None,
            exited: None,
            forced_failure: None,
            stop_requested: false,
            ever_ready: false,
            retry_at: None,
        }
    }

    fn start_attempt(&mut self, now: Instant) -> Result<()> {
        self.retry_at = None;
        self.forced_failure = None;
        self.stop_requested = false;
        self.exited = None;
        self.status.diagnostics.clear();
        self.status.last_error = None;
        self.status.failure_kind = None;
        let process = match ssm::spawn_session(&self.config) {
            Ok(process) => process,
            Err(error) => {
                let kind = if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    FailureKind::MissingDependency
                } else {
                    classify_failure(&format!("{error:#}"))
                };
                self.status.diagnostics =
                    ssm::sanitize_diagnostic(&format!("Starting AWS CLI: {error:#}"));
                self.finish_failure(kind, now);
                return Err(error);
            }
        };
        self.attach(process, now);
        Ok(())
    }

    fn attach(&mut self, process: SessionProcess, now: Instant) {
        self.status.pid = process.child.id();
        self.status.state = ConnectionState::Connecting;
        self.started_at = Some(now);
        self.process = Some(process);
    }

    fn record_failure(&mut self, kind: FailureKind) {
        self.status.failure_kind = Some(kind);
        self.status.last_error = Some(format!("{}. {}", kind.summary(), kind.suggestion()));
    }

    fn terminate_with_failure(&mut self, kind: FailureKind, detail: &str) {
        self.record_failure(kind);
        self.forced_failure = Some(kind);
        if !detail.is_empty() {
            self.status.diagnostics.push_str(detail);
            self.status.diagnostics.push('\n');
        }
        if let Err(error) = stop_process_group(self.status.pid) {
            self.status
                .diagnostics
                .push_str(&format!("Could not stop session: {error}\n"));
        }
        self.status.state = ConnectionState::Stopping;
    }

    fn finish_failure(&mut self, kind: FailureKind, now: Instant) {
        self.record_failure(kind);
        self.status.state = if self.ever_ready {
            ConnectionState::Disconnected
        } else {
            ConnectionState::Failed
        };
        let retryable =
            kind == FailureKind::Network || (kind == FailureKind::Timeout && self.ever_ready);
        if self.config.auto_reconnect
            && retryable
            && self.status.retry_attempt < MAX_RETRIES
            && !self.stop_requested
        {
            self.status.retry_attempt += 1;
            self.retry_at =
                Some(now + Duration::from_secs(2u64.pow(u32::from(self.status.retry_attempt))));
            self.status.state = ConnectionState::Reconnecting;
        }
    }

    fn poll(&mut self, now: Instant, woke: bool) {
        if self.retry_at.is_some_and(|retry_at| now >= retry_at) {
            let _ = self.start_attempt(now);
        }
        let Some(process) = &mut self.process else {
            return;
        };
        let output = process.output();
        if self.forced_failure.is_none() {
            self.status.diagnostics = output.diagnostics.clone();
        }
        if self.exited.is_none() {
            match process.child.try_wait() {
                Ok(Some(status)) => {
                    let _ = stop_process_group(self.status.pid);
                    self.exited = Some((status, now));
                    self.status.state = ConnectionState::Stopping;
                }
                Ok(None) => {}
                Err(error) => {
                    self.terminate_with_failure(
                        FailureKind::Unknown,
                        &format!("Checking AWS process: {error}"),
                    );
                    return;
                }
            }
        }
        if let Some((exit, observed_at)) = self.exited {
            if !process.output_finished() && now.duration_since(observed_at) < OUTPUT_DRAIN_TIMEOUT
            {
                return;
            }
            self.process = None;
            self.status.pid = None;
            self.started_at = None;
            self.exited = None;
            if self.stop_requested {
                self.status.state = ConnectionState::Stopped;
                self.status.last_error = None;
                self.status.failure_kind = None;
            } else {
                let kind = self
                    .forced_failure
                    .take()
                    .unwrap_or_else(|| classify_failure(&self.status.diagnostics));
                self.status
                    .diagnostics
                    .push_str(&format!("AWS session exited: {exit}\n"));
                if !self.ever_ready && output.ready {
                    // The process exited before a poll observed a usable live tunnel.
                    self.status
                        .diagnostics
                        .push_str("Session ended before tunnel readiness could be confirmed.\n");
                }
                self.finish_failure(kind, now);
            }
            return;
        }
        if self.status.state == ConnectionState::Stopping {
            return;
        }
        if woke {
            self.terminate_with_failure(
                FailureKind::Network,
                "Mac resumed from sleep; reconnect to establish a fresh tunnel.",
            );
        } else if output
            .diagnostics
            .to_ascii_lowercase()
            .contains("connection to destination port failed")
        {
            self.terminate_with_failure(
                FailureKind::TargetUnavailable,
                "The plugin reported that the destination port is unreachable.",
            );
        } else if self.status.state == ConnectionState::Connecting {
            if output.ready {
                self.status.state = ConnectionState::Running;
                self.ever_ready = true;
                self.status.last_error = None;
                self.status.failure_kind = None;
            } else if self
                .started_at
                .is_some_and(|started| now.duration_since(started) >= CONNECT_TIMEOUT)
            {
                self.terminate_with_failure(
                    FailureKind::Timeout,
                    "No matching port-open confirmation was received from this session.",
                );
            }
        }
    }
}

pub struct SessionManager {
    connections: BTreeMap<String, ManagedConnection>,
    sleep_time: Option<Duration>,
}

impl SessionManager {
    pub fn new(config: Config) -> Self {
        Self {
            connections: config
                .connections
                .into_iter()
                .map(|config| (config.name.clone(), ManagedConnection::new(config)))
                .collect(),
            sleep_time: elapsed_sleep(),
        }
    }

    pub fn statuses(&self) -> Vec<ConnectionStatus> {
        self.connections
            .values()
            .map(|connection| connection.status.clone())
            .collect()
    }

    pub fn reload_config(&mut self, config: Config) -> Result<()> {
        self.validate_reload(&config)?;
        let mut connections = BTreeMap::new();
        for config in config.connections {
            let mut managed = self
                .connections
                .remove(&config.name)
                .unwrap_or_else(|| ManagedConnection::new(config.clone()));
            managed.config = config;
            connections.insert(managed.config.name.clone(), managed);
        }
        self.connections = connections;
        Ok(())
    }

    pub fn validate_reload(&self, config: &Config) -> Result<()> {
        config.validate()?;
        for (name, managed) in &self.connections {
            if managed.status.state.is_active() {
                let unchanged = config
                    .connections
                    .iter()
                    .find(|item| item.name == *name)
                    .is_some_and(|item| {
                        let mut current = managed.config.clone();
                        current.pinned = item.pinned;
                        current == *item
                    });
                if !unchanged {
                    anyhow::bail!("stop session {name} before editing or deleting it");
                }
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
        connection.status.retry_attempt = 0;
        connection.ever_ready = false;
        connection
            .start_attempt(Instant::now())
            .with_context(|| format!("starting AWS session for {name}"))
    }

    pub fn stop(&mut self, name: &str) -> Result<()> {
        let connection = self
            .connections
            .get_mut(name)
            .with_context(|| format!("unknown connection: {name}"))?;
        if !connection.status.state.is_active() {
            anyhow::bail!("connection {name} is not running");
        }
        connection.retry_at = None;
        connection.stop_requested = true;
        connection.forced_failure = None;
        connection.status.last_error = None;
        connection.status.failure_kind = None;
        if connection.process.is_some() {
            stop_process_group(connection.status.pid).context("stopping AWS session")?;
            connection.status.state = ConnectionState::Stopping;
        } else {
            connection.status.state = ConnectionState::Stopped;
        }
        Ok(())
    }

    pub fn poll(&mut self) {
        let sleep_time = elapsed_sleep();
        let woke = match (self.sleep_time, sleep_time) {
            (Some(previous), Some(current)) => {
                current.saturating_sub(previous) > Duration::from_secs(2)
            }
            _ => false,
        };
        self.sleep_time = sleep_time;
        let now = Instant::now();
        for connection in self.connections.values_mut() {
            connection.poll(now, woke);
        }
    }

    pub async fn shutdown(&mut self) {
        for connection in self.connections.values_mut() {
            connection.retry_at = None;
            connection.stop_requested = true;
            let _ = stop_process_group(connection.status.pid);
        }
        for connection in self.connections.values_mut() {
            if let Some(mut process) = connection.process.take() {
                let _ = timeout(Duration::from_secs(2), process.child.wait()).await;
            }
            connection.status.pid = None;
            connection.status.state = ConnectionState::Stopped;
        }
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        for connection in self.connections.values() {
            let _ = stop_process_group(connection.status.pid);
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

fn elapsed_sleep() -> Option<Duration> {
    fn clock(id: libc::clockid_t) -> Option<Duration> {
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(id, &mut value) } != 0 {
            return None;
        }
        Some(Duration::new(
            value.tv_sec.try_into().ok()?,
            value.tv_nsec.try_into().ok()?,
        ))
    }
    // Unlike a delayed UI poll, only sleep increases the gap between these two macOS clocks.
    Some(clock(libc::CLOCK_MONOTONIC_RAW)?.saturating_sub(clock(libc::CLOCK_UPTIME_RAW)?))
}

fn classify_failure(text: &str) -> FailureKind {
    let text = text.to_ascii_lowercase();
    if text.contains("sso")
        && ["expired", "invalid", "token", "login"]
            .iter()
            .any(|word| text.contains(word))
    {
        FailureKind::SsoExpired
    } else if [
        "expiredtoken",
        "invalidclienttokenid",
        "unable to locate credentials",
        "credential",
        "security token included in the request is invalid",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        FailureKind::Credentials
    } else if ["accessdenied", "not authorized", "unauthorizedoperation"]
        .iter()
        .any(|word| text.contains(word))
    {
        FailureKind::Permission
    } else if [
        "address already in use",
        "only one usage of each socket",
        "bind: address",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        FailureKind::PortInUse
    } else if text.contains("session-manager-plugin")
        && ["not found", "no such file", "not installed"]
            .iter()
            .any(|word| text.contains(word))
    {
        FailureKind::MissingDependency
    } else if [
        "targetnotconnected",
        "connection to destination port failed",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        FailureKind::TargetUnavailable
    } else if [
        "could not connect to the endpoint",
        "network is unreachable",
        "network unreachable",
        "connection reset",
        "broken pipe",
        "connection timed out",
        "i/o timeout",
        "websocket: close",
        "temporary failure in name resolution",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        FailureKind::Network
    } else {
        FailureKind::Unknown
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
                parameters: BTreeMap::from([("localPortNumber".into(), "15432".into())]),
                extra_args: vec![],
                auto_reconnect: false,
                pinned: false,
            }],
        }
    }

    fn attach_child(manager: &mut SessionManager, script: &str) -> u32 {
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", script])
            .process_group(0)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        manager
            .connections
            .get_mut("test")
            .unwrap()
            .attach(SessionProcess::capture(child, Some(15432)), Instant::now());
        pid
    }

    async fn wait_for(manager: &mut SessionManager, predicate: impl Fn(&ConnectionStatus) -> bool) {
        timeout(Duration::from_secs(3), async {
            loop {
                manager.poll();
                if predicate(&manager.statuses()[0]) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    const READY: &str = "printf 'Starting session with SessionId: test-session\\nPort 15432 opened for sessionId test-session.\\n'; sleep 60 & wait";

    #[tokio::test]
    async fn readiness_cancellation_and_process_group_cleanup() {
        let mut manager = SessionManager::new(config());
        let pid = attach_child(&mut manager, READY);
        assert_eq!(manager.statuses()[0].state, ConnectionState::Connecting);
        wait_for(&mut manager, |status| {
            status.state == ConnectionState::Running
        })
        .await;
        let mut pinned = config();
        pinned.connections[0].pinned = true;
        manager.reload_config(pinned).unwrap();
        assert_eq!(manager.statuses()[0].pid, Some(pid));
        let mut changed = config();
        changed.connections[0].target = "i-other".into();
        assert!(manager.validate_reload(&changed).is_err());
        assert!(manager.reload_config(Config::default()).is_err());
        manager.stop("test").unwrap();
        wait_for(&mut manager, |status| status.pid.is_none()).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Stopped);
        assert!(manager.statuses()[0].last_error.is_none());
        timeout(Duration::from_secs(3), async {
            while unsafe { libc::kill(-(pid as i32), 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        manager.reload_config(changed).unwrap();
        manager.reload_config(Config::default()).unwrap();
        assert!(manager.statuses().is_empty());
    }

    #[tokio::test]
    async fn connecting_can_be_cancelled_and_occupied_port_is_not_readiness() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = config();
        config.connections[0].parameters.insert(
            "localPortNumber".into(),
            listener.local_addr().unwrap().port().to_string(),
        );
        let mut manager = SessionManager::new(config);
        attach_child(&mut manager, "sleep 60 & wait");
        tokio::time::sleep(Duration::from_millis(20)).await;
        manager.poll();
        assert_eq!(manager.statuses()[0].state, ConnectionState::Connecting);
        manager.stop("test").unwrap();
        wait_for(&mut manager, |status| status.pid.is_none()).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Stopped);
        drop(listener);
    }

    #[tokio::test]
    async fn timeout_fails_closed_and_releases_the_process() {
        let mut manager = SessionManager::new(config());
        attach_child(&mut manager, "sleep 60 & wait");
        manager.connections.get_mut("test").unwrap().started_at =
            Some(Instant::now() - CONNECT_TIMEOUT);
        manager.poll();
        wait_for(&mut manager, |status| status.pid.is_none()).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Failed);
        assert_eq!(
            manager.statuses()[0].failure_kind,
            Some(FailureKind::Timeout)
        );
    }

    #[tokio::test]
    async fn captures_final_error_and_sanitizes_secrets_before_exposing_diagnostics() {
        let mut manager = SessionManager::new(config());
        attach_child(&mut manager, "printf 'Authorization: Bearer SECRET\\n' >&2; printf 'An error occurred (AccessDeniedException): not authorized\\n' >&2; exit 42");
        wait_for(&mut manager, |status| status.pid.is_none()).await;
        let status = &manager.statuses()[0];
        assert_eq!(status.state, ConnectionState::Failed);
        assert_eq!(status.failure_kind, Some(FailureKind::Permission));
        assert!(status.diagnostics.contains("AccessDeniedException"));
        assert!(status.diagnostics.contains("42"));
        assert!(!status.diagnostics.contains("SECRET"));
    }

    #[tokio::test]
    async fn clean_unrequested_exit_after_readiness_is_disconnected() {
        let mut manager = SessionManager::new(config());
        attach_child(&mut manager, "printf 'Starting session with SessionId: test-session\\nPort 15432 opened for sessionId test-session.\\n'; sleep 0.1; exit 0");
        wait_for(&mut manager, |status| {
            status.state == ConnectionState::Running
        })
        .await;
        wait_for(&mut manager, |status| status.pid.is_none()).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn retries_are_bounded_across_successes_and_manual_stop_cancels_wait() {
        let mut config = config();
        config.connections[0].auto_reconnect = true;
        let mut manager = SessionManager::new(config);
        for attempt in 1..=MAX_RETRIES + 1 {
            let connection = manager.connections.get_mut("test").unwrap();
            connection.retry_at = None;
            connection.exited = None;
            attach_child(&mut manager, "printf 'Starting session with SessionId: test-session\\nPort 15432 opened for sessionId test-session.\\n'; sleep 0.1; printf 'connection reset by peer\\n' >&2; exit 1");
            wait_for(&mut manager, |status| {
                status.state == ConnectionState::Running
            })
            .await;
            wait_for(&mut manager, |status| status.pid.is_none()).await;
            assert_eq!(
                manager.statuses()[0].retry_attempt,
                attempt.min(MAX_RETRIES)
            );
            assert_eq!(
                manager.statuses()[0].state,
                if attempt <= MAX_RETRIES {
                    ConnectionState::Reconnecting
                } else {
                    ConnectionState::Disconnected
                }
            );
        }
        let connection = manager.connections.get_mut("test").unwrap();
        connection.status.retry_attempt = 0;
        connection.finish_failure(FailureKind::Network, Instant::now());
        manager.stop("test").unwrap();
        assert!(manager.connections["test"].retry_at.is_none());
        assert_eq!(manager.statuses()[0].state, ConnectionState::Stopped);
        let connection = manager.connections.get_mut("test").unwrap();
        connection.stop_requested = false;
        connection.finish_failure(FailureKind::SsoExpired, Instant::now());
        assert!(connection.retry_at.is_none());
    }

    #[tokio::test]
    async fn wake_invalidates_ready_tunnel_and_shutdown_cleans_children() {
        let mut manager = SessionManager::new(config());
        let pid = attach_child(&mut manager, READY);
        wait_for(&mut manager, |status| {
            status.state == ConnectionState::Running
        })
        .await;
        manager
            .connections
            .get_mut("test")
            .unwrap()
            .poll(Instant::now(), true);
        wait_for(&mut manager, |status| status.pid.is_none()).await;
        assert_eq!(manager.statuses()[0].state, ConnectionState::Disconnected);
        assert_eq!(
            manager.statuses()[0].failure_kind,
            Some(FailureKind::Network)
        );
        assert_eq!(unsafe { libc::kill(-(pid as i32), 0) }, -1);
        let pid = attach_child(&mut manager, READY);
        manager.shutdown().await;
        assert_eq!(unsafe { libc::kill(-(pid as i32), 0) }, -1);
        assert!(manager.connections["test"].process.is_none());
    }

    #[test]
    fn unsupported_shell_records_failure_without_spawning() {
        let mut config = config();
        config.connections[0].document_name = None;
        let mut manager = SessionManager::new(config);
        assert!(manager.start("test").is_err());
        assert_eq!(manager.statuses()[0].state, ConnectionState::Failed);
        assert!(manager.statuses()[0].pid.is_none());
        assert!(manager.statuses()[0].diagnostics.contains("terminal"));
    }

    #[test]
    fn common_failures_have_specific_recovery_and_unknown_errors_stay_unknown() {
        for (text, kind) in [
            (
                "Error when retrieving token from sso: Token has expired",
                FailureKind::SsoExpired,
            ),
            ("Unable to locate credentials", FailureKind::Credentials),
            ("AccessDeniedException", FailureKind::Permission),
            ("TargetNotConnected", FailureKind::TargetUnavailable),
            ("listen tcp: address already in use", FailureKind::PortInUse),
            (
                "Session-manager-plugin not found",
                FailureKind::MissingDependency,
            ),
            (
                "Could not connect to the endpoint URL",
                FailureKind::Network,
            ),
            ("unexpected exit", FailureKind::Unknown),
        ] {
            assert_eq!(classify_failure(text), kind);
        }
    }
}
