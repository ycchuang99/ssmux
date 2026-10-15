use std::{collections::BTreeMap, process::Stdio};

use anyhow::Result;
use tokio::process::{Child, Command};

use crate::config::ConnectionConfig;

pub fn spawn_session(config: &ConnectionConfig) -> Result<Child> {
    if is_interactive(config) {
        anyhow::bail!("interactive shells require a terminal; choose a port-forwarding document");
    }
    let mut command = command_for(config);
    command
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Into::into)
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
