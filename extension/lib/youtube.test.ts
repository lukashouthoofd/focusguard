import { describe, expect, it, vi } from "vitest";
import { compile, type RuleSet } from "./rules";
import { channelOf, namesChannel, youtubeVideoId } from "./youtube";

function rules(patch: Partial<RuleSet> = {}) {
  return compile({
    blocked_domains: [],
    blocked_keywords: [],
    blocked_wildcards: [],
    blocked_url_paths: [],
    block_entire_internet: false,
    allowed_domains: [],
    ...patch,
  });
}

describe("youtubeVideoId", () => {
  it("finds the video in every kind of YouTube address", () => {
    for (const url of [
      "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
      "https://m.youtube.com/watch?v=dQw4w9WgXcQ&t=42",
      "https://www.youtube.com/shorts/dQw4w9WgXcQ",
      "https://www.youtube.com/live/dQw4w9WgXcQ?si=x",
      "https://youtu.be/dQw4w9WgXcQ",
    ])
      expect(youtubeVideoId(url), url).toBe("dQw4w9WgXcQ");
  });

  it("finds none on other pages", () => {
    for (const url of [
      "https://www.youtube.com/",
      "https://www.youtube.com/@somechannel",
      "https://www.youtube.com/results?search_query=v",
      "https://example.com/watch?v=dQw4w9WgXcQ",
      "not an address",
    ])
      expect(youtubeVideoId(url), url).toBeNull();
  });
});

describe("namesChannel", () => {
  it("is true only when a rule points at a YouTube channel", () => {
    expect(namesChannel(rules({ blocked_url_paths: ["youtube.com/@somechannel"] }))).toBe(true);
    expect(namesChannel(rules({ blocked_wildcards: ["*youtube.com/@some*"] }))).toBe(true);
    expect(namesChannel(rules({ blocked_domains: ["youtube.com"] }))).toBe(false);
    expect(namesChannel(rules({ blocked_url_paths: ["youtube.com/shorts"] }))).toBe(false);
  });

  it("looks inside each list's scope as well", () => {
    const scoped = rules({
      scopes: [
        {
          rules: {
            blocked_domains: [],
            blocked_keywords: [],
            blocked_wildcards: [],
            blocked_url_paths: ["youtube.com/@somechannel"],
            block_entire_internet: false,
            allowed_domains: [],
          },
          shared_permits: null,
          scheduled: false,
        },
      ],
    });
    expect(namesChannel(scoped)).toBe(true);
  });
});

describe("channelOf", () => {
  const oembed = (authorUrl: unknown) =>
    vi.fn(async (_url: string) => new Response(JSON.stringify({ author_url: authorUrl })));

  it("asks YouTube once per video and gives the channel's address", async () => {
    const fetched = oembed("https://www.youtube.com/@SomeChannel");
    expect(await channelOf("vid00000001", fetched)).toBe("https://www.youtube.com/@SomeChannel");
    expect(await channelOf("vid00000001", fetched)).toBe("https://www.youtube.com/@SomeChannel");
    expect(fetched).toHaveBeenCalledTimes(1);
    const asked = new URL(String(fetched.mock.calls[0]?.[0]));
    expect(`${asked.origin}${asked.pathname}`).toBe("https://www.youtube.com/oembed");
    expect(asked.searchParams.get("url")).toBe("https://www.youtube.com/watch?v=vid00000001");
  });

  it("gives null when YouTube does not say, and does not throw", async () => {
    expect(await channelOf("vid00000002", oembed(undefined))).toBeNull();
    expect(await channelOf("vid00000003", oembed("https://evil.test/@x"))).toBeNull();
    const offline = vi.fn(async () => {
      throw new TypeError("Failed to fetch");
    });
    expect(await channelOf("vid00000004", offline)).toBeNull();
    // A lookup that failed is asked again, not remembered as "no channel".
    const back = oembed("https://www.youtube.com/@SomeChannel");
    expect(await channelOf("vid00000004", back)).toBe("https://www.youtube.com/@SomeChannel");
  });
});
