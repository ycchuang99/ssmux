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

Local builds are ad-hoc signed. The release workflow requires Developer ID signing and Apple notarization before publishing new macOS packages. Older downloads may still require manual approval in Privacy & Security.

Or run `cargo run --release` to launch the menu bar directly. `ssmux menubar` remains an explicit launch command. No arguments open only the menu bar; use `--config PATH` to select a different configuration file.

## Using the menu bar

- **New Session…** opens a resizable native editor. Choose **Remote Host**, **SSM Instance**, or **Custom Document**. Choose an existing AWS Profile or enter one manually; a profile's region fills an empty region field without replacing a manually entered region. Saving does not connect.
- **Check Setup** checks local AWS CLI v2, Session Manager Plugin, and saved profiles in the background. It does not contact AWS or prove credentials or permissions. Manual entry remains available if discovery is incomplete.
- Click the connection button to connect, cancel a pending connection/retry, or disconnect. The panel stays open. Text and icons distinguish **Connecting**, **Tunnel Ready**, **Stopping**, **Not Connected**, **Interrupted**, and **Failed**. Only a matching port-open confirmation from the current Session Manager process turns the button and menu bar icon green; an unconfirmed connection times out after 30 seconds.
- **Tunnel Ready** means the plugin opened the forwarding listener, not that a remote database is reachable or authenticated. The pulse is not a traffic measurement and respects macOS Reduce Motion. Custom documents must emit the plugin's forwarding readiness confirmation; otherwise they fail closed after the timeout.
- Each row shows **localhost:PORT → actual-host:PORT**, the configured Profile/Region, and status. AWS default labels mean the CLI resolves the value; they are not a verified AWS account identity.
- On failure, the row explains the next step: **Sign In…** for SSO expiry, **Change Port…**, **Check Setup**, **Reconnect**, or **Details…**. AWS SSO sign-in opens your browser and does not automatically restart the session. Other credential providers use their own renewal process.
- **Manage** includes **Copy Local Address**, **Duplicate Session…**, **Pin Session**, **View Diagnostics…**, **Sign In to AWS SSO…**, **Edit Session…**, and **Delete Session…**. Duplicating preserves advanced parameters but asks for a new local port and leaves automatic reconnection off. Shared local ports are warned about because those sessions cannot run together. Deletion requires confirmation; Cancel is the default.
- Search names, hosts, targets, profiles, or regions with the search field or **⌘F**. Pinned sessions appear first; saved configuration order is preserved. **⌘N** creates a session, and native **⌘C/V/A/Z**, Tab, Enter, and Esc work in the editor.
- Stop an active or reconnecting session before editing/deleting it. Pinning is allowed while active. Changes are saved atomically; outside edits are protected from overwriting and require restarting SSMux.

The editor uses AWS defaults when region or profile is blank. **Remote Host** and **SSM Instance** select the matching AWS forwarding document and validate ports from 1 to 65535. **Advanced Settings** contains the document and additional `key=value` parameters. Editing preserves `extra_args` configured in TOML. Interactive shells and SSH documents require a terminal and cannot run here.

**Automatically reconnect** is off by default, per session. When enabled, recognized temporary network failures retry after 2, 4, and 8 seconds, at most three times per manual Connect, even if a retry briefly succeeds. Credential, permission, target, port, and unknown errors require attention. Cancel/Disconnect and quitting stop retries. After detected Mac sleep, the old tunnel is stopped and shown as interrupted or reconnected according to this preference. Monitoring observes the owned processes and plugin output; it does not continuously test remote services or guarantee immediate detection of every silent network failure.

Diagnostics keep only the most recent 8 KiB of output in memory, redact recognized credentials/authentication URLs and oversized lines, and can be copied from **View Diagnostics…**. They may still contain infrastructure names; review before sharing. No connection telemetry is uploaded.

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
auto_reconnect = false
pinned = false

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
python3 scripts/test-macos-release.py
```

Session lifecycle tests use local child processes and loopback sockets and do not contact AWS. macOS builds also test menu-bar persistence and form validation. Linux builds provide configuration/diagnostic tools only.

Regenerate the app and menu bar icons on macOS with `swift scripts/generate-icons.swift`.

### Manual acceptance checks

Use a disposable config (`--config /path/to/test.toml`) and test AWS account. Confirm first-session creation, profile/region selection, cancellation while connecting, port conflict, expired SSO, denied permission, destination failure, unexpected disconnect, sleep/resume, retry limits, and quit cleanup. Verify search/pinning and duplicate/copy with several environments; check keyboard focus, VoiceOver, dark appearance, and Reduce Motion on a Mac. Unit tests and mock processes do not replace these live AWS checks.

For a small user trial, measure time from first launch to a usable tunnel, recovery without help after a failure, and incorrect environment selections. No baseline or improvement claim is implied by the automated tests.

## Releases

Pull requests targeting `main` run formatting, Clippy, tests, release builds, and macOS app packaging. Pushes to `main` run the same checks, then create a release for Conventional Commits: `feat` bumps the minor version, `fix`, `perf`, and `revert` bump the patch version, and `!` or a `BREAKING CHANGE` footer bumps the major version. Other commit types do not publish a release.

The release automation updates `Cargo.toml` and `Cargo.lock`, commits the version as `chore(release): VERSION`, pushes a `vVERSION` tag, and publishes the CLI archives and macOS menu bar app bundles. The tag workflow then updates `ycchuang99/homebrew-tap` with the Formula for the configuration tools and the Cask for the menu bar app.

Homebrew checksums are calculated from the published release downloads. Retrying a release preserves existing public assets and uses their bytes, even when a repeated signing run produces different timestamps.

Add a repository Actions secret named `RELEASE_TOKEN` with contents write access to both `ycchuang99/ssmux` and `ycchuang99/homebrew-tap`. If branch rules protect `main`, allow this token to push the generated release commit.

### macOS signing and notarization

Before creating a release tag, configure these repository Actions secrets:

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE_BASE64` | Base64-encoded `.p12` export of a Developer ID Application certificate **and its private key**. |
| `MACOS_CERTIFICATE_PASSWORD` | Password protecting that `.p12` export. |
| `MACOS_SIGNING_IDENTITY` | Full certificate name, such as `Developer ID Application: Example Company (TEAMID)`. |
| `APPLE_ID` | Apple Account authorized to notarize for the developer team. |
| `APPLE_TEAM_ID` | Developer team identifier for the signing certificate. |
| `APPLE_APP_SPECIFIC_PASSWORD` | An app-specific password for that Apple Account. |

Keep these values in Actions secrets, never in the repository. The macOS release job creates a temporary keychain, signs the CLI and app with hardened runtime and a secure timestamp, and submits both to Apple. It waits up to 20 minutes for an `Accepted` result, staples and validates the app's ticket, and checks Gatekeeper before creating the final app ZIP and checksums. Missing credentials, rejection, timeout, or verification failure stop publication; there is no ad-hoc fallback. Temporary signing material is removed on success and failure.

For a local release check, export the same variables through your secret-management workflow and run:

```bash
scripts/notarize-macos.sh target/release/ssmux /tmp/SSMux-notarized.zip
```

This submits the built app and CLI to Apple. Normal `package-macos-menubar.sh` and PR builds need no Apple credentials and remain ad-hoc signed. The release-gate self-test uses fake Apple commands; it checks failure handling and cleanup, not certificate validity or Apple's service. Before distributing the first signed release, test the downloaded ZIP on a clean Mac under the default Gatekeeper settings.

References: [Apple notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution), [Apple certificates on GitHub runners](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications).
