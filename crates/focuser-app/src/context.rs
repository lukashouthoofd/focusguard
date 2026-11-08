//! Shared runtime state every command executes against.
//!
//! Moved out of `focuser-ui`'s `AppState` so the GUI, the CLI, and the service
//! all drive the same engine through the same handle instead of each opening
//! their own `BlockEngine`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use focuser_common::allowance::AllowanceTick;
use focuser_core::BlockEngine;
use focuser_core::allowance::AllowanceTracker;

/// Events raised by background loops that a frontend should react to
/// (e.g. an OS notification on Pomodoro phase change).
#[derive(Debug, Clone)]
pub enum PomodoroEvent {
    PhaseAdvanced { to: String, cycle: u32 },
    TamperDetected,
}

/// The system-level side effect commands need: pushing the current set of
/// blocked domains into the OS hosts file after a mutation.
///
/// Abstracted as a trait so `focuser-app` does not depend on any particular
/// frontend. The GUI wires in its `blocker::apply_hosts_blocks`, the service
/// wires in its platform blocker, and tests use [`NoopSync`] — which is what
/// makes command behaviour testable without touching a real hosts file.
pub trait SystemSync: Send + Sync {
    fn sync_hosts(&self, domains: &[String]) -> Result<(), String>;

    /// Browsers currently running, by `BrowserType` debug name.
    ///
    /// Process enumeration is per-OS and lives in the frontend crates, so the
    /// default is "nothing detected" — which is the honest answer for a headless
    /// context rather than a guess.
    fn running_browsers(&self) -> Vec<String> {
        Vec::new()
    }

    /// Browsers that have reported in from the extension recently.
    fn connected_browsers(&self) -> Vec<String> {
        Vec::new()
    }

    /// Browsers that have reported in *and* granted the extension "Allow in
    /// Incognito" — a subset of [`Self::connected_browsers`].
    ///
    /// A browser can be connected without being in this set: the extension
    /// is installed and checking in, but a private window in that browser
    /// has nothing blocking it, because Chrome hides incognito windows from
    /// an extension that lacks the permission. That gap is what this exists
    /// to surface.
    fn safely_connected_browsers(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether the OS hosts file can actually be written right now.
    ///
    /// Writing it needs administrator or root. When it fails there is no error
    /// anywhere the user can see, so this is what lets the UI say so. Defaults
    /// to `true` because a headless context has nothing better to report and
    /// should not invent a warning.
    fn hosts_writable(&self) -> bool {
        true
    }
}

/// Does nothing. For tests and for headless contexts with no hosts access.
pub struct NoopSync;

impl SystemSync for NoopSync {
    fn sync_hosts(&self, _domains: &[String]) -> Result<(), String> {
        Ok(())
    }
}

const EXTENSION_HEARTBEAT_TTL: Duration = Duration::from_secs(120);
const INCOGNITO_PERMISSION_TTL: Duration = Duration::from_secs(180);
const MIN_EXTENSION_TICK_INTERVAL: Duration = Duration::from_secs(2);
const MAX_EXTENSION_TICK_CREDIT: Duration = Duration::from_secs(60);
const MAX_EXTENSION_PROFILES_PER_BROWSER: usize = 16;

#[derive(Clone)]
struct ProfileHeartbeat {
    last_seen: Instant,
    permission_report: Option<(Instant, bool)>,
    last_tick: Option<(String, Option<String>, Instant)>,
    last_shared_tick: Option<(String, Option<String>, Instant)>,
}

struct ExtensionTickSample<'a> {
    session_id: Option<u32>,
    browser: &'a str,
    profile_id: &'a str,
    hostname: &'a str,
    url: Option<&'a str>,
    shared_activity: bool,
}

#[derive(Default)]
struct ExtensionPresence {
    sessions: HashMap<Option<u32>, HashMap<String, HashMap<String, ProfileHeartbeat>>>,
}

impl ExtensionPresence {
    fn prune(&mut self, now: Instant) {
        self.sessions.retain(|_, browsers| {
            browsers.retain(|_, profiles| {
                profiles.retain(|_, heartbeat| {
                    now.saturating_duration_since(heartbeat.last_seen) < EXTENSION_HEARTBEAT_TTL
                });
                !profiles.is_empty()
            });
            !browsers.is_empty()
        });
    }

