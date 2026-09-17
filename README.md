# ssmux

`ssmux` is a Rust terminal manager for AWS Systems Manager sessions, designed to work like K9s: the TUI directly owns the sessions it starts. There is no daemon and no persistent connection mode in this version.

## Status

This repository contains the first MVP:

- Rust + Tokio + Ratatui + Crossterm
- TOML connection configuration
- Configuration CRUD through `ssmux config`
- AWS CLI `ssm start-session` child-process lifecycle management
- `start`, `status`, `doctor`, `init`, and `config` commands
- TUI keyboard controls for selecting, starting, stopping, and refreshing sessions

## Requirements

- Rust toolchain
- AWS CLI v2
- Session Manager Plugin
- An AWS profile with permission to start the configured SSM session

## Quick start

```bash
cargo build --release
./target/release/ssmux init
./target/release/ssmux doctor
./target/release/ssmux
```

In another terminal:

```bash
./target/release/ssmux status
./target/release/ssmux start demo-stage
```

`ssmux start NAME` runs one session in the foreground; press `Ctrl-C` to stop it. `ssmux status` lists configured connections, while live state is shown by the TUI.

The default configuration path is `~/.ssmux/config.toml`. Override it with `--config PATH`.

## Configuration

`ssmux init` creates an empty configuration file. Use `n` in the TUI to add a connection, or use the CLI commands below. A connection has this shape:

```toml
[[connections]]
name = "demo-stage"
target = "i-0deadbeefdeadbeef0"
region = "us-west-2"
document_name = "AWS-StartPortForwardingSession"

[connections.parameters]
portNumber = "22"
localPortNumber = "10022"
```

The session runner translates each connection into an AWS CLI command. Parameter order is deterministic. Extra AWS CLI arguments can be supplied with `extra_args` when needed.

For remote-host forwarding, use `AWS-StartPortForwardingSessionToRemoteHost` and add `host`, `portNumber`, and `localPortNumber` parameters.

Connection definitions can also be managed without opening the TUI:

```bash
ssmux config list
ssmux config add demo-db --target i-0deadbeefdeadbeef0 \
  --region us-west-2 --document-name AWS-StartPortForwardingSessionToRemoteHost \
  --parameter host=db.example.test --parameter portNumber=5432 \
  --parameter localPortNumber=15432
ssmux config edit demo-db --temporary
ssmux config remove demo-db
```

Configuration changes are loaded the next time the TUI or `start` command is launched.

## TUI controls

- `↑` / `↓` or `j` / `k`: select a connection
- `n`: add a connection
- `e`: edit the selected connection
- `d`: ask for confirmation, then delete the selected connection
- `s`: start the selected connection
- `x`: stop the selected connection
- `r`: refresh state
- `/`: open the connection filter; press `Enter` or `Esc` to close it
- `q`, `Esc`, or `Ctrl-C`: quit the TUI

In the connection form, the Document field is a selector for the common SSM documents. Use `↑` / `↓` to change it. Parameters appear as a Key / Value table; press `Tab` from the last Value to add another row and `Ctrl-D` to remove the current row.

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```
