use std::{collections::BTreeMap, path::Path, path::PathBuf};

use anyhow::{Context, Result};
use directories::BaseDirs;
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
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".ssmux").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

pub fn example_config() -> &'static str {
    r#"# ssmux connection configuration
# Run `ssmux` after editing this file to open the TUI.
# Add connections from the TUI with `n`, or use `ssmux config add`.
"#
}

pub fn parse_parameters(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut parameters = BTreeMap::new();
    for value in values {
        let (key, parameter) = value
            .split_once('=')
            .with_context(|| format!("parameter must use KEY=VALUE: {value}"))?;
        if key.trim().is_empty() || parameter.trim().is_empty() {
            anyhow::bail!("parameter must have a non-empty key and value: {value}");
        }
        if parameters
            .insert(key.to_owned(), parameter.to_owned())
            .is_some()
        {
            anyhow::bail!("duplicate parameter: {key}");
        }
    }
    Ok(parameters)
}
