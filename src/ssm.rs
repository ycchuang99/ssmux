use std::{collections::BTreeMap, process::Stdio};

use anyhow::Result;
use tokio::process::{Child, Command};

use crate::config::ConnectionConfig;

pub fn spawn_session(config: &ConnectionConfig) -> Result<Child> {
    let mut command = command_for(config);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Into::into)
}

pub fn spawn_interactive_session(config: &ConnectionConfig) -> Result<Child> {
    let mut command = command_for(config);
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(Into::into)
}

pub fn is_interactive(config: &ConnectionConfig) -> bool {
    matches!(
        config.document_name.as_deref(),
        None | Some("AWS-StartInteractiveCommand")
    )
}

fn command_for(config: &ConnectionConfig) -> Command {
    let mut command = Command::new("aws");
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
        parameters
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(","),
    )
}

#[cfg(test)]
mod tests {
    use super::parameters_argument;
    use crate::config::ConnectionConfig;
    use std::collections::BTreeMap;

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
        };

        assert_eq!(
            parameters_argument(&config.parameters).as_deref(),
            Some("localPortNumber=10022,portNumber=22")
        );
    }
}
