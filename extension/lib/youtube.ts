/**
 * Blocking a YouTube channel on its videos (#48).
 *
 * A video's address is only its id, so a rule for the channel's page never
 * sees it. YouTube's public oEmbed address says which channel a video is
 * from, and the channel's address is then checked against the rules like
 * any other page.
 */
import type { CompiledRules } from "./rules";

/** The video a YouTube address shows, or null. */
export function youtubeVideoId(url: string): string | null {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return null;
  }
  const host = parsed.hostname.replace(/^(www|m)\./, "");
  const id =
    host === "youtu.be"
      ? parsed.pathname.split("/")[1]
      : host !== "youtube.com"
        ? null
        : parsed.pathname === "/watch"
          ? parsed.searchParams.get("v")
          : parsed.pathname.match(/^\/(shorts|live)\/([^/]+)/)?.[2];
  return id && /^[\w-]{6,}$/.test(id) ? id : null;
}

/** Whether any rule points at a channel, so looking one up can matter. */
export function namesChannel(rules: CompiledRules): boolean {
  return (
    [...rules.urlPaths, ...rules.wildcards].some((p) => p.toLowerCase().includes("youtube.com/@")) ||
    (rules.scopes ?? []).some((scope) => namesChannel(scope.rules))
  );
}

/** Lookups by video id. A video never changes channel, so they are kept. */
const channels = new Map<string, Promise<string | null>>();
const MAX_KEPT = 500;

/**
 * The address of the channel a video is from, like
 * `https://www.youtube.com/@name`, or null when YouTube does not say.
 */
export function channelOf(
  videoId: string,
  get: (url: string) => Promise<Response> = (url) => fetch(url),
): Promise<string | null> {
  const known = channels.get(videoId);
  if (known) return known;

  const watch = `https://www.youtube.com/watch?v=${videoId}`;
  const lookup = get(`https://www.youtube.com/oembed?format=json&url=${encodeURIComponent(watch)}`)
    .then((response) => {
      if (!response.ok) throw new Error(`oEmbed answered ${response.status}`);
      return response.json();
    })
    .then((data: unknown) => {
      const author = (data as { author_url?: unknown } | null)?.author_url;
      return typeof author === "string" && /^https:\/\/(www\.)?youtube\.com\//.test(author)
        ? author
        : null;
    })
    .catch(() => {
      // Asked again next time. The network may be back by then.
      channels.delete(videoId);
      return null;
    });

  if (channels.size >= MAX_KEPT) {
    const oldest = channels.keys().next().value;
    if (oldest !== undefined) channels.delete(oldest);
  }
  channels.set(videoId, lookup);
  return lookup;
}
