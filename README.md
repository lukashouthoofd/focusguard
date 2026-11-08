# FocusGuard

FocusGuard is a free, open-source focus blocker for Windows, with macOS support under development. It is being developed from the MIT-licensed [Focuser 0.11.5](https://github.com/aadeshrao123/Focuser) project, with the goal of making blocking more reliable and recovery easier to understand. See [upstream attribution](docs/upstream-focuser.md).

## Current state

The repository contains Focuser's Rust rules engine, Tauri desktop app, browser extension, and tests. An opt-in Windows service-client UI and persistent exact-domain hosts policy exist, but the service is not installed by the normal installer and the desktop app still owns most enforcement. The service does not enforce URL paths, wildcard rules, or application blocks. Website paths and other page-level rules depend on the browser extension. On macOS, enforcement still runs in the desktop process, and hosts-file fallback requires administrator write access. Closing or terminating the app stops its process enforcement until it starts again.

The Rust workspace, desktop frontend, and browser extension have automated tests. The Windows service has local named-pipe and temporary-hosts-file tests. Runtime blocking has not yet been verified in a disposable Windows VM or on macOS. No Windows service has been installed on the development machine, and no real hosts file, firewall, or other system policy has been changed by this project. CI includes unsigned macOS DMG packaging checks for Apple silicon and Intel; a green package job proves packaging, not signing, notarization, or runtime blocking.

## Goals

- Keep scheduled website and application rules active across app closure, crashes, and restarts.
- Put Windows enforcement behind an administrator-installed service with narrowly scoped, reviewable IPC.
- Keep URL-path and page-level rules in an optional browser extension.
- Provide explicit recovery and uninstall paths; describe the administrator and alternate-boot bypass boundary honestly.
- Remain local-first and open source.

These goals are not all implemented yet. Read [PROGRESS.md](PROGRESS.md), the [research notes](docs/research/), the [Windows feasibility notes](docs/research/windows-feasibility.md), and the [service architecture proposal](docs/design/windows-service.md) for current evidence and known gaps.

## Build and test

The desktop app uses stable Rust, Node.js 22/npm, and Tauri v2. Follow the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/): Windows requires the MSVC build tools and WebView2; macOS desktop development requires Xcode or Xcode Command Line Tools. Build the CLI sidecar before running the desktop app; CI stages it under the host-specific sidecar name.

### Windows

```powershell
cargo build -p focuser-cli
$targetTriple = (rustc -vV | Select-String '^host:' | ForEach-Object { ($_ -split '\s+')[1] })
New-Item -ItemType Directory -Force crates/focuser-ui/binaries | Out-Null
Copy-Item "target/debug/focuser-cli.exe" "crates/focuser-ui/binaries/focuser-cli-$targetTriple.exe"
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked

Push-Location crates/focuser-ui/frontend
npm ci
npm test
npm run build
Pop-Location

Push-Location extension
npm ci
npm run compile
npm test
npm run build
npm run build:firefox
Pop-Location
```

### macOS desktop development

From the repository root, install the frontend dependencies, build the host CLI sidecar, and start Tauri:

```sh
npm ci --prefix crates/focuser-ui/frontend
cargo install tauri-cli --locked --version 2.10.1
cargo build --locked -p focuser-cli
target=$(rustc -vV | awk '/^host:/ {print $2}')
mkdir -p crates/focuser-ui/binaries
cp "target/debug/focuser-cli" "crates/focuser-ui/binaries/focuser-cli-$target"
cd crates/focuser-ui
cargo tauri dev
```

The app is not yet a persistent macOS blocker: website host rules need administrator access, and app enforcement stops when the desktop process exits. The current DMG packaging check skips code signing; a user-facing download still needs Apple signing and notarization. See the [Tauri macOS bundle guide](https://v2.tauri.app/distribute/macos-application-bundle/) and [signing guide](https://v2.tauri.app/distribute/sign/macos/).

The frontend and extension test/build commands have been run in this checkout. The Rust suite, strict Clippy, formatting check, and workspace build also pass. These are code checks, not runtime proof of blocking or crash recovery. See [CI](.github/workflows/ci.yml) for the cross-platform jobs.

Do not install a service or enable system-wide blocking on a production machine while evaluating this development build. Those paths must first be documented and tested in a disposable Windows VM.

## Security boundary

FocusGuard does not claim to be tamperproof. An administrator who controls the device can disable or remove its service, edit filtering policy, use another boot environment, or reinstall the OS. Current app-process enforcement is also bypassable by terminating the desktop process. Reliability and attack-surface claims will be tied to tested operating-system versions and user privilege levels.

## License

FocusGuard project files are MIT licensed; see [LICENSE](LICENSE). The imported Focuser baseline retains its separate MIT notice in [LICENSES/Focuser-MIT.txt](LICENSES/Focuser-MIT.txt). See [third-party notices](THIRD_PARTY_NOTICES.md) and the [dependency license inventory](docs/dependency-licenses.md); complete target-specific notices are still a release gate.
