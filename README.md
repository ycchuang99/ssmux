# SSMux

A macOS menu bar manager for AWS Systems Manager port-forwarding sessions. Start and stop connections from the menu bar, with no terminal window or daemon. Quitting SSMux stops the sessions and plugin processes it owns.

## Installation

```bash
brew install --cask ycchuang99/tap/ssmux
```

Requires macOS, AWS CLI v2, the AWS Session Manager Plugin, and an AWS profile permitted to start your configured sessions. Homebrew installs the local command dependencies with the app.

From source:

```bash
./scripts/run-macos-menubar.sh
```

The script builds and opens `target/SSMux.app`.

App bundles are ad-hoc signed, not Developer ID signed or notarized. macOS may require you to allow a downloaded app in Privacy & Security before its first launch.

Or run `cargo run --release` to launch the menu bar directly. `ssmux menubar` remains an explicit launch command. No arguments open only the menu bar; use `--config PATH` to select a different configuration file.

## Using the menu bar

- **New Session…** opens a resizable native editor. Choose **Remote Host**, **SSM Instance**, or **Custom Document**, then fill in the labeled fields. Validation identifies and focuses the first invalid field. Saving does not start a session; a failed save keeps your input in the editor. Existing sessions continue updating while you edit.
- Click the menu bar icon to open your sessions. Each row shows its status in text as well as color. Standard forwarding starts as **Starting…**, then shows **Port Open** when the AWS plugin confirms its local listener. This does not verify that the remote service is available. Custom documents show **Process Running**, because their readiness is unknown. Click the connection button to cancel a startup, connect, retry, or disconnect. The running pulse respects macOS Reduce Motion and does not represent measured traffic.
- Click the connection button to connect or disconnect. The panel stays open, including after a connection failure; click outside to dismiss it. The menu bar icon turns green while sessions are active.
- The route below the name shows **localhost:PORT → destination:PORT** for standard forwarding documents. Long text is shortened visually; its full value remains available in a tooltip. Open **Manage** for live status, destination, SSM target, region, profile, **Copy Local Address**, error details, **Edit Session…**, and **Delete Session…**. Failed rows show a short reason; **View Error Details…** opens a scrollable panel while other sessions keep updating, and **Copy Error Details** copies the diagnostic. Deletion requires confirmation; Cancel is the default action.
- List updates preserve your scroll position. **More → Quit SSMux & Stop Sessions** exits the app and stops its sessions; **⌘Q** remains available.
- Use **⌘N** for a new session and standard **⌘C/V/A/Z** shortcuts in the form. Stop an active session before editing or deleting it. Changes appear immediately and are saved atomically. If another process changes your configuration, SSMux refuses to overwrite it; restart the app to load those changes.

The editor uses your AWS defaults when region or profile is blank. **Remote Host** and **SSM Instance** choose the matching AWS forwarding document automatically and validate ports from 1 to 65535. **Advanced Settings** expands the document name and additional `key=value` parameters. **Custom Document** puts its document and parameters first and does not assume they describe a network route. Switching types preserves your input. Editing preserves custom parameters and any `extra_args` configured in TOML.

SSMux keeps the last 8 KiB of each session's stdout and stderr in memory for diagnostics. Readiness detection uses the AWS plugin's local-port-open message; an unrecognized plugin message leaves the session in **Starting…**, where it can still be cancelled.

Interactive shell and SSH documents need a terminal and cannot run in the menu bar. Existing definitions remain saved and can be edited or removed.

### Upgrading from 0.3.x

Version 0.4.0 opens the menu bar by default and installs as `SSMux.app`. The terminal interface and foreground `start` command have been removed; configuration and diagnostic commands remain available. Your existing configuration is preserved. Quit the old app before opening the new version; quitting stops its active sessions.

## Configuration and diagnostics

The default configuration path is `~/.ssmux/config.toml`. The app starts with an empty list if the file does not exist. You can also use the command-line configuration and diagnostic tools:

```bash
ssmux init
ssmux doctor
ssmux status --json
ssmux config list
ssmux config add demo-db --target i-0deadbeefdeadbeef0 \
  --region us-west-2 --document-name AWS-StartPortForwardingSessionToRemoteHost \
  --parameter host=db.example.test --parameter portNumber=5432 \
  --parameter localPortNumber=15432
ssmux config edit demo-db --region us-east-1
ssmux config remove demo-db
```

`status` lists saved definitions; live process state is shown in the running menu bar. Restart the app after editing the configuration outside SSMux. The Homebrew Formula installs these command-line tools; the Cask installs the menu bar app bundle.

A connection has this shape:

```toml
[[connections]]
name = "Staging database"
target = "i-0deadbeefdeadbeef0"
region = "us-west-2"
document_name = "AWS-StartPortForwardingSession"

[connections.parameters]
portNumber = "5432"
localPortNumber = "15432"
```

Parameters are passed as JSON to the AWS CLI, preserving commas, quotes, and spaces in values. Extra AWS CLI arguments can be supplied with `extra_args`.

## Development

```bash
cargo fmt --all -- --check
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo build --locked --release
scripts/package-macos-menubar.sh target/release/ssmux
```

Session lifecycle tests use local child processes and do not contact AWS. macOS builds also test menu-bar persistence and form validation. Linux builds provide configuration/diagnostic tools only.

Regenerate the app and menu bar icons on macOS with `swift scripts/generate-icons.swift`.

## Releases

Pull requests targeting `main` run formatting, Clippy, tests, release builds, and macOS app packaging. Pushes to `main` run the same checks, then create a release for Conventional Commits: `feat` bumps the minor version, `fix`, `perf`, and `revert` bump the patch version, and `!` or a `BREAKING CHANGE` footer bumps the major version. Other commit types do not publish a release.

The release automation updates `Cargo.toml` and `Cargo.lock`, commits the version as `chore(release): VERSION`, pushes a `vVERSION` tag, and publishes the CLI archives and macOS menu bar app bundles. The tag workflow then updates `ycchuang99/homebrew-tap` with the Formula for the configuration tools and the Cask for the menu bar app.

Add a repository Actions secret named `RELEASE_TOKEN` with contents write access to both `ycchuang99/ssmux` and `ycchuang99/homebrew-tap`. If branch rules protect `main`, allow this token to push the generated release commit.
