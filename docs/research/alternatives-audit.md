# Open Source Alternatives Audit

Audit date: 2026-11-07; implementation status updated 2026-11-08. The original comparison was based on read-only shallow clones. After the audit, FocusGuard imported the Focuser 0.11.5 baseline under its MIT license; see [upstream attribution](../upstream-focuser.md). The notes below describe that audited revision, while the follow-up status records FocusGuard changes.

## Focuser

- Repository: [aadeshrao123/Focuser](https://github.com/aadeshrao123/Focuser)
- Reviewed revision: `14cbfdb3d55d3bc669bc9a39a6eddb9915e61844` (release 0.11.5, 2026-11-04)
- License: MIT (`LICENSE`, copyright notice for Focuser Contributors)
- Stack: Rust workspace, Tauri v2/React UI, SQLite, WXT browser extension.
- README says Windows 10/11 is tested and lists block lists, URL/domain/keyword/path rules, exceptions, scheduled blocks, Pomodoro, allowances, app matching by name/path/window title, statistics, browser extension enforcement, and locks.

### Source observations

- `crates/focuser-ui/src/blocker.rs`: one loop in the desktop process runs a one-second tick and heavier enforcement about every three seconds. It syncs the hosts file when no extension is connected, checks processes, enforces browser extension presence, and monitors uninstall attempts during protected blocks.
- `crates/focuser-ui/src/main.rs`: starts that blocking loop as a thread in the UI application.
- `crates/focuser-ui/src/autostart.rs` and `crates/focuser-ui/nsis/installer-hooks.nsi`: Windows startup is a logon Scheduled Task that launches the UI executable with highest privileges. Source search found no Windows SCM service implementation. The similarly named service protection is a lock setting that prevents selected app actions; it is not evidence of a Windows service.
- `extension/README.md`: extension handles paths, wildcards, and keywords; it fails open if its app API does not answer within two seconds. When disconnected, it reports that blocking is inactive.
- In the reviewed upstream revision, `crates/focuser-ui/src/api.rs` bound `127.0.0.1:17549`, allowed `Access-Control-Allow-Origin: *`, and exposed website and list mutation routes. This was a source-level concern, not a verified exploit.
- `crates/focuser-ui/src/blocker.rs`: app blocking and the hosts re-sync loop live in the same process. If that process is terminated, those checks stop until it starts again. Host entries already written can remain until cleanup or restart.

### Assessment

Focuser is a strong product baseline with broad user-facing features and an MIT license, but its Windows enforcement loop is coupled to the desktop application. Its scheduled logon launch does not establish persistence after process termination. Domain blocking via hosts does not cover URL paths; that relies on an extension. The local API and per-user state need a security review before carrying them into a service-backed design.

### FocusGuard changes and current evidence

- The imported 0.11.5 source retains its MIT notice and is explicitly attributed. The old native-messaging crate remains in the source tree for now, but the current extension uses the loopback HTTP API and the native host is not registered or packaged by the installer.
- FocusGuard changes the local API to port 17593 and rejects ordinary web origins. For validated Chrome/Firefox extension origins, it reflects only that origin instead of using a wildcard. This is covered by unit tests; it is not an authentication boundary against other local processes, and no browser integration or adversarial local-process test has been run.
- Product identifiers and data directories are being moved to FocusGuard / PublikHQ. No migration from an installed Focuser product is supported or tested.
- These changes do not resolve the central Windows limitation: blocking still runs in the desktop process and startup uses a highest-privilege logon task. A crash/termination test and service-owned enforcement are outstanding.

## Open Turkey

- Repository: [bdcdo/open-turkey](https://github.com/bdcdo/open-turkey)
- License: MIT (`LICENSE`)
- Stack and source layout: Go CLI/daemon, `systemd`, `/etc/hosts`, `iptables`, browser enterprise policy, and process termination. The README says the daemon reapplies its layers every five seconds.
- Installation source edits system-wide files, creates a sudoers rule, and installs/starts a systemd unit. The checked-in implementation is Linux-specific and not a Windows base. Its layering is useful as an independent design reference only.

## Lockdown

- Repository: [JimmyMusyoki/Lockdown](https://github.com/JimmyMusyoki/Lockdown)
- License: no `LICENSE` file was present in the reviewed repository root. Do not reuse its code unless the author supplies a license.
- Stack and source: Electron/Node Windows app. README and modules describe hosts-file rules, process polling/termination, a timer, and a boot-time SYSTEM scheduled agent.
- Scope includes LAN screen viewing, file sharing, remote machine management, and updates. The README says screen viewing is not prompted per request. Those capabilities expand attack surface and do not belong in FocusGuard's core blocker.

## Decision

Build FocusGuard on Focuser's MIT-licensed source baseline instead of recreating its schedules, rules, allowance model, extension, and UI. Retain Focuser's license notice and add clear upstream attribution. The Windows enforcement design must be changed: the service, not the tray/UI process, must own persistent policy and reapply it after crashes/restarts. The current code is only a starting point, not evidence of strict protection.

Open Turkey will inform independently written Windows designs. Lockdown source will not be reused. Dependency manifests have been inventoried; target-specific binary notices remain a release gate.

## Follow-up checks

- Build and run the upstream unit/CI suite on this Windows host.
- Audit all direct and transitive dependency licenses before releasing a derivative.
- Inspect Focuser's local API authorization and per-user database boundaries in detail.
- Prototype the Windows Service and named-pipe boundary without altering the host's installed services.
- Verify behavior in a disposable VM before enabling real host protection.
