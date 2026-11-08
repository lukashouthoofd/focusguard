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
    const sent = native.port.postMessage.mock.calls[0]?.[0] as { request_id: number };
    expect(native.port.postMessage).toHaveBeenCalledWith({
      msg_type: "RuleUpdate",
      payload: null,
      request_id: sent.request_id,
    });
    native.deliver({ msg_type: "RuleUpdate", payload: RULES, request_id: sent.request_id });

    await expect(request).resolves.toEqual(RULES);
    expect(fetched).not.toHaveBeenCalled();
  });

  it("correlates rules and heartbeat replies on one persistent native port", async () => {
    const native = nativePort();
    Object.assign(fakeBrowser.runtime, {
      connectNative: vi.fn(() => native.port),
    });
    const fetched = vi.fn();
    vi.stubGlobal("fetch", fetched);
    const { fetchRules, sendHeartbeat } = await import("./api");

    const rulesRequest = fetchRules("Chrome");
    const heartbeatRequest = sendHeartbeat(
      "Chrome",
      true,
      "00000000-0000-4000-8000-000000000001",
    );
    const rulesMessage = native.port.postMessage.mock.calls[0]?.[0] as {
      request_id: number;
    };
    const heartbeatMessage = native.port.postMessage.mock.calls[1]?.[0] as {
      request_id: number;
      payload: unknown;
    };
    expect(heartbeatMessage.payload).toEqual({
      kind: "heartbeat",
      browser: "Chrome",
      profile_id: "00000000-0000-4000-8000-000000000001",
      incognito_allowed: true,
    });

    native.deliver({
      msg_type: "Event",
      payload: { ok: true },
      request_id: heartbeatMessage.request_id,
    });
    await heartbeatRequest;
    native.deliver({ msg_type: "RuleUpdate", payload: RULES, request_id: rulesMessage.request_id });

    await expect(rulesRequest).resolves.toEqual(RULES);
    expect(fetched).not.toHaveBeenCalled();
  });

  it("uses distinct normal and shared activity events without client elapsed time", async () => {
    const native = nativePort();
    Object.assign(fakeBrowser.runtime, {
      connectNative: vi.fn(() => native.port),
    });
    Object.assign(fakeBrowser.storage.local, {
      get: vi.fn(async () => ({ profileId: "00000000-0000-4000-8000-000000000001" })),
    });
    const fetched = vi.fn();
    vi.stubGlobal("fetch", fetched);
    const { sendAllowanceTick } = await import("./api");

    for (const sharedOnly of [false, true]) {
      const request = sendAllowanceTick(
        "example.com",
        47,
        "extension-alarm",
        "https://example.com/a",
        sharedOnly,
      );
      await vi.waitFor(() => expect(native.port.postMessage).toHaveBeenCalledTimes(sharedOnly ? 2 : 1));
      const sent = native.port.postMessage.mock.calls[sharedOnly ? 1 : 0]?.[0] as {
        request_id: number;
        payload: Record<string, unknown>;
      };
      expect(sent.payload).toMatchObject({
        kind: sharedOnly ? "shared_activity_tick" : "allowance_tick",
        profile_id: "00000000-0000-4000-8000-000000000001",
        hostname: "example.com",
        url: "https://example.com/a",
      });
      expect(sent.payload).not.toHaveProperty("increment_secs");
      native.deliver({ msg_type: "Event", payload: { ok: true }, request_id: sent.request_id });
      await request;
    }
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
    const sent = native.port.postMessage.mock.calls[0]?.[0] as { request_id: number };
    native.deliver({
      msg_type: "Error",
      payload: "service unavailable",
      request_id: sent.request_id,
    });

    await expect(request).resolves.toEqual(RULES);
    expect(fetched).toHaveBeenCalledOnce();
  });
});
