//! Narrow adapter from the browser host to the authenticated Windows service.

use anyhow::{Result, bail};
use focuser_app::{Command, CommandResult};
use focuser_common::extension::{ExtensionRuleSet, ExtensionServiceEvent};

/// Read the current extension rules through the pipe that authenticates the
/// running SCM service process. The browser host never opens a loopback port.
pub async fn get_extension_rules() -> Result<ExtensionRuleSet> {
    if !focuser_client::is_service_registered()? {
        bail!("FocusGuard Windows service is not installed");
    }
    match focuser_client::execute(Command::GetExtensionRules).await? {
        CommandResult::ExtensionRules(rules) => Ok(rules),
        other => bail!("service returned an unexpected response: {other:?}"),
    }
}

/// Forward one narrow extension event through the authenticated service pipe.
/// Callers still run as the interactive user, so the service treats these
/// telemetry fields as forgeable and validates/bounds them before use.
pub async fn send_extension_event(event: ExtensionServiceEvent) -> Result<()> {
    if !focuser_client::is_service_registered()? {
        bail!("FocusGuard Windows service is not installed");
    }
    let command = match event {
        ExtensionServiceEvent::Heartbeat {
            browser,
            profile_id,
            incognito_allowed,
        } => Command::ExtensionHeartbeat {
            browser,
            profile_id,
            incognito_allowed,
        },
        ExtensionServiceEvent::AllowanceTick {
            browser,
            profile_id,
            hostname,
            url,
        } => Command::RecordExtensionAllowanceTick {
            browser,
            profile_id,
            hostname,
            url,
        },
        ExtensionServiceEvent::SharedActivityTick {
            browser,
            profile_id,
            hostname,
            url,
        } => Command::RecordExtensionSharedActivityTick {
            browser,
            profile_id,
            hostname,
            url,
        },
    };
    match focuser_client::execute(command).await? {
        CommandResult::Unit => Ok(()),
        other => bail!("service returned an unexpected response: {other:?}"),
    }
}
