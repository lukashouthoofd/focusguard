/**
 * Client for the desktop app's local HTTP API.
 *
 * Everything stays on the machine: the app serves this on loopback only, and
 * the extension talks to nothing else, with one exception. When a rule names
 * a YouTube channel, `youtube.ts` asks YouTube which channel a video is from.
 * There is no FocusGuard server anywhere, by design.
 *
 * Windows builds can read rules through Native Messaging and the authenticated
 * service pipe, so the service keeps enforcing while the desktop UI is closed.
 * Other extension features continue to use the desktop HTTP API until their
 * typed service operations are available.
 */

import type { RuleSet } from "./rules";

export const API_BASE = "http://127.0.0.1:17593";

/** How often to re-read rules. The app recompiles them on every change. */
export const POLL_INTERVAL_MS = 2_000;

type NativePort = ReturnType<typeof browser.runtime.connectNative>;
let nativePort: NativePort | null = null;
let pendingNativeRules:
  | { resolve: (rules: RuleSet | null) => void; timeout: ReturnType<typeof setTimeout> }
  | null = null;
let nativeRulesRequest: Promise<RuleSet | null> | null = null;

function settleNativeRules(rules: RuleSet | null): void {
  const pending = pendingNativeRules;
  if (!pending) return;
  pendingNativeRules = null;
  clearTimeout(pending.timeout);
  pending.resolve(rules);
}

function ensureNativePort(): NativePort {
  if (nativePort) return nativePort;
  const port = browser.runtime.connectNative("com.focuser.native");
  nativePort = port;
  port.onMessage.addListener((message: unknown) => {
    if (nativePort !== port) return;
    if (!message || typeof message !== "object") {
      settleNativeRules(null);
      return;
    }
    const response = message as { msg_type?: unknown; payload?: unknown };
    if (response.msg_type === "RuleUpdate" && isRuleSet(response.payload)) {
      settleNativeRules(response.payload);
    } else {
      settleNativeRules(null);
    }
  });
  port.onDisconnect.addListener(() => {
    if (nativePort === port) {
      nativePort = null;
      settleNativeRules(null);
    }
  });
  return port;
}

function isRuleSet(value: unknown): value is RuleSet {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<RuleSet>;
  return (
    Array.isArray(candidate.blocked_domains) &&
    Array.isArray(candidate.blocked_keywords) &&
    Array.isArray(candidate.blocked_wildcards) &&
    Array.isArray(candidate.blocked_url_paths) &&
    Array.isArray(candidate.allowed_domains)
  );
}

function fetchNativeRules(): Promise<RuleSet | null> {
  if (nativeRulesRequest) return nativeRulesRequest;
  nativeRulesRequest = new Promise<RuleSet | null>((resolve) => {
    try {
      const port = ensureNativePort();
      const pending = {
        resolve,
        timeout: setTimeout(() => {
          if (pendingNativeRules?.resolve !== resolve) return;
          settleNativeRules(null);
          // Drop the host so a late reply cannot be mistaken for the next
          // poll's response.
          port.disconnect();
        }, 5_000),
      };
      pendingNativeRules = pending;
      port.postMessage({ msg_type: "RuleUpdate", payload: null });
    } catch {
      if (pendingNativeRules?.resolve === resolve) settleNativeRules(null);
      else resolve(null);
    }
  }).finally(() => {
    nativeRulesRequest = null;
  });
  return nativeRulesRequest;
}

/** Which browser we are, for the app's "extension connected" tracking. */
export type BrowserName = "Chrome" | "Firefox" | "Edge" | "Brave" | "Opera" | "Other";

export function detectBrowser(): BrowserName {
  const ua = navigator.userAgent;
  // Order matters: Brave, Edge and Opera all claim to be Chrome.
  if ("brave" in navigator) return "Brave";
  if (ua.includes("Edg/")) return "Edge";
  if (ua.includes("OPR/") || ua.includes("Opera")) return "Opera";
  if (ua.includes("Firefox/")) return "Firefox";
  if (ua.includes("Chrome/")) return "Chrome";
  return "Other";
}

