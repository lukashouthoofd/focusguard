import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fakeBrowser } from "wxt/testing/fake-browser";
import type { RuleSet } from "./rules";

const RULES: RuleSet = {
  blocked_domains: ["example.com"],
  blocked_keywords: ["casino"],
  blocked_wildcards: [],
  blocked_url_paths: [],
  block_entire_internet: false,
  allowed_domains: [],
};

function nativePort() {
  const messages: ((message: unknown) => void)[] = [];
  const disconnects: (() => void)[] = [];
  const port = {
    onMessage: {
      addListener: vi.fn((listener: (message: unknown) => void) => messages.push(listener)),
    },
    onDisconnect: {
      addListener: vi.fn((listener: () => void) => disconnects.push(listener)),
    },
    postMessage: vi.fn(),
    disconnect: vi.fn(),
  };
  return {
    port,
    deliver(message: unknown) {
      for (const listener of messages) listener(message);
    },
    close() {
      for (const listener of disconnects) listener();
    },
  };
}

beforeEach(() => {
  vi.resetModules();
  fakeBrowser.reset();
  vi.stubGlobal("browser", fakeBrowser);
});

afterEach(() => vi.restoreAllMocks());

describe("extension rule transport", () => {
  it("reads rules from the native service connection when available", async () => {
    const native = nativePort();
    Object.assign(fakeBrowser.runtime, {
      connectNative: vi.fn(() => native.port),
    });
    const fetched = vi.fn();
    vi.stubGlobal("fetch", fetched);
    const { fetchRules } = await import("./api");

    const request = fetchRules("Chrome");
    expect(native.port.postMessage).toHaveBeenCalledWith({
      msg_type: "RuleUpdate",
      payload: null,
    });
    native.deliver({ msg_type: "RuleUpdate", payload: RULES });

    await expect(request).resolves.toEqual(RULES);
    expect(fetched).not.toHaveBeenCalled();
  });

  it("falls back to the desktop API when native messaging is unavailable", async () => {
    Object.assign(fakeBrowser.runtime, {
      connectNative: vi.fn(() => {
        throw new Error("Native host not installed");
      }),
    });
    const fetched = vi.fn(async () => new Response(JSON.stringify(RULES)));
    vi.stubGlobal("fetch", fetched);
    const { fetchRules } = await import("./api");

    await expect(fetchRules("Chrome")).resolves.toEqual(RULES);
    expect(fetched).toHaveBeenCalledWith(
      "http://127.0.0.1:17593/api/rules",
      { headers: { "X-FocusGuard-Browser": "Chrome" } },
    );
  });

  it("falls back when the native host returns an error", async () => {
    const native = nativePort();
    Object.assign(fakeBrowser.runtime, {
      connectNative: vi.fn(() => native.port),
    });
    const fetched = vi.fn(async () => new Response(JSON.stringify(RULES)));
    vi.stubGlobal("fetch", fetched);
    const { fetchRules } = await import("./api");

    const request = fetchRules("Firefox");
    native.deliver({ msg_type: "Error", payload: "service unavailable" });

    await expect(request).resolves.toEqual(RULES);
    expect(fetched).toHaveBeenCalledOnce();
  });
});
