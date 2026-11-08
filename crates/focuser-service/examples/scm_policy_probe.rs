//! CI helper for the Windows SCM recovery smoke test. This talks to the
//! service only through the same SCM-PID-checked named-pipe client as the UI.

use std::env;

use anyhow::{Context, bail};
use focuser_app::{Command, CommandResult};
use focuser_client::execute;
use focuser_common::types::WebsiteMatchType;

const LIST_NAME: &str = "FocusGuard SCM recovery CI";
const TEST_DOMAIN: &str = "focuser-scm-recovery.invalid";
const PROFILE_ID: &str = "00000000-0000-4000-8000-000000000001";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if env::var("GITHUB_ACTIONS").ok().as_deref() != Some("true")
        || env::var("RUNNER_OS").ok().as_deref() != Some("Windows")
        || env::var("RUNNER_ENVIRONMENT").ok().as_deref() != Some("github-hosted")
        || env::var("FOCUSGUARD_SCM_RUNTIME_TEST").ok().as_deref() != Some("1")
    {
        bail!("this probe is restricted to the GitHub-hosted Windows SCM test job");
    }

    match env::args().nth(1).as_deref() {
        Some("seed") => seed().await,
        Some("verify") => verify().await,
        _ => bail!("usage: scm_policy_probe.exe <seed|verify>"),
    }
}

async fn seed() -> anyhow::Result<()> {
    let result = execute(Command::CreateBlockList {
        name: LIST_NAME.into(),
    })
    .await?;
    let CommandResult::BlockList(list) = result else {
        bail!("service returned no block list after creation");
    };

    execute(Command::ToggleBlockList {
        id: list.id,
        enabled: true,
    })
    .await?;
    execute(Command::AddWebsiteRule {
        list_id: list.id,
        rule: WebsiteMatchType::Domain(TEST_DOMAIN.into()),
    })
    .await?;
    check_extension_presence().await?;
    verify().await.context("verify rule after IPC creation")
}

async fn verify() -> anyhow::Result<()> {
    let result = execute(Command::ListBlockLists).await?;
    let CommandResult::BlockLists(lists) = result else {
        bail!("service returned no block-list collection");
    };

    let found = lists.iter().any(|list| {
        list.name == LIST_NAME
            && list.websites.iter().any(|rule| {
                rule.enabled && rule.match_type == WebsiteMatchType::Domain(TEST_DOMAIN.into())
            })
    });
    if !found {
        bail!("service did not return the persisted exact-domain rule");
    }

    check_extension_presence().await?;

    println!("Authenticated service IPC confirms {TEST_DOMAIN} is persisted.");
    Ok(())
}

async fn check_extension_presence() -> anyhow::Result<()> {
    execute(Command::ExtensionHeartbeat {
        browser: "Chrome".into(),
        profile_id: PROFILE_ID.into(),
        incognito_allowed: Some(true),
    })
    .await?;
    let result = execute(Command::GetBlockingHealth).await?;
    let CommandResult::BlockingHealth(health) = result else {
        bail!("service returned no blocking health after extension heartbeat");
    };
    if !health.extension_connected {
        bail!("service did not scope extension presence to the authenticated session");
    }
    println!("Authenticated service IPC records extension presence for this Windows session.");
    Ok(())
}
