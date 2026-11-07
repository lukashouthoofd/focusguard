# Progress

## 2026-11-07 — repository scaffold

- Created the public project scaffold per the user's request to create the GitHub repository now.
- The README states that no blocking implementation exists yet; no functionality is claimed as verified.
- Added project guidance adapted from the linked Karpathy-inspired guidelines, with source attribution.
- REA's `reverse-engineer-anything` skill is already available in this environment. The REA MCP server is not exposed in this session; check for the CLI and use its documented CLI fallback where applicable.
- Reverse-engineering, candidate-project audits, and PublikHQ setup requirements remain in progress.

## Next evidence to gather

1. Verify REA CLI availability and choose the supported REA workflow for the publicly observable web app.
2. Audit Focuser, Open Turkey, and Lockdown source trees, licenses, and Windows behavior before choosing build-versus-fork.
3. Inspect Cold Turkey's public behavior claims and Windows documentation; record technical inferences separately.
4. Read PublikHQ's setup instructions and prepare listing artifacts.
5. Implement the Windows core, then verify blocking behavior, scheduled starts, process restarts, crashes, and documented recovery paths.

## 2026-11-07 — research and direction

- Completed a public-documentation evidence ledger for Cold Turkey. Its official guide asserts a Windows background service and administrator requirement, but the public sources do not disclose service identity, startup/recovery configuration, filter implementation, or persistence format. No proprietary binary was installed or analyzed; the Terms page incorporates an install-time EULA and we avoid crossing that boundary.
- Audited Focuser 0.11.5 at commit `14cbfdb3d55d3bc669bc9a39a6eddb9915e61844`: MIT licensed, broad features, Windows 10/11 support claimed. Source shows its Windows blocker loop is a thread in the UI app, launched at user logon by an elevated Scheduled Task. It is not a separate Windows SCM service. The extension depends on the app and fails open if its local API is absent.
- Open Turkey is an MIT Linux/systemd/iptables reference. Lockdown has no root LICENSE file in the reviewed snapshot and will not be reused.
- Decision: use Focuser as the open-source baseline, preserve attribution/license, then replace/augment Windows enforcement with a service-owned policy boundary. A transitive dependency license scan remains before release.
- REA skill is present (installed skill version 34). `rea-agents@6.3.0` was fetched through npx after checking the published npm integrity and repository URL. REA doctor reports the skill aligned and Ghidra available, but the Codex MCP registration is missing. A dry-run proposes one write to `.codex/config.toml` with a backup; approval is pending. Static CLI use remains available meanwhile.
- Read PublikHQ's raw setup guide. It requires a reviewable listing/API plan and one approval before changes or listing. FocusGuard does not need an AI API for its current scope; no listing has been submitted.
