# AGENTS.md

These project guidelines are adapted from the four principles in [multica-ai/andrej-karpathy-skills](https://github.com/multica-ai/andrej-karpathy-skills/blob/main/CLAUDE.md). Its README identifies that project as MIT licensed. This file restates the guidance for FocusGuard and adds project-specific requirements.

## Think before changing code

Inspect the relevant code and evidence first. State assumptions and tradeoffs when they materially affect the result. Keep moving when the request is clear; ask only when a missing decision would materially change the outcome.

## Keep solutions simple

Implement the smallest complete change that meets the requested behavior. Avoid speculative features, one-use abstractions, and complexity without a demonstrated need.

## Keep changes scoped

Preserve existing conventions and unrelated behavior. Modify only what the task requires, and remove only unused artifacts introduced by the change.

## Define and verify outcomes

For substantive work, set observable success criteria and verify them against the actual behavior. Keep concise progress notes for multi-step work, including failures and recovery paths.

## FocusGuard requirements

- Use REA as the primary methodology when a claim depends on shipped-app or runtime behavior; distinguish observed evidence from inference and unknowns.
- Build independently from public documentation. Never copy proprietary source or circumvent paid licensing.
- Prioritize Windows blocking reliability, safe recovery, and honest limits over cosmetic features.
- Treat administrator access, service installation, firewall/hosts changes, and other machine-wide changes as explicit, reversible user choices.
- Never describe the product as tamperproof. Name the tested user privilege level and bypass conditions for every security claim.
- Do not publish a release or make irreversible system changes without explicit user approval.
