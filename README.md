# ssmux

`ssmux` is a Rust terminal manager for AWS Systems Manager sessions. It keeps the terminal UI separate from the background daemon. Connections marked `keep_on_exit = true` stay alive after the TUI closes; temporary connections are stopped when the TUI exits.

## Status

This repository contains the first MVP:

- Rust + Tokio + Ratatui + Crossterm
- TOML connection configuration
- Configuration CRUD through `ssmux config`
- Unix-socket JSON IPC between CLI/TUI and daemon
- AWS CLI `ssm start-session` child-process lifecycle management
- `start`, `stop`, `status`, `doctor`, `init`, and `config` commands
- TUI keyboard controls for selecting, starting, stopping, and refreshing sessions

The daemon is intentionally foreground-only in this version. A macOS LaunchAgent can be added after the lifecycle behavior is validated.

## Requirements

- Rust toolchain
- AWS CLI v2
- Session Manager Plugin
- An AWS profile with permission to start the configured SSM session

## Quick start

```bash
cargo build --release
./target/release/ssmux init
# Edit the generated TOML file and replace the example target.
./target/release/ssmux doctor
./target/release/ssmux daemon
```

In another terminal:

```bash
./target/release/ssmux status
./target/release/ssmux start demo-stage
./target/release/ssmux stop demo-stage
./target/release/ssmux
```

The generated example is a background connection. For a temporary connection, omit `keep_on_exit` or set it to `false`.

The default configuration path is platform-specific. On macOS it is normally under `~/Library/Application Support/ssmux/config.toml`. Override it with `--config PATH`; override the daemon socket with `--socket PATH`.

## Configuration

`ssmux init` creates a configuration like this:

```toml
[[connections]]
name = "demo-stage"
target = "i-0deadbeefdeadbeef0"
region = "us-west-2"
document_name = "AWS-StartPortForwardingSession"
keep_on_exit = true

[connections.parameters]
portNumber = "22"
localPortNumber = "10022"
```

The daemon translates each connection into an AWS CLI command. Parameter order is deterministic. Extra AWS CLI arguments can be supplied with `extra_args` when needed.

For remote-host forwarding, use `AWS-StartPortForwardingSessionToRemoteHost` and add `host`, `portNumber`, and `localPortNumber` parameters.

Connection definitions can also be managed without opening the TUI:

```bash
ssmux config list
ssmux config add demo-db --target i-0deadbeefdeadbeef0 \
  --region us-west-2 --document-name AWS-StartPortForwardingSessionToRemoteHost \
  --keep-on-exit --parameter host=db.example.test --parameter portNumber=5432 \
  --parameter localPortNumber=15432
ssmux config edit demo-db --temporary
ssmux config remove demo-db
```

When a daemon is running, configuration changes are reloaded immediately. Editing or removing an active connection requires stopping it first.

## TUI controls

- `↑` / `↓` or `j` / `k`: select a connection
- `s`: start the selected connection
- `x`: stop the selected connection
- `r`: refresh state
- `q`, `Esc`, or `Ctrl-C`: quit the TUI

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```
