//! Browser detection and status types.
//!
//! Provides a constant table of known browsers and their executable names
//! per platform, plus a function to match a running process name to a browser.

use serde::{Deserialize, Serialize};

use crate::extension::BrowserType;

/// Installation instructions until FocusGuard has an approved extension-store listing.
pub const CHROME_STORE_URL: &str = "https://github.com/lukashouthoofd/focusguard#browser-extension";
pub const FIREFOX_STORE_URL: &str = CHROME_STORE_URL;

/// Information about a known browser.
pub struct BrowserInfo {
    /// Which browser this is.
    pub browser_type: BrowserType,
    /// Human-readable name (e.g., "Google Chrome").
    pub display_name: &'static str,
    /// Known executable/process names on the current platform.
    pub exe_names: &'static [&'static str],
}

impl BrowserInfo {
    /// Where to send someone to install the extension for this browser.
    pub fn store_url(&self) -> &'static str {
        match self.browser_type {
            BrowserType::Firefox => FIREFOX_STORE_URL,
            _ => CHROME_STORE_URL,
        }
    }

    /// Short name the OS launcher understands, for opening this browser at a URL.
    pub fn launch_name(&self) -> &'static str {
        match self.browser_type {
            BrowserType::Chrome => "chrome",
            BrowserType::Firefox => "firefox",
            BrowserType::Edge => "msedge",
            BrowserType::Brave => "brave",
            BrowserType::Opera => "opera",
            _ => "chrome",
        }
    }
}

/// Known browsers and their process names per platform.
#[cfg(target_os = "windows")]
pub static KNOWN_BROWSERS: &[BrowserInfo] = &[
    BrowserInfo {
        browser_type: BrowserType::Chrome,
        display_name: "Google Chrome",
        exe_names: &["chrome.exe"],
    },
    BrowserInfo {
        browser_type: BrowserType::Firefox,
        display_name: "Mozilla Firefox",
        exe_names: &["firefox.exe"],
    },
    BrowserInfo {
        browser_type: BrowserType::Edge,
        display_name: "Microsoft Edge",
        exe_names: &["msedge.exe"],
    },
    BrowserInfo {
        browser_type: BrowserType::Brave,
        display_name: "Brave Browser",
        exe_names: &["brave.exe"],
    },
    BrowserInfo {
        browser_type: BrowserType::Opera,
        display_name: "Opera",
        exe_names: &["opera.exe"],
    },
];

#[cfg(target_os = "macos")]
pub static KNOWN_BROWSERS: &[BrowserInfo] = &[
    BrowserInfo {
        browser_type: BrowserType::Chrome,
        display_name: "Google Chrome",
        exe_names: &["Google Chrome"],
    },
    BrowserInfo {
        browser_type: BrowserType::Firefox,
        display_name: "Mozilla Firefox",
        exe_names: &["firefox"],
    },
    BrowserInfo {
        browser_type: BrowserType::Edge,
        display_name: "Microsoft Edge",
        exe_names: &["Microsoft Edge"],
    },
    BrowserInfo {
        browser_type: BrowserType::Brave,
        display_name: "Brave Browser",
        exe_names: &["Brave Browser"],
    },
    BrowserInfo {
        browser_type: BrowserType::Opera,
        display_name: "Opera",
        exe_names: &["Opera"],
    },
];

#[cfg(target_os = "linux")]
pub static KNOWN_BROWSERS: &[BrowserInfo] = &[
    BrowserInfo {
        browser_type: BrowserType::Chrome,
        display_name: "Google Chrome",
        // What the kernel reports is cut to 15 characters, so the last name
        // is `chromium-browser` as a running process actually shows up.
        exe_names: &[
            "chrome",
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "chromium-browse",
        ],
    },
    BrowserInfo {
        browser_type: BrowserType::Firefox,
        display_name: "Mozilla Firefox",
        // Distro packages start `/usr/lib/firefox/firefox`, a launcher that
        // execs `firefox-bin` — so the long-lived main process reports
        // `firefox-bin`, and matching only `firefox` never sees it.
        exe_names: &["firefox", "firefox-bin", "firefox-esr"],
    },
    BrowserInfo {
        browser_type: BrowserType::Edge,
        display_name: "Microsoft Edge",
        // `microsoft-edge` is a wrapper script; the process it leaves
        // running is `msedge`.
        exe_names: &["microsoft-edge", "microsoft-edge-stable", "msedge"],
    },
    BrowserInfo {
        browser_type: BrowserType::Brave,
        display_name: "Brave Browser",
        exe_names: &["brave-browser", "brave"],
    },
    BrowserInfo {
        browser_type: BrowserType::Opera,
        display_name: "Opera",
        exe_names: &["opera"],
    },
];

