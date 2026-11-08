# Windows enforcement service design

Status: first Windows service-host milestone. `focuser-app::protocol` defines typed request/response envelopes, bounded synchronous framing, and Tokio async framing with a 1 MiB payload cap. `crates/focuser-service` now has an SCM entry point, a console test mode, and a local byte-mode named-pipe server. Its only supported request is `AppVersion`; mutations are rejected before they reach a headless `NoopSync` context. The service is not wired into the installer or desktop client, owns no durable policy, and runs no enforcement. Pipe tests pass on the development Windows host. The service was not installed, and no machine-wide policy was changed.

## Why a service is required

The current UI process opens the SQLite database in `crates/focuser-ui/src/main.rs`, builds `AppContext`, then starts the blocker, API server, and foreground watcher as threads. `crates/focuser-ui/src/blocker.rs` performs hosts synchronization, app-process enforcement, extension monitoring, allowance work, and uninstall monitoring. Closing or crashing that process stops those checks. The highest-privilege Scheduled Task in the NSIS installer is a logon launcher; it is not an SCM service and does not restart a process that is killed after login.

`crates/focuser-common/src/ipc.rs` already contains service-shaped messages, but there is no service server. The legacy native-messaging crate is not the service transport: it targets a loopback port and its protocol must not be reused unchanged.

## Smoke host

On Windows, run `cargo run -p focuser-service -- --console` to start the local smoke host as the current user; Ctrl+C stops it. Its named pipe accepts only the `AppVersion` probe. The executable also has an SCM dispatcher, but this repository has no service installer action or service account configuration yet. Do not treat console mode as elevated enforcement. `cargo test -p focuser-service --locked` exercises the local pipe ACL, probe, denied mutation, mismatched caller SID, invalid frame, and first-instance behavior on Windows.

## Ownership model

1. A Windows SCM service owns the policy database, rule engine, lock state, schedules, allowances, hosts changes, process checks, and browser-extension API. It restores policy from committed state before accepting requests after boot or recovery.
2. The desktop app runs unelevated and becomes a typed client. On Windows it must not keep a writable policy database or run a parallel enforcement loop. If the service is unavailable, the UI reports that blocking is unavailable; it must not silently claim enforcement or fork a second policy owner.
3. The first service install records one owner SID and uses one policy database under a service-controlled `ProgramData\PublikHQ\FocusGuard` directory. The UI has no direct write access. The service authenticates the pipe client SID and maps requests only to the configured owner's database and process session. Requests from other Windows users are denied; multi-user policy is deferred until cross-session isolation is implemented and tested.
4. The browser extension continues using the loopback API so URL/path rules can work while the UI is closed. The service hosts the API. It accepts only the installed FocusGuard extension origin for browser access, validates every request, and rejects mutations that conflict with a stored lock. Origin validation is not authentication against another local process; any endpoint whose misuse could weaken a lock needs a per-user credential or must not be exposed over HTTP.

The database move requires a versioned migration from the current per-user app data path. The service must perform the migration once, verify it, and preserve a recoverable backup before switching authority. No data migration is planned from upstream Focuser installs.

## Local IPC boundary

- Use a local named pipe with remote clients rejected. The prototype pipe DACL gives the service process, LocalSystem, and administrators full pipe control, and gives the configured owner SID the individual data/attribute rights needed by clients without `FILE_CREATE_PIPE_INSTANCE`. It verifies the impersonated caller's user SID on each connection rather than trusting a pipe name. Administrator access is intentionally allowed.
- Supply an explicit pipe security descriptor. Microsoft documents that a default named-pipe DACL grants read access to Everyone and the anonymous account. Where the product must isolate terminal sessions, include the active logon SID in the authorization decision; a user SID alone spans that user's sessions. Reject remote clients at pipe creation and claim the first pipe instance to prevent remote access and pipe-name squatting.
- Use byte-mode pipes for the protocol's length-prefixed byte stream. The prototype uses overlapped Tokio handles, a five-second frame deadline, four pipe instances, and at most three request workers. A separate Windows client open uses individual access bits because generic write maps to `FILE_CREATE_PIPE_INSTANCE`.
- The prototype checks the user SID, but not the logon SID. Multiple terminal sessions belonging to the same Windows account are therefore not isolated yet. With an identification-level client token, the SCM service account must have `SeImpersonatePrivilege` when it differs from the caller; the local console tests use the same identity and do not verify that service-account path. Verify this and fail service startup clearly if the configured account cannot authenticate clients.
- Use a versioned typed protocol with a strict maximum message size. `focuser-app::protocol` caps serialized payloads at 1 MiB and returns a correlated `response_too_large` error instead of dropping an oversized result. Queries and exports that can exceed this cap still need pagination or bounded ranges before the UI moves to the service.
- Expose only reviewed commands through the service dispatcher. The current allowlist excludes the UI-only app-icon loader, which resolves caller-supplied filesystem paths; future command variants are denied until reviewed. Never expose arbitrary SQL, filesystem paths, shell commands, or a service-stop command.
- Every client request is untrusted. The service checks persisted lock state and validates fields before committing a transaction. It commits durable state before applying external effects, then reports both policy and enforcement status to the UI.
- Browser HTTP and UI pipe requests converge on the same service-side command layer. The browser endpoint cannot be a second policy implementation.

