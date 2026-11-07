# FocusGuard

FocusGuard is a free, open-source, Windows-first focus blocker. The goal is to help people block distracting websites and applications with dependable schedules and clear recovery paths.

**Status: early scaffold. Blocking code has not been implemented or tested yet.** The repository is being built independently from public product documentation and original implementation work. It does not contain Cold Turkey code or attempt to bypass its licensing.

## Project goals

- Reliable website and application blocking on Windows
- Recurring schedules and timed focus sessions
- Optional locking that explains its limits and recovery options
- Local-first settings and transparent system changes
- Documented bypass boundaries, crash behavior, restarts, and recovery

These are goals, not features available in this initial scaffold. See [PROGRESS.md](PROGRESS.md) for current evidence and next steps.

## Development

The first implementation will prioritize a small, auditable Windows core and a recovery-first installer. System-level behavior will require explicit user consent and will be documented before use. No anti-bypass guarantee is made.

## License

FocusGuard is licensed under the MIT License. See [LICENSE](LICENSE).
