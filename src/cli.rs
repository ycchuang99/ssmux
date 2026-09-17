use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::ConnectionConfig;

#[derive(Debug, Parser)]
#[command(
    name = "ssmux",
    version,
    about = "A terminal manager for AWS SSM sessions"
)]
pub struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the terminal user interface.
    Tui,
    /// Run one configured session in the foreground; Ctrl-C stops it.
    Start { name: String },
    /// List configured connections. Live state belongs to the current TUI.
    Status {
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check local dependencies and configuration.
    Doctor,
    /// Create an empty configuration file.
    Init,
    /// Create, inspect, edit, or remove local connection definitions.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// List configured connections.
    List,
    /// Add a connection definition.
    Add {
        name: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        region: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        document_name: Option<String>,
        /// Repeated key=value session parameters.
        #[arg(long = "parameter", value_name = "KEY=VALUE")]
        parameters: Vec<String>,
    },
    /// Replace editable fields on an existing connection.
    Edit {
        name: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        region: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        document_name: Option<String>,
        /// Repeated key=value session parameters; supplying one replaces all parameters.
        #[arg(long = "parameter", value_name = "KEY=VALUE")]
        parameters: Vec<String>,
        /// Remove all existing session parameters.
        #[arg(long, conflicts_with = "parameters")]
        clear_parameters: bool,
    },
    /// Remove a connection definition.
    Remove { name: String },
}

pub fn print_configurations(configurations: &[ConnectionConfig]) {
    if configurations.is_empty() {
        println!("no connections configured");
        return;
    }

    println!("{:<18} {:<24} {:<18}", "NAME", "TARGET", "REGION");
    println!("{}", "─".repeat(64));
    for configuration in configurations {
        println!(
            "{:<18} {:<24} {:<18}",
            configuration.name,
            configuration.target,
            configuration.region.as_deref().unwrap_or("-")
        );
    }
}
