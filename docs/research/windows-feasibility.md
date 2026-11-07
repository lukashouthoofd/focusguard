# Initial Windows Architecture and Feasibility

## Design direction

The implementation sequence and service boundary are detailed in [the Windows service proposal](../design/windows-service.md). This note records initial feasibility and platform constraints.

Use Focuser's rule engine, schedule/allowance model, interface, and browser extension as the MIT starting point, then separate Windows enforcement from the interactive app.

- **Policy owner:** Windows Service with automatic startup and SCM recovery actions; treat the account and service ACL as a real security boundary, not a hidden process.
- **User interface:** unprivileged per-user tray/desktop process. It sends typed requests through an ACL-restricted named pipe. The service checks every mutation against persisted lock policy.
- **State:** service-owned data under ProgramData with explicit ACLs. Persist schedules/locks before applying policy; restart recovery derives and reapplies enforcement from the committed state.
- **Websites:** retain a browser extension for URL paths, search patterns, and page-level rules. Use the hosts file only for domain-level fallback. Consider a native-messaging broker to the service instead of an unauthenticated loopback API.
- **Applications:** service-side process monitoring with gentle close, bounded grace, then termination. Do not terminate unrelated processes based on fuzzy matches.
- **Recovery:** installer records exactly what it changes; uninstall/recovery restores only FocusGuard-owned entries and offers an admin recovery command. A service crash must not leave an unexplainable state.

Windows Services, the Service Control Manager, and Windows Filtering Platform are feasible building blocks. WFP can persist filters and support boot-time filters, but domain/URL semantics still require browser integration or DNS-aware rules. A custom kernel driver is out of scope for the initial release because signing, compatibility, and recovery costs are high.

## Security boundary

FocusGuard can meaningfully resist casual changes from a standard user account when an administrator installs the service and restricts its service-control permissions. It cannot prevent the local device owner or an administrator from disabling the service, changing policy/firewall state, booting alternate media, or reinstalling Windows. The UI and product copy must say this plainly.

Any service install, hosts/WFP modification, firewall rule, task, registry change, or user-policy change will be shown before it is applied and must have an explicit rollback path. The checked-in development installer currently creates a highest-privilege logon Scheduled Task, and the app can write a marked hosts-file section when enforcement runs. Neither has been run on the development host. No Windows service or WFP/firewall policy has been installed. Run those paths only in a disposable VM until the service design and recovery path are tested.

## Test matrix

Test normal allow/block, overlaps and exceptions, scheduled boundaries, DST/time changes, sleep/resume, service startup, service crash/recovery, UI closure, extension loss, browser private mode, DNS cache, DoH, VPN/proxy, portable/renamed apps, uninstaller attempts, standard user vs administrator, Safe Mode, and rollback/uninstall. Use an isolated VM for service, hosts, WFP, and installer tests. In ordinary development, use a test hosts file and child processes owned by the test harness.

## Official platform references

- [Windows Services](https://learn.microsoft.com/en-us/windows/win32/services/services)
- [Service security and access rights](https://learn.microsoft.com/en-us/windows/win32/services/service-security-and-access-rights)
- [Windows Filtering Platform overview](https://learn.microsoft.com/en-us/windows/win32/fwp/about-windows-filtering-platform)
- [WFP basic operation](https://learn.microsoft.com/en-us/windows/win32/fwp/basic-operation)
- [Chrome Declarative Net Request API](https://developer.chrome.com/docs/extensions/reference/api/declarativeNetRequest)

These are implementation options, not proof that FocusGuard currently provides any of them.
