import tailwindcss from "@tailwindcss/vite";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { defineConfig } from "wxt";

const extensionIdentity = JSON.parse(
  readFileSync(new URL("./identity.json", import.meta.url), "utf8"),
) as {
  chrome_extension_id: string;
  chrome_public_key: string;
  firefox_extension_id: string;
};
const derivedChromeId = createHash("sha256")
  .update(Buffer.from(extensionIdentity.chrome_public_key, "base64"))
  .digest()
  .subarray(0, 16)
  .reduce((id, byte) => id + String.fromCharCode(97 + (byte >> 4), 97 + (byte & 15)), "");
if (derivedChromeId !== extensionIdentity.chrome_extension_id) {
  throw new Error("The Chrome extension ID does not match its pinned public key.");
}

// `public/premade-lists.json` is a copy of the desktop app's. It used to be
// read from `../crates/...` at build time, which made this folder impossible
// to build on its own — and AMO requires a source archive a reviewer can build.
// `starter_lists_match` in focuser-ui fails if the two copies drift.

const ICONS = {
  16: "icons/icon16.png",
  32: "icons/icon32.png",
  48: "icons/icon48.png",
  96: "icons/icon96.png",
  128: "icons/icon128.png",
};

export default defineConfig({
  // `@wxt-dev/i18n` wraps `browser.i18n`, so the extension follows the
  // *browser's* language rather than the desktop app's setting. That is the
  // platform's own mechanism: there is no API to change the locale at runtime,
  // and the only workaround is reloading the extension, which leaves already
  // open pages in the old language.
  modules: ["@wxt-dev/module-react", "@wxt-dev/i18n/module"],
  srcDir: ".",
  outDir: ".output",

  // WXT defaults Firefox to MV2. The published AMO listing is MV3 and has been
  // since Firefox 142, so dropping to MV2 would be a regression for existing
  // users rather than a compatibility win.
  manifestVersion: 3,

  manifest: ({ browser }) => ({
    name: "FocusGuard",
    default_locale: "en",
    description:
      "Blocks distracting sites so you stay focused. Enforces the block lists you set in the FocusGuard desktop app.",
    permissions: [
      "tabs",
      "webNavigation",
      "scripting",
      "storage",
      "alarms",
      "idle",
      "nativeMessaging",
    ],
    host_permissions: ["<all_urls>"],
    icons: ICONS,
    ...(browser === "chrome" ? { key: extensionIdentity.chrome_public_key } : {}),
    action: { default_icon: ICONS },
    // The block page is injected into blocked tabs rather than navigated to,
    // so its script and the category data must be readable from any origin.
    web_accessible_resources: [
      {
        matches: ["<all_urls>"],
        resources: ["block-page.js", "premade-lists.json"],
      },
    ],
    // Chrome ignores unknown keys, but shipping Gecko settings in a Chrome
    // build is noise a store reviewer has to read past.
    ...(browser === "firefox"
      ? {
          browser_specific_settings: {
            gecko: {
              id: extensionIdentity.firefox_extension_id,
              strict_min_version: "142.0",
              data_collection_permissions: { required: ["none"] },
            },
          },
        }
      : {}),
  }),

  vite: () => ({
    plugins: [tailwindcss()],
  }),
});
