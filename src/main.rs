mod cli;
mod config;
mod session;
mod ssm;
mod tui;

use std::path::Path;

use anyhow::{Context, Result};
use clap::Parser;

use crate::{
    cli::{Cli, Command, ConfigCommand},
    config::{Config, ConnectionConfig},
    ssm::{is_interactive, spawn_interactive_session, spawn_session},
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("ssmux: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(config::default_config_path);

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => tui::run(&config_path).await,
        Command::Start { name } => run_foreground(&config_path, &name).await,
        Command::Status { json } => {
            let config = Config::load_or_default(&config_path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&config.connections)?);
            } else {
                cli::print_configurations(&config.connections);
            }
            Ok(())
        }
        Command::Doctor => doctor(&config_path),
        Command::Init => init_config(&config_path),
        Command::Config { command } => config_command(&config_path, command),
    }
}

async fn run_foreground(config_path: &Path, name: &str) -> Result<()> {
    let config = Config::load(config_path)?;
    let connection = config
        .connections
        .iter()
        .find(|connection| connection.name == name)
        .with_context(|| format!("unknown connection: {name}"))?;
    let mut child = if is_interactive(connection) {
        spawn_interactive_session(connection)
    } else {
        spawn_session(connection)
    }
    .with_context(|| format!("starting AWS session for {name}"))?;
    println!("started {name}; press Ctrl-C to stop it");

    tokio::select! {
        result = child.wait() => {
            let status = result.context("waiting for AWS session")?;
            if status.success() {
                println!("stopped {name}");
                Ok(())
            } else {
                anyhow::bail!("AWS session exited with code {:?}", status.code());
            }
        }
        signal = tokio::signal::ctrl_c() => {
            signal.context("waiting for Ctrl-C")?;
            child.start_kill().context("stopping AWS session")?;
            let _ = child.wait().await;
            println!("stopped {name}");
            Ok(())
        }
    }
}

fn config_command(config_path: &Path, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::List => {
            let config = Config::load_or_default(config_path)?;
            cli::print_configurations(&config.connections);
            Ok(())
        }
        ConfigCommand::Add {
            name,
            target,
            region,
            profile,
            document_name,
            parameters,
        } => {
            let mut config = Config::load_or_default(config_path)?;
            let connection = ConnectionConfig {
                name,
                target,
                region,
                profile,
                document_name,
                parameters: config::parse_parameters(&parameters)?,
                extra_args: Vec::new(),
            };
            if config
                .connections
                .iter()
                .any(|item| item.name == connection.name)
            {
                anyhow::bail!("connection already exists: {}", connection.name);
            }
            config.connections.push(connection);
            config.save_atomic(config_path)?;
            println!("configuration saved");
            Ok(())
        }
        ConfigCommand::Edit {
            name,
            target,
            region,
            profile,
            document_name,
            parameters,
            clear_parameters,
        } => {
            let mut config = Config::load(config_path)?;
            let connection = config
                .connections
                .iter_mut()
                .find(|item| item.name == name)
                .with_context(|| format!("unknown connection: {name}"))?;
            if let Some(target) = target {
                connection.target = target;
            }
            if region.is_some() {
                connection.region = region;
            }
            if profile.is_some() {
                connection.profile = profile;
            }
            if document_name.is_some() {
                connection.document_name = document_name;
            }
            if clear_parameters {
                connection.parameters.clear();
            } else if !parameters.is_empty() {
                connection.parameters = config::parse_parameters(&parameters)?;
            }
            config.save_atomic(config_path)?;
            println!("configuration saved");
            Ok(())
        }
        ConfigCommand::Remove { name } => {
            let mut config = Config::load(config_path)?;
            let old_len = config.connections.len();
            config.connections.retain(|item| item.name != name);
            if config.connections.len() == old_len {
                anyhow::bail!("unknown connection: {name}");
            }
            config.save_atomic(config_path)?;
            println!("configuration saved");
            Ok(())
        }
    }
}

fn init_config(path: &Path) -> Result<()> {
    if path.exists() {
        anyhow::bail!(
            "configuration already exists at {}; refusing to overwrite it",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, config::example_config())
        .with_context(|| format!("writing {}", path.display()))?;
    println!("created {}", path.display());
    Ok(())
}

fn doctor(config_path: &Path) -> Result<()> {
    let mut failed = false;

    if config_path.exists() {
        match Config::load(config_path) {
            Ok(config) => println!("ok   config: {} connection(s)", config.connections.len()),
            Err(error) => {
                println!("fail config: {error:#}");
                failed = true;
            }
        }
    } else {
        println!("fail config: {} does not exist", config_path.display());
        failed = true;
    }

    for program in ["aws", "session-manager-plugin"] {
        match std::process::Command::new(program)
            .arg("--version")
            .output()
        {
            Ok(output) if output.status.success() => println!("ok   command: {program}"),
            Ok(output) => {
                let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                println!("fail command: {program} ({detail})");
                failed = true;
            }
            Err(error) => {
                println!("fail command: {program} ({error})");
                failed = true;
            }
        }
    }

    if failed {
        anyhow::bail!("doctor found one or more issues");
    }
    Ok(())
}
