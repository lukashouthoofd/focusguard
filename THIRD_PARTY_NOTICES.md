# Third-party notices

FocusGuard project files are licensed under MIT; see [LICENSE](LICENSE). The imported Focuser 0.11.5 source remains under its original MIT notice in [LICENSES/Focuser-MIT.txt](LICENSES/Focuser-MIT.txt). The bundled Inter variable font is SIL Open Font License 1.1; see [LICENSES/Inter-OFL-1.1.txt](LICENSES/Inter-OFL-1.1.txt).

Third-party dependencies are pinned by [Cargo.lock](Cargo.lock), [the desktop package lock](crates/focuser-ui/frontend/package-lock.json), and [the extension package lock](extension/package-lock.json). A current license metadata inventory is in [docs/dependency-licenses.md](docs/dependency-licenses.md).

Before redistributing binaries, generate and review complete notices for the exact target-specific dependency set. In particular, preserve MPL-2.0 notices for Rust dependencies, and choose the permissive license option for the extension tooling packages `jszip` (MIT OR GPL-3.0-or-later) and `node-forge` (BSD-3-Clause OR GPL-2.0). Those GPL terms are alternatives, not the only offered licenses.
