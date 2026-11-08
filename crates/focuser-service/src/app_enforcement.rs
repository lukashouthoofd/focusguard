//! Pure, session-scoped coordination for executable-name app rules.
//!
//! The pure coordinators do not enumerate or terminate processes. The
//! service adapter reads durable rules and asks the common Windows identity
//! layer for complete snapshots of candidate executable names; process action
//! still goes through identity-revalidating OS APIs.

use std::{
    collections::{HashMap, HashSet},
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use focuser_app::AppContext;
use focuser_common::{
    allowance::{Allowance, AllowanceMatch},
    process_identity::{ProcessIdentity, list_processes_matching_complete, terminate_if_unchanged},
    types::{AppMatchType, BlockList},
};

/// Preserve the desktop's existing opportunity for an application to save.
pub const CLOSE_GRACE: Duration = Duration::from_secs(30);

/// Rate-limit retries when helper delivery has not yet been acknowledged.
/// Unacknowledged requests are retried after this interval while the grace
/// deadline has not elapsed and the session helper is available.
pub const CLOSE_REQUEST_RETRY_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperHealth {
    Connected,
    Unavailable,
}

/// App-rule projection that the service coordinator is allowed to enforce.
///
/// The UI still owns per-app allowance accounting, and `BlockEngine` can grant
/// app access while a shared allowance is active. Therefore callers must only
/// put rules from active lists with no app/shared allowance semantics in
/// `unmetered_executable_names`. Rules deferred for allowance tracking and
/// rules using unsupported match kinds are counted explicitly so callers can
/// report degraded coverage instead of silently treating them as enforced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppRuleSnapshot {
    pub unmetered_executable_names: Vec<String>,
    pub deferred_allowance_rule_count: usize,
    pub unsupported_match_rule_count: usize,
}