/** Result of a rules fetch. `null` means the app is not reachable. */
export async function fetchRules(browser: BrowserName): Promise<RuleSet | null> {
  const nativeRules = await fetchNativeRules();
  if (nativeRules) return nativeRules;

  try {
    const response = await fetch(`${API_BASE}/api/rules`, {
      headers: { "X-FocusGuard-Browser": browser },
    });
    if (!response.ok) return null;
    return (await response.json()) as RuleSet;
  } catch {
    // The app being closed is a normal state, not an error worth logging on
    // every poll — the toolbar badge is how the user finds out.
    return null;
  }
}

/**
 * Tell the app we are alive, so it does not close the browser for running
 * without the extension.
 *
 * `incognitoAllowed` is whether this extension has been granted "Allow in
 * Incognito" — without it, the app cannot tell a covered private window from
 * an uncovered one, since Chrome hides incognito windows from an extension
 * that lacks the permission entirely. The app treats "not allowed" the same
 * as "not installed": it is a real gap, not a false alarm.
 *
 * `null` is "could not find out" and sends nothing. The app closes the
 * browser on a no, so only a real no may be sent as one.
 */
export async function sendHeartbeat(
  browser: BrowserName,
  incognitoAllowed: boolean | null = null,
  profile = "",
): Promise<void> {
  const answer =
    incognitoAllowed === null
      ? ""
      : `&incognito_allowed=${incognitoAllowed}&profile=${encodeURIComponent(profile)}`;
  try {
    await fetch(`${API_BASE}/api/heartbeat?browser=${encodeURIComponent(browser)}${answer}`);
  } catch {
    /* app closed */
  }
}

/**
 * Whether this extension can run in private/incognito windows. `null` when
 * the browser does not say: the call is missing, throws, or answers with
 * something that is not a yes or a no.
 */
export async function isIncognitoAllowed(): Promise<boolean | null> {
  try {
    const allowed: unknown = await browser.extension.isAllowedIncognitoAccess();
    return typeof allowed === "boolean" ? allowed : null;
  } catch {
    return null;
  }
}

/** Blocking needs every site, and the app's address is one of them. */
const ALL_SITES = { origins: ["<all_urls>"] };

/**
 * Whether the user left this extension its access to websites. Firefox lets
 * them switch it off, and Chrome's "on click" does the same. Without it every
 * call to the app fails, which looks just like the app being closed (#61).
 * True when the browser does not say, so a guess never hides a real error.
 */
export async function hasSiteAccess(): Promise<boolean> {
  try {
    return await browser.permissions.contains(ALL_SITES);
  } catch {
    return true;
  }
}

/** Ask for that access back. Firefox only shows the prompt straight from a click. */
export function requestSiteAccess(): Promise<boolean> {
  return browser.permissions.request(ALL_SITES);
}

/**
 * A name for this copy of the extension, made up once and kept.
 *
 * A browser runs one copy per profile, and each has its own answer about
 * private windows. The app needs to tell them apart to hold each to its own.
 * It never leaves this machine.
 */
/**
 * Leave the browser an address to open if this extension is removed, so the
 * app hears of it at once and not two quiet minutes later. Only while the
 * app answers: with no app there, the tab would only show an error.
 */
export async function leaveRemovalAddress(
  browserName: BrowserName,
  appRunning: boolean,
): Promise<void> {
  try {
    const url = appRunning
      ? `${API_BASE}/api/extension-removed?browser=${encodeURIComponent(browserName)}&profile=${encodeURIComponent(await profileId())}`
      : "";
    await browser.runtime.setUninstallURL(url);
  } catch {
    /* not in this browser: the app notices when the check-ins stop */
  }
}

export async function profileId(): Promise<string> {
  try {
    const stored: unknown = (await browser.storage.local.get("profileId")).profileId;
    if (typeof stored === "string" && stored) return stored;
    const id = crypto.randomUUID();
    await browser.storage.local.set({ profileId: id });
    return id;
  } catch {
    return "";
  }
}

/**
 * Record a block and get back how many times this target has been blocked.
 *
 * The count is shown on the block page. A failed report still returns 1 rather
 * than throwing, because a statistics hiccup must never stop the page being
 * blocked.
 */
