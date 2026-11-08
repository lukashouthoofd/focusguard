# Public Cold Turkey Evidence Ledger

Public documentation checked: 2026-11-08. This review is limited to official product documentation and operating-system documentation. Product-documentation claims are not treated as independently tested runtime behavior. Separate REA static metadata evidence for a pre-existing local executable is documented in [cold-turkey-rea-evidence.md](cold-turkey-rea-evidence.md).

## Separate REA artifact evidence

On 2026-11-08, REA recorded read-only static metadata inspections of a pre-existing local `Cold Turkey Blocker.exe`. The artifact identity Evidence ID is `ev_1b58188d8da15bdea285229bc04b4b28d72eb08151494d5cb45d38622eb9b909`; its SHA-256 is `5bc434b593d219d3686a403296d02e3d163234504900ce94fa04a6b2e982d509`. REA reports a 64-bit .NET Framework 4.8 managed assembly, SQLite and WinForms references, and member names associated with blocks, schedules, timers, app/web settings, Task Manager, time-change, and strict-password options. These are static metadata observations only. They do not establish runtime behavior, service architecture, persistence, filtering, or anti-bypass strength. See [the REA evidence record and limitations](cold-turkey-rea-evidence.md).

No licensing or entitlement mechanism was analyzed, no paid-only behavior was accessed, and the executable was not launched. The checked-in member record is a selected projection, not the full canonical REA Evidence bundle.

## Publicly documented behavior

| Area | Documented behavior | Evidence type |
|---|---|---|
| Website rules | Domains, specific URLs, wildcards, search terms, channel rules, exceptions, category lists, whole-internet blocking | Official features page and user guide |
| App rules | Windows executable paths, folders, Microsoft Store apps, and window titles. The features page says folder rules also prevent browsing those folders. | Official features page and user guide |
| Scheduling | Continuous blocks or recurring weekly schedules; the guide says scheduled blocks require the block toggle to remain enabled. The same block list cannot contain overlapping scheduled blocks. Autostart options include a one-time time, time of day, sign-in/wake, or the next scheduled block. | Official features page and user guide |
| Breaks | Pomodoro, allowances (rolling or calendar-period limits), rewards, sessions, and delay or text-entry gates. | Official features page |
| Locks | Timer locks, time-range locks, schedule locks, restart locks, passwords, and random/custom text. The features page also lists a delay before unlocking. The guide says the timer can be extended and distinguishes continuous-block restart locks from scheduled-block locks. | Official features page and user guide |
| Lock side effects | The guide says a locked block prevents the uninstaller from proceeding, requires the browser extension while browsers are used, disables some Blocking settings, and blocks some alternate browser modes. These are published claims, not independently tested behavior. | Official user guide |
| Device controls | Device blocks can lock, log off, or shut down the computer. | Official features page and user guide |
| Strictness | Published toggles include unsupported-browser blocking, closing supported browsers after extension/process failure, blocking system-time changes, blocking Task Manager, inactive tabs, embedded content, the installer, and `file://` access. The guide says strictness choices cannot be changed during locked blocks. | Official features page and user guide |
| Statistics and data | Statistics cover blocked visits and app/web usage. The features page says settings/statistics are stored locally and statistics can be deleted or exported as JSON. | Official features page |
| Paid/free boundary | The pricing page lists websites, exceptions, timed blocks, and statistics as Free features; app blocking, schedules, application password, all locking features, user selection, breaks, and allowances are Pro features. We did not access or test paid-only behavior. | Official pricing page |
| Browser requirements | Browser extensions need broad site access; incognito permission is called out in setup and troubleshooting | Official user guide |
| Windows service | The guide says application blocking uses a background service and documents reinstalling it with `CTServiceInstaller.exe`. It does not specify account, startup type, ACLs, recovery actions, or the website-blocking path. | Official user guide only; no independent runtime confirmation |
| Administrator rights | The user guide says installation requires administrator rights | Official user guide |

The official pages disagree on the maximum random-text lock length: the features page says 1–5,000 characters, while the user guide says 1–999 randomized characters. This may reflect different versions or separate lock modes; the documentation alone does not resolve it.

## What remains unknown

Public documentation does not specify the Windows service account, startup type, service ACL, recovery actions, exact website filtering implementation, persistence format, policy behavior during a service crash, or the limits of each anti-bypass option. It documents app blocking via a background service and browser extensions for website behavior; that does not establish the full enforcement architecture or what survives if either component stops. The user guide's mention of the hosts file for a separate Safe Search setup does not establish that hosts-file editing is the primary website-blocking mechanism.

The marketing phrase "almost impossible" is not a security property. We have no evidence that the product resists a local administrator, alternate boot media, or a separate operating system.

## Legal and research boundary

Cold Turkey's current Terms page (effective 2026-10-15) says software use is governed by an EULA that may be presented during installation, and states that users must not allow others to evaluate copies. The EULA accepted for the pre-existing local copy is unavailable here, and this project does not assess the Terms' enforceability. We did not install or launch that executable; the limited REA work was static metadata inspection only. Further binary or non-public runtime analysis is held pending legal review. We rely on published product documentation and implement against Windows APIs with original code and assets. We do not access Pro-only behavior by bypassing licensing and do not copy proprietary code.

EU Directive 2009/24/EC protects program expression while stating that underlying ideas and principles are not protected by copyright. Its decompilation exception is narrow and concerns interoperability, with express limits on using obtained information to develop a substantially similar expression or infringe copyright. We are not relying on that exception. This is a product-engineering boundary, not legal advice; counsel should review any later binary analysis, trademark use, or distribution questions.

## Sources

- [Cold Turkey Features](https://getcoldturkey.com/features/)
- [Cold Turkey User Guide](https://getcoldturkey.com/support/user-guide/)
- [Cold Turkey Support FAQ](https://getcoldturkey.com/support/)
- [Cold Turkey Pricing](https://getcoldturkey.com/pricing/)
- [Cold Turkey Terms](https://getcoldturkey.com/terms/)
- [EU Directive 2009/24/EC, Articles 1 and 6](https://eur-lex.europa.eu/legal-content/en/TXT/?uri=CELEX%3A32009L0024)

## Evidence status

- Publicly documented functionality: **observed in documentation**.
- Executable identity/member metadata: **observed by REA static inspection; selected evidence preserved, with member projection limitations documented**.
- Windows service existence: **asserted by official documentation, not runtime-tested**.
- Internal service and filtering design: **unknown**.
- Anti-bypass resilience: **not independently verified**.
