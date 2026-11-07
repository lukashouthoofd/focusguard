//! One definition of what a hostname is and when two of them match.
//!
//! Every place that compared domains used to do its own trimming, lowercasing
//! and `www.` juggling, and they did not agree. Blocking `youtube.com` covered
//! `www.youtube.com`, but an *allowance* on `www.youtube.com` did not release
//! `youtube.com`, so the daily budget silently did nothing. The rule is now
//! stated once, here, and everything defers to it:
//!
//! **`www.` is never significant, and a rule always covers its subdomains.**
//!
//! A rule with no dot in it is a name rather than an address, `crunchyroll`,
//! and covers that site on every ending.

/// Reduce a hostname or a user-typed rule to the form everything compares on.
///
/// Forgiving on input, because people paste what is in the address bar: a
/// scheme, a path, a port, a trailing dot and a `www.` prefix are all removed.
///
/// ```
/// # use focuser_common::host::canonical_host;
/// assert_eq!(canonical_host("https://WWW.YouTube.com:443/feed"), "youtube.com");
/// ```
pub fn canonical_host(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();

    // The path goes first: an `@` after the first slash is part of the page
    // (youtube.com/@name), not credentials.
    let authority = without_scheme(&lowered)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.rsplit_once(':') {
        Some((name, _port)) if !name.is_empty() => name,
        _ => host,
    };

    let host = host.trim_end_matches('.');
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// Everything after a leading `scheme://`. Only at the very start: a `://`
/// further along belongs to a query string.
fn without_scheme(raw: &str) -> &str {
    match raw.split_once("://") {
        Some((scheme, rest))
            if scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-') =>
        {
            rest
        }
        _ => raw,
    }
}

/// Split a typed address into its host and the page under it, when it names a
/// page at all: `youtube.com/@name` does, `youtube.com/` does not.
///
/// ```
/// # use focuser_common::host::host_and_page;
/// let page = host_and_page("https://www.reddit.com/r/programming/");
/// assert_eq!(page, Some(("reddit.com".into(), "/r/programming".into())));
/// ```
pub fn host_and_page(raw: &str) -> Option<(String, String)> {
    let rest = without_scheme(raw.trim());
    let cut = rest.find(['/', '?', '#'])?;

    let host = canonical_host(&rest[..cut]);
    let page = rest[cut..].split('#').next().unwrap_or_default();
    let page = page.trim_end_matches('/');
    if host.is_empty() || page.is_empty() {
        return None;
    }
    // `example.com?q=1` has no slash, but the browser will report `/?q=1`.
    let slash = if page.starts_with('?') { "/" } else { "" };
    Some((host, format!("{slash}{page}")))
}

/// Does `host` fall under `rule`?
///
/// True for the domain itself and for any subdomain of it. `www.` is
/// irrelevant on either side, so all four combinations of
/// `youtube.com` / `www.youtube.com` match each other.
pub fn host_matches(rule: &str, host: &str) -> bool {
    let rule = canonical_host(rule);
    if rule.is_empty() {
        return false;
    }

    let host = canonical_host(host);
    if host == rule || host.ends_with(&format!(".{rule}")) {
        return true;
    }
    // A name, not an address (#24). People type what the site is called and
    // do not know or care how its address ends, so it matches as one whole
    // label of the host, wherever that label sits.
    !rule.contains('.') && host.split('.').any(|label| label == rule)
}

/// A rule with no dot in it, as host patterns for the extension.
///
/// The extension compares its domain list against the end of a host, which a
/// bare name never is. As wildcards, every published version already matches
/// it the way [`host_matches`] does, so nobody has to wait for a store update.
pub fn name_patterns(rule: &str) -> Option<[String; 2]> {
    let name = canonical_host(rule);
    (!name.is_empty() && !name.contains('.')).then(|| [format!("*.{name}.*"), format!("{name}.*")])
}

/// Does `host` fall under any of `rules`?
pub fn any_host_matches<I, S>(rules: I, host: &str) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let host = canonical_host(host);
    rules
        .into_iter()
        .any(|rule| host_matches(rule.as_ref(), &host))
}

