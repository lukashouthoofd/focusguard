# Cold Turkey REA Evidence

Collected 2026-11-08 from a pre-existing local installation. This note records static PE/CLI metadata only; it is not a runtime reverse-engineering report.

## Artifact and tooling

- Artifact: `C:\Program Files\Cold Turkey\Cold Turkey Blocker.exe`
- Size: 2,030,072 bytes
- SHA-256: `5bc434b593d219d3686a403296d02e3d163234504900ce94fa04a6b2e982d509`
- REA package: `rea-agents` 6.3.0, provider `rea-dotnet-static` version 1
- The installed REA command describes `inspect-managed-artifact` as inspecting PE/CLI identity “without loading target code”; `inspect-managed-members` likewise says it does not load target code.

## Preserved evidence

- `ev_1b58188d8da15bdea285229bc04b4b28d72eb08151494d5cb45d38622eb9b909` — artifact identity and managed classification. The concise REA JSON view is [cold-turkey-rea-artifact-identity-20261108.json](cold-turkey-rea-artifact-identity-20261108.json).
- `ev_b29b148034ed158bb350768ed0ec89439c61eda2dd6551d38e53dfc7fd6080ea` — managed member inventory for the same path and SHA-256. [cold-turkey-rea-managed-members-20261108.json](cold-turkey-rea-managed-members-20261108.json) is a labeled projection containing selected names and signatures only; it omits CIL bodies, anchors, and all non-selected members. No licensing or entitlement logic was selected or interpreted.

The session history contains the first record's `--format toon --token-limit 3000` output and the member inventory's `--format toon --token-limit 7000` output; those console captures are token-limited. A later JSON export of the member result contained only `normalized_result.methods` (557 methods, 17,688,114 bytes) and no Evidence envelope. Its SHA-256 is `a8baa1b1efe6fbf8b64eaeb83ec5c9fa3256b4ab5667817f76454267c9f1ec79`. The checked-in member file is therefore explicitly a projection, not a claim that the full canonical Evidence bundle was recovered.

## Commands

The original static inspections were invoked through REA 6.3.0 with these commands (the member result was subsequently exported as JSON):

```powershell
node.exe rea.mjs inspect-managed-artifact "C:\Program Files\Cold Turkey\Cold Turkey Blocker.exe" --format toon --token-limit 3000
node.exe rea.mjs inspect-managed-members "C:\Program Files\Cold Turkey\Cold Turkey Blocker.exe" --format toon --token-limit 7000
node.exe rea.mjs inspect-managed-members "C:\Program Files\Cold Turkey\Cold Turkey Blocker.exe" --format json --filter-output normalized_result.methods
```

The checked-in artifact identity JSON was refreshed with a filtered `--format json` invocation using Evidence ID, subject, provider, operation, artifact, PE, classification, module, assembly, framework, and reference fields. No Cold Turkey application launch was performed for these records.

## Observations from static metadata

- REA classifies the file as a 64-bit managed PE/CLI image using .NET Framework 4.8; the assembly metadata reports version `4.9.0.0`.
- Assembly references include `System.Windows.Forms` and `Microsoft.Data.Sqlite`.
- Member names and signatures include properties on `Cold_Turkey.Block` for `Apps`, `Web`, `Schedule`, `Lock`, `Timer`, `Password`, `PomodoroTime`, and unblock/start-time values; `Cold_Turkey.Schedule` exposes start/end-time properties.
- Selected `Cold_Turkey.SubSettings` names cover documented settings such as Task Manager, time-change, installer, inactive-tab, embedded-content, unsupported-browser, and strict-password options.

These are names, signatures, assembly references, and PE/CLI facts. They do not show how a setting is enforced or prove that a feature succeeds at runtime.

## Unknowns and boundary

No runtime behavior was observed. These records do not identify the Windows service account, startup type, ACLs, restart recovery, website filtering path, persistence behavior, failure handling, or anti-bypass effectiveness. No service, helper, browser host, installer, or other Cold Turkey file was analyzed in this evidence task. No license or entitlement logic was inspected, and no paid-only behavior was accessed.

The current public [Cold Turkey Terms](https://getcoldturkey.com/terms/) (effective 2026-10-15) incorporate an EULA and say users must not allow others to evaluate copies. The EULA accepted for this pre-existing installation is not available here, and this record does not assess the Terms' enforceability. Further binary or non-public runtime analysis is held pending legal review; FocusGuard will continue from public product and OS documentation.

The artifact was already present on the machine; it was not installed or launched for this analysis. The work was limited to REA static identity/member metadata. This evidence does not support claims of tamper resistance or administrator resistance.
