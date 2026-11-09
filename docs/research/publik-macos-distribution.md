# Publik macOS distribution notes

Checked 2026-11-09 while preparing FocusGuard's macOS service.

## What other Publik apps ship

- [Lidless](https://publikhq.com/lidless) is the closest comparison: it uses an `SMAppService` root LaunchDaemon and embeds the executable and plist in the app bundle. Its Publik listing says the release is signed with Publik's Developer ID; the [Mac install guide](https://publikhq.com/lidless/install/mac) delivers a signed and notarized DMG, then asks the user to approve the helper in Login Items.
- Lidless's public [`publik-build.yml`](https://github.com/Blueturboguy07/Lidless/blob/main/.github/workflows/publik-build.yml) intentionally creates an unsigned CI artifact. Its [`release.sh`](https://github.com/Blueturboguy07/Lidless/blob/main/scripts/release.sh) performs signing, notarization, stapling, and DMG creation on a Mac with a Developer ID certificate and a notarytool keychain profile.
- [Craft Launcher](https://publikhq.com/craft-launcher) also ships a Publik-signed, notarized macOS build. Its [release workflow](https://github.com/Blueturboguy07/craft-launcher/blob/main/.github/workflows/release.yml) calls a reusable release workflow and passes Apple certificate, password, Apple ID, and team-ID secrets.
- [publikclip's source-install guide](https://publikhq.com/publikclip/install/mac) builds and copies a local app, noting that a local build opens without a Gatekeeper warning. It does not claim to package or register a privileged LaunchDaemon, so it does not solve FocusGuard's helper-signing requirement.

## FocusGuard status

The macOS architecture already follows Apple's app-bundle model: the daemon executable and plist are staged inside the `.app`, the UI registers it through `SMAppService`, and CI mounts both unsigned DMGs and verifies those files. Apple's docs say LaunchDaemon registration is subject to administrator approval and describe keeping its plist in the app bundle ([registration](https://developer.apple.com/documentation/servicemanagement/smappservice/register%28%29), [bundle and installer guidance](https://developer.apple.com/documentation/servicemanagement/updating-your-app-package-installer-to-use-the-new-service-management-api)).

GitHub Actions run [42615271494](https://github.com/lukashouthoofd/focusguard/actions/runs/42615271494) passed all jobs on commit `8de64fb`. It uploaded Intel and Apple Silicon unsigned DMGs after verifying the helper and plist inside each image. These are expiring CI artifacts, not a GitHub Release or signed public downloads. There has been no real-Mac approval, reboot, crash-recovery, DNS, update, or uninstall test.

## Distribution route

There is no documented self-serve Publik signing endpoint in the [Publik publish guide](https://publikhq.com/skills/publish.md); it lists an app from its GitHub repository and latest release. The published examples show that Publik can sign and notarize builds it maintains, but they do not promise that every listed repository receives that service. **Inference:** to distribute FocusGuard without owning an Apple Developer ID, ask Publik to produce or sponsor a signed release/fork. Otherwise, configure an Apple Developer ID certificate and notarization credentials in a release workflow, following the Craft workflow pattern.

Do not remove the LaunchDaemon or weaken its registration checks just to make an unsigned DMG appear to work. Keep unsigned CI packages labeled for development until a signed, notarized release has been produced and tested on a Mac.
