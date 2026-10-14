# ssmux

`ssmux` is a Rust terminal manager for AWS Systems Manager sessions, designed to work like K9s: the TUI directly owns the sessions it starts. There is no daemon and no persistent connection mode in this version.

## Status

This repository contains the first MVP:

- Rust + Tokio + Ratatui + Crossterm
- TOML connection configuration
- Configuration CRUD through `ssmux config`
- AWS CLI `ssm start-session` child-process lifecycle management
- `start`, `status`, `doctor`, `init`, and `config` commands
- TUI keyboard controls for selecting, starting, and stopping sessions
- A macOS menu bar mode in the same executable

## Installation

### Homebrew

The Homebrew formula is maintained in the `ycchuang99/homebrew-ssmux` tap. After the formula is published:

```bash
brew install ycchuang99/ssmux/ssmux
```

`ssmux` also requires the AWS CLI and Session Manager Plugin:

```bash
brew install awscli
brew install --cask session-manager-plugin
```

### Build from source

```bash
cargo build --release
sudo install -m 0755 target/release/ssmux /usr/local/bin/ssmux
```

## Requirements

- AWS CLI v2
- Session Manager Plugin
- An AWS profile with permission to start the configured SSM session

Rust is only required when building `ssmux` from source.

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

`ssmux start NAME` runs one session in the foreground; press `Ctrl-C` to stop it. `ssmux status` lists configured connections, while live state is shown by a running TUI or menu bar process.

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
ssmux config edit demo-db --region us-east-1
ssmux config remove demo-db
```

Configuration changes are loaded when the TUI, menu bar, or `start` command is launched.

## macOS menu bar

The menu bar uses a monochrome terminal-and-channels icon that adapts to macOS appearance. The icon turns green while sessions are active; no number is displayed. Connected sessions use green status dots in the list. Each connection shows its state, target, AWS region/profile, forwarding endpoint, and last error in its submenu. Choose **New Session…** to open a native form and save a connection; it appears in the list immediately and remains disconnected until you choose **Start Session**. The app bundle includes the matching blue/teal icon. Regenerate the vector-drawn assets on macOS with `swift scripts/generate-icons.swift`.

Run `./scripts/run-macos-menubar.sh` from the source checkout to build and open the menu bar-only bundle using the same `ssmux` executable. GitHub releases also include a menu bar app bundle for each supported Mac architecture. Click `ssmux` in the menu bar, choose a connection, then start or stop its background session. Quitting the menu bar process stops sessions it owns. Interactive shell sessions must be started from the TUI.

## TUI controls

- `↑` / `↓` or `j` / `k`: select a connection
- Click a header action; session actions apply to the selected connection
- `n`: add a connection
- `e`: edit the selected connection
- `d`: ask for confirmation, then delete the selected connection
- `s`: start the selected connection
- `x`: stop the selected connection
- `/`: open the connection filter; press `Enter` or `Esc` to close it
- `q`, `Esc`, or `Ctrl-C`: quit the TUI

In the connection form, the Document field is a selector for the common SSM documents. Use `↑` / `↓` to change it. Parameters appear as a Key / Value table; press `Tab` from the last Value to add another row and `Ctrl-D` to remove the current row.

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```