    fn record_heartbeat(
        &mut self,
        session_id: Option<u32>,
        browser: &str,
        profile_id: &str,
        incognito_allowed: Option<bool>,
        now: Instant,
    ) -> bool {
        self.prune(now);
        let profiles = self
            .sessions
            .entry(session_id)
            .or_default()
            .entry(browser.to_string())
            .or_default();
        if !profiles.contains_key(profile_id)
            && profiles.len() >= MAX_EXTENSION_PROFILES_PER_BROWSER
        {
            return false;
        }
        let heartbeat = profiles
            .entry(profile_id.to_string())
            .or_insert(ProfileHeartbeat {
                last_seen: now,
                permission_report: None,
                last_tick: None,
                last_shared_tick: None,
            });
        heartbeat.last_seen = now;
        if let Some(allowed) = incognito_allowed {
            heartbeat.permission_report = Some((now, allowed));
        }
        true
    }

    fn record_tick(
        &mut self,
        sample: ExtensionTickSample<'_>,
        now: Instant,
    ) -> Option<AllowanceTick> {
        self.prune(now);
        let heartbeat = self
            .sessions
            .get_mut(&sample.session_id)
            .and_then(|browsers| browsers.get_mut(sample.browser))
            .and_then(|profiles| profiles.get_mut(sample.profile_id))?;
        let previous_tick = if sample.shared_activity {
            &mut heartbeat.last_shared_tick
        } else {
            &mut heartbeat.last_tick
        };
        let previous = previous_tick.clone();
        if let Some((previous_hostname, previous_url, previous_at)) = previous {
            let elapsed = now.saturating_duration_since(previous_at);
            if elapsed < MIN_EXTENSION_TICK_INTERVAL {
                return None;
            }
            heartbeat.last_seen = now;
            *previous_tick = Some((
                sample.hostname.to_string(),
                sample.url.map(str::to_string),
                now,
            ));
            return Some(AllowanceTick {
                url: previous_url,
                shared_active: sample.shared_activity,
                shared_only: sample.shared_activity,
                hostname: Some(previous_hostname),
                app_exe: None,
                active: true,
                source: "native-extension".into(),
                increment_secs: Some(
                    elapsed.as_secs().min(MAX_EXTENSION_TICK_CREDIT.as_secs()) as u32
                ),
            });
        }
        heartbeat.last_seen = now;
        *previous_tick = Some((
            sample.hostname.to_string(),
            sample.url.map(str::to_string),
            now,
        ));
        None
    }

    fn connected_browsers(&mut self, session_id: Option<u32>, now: Instant) -> Vec<String> {
        self.prune(now);
        let mut browsers: Vec<_> = self
            .sessions
            .get(&session_id)
            .into_iter()
            .flat_map(|browsers| browsers.keys().cloned())
            .collect();
        browsers.sort();
        browsers
    }

    fn safely_connected_browsers(&mut self, session_id: Option<u32>, now: Instant) -> Vec<String> {
        self.prune(now);
        let mut browsers: Vec<_> = self
            .sessions
            .get(&session_id)
            .into_iter()
            .flat_map(|browsers| browsers.iter())
            .filter(|(_, profiles)| {
                !profiles.is_empty()
                    && profiles.values().all(|heartbeat| {
                        !heartbeat.permission_report.is_some_and(|(at, allowed)| {
                            !allowed && now.saturating_duration_since(at) < INCOGNITO_PERMISSION_TTL
                        })
                    })
            })
            .map(|(browser, _)| browser.clone())
            .collect();
        browsers.sort();
        browsers
    }
}

/// Everything a command needs in order to run.
pub struct AppContext {
    pub engine: Mutex<BlockEngine>,
    pub allowance_tracker: AllowanceTracker,
    pomodoro_events: Mutex<Vec<PomodoroEvent>>,
    system: Arc<dyn SystemSync>,
    extension_presence: Mutex<ExtensionPresence>,
    last_hosts_sync_error: Mutex<Option<String>>,
}

