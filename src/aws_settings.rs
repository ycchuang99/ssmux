use std::process::Stdio;

use tokio::{
    io::AsyncReadExt,
    process::Command,
    task::JoinSet,
    time::{timeout, Duration},
};

#[derive(Clone, Debug, Default)]
pub struct AwsSettings {
    pub profiles: Vec<AwsProfile>,
    pub issues: Vec<String>,
    pub checked: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AwsProfile {
    pub name: String,
    pub region: Option<String>,
    pub sso: bool,
}

impl AwsSettings {
    pub fn summary(&self) -> String {
        if !self.checked {
            return "Checking local AWS setup… Enter settings manually, or reopen this editor after the check finishes.".into();
        }
        if !self.issues.is_empty() {
            return self.issues.join("\n");
        }
        if self.profiles.is_empty() {
            "AWS tools found. No saved profiles; configure AWS credentials or use your existing environment credentials.".into()
        } else {
            "AWS tools found. Choose an existing profile below. AWS access is checked when connecting.".into()
        }
    }
}

pub fn command(program: &str) -> Command {
    let mut command = Command::new(program);
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
    command
        .env("AWS_PAGER", "")
        .env("AWS_CLI_AUTO_PROMPT", "off");
    command
}

pub async fn discover() -> AwsSettings {
    let mut settings = AwsSettings {
        checked: true,
        ..Default::default()
    };
    if timeout(Duration::from_secs(5), discover_into(&mut settings))
        .await
        .is_err()
    {
        settings.issues.push("AWS setup check timed out. Loaded profiles remain available; enter missing details manually.".into());
    }
    settings
}

async fn discover_into(settings: &mut AwsSettings) {
    let mut aws_command = command("aws");
    let mut plugin_command = command("session-manager-plugin");
    let (aws, plugin) = tokio::join!(
        local_output(aws_command.arg("--version")),
        local_output(plugin_command.arg("--version")),
    );
    let aws_available = aws
        .as_deref()
        .is_some_and(|value| value.starts_with("aws-cli/2."));
    if !aws_available {
        settings.issues.push(
            "AWS CLI v2 is unavailable. Install or update it with: brew install awscli".into(),
        );
    }
    if plugin.as_deref().is_none_or(str::is_empty) {
        settings.issues.push("Session Manager Plugin is unavailable. Install it with: brew install --cask session-manager-plugin".into());
    }
    if !aws_available {
        return;
    }
    let Some(profiles) = local_output(command("aws").args(["configure", "list-profiles"])).await
    else {
        settings.issues.push("Could not read AWS profiles. Check your AWS configuration; manual entry is still available.".into());
        return;
    };
    settings.profiles = profile_names(&profiles)
        .into_iter()
        .map(|name| AwsProfile {
            name,
            ..Default::default()
        })
        .collect();
    let names = settings
        .profiles
        .iter()
        .map(|profile| profile.name.clone())
        .collect::<Vec<_>>();
    let mut jobs = JoinSet::new();
    let mut pending = names.into_iter().enumerate();
    let enrich = async {
        loop {
            while jobs.len() < 4 {
                let Some((index, name)) = pending.next() else {
                    break;
                };
                jobs.spawn(async move {
                    let (region, session, legacy) = tokio::join!(
                        setting(&name, "region"),
                        setting(&name, "sso_session"),
                        setting(&name, "sso_start_url"),
                    );
                    (index, region, session.is_some() || legacy.is_some())
                });
            }
            let Some(result) = jobs.join_next().await else {
                break;
            };
            if let Ok((index, region, sso)) = result {
                settings.profiles[index].region = region;
                settings.profiles[index].sso = sso;
            }
        }
    };
    enrich.await;
}

async fn setting(profile: &str, key: &str) -> Option<String> {
    local_output(command("aws").args(["configure", "get", key, "--profile", profile]))
        .await
        .filter(|value| !value.is_empty())
}

fn profile_names(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn local_output(command: &mut Command) -> Option<String> {
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let _process_group = LocalProcessGroup(child.id());
    let stdout = child.stdout.take()?;
    let result = timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        stdout.take(65_537).read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > 65_536 || !child.wait().await.ok()?.success() {
            return None;
        }
        let value = String::from_utf8(bytes).ok()?.trim().to_owned();
        Some(value)
    })
    .await
    .ok()
    .flatten();
    if result.is_none() {
        let _ = child.kill().await;
    }
    result
}

struct LocalProcessGroup(Option<u32>);

impl Drop for LocalProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // A user-installed AWS wrapper can spawn children; cancellation must stop those too.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_sorted_and_empty_or_duplicate_lines_are_removed() {
        assert_eq!(
            profile_names("work\ndefault\n\nwork\n"),
            ["default", "work"]
        );
        assert!(!AwsSettings::default().summary().contains("tools found"));
        let settings = AwsSettings {
            checked: true,
            ..Default::default()
        };
        assert!(settings.summary().contains("No saved profiles"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_commands_reject_failures_and_unbounded_output() {
        assert_eq!(
            local_output(Command::new("/bin/sh").args(["-c", "printf ' us-west-2\\n'"]))
                .await
                .as_deref(),
            Some("us-west-2")
        );
        assert!(
            local_output(Command::new("/bin/sh").args(["-c", "printf secret >&2; exit 1"]))
                .await
                .is_none()
        );
        assert!(
            local_output(Command::new("/bin/sh").args(["-c", "head -c 70000 /dev/zero"]))
                .await
                .is_none()
        );
        assert_eq!(
            local_output(Command::new("/bin/sh").args(["-c", "exit 0"])).await,
            Some(String::new())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn setup_timeout_terminates_the_local_command() {
        let path = std::env::temp_dir().join(format!("ssmux-setup-timeout-{}", std::process::id()));
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "echo $$ > \"$1\"; exec sleep 60", "ssmux-setup-test"])
            .arg(&path);
        assert!(local_output(&mut command).await.is_none());
        let pid = std::fs::read_to_string(&path).unwrap();
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn cancelling_setup_terminates_a_wrapper_and_its_children() {
        let directory =
            std::env::temp_dir().join(format!("ssmux-setup-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let parent = directory.join("parent");
        let descendant = directory.join("child");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "echo $$ > \"$1\"; sleep 60 & echo $! > \"$2\"; wait",
                "ssmux-setup-test",
            ])
            .arg(&parent)
            .arg(&descendant);
        let task = tokio::spawn(async move { local_output(&mut command).await });
        timeout(Duration::from_secs(2), async {
            while !descendant.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        for path in [&parent, &descendant] {
            let pid = std::fs::read_to_string(path)
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap();
            timeout(Duration::from_secs(2), async {
                while unsafe { libc::kill(pid, 0) } == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
