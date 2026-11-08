# FocusGuard

FocusGuard is a free, open-source focus blocker for Windows. It is being developed from the MIT-licensed [Focuser 0.11.5](https://github.com/aadeshrao123/Focuser) project, with the goal of making blocking more reliable and recovery easier to understand. See [upstream attribution](docs/upstream-focuser.md).

## Current state

The repository contains Focuser's Rust rules engine, Tauri desktop app, browser extension, and existing tests. A Windows SCM host and secured named-pipe smoke server now exist, but the service accepts only a version probe and owns no policy. The current Windows blocking loop still runs inside the desktop app. Website domain fallback uses the Windows hosts file; URL paths and similar browser rules depend on the extension. If the app exits unexpectedly, process enforcement stops until it starts again. This is not yet a service-backed blocker.

The Rust workspace, desktop frontend, and browser extension have automated tests. The named-pipe smoke server has also passed local Windows pipe tests. Runtime blocking has not yet been verified in a Windows VM. No installer has been run, no Windows service has been installed, and no hosts, firewall, or system policy changes have been made by this project.

## Goals

- Keep scheduled website and application rules active across app closure, crashes, and restarts.
- Put Windows enforcement behind an administrator-installed service with narrowly scoped, reviewable IPC.
- Keep URL-path and page-level rules in an optional browser extension.
- Provide explicit recovery and uninstall paths; describe the administrator and alternate-boot bypass boundary honestly.
- Remain local-first and open source.

These goals are not all implemented yet. Read [PROGRESS.md](PROGRESS.md), the [research notes](docs/research/), the [Windows feasibility notes](docs/research/windows-feasibility.md), and the [service architecture proposal](docs/design/windows-service.md) for current evidence and known gaps.

## Build and test

Development requires stable Rust with the MSVC toolchain, Node.js 22/npm, and the Windows build prerequisites for Tauri v2 (including WebView2). Build the CLI sidecar before building the desktop application; CI stages it under the host-specific sidecar name.

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

The frontend and extension test/build commands have been run in this checkout. The Rust suite, strict Clippy, formatting check, and workspace build also pass. These are code checks, not runtime proof of blocking or crash recovery. See [CI](.github/workflows/ci.yml) for the cross-platform jobs.

Do not install a service or enable system-wide blocking on a production machine while evaluating this development build. Those paths must first be documented and tested in a disposable Windows VM.

## Security boundary

FocusGuard does not claim to be tamperproof. An administrator who controls the device can disable or remove its service, edit filtering policy, use another boot environment, or reinstall the OS. Current app-process enforcement is also bypassable by terminating the desktop process. Reliability and attack-surface claims will be tied to tested Windows versions and user privilege levels.

## License

FocusGuard project files are MIT licensed; see [LICENSE](LICENSE). The imported Focuser baseline retains its separate MIT notice in [LICENSES/Focuser-MIT.txt](LICENSES/Focuser-MIT.txt). See [third-party notices](THIRD_PARTY_NOTICES.md) and the [dependency license inventory](docs/dependency-licenses.md); complete target-specific notices are still a release gate.
