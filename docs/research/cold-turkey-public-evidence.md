# Public Cold Turkey Evidence Ledger

Public documentation checked: 2026-11-07. That review was limited to official product documentation and Windows documentation. Product-documentation claims are not treated as independently tested runtime behavior. Separate REA static metadata evidence for a pre-existing local executable is documented in [cold-turkey-rea-evidence.md](cold-turkey-rea-evidence.md).

## Separate REA artifact evidence

On 2026-11-08, REA recorded read-only static metadata inspections of a pre-existing local `Cold Turkey Blocker.exe`. The artifact identity Evidence ID is `ev_1b58188d8da15bdea285229bc04b4b28d72eb08151494d5cb45d38622eb9b909`; its SHA-256 is `5bc434b593d219d3686a403296d02e3d163234504900ce94fa04a6b2e982d509`. REA reports a 64-bit .NET Framework 4.8 managed assembly, SQLite and WinForms references, and member names associated with blocks, schedules, timers, app/web settings, Task Manager, time-change, and strict-password options. These are static metadata observations only. They do not establish runtime behavior, service architecture, persistence, filtering, or anti-bypass strength. See [the REA evidence record and limitations](cold-turkey-rea-evidence.md).

No licensing or entitlement mechanism was analyzed, no paid-only behavior was accessed, and the executable was not launched. The checked-in member record is a selected projection, not the full canonical REA Evidence bundle.

## Publicly documented behavior

| Area | Documented behavior | Evidence type |
|---|---|---|
| Website rules | Domains, specific URLs, wildcards, search terms, channel rules, exceptions, category lists, whole-internet blocking | Official features page and user guide |
| App rules | Windows executable paths, folders, Microsoft Store apps, and window titles | Official features page and user guide |
| Scheduling | Recurring weekly schedules, on-demand timed blocks, and automatic starts | Official features page and user guide |
| Breaks | Pomodoro, allowances, rewards, sessions, and delay or text-entry gates | Official features page |
| Locks | Timer, schedule/time range, restart, password, and randomized/custom text options | Official features page and user guide |
| Device controls | Lock, log out, or shut down the computer | Official features page and user guide |
| Strictness | Options documented for inactive tabs, embedded content, Task Manager, and time/date settings | Official features page and support FAQ |
| Browser requirements | Browser extensions need broad site access; incognito permission is called out in setup and troubleshooting | Official user guide |
| Windows service | The user guide says Windows app blocking is handled by a background service and documents a service installer repair path | Official user guide only; no independent runtime confirmation |
| Administrator rights | The user guide says installation requires administrator rights | Official user guide |

## What remains unknown

Public documentation does not specify the Windows service account, startup type, service ACL, recovery actions, exact network filtering implementation, persistence format, policy behavior during a service crash, or the limits of each anti-bypass option. The user guide's mention of the hosts file for a separate Safe Search setup does not establish that hosts-file editing is the primary website-blocking mechanism.

The marketing phrase "almost impossible" is not a security property. We have no evidence that the product resists a local administrator, alternate boot media, or a separate operating system.

## Legal and research boundary

Cold Turkey's current Terms page (effective 2026-10-15) says software use is governed by an EULA that may be presented during installation, and states that users must not allow others to evaluate copies. The EULA accepted for the pre-existing local copy is unavailable here, and this project does not assess the Terms' enforceability. We did not install or launch that executable; the limited REA work was static metadata inspection only. Further binary or non-public runtime analysis is held pending legal review. We rely on published product documentation and implement against Windows APIs with original code and assets. We do not access Pro-only behavior by bypassing licensing and do not copy proprietary code.

EU Directive 2009/24/EC protects program expression while stating that underlying ideas and principles are not protected by copyright. Its decompilation exception is narrow and concerns interoperability, with express limits on using obtained information to develop a substantially similar expression or infringe copyright. We are not relying on that exception. This is a product-engineering boundary, not legal advice; counsel should review any later binary analysis, trademark use, or distribution questions.

## Sources

- [Cold Turkey Features](https://getcoldturkey.com/features/)
- [Cold Turkey User Guide](https://getcoldturkey.com/support/user-guide/)
- [Cold Turkey Support FAQ](https://getcoldturkey.com/support/)
- [Cold Turkey Terms](https://getcoldturkey.com/terms/)
- [EU Directive 2009/24/EC, Articles 1 and 6](https://eur-lex.europa.eu/legal-content/en/TXT/?uri=CELEX%3A32009L0024)

## Evidence status

- Publicly documented functionality: **observed in documentation**.
- Executable identity/member metadata: **observed by REA static inspection; selected evidence preserved, with member projection limitations documented**.
- Windows service existence: **asserted by official documentation, not runtime-tested**.
- Internal service and filtering design: **unknown**.
- Anti-bypass resilience: **not independently verified**.
