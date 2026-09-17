mod cli;
mod config;
mod daemon;
mod ipc;
mod ssm;
mod tui;

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use clap::Parser;

use crate::{
    cli::{Cli, Command, ConfigCommand},
    config::{Config, ConnectionConfig},
    ipc::{ConnectionState, Request},
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
    let socket_path = cli.socket.unwrap_or_else(config::default_socket_path);

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => tui::run(&socket_path).await,
        Command::Daemon => daemon::run(&config_path, &socket_path).await,
        Command::Start { name } => {
            let response = ipc::send_request(&socket_path, ipc::Request::Start { name }).await?;
            print_response(response)
        }
        Command::Stop { name, all } => {
            let request = if all {
                Request::StopAll
            } else {
                Request::Stop {
                    name: name.expect("clap requires a name unless --all is present"),
                }
            };
            let response = ipc::send_request(&socket_path, request).await?;
            print_response(response)
        }
        Command::Status { json } => {
            let response = ipc::send_request(&socket_path, ipc::Request::Status).await?;
            if !response.ok {
                anyhow::bail!(response.message);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&response.statuses)?);
            } else {
                cli::print_statuses(&response.statuses);
            }
            Ok(())
        }
        Command::Doctor => doctor(&config_path, &socket_path).await,
        Command::Init => init_config(&config_path),
        Command::Config { command } => config_command(&config_path, &socket_path, command).await,
    }
}

async fn config_command(
    config_path: &Path,
    socket_path: &Path,
    command: ConfigCommand,
) -> Result<()> {
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
            keep_on_exit,
            parameters,
        } => {
            let mut config = Config::load_or_default(config_path)?;
            let connection = ConnectionConfig {
                name,
                target,
                region,
                profile,
                document_name,
                parameters: parse_parameters(&parameters)?,
                extra_args: Vec::new(),
                keep_on_exit,
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
            reload_daemon(socket_path).await
        }
        ConfigCommand::Edit {
            name,
            target,
            region,
            profile,
            document_name,
            keep_on_exit,
            temporary,
            parameters,
            clear_parameters,
        } => {
            ensure_not_active(socket_path, &name).await?;
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
            if keep_on_exit {
                connection.keep_on_exit = true;
            } else if temporary {
                connection.keep_on_exit = false;
            }
            if clear_parameters {
                connection.parameters.clear();
            } else if !parameters.is_empty() {
                connection.parameters = parse_parameters(&parameters)?;
            }
            config.save_atomic(config_path)?;
            reload_daemon(socket_path).await
        }
        ConfigCommand::Remove { name } => {
            ensure_not_active(socket_path, &name).await?;
            let mut config = Config::load(config_path)?;
            let old_len = config.connections.len();
            config.connections.retain(|item| item.name != name);
            if config.connections.len() == old_len {
                anyhow::bail!("unknown connection: {name}");
            }
            config.save_atomic(config_path)?;
            reload_daemon(socket_path).await
        }
    }
}

fn parse_parameters(values: &[String]) -> Result<BTreeMap<String, String>> {
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

async fn ensure_not_active(socket_path: &Path, name: &str) -> Result<()> {
    let Ok(response) = ipc::send_request(socket_path, Request::Status).await else {
        return Ok(());
    };
    if let Some(status) = response.statuses.iter().find(|status| status.name == name) {
        if matches!(
            status.state,
            ConnectionState::Starting | ConnectionState::Running | ConnectionState::Stopping
        ) {
            anyhow::bail!("stop connection {name} before editing or removing it");
        }
    }
    Ok(())
}

async fn reload_daemon(socket_path: &Path) -> Result<()> {
    match ipc::send_request(socket_path, Request::Reload).await {
        Ok(response) if response.ok => {
            println!("configuration saved and daemon reloaded");
            Ok(())
        }
        Ok(response) => anyhow::bail!(response.message),
        Err(_) => {
            println!("configuration saved; start or restart the daemon to load it");
            Ok(())
        }
    }
}

fn print_response(response: ipc::Response) -> Result<()> {
    if response.ok {
        println!("{}", response.message);
        Ok(())
    } else {
        anyhow::bail!(response.message);
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

async fn doctor(config_path: &Path, socket_path: &Path) -> Result<()> {
    let mut failed = false;

    if config_path.exists() {
        match config::Config::load(config_path) {
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

    match ipc::send_request(socket_path, ipc::Request::Ping).await {
        Ok(response) if response.ok => println!("ok   daemon: {}", socket_path.display()),
        Ok(response) => {
            println!("fail daemon: {}", response.message);
            failed = true;
        }
        Err(error) => {
            println!("fail daemon: {error:#}");
            failed = true;
        }
    }

    if failed {
        anyhow::bail!("doctor found one or more issues");
    }
    Ok(())
}
