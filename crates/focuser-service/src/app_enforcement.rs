//! Pure, session-scoped coordination for executable-name app rules.
//!
//! This module does not enumerate processes, communicate with a session helper,
//! or terminate anything. The service supplies a complete process snapshot and
//! the enabled executable-name rules, then acts on the returned close requests
//! and expired grace targets through separately authenticated OS boundaries.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use focuser_common::process_identity::ProcessIdentity;

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

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "S-1-5-21-100-200-300-1001";
    const SESSION: u32 = 7;
    const SERVICE_PID: u32 = 500;

    fn coordinator() -> AppEnforcementCoordinator {
        AppEnforcementCoordinator::new(OWNER, SESSION, SERVICE_PID).unwrap()
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
