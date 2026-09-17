use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{
    config::ConnectionConfig,
    ipc::{ConnectionState, ConnectionStatus},
};

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

    /// Path to the daemon Unix socket.
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the terminal user interface.
    Tui,
    /// Run the background connection manager in the foreground.
    Daemon,
    /// Start one configured connection.
    Start { name: String },
    /// Stop one active connection.
    Stop {
        /// Connection name. Omit this when using --all.
        #[arg(required_unless_present = "all")]
        name: Option<String>,
        /// Stop every connection managed by ssmux.
        #[arg(long, conflicts_with = "name")]
        all: bool,
    },
    /// Show the state of all configured connections.
    Status {
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check local dependencies, configuration, and daemon availability.
    Doctor,
    /// Create an example configuration file.
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
        /// Keep this connection running when the TUI exits.
        #[arg(long)]
        keep_on_exit: bool,
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
        #[arg(long, conflicts_with = "temporary")]
        keep_on_exit: bool,
        #[arg(long, conflicts_with = "keep_on_exit")]
        temporary: bool,
        /// Repeated key=value session parameters; supplying one replaces all parameters.
        #[arg(long = "parameter", value_name = "KEY=VALUE")]
        parameters: Vec<String>,
        /// Remove all existing session parameters.
        #[arg(long, conflicts_with = "parameters")]
        clear_parameters: bool,
    },
    /// Remove a connection definition. Stop it first if it is active.
    Remove { name: String },
}

pub fn print_statuses(statuses: &[ConnectionStatus]) {
    if statuses.is_empty() {
        println!("no connections configured");
        return;
    }

    println!(
        "{:<18} {:<10} {:<24} {:<8} {:<6}",
        "NAME", "STATE", "TARGET", "PID", "MODE"
    );
    println!("{}", "─".repeat(74));
    for status in statuses {
        let pid = status
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{:<18} {:<10} {:<24} {:<8} {:<12}",
            status.name,
            state_label(status.state),
            status.target,
            pid,
            if status.keep_on_exit {
                "background"
            } else {
                "temporary"
            }
        );
        if let Some(error) = &status.last_error {
            println!("  error: {error}");
        }
    }
}

pub fn print_configurations(configurations: &[ConnectionConfig]) {
    if configurations.is_empty() {
        println!("no connections configured");
        return;
    }

    println!(
        "{:<18} {:<10} {:<24} {:<18}",
        "NAME", "MODE", "TARGET", "REGION"
    );
    println!("{}", "─".repeat(74));
    for configuration in configurations {
        println!(
            "{:<18} {:<10} {:<24} {:<18}",
            configuration.name,
            if configuration.keep_on_exit {
                "background"
            } else {
                "temporary"
            },
            configuration.target,
            configuration.region.as_deref().unwrap_or("-")
        );
    }
}

fn state_label(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Starting => "starting",
        ConnectionState::Running => "running",
        ConnectionState::Stopping => "stopping",
        ConnectionState::Stopped => "stopped",
        ConnectionState::Failed => "failed",
    }
}