impl AppContext {
    /// Build a context with a real system-sync implementation.
    pub fn new(engine: BlockEngine, system: Arc<dyn SystemSync>) -> Self {
        Self {
            engine: Mutex::new(engine),
            allowance_tracker: AllowanceTracker::new(),
            pomodoro_events: Mutex::new(Vec::new()),
            system,
            extension_presence: Mutex::new(ExtensionPresence::default()),
            last_hosts_sync_error: Mutex::new(None),
        }
    }

    /// Build a context that performs no system-level side effects.
    pub fn new_headless(engine: BlockEngine) -> Self {
        Self::new(engine, Arc::new(NoopSync))
    }

    /// Re-derive the blocked-domain set from the engine and push it to the OS.
    ///
    /// Call after any mutation that can change which domains are blocked.
    pub fn sync_hosts(&self, engine: &BlockEngine) -> Result<(), String> {
        self.sync_hosts_with(&engine.collect_blocked_domains())
    }

    /// Push an explicit domain set, bypassing the engine.
    ///
    /// Only used to write an empty set, which is how "unblock everything" is
    /// expressed — the sync replaces FocusGuard's hosts section wholesale, so an
    /// empty list clears it.
    /// Whether the hosts file is writable. See [`SystemSync::hosts_writable`].
    pub fn hosts_writable(&self) -> bool {
        self.system.hosts_writable()
    }

    pub fn sync_hosts_with(&self, domains: &[String]) -> Result<(), String> {
        let result = self.system.sync_hosts(domains);
        if let Ok(mut last_error) = self.last_hosts_sync_error.lock() {
            *last_error = result.as_ref().err().cloned();
        }
        result
    }

