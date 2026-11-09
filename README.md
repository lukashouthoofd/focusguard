# FocusGuard

<p align="center">
  <img src="assets/focusguard-cover.png" alt="A violet focus target surrounded by browser windows" width="900">
</p>

FocusGuard is a free, open-source focus blocker for Windows, with macOS support under development. It is being developed from the MIT-licensed [Focuser 0.11.5](https://github.com/aadeshrao123/Focuser) project, with the goal of making blocking more reliable and recovery easier to understand. See [upstream attribution](docs/upstream-focuser.md).

## Current state

FocusGuard builds on the MIT-licensed Focuser rules engine, desktop app, and browser extension. Its Windows NSIS installer contains a per-machine, single-policy-owner service lifecycle, Native Messaging registration, and rollback/recovery handling. [Full CI run 42574126132](https://github.com/lukashouthoofd/focusguard/actions/runs/42574126132) passed Windows, Linux, Intel macOS, Apple Silicon macOS, frontend, extension, bindings, and package jobs. The Windows installer smoke ran on a disposable GitHub-hosted runner and exercised silent install and update, synthetic Native Messaging heartbeats, recovery after a privileged hosts-file edit and forced service crash, Windows resolution through the hosts file, and byte-for-byte hosts restoration on uninstall. The installer smoke also verified the full-week schedule remained active after update and forced service-crash recovery; a separate hosted SCM run confirmed it persisted across restart. It also confirmed that Cloudflare DoH returned public addresses for `example.com` while normal Windows resolution returned `127.0.0.1`, reproducing a documented bypass. Unsigned Windows NSIS and Intel/Apple Silicon macOS DMG artifacts are attached to that run for review; they are not public releases.

The service-mode UI lets users edit weekly schedules for a selected list. The hourly editor rejects an empty custom schedule (the engine treats an empty schedule as always active) and retains the existing gap requirement when a list uses scheduled protection or a shared allowance. The service's website enforcement writes each listed exact domain and its `www` form to the hosts file; it does not cover arbitrary subdomains, URL paths, keywords, or wildcard rules. Page-level browser matching requires the separately installed extension. The service also enforces a limited set of executable-name app rules after a fixed 30-second grace period. The service-mode UI and IPC cannot edit app rules; allowance-backed app rules and path, title, or bundle-ID matching are not enforced, and the service has no interactive save-and-close helper. Block reports and visit statistics still depend on the desktop UI's local API. No real Chrome or Firefox session has been tested against an installed service. The installer does not install the browser extension.

The installer requires administrator elevation and binds policy to one Windows account. It edits the Windows hosts file and installs a LocalSystem service. A user can install the browser extension separately from an unpacked build; Chrome Web Store and Mozilla add-on submission archives can be built in CI, but no store listing or signed browser package has been published. The Windows installer and macOS DMGs are unsigned; signing, notarization, store approval, updater signing, and a public release remain outstanding.

No service or system-wide policy has been installed on the development machine. The Windows service has passed local named-pipe and temporary-hosts-file tests; a separate hosted SCM test also passed crash recovery against a fixture. The installer smoke is a single hosted Windows run, not a supported-version matrix; it uses a synthetic Native Messaging client rather than a real browser. Windows hosts-file resolution and a Cloudflare DoH bypass were verified once; clean-boot recovery and standard-user service ACL behavior remain unverified. macOS enforcement still runs in the desktop process and requires administrator access for hosts-file writes.

## Windows installation boundary

The planned NSIS install is per-machine and applies the selected interactive account's policy. Its service runs as LocalSystem and enforces supported domain rules with the Windows hosts file while the desktop UI is closed. Install the extension separately for URL paths and other browser-only matching. The service reconciles its marked hosts-file section and removes only that section on uninstall; it does not rewrite the user's unrelated hosts entries.

This is not tamperproof. A local administrator can stop or remove the service, edit the hosts file, change DNS settings, use another boot environment, or reinstall Windows. Hosts-file filtering can also be bypassed by some DNS-over-HTTPS, VPN, proxy, alternate-browser, or networking setups. Do not treat the extension's reports or allowance telemetry as an authenticated record of browser activity.

## Goals

- Keep supported scheduled website rules active across app closure, service crashes, and restarts.
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

The frontend and extension tests/builds, Rust workspace suite, strict Clippy, and formatting check pass in this checkout. The NSIS package and Chrome/Firefox archives also build locally. These are not a substitute for the pending installed-product Windows run or a real browser session. See [CI](.github/workflows/ci.yml) and [extension packaging](.github/workflows/package-extension.yml).

The project has not yet produced a signed public download. The local installer payload in this checkout is unsigned and should be treated as a development build.

## License

FocusGuard project files are MIT licensed; see [LICENSE](LICENSE). The imported Focuser baseline retains its separate MIT notice in [LICENSES/Focuser-MIT.txt](LICENSES/Focuser-MIT.txt). See [third-party notices](THIRD_PARTY_NOTICES.md) and the [dependency license inventory](docs/dependency-licenses.md); complete target-specific notices are still a release gate.
