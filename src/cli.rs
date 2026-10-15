use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::ConnectionConfig;

#[derive(Debug, Parser)]
#[command(
    name = "ssmux",
    version,
    about = "A macOS menu bar manager for AWS SSM sessions"
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
    #[cfg(target_os = "macos")]
    /// Run a menu bar-only session controller on macOS.
    Menubar,
    /// List configured connections. Live state belongs to the menu bar process.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_interfaces_are_rejected_and_default_launch_accepts_config() {
        assert!(Cli::try_parse_from(["ssmux", "tui"]).is_err());
        assert!(Cli::try_parse_from(["ssmux", "start", "demo"]).is_err());
        let cli = Cli::try_parse_from(["ssmux", "--config", "/tmp/demo.toml"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.config.unwrap(), PathBuf::from("/tmp/demo.toml"));
    }
}