export async function reportBlocked(domain: string, key: string): Promise<number> {
  try {
    const response = await fetch(`${API_BASE}/api/blocked`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ domain, tracking_key: key }),
    });
    const data = (await response.json()) as { count?: number };
    return data?.count ?? 1;
  } catch {
    return 1;
  }
}

/**
 * Report time spent on the active tab, so website allowances count down.
 *
 * `increment_secs` is clamped: a service worker that slept for an hour must not
 * retroactively bill the user an hour of quota when it wakes.
 */
export async function sendAllowanceTick(
  hostname: string,
  incrementSecs: number,
  source: string,
  url?: string,
  sharedOnly = false,
): Promise<void> {
  try {
    await fetch(`${API_BASE}/api/allowance-tick`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        hostname,
        app_exe: null,
        active: true,
        source,
        increment_secs: incrementSecs,
        url: url ?? null,
        shared_active: sharedOnly,
        shared_only: sharedOnly,
      }),
    });
  } catch {
    /* app closed */
  }
}

/** Clamp an elapsed span to a sane per-tick credit. */
export function clampIncrement(elapsedMs: number): number {
  return Math.min(60, Math.max(5, Math.round(elapsedMs / 1000)));
}

// ─── Popup-facing endpoints ──────────────────────────────────────────

export interface AppStatus {
  running: boolean;
  active_lists: number;
  blocked_sites: number;
  blocked_apps: number;
  blocked_today: number;
}

export interface BlockListSummary {
  id: string;
  name: string;
  enabled: boolean;
  website_count: number;
  app_count: number;
}

async function getJson<T>(path: string): Promise<T | null> {
  try {
    const response = await fetch(`${API_BASE}${path}`);
    if (!response.ok) return null;
    return (await response.json()) as T;
  } catch {
    return null;
  }
}

async function postJson<T = unknown>(path: string, body: unknown): Promise<T | null> {
  try {
    const response = await fetch(`${API_BASE}${path}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!response.ok) return null;
    return (await response.json().catch(() => ({}))) as T;
  } catch {
    return null;
  }
}

/** Where a site is listed, which is not the same as which list is selected. */
export interface SiteStatus {
  domain: string;
  blocked: boolean;
  lists: Array<{
    id: string;
    name: string;
    enabled: boolean;
    rule_kind: "domain" | "wildcard" | "keyword" | "url_path" | "everything";
  }>;
}

export const fetchStatus = () => getJson<AppStatus>("/api/status");
export const fetchLists = () => getJson<BlockListSummary[]>("/api/lists");

/**
 * `null` means the question could not be answered, which is not the same as
 * "not blocked". An older desktop app has no such endpoint and 404s, and
 * treating that as "not in any list" is how a site already on a list got
 * offered for blocking a second time.
 */
export const fetchSiteStatus = (domain: string) =>
  getJson<SiteStatus>(`/api/site-status?domain=${encodeURIComponent(domain)}`);

/** `duplicate` distinguishes "already there" from a fresh add. */
export async function addSite(
  listId: string,
  domain: string,
): Promise<{ duplicate: boolean } | null> {
  const reply = await postJson<{ duplicate?: boolean }>("/api/add-site", {
    list_id: listId,
    domain,
    rule_type: "domain",
  });
  if (!reply) return null;
  return { duplicate: reply.duplicate === true };
}

/**
 * Removes the site from one list, reporting what actually happened.
 *
 * `removed` is the point: the API used to answer "ok" whether or not anything
 * matched, so unblocking a site held by a *different* list looked like it had
 * worked while the site stayed blocked.
 */
export async function removeSite(
  listId: string,
  domain: string,
): Promise<{ removed: number; leftBehind: number } | null> {
  const reply = await postJson<{ removed?: number; left_behind?: number }>("/api/remove-site", {
    list_id: listId,
    domain,
  });
  if (!reply) return null;
  return { removed: reply.removed ?? 0, leftBehind: reply.left_behind ?? 0 };
}

/** Ask the desktop app to bring its window to the front. */
export const showApp = () => postJson("/api/show", {}).then((r) => r !== null);