    pub fn hosts_sync_error(&self) -> Option<String> {
        self.last_hosts_sync_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    pub fn running_browsers(&self) -> Vec<String> {
        self.system.running_browsers()
    }

    pub fn connected_browsers(&self) -> Vec<String> {
        self.connected_browsers_for_session(None)
    }

    pub fn connected_browsers_for_session(&self, session_id: Option<u32>) -> Vec<String> {
        let mut connected = self.system.connected_browsers();
        if let Ok(mut presence) = self.extension_presence.lock() {
            for browser in presence.connected_browsers(session_id, Instant::now()) {
                if !connected.contains(&browser) {
                    connected.push(browser);
                }
            }
        }
        connected
    }

    pub fn safely_connected_browsers(&self) -> Vec<String> {
        self.safely_connected_browsers_for_session(None)
    }

    pub fn safely_connected_browsers_for_session(&self, session_id: Option<u32>) -> Vec<String> {
        let mut safely_connected = self.system.safely_connected_browsers();
        if let Ok(mut presence) = self.extension_presence.lock() {
            for browser in presence.safely_connected_browsers(session_id, Instant::now()) {
                if !safely_connected.contains(&browser) {
                    safely_connected.push(browser);
                }
            }
        }
        safely_connected
    }

    pub fn record_extension_heartbeat(
        &self,
        session_id: Option<u32>,
        browser: &str,
        profile_id: &str,
        incognito_allowed: Option<bool>,
    ) -> bool {
        self.extension_presence.lock().is_ok_and(|mut presence| {
            presence.record_heartbeat(
                session_id,
                browser,
                profile_id,
                incognito_allowed,
                Instant::now(),
            )
        })
    }

    pub fn extension_profile_connected(
        &self,
        session_id: Option<u32>,
        browser: &str,
        profile_id: &str,
    ) -> bool {
        self.extension_presence.lock().is_ok_and(|mut presence| {
            presence.prune(Instant::now());
            presence
                .sessions
                .get(&session_id)
                .and_then(|browsers| browsers.get(browser))
                .is_some_and(|profiles| profiles.contains_key(profile_id))
        })
    }

    /// Credit a profile's active-tab stream using service-measured elapsed
    /// time. Normal and shared-activity streams keep independent baselines.
    pub fn record_extension_tick(
        &self,
        session_id: Option<u32>,
        browser: &str,
        profile_id: &str,
        hostname: &str,
        url: Option<&str>,
        shared_activity: bool,
    ) -> Option<AllowanceTick> {
        self.extension_presence
            .lock()
            .ok()
            .and_then(|mut presence| {
                presence.record_tick(
                    ExtensionTickSample {
                        session_id,
                        browser,
                        profile_id,
                        hostname,
                        url,
                        shared_activity,
                    },
                    Instant::now(),
                )
            })
    }

    /// Rebuild durable allowance exhaustion before the service begins serving
    /// browser rule requests after startup.
    pub fn rebuild_allowance_state(&self) -> Result<(), String> {
        let engine = self
            .engine
            .lock()
            .map_err(|_| "engine lock poisoned".to_string())?;
        self.allowance_tracker
            .rebuild_from_db(engine.db())
            .map_err(|error| error.to_string())
    }

    /// Domains currently exempt from blocking because an allowance still has
    /// time left today.
    ///
    /// **Empty when no browser extension is connected.** Browser time is
    /// measured by the extension and nothing else — the desktop watcher only
    /// sees processes, not tabs. Granting the exemption anyway would turn
    /// "thirty minutes a day" into "unlimited", because the clock would never
    /// start. Refusing it is the safe direction: the site stays blocked and the
    /// UI says why.
    pub fn allowance_exempt_domains(&self, engine: &BlockEngine) -> Vec<String> {
        self.allowance_exempt_domains_for_session(engine, None)
    }

    pub fn allowance_exempt_domains_for_session(
        &self,
        engine: &BlockEngine,
        session_id: Option<u32>,
    ) -> Vec<String> {
        if self.connected_browsers_for_session(session_id).is_empty() {
            return Vec::new();
        }
        self.allowance_tracker.active_allowance_domains(engine)
    }

    pub fn push_pomodoro_event(&self, event: PomodoroEvent) {
        if let Ok(mut buf) = self.pomodoro_events.lock() {
            buf.push(event);
        }
    }

    pub fn drain_pomodoro_events(&self) -> Vec<PomodoroEvent> {
        self.pomodoro_events
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focuser_core::Database;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn ctx() -> AppContext {
        let db = Database::open_in_memory().unwrap();
        AppContext::new_headless(BlockEngine::new(db).unwrap())
    }

    #[test]
    fn extension_presence_is_session_scoped_and_unknown_permission_is_not_a_gap() {
        let mut presence = ExtensionPresence::default();
        let start = Instant::now();
        presence.record_heartbeat(None, "Chrome", "p1", None, start);
        assert_eq!(presence.connected_browsers(None, start), ["Chrome"]);
        assert_eq!(
            presence.connected_browsers(Some(4), start),
            Vec::<String>::new()
        );
        assert_eq!(presence.safely_connected_browsers(None, start), ["Chrome"]);

        presence.record_heartbeat(Some(4), "Chrome", "p2", Some(false), start);
        assert!(
            presence
                .safely_connected_browsers(Some(4), start)
                .is_empty()
        );
        // A heartbeat with no new permission answer must not renew the old no.
        presence.record_heartbeat(
            Some(4),
            "Chrome",
            "p2",
            None,
            start + Duration::from_secs(100),
        );
        assert!(
            presence
                .safely_connected_browsers(Some(4), start + Duration::from_secs(100))
                .is_empty()
        );
        assert_eq!(
            presence.safely_connected_browsers(Some(4), start + Duration::from_secs(181)),
            ["Chrome"]
        );
    }

    #[test]
    fn extension_profile_cap_never_evicts_a_live_profile() {
        let mut presence = ExtensionPresence::default();
        let start = Instant::now();
        for index in 0..MAX_EXTENSION_PROFILES_PER_BROWSER {
            presence.record_heartbeat(
                Some(1),
                "Chrome",
                &format!("profile-{index}"),
                Some(false),
                start + Duration::from_secs(index as u64),
            );
        }
        presence.record_heartbeat(
            Some(1),
            "Chrome",
            "profile-overflow",
            Some(true),
            start + Duration::from_secs(30),
        );
        let profiles = &presence.sessions[&Some(1)]["Chrome"];
        assert_eq!(profiles.len(), MAX_EXTENSION_PROFILES_PER_BROWSER);
        assert!(profiles.contains_key("profile-0"));
        assert!(!profiles.contains_key("profile-overflow"));
        assert!(
            presence
                .safely_connected_browsers(Some(1), start + Duration::from_secs(30))
                .is_empty()
        );
    }

    #[test]
    fn native_allowance_ticks_use_server_elapsed_time_and_separate_streams() {
        fn tick(
            presence: &mut ExtensionPresence,
            session_id: Option<u32>,
            hostname: &str,
            url: Option<&str>,
            shared_activity: bool,
            now: Instant,
        ) -> Option<AllowanceTick> {
            presence.record_tick(
                ExtensionTickSample {
                    session_id,
                    browser: "Firefox",
                    profile_id: "p1",
                    hostname,
                    url,
                    shared_activity,
                },
                now,
            )
        }

        let mut presence = ExtensionPresence::default();
        let start = Instant::now();
        presence.record_heartbeat(Some(9), "Firefox", "p1", Some(true), start);
        assert!(
            tick(
                &mut presence,
                Some(9),
                "first.example",
                Some("https://first.example/a"),
                false,
                start
            )
            .is_none()
        );
        assert!(tick(&mut presence, Some(9), "shared.example", None, true, start).is_none());

        let normal = tick(
            &mut presence,
            Some(9),
            "second.example",
            Some("https://second.example/b"),
            false,
            start + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(normal.hostname.as_deref(), Some("first.example"));
        assert_eq!(normal.url.as_deref(), Some("https://first.example/a"));
        assert_eq!(normal.increment_secs, Some(2));
        assert!(!normal.shared_only);

        let shared = tick(
            &mut presence,
            Some(9),
            "shared.example",
            Some("https://shared.example/"),
            true,
            start + Duration::from_secs(3),
        )
        .unwrap();
        assert_eq!(shared.hostname.as_deref(), Some("shared.example"));
        assert_eq!(shared.increment_secs, Some(3));
        assert!(shared.shared_active && shared.shared_only);

        assert!(
            tick(
                &mut presence,
                Some(9),
                "other.example",
                None,
                false,
                start + Duration::from_millis(2500)
            )
            .is_none()
        );
        let capped = tick(
            &mut presence,
            Some(9),
            "third.example",
            None,
            false,
            start + Duration::from_secs(100),
        )
        .unwrap();
        assert_eq!(capped.increment_secs, Some(60));
        assert!(tick(&mut presence, Some(4), "first.example", None, false, start).is_none());
    }

    #[test]
    fn drain_returns_pushed_events_then_empties() {
        let ctx = ctx();
        ctx.push_pomodoro_event(PomodoroEvent::TamperDetected);
        ctx.push_pomodoro_event(PomodoroEvent::PhaseAdvanced {
            to: "work".into(),
            cycle: 2,
        });

        assert_eq!(ctx.drain_pomodoro_events().len(), 2);
        assert!(
            ctx.drain_pomodoro_events().is_empty(),
            "drain must consume the buffer"
        );
    }

    #[derive(Default)]
    struct CountingSync(AtomicUsize);

    impl SystemSync for CountingSync {
        fn sync_hosts(&self, _domains: &[String]) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn sync_hosts_delegates_to_the_injected_impl() {
        let counter = Arc::new(CountingSync::default());
        let db = Database::open_in_memory().unwrap();
        let engine = BlockEngine::new(db).unwrap();
        let ctx = AppContext::new(engine, counter.clone());

        {
            let eng = ctx.engine.lock().unwrap();
            ctx.sync_hosts(&eng).unwrap();
        }

        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    }

    struct ToggleFailure(AtomicBool);

    impl SystemSync for ToggleFailure {
        fn sync_hosts(&self, _domains: &[String]) -> Result<(), String> {
            if self.0.load(Ordering::SeqCst) {
                Err("fixture write failure".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn hosts_sync_error_is_visible_and_cleared_after_recovery() {
        let system = Arc::new(ToggleFailure(AtomicBool::new(true)));
        let database = Database::open_in_memory().unwrap();
        let engine = BlockEngine::new(database).unwrap();
        let context = AppContext::new(engine, system.clone());

        assert_eq!(
            context.sync_hosts_with(&[]).unwrap_err(),
            "fixture write failure"
        );
        assert_eq!(
            context.hosts_sync_error().as_deref(),
            Some("fixture write failure")
        );

        system.0.store(false, Ordering::SeqCst);
        context.sync_hosts_with(&[]).unwrap();
        assert_eq!(context.hosts_sync_error(), None);
    }
}
