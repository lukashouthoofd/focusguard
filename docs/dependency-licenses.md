# Dependency license inventory

Inventory date: 2026-11-08. This is a manifest-level audit of the locked dependency graphs, not a legal opinion or a complete binary notice bundle.

## Rust

`cargo metadata --locked --format-version 1` reports 678 external packages across the workspace's Windows, macOS, Linux, and other target-specific dependency branches. All 678 declare a license expression. The inventory includes MIT/Apache-2.0, BSD, ISC, Unicode-3.0, Zlib, Unlicense, 17 MPL-2.0 packages, one CDLA-Permissive-2.0 package, and two `r-efi` versions whose expression offers MIT, Apache-2.0, or LGPL-2.1-or-later. No GPL-only expression was found.

The count includes packages not built on Windows and duplicates at different versions. A release should generate notices from the actual Windows target and include all required text notices.

## JavaScript

`license-checker --json` and `--summary` were run against both installed lockfiles, including development tools. The desktop frontend contains MIT, ISC, Apache-2.0, BSD, MPL-2.0, OFL-1.1, BlueOak-1.0.0, CC0-1.0, 0BSD, and related dual-license packages. Its sole OFL package is the Inter font, whose license text is included in `LICENSES/Inter-OFL-1.1.txt`.

The extension dependency tree includes `jszip@3.10.2` under `(MIT OR GPL-3.0-or-later)` and `node-forge@1.4.3` under `(BSD-3-Clause OR GPL-2.0)`. Each offers a permissive choice; neither requires GPL. The tree also includes MPL-2.0 and other permissive packages. The scanner marks the two private workspace roots as `UNLICENSED` because they are private npm packages; their source files are covered by FocusGuard's MIT license and these roots are not separately published as npm packages.

## Vulnerability scan

After applying non-forced npm audit remediations and updating the remaining vulnerable direct frontend pins, `npm audit` reports zero advisories for the desktop frontend. The extension tree reports four low-severity findings in development tooling through `web-ext` / `addons-linter` → `deepmerge`; the extension manifest lists these packages as development dependencies, and they are not included in the browser bundle.

GitHub's advisory for [deepmerge prototype poisoning (GHSA-gcqx-w48m-w2h4)](https://github.com/advisories/GHSA-gcqx-w48m-w2h4) lists no patched version. npm's only proposed remediation is a breaking downgrade to `web-ext@3.2.1`; that downgrade was not applied. This is a documented development-tooling residual, not a claim that the overall product has no security risks.

## Release gate

The upstream dependency metadata is broad and several licenses require notice preservation. Before a downloadable app or extension release, generate the full notices for the exact Windows and browser outputs, verify embedded runtime assets, and include the required license texts. No binary release is prepared or published yet.
