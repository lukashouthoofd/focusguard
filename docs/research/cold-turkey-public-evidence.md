# Public Cold Turkey Evidence Ledger

Date checked: 2026-11-07. Evidence is limited to public product documentation and official Windows documentation. No Cold Turkey executable was installed, executed, or decompiled. Product-documentation claims are not treated as independently tested runtime behavior.

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

Cold Turkey's current Terms page (effective 2026-10-15) says software use is governed by an EULA that may be presented during installation, and its licence terms restrict evaluating copies by others. We therefore did not install or inspect the proprietary executable. We rely on published product documentation and implement against Windows APIs with original code and assets. We do not access Pro-only behavior by bypassing licensing and do not copy proprietary code.

EU Directive 2009/24/EC protects program expression while stating that underlying ideas and principles are not protected by copyright. Its decompilation exception is narrow and concerns interoperability, with express limits on using obtained information to develop a substantially similar expression or infringe copyright. We are not relying on that exception. This is a product-engineering boundary, not legal advice; counsel should review any later binary analysis, trademark use, or distribution questions.

## Sources

- [Cold Turkey Features](https://getcoldturkey.com/features/)
- [Cold Turkey User Guide](https://getcoldturkey.com/support/user-guide/)
- [Cold Turkey Support FAQ](https://getcoldturkey.com/support/)
- [Cold Turkey Terms](https://getcoldturkey.com/terms/)
- [EU Directive 2009/24/EC, Articles 1 and 6](https://eur-lex.europa.eu/legal-content/en/TXT/?uri=CELEX%3A32009L0024)

## Evidence status

- Publicly documented functionality: **observed in documentation**.
- Windows service existence: **asserted by official documentation, not runtime-tested**.
- Internal service and filtering design: **unknown**.
- Anti-bypass resilience: **not independently verified**.
