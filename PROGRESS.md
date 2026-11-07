# Progress

## 2026-11-08 — FocusGuard implementation baseline

The public repository was created at <https://github.com/lukashouthoofd/focusguard> after the user requested it. It now contains the MIT-licensed Focuser 0.11.5 source baseline, with its original notice preserved at `LICENSES/Focuser-MIT.txt` and attribution in `docs/upstream-focuser.md`. FocusGuard-specific work is tracked in this repository. The source import does not include Cold Turkey or Lockdown code.

### REA and research

- The REA skill is installed at `C:\Users\lukas\.agents\skills\reverse-engineer-anything\SKILL.md`. The user approved its Codex setup plan; REA MCP configuration was applied to `C:\Users\lukas\.codex\config.toml` with backup `config.toml.rea.backup`. The Codex host must restart/reconnect before its MCP tools appear. `rea doctor --client codex --json` reports the registration aligned.
- REA CLI `rea-agents@6.3.0` is installed through npx. Ghidra 12.1.4 and JDK 21 were found. REA artifact inspection inventoried the public Focuser portable executable; a subsequent Ghidra analysis timed out at the provider's 330-second limit before returning decompiler evidence. No Cold Turkey binary was installed or inspected.
- Cold Turkey research is limited to official public documentation. It describes a background Windows service and administrator setup plus website/app blocking, schedules, locks, and restart-related options, but does not disclose the implementation details needed to reproduce its internals. See `docs/research/cold-turkey-public-evidence.md`.
- Focuser 0.11.5 was audited at `14cbfdb3d55d3bc669bc9a39a6eddb9915e61844`. Its Windows enforcement loop is in the desktop process and its installer uses a highest-privilege logon Scheduled Task. Open Turkey is an MIT Linux reference. Lockdown had no root license in the reviewed snapshot and was not reused. See `docs/research/alternatives-audit.md`.
- PublikHQ setup requirements were read. Preparing a final listing plan and submitting the listing require a review step; no PublikHQ listing or release has been submitted.

### Implemented in this checkout

- Rebranded the desktop app, extension, CLI data path, configuration export, updater endpoint, installer task, Linux unit, and selected generated metadata to FocusGuard / PublikHQ.
- Changed the loopback extension API to port 17593 and replaced wildcard CORS with validation for supported browser-extension origins. Unit tests cover rejecting normal web origins and reflecting only a validated extension origin. This does not authenticate another local process running as the same user.
- Removed the unused native-messaging sidecar from installer wiring and trimmed its extension permissions. The old crate source remains in the workspace but is not registered or packaged by the installer.
- Added project attribution, third-party notices, and a manifest-level Cargo/npm license inventory. Target-specific notices for distributable binaries remain a release gate.
- Kept user-visible lock wording honest: current Windows behavior refuses normal app quit while locked; no Windows service has been implemented.
- Verification on this checkout: `cargo test --workspace --locked --quiet` passed all Rust test groups and doctests; `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo build --workspace --locked` passed. The desktop frontend passed 279 tests and a production Vite build. The extension passed typecheck, 137 tests, and Chromium/Firefox MV3 builds.
- Frontend Biome lint initially surfaced Windows line-ending formatting drift only; `biome lint` found no code-rule violations. Frontend files were formatted, LF endings are declared in `.gitattributes`, and `npm run lint`, all 279 frontend tests, and the production build pass afterward.
- First public CI run `42425408207`: extension, frontend, binding-drift, and all three platform build jobs passed. Windows Rust tests/Clippy/format also passed. Ubuntu and macOS test jobs found two stale platform-specific uninstall fixtures from the rebrand (old macOS bundle ID and a CLI path outside the actual Linux bundle); these fixtures are corrected and awaiting the next CI run.
- One Rust API test initially failed because a custom HTTP helper omitted the now-required extension Origin. The helper was corrected to model extension requests, the explicit website-origin denial test still overrides it, and the complete Rust suite passed afterward.
- Strict Clippy surfaced an inherited boolean simplification in `crates/focuser-ui/src/closing.rs`; the equivalent predicate was simplified and strict Clippy then passed.

### Current limits and required evidence

- Windows blocking still runs inside the desktop process. The elevated scheduled task is a logon launcher, not a Windows Service Control Manager service. Terminating the app stops the enforcement loop; hosts entries written before termination may persist until cleaned up. This is not yet a strict or crash-resilient blocker.
- No installer has been run, no service has been installed, and no hosts, firewall, or other system-wide policy has been changed on the development machine. Runtime behavior still needs a disposable Windows VM, including app kill/crash, reboot, task recovery, browser coverage, lock expiry, and uninstall/recovery checks.
- The local extension API's origin checks are not a defense against native software that can call loopback directly. Browser integration and same-user local-process tests remain outstanding.
- A genuine service design needs a least-privilege reviewed IPC boundary, durable policy source, restart recovery, uninstall cleanup, and explicit administrator recovery paths. Do not claim administrator-resistant enforcement.
- The exact Windows-target dependency notice bundle, signed installer, reproducible CI artifacts, and PublikHQ-ready listing have not been completed. No release has been published.
- A focused source review confirmed that service integration must move database/command ownership and the extension API out of the UI together; otherwise UI shutdown or the existing loopback endpoint remains a bypass. The detailed phased design and its pipe/storage/installer gates are in `docs/design/windows-service.md`.
- The host has no `New-VM`, `VBoxManage`, `vmrun`, or `WindowsSandbox` command available. The virtualization layer is present, but no VM management tool is available in this session. No app installer, service, hosts, or firewall behavior has been exercised in an isolated Windows VM.

## 2026-11-07 — repository scaffold and direction

- Created the public repository and added project guidance adapted from the four principles in [multica-ai/andrej-karpathy-skills](https://github.com/multica-ai/andrej-karpathy-skills/blob/main/CLAUDE.md), with attribution in `AGENTS.md`.
- Established the project direction: use the MIT Focuser baseline for its broad rules, schedule, allowance, UI, and extension features, then make Windows enforcement service-owned before claiming reliability.
- Read the PublikHQ setup guide and recorded its approval requirement. No listing was submitted.