## Privileges and enforcement

The service needs enough authority to update the shared Windows hosts file and inspect or stop blocked processes in its supported sessions. The service account is an open design decision: prototype the required operations under LocalService with only explicit service-specific access, and compare with LocalSystem plus a restricted service SID. Select the least-privileged account that passes the complete VM test matrix; do not assume account choice from development builds.

Begin with service-owned hosts-file domain fallback and per-session process checks. Hosts edits are marked, deterministic, and idempotent; cleanup removes only the FocusGuard section. URL paths, search patterns, and page rules remain extension features. Hosts filtering does not defeat DoH, VPNs, proxies, alternate browsers, or local administrators.

Do not add a kernel driver to the first service milestone. Evaluate user-mode Windows Filtering Platform integration separately after service lifecycle, ACLs, recovery, and rollback are proven. WFP rules still do not provide URL path semantics and any persistent filter needs a tested removal path.

## SCM lifecycle and installer

- Install/configure/start the service only from an elevated installer action. Set service DACLs so a standard user cannot stop, reconfigure, or delete it; administrators remain able to recover the machine.
- Configure bounded SCM recovery restarts and emit useful event-log diagnostics. At start, load and validate committed state, reapply hosts policy, initialize process/session tracking, then accept pipe/API clients.
- On graceful stop, preserve active policy while a lock remains. On uninstall or explicit recovery, use a separate admin-authorized cleanup action that removes only FocusGuard-owned artifacts. Preserve the database unless the user selects deletion.
- Installer upgrade rollback and uninstall must handle partial install, a running service, stale hosts entries, corrupt database, and service startup failure. The old logon task must not run alongside the service after migration.

## Implementation sequence

1. The shared command layer and lock-preserving mutation tests already live in `focuser-app`; its `protocol` module supplies typed, versioned, bounded wire frames. Async framing and transport-level peer authorization now exist; expand identity/session and lifecycle coverage.
2. Add a Windows-only service executable and named-pipe server; this smoke-host step is implemented and tested without installing it on the development host. It is not yet an enforcement service.
3. Move the loopback API and enforcement workers into the service; make the UI a client and remove Windows UI-owned host/process enforcement.
4. Add service installation, ACLs, recovery actions, data migration, and admin cleanup to the installer only after the service and pipe tests pass.
5. Run the full installer and behavior matrix in a disposable Windows VM. Treat each supported Windows version, standard-user boundary, and administrator boundary as separate evidence.

## Required VM checks before a release

- A clean install starts the service before login; reboot restores the same locked policy before the UI launches.
- The UI may close or crash without stopping enforcement. Forced service crash causes SCM recovery and safe policy reapplication.
- A standard user cannot stop/reconfigure/delete the service, edit service-owned state, or connect to the pipe from another SID. Administrators can use documented recovery.
- Pipe framing, oversized input, malformed commands, unauthorized SIDs, duplicate clients, timeout, and service shutdown are tested.
- Extension blocking works with the UI closed; missing extension and unsupported/private browser behavior is visible and accurately described.
- Schedule boundaries, clock changes, sleep/resume, lock expiry, DNS cache, DoH/VPN/proxy, alternate browser, and app path/rename cases are exercised.
- Upgrade rollback, corrupt database recovery, service removal, and hosts cleanup preserve unrelated entries and do not strand users offline.

## Security limit

This design aims to keep enforcement alive and make casual bypass harder for a standard user when an administrator has installed and configured the service. It cannot protect the device from its local administrator or owner, alternate boot media, another operating system, or physical access. A service is a reliability boundary and a privilege boundary; it is not tamperproof security.

## Platform references

- [Windows Services](https://learn.microsoft.com/en-us/windows/win32/services/about-services)
- [Service security and access rights](https://learn.microsoft.com/en-us/windows/win32/services/service-security-and-access-rights)
- [Named pipe security and access rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- [Named pipe client access modes](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-open-modes)
- [Impersonating a named pipe client](https://learn.microsoft.com/en-us/windows/win32/ipc/impersonating-a-named-pipe-client)
- [Windows Filtering Platform overview](https://learn.microsoft.com/en-us/windows/win32/fwp/about-windows-filtering-platform)
- [WFP basic operation](https://learn.microsoft.com/en-us/windows/win32/fwp/basic-operation)
