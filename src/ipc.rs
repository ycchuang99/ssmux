use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    Ping,
    Start { name: String },
    Stop { name: String },
    StopAll,
    Reload,
    TuiExit,
    Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    #[serde(default)]
    pub statuses: Vec<ConnectionStatus>,
}

impl Response {
    pub fn ok(message: impl Into<String>, statuses: Vec<ConnectionStatus>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            statuses,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            statuses: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConnectionStatus {
    pub name: String,
    pub target: String,
    pub keep_on_exit: bool,
    pub state: ConnectionState,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub last_error: Option<String>,
}

pub async fn send_request(socket_path: &Path, request: Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket_path).await.with_context(|| {
        format!(
            "connecting to daemon at {}; is `ssmux daemon` running?",
            socket_path.display()
        )
    })?;
    let payload = serde_json::to_string(&request)?;
    stream.write_all(payload.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.trim().is_empty() {
        anyhow::bail!("daemon returned an empty response");
    }
    serde_json::from_str(line.trim()).context("decoding daemon response")
}
