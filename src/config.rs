use std::{collections::BTreeMap, path::Path, path::PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ConnectionConfig {
    pub name: String,
    pub target: String,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub document_name: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub keep_on_exit: bool,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("parsing configuration {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating configuration directory {}", parent.display())
            })?;
        }
        let temporary_path = path.with_extension("toml.tmp");
        let source = toml::to_string_pretty(self).context("serializing configuration")?;
        std::fs::write(&temporary_path, source)
            .with_context(|| format!("writing {}", temporary_path.display()))?;
        std::fs::rename(&temporary_path, path)
            .with_context(|| format!("replacing configuration {}", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let mut names = std::collections::BTreeSet::new();
        for connection in &self.connections {
            if connection.name.trim().is_empty() {
                anyhow::bail!("connection names cannot be empty");
            }
            if connection.target.trim().is_empty() {
                anyhow::bail!("connection {} has an empty target", connection.name);
            }
            if !names.insert(&connection.name) {
                anyhow::bail!("duplicate connection name: {}", connection.name);
            }
            if connection
                .parameters
                .iter()
                .any(|(key, value)| key.trim().is_empty() || value.trim().is_empty())
            {
                anyhow::bail!(
                    "connection {} has an empty parameter name or value",
                    connection.name
                );
            }
        }
        Ok(())
    }
}

pub fn default_config_path() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.config_dir().join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

pub fn default_socket_path() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.data_local_dir().join("ssmux.sock"))
        .unwrap_or_else(|| PathBuf::from("ssmux.sock"))
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "ssmux")
}

pub fn example_config() -> &'static str {
    r#"# ssmux connection configuration
# Run `ssmux daemon` in another terminal after editing this file.

[[connections]]
name = "demo-stage"
target = "i-0deadbeefdeadbeef0"
region = "us-west-2"
# profile = "example-profile"
document_name = "AWS-StartPortForwardingSession"
keep_on_exit = true

[connections.parameters]
portNumber = "22"
localPortNumber = "10022"

# For remote-host forwarding, use this document and add a host parameter:
# document_name = "AWS-StartPortForwardingSessionToRemoteHost"
# [connections.parameters]
# host = "db.example.test"
# portNumber = "5432"
# localPortNumber = "15432"
"#
}