/// Project durable app rules to the first service-owned enforcement surface.
///
/// Only executable-name rules from currently active lists are eligible. Lists
/// with a configured shared allowance and names covered by an enabled
/// per-app allowance are deferred because this service loop does not own the
/// interactive usage clock. Path, title, and bundle-id rules remain explicit
/// unsupported coverage. The caller must load both inputs strictly; a database
/// decode error must skip the cycle rather than provide a partial projection.
pub fn project_app_rules(lists: &[BlockList], allowances: &[Allowance]) -> AppRuleSnapshot {
    let allowance_names: HashSet<String> = allowances
        .iter()
        .filter(|allowance| allowance.enabled)
        .filter_map(|allowance| match &allowance.target {
            AllowanceMatch::AppExecutable(name) => Some(name.to_lowercase()),
            AllowanceMatch::Domain(_) => None,
        })
        .collect();

    let mut snapshot = AppRuleSnapshot::default();
    for list in lists.iter().filter(|list| list.is_effectively_active()) {
        for rule in list.applications.iter().filter(|rule| rule.enabled) {
            match &rule.match_type {
                AppMatchType::ExecutableName(name) => {
                    if list.shared_allowance.is_some()
                        || allowance_names.contains(&name.to_lowercase())
                    {
                        snapshot.deferred_allowance_rule_count += 1;
                    } else {
                        snapshot.unmetered_executable_names.push(name.clone());
                    }
                }
                AppMatchType::ExecutablePath(_)
                | AppMatchType::WindowTitle(_)
                | AppMatchType::BundleId(_) => {
                    snapshot.unsupported_match_rule_count += 1;
                }
            }
        }
    }
    snapshot
        .unmetered_executable_names
        .sort_by_key(|name| name.to_lowercase());
    snapshot
        .unmetered_executable_names
        .dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    snapshot
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEnforcementStatus {
    /// This coordinator is scoped to exactly one interactive user session.
    pub session_id: u32,
    pub helper: HelperHealth,
    /// A complete process snapshot was supplied this cycle. If false, the
    /// coordinator retains state but issues no close or termination actions.
    pub process_snapshot_available: bool,
    pub pending_targets: usize,
    /// Blocked targets whose WM_CLOSE request lacks helper acknowledgement.
    pub waiting_for_helper: usize,
    /// Deadlines have elapsed, but the caller must still revalidate identity
    /// before terminating through `ProcessIdentity::terminate_if_unchanged`.
    pub grace_expired_targets: usize,
    /// Active app rules intentionally omitted because this service has no
    /// allowance-usage tracker. These rules are not enforced by this module.
    pub deferred_allowance_rule_count: usize,
    /// Active app rules whose match kind is outside this first milestone.
    pub unsupported_match_rule_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEnforcementUpdate {
    /// Send these to the helper attached to this session. They repeat until
    /// `acknowledge_close_request` confirms delivery for the exact identity,
    /// at most once per `CLOSE_REQUEST_RETRY_INTERVAL`; constructing or
    /// attempting to write a request is not an acknowledgement.
    pub close_requests: Vec<ProcessIdentity>,
    /// These targets have received their full grace period. They are returned
    /// on each available-snapshot cycle until they exit or become unblocked,
    /// so a transient failure in the eventual termination adapter is retryable.
    /// This module itself never terminates a process.
    pub ready_for_termination: Vec<ProcessIdentity>,
    pub status: AppEnforcementStatus,
}

#[derive(Debug, Clone)]
struct PendingTarget {
    deadline: Instant,
    close_delivery: CloseDeliveryState,
}

impl PendingTarget {
    fn close_retry_due(&self, now: Instant) -> bool {
        match self.close_delivery {
            CloseDeliveryState::NotAttempted => true,
            CloseDeliveryState::AwaitingAcknowledgement { last_attempt } => {
                now.saturating_duration_since(last_attempt) >= CLOSE_REQUEST_RETRY_INTERVAL
            }
            CloseDeliveryState::Acknowledged => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseDeliveryState {
    NotAttempted,
    AwaitingAcknowledgement { last_attempt: Instant },
    Acknowledged,
}

/// Coordinates close requests for one configured owner SID and one user
/// session. The service PID is excluded even if a caller supplies an otherwise
/// matching process snapshot.
#[derive(Debug)]
pub struct AppEnforcementCoordinator {
    owner_sid: String,
    session_id: u32,
    service_pid: u32,
    pending: HashMap<ProcessIdentity, PendingTarget>,
}

impl AppEnforcementCoordinator {
    pub fn new(
        owner_sid: impl Into<String>,
        session_id: u32,
        service_pid: u32,
    ) -> Result<Self, &'static str> {
        let owner_sid = owner_sid.into();
        if owner_sid.is_empty() {
            return Err("configured owner SID must not be empty");
        }
        if session_id == 0 {
            return Err("app enforcement requires an interactive session");
        }
        if service_pid <= 4 {
            return Err("service PID must be greater than the protected system PIDs");
        }

        Ok(Self {
            owner_sid,
            session_id,
            service_pid,
            pending: HashMap::new(),
        })
    }

    /// Advance the close state machine.
    ///
    /// `rules` must be a projection of currently active lists, with only
    /// allowance-free executable-name matches included for enforcement. Other
    /// match kinds (paths, titles, bundle IDs) and allowance-aware rules are
    /// reported as deferred/unsupported and are intentionally outside this
    /// first service milestone. `processes == None` means enumeration failed
    /// or returned incomplete data; it is distinct from `Some(&[])`, which
    /// means a complete scan found no processes.
    pub fn reconcile(
        &mut self,
        now: Instant,
        rules: &AppRuleSnapshot,
        processes: Option<&[ProcessIdentity]>,
        helper_available: bool,
    ) -> AppEnforcementUpdate {
        let helper = if helper_available {
            HelperHealth::Connected
        } else {
            HelperHealth::Unavailable
        };

        let Some(processes) = processes else {
            // A failed enumeration cannot cancel a grace period or justify a
            // termination candidate. Preserve deadlines for the next complete
            // scan, and expose the degraded source in status.
            return self.update_result(now, helper, rules, false, Vec::new(), Vec::new());
        };

        let rule_names: HashSet<String> = rules
            .unmetered_executable_names
            .iter()
            .map(|name| name.to_lowercase())
            .collect();
        let targets: HashSet<ProcessIdentity> = processes
            .iter()
            .filter(|process| self.is_in_scope(process))
            .filter(|process| rule_names.contains(&process.image_name.to_lowercase()))
            .cloned()
            .collect();

        // Exact ProcessIdentity keys include PID, owner, session, image path,
        // and creation time. A recycled PID becomes a new target and starts a
        // fresh deadline; the old target is discarded.
        self.pending
            .retain(|identity, _| targets.contains(identity));
        for identity in &targets {
            self.pending
                .entry(identity.clone())
                .or_insert(PendingTarget {
                    deadline: now + CLOSE_GRACE,
                    close_delivery: CloseDeliveryState::NotAttempted,
                });
        }

        let mut close_requests = Vec::new();
        let mut ready_for_termination = Vec::new();
        for (identity, pending) in &mut self.pending {
            if now >= pending.deadline {
                ready_for_termination.push(identity.clone());
            } else if helper_available && pending.close_retry_due(now) {
                // No acknowledgement means the send may have failed or its
                // outcome is unknown. WM_CLOSE is safe to retry for the same
                // process identity, but retries are rate-limited to avoid
                // flooding a slow helper. Helper absence never resets the
                // deadline or turns into an immediate-kill case.
                pending.close_delivery =
                    CloseDeliveryState::AwaitingAcknowledgement { last_attempt: now };
                close_requests.push(identity.clone());
            }
        }

        close_requests.sort_by_key(|identity| (identity.pid, identity.creation_time));
        ready_for_termination.sort_by_key(|identity| (identity.pid, identity.creation_time));
        self.update_result(
            now,
            helper,
            rules,
            true,
            close_requests,
            ready_for_termination,
        )
    }

    fn is_in_scope(&self, process: &ProcessIdentity) -> bool {
        process.pid > 4
            && process.pid != std::process::id()
            && process.pid != self.service_pid
            && process.session_id != 0
            && process.session_id == self.session_id
            && process.owner_sid == self.owner_sid
            && !process.is_focusguard_process()
    }

    /// Mark a helper close attempt delivered for a currently pending process.
    ///
    /// The caller must invoke this only after receiving an authenticated
    /// acknowledgement from the session helper. A stale acknowledgement for a
    /// process identity already removed by reconciliation is rejected.
    pub fn acknowledge_close_request(&mut self, identity: &ProcessIdentity) -> bool {
        let Some(pending) = self.pending.get_mut(identity) else {
            return false;
        };
        if !matches!(
            pending.close_delivery,
            CloseDeliveryState::AwaitingAcknowledgement { .. }
        ) {
            return false;
        }
        pending.close_delivery = CloseDeliveryState::Acknowledged;
        true
    }

    fn update_result(
        &self,
        now: Instant,
        helper: HelperHealth,
        rules: &AppRuleSnapshot,
        process_snapshot_available: bool,
        close_requests: Vec<ProcessIdentity>,
        ready_for_termination: Vec<ProcessIdentity>,
    ) -> AppEnforcementUpdate {
        let waiting_for_helper = self
            .pending
            .values()
            .filter(|pending| {
                pending.close_delivery != CloseDeliveryState::Acknowledged && now < pending.deadline
            })
            .count();
        let grace_expired_targets = self
            .pending
            .values()
            .filter(|pending| now >= pending.deadline)
            .count();

        AppEnforcementUpdate {
            close_requests,
            ready_for_termination,
            status: AppEnforcementStatus {
                session_id: self.session_id,
                helper,
                process_snapshot_available,
                pending_targets: self.pending.len(),
                waiting_for_helper,
                grace_expired_targets,
                deferred_allowance_rule_count: rules.deferred_allowance_rule_count,
                unsupported_match_rule_count: rules.unsupported_match_rule_count,
            },
        }
    }
}

/// Holds one independent coordinator per interactive session belonging to the
/// configured owner. The service supplies a complete snapshot of candidate
/// executable names; this owner/session filter creates and drops per-session state without
/// routing app-rule mutation or process-action authority through the helper.
#[derive(Debug)]
pub struct AppEnforcementRuntime {
    owner_sid: String,
    service_pid: u32,
    sessions: HashMap<u32, AppEnforcementCoordinator>,
}

impl AppEnforcementRuntime {
    pub fn new(owner_sid: impl Into<String>, service_pid: u32) -> Result<Self, &'static str> {
        let owner_sid = owner_sid.into();
        if owner_sid.is_empty() {
            return Err("configured owner SID must not be empty");
        }
        if service_pid <= 4 {
            return Err("service PID must be greater than the protected system PIDs");
        }
        Ok(Self {
            owner_sid,
            service_pid,
            sessions: HashMap::new(),
        })
    }

    /// Reconcile per-session coordinators. `None` means enumeration was
    /// incomplete; in that case state is retained and no process actions are
    /// returned. The caller must invoke `terminate_if_unchanged` for every
    /// returned candidate before taking process action.
    pub fn reconcile(
        &mut self,
        now: Instant,
        rules: &AppRuleSnapshot,
        processes: Option<&[ProcessIdentity]>,
        helper_available: bool,
    ) -> Vec<AppEnforcementUpdate> {
        let Some(processes) = processes else {
            return self
                .sessions
                .values_mut()
                .map(|coordinator| coordinator.reconcile(now, rules, None, helper_available))
                .collect();
        };

        let live_sessions: HashSet<u32> = processes
            .iter()
            .filter(|process| {
                process.pid > 4
                    && process.pid != std::process::id()
                    && process.pid != self.service_pid
                    && process.session_id != 0
                    && process.owner_sid == self.owner_sid
                    && !process.is_focusguard_process()
            })
            .map(|process| process.session_id)
            .collect();

        for session_id in &live_sessions {
            self.sessions.entry(*session_id).or_insert_with(|| {
                AppEnforcementCoordinator::new(
                    self.owner_sid.clone(),
                    *session_id,
                    self.service_pid,
                )
                .expect("runtime has valid owner SID and service PID")
            });
        }
        self.sessions
            .retain(|session_id, _| live_sessions.contains(session_id));

        let mut sessions: Vec<_> = self.sessions.iter_mut().collect();
        sessions.sort_by_key(|(session_id, _)| **session_id);
        sessions
            .into_iter()
            .map(|(_, coordinator)| {
                coordinator.reconcile(now, rules, Some(processes), helper_available)
            })
            .collect()
    }
}

/// One service-side app-enforcement cycle. Incomplete process enumeration is
/// surfaced and produces updates with no actions; it does not cancel pending
/// grace periods or permit termination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEnforcementCycle {
    /// Coverage projection for this cycle, including deferred allowances and
    /// unsupported match kinds even when no matching process is running.
    pub policy: AppRuleSnapshot,
    pub updates: Vec<AppEnforcementUpdate>,
    pub process_snapshot_error: Option<String>,
}

/// Durable-policy adapter around the pure multi-session coordinator. It reads
/// strict block-list and allowance snapshots from the service-owned database,
/// then revalidates every expired target with `terminate_if_unchanged`.
#[derive(Debug)]
pub struct ServiceAppEnforcement {
    owner_sid: String,
    runtime: AppEnforcementRuntime,
}

impl ServiceAppEnforcement {
    pub fn new(owner_sid: impl Into<String>, service_pid: u32) -> Result<Self, &'static str> {
        let owner_sid = owner_sid.into();
        let runtime = AppEnforcementRuntime::new(owner_sid.clone(), service_pid)?;
        Ok(Self { owner_sid, runtime })
    }

    /// Enumerate a complete snapshot for the currently eligible executable
    /// names and reconcile it against the service database. The interactive helper is not implemented yet, so
    /// this method never sends WM_CLOSE; due targets are terminated only after
    /// the coordinator's grace period and identity revalidation.
    pub fn reconcile_system_once(
        &mut self,
        context: &AppContext,
        now: Instant,
    ) -> Result<AppEnforcementCycle, String> {
        let rules = service_app_rule_snapshot(context)?;
        let (processes, process_snapshot_error) =
            match list_processes_matching_complete(&rules.unmetered_executable_names) {
                Ok(processes) => (Some(processes), None),
                Err(error) => (None, Some(error.to_string())),
            };
        let updates = self.reconcile_with_rules(
            &rules,
            now,
            processes.as_deref(),
            false,
            terminate_if_unchanged,
        )?;
        Ok(AppEnforcementCycle {
            policy: rules,
            updates,
            process_snapshot_error,
        })
    }

    /// Reconcile an injected snapshot. Tests use this to prove that only
    /// grace-expired identities reach the termination adapter; production
    /// should prefer `reconcile_system_once` so identities are revalidated by
    /// the Windows handle-based terminator.
    pub fn reconcile_snapshot(
        &mut self,
        context: &AppContext,
        now: Instant,
        processes: Option<&[ProcessIdentity]>,
        helper_available: bool,
        terminate: impl FnMut(&ProcessIdentity, &str) -> io::Result<bool>,
    ) -> Result<Vec<AppEnforcementUpdate>, String> {
        let rules = service_app_rule_snapshot(context)?;
        self.reconcile_with_rules(&rules, now, processes, helper_available, terminate)
    }

    fn reconcile_with_rules(
        &mut self,
        rules: &AppRuleSnapshot,
        now: Instant,
        processes: Option<&[ProcessIdentity]>,
        helper_available: bool,
        mut terminate: impl FnMut(&ProcessIdentity, &str) -> io::Result<bool>,
    ) -> Result<Vec<AppEnforcementUpdate>, String> {
        let updates = self
            .runtime
            .reconcile(now, rules, processes, helper_available);

        for update in &updates {
            for identity in &update.ready_for_termination {
                match terminate(identity, &self.owner_sid) {
                    Ok(true) => tracing::info!(
                        pid = identity.pid,
                        session_id = identity.session_id,
                        "terminated an app blocked by service policy after its grace period"
                    ),
                    Ok(false) => tracing::debug!(
                        pid = identity.pid,
                        session_id = identity.session_id,
                        "app termination skipped because the process identity changed or is protected"
                    ),
                    Err(error) => tracing::debug!(
                        pid = identity.pid,
                        session_id = identity.session_id,
                        %error,
                        "app termination attempt failed; it will be retried after identity revalidation"
                    ),
                }
            }
        }
        Ok(updates)
    }
}

fn service_app_rule_snapshot(context: &AppContext) -> Result<AppRuleSnapshot, String> {
    let mut engine = context
        .engine
        .lock()
        .map_err(|_| "engine lock poisoned".to_string())?;
    engine.refresh().map_err(|error| error.to_string())?;
    let lists = engine
        .db()
        .list_block_lists_strict()
        .map_err(|error| error.to_string())?;
    let allowances = engine
        .db()
        .list_allowances()
        .map_err(|error| error.to_string())?;
    Ok(project_app_rules(&lists, &allowances))
}

/// Service-loop entry point for the app enforcement adapter. The SCM host can
/// select this alongside its existing policy and pipe futures once startup is
/// ready. It deliberately remains a separate loop so app policy can be
/// enabled independently after its runtime behavior is reviewed.
pub async fn run_app_enforcement_reconciler(
    context: Arc<AppContext>,
    owner_sid: impl Into<String>,
    service_pid: u32,
) {
    let mut enforcement = match ServiceAppEnforcement::new(owner_sid, service_pid) {
        Ok(enforcement) => enforcement,
        Err(error) => {
            tracing::error!(%error, "could not initialize service app enforcement");
            return;
        }
    };
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut last_policy: Option<AppRuleSnapshot> = None;
    let mut last_policy_error: Option<String> = None;
    let mut last_process_error: Option<String> = None;
    let mut warned_helper_sessions = HashSet::new();

    loop {
        interval.tick().await;
        match enforcement.reconcile_system_once(&context, Instant::now()) {
            Ok(cycle) => {
                if let Some(error) = cycle.process_snapshot_error {
                    if last_process_error.as_deref() != Some(error.as_str()) {
                        tracing::warn!(%error, "app process snapshot is incomplete; no app targets will be terminated");
                    }
                    last_process_error = Some(error);
                } else if last_process_error.take().is_some() {
                    tracing::info!("app process enumeration recovered");
                }

                if last_policy.as_ref() != Some(&cycle.policy) {
                    if cycle.policy.deferred_allowance_rule_count > 0
                        || cycle.policy.unsupported_match_rule_count > 0
                    {
                        tracing::warn!(
                            supported_executable_rules =
                                cycle.policy.unmetered_executable_names.len(),
                            deferred_allowance_rules = cycle.policy.deferred_allowance_rule_count,
                            unsupported_match_rules = cycle.policy.unsupported_match_rule_count,
                            "service app enforcement has deferred or unsupported app rules"
                        );
                    } else {
                        tracing::info!(
                            supported_executable_rules =
                                cycle.policy.unmetered_executable_names.len(),
                            "service app enforcement policy refreshed"
                        );
                    }
                    last_policy = Some(cycle.policy);
                }
                last_policy_error = None;

                let live_sessions: HashSet<u32> = cycle
                    .updates
                    .iter()
                    .map(|update| update.status.session_id)
                    .collect();
                warned_helper_sessions.retain(|session| live_sessions.contains(session));
                for update in cycle.updates {
                    if update.status.waiting_for_helper > 0
                        && warned_helper_sessions.insert(update.status.session_id)
                    {
                        tracing::warn!(
                            session_id = update.status.session_id,
                            pending_targets = update.status.waiting_for_helper,
                            "interactive WM_CLOSE helper is unavailable; service termination waits for the grace period"
                        );
                    }
                }
            }
            Err(error) => {
                if last_policy_error.as_deref() != Some(error.as_str()) {
                    tracing::warn!(%error, "could not read durable app policy; app enforcement is paused");
                }
                last_policy_error = Some(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "S-1-5-21-100-200-300-1001";
    const SESSION: u32 = 7;
    const SERVICE_PID: u32 = 500;

    fn coordinator() -> AppEnforcementCoordinator {
        AppEnforcementCoordinator::new(OWNER, SESSION, SERVICE_PID).unwrap()
    }

    fn context_with_durable_app_rule(name: &str) -> AppContext {
        let database = focuser_core::Database::open_in_memory().unwrap();
        let mut list = BlockList::new("Apps");
        list.applications
            .push(focuser_common::types::AppRule::executable(name));
        database.create_block_list(&list).unwrap();
        AppContext::new_headless(focuser_core::BlockEngine::new(database).unwrap())
    }

    fn process(pid: u32, name: &str, creation_time: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            session_id: SESSION,
            owner_sid: OWNER.into(),
            image_path: format!(r"C:\Apps\{name}"),
            image_name: name.into(),
            creation_time,
        }
    }

    fn rules(names: &[&str]) -> AppRuleSnapshot {
        AppRuleSnapshot {
            unmetered_executable_names: names.iter().map(|name| (*name).into()).collect(),
            ..AppRuleSnapshot::default()
        }
    }

    #[test]
    fn projects_active_name_rules_and_reports_allowance_and_matcher_gaps() {
        let mut plain = BlockList::new("Plain");
        plain
            .applications
            .push(focuser_common::types::AppRule::executable("game.exe"));
        plain
            .applications
            .push(focuser_common::types::AppRule::executable("Discord.EXE"));
        plain
            .applications
            .push(focuser_common::types::AppRule::path(r"C:\Games\path.exe"));
        plain
            .applications
            .push(focuser_common::types::AppRule::window_title("Minecraft"));
        plain.applications.push(focuser_common::types::AppRule {
            id: uuid::Uuid::new_v4(),
            match_type: AppMatchType::BundleId("com.example.game".into()),
            enabled: true,
        });

        let mut shared = BlockList::new("Shared");
        shared.shared_allowance =
            Some(focuser_common::allowance::SharedAllowanceConfig { minutes: 10 });
        shared
            .applications
            .push(focuser_common::types::AppRule::executable("shared.exe"));

        let mut disabled = BlockList::new("Disabled");
        disabled.enabled = false;
        disabled
            .applications
            .push(focuser_common::types::AppRule::executable("disabled.exe"));

        let allowances = [Allowance::new(
            AllowanceMatch::AppExecutable("discord.exe".into()),
            600,
            true,
        )];
        let snapshot = project_app_rules(&[plain, shared, disabled], &allowances);

        assert_eq!(snapshot.unmetered_executable_names, ["game.exe"]);
        assert_eq!(snapshot.deferred_allowance_rule_count, 2);
        assert_eq!(snapshot.unsupported_match_rule_count, 3);
    }

    #[test]
    fn runtime_keeps_owner_sessions_separate_and_incomplete_scans_issue_no_actions() {
        let now = Instant::now();
        let first = process(1000, "game.exe", 1);
        let mut second = process(2000, "game.exe", 2);
        second.session_id = SESSION + 1;
        let mut runtime = AppEnforcementRuntime::new(OWNER, SERVICE_PID).unwrap();
        let rules = rules(&["game.exe"]);

        let initial = runtime.reconcile(now, &rules, Some(&[first.clone(), second.clone()]), false);
        assert_eq!(initial.len(), 2);
        assert_eq!(initial[0].status.session_id, SESSION);
        assert_eq!(initial[1].status.session_id, SESSION + 1);
        assert_eq!(initial[0].close_requests.len(), 0);
        assert_eq!(initial[1].close_requests.len(), 0);

        let incomplete = runtime.reconcile(now + CLOSE_GRACE, &rules, None, false);
        assert_eq!(incomplete.len(), 2);
        assert!(incomplete.iter().all(|update| {
            !update.status.process_snapshot_available
                && update.status.pending_targets == 1
                && update.ready_for_termination.is_empty()
        }));

        let expired = runtime.reconcile(now + CLOSE_GRACE, &rules, Some(&[first, second]), false);
        assert_eq!(expired.len(), 2);
        assert!(expired.iter().all(|update| {
            update.status.process_snapshot_available && update.ready_for_termination.len() == 1
        }));
    }

    #[test]
    fn durable_service_adapter_attempts_termination_only_after_grace() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let context = context_with_durable_app_rule("game.exe");
        let mut enforcement = ServiceAppEnforcement::new(OWNER, SERVICE_PID).unwrap();
        let mut attempts = Vec::new();

        let initial = enforcement
            .reconcile_snapshot(
                &context,
                now,
                Some(std::slice::from_ref(&target)),
                false,
                |identity, owner| {
                    attempts.push((identity.clone(), owner.to_owned()));
                    Ok(true)
                },
            )
            .unwrap();
        let before_deadline = enforcement
            .reconcile_snapshot(
                &context,
                now + CLOSE_GRACE - Duration::from_millis(1),
                Some(std::slice::from_ref(&target)),
                false,
                |identity, owner| {
                    attempts.push((identity.clone(), owner.to_owned()));
                    Ok(true)
                },
            )
            .unwrap();
        let after_deadline = enforcement
            .reconcile_snapshot(
                &context,
                now + CLOSE_GRACE,
                Some(std::slice::from_ref(&target)),
                false,
                |identity, owner| {
                    attempts.push((identity.clone(), owner.to_owned()));
                    Ok(true)
                },
            )
            .unwrap();

        assert_eq!(initial[0].status.pending_targets, 1);
        assert!(before_deadline[0].ready_for_termination.is_empty());
        assert_eq!(
            after_deadline[0].ready_for_termination.as_slice(),
            std::slice::from_ref(&target)
        );
        assert_eq!(attempts, [(target, OWNER.into())]);
    }

    #[test]
    fn incomplete_durable_process_scan_preserves_pending_target_without_termination() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let context = context_with_durable_app_rule("game.exe");
        let mut enforcement = ServiceAppEnforcement::new(OWNER, SERVICE_PID).unwrap();
        let mut attempts = 0;

        enforcement
            .reconcile_snapshot(
                &context,
                now,
                Some(std::slice::from_ref(&target)),
                false,
                |_, _| {
                    attempts += 1;
                    Ok(true)
                },
            )
            .unwrap();
        let incomplete = enforcement
            .reconcile_snapshot(&context, now + CLOSE_GRACE, None, false, |_, _| {
                attempts += 1;
                Ok(true)
            })
            .unwrap();

        assert_eq!(attempts, 0);
        assert_eq!(incomplete[0].status.pending_targets, 1);
        assert!(!incomplete[0].status.process_snapshot_available);
        assert!(incomplete[0].ready_for_termination.is_empty());
    }

    #[test]
    fn matches_exact_executable_names_case_insensitively() {
        let now = Instant::now();
        let mut coordinator = coordinator();
        let processes = [process(1000, "Game.EXE", 11), process(1001, "chat.exe", 12)];

        let update = coordinator.reconcile(now, &rules(&["game.exe"]), Some(&processes), true);

        assert_eq!(update.close_requests, [process(1000, "Game.EXE", 11)]);
        assert!(update.ready_for_termination.is_empty());
        assert_eq!(update.status.pending_targets, 1);
    }

    #[test]
    fn allowance_aware_rules_are_deferred_and_never_blocked_by_this_coordinator() {
        let now = Instant::now();
        let mut coordinator = coordinator();
        let process = process(1000, "game.exe", 11);
        let rule_snapshot = AppRuleSnapshot {
            // A matching rule attached to an allowance-aware list must be
            // omitted from the enforceable projection by the policy adapter.
            unmetered_executable_names: Vec::new(),
            deferred_allowance_rule_count: 1,
            unsupported_match_rule_count: 2,
        };

        let update = coordinator.reconcile(now, &rule_snapshot, Some(&[process]), true);

        assert!(update.close_requests.is_empty());
        assert!(update.ready_for_termination.is_empty());
        assert_eq!(update.status.pending_targets, 0);
        assert_eq!(update.status.deferred_allowance_rule_count, 1);
        assert_eq!(update.status.unsupported_match_rule_count, 2);
    }

    #[test]
    fn rejects_other_owner_sessions_session_zero_self_and_focusguard_processes() {
        let now = Instant::now();
        let mut coordinator = coordinator();
        let mut other_owner = process(1000, "game.exe", 1);
        other_owner.owner_sid = "S-1-5-21-9".into();
        let mut other_session = process(1001, "game.exe", 2);
        other_session.session_id = SESSION + 1;
        let mut session_zero = process(1002, "game.exe", 3);
        session_zero.session_id = 0;
        let service = process(SERVICE_PID, "game.exe", 4);
        let current_service_process = process(std::process::id(), "game.exe", 5);
        let ui = process(1004, "focusguard-ui.exe", 5);
        let helper = process(1005, "focusguard-agent.exe", 6);
        let processes = [
            other_owner,
            other_session,
            session_zero,
            service,
            current_service_process,
            ui,
            helper,
        ];

        let update = coordinator.reconcile(
            now,
            &rules(&["game.exe", "focusguard-ui.exe", "focusguard-agent.exe"]),
            Some(&processes),
            true,
        );

        assert!(update.close_requests.is_empty());
        assert!(update.ready_for_termination.is_empty());
        assert_eq!(update.status.pending_targets, 0);
    }

    #[test]
    fn suppresses_repeats_only_after_close_delivery_is_acknowledged() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let mut coordinator = coordinator();

        let first = coordinator.reconcile(
            now,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        let inside_retry_interval = coordinator.reconcile(
            now + CLOSE_REQUEST_RETRY_INTERVAL - Duration::from_millis(1),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        let retry = coordinator.reconcile(
            now + CLOSE_REQUEST_RETRY_INTERVAL,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        assert!(coordinator.acknowledge_close_request(&target));
        let acknowledged = coordinator.reconcile(
            now + CLOSE_REQUEST_RETRY_INTERVAL + Duration::from_secs(1),
            &rules(&["game.exe"]),
            Some(&[target]),
            true,
        );

        assert_eq!(first.close_requests.len(), 1);
        assert!(inside_retry_interval.close_requests.is_empty());
        assert_eq!(retry.close_requests.len(), 1);
        assert!(acknowledged.close_requests.is_empty());
        assert_eq!(acknowledged.status.pending_targets, 1);
    }

    #[test]
    fn retries_after_helper_disconnect_when_delivery_was_not_acknowledged() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let mut coordinator = coordinator();

        let first_attempt = coordinator.reconcile(
            now,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        // Model a failed pipe write or helper crash after request construction:
        // there is deliberately no acknowledgement.
        let disconnected = coordinator.reconcile(
            now + Duration::from_secs(1),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            false,
        );
        let reconnected = coordinator.reconcile(
            now + Duration::from_secs(2),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        assert!(coordinator.acknowledge_close_request(&target));
        let after_ack = coordinator.reconcile(
            now + Duration::from_secs(3),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );

        assert_eq!(
            first_attempt.close_requests.as_slice(),
            std::slice::from_ref(&target)
        );
        assert_eq!(disconnected.status.helper, HelperHealth::Unavailable);
        assert!(disconnected.close_requests.is_empty());
        assert_eq!(reconnected.close_requests, [target]);
        assert!(after_ack.close_requests.is_empty());
    }

    #[test]
    fn cancels_unblocked_exited_and_replaced_process_identities() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let mut coordinator = coordinator();

        coordinator.reconcile(
            now,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        let unblocked = coordinator.reconcile(
            now + Duration::from_secs(1),
            &AppRuleSnapshot::default(),
            Some(std::slice::from_ref(&target)),
            true,
        );
        assert_eq!(unblocked.status.pending_targets, 0);

        coordinator.reconcile(
            now + Duration::from_secs(2),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        let exited = coordinator.reconcile(
            now + Duration::from_secs(3),
            &rules(&["game.exe"]),
            Some(&[]),
            true,
        );
        assert_eq!(exited.status.pending_targets, 0);

        coordinator.reconcile(
            now + Duration::from_secs(4),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        let replacement = process(target.pid, "game.exe", target.creation_time + 1);
        let replaced = coordinator.reconcile(
            now + Duration::from_secs(5),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&replacement)),
            true,
        );
        assert_eq!(replaced.close_requests, [replacement]);
        assert_eq!(replaced.status.pending_targets, 1);
        assert_eq!(replaced.status.grace_expired_targets, 0);
    }

    #[test]
    fn grace_expires_after_thirty_seconds_and_due_targets_remain_retryable() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let mut coordinator = coordinator();
        coordinator.reconcile(
            now,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );

        let before = coordinator.reconcile(
            now + CLOSE_GRACE - Duration::from_millis(1),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        assert!(before.ready_for_termination.is_empty());

        let expired = coordinator.reconcile(
            now + CLOSE_GRACE,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        assert_eq!(
            expired.ready_for_termination.as_slice(),
            std::slice::from_ref(&target)
        );
        assert_eq!(expired.status.grace_expired_targets, 1);

        let retry = coordinator.reconcile(
            now + CLOSE_GRACE + Duration::from_secs(1),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        assert_eq!(retry.ready_for_termination, [target]);
        assert!(retry.close_requests.is_empty());
    }

    #[test]
    fn helper_absence_is_explicit_and_does_not_reset_or_skip_the_grace() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let mut coordinator = coordinator();

        let unavailable = coordinator.reconcile(
            now,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            false,
        );
        assert_eq!(unavailable.status.helper, HelperHealth::Unavailable);
        assert_eq!(unavailable.status.waiting_for_helper, 1);
        assert!(unavailable.close_requests.is_empty());
        assert!(unavailable.ready_for_termination.is_empty());

        let recovered = coordinator.reconcile(
            now + Duration::from_secs(10),
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            true,
        );
        assert_eq!(recovered.status.helper, HelperHealth::Connected);
        assert_eq!(
            recovered.close_requests.as_slice(),
            std::slice::from_ref(&target)
        );

        let expired = coordinator.reconcile(
            now + CLOSE_GRACE,
            &rules(&["game.exe"]),
            Some(std::slice::from_ref(&target)),
            false,
        );
        assert_eq!(expired.status.helper, HelperHealth::Unavailable);
        assert_eq!(expired.ready_for_termination, [target]);
        assert!(expired.close_requests.is_empty());
    }

    #[test]
    fn incomplete_process_snapshot_preserves_state_without_actions() {
        let now = Instant::now();
        let target = process(1000, "game.exe", 1);
        let mut coordinator = coordinator();
        coordinator.reconcile(now, &rules(&["game.exe"]), Some(&[target]), true);

        let incomplete =
            coordinator.reconcile(now + CLOSE_GRACE, &rules(&["game.exe"]), None, true);

        assert!(!incomplete.status.process_snapshot_available);
        assert_eq!(incomplete.status.pending_targets, 1);
        assert_eq!(incomplete.status.grace_expired_targets, 1);
        assert!(incomplete.close_requests.is_empty());
        assert!(incomplete.ready_for_termination.is_empty());
    }
}