/// Does `host` match the wildcard `pattern`?
///
/// `*.youtube.com` also covers `youtube.com` itself. People write it meaning
/// "the site", and a pattern that skipped the apex just looked broken.
pub fn wildcard_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    if pattern.is_empty() {
        return false;
    }

    // Both the host as typed and its canonical form: a glob may be aiming at
    // the `www.` label that `canonical_host` removes.
    let raw = host.trim().to_ascii_lowercase();
    let canonical = canonical_host(host);
    if canonical.is_empty() {
        return false;
    }
    if glob_match::glob_match(&pattern, &raw) || glob_match::glob_match(&pattern, &canonical) {
        return true;
    }

    match pattern.strip_prefix("*.") {
        Some(apex) if !apex.contains(['*', '?']) => host_matches(apex, &canonical),
        _ => false,
    }
}

/// The hosts-file lines needed to block `domain`.
///
/// A hosts file has no wildcards, so the bare domain and its `www.` form both
/// have to be listed. Anything deeper (`m.`, `music.`) needs the extension.
pub fn hosts_entries(domain: &str) -> Vec<String> {
    let host = canonical_host(domain);
    if host.is_empty() {
        return Vec::new();
    }
    vec![format!("www.{host}"), host]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_strips_everything_that_is_not_the_host() {
        for raw in [
            "youtube.com",
            "  YouTube.com  ",
            "www.youtube.com",
            "WWW.YOUTUBE.COM",
            "https://www.youtube.com",
            "http://youtube.com/feed/subscriptions",
            "https://www.youtube.com:443/watch?v=abc#t=10",
            "youtube.com.",
            "https://user:pass@www.youtube.com/",
        ] {
            assert_eq!(canonical_host(raw), "youtube.com", "input was {raw:?}");
        }
    }

    #[test]
    fn an_at_sign_or_a_second_scheme_in_the_path_is_not_the_host() {
        // #21: `/@YouTube` was read as credentials and gave the host "youtube".
        for raw in ["https://www.youtube.com/@YouTube", "youtube.com/@YouTube"] {
            assert_eq!(canonical_host(raw), "youtube.com", "input was {raw:?}");
        }
        assert_eq!(
            canonical_host("example.com/login?next=https://other.com"),
            "example.com"
        );
    }

    #[test]
    fn a_page_is_a_host_with_something_after_it() {
        for (raw, host, page) in [
            (
                "https://www.youtube.com/@YouTube",
                "youtube.com",
                "/@YouTube",
            ),
            ("reddit.com/r/programming/", "reddit.com", "/r/programming"),
            (
                "HTTPS://youtube.com/watch?v=abc#t=10",
                "youtube.com",
                "/watch?v=abc",
            ),
            ("example.com?q=1", "example.com", "/?q=1"),
        ] {
            let expected = Some((host.to_string(), page.to_string()));
            assert_eq!(host_and_page(raw), expected, "input was {raw:?}");
        }
    }

    #[test]
    fn a_bare_site_is_not_a_page() {
        for raw in [
            "youtube.com",
            "https://www.youtube.com/",
            "youtube.com/#top",
            "  ",
            "/just/a/path",
        ] {
            assert_eq!(host_and_page(raw), None, "input was {raw:?}");
        }
    }

    #[test]
    fn canonical_leaves_a_deeper_subdomain_alone() {
        assert_eq!(canonical_host("music.youtube.com"), "music.youtube.com");
        assert_eq!(canonical_host("www.music.youtube.com"), "music.youtube.com");
    }

    #[test]
    fn www_is_irrelevant_in_every_direction() {
        for rule in ["youtube.com", "www.youtube.com"] {
            for host in ["youtube.com", "www.youtube.com"] {
                assert!(host_matches(rule, host), "{rule} should match {host}");
            }
        }
    }

    #[test]
    fn a_rule_covers_its_subdomains() {
        assert!(host_matches("youtube.com", "music.youtube.com"));
        assert!(host_matches("youtube.com", "m.youtube.com"));
        assert!(host_matches("www.youtube.com", "music.youtube.com"));
    }

    #[test]
    fn a_subdomain_rule_does_not_cover_the_parent() {
        assert!(!host_matches("music.youtube.com", "youtube.com"));
    }

    #[test]
    fn suffixes_have_to_be_on_a_label_boundary() {
        assert!(!host_matches("youtube.com", "notyoutube.com"));
        assert!(!host_matches("tube.com", "youtube.com"));
    }

    /// #24: someone types "crunchyroll" and expects Crunchyroll to be blocked.
    /// A name is not a host, so as a host rule it used to match nothing.
    #[test]
    fn a_name_with_no_ending_covers_that_site_on_every_ending() {
        for host in [
            "crunchyroll.com",
            "www.crunchyroll.com",
            "beta.crunchyroll.com",
            "crunchyroll.co.uk",
        ] {
            assert!(host_matches("crunchyroll", host), "{host}");
        }
        // Still a whole label, never part of one.
        assert!(!host_matches("crunchyroll", "notcrunchyroll.com"));
        assert!(!host_matches("crunchy", "crunchyroll.com"));
    }

    #[test]
    fn the_extension_gets_a_bare_name_as_two_host_patterns() {
        let patterns = name_patterns("Crunchyroll").unwrap();
        for host in [
            "crunchyroll.com",
            "beta.crunchyroll.com",
            "crunchyroll.co.uk",
        ] {
            assert!(
                patterns.iter().any(|p| wildcard_matches(p, host)),
                "{host} against {patterns:?}"
            );
        }
        assert!(
            !patterns
                .iter()
                .any(|p| wildcard_matches(p, "notcrunchyroll.com"))
        );
        // A real address is sent as a domain, as it always was.
        assert_eq!(name_patterns("crunchyroll.com"), None);
        assert_eq!(name_patterns("  "), None);
    }

    #[test]
    fn an_empty_rule_matches_nothing() {
        assert!(!host_matches("", "youtube.com"));
        assert!(!host_matches("   ", "youtube.com"));
    }

    #[test]
    fn hosts_entries_cover_both_forms_whichever_was_typed() {
        for raw in ["youtube.com", "www.youtube.com", "https://www.youtube.com/"] {
            let entries = hosts_entries(raw);
            assert!(entries.contains(&"youtube.com".to_string()), "{raw}");
            assert!(entries.contains(&"www.youtube.com".to_string()), "{raw}");
        }
    }

    #[test]
    fn hosts_entries_ignores_a_blank_rule() {
        assert!(hosts_entries("  ").is_empty());
    }

    #[test]
    fn a_leading_star_dot_covers_the_apex_as_well() {
        for host in ["youtube.com", "www.youtube.com", "music.youtube.com"] {
            assert!(wildcard_matches("*.youtube.com", host), "{host}");
        }
        assert!(!wildcard_matches("*.youtube.com", "notyoutube.com"));
    }

    #[test]
    fn a_pattern_can_still_aim_at_the_www_label() {
        assert!(wildcard_matches("*.social.*", "www.social.network"));
        assert!(wildcard_matches("www.*", "www.example.com"));
    }

    #[test]
    fn a_bare_star_matches_every_host() {
        for host in ["youtube.com", "a.b.example.org", "localhost"] {
            assert!(wildcard_matches("*", host), "{host}");
        }
    }

    #[test]
    fn wildcard_ignores_case_and_blank_patterns() {
        assert!(wildcard_matches("*.YouTube.COM", "MUSIC.youtube.com"));
        assert!(!wildcard_matches("   ", "youtube.com"));
        assert!(!wildcard_matches("*", ""));
    }

    #[test]
    fn any_host_matches_scans_the_whole_set() {
        let rules = ["reddit.com", "www.youtube.com"];
        assert!(any_host_matches(rules, "music.youtube.com"));
        assert!(any_host_matches(rules, "reddit.com"));
        assert!(!any_host_matches(rules, "example.com"));
    }
}