/// Match a running process name against known browsers.
///
/// Returns the `BrowserInfo` if the process name matches any known browser
/// executable on the current platform. Matching is case-insensitive.
pub fn identify_browser(process_name: &str) -> Option<&'static BrowserInfo> {
    let lower = process_name.to_lowercase();
    KNOWN_BROWSERS
        .iter()
        .find(|b| b.exe_names.iter().any(|name| name.to_lowercase() == lower))
}

/// Start flags only a test tool puts on a browser: Playwright, Puppeteer,
/// Selenium and the WebDriver BiDi drivers.
const AUTOMATION_FLAGS: &[&str] = &[
    "--remote-debugging-pipe",
    "--remote-debugging-port",
    "-remote-debugging-port",
    "--enable-automation",
    "--headless",
    "-headless",
    "--marionette",
    "-marionette",
    "-juggler",
    "-juggler-pipe",
];

/// Whether a browser's command line says a test tool started it (#45).
///
/// Whole arguments only, so an address with the text in it does not count.
pub fn is_automation(command_line: &str) -> bool {
    command_line.split_whitespace().any(|arg| {
        let arg = arg.trim_matches('"');
        let flag = arg.split_once('=').map_or(arg, |(flag, _)| flag);
        AUTOMATION_FLAGS.contains(&flag)
    })
}

/// Status of a detected browser — used for IPC responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserStatusInfo {
    /// Which browser.
    pub browser_type: BrowserType,
    /// Human-readable name.
    pub display_name: String,
    /// Whether at least one process for this browser is running.
    pub is_running: bool,
    /// Whether the FocusGuard extension is connected for this browser.
    pub extension_connected: bool,
    /// Seconds remaining in grace period before enforcement, if applicable.
    pub grace_period_remaining_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browser_started_by_a_test_tool_is_known_by_its_flags() {
        for line in [
            // Playwright, Puppeteer and Selenium's chromedriver.
            r#""C:\pw\chromium-1223\chrome-win64\chrome.exe" --remote-debugging-pipe --no-first-run"#,
            "/usr/bin/chromium --remote-debugging-port=9222 --user-data-dir=/tmp/p",
            "chrome --enable-automation --headless=new about:blank",
            // Firefox under Playwright, geckodriver and BiDi.
            "/opt/firefox/firefox -juggler-pipe -profile /tmp/p",
            "firefox --marionette -profile /tmp/x",
            "firefox.exe --remote-debugging-port 9222",
        ] {
            assert!(is_automation(line), "{line}");
        }
    }

    #[test]
    fn a_browser_someone_opened_is_not() {
        for line in [
            r#""C:\Program Files\Google\Chrome\Application\chrome.exe""#,
            "/usr/bin/firefox https://example.com/?q=--enable-automation",
            "chrome --profile-directory=Default --restore-last-session",
            "",
        ] {
            assert!(!is_automation(line), "{line}");
        }
    }

    #[test]
    fn test_identify_browser_chrome() {
        #[cfg(target_os = "windows")]
        {
            let info = identify_browser("chrome.exe").unwrap();
            assert_eq!(info.browser_type, BrowserType::Chrome);
            assert_eq!(info.display_name, "Google Chrome");
        }
        #[cfg(target_os = "linux")]
        {
            let info = identify_browser("google-chrome").unwrap();
            assert_eq!(info.browser_type, BrowserType::Chrome);
        }
    }

    #[test]
    fn test_identify_browser_case_insensitive() {
        #[cfg(target_os = "windows")]
        {
            let info = identify_browser("Chrome.EXE").unwrap();
            assert_eq!(info.browser_type, BrowserType::Chrome);
        }
    }

    /// The kernel keeps 15 characters of a name, and Edge's wrapper script
    /// execs a binary called `msedge`.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_browsers_are_known_by_the_name_the_kernel_reports() {
        let chromium = identify_browser("chromium-browse").unwrap();
        assert_eq!(chromium.browser_type, BrowserType::Chrome);
        let edge = identify_browser("msedge").unwrap();
        assert_eq!(edge.browser_type, BrowserType::Edge);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_identify_browser_firefox_bin() {
        let info = identify_browser("firefox-bin").unwrap();
        assert_eq!(info.browser_type, BrowserType::Firefox);
    }

    #[test]
    fn test_identify_browser_unknown() {
        assert!(identify_browser("notepad.exe").is_none());
        assert!(identify_browser("some-random-app").is_none());
    }

    #[test]
    fn test_all_known_browsers_detectable() {
        for browser in KNOWN_BROWSERS {
            for exe in browser.exe_names {
                let result = identify_browser(exe);
                assert!(result.is_some(), "Failed to identify {exe}");
                assert_eq!(result.unwrap().browser_type, browser.browser_type);
            }
        }
    }
}
