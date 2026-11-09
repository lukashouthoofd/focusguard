# macOS enforcement and parity plan

Status: partial implementation. The app now has an SMAppService registration/status panel, and the macOS bundle configuration stages a LaunchDaemon plist and `focusguard-service`. The root daemon creates protected support paths, records the first console user, migrates that user's database with SQLite's backup API, and exposes the existing peer-UID-authenticated Unix socket. The workflow is configured to package unsigned DMGs for Apple Silicon and Intel. These Mac-specific paths have not yet been built or exercised on a Mac, and unsigned DMGs are not user-ready. This document tracks the remaining work for persistent macOS enforcement and full Blocker feature parity. It does not authorize installing a helper or changing a development machine.

## Evidence boundary

- FocusGuard's [public feature ledger](../research/cold-turkey-public-evidence.md) is the functional reference. The installed Cold Turkey 4.9 binary's REA evidence is limited to static PE/CLI identity and selected metadata; it does not establish runtime behavior. Continue from public product and operating-system documentation, implement original code/assets, and do not inspect licensing or entitlement logic.
- The desktop app can still enforce locally while unregistered. The bundled helper is intended to keep exact-domain hosts rules active after the window closes. Website host rules require administrator approval. Browser URL/path matching remains an extension responsibility, and the helper's current command allowlist is domain-focused.
- Apple's `SMAppService` manages app-bundled LoginItems, LaunchAgents, and LaunchDaemons starting with macOS 13. A LaunchDaemon runs as root and does not interact directly with the user's UI. Registering one requires administrator approval in System Settings; the UI and daemon therefore need a narrow IPC boundary. See [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice), [register()](https://developer.apple.com/documentation/servicemanagement/smappservice/register()), and Apple's [launchd.plist reference](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5).

## Required product behavior

1. **Keep policy enforcement independent of the window.** Closing or crashing the UI must not silently cancel an active block. Persisted schedules must reconcile after login, wake, helper restart, and an app update.
2. **Keep the UI in the user's session.** Notifications, warnings, break prompts, app-close grace periods, and recovery controls belong to a per-user process; a system daemon must not try to display windows.
3. **Use least-privilege IPC.** The helper may apply only validated FocusGuard policy operations. Authenticate the caller, bound messages and responses, reject remote/unowned clients, and keep an auditable uninstall/recovery path.
4. **Make privilege explicit and reversible.** Registering a system helper requires the user's administrator approval. Uninstall removes only FocusGuard-owned launchd registrations and marked hosts entries. Never claim administrator-resistant or tamperproof enforcement.
5. **Retain browser boundaries.** Exact domain hosts rules cannot enforce URL paths, search keywords, YouTube channels, or embedded-page rules. Continue to route those to the optional browser extension and disclose when its required permission/presence is missing.

## Implementation stages

### A. Shared policy parity

- Give the desktop UI and platform helper one shared, versioned policy model for websites, exceptions, applications, schedules, locks, breaks/allowances, and strictness settings.
- Bring Windows service UI/IPC beyond its current exact-domain screen and limited executable-name enforcement. Preserve the richer desktop rule semantics instead of silently dropping unsupported records during service-mode migration.
- For macOS, enforce supported app rules by stable bundle identifier and display a user-session warning before asking the helper to close a process. Keep title/path matching limitations explicit where the OS does not provide a stable identity.

### B. macOS helper lifecycle

- Bundle the helper and its LaunchDaemon plist in the app. The declared minimum is now macOS 13, where `SMAppService` is available; registration status and Login Items navigation are exposed in the UI. Still required: build and exercise the bundle on both Mac architectures, verify signing requirements, and test approval, denial, disable, update, and recovery.
- Keep privileged operations in a system LaunchDaemon and user interaction in a LaunchAgent/UI process. A user-level LaunchAgent alone can restart the app after abnormal exit, but it is not a substitute for privileged, durable policy enforcement.
- Make registration, approval denial, disabled-helper status, update, crash recovery, and uninstall observable in the UI. Do not treat “plist exists” as proof that launchd approved or loaded the service.

### C. Full documented Blocker surface

Track each row against [the public evidence ledger](../research/cold-turkey-public-evidence.md), including website domains/URLs/wildcards/search/channel rules and exceptions; app paths/folders/store apps/window titles or macOS bundle IDs; weekly and timed schedules/autostart; timer/time-range/schedule/restart/password/text/delay locks; Pomodoro, allowances, rewards, and sessions; Frozen Turkey device actions; usage statistics/export; affected users; settings protection; browser requirements; and strictness controls. Mark features supported only after a source path and behavior test exist on the target platform. Never infer runtime support from a type name or a successful package build.

## Release and verification gates

- Rust unit and property tests for policy validation, schedules, lock expiry, recovery, migration, and host-file round trips.
- macOS CI build on supported Intel and Apple Silicon runners, including assertions that the helper and plist are in the app bundle. Cross-compiling an Apple target from Windows is not a substitute for an Xcode/SDK build.
- Disposable macOS VM tests for helper approval/denial, close/crash/reboot/wake recovery, multi-user isolation, hosts restoration, app-rule warning/close behavior, update, and clean uninstall. Do not run privileged lifecycle tests on a developer's primary machine.
- Signed and notarized user-facing app bundle/installer, with a reproducible release workflow and complete target-specific dependency notices. Current DMGs are unsigned; this gate is open.
- For PublikHQ, derive the listing from the finished repository and release artifact. FocusGuard is local-first and currently has no AI/search API requirement, so do not add a Publik API integration unless a later feature actually needs one. Follow the Publik setup flow's single approval step before creating or publishing a listing.
