use std::{
    collections::BTreeMap,
    process::Stdio,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    task::JoinHandle,
};

use crate::config::ConnectionConfig;

const OUTPUT_LIMIT: usize = 8 * 1024;

#[derive(Clone, Default)]
pub struct SessionOutput {
    pub diagnostics: String,
    pub ready: bool,
    session_id: Option<String>,
}

pub struct SessionProcess {
    pub child: Child,
    output: Arc<Mutex<SessionOutput>>,
    readers: Vec<JoinHandle<()>>,
}

impl SessionProcess {
    pub fn capture(mut child: Child, expected_port: Option<u16>) -> Self {
        let output = Arc::new(Mutex::new(SessionOutput::default()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(tokio::spawn(capture_output(
                stdout,
                output.clone(),
                expected_port,
                true,
            )));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(tokio::spawn(capture_output(
                stderr,
                output.clone(),
                expected_port,
                false,
            )));
        }
        Self {
            child,
            output,
            readers,
        }
    }

    pub fn output(&self) -> SessionOutput {
        self.output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn output_finished(&self) -> bool {
        self.readers.iter().all(JoinHandle::is_finished)
    }
}

impl Drop for SessionProcess {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

pub fn spawn_session(config: &ConnectionConfig) -> Result<SessionProcess> {
    if is_interactive(config) {
        anyhow::bail!("interactive shells require a terminal; choose a port-forwarding document");
    }
    let expected_port = config
        .parameters
        .get("localPortNumber")
        .map(|port| port.parse::<u16>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("localPortNumber must be a port from 1 to 65535"))?;
    if expected_port == Some(0) {
        anyhow::bail!("localPortNumber must be a port from 1 to 65535");
    }
    let mut command = command_for(config);
    let child = command
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    Ok(SessionProcess::capture(child, expected_port))
}

async fn capture_output(
    mut reader: impl AsyncRead + Unpin,
    output: Arc<Mutex<SessionOutput>>,
    expected_port: Option<u16>,
    stdout: bool,
) {
    let mut bytes = [0; 1024];
    let mut line = Vec::new();
    let mut oversized = false;
    let mut private_key = false;
    loop {
        match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                for byte in &bytes[..count] {
                    if *byte == b'\n' || *byte == b'\r' {
                        record_line(
                            &output,
                            &line,
                            oversized,
                            expected_port,
                            stdout,
                            &mut private_key,
                        );
                        line.clear();
                        oversized = false;
                    } else if line.len() < OUTPUT_LIMIT {
                        line.push(*byte);
                    } else {
                        oversized = true;
                    }
                }
            }
        }
    }
    record_line(
        &output,
        &line,
        oversized,
        expected_port,
        stdout,
        &mut private_key,
    );
}

fn record_line(
    output: &Mutex<SessionOutput>,
    bytes: &[u8],
    oversized: bool,
    expected_port: Option<u16>,
    stdout: bool,
    private_key: &mut bool,
) {
    if bytes.is_empty() && !oversized {
        return;
    }
    let line = String::from_utf8_lossy(bytes);
    let line = line.trim();
    let mut output = output.lock().unwrap_or_else(|error| error.into_inner());
    if stdout && !oversized && !*private_key {
        if let Some(id) = line.strip_prefix("Starting session with SessionId: ") {
            output.session_id = Some(id.trim().to_owned());
        }
        // shortcut: AWS plugin has no readiness API; fail closed if its port-open message changes.
        if let Some((port, id)) = line
            .strip_prefix("Port ")
            .and_then(|line| line.split_once(" opened for sessionId "))
        {
            if let Ok(port) = port.parse::<u16>() {
                output.ready |= port != 0
                    && expected_port.is_none_or(|expected| expected == port)
                    && output.session_id.as_deref() == Some(id.trim_end_matches('.'));
            }
        }
    }
    let begins_key = line.contains("-----BEGIN") && line.contains("PRIVATE KEY");
    let ends_key = line.contains("-----END") && line.contains("PRIVATE KEY");
    let sanitized = if oversized || begins_key || *private_key {
        "[Sensitive or oversized diagnostic line omitted]".to_owned()
    } else {
        sanitize_diagnostic(line)
    };
    *private_key = (begins_key || *private_key) && !ends_key;
    output.diagnostics.push_str(&sanitized);
    output.diagnostics.push('\n');
    if output.diagnostics.len() > OUTPUT_LIMIT {
        let mut cut = output.diagnostics.len() - OUTPUT_LIMIT;
        while !output.diagnostics.is_char_boundary(cut) {
            cut += 1;
        }
        output.diagnostics.drain(..cut);
    }
}

pub fn sanitize_diagnostic(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    if [
        "authorization",
        "x-amz-security-token",
        "x-amz-credential",
        "x-amz-signature",
        "aws_access_key_id",
        "aws_secret_access_key",
        "aws_session_token",
        "accesskeyid",
        "secretaccesskey",
        "sessiontoken",
        "tokenvalue",
        "access_token",
        "refresh_token",
        "clientsecret",
        "client_secret",
        "password",
        "bearer ",
        "token=",
        "token:",
        "token\":",
        "token':",
        "private key",
    ]
    .iter()
    .any(|key| lower.contains(key))
        || line
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|word| word.len() >= 16 && (word.starts_with("AKIA") || word.starts_with("ASIA")))
    {
        return "[Sensitive diagnostic line omitted]".to_owned();
    }
    line.split_whitespace()
        .map(|word| {
            if (word.contains("://") && (word.contains('?') || word.contains('@')))
                || (word.len() >= 40
                    && word
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || "+/=_-".contains(ch)))
            {
                "[Sensitive value omitted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn is_interactive(config: &ConnectionConfig) -> bool {
    matches!(
        config.document_name.as_deref(),
        None | Some(
            "AWS-StartInteractiveCommand" | "SSM-SessionManagerRunShell" | "AWS-StartSSHSession"
        )
    )
}

fn command_for(config: &ConnectionConfig) -> Command {
    let mut command = Command::new("aws");
    // Finder launches do not inherit the shell's Homebrew paths.
    let mut paths =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    for directory in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/local/sessionmanagerplugin/bin",
        "/usr/bin",
        "/bin",
    ] {
        let path = std::path::PathBuf::from(directory);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
    command.env("AWS_PAGER", "");
    command.env("AWS_CLI_AUTO_PROMPT", "off");
    if let Some(profile) = &config.profile {
        command.arg("--profile").arg(profile);
    }
    if let Some(region) = &config.region {
        command.arg("--region").arg(region);
    }

    command
        .arg("ssm")
        .arg("start-session")
        .arg("--target")
        .arg(&config.target);
    if let Some(document_name) = &config.document_name {
        command.arg("--document-name").arg(document_name);
    }
    if let Some(parameters) = parameters_argument(&config.parameters) {
        command.arg("--parameters").arg(parameters);
    }
    command.args(&config.extra_args);
    command
}

fn parameters_argument(parameters: &BTreeMap<String, String>) -> Option<String> {
    if parameters.is_empty() {
        return None;
    }

    Some(
        serde_json::to_string(
            &parameters
                .iter()
                .map(|(key, value)| (key, [value]))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("string parameters are JSON serializable"),
    )
}

#[cfg(test)]
mod tests {
    use super::parameters_argument;
    use crate::config::ConnectionConfig;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn readiness_matches_this_sessions_stdout_and_expected_port() {
        async fn capture(script: &str) -> super::SessionOutput {
            let child = tokio::process::Command::new("/bin/sh")
                .args(["-c", script])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut process = super::SessionProcess::capture(child, Some(15432));
            process.child.wait().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !process.output_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            process.output()
        }
        for script in [
            "printf 'Port 15432 opened for sessionId current.\\n'",
            "printf 'Starting session with SessionId: current\\nPort 15432 opened for sessionId stale.\\n'",
            "printf 'Starting session with SessionId: current\\nPort 11111 opened for sessionId current.\\n'",
            "printf 'Starting session with SessionId: current\\nPort 15432 opened for sessionId current.\\n' >&2",
        ] { assert!(!capture(script).await.ready, "{script}"); }
        assert!(capture("printf 'Starting session with SessionId: current\\nPort 15432 opened for sessionId current.\\n'").await.ready);
    }

    #[tokio::test]
    async fn capture_bounds_long_lines_and_redacts_multiline_private_keys() {
        let output = std::sync::Arc::new(std::sync::Mutex::new(super::SessionOutput::default()));
        let text = format!("{}\n-----BEGIN PRIVATE KEY-----\nPRIVATE-CONTENT\n-----END PRIVATE KEY-----\nFinal failure\n", "S".repeat(100_000));
        super::capture_output(text.as_bytes(), output.clone(), None, true).await;
        let output = output.lock().unwrap();
        assert!(output.diagnostics.len() <= super::OUTPUT_LIMIT);
        assert!(!output.diagnostics.contains("PRIVATE-CONTENT"));
        assert!(!output.diagnostics.contains("SSSS"));
        assert!(output.diagnostics.contains("Final failure"));
    }

    #[test]
    fn sanitization_preserves_recovery_errors_but_not_credentials_or_auth_urls() {
        for text in [
            "Authorization: Bearer sensitive",
            "{\"TokenValue\": \"sensitive\"}",
            "AWS_SECRET_ACCESS_KEY=sensitive",
            "Password: sensitive",
            "AKIAIOSFODNN7EXAMPLE",
            "https://example.test/authorize?token=sensitive",
            "https://user:sensitive@example.test/",
        ] {
            let sanitized = super::sanitize_diagnostic(text);
            assert!(!sanitized.contains("sensitive"), "{sanitized}");
            assert!(!sanitized.contains("AKIA"));
        }
        let expired = "Error when retrieving token from sso: Token has expired";
        assert_eq!(super::sanitize_diagnostic(expired), expired);
    }

    #[test]
    fn config_requires_expected_ssm_arguments() {
        let config = ConnectionConfig {
            name: "demo".to_owned(),
            target: "i-123".to_owned(),
            region: Some("us-west-2".to_owned()),
            profile: Some("staging".to_owned()),
            document_name: Some("AWS-StartPortForwardingSession".to_owned()),
            parameters: BTreeMap::from([
                ("localPortNumber".to_owned(), "10022".to_owned()),
                ("portNumber".to_owned(), "22".to_owned()),
            ]),
            extra_args: Vec::new(),
            auto_reconnect: false,
            pinned: false,
        };

        assert_eq!(
            parameters_argument(&config.parameters).as_deref(),
            Some(r#"{"localPortNumber":["10022"],"portNumber":["22"]}"#)
        );
    }
    #[test]
    fn command_preserves_special_characters_and_finder_dependency_paths() {
        let config = ConnectionConfig {
            name: "quoted".into(),
            target: "i-123".into(),
            region: Some("us-west-2".into()),
            profile: Some("work profile".into()),
            document_name: Some("custom-document".into()),
            parameters: BTreeMap::from([("value".into(), "comma, quote\" and space".into())]),
            extra_args: vec!["--endpoint-url".into(), "https://example.test".into()],
            auto_reconnect: false,
            pinned: false,
        };
        let command = super::command_for(&config);
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            &args[..4],
            &["--profile", "work profile", "--region", "us-west-2"]
        );
        let parameters = args[args.iter().position(|arg| *arg == "--parameters").unwrap() + 1];
        let json: serde_json::Value = serde_json::from_str(parameters).unwrap();
        assert_eq!(json["value"][0], config.parameters["value"]);
        assert_eq!(
            &args[args.len() - 2..],
            &["--endpoint-url", "https://example.test"]
        );
        let path = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == "PATH")
            .unwrap()
            .1
            .unwrap();
        let paths = std::env::split_paths(path).collect::<Vec<_>>();
        assert!(paths.contains(&std::path::PathBuf::from("/opt/homebrew/bin")));
        assert!(paths.contains(&std::path::PathBuf::from("/usr/local/bin")));
    }
}
