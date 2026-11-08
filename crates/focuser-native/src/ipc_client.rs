//! Narrow adapter from the browser host to the authenticated Windows service.

use anyhow::{Result, bail};
use focuser_app::{Command, CommandResult};
use focuser_common::extension::ExtensionRuleSet;

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
